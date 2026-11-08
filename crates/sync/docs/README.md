# holtmail-sync

Archives an account's folders incrementally through a [`Source`](../../source/docs/README.md) and the [ingest pipeline](../../ingest/docs/README.md).

## Usage

```rust
let syncer = Syncer::new(store, ingest);
let report = syncer.sync_account(&account, &*source, &SyncOptions::default()).await?;
```

`SyncOptions { include, exclude, batch: 50, max_attempts: 5, flag_scan_interval: 24 h, flag_batch: 1000, min_age: None, only: None }`. A folder whose source state says nothing changed is still looked at when its daily flag scan is due (no mod-sequences), since the state may have been taken by a sync that did not read flags. `only` limits one run to the named folder paths on top of `include` and `exclude` (a run a watch started); which folders count as Gmail labels is decided as in a full run. `SyncReport` has one `FolderReport { path, new, known, removed, flags_changed, failed, deleted, deferred, renumbered, unchanged, error }` per folder; `unchanged` says the folder was skipped because its source state said nothing changed.

## Only mail older than N days

With `min_age` set, a message is archived only once it is at least that old: by when the server received it (`INTERNALDATE`), or by its `Date` header when the server gives no received time; a message with neither is archived. The UID cursor is the only record of how far a folder got, so it must never pass a message that was not archived. A folder's sync therefore stops at the first new message that is too young: the messages before it in its batch are archived and the cursor stops just below it; it and every later UID are left on the server and counted in `deferred`. A later sync starts again from there and moves on as messages age. Since UIDs grow with arrival (on JMAP, with the received time), the later messages are usually younger anyway; an older message moved into the folder after a young one waits until the young one is old enough. The young message's body is fetched once per sync to read its dates (one batch at most per folder), as the source contract has no date-only fetch. Retries of earlier failures and flag changes are not affected.

## Gmail labels

When the account has a folder with the `All` role that the rules let through, folders the source reports as labels (`RemoteFolder.label`, Gmail's Inbox, Sent, Starred and user labels) are skipped: their messages arrive once from All Mail, with their labels attached through `RemoteMessage.labels`. Trash and Spam are not in All Mail and still sync as folders. A label folder named in `include` is synced anyway, and when All Mail is excluded, label folders sync as before.

Gmail accounts archived before labels existed keep the location rows of their label folders (nothing is deleted); those folders simply stop being synced, so their counts stay as they were. The first sync afterwards archives All Mail: messages already archived from a label folder are recognised by their bytes and only gain an All Mail location and their labels.

## How a folder syncs

1. Select it read-only and compare `UIDVALIDITY` with the saved cursor. When it changed, the folder's UIDs are forgotten and every message is looked at again; content addressing recognises messages already archived, so nothing is duplicated.
2. When the cursor holds a source state (`FolderCursor::state`, a Graph delta link) and the source answers `unchanged_since(state)` with a token, nothing changed: the token is saved, `synced_at` set, and the folder is done without listing, fetching or a flag read. Otherwise the stored state is dropped (and saved as dropped) before anything else, so an interrupted sync never skips unfinished work. Sources without state (IMAP, JMAP) always go on.
3. List the folder's UIDs. Archived UIDs missing from the server are marked removed: the message stays in the archive, only its location records when it left. Its search document is refreshed.
4. Fetch UIDs above the cursor, plus earlier failures that have been tried fewer than `max_attempts` times, in batches of `batch`. When the folder says its UIDs do not ascend with arrival (`FolderStatus::uids_ascend` is `false`, as on JMAP), fetch instead every UID on the server that has no live location in the folder yet, except those that failed `max_attempts` times; the cursor then only bounds the flag reads.
5. Take in flag and label changes of messages archived before (UIDs up to the cursor), see below, and save the cursor. Then confirm held changes (below).
6. Ingest each new message; with `min_age`, stop at the first one that is too young (see above). Bytes that are not a message are recorded as a failure with the reason and the sync moves on; one broken message never stops a folder from advancing.
7. Save the cursor and commit the index after every batch, so an interrupted sync resumes where it stopped.
8. At the end, store the source's `state()` with the cursor only when nothing was left behind: nothing deferred by `min_age` and no failure on the server that will be retried. Otherwise the state stays empty and the next sync lists the folder again, so a message waiting for its age or a retry is never skipped by an "unchanged" answer.

## Live arrivals

A message is ingested with `Placement::live` when its folder had finished a sync before (`FolderCursor::synced_at` set when the run started): the folder is being followed, so the message is new mail, not a backfill. A first sync, and the rest of a first sync that was interrupted, are not live; a folder that was empty at its first sync is followed from then on. Test: `only_mail_after_a_finished_sync_is_live`.

## Flag changes

Messages read, flagged or relabelled in another client after archiving are updated in place (`MessageStore::set_flags`, `LabelStore::set_labels`) and their search documents rebuilt; nothing is ever deleted.

- **With mod-sequences** (`CONDSTORE`, the folder reports `HIGHESTMODSEQ`): when it moved past the cursor's `highest_modseq`, one `flags(1..=last_uid, changed_since)` returns only what changed. An unchanged mod-sequence costs nothing.
- **Without**: every archived UID's flags are read again once per `flag_scan_interval` (a day by default), `flag_batch` UIDs per command, and `flags_checked_at` records when. Between scans nothing is read, so a change shows up within a day.
- A folder synced for the first time records the current mod-sequence and time: its messages were just fetched with fresh flags. A folder archived by an older Holtmail (no `flags_checked_at` yet), a server that starts reporting mod-sequences, or one that reports a lower one than saved gets one full read first.

## Confirming held changes

After the flag changes, the folder's messages with a `held` or `sending` flags or labels change in `mail_changes` (written by the API and sent by `mail-writeback`) get one more flag read each, `flags(uid..=uid, None)`, and what it shows goes to `MailChangeStore::confirm_mail_flags` with the time taken before the read, and into the archive like any flag change. A change the server shows is `applied`; a change sent before the read that the server contradicts is `superseded_by_server`: the server wins. Folders without open changes cost one store query; at most 500 open changes per account are looked at per sync, and the rest are settled by `mail-writeback` after a while.

A connection that breaks inside a folder ends that folder with `error` set and continues with the next one. Rejected credentials end the whole sync with `SyncError::Source(SourceError::Auth)`. Failures of Holtmail's own storage end it with `Store` or `Ingest`.

## Compared with date-window syncing

Holtmail never re-downloads mail it already has in a folder whose `UIDVALIDITY` is unchanged: only UIDs above the cursor are fetched. Mail-Archiver re-fetches a 12-hour window on every run and stalls its checkpoint on a single broken message; both are avoided here.

## Not yet

- `QRESYNC` `VANISHED` to notice removals without listing every UID.
- Recognising known messages after renumbering by `Message-ID` and size before downloading them.

Messages deleted from the archive on purpose stay deleted: `folder_uids` lists the UIDs that held them, so neither an IMAP folder nor a JMAP or Graph folder (which fetch every UID without a live location) asks for them again. After a renumbering they are fetched once more, refused by ingest (`IngestError::Deleted`, counted in `deleted`) and their new UIDs remembered. Test: `never_brings_back_a_deleted_message`, with ordered and unordered UIDs.
