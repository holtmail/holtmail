# Plan: contacts and calendars

Holtmail archives mail. This plan extends it to contacts and calendar events from the same accounts, with their full history: every version a server ever showed, and when an item disappeared. It also covers how these link to mail and to each other. Nothing here is built yet. The checklist at the end becomes WORKPLAN phases once the maintainer agrees.

## Principles carried over from mail

- **Raw is the truth.** Every version of a contact or an event is stored exactly as the server sent it, as a blob addressed by its BLAKE3 hash: a vCard, an iCalendar object, or the provider's JSON. Identical versions cost nothing.
- **Everything else is derived.** The normalized model, recurrence instances, the "person" groups and the search index can all be rebuilt from the raw versions.
- **Read-only.** Holtmail never writes to a contact or calendar server.
- **The archive never forgets.** An item removed on the server is marked removed, with the time it was noticed, and stays readable.
- **Same module rules as mail.** Every source sits behind a contract with a conformance suite, and tests run against in-process fakes only.

## What the protocols look like

IMAP has no contacts or calendars. Each provider offers one of a few families:

| Provider | Contacts | Calendars | Sign-in |
| --- | --- | --- | --- |
| Most self-hosted servers (Nextcloud, Radicale, Baïkal, Stalwart, Cyrus), iCloud, Fastmail, Yandex, mailbox.org | CardDAV (RFC 6352, vCard) | CalDAV (RFC 4791, iCalendar) | password or app password |
| Google | CardDAV, or the People API | CalDAV, or the Calendar API | OAuth: Thunderbird's Google client already lists CardDAV and CalDAV scopes |
| Microsoft 365, Outlook.com | Graph `/me/contacts` (JSON, delta) | Graph `/me/events` (JSON, delta) | OAuth (Graph source from Phase "Later") |
| Fastmail, Stalwart | JMAP for Contacts (RFC 9610, JSContact) | JMAP Calendars (draft, JSCalendar) | token or password |

Despite the different wire formats, every one of them reduces to the same shape:

- an account has collections (address books and calendars);
- a collection holds items, each with a stable id and a version tag (an ETag, or a change key);
- the source can say what changed since a sync token: items added or changed, and items removed.

This is how WebDAV `sync-collection` (RFC 6578), Graph delta, JMAP `/changes` and Google `syncToken` all work. Holtmail therefore needs one contract, not one per provider.

### A common model

The IETF already defined JSON models meant to be shared by every protocol:

- JSContact (RFC 9553) for contacts;
- JSCalendar (RFC 8984) for events and tasks.

The Rust crate `calcard` (Stalwart Labs, Apache-2.0 OR MIT) parses vCard and iCalendar, tolerates non-conforming data, and converts both ways between vCard and JSContact and between iCalendar and JSCalendar. Holtmail stores the raw object and derives a JSContact or JSCalendar view from it. Graph and Google JSON are mapped into the same models by the source that reads them. The web app and the API only ever see the normalized model and the raw bytes for "view source".

## Storage

| Table | What it holds |
| --- | --- |
| `collections` | Account, kind (contacts, calendar), remote id, name, color, the sync token, `removed_at` |
| `items` | Collection, remote id (href or provider id), kind (contact, event, task), the stable key (see Identity), `first_seen_at`, `removed_at` |
| `item_versions` | Item, `blob_id` of the raw object, content type, ETag or change key, `seen_at`; a new row only when the bytes change |
| derived `pim_index` | Normalized fields for listing and joins: names, emails, phones, organization, event start and end in UTC, original time zone, recurrence flag, organizer, attendees, `UID`, `SEQUENCE` |
| derived `occurrences` | Expanded instances of recurring events for a window (for example two years back and forward from today, grown on demand); rebuilt from the latest version |

Photos inside vCards are split off into their own blobs, like attachments, so the same avatar in three address books is stored once.

## Sync and history

