# holtmail-store

The contract for Holtmail's metadata: archives, accounts, folders, messages with their attachments, locations, labels and threads, sync progress, users with their sign-in sessions, address books and calendars with every version of their contacts and events, and the people derived from contacts and mail. The store never holds message, contact or event bytes; it references blobs by `BlobId`. Everything except archives, accounts, sync state, users, the splits and joins of people made by hand and the history of items (which collection, which versions, when seen and when removed) can be rebuilt from the blobs; the normalized fields of items (`pim_index`) can.

## Traits

`Store` is `ArchiveStore + MessageStore + LabelStore + SyncStore + UserStore + RemoteImageStore + SettingsStore + DerivedStore + InventoryStore + TaskStore + PimStore + PeopleStore + CalendarStore + LedgerStore + DeletionStore + WebhookStore + AliasStore + InsightStore + CategoryStore + WriteStore + SendStore + MailChangeStore + ItemChangeStore`, implemented automatically for any type that implements them all. Use it as `Arc<dyn Store>`.

### ArchiveStore

| Method | Guarantees |
| --- | --- |
| `create_archive`, `get_archive`, `list_archives` | Lists are oldest first |
| `rename_archive(id, name)` | `NotFound` for a missing archive |
| `set_save_remote_images(id, save)` | Sets `Archive.save_remote_images` (off for new archives). Turning it on records `save_remote_images_since` unless it was already on; turning it off clears it. `NotFound` for a missing archive |
| `delete_archive(id)` | Removes the archive with its accounts, folders, messages, attachments, labels, insights, categories, category overrides, trusted authserv-ids, sync state and grants, its transports, identities, drafts and outbox, the remote copies no remaining message links, and the derived records of blobs no longer used. Returns the blobs no message, attachment, remote copy, outbox message, draft or derived record references any more, so the caller can delete them. `NotFound` for a missing archive |
| `archive_stats` | Messages, attachments, total raw bytes, oldest and newest date; zeros for an empty archive |
| `create_account` | `NotFound` when the archive does not exist |
| `get_account`, `list_accounts`, `update_account` | `update_account` replaces everything except `id`, `archive_id`, `kind`, `created_at` |
| `delete_account(id)` | Removes the account with its folders, locations, cursors, failures and trusted authserv-ids; its messages stay in the archive, and its transports and identities stay without an account. `NotFound` for a missing account |
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
| `list_messages(query)` | Newest first by `date`, ties by id; keyset pagination with an opaque `PageCursor`; `limit` capped at 500; optional folder filter (located and not removed), label filter (exact name) and `category` filter (`CategoryFilter::Kind` for the effective kind, `CategoryFilter::Topic` for an effective topic, `health` included when asked for); any may be combined |
| `message_extras(ids, folder)` | One batched answer for a list page: for each existing message, its flags (sorted, deduplicated; in `folder` when given, else the union over locations not removed), the number of messages in its thread, the folders it is in and not removed from (by id), its labels (sorted), `insight_kinds`, the kind bits of its insights (0 without insights), and `kind` and `topic`, its effective kind and stored list topic (never `health`; both `None` before stage B). Missing ids are left out |
| `thread_messages(thread)` | Oldest first |
| `messages_by_rfc_id(archive, ids)` | The archive's messages whose `Message-ID` is one of `ids`, oldest first; empty for no ids |
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
| `delete_user(id)` | Removes the user with their sessions, grants, drafts, identity grants and default identities; `NotFound` for a missing user |
| `grants(user)`, `set_grants(user, archives)` | The granted archives, oldest first; `set_grants` replaces them, skips unknown archives and answers `NotFound` for a missing user. Archives that stay granted keep their role; new ones are granted as `reader`. Grants go away with their archive or user |
| `password_hash(user)`, `set_password_hash(user, hash)` | `NotFound` for a missing user; `None` for a user without a password |
| `create_session(hash, user, expires_at)` | `NotFound` for a missing user, `Invalid` for a hash already in use |
| `session_user(hash, now)` | The session's user, `None` when unknown or expired (`expires_at <= now`) |
| `session_started_at(hash)` | When the session started, `None` when there is no such session; an expired session not yet removed still answers |
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
| `forget_derived(source, kind)` | Forgets one record; returns its derived blob when nothing references it any more. Nothing recorded is not an error |

### TaskStore

Background tasks (syncs, imports, remote image work) kept across restarts, so the server can list them again and tell which ones a restart stopped.

| Method | Guarantees |
| --- | --- |
| `save_task(record)` | Inserts or replaces the `TaskRecord` with that id; its log stays. Idempotent |
| `append_task_log(id, lines)` | Adds the lines in order; the log then keeps its first `TASK_LOG_HEAD` (50) lines and its most recent ones, `TASK_LOG_LINES` (500) in all, exactly as if they were added one by one; `NotFound` for an unknown task |
| `recent_tasks(limit)` | Up to `limit` `StoredTask { record, log }`, most recently started first, logs oldest line first |
| `interrupt_tasks(at)` | Every `Running` task becomes `Interrupted` with `finished_at = at`; returns them. Meant for a process start, before any task of its own runs |
| `prune_tasks(keep)` | Removes the tasks that are not running, with their logs, except the `keep` most recently started; returns how many went |

`TaskRecord` holds `id`, `kind` (`TaskKind::{Sync, Import, RemoteImages, ContactsCalendars, LedgerVerify, DeleteMessages, Analyse}`; `Analyse` is reading stored mail again for derived data such as insights), `archive_id`, `account_id`, `title`, `state` (`TaskState::{Running, Done, Failed, Cancelled, Interrupted}`), `started_at`, `finished_at`, `done`, `total`, `summary`, `cancellable` and `job`, an opaque string the caller uses to start interrupted work again. `TaskLine` is `{ at, level: TaskLevel::{Info, Warning, Error}, text }`; lines carry counts, names and reasons, never message content or secrets. The kinds, states and levels have stable names (`as_str`, `parse`). Task records name archives and accounts without referencing them: a task outlives a removed archive.

### PimStore

Address books and calendars (`Collection`, `CollectionKind::{Contacts, Calendar}`) with their items (`Item`, `ItemKind::{Contact, Event, Task, Other}`) and every version of each item (`ItemVersion`): the raw bytes as a blob, their media type, the source's ETag, when the version was seen, and a picture split off a vCard as a blob of its own. `PimIndex` holds the normalized fields of an item's latest version (title, start and end in UTC, all-day, original time zone, recurrence, `SEQUENCE`, `UID`, organizer, emails, E.164 phones, and the whole normalized view as JSON); it is derived and can be rebuilt. A collection belongs to an archive, and optionally to the account it is read from; access follows the archive, as for messages. The store does not parse vCard or iCalendar: callers (`holtmail-ingest`) pass the normalized fields in.

