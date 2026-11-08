# holtmail-collection-graph

Reads the contacts and calendars of a Microsoft 365, Outlook.com or Hotmail account through Microsoft Graph, behind the [`CollectionSource`](../../collection/docs/README.md) contract, and writes changes to them behind [`CollectionWriter`](../../collection/docs/README.md#writing-collectionwriter). Reading sends only `GET`; writes happen only through the writer, which the server hands out for accounts allowed to write. Each item is stored as the JSON Graph answered for it, and [`holtmail-pim`](../../pim/docs/README.md) reads that JSON into the normalized contact and event model.

## Config

```rust
let source = GraphCollections::new(
    None,                                    // GRAPH_URL, or a national cloud's Graph address
    Credentials::Bearer(access_token),       // Contacts.Read and Calendars.Read
    Duration::from_secs(120),
)?;
```

`with_roots(&TlsRoots)` trusts the given CA certificates (from Holtmail's `tls_ca_file`, see [`holtmail-core`](../../core/docs/README.md)) besides the system's roots; verification and the host name check stay on. An unusable certificate is a `Config` error.

The address must use https (plain http only to a loopback address); a password is `Config`. The server builds the source with a fresh token before each sync, from a Microsoft sign-in that granted `Access::GraphPim` (mail, `Contacts.Read`, `Calendars.Read`).

Writing needs `Contacts.ReadWrite` and `Calendars.ReadWrite` instead. `holtmail-oauth` does not ask for them yet: the server is expected to add them to the sign-in while `pim.edit` (or `pim.respond`) is on for the account, as it does with `Mail.ReadWrite` for mailbox changes, and to hand out the writer only once the token carries them. The server checks that the sign-in granted them before it hands out a writer, so a write `403` means the account may not change that calendar or folder and is `Refused`.

## Collections

| Collection id | What | Name |
| --- | --- | --- |
| `contacts` (`DEFAULT_CONTACTS`) | the default contacts folder, `/me/contacts` | `Contacts` |
| `contacts/<folder id>` | every folder of `/me/contactFolders`, and their `childFolders`, at any depth (1 000 at most) | the folder path, `Work/Clients` |
| `calendars/<calendar id>` | every calendar of `/me/calendars` | its `name`, with `hexColor` as the color |

No collection has a tag (Graph keeps no ctag), so every sync asks.

## Changes and bodies

- **Contacts** change by delta query: `changes(c, None)` follows `…/contacts/delta?$select=id,changeKey` page by page to its `@odata.deltaLink`, which is the token; `changes(c, Some(link))` asks that link. `@removed` entries are removals. A `410 Gone` for a token (Graph's `syncStateNotFound` and `resyncRequired`) is `TokenExpired`, and so is a stored link that is not on the configured Graph host (it is never asked). `list` is `…/contacts?$select=id,changeKey`. The version tag is `changeKey`.
- **Calendars** cannot list changes: Graph's event delta exists only for the primary calendar's `calendarView`, which needs a time window and expands series into single occurrences, so it can neither keep series nor cover all time. `changes` answers `Unsupported`, and the sync engine lists the calendar each time: `/me/calendars/{id}/events?$select=id,changeKey,type` gives every single event and series master (never occurrences or exceptions) with its `changeKey`, and only events whose tag changed are fetched.
- **Items** are fetched one `GET` each, four at a time. A contact is `GET …/contacts/{id}`. A single event is `GET /me/calendars/{c}/events/{id}`. A series master (known from the listing's `type`, or from a first plain `GET`) is asked with `$select` of the event's fields plus `cancelledOccurrences` and `$expand=exceptionOccurrences`, which Graph returns only when asked by a master's id; the stored JSON then holds the master, its recurrence pattern, the ids of cancelled occurrences and every changed occurrence in full. A `404` leaves the item out.
- Every request sends `Prefer: IdType="ImmutableId"` (ids survive moves between folders), `outlook.body-content-type="text"` (descriptions as text) and `odata.maxpagesize=200`. Times come in UTC; the original zone is in `originalStartTimeZone`.

What is stored per version: the body of that item's `GET` exactly as Graph sent it (`application/vnd.microsoft.graph.contact+json`, `application/vnd.microsoft.graph.event+json`), `@odata.context` and `@odata.etag` included. Contact photos (`/contacts/{id}/photo`) are not read.

## Limits and safety

- **The token stays home.** It is sent only to the origin of the configured Graph address. `@odata.nextLink` and `@odata.deltaLink` on another host are refused with `Protocol` before anything is sent; redirects are not followed.
- **Sizes.** Listings are capped at 64 MiB per page, one item at 16 MiB; a listing stops after 10 000 pages or a million items with `Protocol`, and so does one whose `@odata.nextLink` names a page it already gave.
- **Throttling.** `429`, `503` and `504` are waited out as `Retry-After` says, at most two minutes, five tries at most, then `Connection`.
- **Errors.** `401` is `Auth`; `403` on a listing is `Auth` (the sign-in lacks `Contacts.Read` or `Calendars.Read`), on one item `Protocol`. Messages carry the host and status only.
- **Series.** A change to only an exception is expected to change the master's `changeKey` (Exchange keeps exceptions inside the master); if a server does not, the change shows at the master's next change. See [VERIFY.md](../../../docs/VERIFY.md).

## Writing

`GraphCollections` is also a `CollectionWriter`. Changes arrive as `ItemPatch` in iCalendar (events) or vCard (contacts); the writer reads them with `holtmail-pim` (and `calcard` for alarms, categories, `TRANSP`, `CLASS`, `ORG` and `URL`) and sends Graph JSON.

| Call | Graph |
| --- | --- |
| `capabilities(c)` | Before the first `collections()`, from the collection id alone: `contacts`, `contacts/<id>` and `calendars/<id>` can create, update and delete, calendars also respond. Once listed, only the listed collections the account may edit (contact folders, and calendars without `canEdit: false`); a read-only or unlisted id offers nothing. `field_patch` and `tags` are on, `keeps_uid` is off, no `max_size` |
| `put(c, None, patch, _, key)` | `POST …/events` with `transactionId: key`, so Graph answers a repeat with the event it already made. Contacts have no such field: the key is kept in a single-value extended property (`String {6f1d2c3b-8a4e-4b7f-9c5d-0e2a7b3c9d14} Name HoltmailCreateKey`), and a create first looks it up with `$filter=singleValueExtendedProperties/Any(…)`, answering with the contact it finds. A create sends every field Graph can take, whatever `fields` lists. `404` for the folder or calendar is `Gone` |
| `put(c, Some(id), patch, precondition, _)` | Reads the item (as `fetch` does), checks the precondition against its `changeKey` (`IfMatch`, with or without `W/"…"` quoting) or its bytes (`IfUnchanged`), then `PATCH`es only the listed fields with `If-Match: W/"<changeKey>"`, so a change made in between fails with `412` too. A missing item, a precondition that does not hold, or a `412` is `PreconditionFailed` with the tag read afterwards. A patch with nothing Graph needs to change answers with the item as it is |
| `delete(c, id, if_match)` | `DELETE`, with the same read and `If-Match` when a tag is given; `404` succeeds |
| `respond(c, id, rid, answer, notify)` | `POST …/events/{id}/accept`, `tentativelyAccept` or `decline` with `sendResponse: notify`. For one occurrence the writer lists `…/events/{id}/instances` in a window of two days around `rid` and answers the instance whose `originalStart` is that moment (`…Z` is UTC; a local time is read in the series' `originalStartTimeZone`, else UTC; a date matches the instance's UTC date). `NeedsAction` is `Refused` (Graph cannot take an answer back), and so is an answer in an address book or for an occurrence the series does not have. The answer is the item read back |

Graph answers `400`, `409`, `413` and `422` with `Refused`, carrying the host, status and Graph's error code (never its message, which may quote the item). `403` is `Refused` too. Throttling is waited out as for reads; a create retried after a `503` stays single through `transactionId` or the key lookup.

**Fields.** An update sends exactly the fields the patch lists (`None`: every field below), each read from the patch's main component; a listed field Graph has no room for is `Refused` before anything is sent.

| Field | Event | Contact |
| --- | --- | --- |
| `Summary` | `subject` | Refused |
| `Start`, `End` | `start`, `end` and `isAllDay` together: the local time with the `TZID`'s IANA name (Windows names become IANA), `UTC` for `Z` and floating times, UTC for times with an offset; all-day events as midnight `UTC`, the end defaulting to the next day | Refused |
| `Recurrence` | `recurrence` as a `patternedRecurrence` (below), `null` when the rule is gone. `RDATE`s are `Refused`, and so are `EXDATE`s that differ from the server's `cancelledOccurrences`: Graph excludes an occurrence only by deleting it | Refused |
| `Location` | `location.displayName` | Refused |
| `Description` | `body` as text | Refused |
| `Attendees` | `attendees` with `type` from `ROLE`, leaving out the organizer | Refused |
| `Alarms` | the first `VALARM`: `isReminderOn` and `reminderMinutesBeforeStart`; one relative to the end, absolute or after the start is `Refused`; none turns the reminder off | Refused |
| `Categories` | `categories` | Refused |
| `Transparency` | `showAs` `free` or `busy` | Refused |
| `Privacy` | `sensitivity` from `CLASS` | Refused |
| `Status` | `CANCELLED` is `Refused` (Graph cancels with a request of its own); other values send nothing | Refused |
| `Organizer` | `Refused` (Graph sets it) | Refused |
| `Name` | Refused | `displayName`, `givenName`, `surname`, `middleName`, `title`, `generation`, `nickName` (one) |
| `Emails` | Refused | `emailAddresses`, three at most |
| `Phones` | Refused | `mobilePhone` (one `cell`), `homePhones`, the rest `businessPhones` |
| `Addresses` | Refused | `homeAddress`, `businessAddress`, `otherAddress` (one each), PO box and extended address in `street` |
| `Organization` | Refused | `companyName` (first unit), `department` (the rest) |
| `Title`, `Notes` | Refused | `jobTitle`, `personalNotes` |
| `Dates` | Refused | `birthday`; one without a year is `Refused` |
| `Urls` | Refused | `businessHomePage` (one) |
| `Photo`, `Property(_)` | Refused | Refused |

A field cleared in the patch is sent as `null` (an empty string for `subject`, `location` and `body`). Changed instances in the iCalendar are not written: Graph keeps exceptions inside the series, and they stay as the server holds them. Graph gives events its own `iCalUId`, so the patch's `UID` is not kept (`WriteCaps::keeps_uid` is false); the item read back after a create carries Graph's.

**Rules to patterns** (the reverse of the reading table in `holtmail-pim`): `DAILY` is `daily`, or `weekly` with `BYDAY` and interval 1; `WEEKLY` is `weekly` with `BYDAY` (else the start's weekday) and `WKST` as `firstDayOfWeek` (default Monday); `MONTHLY` with one `BYMONTHDAY` (or none: the start's day) is `absoluteMonthly`, with `BYDAY` and a position (`2TU`, or `TU` with `BYSETPOS=2`; first to fourth and last) `relativeMonthly`; `YEARLY` likewise `absoluteYearly` or `relativeYearly` with `BYMONTH` (else the start's month). `COUNT` is `numbered`, `UNTIL` `endDate` (its date), neither `noEnd`; the range starts on the start's date in its zone. Anything else (`BYHOUR`, several month days, a fifth weekday, `HOURLY`, ...) is `Refused`.

## Tests

`src/tests.rs` runs an in-process fake Graph (axum) with the default contacts folder, nested folders and calendars, delta links with generations, paging of two items per page, `410` for an old generation, `429` with `Retry-After`, and bearer checks. It runs the conformance suite; further tests cover folder paths and calendar colors, delta changes and removals in a folder with the raw JSON kept, series masters fetched with their exceptions (and without a listing, plainly first), throttling, links to another host refused with nothing sent there, refused passwords, plain http, stale tokens and unknown collections, and a `@odata.nextLink` that repeats itself, refused for a listing and a delta. A test against a TLS server in the process with a certificate from `holtmail_core::tls::testing::TestCa` checks that its CA is trusted only through `with_roots`.

`src/tests/writes.rs` makes the fake writable: `POST` with `transactionId` deduplication, contacts with the create-key property and its `$filter` lookup, `PATCH` merging fields, `DELETE`, `If-Match` against `W/"<changeKey>"` with `412`, `instances` of daily and weekly series, `accept`/`tentativelyAccept`/`decline` recorded with `sendResponse` (an occurrence's answer lands in `exceptionOccurrences`), `403` for a write the account may not make, calendars listed with `canEdit`, and a switch that changes an item just before the next write. The fake is its own `WriteProbe` (`holds` compares the fields written through `holtmail-pim`). It passes `writer_conformance` for a calendar, a contact folder and the default contacts folder; further tests check that an update sends only the listed fields under `If-Match` and keeps the rest, that a change made while sending fails the update and the delete through `412`, idempotent creates by `transactionId` and by the key lookup, refusals that send nothing (organizer, excluded occurrences, `NeedsAction`, an unknown occurrence, an address book), a `400` reported with its code but not its message, `Refused` for `403`, and no capabilities for a listed read-only or unlisted calendar. Unit tests in `src/convert.rs` cover the field mapping for events and contacts, times, rules to patterns and every refusal; `src/write.rs` covers reading occurrences and tag quoting.
