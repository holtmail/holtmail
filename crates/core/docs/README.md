# holtmail-core

Small types that several contracts share, so that ids and times mean the same thing in the store, the index, the API and the web app.

## Ids

`ArchiveId`, `AccountId`, `FolderId`, `MessageId`, `ThreadId`, `UserId`.

- UUIDv7: ids created later sort after ids created earlier, so lists ordered by id are ordered by creation.
- `new()` creates an id without asking the database, which keeps every store implementation free to pick its storage.
- Displays, parses (`FromStr`, `InvalidId` on failure) and serializes as the hyphenated lowercase UUID string.
- Each id is its own type; a `FolderId` cannot be passed where a `MessageId` is expected.

## Timestamp

Milliseconds since the Unix epoch, UTC, as `i64`. Serializes as a plain integer. `now()`, `from_millis`, `from_secs`, `as_millis`, `as_secs` (rounds down, also for times before 1970). Formatting for people happens at the edges, in the web app.

## Address

`Address { name: Option<String>, email: String }`: one mailbox from an address header, as parsed by `holtmail-mime` and stored by `holtmail-store`.
