# holtmail-store

The contract for Holtmail's metadata: archives, accounts, folders, messages with their attachments, locations, labels and threads, sync progress, and users with their sign-in sessions. The store never holds message bytes; it references blobs by `BlobId`. Everything except archives, accounts, sync state and users can be rebuilt from the blobs.

## Traits

`Store` is `ArchiveStore + MessageStore + LabelStore + SyncStore + UserStore + RemoteImageStore + SettingsStore + DerivedStore + InventoryStore + TaskStore`, implemented automatically for any type that implements them all. Use it as `Arc<dyn Store>`.

### ArchiveStore

| Method | Guarantees |
| --- | --- |
| `create_archive`, `get_archive`, `list_archives` | Lists are oldest first |
| `rename_archive(id, name)` | `NotFound` for a missing archive |
| `set_save_remote_images(id, save)` | Sets `Archive.save_remote_images` (off for new archives). Turning it on records `save_remote_images_since` unless it was already on; turning it off clears it. `NotFound` for a missing archive |
| `delete_archive(id)` | Removes the archive with its accounts, folders, messages, attachments, labels, sync state and grants, the remote copies no remaining message links, and the derived records of blobs no longer used. Returns the blobs no message, attachment, remote copy or derived record references any more, so the caller can delete them. `NotFound` for a missing archive |
| `archive_stats` | Messages, attachments, total raw bytes, oldest and newest date; zeros for an empty archive |
| `create_account` | `NotFound` when the archive does not exist |
| `get_account`, `list_accounts`, `update_account` | `update_account` replaces everything except `id`, `archive_id`, `kind`, `created_at` |
| `delete_account(id)` | Removes the account with its folders, locations, cursors and failures; its messages stay in the archive. `NotFound` for a missing account |
| `upsert_folder(account, path, role)` | Idempotent per `(account, path)`; a new role replaces the stored one |
| `list_folders(archive)` | Every folder of the archive's accounts with the count of messages located there and not removed, and of those without `\Seen` (any letter case), by account then path |

`Account.settings` is JSON owned by the source module (host, port, folder rules) and the server (the sync interval, and `oauth: { provider, needs_sign_in }` for accounts that sign in with Google or Microsoft). `Account.sealed_secret` is opaque bytes sealed by `holtmail-secret`: the password, or for OAuth accounts the refresh token; the store never looks inside and the field is skipped when an account is serialized. How an account signs in therefore needs no column of its own.

### MessageStore

| Method | Guarantees |
| --- | --- |
| `insert_message` | Idempotent per `(archive, blob)`: the same bytes return the existing message with `created: false`. The same bytes in another archive are a new message |
| `get_message`, `get_messages(ids)` | `get_messages` keeps the order of `ids` and skips missing ones |
| `find_by_blob(archive, blob)` | The message with these bytes in the archive |
| `attachments(message)` | By ordinal; inline parts included |
| `list_messages(query)` | Newest first by `date`, ties by id; keyset pagination with an opaque `PageCursor`; `limit` capped at 500; optional folder filter (located and not removed) and label filter (exact name); both may be given |
| `message_extras(ids, folder)` | One batched answer for a list page: for each existing message, its flags (sorted, deduplicated; in `folder` when given, else the union over locations not removed), the number of messages in its thread, the folders it is in and not removed from (by id) and its labels (sorted). Missing ids are left out |
| `thread_messages(thread)` | Oldest first |
| `record_location(message, folder, uid, flags, seen_at)` | One location per `(folder, message)`; updates UID, flags and `last_seen_at`, clears `removed_at`; a UID held by another message moves to this one |
| `locations`, `folder_uids`, `message_at_uid` | `folder_uids` lists only locations not removed, ascending |
| `mark_removed(folder, uids, at)` | Marks locations removed; the message stays archived |
| `forget_uids(folder)` | Clears every UID in the folder after a `UIDVALIDITY` change; locations stay |
| `set_flags(folder, uid, flags)` | Replaces the flags of the location at the UID (removed or not), e.g. after the message was read elsewhere. Flags compare as a set; returns whether they changed, `false` for an unknown UID |

### LabelStore

Labels as Gmail has them. A label belongs to an archive and is known by its name; nested labels use `/` (`Projects/2025`). A message can carry many.

