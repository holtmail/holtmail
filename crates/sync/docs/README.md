# holtmail-sync

Archives an account's folders incrementally through a [`Source`](../../source/docs/README.md) and the [ingest pipeline](../../ingest/docs/README.md).

## Usage

```rust
let syncer = Syncer::new(store, ingest);
let report = syncer.sync_account(&account, &*source, &SyncOptions::default()).await?;
```

`SyncOptions { include, exclude, batch: 50, max_attempts: 5 }`. `SyncReport` has one `FolderReport { path, new, known, removed, failed, renumbered, error }` per folder.

## How a folder syncs

1. Select it read-only and compare `UIDVALIDITY` with the saved cursor. When it changed, the folder's UIDs are forgotten and every message is looked at again; content addressing recognises messages already archived, so nothing is duplicated.
2. List the folder's UIDs. Archived UIDs missing from the server are marked removed: the message stays in the archive, only its location records when it left. Its search document is refreshed.
3. Fetch UIDs above the cursor, plus earlier failures that have been tried fewer than `max_attempts` times, in batches of `batch`.
4. Ingest each message. Bytes that are not a message are recorded as a failure with the reason and the sync moves on; one broken message never stops a folder from advancing.
5. Save the cursor and commit the index after every batch, so an interrupted sync resumes where it stopped.

A connection that breaks inside a folder ends that folder with `error` set and continues with the next one. Rejected credentials end the whole sync with `SyncError::Source(SourceError::Auth)`. Failures of Holtmail's own storage end it with `Store` or `Ingest`.

## Compared with date-window syncing

Holtmail never re-downloads mail it already has in a folder whose `UIDVALIDITY` is unchanged: only UIDs above the cursor are fetched. Mail-Archiver re-fetches a 12-hour window on every run and stalls its checkpoint on a single broken message; both are avoided here.

## Not yet

- Flag changes on messages already archived (needs `CONDSTORE` `CHANGEDSINCE`).
- Gmail: syncing `[Gmail]/All Mail` with `X-GM-LABELS` instead of every label folder, to avoid downloading the same message once per label.
- Recognising known messages after renumbering by `Message-ID` and size before downloading them.