| Method | Guarantees |
| --- | --- |
| `upsert_collection(new)` | The collection with this archive, account, kind and `remote_id`, created when missing; idempotent; a new name or color replaces the old, and a removed collection is no longer removed. `NotFound` for a missing archive or account, `Invalid` for an account of another archive |
| `get_collection(id)`, `list_collections(archive)` | One collection, or all of an archive with counts of live and removed items, removed collections included, by kind, then name, then id |
| `set_sync_token(id, token)` | Replaces the opaque sync token; `NotFound` for an unknown collection |
| `set_collection_synced(id, token, ctag, at)` | Records a finished sync from the source: the sync token and the collection's tag (`Collection.ctag`, a DAV `getctag`), each replacing the stored one (`None` clears it), and `Collection.synced_at`; `NotFound` for an unknown collection |
| `mark_collection_removed(id, at)` | Marks the collection and its live items removed; nothing is deleted; an earlier removal time is kept |
| `record_version(new)` | Creates the item when missing. Adds a version only when `blob_id` differs from the latest version's; then the item's kind and key and its `PimIndex` follow the new version, numbered 1, 2, …. The same bytes again change nothing, except that a removed item is live again (`restored`) and the item's current ETag becomes the given one. Versions are never removed or rewritten. Returns `RecordedVersion { item_id, number, created_item, new_version, restored }` |
| `record_mail_version(NewMailVersion { version, message_id, mail })` | For a calendar object found in a message (an invitation, update, reply or cancellation). Creates the item when missing. When one of the item's versions already has the bytes, adds nothing and links the message to that version (idempotent; the same invitation forwarded in another message is one version with two messages). Otherwise places the version by `mail.sequence`, then `mail.dtstamp`, then `mail.sent_at` (missing before present), then blob id, and numbers all versions 1, 2, … in that order, so later ones move up by one: the same messages give the same numbered versions whatever order they arrive in. When the new version is the latest, the item's kind, key and `PimIndex` follow `version`; callers that merge several versions replace the index afterwards. `NotFound` for an unknown collection or message, `Invalid` for a message of another archive |
| `version_messages(item)`, `message_versions(message)` | The messages an item's versions were found in (by number, then message id), and the item versions found in a message (by item, then number), as `VersionSource { item_id, number, message_id }`; empty for items not from mail |
| `live_remote_ids(collection)`, `mark_items_removed(collection, ids, at)` | The ids of live items, sorted; marking some removed returns how many were newly marked and keeps earlier times |
| `item_etags(collection)` | The live items' ids with the ETag of their last `record_version`, sorted by id; empty for an unknown collection. A stored version keeps the ETag it was seen with; this is the source's current one, for diffing a listing |
| `get_item(id)`, `item_versions(id)` | The item with its `PimIndex`; its versions by number, oldest first. A version found in mail carries `mail: Some(MailVersionInfo { method, sequence, dtstamp, sent_at })` |
| `list_items(query)` | Items of an archive by `ItemOrder::Title` (case-insensitive, untitled last) or `ItemOrder::Start` (no start last), then id; filters by kind, collection and `RemovedFilter::{Hide, Include, Only}`; keyset pages of at most 500; `Invalid` for a foreign cursor |
| `set_pim_index(item, index)` | Replaces the derived fields, for rebuilding; fields from an older version than the latest (`index.version` below `versions`) are ignored; `NotFound` for an unknown item |
| `item_ids(after, limit)` | Every item id ascending, for rebuilds |