1. Discover collections:
   - DAV: RFC 6764 well-known address, then `current-user-principal`, then `addressbook-home-set` and `calendar-home-set`.
   - Graph and Google: list endpoints.
   - JMAP: `AddressBook/get` and `Calendar/get`.
2. Ask for changes since the stored token, and fetch the bodies of changed items only.
3. Store a new version only when the bytes changed. Removed ids get `removed_at` and are never deleted.
4. When a token expired or the server has no `sync-collection`:
   - list every id and ETag, which is cheap;
   - diff that list against the archive;
   - fetch only what differs;
   - mark the missing ids as removed.

   This is the same recovery the mail sync already does.
5. A collection that disappears is marked removed with all its items, and stays browsable.

What history can and cannot show:

- **Versions between two syncs are invisible.** A contact edited three times within the sync interval shows one change.
- **Some fields come with their own history.** iCalendar `SEQUENCE`, `DTSTAMP` and `LAST-MODIFIED`, and vCard `REV`, are kept inside the raw versions and shown on the timeline.
- **Mail fills gaps for meetings.** Invitations, updates, replies and cancellations (iMIP, RFC 6047: `text/calendar` parts with `METHOD:REQUEST`, `REPLY` or `CANCEL`) are already in the archived mail, often from years before the calendar was connected. They become an event's history too, so the calendar is useful even for an account with mail only.

The item page shows a timeline of versions, each with a field-level diff computed between two normalized versions: "time moved from 10:00 to 11:00", "phone added", "removed from server".

## Identity across accounts

The same person or meeting usually exists in several places: a work and a private address book, the organizer's copy and each attendee's copy of a meeting.

**Events** have a global key: iCalendar `UID`, plus `RECURRENCE-ID` for one changed instance. Graph exposes it as `iCalUId`, and Google and JMAP keep it too. Items with the same `UID` are grouped automatically:

- one event card says "in 3 calendars";
- the copy with the highest `SEQUENCE`, then the latest `DTSTAMP`, is shown first;
- the differences between copies are listed: a different time means someone missed an update, and each attendee's answer (`PARTSTAT`) is per copy.

**Contacts** have no global key. Grouping into a "person" is a derived, explainable guess:

- exact match of a normalized email address or an E.164 phone number links two contacts;
- a name alone never does;
- groups are shown with the reason ("same email ada@example.com");
- the owner can split a group or join two by hand. These overrides are the only stored state, and the raw contacts never change.

The merged person page shows every field with its sources. Where the sources agree, the value is shown once with small source marks. Where they disagree, the field shows each value side by side with its account and when it was last seen: "Phone: +1 555 0100 (Work, seen today) · +1 555 0199 (Private, last seen 2024)". A "Differences" filter lists people whose sources disagree.

## Links between mail, people and events

All links are derived and rebuilt with the rest:

- **Person ↔ mail.** Addresses in From, To, Cc and Reply-To match contact emails. The person page shows the correspondence: the first and last message, counts per year, threads and attachments exchanged. A sender in the reader links to the person.
- **Event ↔ mail.** Invitations and replies match by `UID`. The event shows its mail thread; the message shows "This invitation is in your calendar (updated twice since)".
- **Event ↔ person.** The organizer and attendees match contacts by email. The person page lists meetings together.
- **Mail without contacts.** Frequent correspondents with no contact can be shown as "people from mail" (derived from addresses and display names). The contacts page is useful even before any address book is connected.
- **Later.** Event descriptions and locations containing links or phone numbers; attachments of events (`ATTACH`) as blobs; birthdays and anniversaries from contacts as a read-only calendar.

## Modules

