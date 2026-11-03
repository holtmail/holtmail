# holtmail-store

The contract for Holtmail's metadata: archives, accounts, folders, messages with their attachments, locations and threads, sync progress, and users with their sign-in sessions. The store never holds message bytes; it references blobs by `BlobId`. Everything except archives, accounts, sync state and users can be rebuilt from the blobs.

## Traits

`Store` is `ArchiveStore + MessageStore + SyncStore + UserStore`, implemented automatically for any type that implements the four. Use it as `Arc<dyn Store>`.

### ArchiveStore

| Method | Guarantees |
| --- | --- |
| `create_archive`, `get_archive`, `list_archives` | Lists are oldest first |
| `rename_archive(id, name)` | `NotFound` for a missing archive |
| `set_save_remote_images(id, save)` | Sets `Archive.save_remote_images` (off for new archives); `NotFound` for a missing archive |
| `delete_archive(id)` | Removes the archive with its accounts, folders, messages, attachments, sync state and grants, and the remote copies no remaining message links. Returns the blobs no message, attachment or remote copy references any more, so the caller can delete them. `NotFound` for a missing archive |
| `archive_stats` | Messages, attachments, total raw bytes, oldest and newest date; zeros for an empty archive |
| `create_account` | `NotFound` when the archive does not exist |
| `get_account`, `list_accounts`, `update_account` | `update_account` replaces everything except `id`, `archive_id`, `kind`, `created_at` |
| `delete_account(id)` | Removes the account with its folders, locations, cursors and failures; its messages stay in the archive. `NotFound` for a missing account |
| `upsert_folder(account, path, role)` | Idempotent per `(account, path)`; a new role replaces the stored one |
| `list_folders(archive)` | Every folder of the archive's accounts with the count of messages located there and not removed, and of those without `\Seen` (any letter case), by account then path |

`Account.settings` is JSON owned by the source module (host, port, folder rules). `Account.sealed_secret` is opaque bytes sealed by `holtmail-secret`; the store never looks inside and the field is skipped when an account is serialized.

### MessageStore

| Method | Guarantees |
| --- | --- |
| `insert_message` | Idempotent per `(archive, blob)`: the same bytes return the existing message with `created: false`. The same bytes in another archive are a new message |
| `get_message`, `get_messages(ids)` | `get_messages` keeps the order of `ids` and skips missing ones |
| `find_by_blob(archive, blob)` | The message with these bytes in the archive |
| `attachments(message)` | By ordinal; inline parts included |
| `list_messages(query)` | Newest first by `date`, ties by id; keyset pagination with an opaque `PageCursor`; `limit` capped at 500; optional folder filter (located and not removed) |
| `message_extras(ids, folder)` | One batched answer for a list page: for each existing message, its flags (sorted, deduplicated; in `folder` when given, else the union over locations not removed), the number of messages in its thread and the folders it is in and not removed from (by id). Missing ids are left out |
| `thread_messages(thread)` | Oldest first |
| `record_location(message, folder, uid, flags, seen_at)` | One location per `(folder, message)`; updates UID, flags and `last_seen_at`, clears `removed_at`; a UID held by another message moves to this one |
| `locations`, `folder_uids`, `message_at_uid` | `folder_uids` lists only locations not removed, ascending |
| `mark_removed(folder, uids, at)` | Marks locations removed; the message stays archived |
| `forget_uids(folder)` | Clears every UID in the folder after a `UIDVALIDITY` change; locations stay |

`Message.date` is what lists sort by: `sent_at`, else `received_at`, else `archived_at`. `attachment_count` counts attachments that are not inline.

### Threads

A new message joins every thread that contains a message whose `Message-ID` is among its `parent_ids`, a message that names its `Message-ID` as a parent, or a message that shares one of its parents. When several threads are touched they merge into the oldest one. The outcome does not depend on the order messages arrive in, which matters for imports and for syncing folders in parallel. Threads never cross archives.

### SyncStore

| Method | Guarantees |
| --- | --- |
| `cursor(folder)` / `set_cursor` | The default cursor for a folder that never synced; `set_cursor` replaces the whole cursor |
| `record_failure(folder, uid, reason, at)` | Adds one to `attempts` and keeps the latest reason and time |
| `clear_failure`, `failures` | Failures by UID |

`FolderCursor` holds `uid_validity`, `last_uid`, `highest_modseq`, an opaque provider `state` (JMAP state string, Graph delta link) and `synced_at`. Failures let a sync move past a broken message instead of stalling on it, and retry it later.

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
| `unscanned_messages(archive, limit)` | Messages of the archive whose remote images were never looked at, oldest first |
| `record_remote_scan(message, keys)` | Marks the message as looked at and links it to the recorded copies among `keys`; unknown keys are ignored; idempotent. `NotFound` for a missing message |
| `message_remote_copies(message)` | The linked copies that have saved bytes |

## Errors

`StoreError::NotFound(what)`, `StoreError::Invalid(reason)` (a malformed cursor, a value out of range, corrupt stored data) and `StoreError::Backend(..)` for failures that may succeed on retry.

## Writing an implementation

1. Create `crates/store-<kind>` depending on `holtmail-store`.
2. Implement the four traits.
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
