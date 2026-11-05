# holtmail-store-sqlite

A [`Store`](../../store/docs/README.md) in one SQLite database file. The default for single-machine installs; with message bytes in a blob store, the database stays small (roughly 1 KB per message).

## Usage

```rust
let store = SqliteStore::open("/var/lib/holtmail/holtmail.db").await?;
```

`open` creates the file when missing and applies the migrations in `migrations/` (embedded into the binary).

```rust
store.snapshot("/backups/holtmail.db").await?; // a consistent copy, safe while others read and write
store.close().await;                            // wait for every connection to close
```

`snapshot(path)` writes a copy of the whole database with `VACUUM INTO`: it holds every write committed before it started and none after, other connections keep working meanwhile, and it refuses a path that exists. `holtmail backup` uses it. `close()` closes every connection so the file is complete on disk; calls after it fail.

## Settings

WAL journal, `synchronous = NORMAL`, foreign keys on, a 10 second busy timeout, up to 8 connections.

## Guarantees and limits

- Reads run in parallel. Writes are serialized inside the process, so one database file must be used by one Holtmail process at a time. Use `store-postgres` for several processes.
- Ids are stored as UUID text, times as Unix milliseconds, addresses and flags as JSON text.
- Message lists use the `(archive_id, date DESC, id DESC)` index and keyset pagination; there is no `OFFSET` and no total count per page.
- `message_extras` answers a page with two queries per 200 ids (thread sizes through the `(thread_id, …)` index, locations through `locations_message`). Unread counts read the flags JSON with `json_each`, so they cost one pass over the folder's locations.
- UIDs, sizes and modseqs above `i64::MAX` are rejected with `StoreError::Invalid`.
- `delete_archive` runs in one transaction and checks each of the archive's blobs against every remaining message, attachment, remote copy and derived record before listing it as orphaned. Grants live in `archive_grants` and cascade with users and archives.
- Settings live in `settings`. Migration 0005 adds `archives.save_remote_images_since` (set to the archive's creation for archives that already saved images) and an index on `(archive_id, archived_at)` for scans by age.
- Remote copies live in `remote_images`, keyed by URL key; `message_remote_images` links messages to them and `remote_scans` marks messages already looked at. `delete_archive` removes the links of the archive's messages, then each copy no other message links, and treats its blob like any other: orphaned only when no message, attachment or other copy uses it.
- Labels live in `labels` (unique per `(archive_id, name)`) and `message_labels`, added by migration 0006 together with `folder_cursors.flags_checked_at`. `set_labels` runs in one transaction and deletes labels left without messages; `list_labels` counts per label with one grouped query and reads `\Seen` from the locations' flags JSON; the label filter of `list_messages` joins `message_labels` and keeps the keyset pagination. `set_flags` stores the flags sorted and deduplicated.
- Derived records live in `derived` (migration 0007), keyed by `(source_blob, kind)` with a nullable `blob_id` for "nothing can be made". `delete_archive` drops the records of every blob it orphans and lists their derived blobs too when nothing else uses them; a blob counts as used when a message, attachment, remote copy or derived record names it. `clear_derived` runs in one transaction.
- Tasks live in `tasks` and `task_log` (migration 0009), the log keyed by `(task_id, seq)` with a per-task counter `tasks.log_next`; `append_task_log` inserts the lines and deletes those between the first 50 and the last 450 in one transaction, so a log never grows past 500 rows. `recent_tasks` reads one page of `tasks` by `started_at` and each task's log by key; `interrupt_tasks` is one `UPDATE … RETURNING`; `prune_tasks` drops finished tasks past `keep` with their logs in one transaction. Each account's last run is one row in `account_runs` (same migration), removed with the account and with its archive.
- `referenced_blobs` takes the next `limit` distinct ids above `after` from each of `messages`, `attachments`, `remote_images` and `derived` through their `blob_id` indexes (migration 0008 adds the one on `messages`), merges them and keeps the first `limit`, so each page costs four short index range scans whatever the database size. Pages hold at most 10 000 ids.
- User emails are unique through an `email_key` column holding the trimmed, Unicode-lowercased email. Sessions are keyed by the 32-byte token hash and are deleted with their user.
