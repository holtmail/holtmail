# holtmail-store-sqlite

A [`Store`](../../store/docs/README.md) in one SQLite database file. The default for single-machine installs; with message bytes in a blob store, the database stays small (roughly 1 KB per message).

## Usage

```rust
let store = SqliteStore::open("/var/lib/holtmail/holtmail.db").await?;
```

`open` creates the file when missing and applies the migrations in `migrations/` (embedded into the binary).

## Settings

WAL journal, `synchronous = NORMAL`, foreign keys on, a 10 second busy timeout, up to 8 connections.

## Guarantees and limits

- Reads run in parallel. Writes are serialized inside the process, so one database file must be used by one Holtmail process at a time. Use `store-postgres` for several processes.
- Ids are stored as UUID text, times as Unix milliseconds, addresses and flags as JSON text.
- Message lists use the `(archive_id, date DESC, id DESC)` index and keyset pagination; there is no `OFFSET` and no total count per page.
- `message_extras` answers a page with two queries per 200 ids (thread sizes through the `(thread_id, …)` index, locations through `locations_message`). Unread counts read the flags JSON with `json_each`, so they cost one pass over the folder's locations.
- UIDs, sizes and modseqs above `i64::MAX` are rejected with `StoreError::Invalid`.
- `delete_archive` runs in one transaction and checks each of the archive's blobs against every remaining message and attachment before listing it as orphaned. Grants live in `archive_grants` and cascade with users and archives.
- User emails are unique through an `email_key` column holding the trimmed, Unicode-lowercased email. Sessions are keyed by the 32-byte token hash and are deleted with their user.
