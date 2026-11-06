# holtmail-core

Small types that several contracts share, so that ids and times mean the same thing in the store, the index, the API and the web app.

## Ids

`ArchiveId`, `AccountId`, `FolderId`, `MessageId`, `ThreadId`, `UserId`, and for contacts and calendars `CollectionId` (an address book or a calendar) `ItemId` (a contact, event or task with all its versions) and `PersonOverrideId` (a split or join of people made by hand).

- UUIDv7: ids created later sort after ids created earlier, so lists ordered by id are ordered by creation.
- `new()` creates an id without asking the database, which keeps every store implementation free to pick its storage.
- Displays, parses (`FromStr`, `InvalidId` on failure) and serializes as the hyphenated lowercase UUID string.
- Each id is its own type; a `FolderId` cannot be passed where a `MessageId` is expected.

`PersonId` names a person derived from contacts and mail (see [`holtmail-people`](../../people/docs/README.md)). It is not created but computed: `PersonId::from_hash(bytes)` makes a UUIDv8 from 16 bytes of a hash, so the same group of contacts and addresses gets the same id in every rebuild. It displays, parses and serializes like the others.

`EventGroupId` names the copies of one calendar event in an archive: every event item with the same iCalendar `UID` (see [`holtmail-calendar`](../../calendar/docs/README.md)). Like `PersonId` it is computed with `EventGroupId::from_hash(bytes)`, so the same archive and `UID` always give the same id.

## Timestamp

Milliseconds since the Unix epoch, UTC, as `i64`. Serializes as a plain integer. `now()`, `from_millis`, `from_secs`, `as_millis`, `as_secs` (rounds down, also for times before 1970). Formatting for people happens at the edges, in the web app.

## Address

`Address { name: Option<String>, email: String }`: one mailbox from an address header, as parsed by `holtmail-mime` and stored by `holtmail-store`.