`Collection.write_mode` (`WriteMode::{ReadOnly, Held, WriteThrough}`) says whether devices and the web app may change a collection and whether changes go upstream; a new collection read from an account is `held`; one no account owns ("From mail") and one of an import account are `read_only`, and only `read_only` is allowed for them. `Item.dav_name` is the href a client created the item under, kept once the upstream id is known; `Item::href()` is the DAV name, else `remote_id`. `ItemVersion.origin` is `VersionOrigin::Upstream` for what a source showed and `Writeback` for what upstream held after Holtmail sent it a change, with `change_id` naming the change. See [ItemChangeStore](#itemchangestore).

Removing an account keeps its collections and items (their `account_id` becomes `None` and their write mode `read_only`), like its messages. Removing an archive removes its collections, items, versions and their links to messages and returns the version and picture blobs nothing else uses. A collection with no account from the start, such as each archive's "From mail" calendar, is one no source owns.

### PeopleStore

The people derived from an archive's contacts and mail (see [`holtmail-people`](../../people/docs/README.md)), and what the mail says about a set of addresses. `Person { id, archive_id, name, email, phone, emails, mail_emails, phones, cards: Vec<PersonCard { item_id, collection_id, removed }>, links: Vec<PersonLink { from, to, key }>, mail: Option<PersonMail { messages, from_them, from_me, first_at, last_at }>, differences, removed }`. The store keeps people as given; it never computes them. Addresses compare without regard to ASCII letter case.

| Method | Guarantees |
| --- | --- |
| `mail_addresses(archive)` | `MailAddresses { own, addresses }`: every address of the archive's mail (`From`, `To`, `Cc`, `Bcc`), trimmed and lowercased, sorted, as `MailAddress { email, name, messages, from_them, from_me, first_at, last_at }`. A reverse alias recorded with a sender (`AliasStore`) counts as that sender, with the sender's name. `name` is the display name used most (ties: the one that sorts first); `from_me` counts messages whose `From` is one of `own`. `own` are the archive's account addresses, every `From` of a message located in a folder with role `sent` or carrying the label `Sent`, and the archive's aliases (known or seen in mail). Empty for an archive without mail |
| `replace_people(archive, people)` | Replaces every person of the archive in one transaction: readers see the old set or the new one. `Invalid` when a person belongs to another archive, `NotFound` for an unknown archive |
| `list_people(query)` | `PersonQuery { archive_id, text, source, differences, after, limit }`: by name without regard to letter case, nameless last, then id; `text` matches a name, address or phone substring without regard to case; `source` is `PersonSource::{Contacts, Mail, Collection(id)}` (with a card, from mail alone, with a card in that address book); `differences` keeps people whose current cards disagree. Keyset pages of at most 500; `Invalid` for a foreign cursor |
| `count_people(query)` | How many people the query lists, ignoring paging |
| `get_person(id)` | The person, or `None` |
| `people_by_email(archive, emails)` | `(email as given, person)` for each address that belongs to a person of the archive, in the order given |
| `correspondence(archive, emails, threads, attachments)` | Every message of the archive with one of the addresses (or a reverse alias recorded for one of them), each once: `messages`, `from_them`, `to_them`, `first_at`, `last_at`, `years` (UTC, oldest first), up to `threads` conversations as `ThreadActivity { thread_id, messages, latest, last_at }` (most recently active first) and up to `attachments` `ExchangedAttachment { message_id, date, from_them, attachment }`, not inline and not calendar parts, newest first. Empty for no addresses |
| `items_by_email(archive, kind, emails, limit)` | Items of the kind, removed ones too, whose normalized emails (organizer and attendees for events) include one of the addresses, latest start first, undated last |
| `add_person_override(value)` | Stores a split or join made by hand: `PersonOverride { id: PersonOverrideId, archive_id, kind: PersonOverrideKind::{Join, Split}, keys, from, created_at, created_by }`. `NotFound` for an unknown archive; `Invalid` for an id already stored, a join with fewer than two keys, or a split without keys or without `from` |
| `person_overrides(archive)` | The archive's overrides, oldest first (by id); empty for an unknown archive |
| `get_person_override(id)` | The override, or `None` |
| `remove_person_override(id)` | Forgets it; whether it was stored. The people stay as they are until rebuilt |

The overrides are the only stored state about people and are not derived: they name cards (`item:<item id>`) and addresses (`email:<address>`), never person ids, so they hold through rebuilds (see [`holtmail-people`](../../people/docs/README.md)). The store keeps them as given and never interprets the keys.

Removing an archive removes its people and overrides. People and overrides are not part of backups' blob inventory; they live in the database snapshot (so a backup keeps the overrides), and the people are rebuilt by `holtmail reindex`.

### CalendarStore

The derived calendar (see [`holtmail-calendar`](../../calendar/docs/README.md)): for each event item its `EventSeries { item_id, group_id, sequence, stamp, starts_at, ends_at, recurring, window }` and its stored instances (`NewOccurrence { recurrence_id, starts_at, ends_at, component, start_local, end_local, time_zone, all_day, floating, cancelled, changed }`). The store keeps what it is given; it never expands rules.

| Method | Guarantees |
| --- | --- |
| `replace_series(item, series, occurrences)` | Replaces the item's series and instances in one step; `None` forgets both. `NotFound` for an unknown item, `Invalid` when `series` names another item |
| `series(item)` | The item's series, or `None` |
| `occurrences(query)` | `OccurrenceQuery { archive_id, from, to, collections, removed }`: stored instances ending after `from` (or starting at it when they last no time) and starting before `to`, as `Occurrence { item_id, collection_id, group_id, removed, sequence, stamp, occurrence }`, by start, item, `recurrence_id`. `collections: Some(empty)` lists nothing; `removed: false` leaves out items and collections removed from their source. Items whose stored `window` does not cover all of `from..to` are left out |
| `uncovered_series(query)` | Exactly those left-out repeating items whose `starts_at..ends_at` (no end for endless ones) reaches into the span, as `UncoveredSeries { summary, series, removed }`, by item id |
| `group_members(group)` | Every item of the group, removed ones too, as `GroupMember { summary, series, collection_removed_at }`, by item id; empty for an unknown group |

Removing an archive removes its series and instances. They are derived: `holtmail reindex` rebuilds them from the items' normalized fields.

### InventoryStore

What the store references in the blob store, for backups and checks after a restore.

| Method | Guarantees |
| --- | --- |
| `referenced_blobs(after, limit)` | Up to `limit` blobs some record names: a message, an attachment, an item version or its picture, a saved remote image, an outbox message, a kept draft version, an append in the mail changes, the bytes of a contact or calendar change, or a derived file. Each blob once, ascending by id, strictly after `after` (from the start when `None`); paging with the last id of each page visits every referenced blob exactly once, and a page shorter than `limit` is the last |

### DeletionStore

Deleting messages on purpose. The archive otherwise never forgets; a deleted message leaves tombstones of its bytes' hash in its archive and of every folder and UID it had, so syncs and imports do not bring it back.

| Method | Guarantees |
| --- | --- |
| `delete_messages(archive, ids, actor)` | In one transaction removes the archive's messages among `ids` with their locations, attachments, labels (labels left without messages go), addresses, thread links, remote image links, insights, categories, effective topics and reindex queue entries, and calendar version links; calendar versions read only from them go too, and an item left without versions is removed with its calendar instances. Unsubscribe requests made from them stay, with no message. Records the tombstones and enters each message in the ledger as `deleted` by `actor`. Returns `DeletedMessages { messages, orphans, changed_items, removed_items }`: blobs nothing references any more (the caller deletes them), items to rebuild and items gone. Idempotent; ids not in the archive are skipped |
| `is_deleted(archive, blob)` | Whether those bytes were deleted from the archive |
| `record_deleted_location(folder, uid)` | Remembers a UID that holds a deleted message; idempotent |

`folder_uids` lists tombstoned UIDs too; `mark_removed` drops the tombstones of UIDs the server no longer has; `forget_uids` drops the folder's UID tombstones and keeps those of the bytes; `delete_archive` and `delete_account` drop their tombstones.

### WebhookStore

Webhooks and their deliveries, which survive restarts.

| Method | Guarantees |
| --- | --- |
| `create_webhook(&webhook)` | Adds it with the caller's id (so the secret can be sealed for it first); `Invalid` when the id is taken. Events are stored sorted and once |
| `list_webhooks()` / `get_webhook(id)` | Oldest first / `None` when unknown |
| `update_webhook(&webhook)` | Replaces address, events, sealed secret and switch; `NotFound` when unknown |
| `delete_webhook(id)` | Removes it with all its deliveries; `NotFound` when unknown |
| `enqueue_deliveries(&[NewDelivery])` | Adds them in order, each due at its event's time; all or nothing (`NotFound` for an unknown webhook) |
| `due_deliveries(now, limit)` | Deliveries due at or before `now`, in the order they were added |
| `record_attempt(id, status, error, next_at, delivered_at)` | Counts one attempt and keeps its result; `next_at: None` ends the delivery (delivered or given up); unknown ids are ignored |
| `recent_deliveries(webhook, limit)` | Newest first |
| `prune_deliveries(keep)` | Keeps the newest `keep` finished deliveries per webhook; waiting ones stay |

### AliasStore

What alias detection found per message, the alias services connected to archives and the aliases they list or the owner added (see [ALIASES.md](../../../docs/ALIASES.md)).

| Method | Guarantees |
| --- | --- |
| `set_message_alias(message, Option<&MessageAlias>)` | Replaces what was recorded; `None` records nothing found. Idempotent; addresses stored lowercase. `NotFound` for an unknown message |
| `message_alias(message)` | `MessageAlias { provider, alias, sender, reverse_alias, signed }` or `None` |
| `known_aliases_among(archive, addresses)` | The known aliases among the addresses, compared lowercase |
| `aliases(archive)` | `AliasSummary { address, provider, known, mail }` for every known alias and every alias seen in mail, each once, sorted by address. `AliasMail { messages, senders, signed, first_at, last_at, accounts }` counts messages forwarded to it plus messages addressed (`To`, `Cc`, `Bcc`) to a known alias that no detector recognised; `accounts` are the accounts with a location of those messages, most first |
| `put_alias(archive, &KnownAlias)` / `remove_alias(archive, address)` | Adds or replaces / forgets a known alias (whether there was one); what mail says of it stays |
| `replace_service_aliases(service, aliases, at)` | Makes these the service's aliases: adds and replaces them (with the service's provider), forgets its others, takes over an alias added by hand with the same address, sets `synced_at`, clears `sync_error`. `NotFound` for an unknown service |
| `create_alias_service` / `alias_services(archive)` / `all_alias_services()` / `get_alias_service(id)` / `update_alias_service` | `AliasService { id, archive_id, provider, name, base_url, sealed_key, created_at, synced_at, sync_error }`; the id is the caller's so the key can be sealed for it first (`Invalid` when taken, `NotFound` for an unknown archive); oldest first; `NotFound` for an unknown id on update |
| `delete_alias_service(id, keep_aliases)` | Removes it; its aliases become aliases added by hand, or are forgotten. `NotFound` for an unknown id |
| `own_alias_addresses(archive)` | Every alias of the archive, known or seen in mail, lowercase, sorted |
| `alias_strategies(archive)` | Every reply strategy override as `AliasStrategy { scope, key, strategy }`, sorted by scope (`Alias`, `Domain`, `Service`, see `StrategyScope::as_str`) then key; empty for an unknown archive |
| `put_reverse_alias(archive, alias, contact, reverse_alias, at)` | Records that the reverse alias reaches the contact through the alias (all trimmed, lowercase), replacing the pair of that reverse alias; idempotent. `mail_addresses` and `correspondence` then count the reverse alias as the contact, unless mail already maps it to a sender, which wins. Removing the archive removes the pairs. `NotFound` for an unknown archive |
| `reverse_alias_contact(archive, reverse_alias)` | The contact recorded for the reverse alias, lowercase, or `None` |
| `set_alias_strategy(archive, scope, key, strategy)` | Sets the override for `scope` and `key` (trimmed, lowercase), replacing the one there; `None` removes it, removing a missing one is fine. The strategy name is kept as given; callers check it. Removing the archive removes its overrides. `NotFound` for an unknown archive |

