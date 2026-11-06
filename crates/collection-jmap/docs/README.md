# holtmail-collection-jmap

Reads address books and calendars from a JMAP server behind the [`CollectionSource`](../../collection/docs/README.md) contract: JMAP for Contacts (RFC 9610, `urn:ietf:params:jmap:contacts`) and, when the session offers it, JMAP Calendars (draft-ietf-jmap-calendars, `urn:ietf:params:jmap:calendars`). Read-only: it calls only `/get`, `/query` and `/changes` methods. Each card is stored as its JSContact object (`application/jscontact+json`) and each event as its JSCalendar object (`application/jscalendar+json`), as the server's `/get` answer has it with the keys of every object sorted and no spaces; [`holtmail-pim`](../../pim/docs/README.md) reads them.

## Config

```rust
let source = JmapCollections::new(
    "https://api.fastmail.com/jmap/session",   // or https://host/.well-known/jmap
    Credentials::Password { username, password }, // or Credentials::Bearer(token)
    Duration::from_secs(120),
)?;
let offered = source.offers_contacts().await?; // the session has a primary contacts account
let calendars = source.offers_calendars().await?; // and one for calendars
```

`with_push_ping(Duration)` asks the event source to ping every that often (at least a second) instead of every minute.

`with_roots(&TlsRoots)` trusts the given CA certificates (from Holtmail's `tls_ca_file`, see [`holtmail-core`](../../core/docs/README.md)) besides the system's roots; verification and the host name check stay on. An unusable certificate is a `Config` error.

The server passes the JMAP account's own session address (normalized by `holtmail_source_jmap::session_url`) and its own password or token. The address must use https (plain http only to a loopback address) and must not hold a user name.

## Collections and items

| Collection id | JMAP |
| --- | --- |
| `addressbook/<id>` | an `AddressBook` of the primary account for contacts; name from `name` |
| `calendar/<id>` | a `Calendar` of the primary account for calendars, only when the session offers calendars; `name`, `color` |

The collection tag is the account's `ContactCard` (or `CalendarEvent`) state from a `/get` with no ids: equal states mean nothing changed in any address book, so an unchanged account costs one request per kind. Items have no ETag (JMAP has none per object), so every item `changes` names is fetched.

- `changes(c, None)`: the state, then every id from `ContactCard/query` (`CalendarEvent/query`) with the filter `inAddressBook` (`inCalendar`), paged 1 000 at a time, at most a million ids (a page equal to the one before, from a server that ignores `position`, is `Protocol`); `Gone` when the collection is not in `AddressBook/get`.
- `changes(c, Some(state))`: `ContactCard/changes` (`CalendarEvent/changes`) from that state, following `hasMoreChanges` (at most a million ids; a `newState` equal to the state asked with while more changes are said to follow is `Protocol`); created and updated ids are checked with a `/get` of `id` and `addressBookIds` (`calendarIds`): those in the collection are changed, the others (moved to another collection) removed, and destroyed ids are removed. Since changes are per account, `removed` can name ids that were never in the collection (the contract allows it). `cannotCalculateChanges` is `TokenExpired`.
- `fetch`: `/get` of the ids in batches of the session's `maxObjectsInGet` (500 at most), leaving out ids not in the collection.
- `list`: the ids of the query, sorted, without tags.

- `watch`: the session's `eventSourceUrl` (RFC 8620 §7.3) with the types the session offers (`ContactCard,AddressBook` for contacts, `CalendarEvent,Calendar` for calendars), `closeafter=no`, `ping=60`; each `StateChange` naming one of the session's contacts or calendars accounts with one of those types reports every collection. No answer for two and a half pings, or a closed stream, ends it with `Connection`. `None` when the session names no event source or offers neither kind. A relative event source address is resolved against the session's origin; one without https is refused with `Protocol`.

Method errors: `cannotCalculateChanges` → `TokenExpired`; `forbidden`, `accountNotFound`, `accountNotSupportedByMethod` → `Auth`; others `Protocol` with the error type.

## Limits and safety

- **Credentials stay home.** A session address under `/.well-known/` is followed through its redirects (at most five, https only) **without credentials**; the session is then asked with credentials where it led. Credentials go to that session address and to the `apiUrl` the authenticated session names, also on another host (the same rule as [`holtmail-source-jmap`](../../source-jmap/docs/README.md) for mail, which Fastmail needs), but only over https (plain http only to a loopback address); an `apiUrl` without https is refused with `Protocol` before anything is sent there. Redirects are never followed with credentials.
- **Sizes.** The session is capped at 1 MiB, each API answer at 64 MiB.
- **Busy servers.** `429`, `503` and `504` are waited out as `Retry-After` says, at most two minutes, five tries at most.
- **Bytes.** An item's raw bytes are its JSON object written again with the keys of every object sorted, compact, with `serde_json`'s escapes; values, array order and every property stay the server's. Stalwart writes the keys of one unchanged object in a different order on every answer, so the server's own text would make a new version each time an item is read again (after `cannotCalculateChanges`, or a collection read in full). Items archived before this change get one more version, with the same content, the next time they are read.
- **Calendars** follow the draft: `calendarIds`, `inCalendar`, `CalendarEvent/get` without expanding recurrences (the master with its `recurrenceOverrides`). A server whose session does not advertise calendars is read for contacts only.

## Tests

`src/tests.rs` runs an in-process fake JMAP server (axum) with a well-known redirect, a session with or without calendars, `AddressBook`/`Calendar` `/get`, `ContactCard`/`CalendarEvent` `/get`, `/query` and `/changes` with generations and optional one-item pages, and bearer checks. It serves an event source with `StateChange`s on every change, and runs the watch conformance suite and a check that a session without calendars asks only for contact types and that silence ends a watch. It runs the conformance suite (the fake wraps the suite's vCards and iCalendar objects in a JSON object and answers `Remote::stored` with it), again with one-item pages; further tests cover a server without calendars, names, colors and the JSON kept with its keys sorted (the same object in another key order reads the same), a card moved to another address book, the well-known address asked without credentials, an API address on another host used with the credentials, an API address without https refused with nothing sent there, waiting on `503`, a refused password, plain http and an unknown collection, and a server that ignores `position` and does not move its state on, refused after its first repeat. Against Stalwart 0.16 through the real binary: [`tests/integration/tests/jmap_pim.rs`](../../../tests/integration/tests/jmap_pim.rs) (first sync, changes, removals, a removed address book, a state Stalwart cannot calculate changes from, a session served from another host whose `apiUrl` names Stalwart). A test against a TLS server in the process with a certificate from `holtmail_core::tls::testing::TestCa` checks that its CA is trusted only through `with_roots`.