| Method | Guarantees |
| --- | --- |
| `set_labels(message, labels)` | Replaces the message's labels; empty names and duplicates are dropped and labels no message carries any more are forgotten. Returns whether the set changed. `NotFound` for a missing message |
| `labels(message)` | Sorted; empty for an unknown message |
| `list_labels(archive)` | Labels carried by at least one message of the archive, by name, with `messages` and `unread` (messages without `\Seen` in any location not removed, so a message without locations counts as unread, as lists show it) |

`Message.date` is what lists sort by: `sent_at`, else `received_at`, else `archived_at`. `attachment_count` counts attachments that are not inline.

### Threads

A new message joins every thread that contains a message whose `Message-ID` is among its `parent_ids`, a message that names its `Message-ID` as a parent, or a message that shares one of its parents. When several threads are touched they merge into the oldest one. The outcome does not depend on the order messages arrive in, which matters for imports and for syncing folders in parallel. Threads never cross archives.

### SyncStore

| Method | Guarantees |
| --- | --- |
| `cursor(folder)` / `set_cursor` | The default cursor for a folder that never synced; `set_cursor` replaces the whole cursor |
| `record_failure(folder, uid, reason, at)` | Adds one to `attempts` and keeps the latest reason and time |
| `clear_failure`, `failures` | Failures by UID |
| `set_last_run(account, run)` | Replaces the account's `AccountRun { finished_at, new_messages, error }`; removed with the account or its archive; `NotFound` for an unknown account |
| `last_runs()` | The last run of every account that has one |