Deleting a message removes its record; deleting an archive removes its records, services and aliases.

### InsightStore

Insights derived from messages (see [INSIGHTS.md](../../../docs/INSIGHTS.md)): one `InsightRow` per message, replaced whole and rebuilt when the extraction fingerprint changes. The store treats `items` and `auth` as opaque JSON text and returns them byte for byte; `signals` and `kinds` keep all 64 bits.

`InsightRow { message, archive, fingerprint, outcome, truncated, from_address, signals, kinds, auth, items, computed_at }`; `InsightOutcome` is `Ok`, `Partial`, `Unparsable` or `Unreadable`, named `ok`, `partial`, `unparsable`, `unreadable` by `as_str` and `parse`. `InsightView { insights, categories }` is everything known about one message's insights, `categories` being its stage-B row with the overrides that set it (`None` before stage B); `InsightSummary { kinds, kind, topic }` is what a list row shows.

| Method | Guarantees |
| --- | --- |
| `set_message_insights(message, row)` | Replaces the message's row whole; idempotent. `NotFound` for an unknown message, `Invalid` when `row.message` is another message or `row.archive` is not its archive |
| `insight_view(message)` | The view with its categories, or `None` when nothing was stored (the API's `pending`). At most 4 indexed reads |
| `insight_views_among(ids)`, `insights_among(ids)` | Views or summaries of the ids that have a row, any number of ids read 200 at a time; others are left out |
| `messages_needing_insights(archive, fingerprint, after, limit)` | Up to `limit` (at most 10,000) messages of the archive without a row or with a row of another fingerprint, ascending by id, strictly after `after`. A row with the current fingerprint is never listed, whatever its outcome, so `unreadable` and `unparsable` messages wait for the next fingerprint. Paging with the last id visits each such message once |
| `count_needing_insights(archive, fingerprint)` | How many messages the walk would list from the start; 0 for an unknown archive |
| `record_unsubscribe(&request)` | Records a one-click unsubscribe request. `NotFound` for an unknown archive or, when `request.message` is set, an unknown message; `Invalid` when that message is in another archive or the id is taken. Nothing is written on an error |
| `last_unsubscribe(archive, list_key)` | The archive's most recent request for the key by `requested_at`, the greatest id among equal times; `None` when there is none or the archive is unknown. Keys compare byte for byte. One indexed read |
| `last_unsubscribes(archive, list_keys)` | `last_unsubscribe` for every key that has a request, keyed by list key; others are left out. Any number of keys, read 200 at a time |

Row states: no row is pending; a row with an empty item list is ready; a row with another fingerprint is outdated and still served until rewritten; `partial`, `unparsable` and `unreadable` are what the outcome says. Deleting a message removes its row; deleting an archive removes its rows.

Unsubscribe requests are the audit of the one outbound action. `UnsubscribeRequest { id, archive, message, list_key, requested_at, requested_by, outcome, status }`: `id` is an `UnsubscribeRequestId` (a UUIDv7, later ids sort after earlier ones), `message` is `None` once the message is deleted, `list_key` is normalized by the caller, `status` is the HTTP status when the list answered. `UnsubscribeOutcome` is `Accepted`, `Redirected`, `Refused`, `HttpError` or `Network`, named `accepted`, `redirected`, `refused`, `http_error`, `network` by `as_str` and `parse`. Deleting a message keeps its requests with no message; deleting an archive removes its requests. The store cannot derive a list key from the opaque items JSON, so `insight_view` does not read the last request: the caller computes the key and asks `last_unsubscribe` (or `last_unsubscribes` for a thread). Which waits apply after which outcome is the caller's rule. The conformance area `unsubscribe_requests` checks every outcome and a missing status read back, the newest request per key with ties by id, unknown keys and other archives, more than 200 keys at once, the `NotFound` and `Invalid` cases, and both deletions.

### CategoryStore

Stage-B categories (see [INSIGHTS.md](../../../docs/INSIGHTS.md) §4.2–4.5): one row per message with the trust of its sender, the derived kind and topics, and the effective values after the user's overrides; the effective topics also as one indexed row each. Rows are derived and rebuilt whenever `derive_fp` changes. Overrides and trust lists are user state. Kinds, topics, trust and sources are the types of `holtmail-category` (`MessageKind`, `Topic`, `TopicSet`, `TrustState`, `CategorySource`).

Types:

- `CategoryOverrideId`: a UUIDv7 like the ids of `holtmail-core`; displays, parses and serializes as the hyphenated lowercase form.
- `OverrideScope`: `Address` or `Domain`, named `address` and `domain` by `as_str` and `parse`.
- `CategoryOverride { id, archive_id, scope, key, kind, topics, created_at, created_by }`: `kind` and `topics` are `None` to keep the derived value; `topics` keeps every bit. The key is normalised by the caller (lowercase, A-label domain, no trailing dot; a registrable domain for the domain scope). The store compares it exactly and never normalises it.
- `CategoryPolicy { health }`: whether `health` may be applied.
- `CategoryWrite { message, archive, derive_fp, trust, trust_authserv, one_click_ok, own, sender_address, sender_domain, derived_kind, derived_kind_source, derived_topics, derived_sources }`: the runner's result before overrides. The stored `trust_domain` column is the domain of `Aligned` or the provider of `Relayed`, and reads back into the state.
- `CategoryRow { write, kind, topics, list_topic, kind_override, topics_override }`: the stored row with its effective values and the overrides that set them. `topics` holds known topics only; `list_topic` is the first effective topic in taxonomy order other than `health`.
- `CategoryView { row, kind_override, topics_override }`: the row with the overrides themselves.
- `DeriveRow { message, archive, from_address, signals, kinds, auth, items, alias, accounts }`: what stage B reads about a message: its stored insights columns, its `message_aliases` row and the distinct accounts owning a folder it is located in, removed or not, sorted.
- `CategoryCounts { kinds, topics }`: every kind and topic in taxonomy order with its count, zeros included.
- `CategoryFilter`: `Kind(MessageKind)` or `Topic(Topic)`, for `MessageQuery.category`.

Effective values, per axis (kind, topics): the address override that sets the axis, else the domain override that sets it, else the derived value. Override topics pass through `TopicSet::gated` with the row's trust and the policy's health flag, so a sensitive override topic is held back on a row that is not `aligned`, and `health` is dropped (but kept stored) while the flag is off. Derived topics are stored as the runner gave them.

| Method | Guarantees |
| --- | --- |
| `derive_inputs(archive, derive_fp, after, limit)` | Up to `limit` (at most 10,000) messages of the archive whose insights row has outcome `ok` or `partial` and that have no categories row or one with another `derive_fp`, ascending by id, strictly after `after`. Paging with the last id visits each such message once |
| `derive_input(message)` | The same input for one message whatever its categories row holds; `None` without an insights row with outcome `ok` or `partial` |
| `count_needing_derive(archive, derive_fp)` | How many messages `derive_inputs` would list from the start; 0 for an unknown archive |
| `invalidate_derived(ids)` | Removes the categories rows and effective topics of those messages, so stage B lists them again; unknown ids are ignored |
| `set_message_categories(writes, policy)` | Replaces each message's row with the write and its effective values, in transactions of up to 500 rows. A write for a message that no longer exists is skipped; `Invalid` when a write names another archive than its message's. Returns the ids whose effective kind or topics changed, a new row counting as changed, in write order; an unchanged rewrite returns nothing. Queues nothing: ingest indexes right after it |
| `set_message_categories_queued(writes, policy)` | Exactly `set_message_categories`, and each batch's changed ids are queued for reindexing (as `queue_reindex`) in the same transaction, so a changed category is never committed without its queued reindex |
| `put_category_override(override, policy)` | Upserts on `(archive, scope, key)`, keeping the stored id, `created_at` and `created_by`, and recomputes only the rows whose `sender_address` (address scope) or `sender_domain` (domain scope) equals the key, in one transaction that also queues the changed ids for reindexing. Returns the stored override and the changed ids, ascending. `Invalid` when it sets neither axis or the key is empty, `NotFound` for an unknown archive |
| `delete_category_override(archive, id, policy)` | Removes it and recomputes the rows it matched: derived values come back, then any remaining override per axis, queuing the changed ids for reindexing in the same transaction. Returns the changed ids, ascending. `NotFound` for an unknown id or one of another archive |
| `category_overrides(archive)` | By scope (address first), then key |
| `category_counts(archive, include_health)` | Effective kinds and topics counted in the store, exact once a write returns; `health` only when asked for |
| `trusted_authserv_ids(archive)` | Every account of the archive with its list, empty when none was set |
| `set_trusted_authserv_ids(account, ids)` | Replaces the account's list, kept exactly as given and in order. `NotFound` for an unknown account |
| `recent_auth_facts(account, limit)` | The `auth` JSON of up to `limit` (at most 1,000) messages located in the account, removed or not, that have an insights row, the most recently archived first (archive order, never the sent date). Empty for an unknown account |
| `queue_reindex(ids)` | Queues the messages for reindexing in their archives: a new entry gets sequence 1, an existing one keeps its row with the sequence raised. Unknown ids are ignored, a repeated id counts once; transactions of up to 500 ids, in order |
| `reindex_queue(archive, after, limit)` | Up to `limit` (at most 10,000) queued `(message, seq)` of the archive, ascending by id, strictly after `after`. Paging with the last id visits each entry once while nothing is written; empty for an unknown archive |
| `dequeue_reindex(done)` | Removes each entry whose sequence still equals the given one; an entry queued again since (higher sequence) stays. Unknown ids and stale sequences are ignored; idempotent |
| `count_reindex_queue(archive)` | Queued entries of the archive; 0 for an unknown archive |

The reindex queue is durable: an index step reads `(id, seq)`, reindexes, then dequeues with the same `seq`, so a re-queue in between is never lost.

Deleting a message removes its row, topics and queue entry; deleting an archive removes its rows, topics, queue, overrides and the trust lists of its accounts; deleting an account removes its trust list.

### LedgerStore

The archive ledger (`holtmail-ledger`): a hash chain of every message archived and deleted, written in the same transaction as the change it records. `insert_message` enters each created message as `archived`, `delete_archive` enters one `archive_deleted`, and message deletion enters `deleted` per message. Entries are numbered from 1 without gaps, each links to the hash before it, and none is ever changed or removed, also when its archive is deleted.

| Method | Guarantees |
| --- | --- |
| `ledger_head()` | The newest entry; `None` while empty |
| `ledger_entries(after, limit)` | Up to `limit` entries after position `after` (0 for the first), in order, exactly as stored, so a changed entry reads back changed |
| `enter_unentered(limit)` | Enters up to `limit` live messages without an `archived` entry, oldest archived first, as entries made now; returns how many, 0 when none are left |
| `ledger_gaps(limit)` | `LedgerGaps`: live messages without an entry, live messages whose entry names other bytes or another archive, and messages entered as archived that are gone without a `deleted` entry or a later `archive_deleted` of their archive; up to `limit` ids of each with full counts |

### WriteStore

The foundation of writing ([WRITE.md](../../../docs/WRITE.md)): what each account allows, the role of each grant, the audit log of writes and the records behind `Idempotency-Key`.

| Method | Guarantees |
| --- | --- |
| `account_capabilities(account)` | The account's `Capabilities`; empty (nothing allowed) for a new or unknown account |
| `set_account_capabilities(account, &caps)` | Replaces them; idempotent; `NotFound` for an unknown account. They go away with the account or its archive |
| `grant_roles(user)` | `Grant { archive, role }` per granted archive, ordered like `grants`; empty for an unknown user |
| `set_grant_role(user, archive, role)` | `Some(role)` grants or changes the role, `None` takes the grant away; idempotent; `NotFound` for an unknown user or archive |
| `append_audit(&entry)` | Adds `NewAuditEntry { at, user, kind, archive, account, targets, count }` and returns it with its `seq`; `seq` grows with every entry. Entries are never changed or removed, also not with their user, archive or account |
| `list_audit(&AuditQuery { archive, before, limit })` | Newest first; only the archive's entries with `archive`, only `seq < before` with `before`; at most `limit` (the store may cap it, SQLite at 500) |
| `claim_idempotency_key(&key, fingerprint, at, since, abandoned_before)` | Atomic. With no record of `IdempotencyKey { user, key, route }` newer than `since` it holds the key for this request (`Claimed`), replacing an older record or an unfinished claim made before `abandoned_before` (a request that died without completing or releasing it); otherwise `Mismatch` when the fingerprint differs, `InFlight` while the first request runs, `Done(StoredResponse)` once it finished |
| `complete_idempotency_key(&key, &response)` | Keeps `StoredResponse { status, content_type, body }` for later claims; `NotFound` when the key is not held |
| `release_idempotency_key(&key)` | Forgets the key so the request can run again; idempotent |
| `prune_idempotency_keys(before)` | Removes every record claimed before `before`, finished or not; returns how many. Records also go with their user |

`Capability` is `MailFlags`, `MailMove`, `MailDelete`, `MailExpunge`, `MailAppend`, `MailSend`, `PimEdit`, `PimRespond`, named `mail.flags` … `pim.respond` by `as_str`, `parse` and serde; `Capabilities` is a set of them whose default is empty. `GrantRole` is `Reader < Editor < Sender`, each including the ones before it; grants made before roles existed read as `Reader`. `AuditKind` names every kind of write (`capabilities_changed`, `grant_changed`, `mail_flags`, `mail_move`, `mail_delete`, `mail_append`, `mail_send`, `pim_edit`, `pim_respond`, `rule_changed`, `transport_changed`, `identity_changed`, `outbox_changed`, `device_created`, `device_changed`, `device_revoked`). An audit entry carries ids and counts only, never content, subjects or addresses; `user` is `None` for the server itself. The conformance area `write` checks capabilities with their removal, roles kept by `set_grants`, the audit order, paging and survival of deletions, and every idempotency claim, expiry and pruning.

### SendStore

Sending ([WRITE.md](../../../docs/WRITE.md#sending)): transports, identities with the users who may send as them, drafts with their versions, and the outbox. `holtmail-outbox` drives the outbox; the store only keeps rows and makes their state changes atomic.

| Method | Guarantees |
| --- | --- |
| `create_transport(&t)` | Adds a `Transport` whose id the caller chose (so its secret can be sealed with context `transport:<id>` first). `archive_id: None` is an instance transport. `Invalid` for a taken id, an account transport without an account, with an account of another archive, or at instance scope, and an SMTP transport that names an account; `NotFound` for an unknown archive or account |
| `get_transport(id)` | The transport or `None` |
| `archive_transports(archive)` | The archive's own transports and the instance transports granted to it, oldest first |
| `instance_transports()` | Every instance transport, oldest first |
| `update_transport(&t)` | Replaces everything but `id`, `archive_id`, `kind` and `created_at`, with the checks of `create_transport`; `NotFound` for an unknown id |
| `delete_transport(id)` | Removes it with its grants; `Invalid` while an identity uses it. Outbox rows that name it stay (the outbox fails them) |
| `transport_grants(id)` / `set_transport_grants(id, &archives)` | The archives an instance transport is granted to, ordered by id; replacing them is `Invalid` for an archive transport or when an archive losing the grant has an identity on the transport |
| `create_identity(new)` | Adds an `Identity` with a fresh id; its `created_by` user (when it exists) may send as it. `Invalid` when the archive may not use the transport or the account is in another archive; `NotFound` for an unknown archive, transport or account |
| `get_identity(id)`, `list_identities(archive)` | One identity, or the archive's oldest first |
| `update_identity(&i)` | Replaces every field but `id`, `archive_id`, `created_by` and `created_at`, with the checks of `create_identity` |
| `delete_identity(id)` | Removes it with its users and defaults; outbox rows that name it stay |
| `identity_users(id)` / `set_identity_users(id, &users)` | Who may send as it, ordered by id; `NotFound` for an unknown identity or user. Users go away with their user |
| `user_identities(user, archive)` | The archive's identities the user may send as, oldest first |
| `default_identity(user, archive)` / `set_default_identity(user, archive, id)` | The user's default identity per archive; `Invalid` for an identity of another archive. It goes away with the identity |
| `create_draft(archive, user, &payload, &blobs, at)` | A `Draft` at version 1. The payload is the API's JSON; `blobs` (attachments) are referenced, so they are not deleted while a kept version names them |
| `save_draft(id, expected, &payload, &blobs, at)` | Compare-and-set: a new version only when the draft is still at `expected`, otherwise `DraftSave::Conflict(current)`. Keeps the newest `DRAFT_VERSIONS_KEPT` (50) versions |
| `get_draft`, `draft_versions` (newest first), `list_drafts(archive, user)` (most recently saved first), `delete_draft` (idempotent) | Drafts go away with their user and their archive |
| `enqueue_outbox(&new)` | Adds a row in `queued` or `draft` with every recipient `pending`, or returns the row with the same user and `idempotency_key` and `false`. `Invalid` for another state or no recipient; `NotFound` for an unknown archive. The row's blob counts as referenced |
| `get_outbox(id)`, `list_outbox(&OutboxQuery { archive, states, before, user, limit })` | One row, or the archive's newest first, narrowed by states and by the user who queued it (before the limit) and paged by id; at most `limit` (SQLite caps at 500) |
| `change_outbox(id, from, &change)` | Atomic compare-and-set: applies `OutboxChange` only when the row is in one of `from`, else `None`; of two changes from the same state one wins. Recipients may change state but not address or order (`Invalid`) |
| `due_transports(now)` | Transports with a queued row whose `send_at` passed or a retrying row whose `next_attempt_at` passed |
| `claim_outbox(transport, now)` | Atomically moves the transport's earliest due row to `sending`, counting an attempt at `now`; `None` when nothing is due or a row of the transport is already `sending`, so one message per transport is ever in flight |
| `outbox_attempts_since(transport, since)` | Rows of the transport whose last attempt started at or after `since`, for the hourly limit |
| `interrupt_sending(at, error)` | Moves every `sending` row to `uncertain` with `error` and returns them, for a start after a stop mid-send |
| `outbox_by_rfc_message_id(archive, id)` | The archive's outbox rows with that `Message-ID` (no angle brackets, exact), oldest first |
| `outbox_for_message(message)` | The rows whose sent bytes are `message` or whose `synced_copy` is `message`, oldest first |
| `link_outbox_copy(id, message, at)` | Sets `synced_copy` (replacing an earlier one), state untouched; `Invalid` for a message of another archive, `NotFound` for an unknown row or message |
| `outbox_by_idempotency_key(user, key)` | The row with this user and key, whatever became of it, or `None` |
| `device_sends_since(user, since)` | `DeviceSends { messages, recipients, oldest }` of what the user's devices submitted at or after `since`, across every archive. Cancelled messages do not count, and messages from the web app never do |
| `set_transport_smtputf8(id, smtputf8)` | Records what the last check found; `update_transport` leaves it alone. `NotFound` for an unknown id |
| `set_outbox_note(id, note, at)` | Replaces `note` (`None` clears it), state untouched; `NotFound` for an unknown row |
| `set_identity_rewritten_from(id, rewritten)` | Records or clears `RewrittenFrom { seen, at }` on the identity; idempotent; `update_identity` clears it when the address changes |

`Transport.smtputf8` says whether the last check found that the transport takes internationalized addresses (false until one did). `TransportKind` is `smtp`, `jmap`, `graph` or `account` (only `account` names an account); `TransportLimits { max_size, max_recipients, per_hour }` are all optional. `SentCopyPolicy` is `Provider`, `Append(folder)` or `None`, stored as `provider`, `append:<folder>` and `none`; `append` is acted on from Phase 60 (it needs `MailboxWriter`). `OutboxState` is `draft`, `queued`, `sending`, `retrying`, `sent`, `failed`, `uncertain`, `bounced`, `cancelled`; `RecipientState` is `pending`, `accepted`, `refused { permanent, code, text }` or `bounced { status, text }` (taken, then reported undeliverable by a DSN), where `text` is the server's and may name addresses, so it is shown but never logged. `OutboxEntry.synced_copy` is the provider's own archived copy with other bytes and `note` something learned after sending (a delay); both may name addresses. The conformance area `send` checks transport scopes, grants and their checks, identities with users and defaults, removal with accounts, users and archives, draft compare-and-set and version trimming, outbox idempotency, paging, compare-and-set changes, claims with one in flight per transport, retries coming due, interruption, copy links, notes, the rewritten-`From` flag, JMAP and Graph transports, and that outbox and draft blobs count as referenced.

### MailChangeStore

Changes to messages on their servers ([WRITE.md](../../../docs/WRITE.md#mailbox-changes)): flags, labels, moves, deletes and appends (sent copies and restores). A change is recorded and shown at once, sent by `mail-writeback` through the account's `MailboxWriter`, and confirmed by what sync reads. The payload of a row never changes; only its state and bookkeeping do, and every state change is atomic.

States: `held` → `sending` → `applied` | `superseded_by_server` | `failed`; a temporary failure goes from `sending` back to `held` with `next_attempt_at`. A flags or labels change the server accepted stays `sending` with `sent_at` until a sync confirms it (`MailOp::needs_confirmation`); everything else is `applied` as soon as the server accepts it.

| Method | Guarantees |
| --- | --- |
| `enqueue_mail_change(&NewMailChange)` | Records it `held`, with the `device_id` it came from when a device asked. `Invalid` when the folder or a move's target is not the account's, a move targets its own folder, `uid` is missing (or given for an append), flags or labels add and remove nothing, or the message is in another archive; `NotFound` for an unknown account, folder or message |
| `get_mail_change(id)` | The change, or `None` |
| `due_mail_accounts(now)` | Accounts for which `claim_mail_change(now)` would find something, ordered by id |
| `claim_mail_change(account, now)` | Atomically moves the oldest due held change to `sending` and counts an attempt. A change waits while an older change of the same message in the same account is held (even waiting for a retry) or sending, so one message's changes go out in order, one at a time; other messages pass it |
| `mark_mail_change_sent(id, result_uid, at)` | From `sending` and not yet sent: flags and labels keep `sending` with `sent_at`, others become `applied`; keeps the UID a move or append got. Else `None` |
| `applied_move_from(message, folder, uid, before)` | The newest `applied` move of the message from that place recorded before the change `before`, or `None`: where a later change made at the old place should go |
| `retarget_mail_change(id, folder, uid, at)` | From `sending` and not yet sent: points the change at another place of its account (where an earlier move took the message). `Invalid` for another account's folder, an append, or a move to its own folder; else `None` |
| `retry_mail_change(id, next_attempt_at, error, at)` | From `sending` and not yet sent back to `held` with the error code and the next attempt. Else `None` |
| `fail_mail_change(id, error, at)` | From `held` or `sending` to `failed` (also how a held change is discarded). Else `None` |
| `confirm_mail_flags(folder, uid, flags, labels, read_at)` | What sync read at `read_at`: an open flags change (and labels change, when `labels` is given) of that location that the state satisfies is `applied`, held ones too; a sent one sent before `read_at` that the state contradicts is `superseded_by_server` (the server wins); one being sent right now and held ones that disagree stay, and so does every change newer than the first one that stays (judged oldest first), so a later change that undoes an older one is never closed ahead of it. Flags compare ignoring ASCII case, labels exactly. Returns the closed changes, oldest first |
| `settle_mail_changes(account, sent_before, at)` | Applies sent changes no sync confirmed before the cutoff, trusting the server's answer; returns how many |
| `release_mail_changes(at)` | At start: every `sending` change not yet sent goes back to `held`, oldest first. Flags, labels, moves and deletes are safe to send again; an append may be saved twice |
| `pending_mail_changes(message)` | The message's open changes in every account, oldest first, for the held mark |
| `failed_mail_changes(message)` | The message's failed changes in every account that nobody dismissed, oldest first, for the reader's retry and dismiss |
| `dismiss_mail_change(id, at)` | Hides a failed change from `failed_mail_changes`; the row and its state stay. `None` when it is not failed or already dismissed |
| `outbox_mail_changes(outbox)` | The changes recorded for an outbox message (its sent copy), oldest first |
| `retention_candidates(account, before, skip_trash, limit)` | Live located UIDs in the account's folders whose message was received (or, without a received time, dated) before `before`, with no held or sending change at that location and no retention change ever recorded there (so a failed one is not recorded again every pass), oldest message first, at most `limit`; `skip_trash` leaves out folders with the `trash` role. What the retention job may delete from the server |
| `list_mail_changes(&MailChangeQuery { account, states, limit })` | The account's changes, newest first, narrowed by states (empty for all); SQLite caps `limit` at 500 |

`MailOp` is `flags { add, remove }`, `labels { add, remove }` (on the UID in the `All` folder), `move { target }` (a folder id of the same account), `delete { expunge }` or `append { blob_id, flags, date }`, serialized with a `kind` tag; flags are IMAP spellings. `MailChangeOrigin` is `user` (restores included), `rule`, `retention` or `sent_copy` (with `outbox_id`). `last_error` is a short code such as `connection` or `unsupported`, never server text. An append's blob counts as referenced while the row exists. Rows go with their account and archive. The conformance area `mail_changes` checks the checks of `enqueue`, FIFO claims with one in flight per message, retries coming due, release, sent and applied outcomes, settling, failing, confirmation with the server winning, listing, the pending lookup, dismissing failed changes, removal with the account and the archive, and that append blobs count as referenced and are released with the archive, the outbox lookup, and which locations are retention candidates (not one whose retention change failed), and finding and following an applied move (`applied_move_from`, `retarget_mail_change`).

### ItemChangeStore

Edits to contacts and events from devices and the web app, held in Holtmail and sent upstream by `pim-writeback` when the collection is `write_through` ([CLIENTS.md, Edits from native apps](../../../docs/CLIENTS.md#edits-from-native-apps), [WRITE.md](../../../docs/WRITE.md#contacts-and-calendars)). Upstream versions stay what sources showed; each edit is an `ItemChange` row whose payload (target, `ChangeOp::{Create, Update, Delete}`, `ChangeClass::{Noop, Personal, Reply, Edit, Create, Delete}`, the client's bytes as a blob, the patch as opaque JSON, the base version with its upstream ETag, `supersedes`, `ChangeOrigin::{Dav, Api}`, user and device) never changes. A change is about an item (`ChangeTarget::Item`) or an href in a collection (`ChangeTarget::Href`), which names the item served under it (its `dav_name`, else its `remote_id`) and otherwise the changes made under it before any item existed (a held create). The store never parses bytes; classification, patches and rebasing belong to `pim-edit`.

States: `held`, `sending`, `applied`, `superseded`, `conflict`, `rejected`, `discarded`. Open (served) means held, sending, conflict or rejected; at most one open change per item is served, the newest (the head). Every recorded change and every state change writes a `ChangeLogEntry` in the same transaction; the bytes of every change, discarded ones too, count as referenced blobs. Flags: a newer upstream version (`record_version`, `record_mail_version`) sets `upstream_changed` on the item's open changes, and an item marked removed (`mark_items_removed`, `mark_collection_removed`) sets `upstream_deleted`; an item that comes back clears `upstream_deleted`. A version recorded by a writeback flags only the item's other open changes, and not the changes that superseded the written one (the user's own later edits): those are rebased onto the new version (`rebased_onto`) instead. A change that settles or is resolved as a conflict, or settles as rejected, has both flags cleared.

| Method | Guarantees |
| --- | --- |
| `set_write_mode(collection, mode)` | Sets and returns the collection. `Invalid` for a mode other than `read_only` on a collection no account owns or one of an import account; `NotFound` for an unknown collection |
| `record_change(&NewItemChange, expect)` | Atomic compare-and-set. `NewItemChange.not_before` keeps the change from going upstream before then, and `messages` links held outbox messages to it in the same transaction. `Expect::Any` (last writer wins), `Expect::Absent` (nothing served under the href and no item ever had it) or `Expect::State(ServedState { version, change })`, the state the client's ETag was computed from; a mismatch records nothing and answers `RecordedChange::PreconditionFailed { current }`. The target's open head is superseded (one being sent stays `sending` and becomes `superseded` when it settles, unless applied), and the new row links it in `supersedes`. The new change is `held`, except a delete of a create that never reached upstream (no item, nothing sending), which is `applied` at once. The base ETag is copied from the base version. The store does not check the write mode, roles or scopes. `Invalid` when the class, bytes or base do not fit the operation, a create names an existing item or a served href, an update or delete names an href with nothing served, the base version does not exist, or the collection has no account; `NotFound` for an unknown item or collection |
| `get_change(id)` | The change, or `None` |
| `effective(target)` | `Effective { collection_id, href, item, latest, change, state, served }`: the latest upstream version with the open head on top; `served` is false for a deleted item, a held delete, or a create deleted before it left. An item removed upstream stays served while a change other than a delete is open. `None` when the target names no item and no change |
| `open_changes(&ItemChangeQuery { archive_id, collection_id, states, flagged_only, limit })` | Open changes, oldest first; `states` narrows (empty for all open states; non-open states match nothing); `flagged_only` keeps those with an upstream flag; capped at 500 |
| `changes_of(target)` | Every change of the target in any state, oldest first |
| `resolve_change(id, resolution, at)` | `Rebased { onto }` (from `held`; clears `upstream_changed` when `onto` is the latest; `Invalid` for a version that does not exist or a change without an item), `Applied` and `Discard` (from `held`, `conflict` or `rejected`), `Conflict { reason }` (from `held` or `rejected`; clears `upstream_changed`), `Retry` (from `rejected`, back to `held` with no attempts). `None` from any other state |
| `due_change_collections(now)`, `claim_change(collection, now)` | The `write_through` collections with a due change, and atomically moving the oldest due change of one to `sending` with an attempt counted. Due: `held`, a class that is sent (not `noop` or `personal`), no upstream flag, past `next_attempt_at`, the head of its chain, and no change of the same item (or href) sending. First in, first out per collection; other items pass a waiting one. A change with no item is sent as a create |
| `settle_change(id, &Settle, at)` | Only from `sending`. `Applied { version }` records the version with origin `writeback` (same bytes as the latest add none), creating the item with the change's href as its `dav_name` when it had none and filling in the item on every change under that href; without a version, a delete marks the item removed. `Rejected { reason }`, `Conflict { reason }` and `Retry { next_attempt_at, error }` (back to `held`) become `superseded` instead when a later change superseded it. `Invalid` for a version of another collection or item |
| `held_change_messages()` | Outbox messages in `draft` that `NewItemChange.messages` linked to a recorded change, oldest first |
| `defer_reply(archive, message, at, next)`, `due_replies(now, limit)`, `forget_reply(message)` | The queue of incoming iMIP replies waiting for another try; deferring again counts an attempt, moves the time and keeps `since` |
| `release_changes(at)` | At start: every `sending` change goes back to `held` (or `superseded` when a later change exists); returns the held ones, oldest first |
| `changes_since(archive, after, limit)` | The archive's log after `seq` `after`, oldest first, capped at 1 000. Only item changes write to it so far; the live stream (CLIENTS Phase 45) extends it to everything |

`reason` and `last_error` are short codes (`overlap`, `reply_obsolete`, `upstream_deleted`, `refused`, `connection`), never server text, names or addresses. Rows go only with their archive. The conformance area `item_changes` checks write modes and their defaults, recording with every precondition and refusal, superseding, creates under an href (taken hrefs, deleting a create before it left, a new create on a freed href), each resolution and the states it starts from, upstream flags from new versions, removals and restores, the writeback queue (FIFO, one in flight per item, personal changes never sent, held collections never sent, retries coming due, release, rejection and retry, conflicts, writeback versions creating the item with its DAV name, a sent delete marking the item removed), the change log, and change bytes counting as referenced and released with the archive.

### DeviceStore

Devices and their app password hashes ([SERVE.md](../../../docs/SERVE.md#device-credentials)). `DeviceSettings` is what can be chosen, at creation and later: `name`, `mail_archive` (required with IMAP or SMTP), `archives` (`None` is every archive the user can read; an empty set is none), `protocols`, `scopes`, `implicit_seen`. `NewDevice` adds the user, `secret_hash` (the BLAKE3 hash of the app password; `None` for a device that signs in another way, such as the enrolment of CLIENTS Phase 44) and the time. `Device` carries `has_secret`, never the hash.

| Method | Guarantees |
| --- | --- |
| `create_device(new)` | `NotFound` for an unknown user or archive; `Invalid` when the protocols need a mail archive and it has none, or another device has the hash |
| `get_device(id)` | The device, revoked or not |
| `find_device_by_secret(&hash)` | The device with this hash, revoked or not, so a revoked secret can be told from a wrong one. An exact lookup, so the same time for any wrong secret |
| `list_devices(user)` | Revoked ones too, oldest first |
| `update_device_settings(id, &settings, raised_at)` | Replaces the settings and records `raised_at` when given (`None` keeps the earlier value); `Invalid` for a revoked device or a missing mail archive |
| `touch_device(id, protocol, at)` | Keeps only the UTC day and the protocol (`start_of_day`), so use is not a tracking log. An earlier day and a revoked device change nothing. `NotFound` for an unknown device |
| `revoke_device(id, at)` | Permanent; true when this call revoked it, false when it already was, which keeps the first time |
| `revoke_user_devices(user, at)` | Revokes those not revoked yet and returns them, oldest first |

Deleting an archive clears `mail_archive` where it was that archive and takes it off every `archives` list; the device stays, and IMAP and SMTP stop working for it until a new mail archive is set. Deleting a user removes their devices.

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
