# holtmail-collection-graph

Reads the contacts and calendars of a Microsoft 365, Outlook.com or Hotmail account through Microsoft Graph, behind the [`CollectionSource`](../../collection/docs/README.md) contract. Read-only: only `GET` is sent. Each item is stored as the JSON Graph answered for it, and [`holtmail-pim`](../../pim/docs/README.md) reads that JSON into the normalized contact and event model.

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

## Tests

`src/tests.rs` runs an in-process fake Graph (axum) with the default contacts folder, nested folders and calendars, delta links with generations, paging of two items per page, `410` for an old generation, `429` with `Retry-After`, and bearer checks. It runs the conformance suite; further tests cover folder paths and calendar colors, delta changes and removals in a folder with the raw JSON kept, series masters fetched with their exceptions (and without a listing, plainly first), throttling, links to another host refused with nothing sent there, refused passwords, plain http, stale tokens and unknown collections, and a `@odata.nextLink` that repeats itself, refused for a listing and a delta. A test against a TLS server in the process with a certificate from `holtmail_core::tls::testing::TestCa` checks that its CA is trusted only through `with_roots`.
