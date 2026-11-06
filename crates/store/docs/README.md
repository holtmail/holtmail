# holtmail-store

The contract for Holtmail's metadata: archives, accounts, folders, messages with their attachments, locations, labels and threads, sync progress, users with their sign-in sessions, address books and calendars with every version of their contacts and events, and the people derived from contacts and mail. The store never holds message, contact or event bytes; it references blobs by `BlobId`. Everything except archives, accounts, sync state, users, the splits and joins of people made by hand and the history of items (which collection, which versions, when seen and when removed) can be rebuilt from the blobs; the normalized fields of items (`pim_index`) can.

## Traits

`Store` is `ArchiveStore + MessageStore + LabelStore + SyncStore + UserStore + RemoteImageStore + SettingsStore + DerivedStore + InventoryStore + TaskStore + PimStore + PeopleStore + CalendarStore + LedgerStore + DeletionStore + WebhookStore + AliasStore + InsightStore`, implemented automatically for any type that implements them all. Use it as `Arc<dyn Store>`.

### ArchiveStore

| Method | Guarantees |
| --- | --- |
| `create_archive`, `get_archive`, `list_archives` | Lists are oldest first |
| `rename_archive(id, name)` | `NotFound` for a missing archive |
| `set_save_remote_images(id, save)` | Sets `Archive.save_remote_images` (off for new archives). Turning it on records `save_remote_images_since` unless it was already on; turning it off clears it. `NotFound` for a missing archive |
| `delete_archive(id)` | Removes the archive with its accounts, folders, messages, attachments, labels, insights, sync state and grants, the remote copies no remaining message links, and the derived records of blobs no longer used. Returns the blobs no message, attachment, remote copy or derived record references any more, so the caller can delete them. `NotFound` for a missing archive |
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
| `message_extras(ids, folder)` | One batched answer for a list page: for each existing message, its flags (sorted, deduplicated; in `folder` when given, else the union over locations not removed), the number of messages in its thread, the folders it is in and not removed from (by id), its labels (sorted) and `insight_kinds`, the kind bits of its insights (0 without insights). Missing ids are left out |
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

Removing an account keeps its collections and items (their `account_id` becomes `None`), like its messages. Removing an archive removes its collections, items, versions and their links to messages and returns the version and picture blobs nothing else uses. A collection with no account from the start, such as each archive's "From mail" calendar, is one no source owns.

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
| `referenced_blobs(after, limit)` | Up to `limit` blobs some record names: a message, an attachment, an item version or its picture, a saved remote image or a derived file. Each blob once, ascending by id, strictly after `after` (from the start when `None`); paging with the last id of each page visits every referenced blob exactly once, and a page shorter than `limit` is the last |

### DeletionStore

Deleting messages on purpose. The archive otherwise never forgets; a deleted message leaves tombstones of its bytes' hash in its archive and of every folder and UID it had, so syncs and imports do not bring it back.

| Method | Guarantees |
| --- | --- |
| `delete_messages(archive, ids, actor)` | In one transaction removes the archive's messages among `ids` with their locations, attachments, labels (labels left without messages go), addresses, thread links, remote image links, insights and calendar version links; calendar versions read only from them go too, and an item left without versions is removed with its calendar instances. Records the tombstones and enters each message in the ledger as `deleted` by `actor`. Returns `DeletedMessages { messages, orphans, changed_items, removed_items }`: blobs nothing references any more (the caller deletes them), items to rebuild and items gone. Idempotent; ids not in the archive are skipped |
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

Deleting a message removes its record; deleting an archive removes its records, services and aliases.

### InsightStore

Insights derived from messages (see [INSIGHTS.md](../../../docs/INSIGHTS.md)): one `InsightRow` per message, replaced whole and rebuilt when the extraction fingerprint changes. The store treats `items` and `auth` as opaque JSON text and returns them byte for byte; `signals` and `kinds` keep all 64 bits.

`InsightRow { message, archive, fingerprint, outcome, truncated, from_address, signals, kinds, auth, items, computed_at }`; `InsightOutcome` is `Ok`, `Partial`, `Unparsable` or `Unreadable`, named `ok`, `partial`, `unparsable`, `unreadable` by `as_str` and `parse`. `InsightView { insights }` is everything known about one message's insights and gains the categories later; `InsightSummary { kinds }` is what a list row shows.

| Method | Guarantees |
| --- | --- |
| `set_message_insights(message, row)` | Replaces the message's row whole; idempotent. `NotFound` for an unknown message, `Invalid` when `row.message` is another message or `row.archive` is not its archive |
| `insight_view(message)` | The view, or `None` when nothing was stored (the API's `pending`) |
| `insight_views_among(ids)`, `insights_among(ids)` | Views or summaries of the ids that have a row, any number of ids; others are left out |
| `messages_needing_insights(archive, fingerprint, after, limit)` | Up to `limit` (at most 10,000) messages of the archive without a row or with a row of another fingerprint, ascending by id, strictly after `after`. A row with the current fingerprint is never listed, whatever its outcome, so `unreadable` and `unparsable` messages wait for the next fingerprint. Paging with the last id visits each such message once |
| `count_needing_insights(archive, fingerprint)` | How many messages the walk would list from the start; 0 for an unknown archive |

Row states: no row is pending; a row with an empty item list is ready; a row with another fingerprint is outdated and still served until rewritten; `partial`, `unparsable` and `unreadable` are what the outcome says. Deleting a message removes its row; deleting an archive removes its rows.

### LedgerStore

The archive ledger (`holtmail-ledger`): a hash chain of every message archived and deleted, written in the same transaction as the change it records. `insert_message` enters each created message as `archived`, `delete_archive` enters one `archive_deleted`, and message deletion enters `deleted` per message. Entries are numbered from 1 without gaps, each links to the hash before it, and none is ever changed or removed, also when its archive is deleted.

| Method | Guarantees |
| --- | --- |
| `ledger_head()` | The newest entry; `None` while empty |
| `ledger_entries(after, limit)` | Up to `limit` entries after position `after` (0 for the first), in order, exactly as stored, so a changed entry reads back changed |
| `enter_unentered(limit)` | Enters up to `limit` live messages without an `archived` entry, oldest archived first, as entries made now; returns how many, 0 when none are left |
| `ledger_gaps(limit)` | `LedgerGaps`: live messages without an entry, live messages whose entry names other bytes or another archive, and messages entered as archived that are gone without a `deleted` entry or a later `archive_deleted` of their archive; up to `limit` ids of each with full counts |

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