`FolderCursor` holds `uid_validity`, `last_uid`, `highest_modseq` (the mod-sequence up to which flag changes were taken in), `flags_checked_at` (when every archived message's flags were last read again, for servers without mod-sequences), an opaque provider `state` (JMAP state string, Graph delta link) and `synced_at`. Failures let a sync move past a broken message instead of stalling on it, and retry it later.

### UserStore

| Method | Guarantees |
| --- | --- |
| `count_users`, `get_user`, `list_users` | `list_users` is oldest first |
| `create_user(NewUser)` | `Invalid` when another user has the same email, compared without regard to letter case; the email is kept as entered |
| `find_user_by_email` | Case-insensitive |
| `update_user(user)` | Replaces email, name and role; `NotFound` for a missing user, `Invalid` when another user has the email (any letter case) |
| `delete_user(id)` | Removes the user with their sessions and grants; `NotFound` for a missing user |
| `grants(user)`, `set_grants(user, archives)` | The archives a member may read, oldest first; `set_grants` replaces them, skips unknown archives and answers `NotFound` for a missing user. Grants go away with their archive or user |
| `password_hash(user)`, `set_password_hash(user, hash)` | `NotFound` for a missing user; `None` for a user without a password |
| `create_session(hash, user, expires_at)` | `NotFound` for a missing user, `Invalid` for a hash already in use |
| `session_user(hash, now)` | The session's user, `None` when unknown or expired (`expires_at <= now`) |
| `delete_session`, `delete_user_sessions` | Idempotent |
| `delete_expired_sessions(now)` | Removes sessions with `expires_at <= now`, returns how many |

`User` has `id`, `email`, `name`, `role` (`InstanceRole::Owner | Admin | Member`) and `created_at`; the password hash is not part of it and is read only through `password_hash`. Hashes are opaque PHC strings made by `holtmail-auth`. Sessions are keyed by `SessionHash` (`[u8; 32]`, the hash of the token); the store never sees a token.

### RemoteImageStore

Copies of remote images linked from messages, shared by every message and archive that links the same URL. A `RemoteCopy` has the `key` (BLAKE3 hex of the absolute URL, see [`holtmail-sanitize`](../../sanitize/docs/README.md)), `url`, and either `blob_id`, `content_type` and `size` of the saved bytes or an `error`, plus `fetched_at`.

| Method | Guarantees |
| --- | --- |
| `remote_copy(key)` | The copy or failure recorded for the key, `None` when never fetched |
| `save_remote_copy(copy)` | Records it, replacing what was recorded for the key |
| `unscanned_messages(archive, when, limit)` | Messages of the archive archived `when` (`ArchivedWhen::Any`, `Since(t)`, `Before(t)`, by `archived_at`) whose remote images were never looked at, oldest first |
| `count_unscanned(archive, when)` | How many of those there are |
| `remote_copy_totals(archive)` | Saved images (`images`, `bytes`) and failed ones the archive's messages link to, each counted once |
| `failed_remote_copies(archive, tried_before, limit)` | Failed copies the archive's messages link to, last tried before `tried_before`, oldest attempt first |
| `record_remote_scan(message, keys)` | Marks the message as looked at and links it to the recorded copies among `keys`; unknown keys are ignored; idempotent. `NotFound` for a missing message |
| `message_remote_copies(message)` | The linked copies that have saved bytes |

### SettingsStore

Instance-wide settings as text by key: `setting(key)` (`None` when unset) and `set_setting(key, value)` (`None` removes; idempotent). Values are opaque to the store; the server seals secrets before storing them.

### DerivedStore

Files made from attachment blobs and kept as blobs of their own: thumbnails and extracted text (`DerivedKind::{Thumbnail, Text}`). Everything here is derived data and can be dropped and made again from the source blobs.

| Method | Guarantees |
| --- | --- |
| `derived(source, kind)` | `None` when nothing was recorded; `Some(Derived::Blob(id))` for a stored result; `Some(Derived::Nothing)` when nothing can be made, so callers do not try again |
| `set_derived(source, kind, derived)` | Replaces any earlier record of that kind; idempotent. Several sources may point at the same derived blob |
| `clear_derived(kind)` | Forgets every record of the kind and returns the derived blobs nothing references any more, each once |

### TaskStore

Background tasks (syncs, imports, remote image work) kept across restarts, so the server can list them again and tell which ones a restart stopped.

| Method | Guarantees |
| --- | --- |
| `save_task(record)` | Inserts or replaces the `TaskRecord` with that id; its log stays. Idempotent |
| `append_task_log(id, lines)` | Adds the lines in order; the log then keeps its first `TASK_LOG_HEAD` (50) lines and its most recent ones, `TASK_LOG_LINES` (500) in all, exactly as if they were added one by one; `NotFound` for an unknown task |
| `recent_tasks(limit)` | Up to `limit` `StoredTask { record, log }`, most recently started first, logs oldest line first |
| `interrupt_tasks(at)` | Every `Running` task becomes `Interrupted` with `finished_at = at`; returns them. Meant for a process start, before any task of its own runs |
| `prune_tasks(keep)` | Removes the tasks that are not running, with their logs, except the `keep` most recently started; returns how many went |

`TaskRecord` holds `id`, `kind` (`TaskKind::{Sync, Import, RemoteImages}`), `archive_id`, `account_id`, `title`, `state` (`TaskState::{Running, Done, Failed, Cancelled, Interrupted}`), `started_at`, `finished_at`, `done`, `total`, `summary`, `cancellable` and `job`, an opaque string the caller uses to start interrupted work again. `TaskLine` is `{ at, level: TaskLevel::{Info, Warning, Error}, text }`; lines carry counts, names and reasons, never message content or secrets. The kinds, states and levels have stable names (`as_str`, `parse`). Task records name archives and accounts without referencing them: a task outlives a removed archive.

### InventoryStore

What the store references in the blob store, for backups and checks after a restore.

| Method | Guarantees |
| --- | --- |
| `referenced_blobs(after, limit)` | Up to `limit` blobs some record names: a message, an attachment, a saved remote image or a derived file. Each blob once, ascending by id, strictly after `after` (from the start when `None`); paging with the last id of each page visits every referenced blob exactly once, and a page shorter than `limit` is the last |

## Errors

`StoreError::NotFound(what)`, `StoreError::Invalid(reason)` (a malformed cursor, a value out of range, corrupt stored data) and `StoreError::Backend(..)` for failures that may succeed on retry.

## Writing an implementation

1. Create `crates/store-<kind>` depending on `holtmail-store`.
2. Implement the traits.
3. Run the conformance suite, which also exports `testing::message(archive, n, sent_secs)` to build invented messages:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_store_contract() {
    let store = MyStore::open(/* empty database */).await.unwrap();
    holtmail_store::testing::conformance(Arc::new(store)).await;
}
```

## Implementations

- [holtmail-store-sqlite](../../store-sqlite/docs/README.md) — one SQLite file