| Module | Job |
| --- | --- |
| `holtmail-pim` | Types shared by contacts and calendars: the normalized JSContact and JSCalendar views (wrapping `calcard`), field diff, identity keys |
| `holtmail-collection` | Contract: list collections, `changes(collection, token)`, `fetch(ids)`, `list_versions(collection)` for recovery; own error enum; conformance suite and a fake |
| `collection-dav` | CardDAV and CalDAV: discovery, `sync-collection`, `multiget`, ETag listing; password, app password and OAuth bearer |
| `collection-graph` | Graph contacts and events with delta, mapped into JSContact and JSCalendar; raw JSON kept |
| `collection-jmap` | JMAP Contacts (RFC 9610) and Calendars (once the draft settles) |
| `holtmail-recur` | Recurrence expansion with time zones (`RRULE`, `EXDATE`, `RDATE`, overrides, `VTIMEZONE`) for the derived occurrences |
| store and store-sqlite | The tables above, conformance cases |
| `holtmail-pim-sync` | The engine: tokens, versions, removals, recovery; one per account, run by the existing scheduler and task list |
| ingest | iMIP parts in mail become event versions with the message as their source |
| import | `.vcf` and `.ics` files, Google Takeout contacts and calendars |
| index | Contacts and events as their own document types; `is:event`, `is:contact` and `with:` (a person) in the query language |
| api, api-client, web | Contacts page, calendar page, person and event pages, history timelines, links in the reader |

A connected account gets "also archive contacts and calendars", using the same sign-in:

- Google: the CardDAV and CalDAV scopes are added on the next sign-in.
- Microsoft: Graph `Contacts.Read` and `Calendars.Read`.
- DAV: the address is discovered from the mail domain, or entered by hand.

## Interface

- **Contacts.** A people list (persons, not raw cards), filters by source and "has differences", search. The person page has fields with sources, correspondence, meetings and the timeline of every source card.
- **Calendar.** Month, week and agenda views built from the derived occurrences. Each source calendar is a toggle with its color, and removed events and removed calendars can be shown on request. Grouped copies show as one event with a "3 copies" mark.
- **Event page.** Time and recurrence in the original zone and the viewer's zone. It shows attendees with each copy's answers, the differences between copies, the mail thread, and the timeline including iMIP messages.
- **Everywhere:** "view source" for any version, and export (`.vcf` or `.ics` of the latest versions, or of every version as a zip).

## Hard parts and their answers

| Problem | Answer |
| --- | --- |
| Formats differ per provider | Raw kept unchanged; one normalized model (JSContact, JSCalendar) derived by `calcard` or by the source's mapping |
| Broken vCards and iCalendars from real servers | Tolerant parsing; an item that cannot be parsed is still archived raw and shown as "unreadable", like a broken message |
| Recurrence and time zones | Expansion in one module with tests against invented edge cases (DST changes, `EXDATE`, moved instances, `UNTIL` in other zones); custom `VTIMEZONE` definitions honoured |
| Same event in several calendars | Grouped by `UID` and `RECURRENCE-ID`, newest by `SEQUENCE` and `DTSTAMP`, differences listed |
| Same person in several address books | Derived groups by email or phone with a reason, manual split and join, per-field sources |
| Sync tokens expire or are missing | ETag listing and diff, as the mail sync recovers |
| History between syncs is lost | Stated plainly; iMIP mail and `SEQUENCE` fill gaps for meetings; the sync interval can be shorter for calendars |
| Large calendars (shared, years of events) | Only changed items fetched; occurrences materialized for a window, not forever |
| Privacy | Contacts and events are personal data: same access model as archives (per-archive grants); logs carry ids and counts only; fixtures invented |

## Order of work

1. `holtmail-pim` and the storage tables, with `.vcf` and `.ics` import. This proves the raw-and-derived model without any network.
2. Events from iMIP mail already in the archive. This gives immediate value and links mail to events.
3. `holtmail-collection` contract and `collection-dav` (Nextcloud, Radicale, iCloud, Fastmail, Google with OAuth scopes), the sync engine with versions and removals.
4. Contacts page and person page, with links to mail.
5. Calendar page, event page, recurrence expansion, grouping across calendars.
6. `collection-graph` for Microsoft.
7. `collection-jmap` (contacts now, calendars once the draft settles).
8. Person grouping overrides, the differences filter, export.

Each step gets its own WORKPLAN phase with tests, docs and VERIFY.md entries for what only real servers can prove.
