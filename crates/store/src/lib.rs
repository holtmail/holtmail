//! Contract for Holtmail's metadata: archives, accounts, folders, messages, threads, sync state,
//! users and their sessions, the recent background tasks, contacts and calendars with every
//! version of their items, and the people derived from contacts and mail.
//!
//! The store never holds message bytes; those live in a `BlobStore` and are referenced by
//! [`holtmail_blob::BlobId`]. Everything in the store except accounts, archives, sync state and
//! task history can be rebuilt from the blobs.

mod aliases;
mod calendar;
mod categories;
mod changes;
mod deletion;
mod devices;
mod insights;
mod item_changes;
mod ledger;
mod mail_changes;
mod people;
mod pim;
mod rules;
mod schedule;
mod send;
mod serve;
mod types;
mod unsubscribe;
mod view;
mod webhooks;
mod write;

#[cfg(feature = "testing")]
pub mod testing;

use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, ArchiveId, CollectionId, EventGroupId, FolderId, ItemId, MessageId, PersonId,
    PersonOverrideId, ThreadId, Timestamp, UserId,
};

pub use aliases::*;
pub use calendar::*;
pub use categories::*;
pub use changes::*;
pub use deletion::*;
pub use devices::*;
pub use insights::*;
pub use item_changes::*;
pub use ledger::*;
pub use mail_changes::*;
pub use people::*;
pub use pim::*;
pub use rules::*;
pub use schedule::*;
pub use send::*;
pub use serve::*;
pub use types::*;
pub use unsubscribe::*;
pub use view::*;
pub use webhooks::*;
pub use write::*;

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A referenced record does not exist.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// The request breaks a rule of the data, e.g. a cursor from another query.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The backend failed; the operation may succeed if retried.
    #[error("store backend failed: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Archives, accounts and folders.
#[async_trait]
pub trait ArchiveStore: Send + Sync {
    /// Creates an archive named `name`.
    async fn create_archive(&self, name: &str) -> Result<Archive, StoreError>;

    /// The archive, or `None`.
    async fn get_archive(&self, id: ArchiveId) -> Result<Option<Archive>, StoreError>;

    /// Every archive, oldest first.
    async fn list_archives(&self) -> Result<Vec<Archive>, StoreError>;

    /// Renames the archive. `NotFound` when it does not exist.
    async fn rename_archive(&self, id: ArchiveId, name: &str) -> Result<(), StoreError>;

    /// Turns saving remote images for an archive on or off. Turning it on records the time in
    /// `save_remote_images_since`, unless it was already on; turning it off clears it. Fails with
    /// `NotFound` for an unknown archive.
    async fn set_save_remote_images(&self, id: ArchiveId, save: bool) -> Result<(), StoreError>;

    /// Removes the archive with its accounts, folders, messages, attachments, labels, sync state,
    /// grants, people and their splits and joins, the insights and categories of its messages, its
    /// category overrides, the trusted authserv-ids of its accounts, its transports, identities, drafts and outbox, the grants of instance transports to it, collections with their items and versions, the remote copies no remaining message
    /// links to, and what was derived from blobs that are no longer used. Returns the blobs that no
    /// message, attachment, item version, remote copy, outbox message, draft or derived record references any more, so
    /// the caller can delete them. Enters the deletion in the ledger as one `archive_deleted`
    /// entry, in the same transaction; earlier entries stay.
    /// `NotFound` when the archive does not exist.
    async fn delete_archive(&self, id: ArchiveId) -> Result<Vec<BlobId>, StoreError>;

    /// Totals of the archive; zeros for an archive without messages.
    async fn archive_stats(&self, id: ArchiveId) -> Result<ArchiveStats, StoreError>;

    /// Creates an account. Fails with `NotFound` when the archive does not exist.
    async fn create_account(&self, account: NewAccount) -> Result<Account, StoreError>;

    /// The account, or `None`.
    async fn get_account(&self, id: AccountId) -> Result<Option<Account>, StoreError>;

    /// Accounts of the archive, oldest first.
    async fn list_accounts(&self, archive: ArchiveId) -> Result<Vec<Account>, StoreError>;

    /// Replaces every field of the account except `id`, `archive_id`, `kind` and `created_at`.
    async fn update_account(&self, account: &Account) -> Result<(), StoreError>;

    /// Removes the account with its folders, locations, sync state, capabilities and trusted authserv-ids. Its messages stay in the
    /// archive, and so do its collections, transports and identities, which no longer name an account. `NotFound` when the
    /// account does not exist.
    async fn delete_account(&self, id: AccountId) -> Result<(), StoreError>;

    /// The folder with `path` in the account, created when missing. Idempotent; a different `role`
    /// updates the stored one.
    async fn upsert_folder(
        &self,
        account: AccountId,
        path: &str,
        role: Option<FolderRole>,
    ) -> Result<Folder, StoreError>;

    /// The folder, or `None`.
    async fn get_folder(&self, id: FolderId) -> Result<Option<Folder>, StoreError>;

    /// Folders of every account of the archive with message and unread counts, ordered by account,
    /// then path.
    async fn list_folders(&self, archive: ArchiveId) -> Result<Vec<FolderSummary>, StoreError>;
}

/// Messages, their attachments, locations and threads.
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Adds a message to its archive, or finds the one with the same `blob_id`.
    ///
    /// Idempotent per `(archive_id, blob_id)`: inserting the same bytes again returns the existing
    /// message with `created: false` and changes nothing. A new message joins the thread of any
    /// message in the archive whose `Message-ID` is among its `parent_ids`, or whose `parent_ids`
    /// contain its `Message-ID`. When that touches several threads they merge into the oldest, so
    /// the result does not depend on the order messages arrive in. A created message is entered
    /// in the ledger as `archived` in the same transaction.
    async fn insert_message(&self, message: NewMessage) -> Result<Inserted, StoreError>;

    /// The message, or `None`.
    async fn get_message(&self, id: MessageId) -> Result<Option<Message>, StoreError>;

    /// The messages that exist among `ids`, in the order of `ids`.
    async fn get_messages(&self, ids: &[MessageId]) -> Result<Vec<Message>, StoreError>;

    /// The message of the archive with these bytes, or `None`.
    async fn find_by_blob(
        &self,
        archive: ArchiveId,
        blob: &BlobId,
    ) -> Result<Option<MessageId>, StoreError>;

    /// Attachments of the message by `ordinal`.
    async fn attachments(&self, message: MessageId) -> Result<Vec<Attachment>, StoreError>;

    /// Messages newest first by `date`, ties broken by id, narrowed by the query's folder, label
    /// and category when given.
    async fn list_messages(&self, query: &MessageQuery) -> Result<Page<Message>, StoreError>;

    /// Flags, thread size, folders and labels of each message among `ids` that exists, for showing a list
    /// in one call. With `folder`, `flags` are the message's flags in that folder (empty when it
    /// is not located there); otherwise their union over every location not removed.
    async fn message_extras(
        &self,
        ids: &[MessageId],
        folder: Option<FolderId>,
    ) -> Result<HashMap<MessageId, MessageExtras>, StoreError>;

    /// Messages of the thread, oldest first.
    async fn thread_messages(&self, thread: ThreadId) -> Result<Vec<Message>, StoreError>;

    /// The archive's messages whose `Message-ID` is one of `ids` (without angle brackets,
    /// compared exactly), oldest first; empty for no ids.
    async fn messages_by_rfc_id(
        &self,
        archive: ArchiveId,
        ids: &[String],
    ) -> Result<Vec<Message>, StoreError>;

    /// Records that the message is in the folder with this UID and flags at `seen_at`.
    ///
    /// One location per `(folder, message)`: later calls update `uid`, `flags` and `last_seen_at`
    /// and clear `removed_at`. A UID belongs to at most one message in a folder; a call that gives
    /// a UID held by another message takes it from that message.
    async fn record_location(
        &self,
        message: MessageId,
        folder: FolderId,
        uid: Option<u64>,
        flags: &[String],
        seen_at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Where the message was seen, oldest first.
    async fn locations(&self, message: MessageId) -> Result<Vec<Location>, StoreError>;

    /// UIDs of the folder's locations that are not removed, and UIDs holding deleted messages
    /// (see [`DeletionStore`]), ascending.
    async fn folder_uids(&self, folder: FolderId) -> Result<Vec<u64>, StoreError>;

    /// The message at `uid` in the folder, removed or not, or `None`.
    async fn message_at_uid(
        &self,
        folder: FolderId,
        uid: u64,
    ) -> Result<Option<MessageId>, StoreError>;

    /// Marks the folder's locations with these UIDs as removed at `at`. Messages stay archived.
    /// Tombstones of deleted messages at these UIDs are dropped, since the server no longer has
    /// them there.
    async fn mark_removed(
        &self,
        folder: FolderId,
        uids: &[u64],
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Forgets every UID in the folder, for when the server changed its `UIDVALIDITY`. Locations
    /// stay; the next sync re-attaches UIDs. Tombstones of UIDs go; those of the bytes stay.
    async fn forget_uids(&self, folder: FolderId) -> Result<(), StoreError>;

    /// Replaces the flags of the folder's location with this UID, e.g. after the message was read
    /// or flagged in another client. Flags compare as a set: order and duplicates do not count as a
    /// change. Returns whether the flags changed; `false` when no location has the UID. A removed
    /// location is updated too.
    async fn set_flags(
        &self,
        folder: FolderId,
        uid: u64,
        flags: &[String],
    ) -> Result<bool, StoreError>;
}

/// Labels of messages, as Gmail has them. A label belongs to an archive and is known by its name.
#[async_trait]
pub trait LabelStore: Send + Sync {
    /// Replaces the labels of the message with `labels`; empty names and duplicates are dropped,
    /// and labels no message carries any more are forgotten. Returns whether the set changed.
    /// `NotFound` when the message does not exist.
    async fn set_labels(&self, message: MessageId, labels: &[String]) -> Result<bool, StoreError>;

    /// The labels of the message, sorted; empty for an unknown message.
    async fn labels(&self, message: MessageId) -> Result<Vec<String>, StoreError>;

    /// Every label carried by at least one message of the archive, with counts, sorted by name.
    async fn list_labels(&self, archive: ArchiveId) -> Result<Vec<LabelSummary>, StoreError>;
}

/// Sync progress per folder.
#[async_trait]
pub trait SyncStore: Send + Sync {
    /// The folder's cursor; the default cursor when it never synced.
    async fn cursor(&self, folder: FolderId) -> Result<FolderCursor, StoreError>;

    /// Replaces the folder's cursor.
    async fn set_cursor(&self, folder: FolderId, cursor: &FolderCursor) -> Result<(), StoreError>;

    /// Records a failure for the UID, adding one to its attempts.
    async fn record_failure(
        &self,
        folder: FolderId,
        uid: u64,
        reason: &str,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Forgets the UID's failure after it was archived.
    async fn clear_failure(&self, folder: FolderId, uid: u64) -> Result<(), StoreError>;

    /// The folder's failures by UID.
    async fn failures(&self, folder: FolderId) -> Result<Vec<SyncFailure>, StoreError>;

    /// Records how the account's last sync or import ended, replacing the earlier record.
    /// Removed with the account. `NotFound` for an unknown account.
    async fn set_last_run(&self, account: AccountId, run: &AccountRun) -> Result<(), StoreError>;

    /// The last recorded run of every account that has one.
    async fn last_runs(&self) -> Result<HashMap<AccountId, AccountRun>, StoreError>;
}

/// Background tasks and their logs, kept across restarts.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Stores the task, replacing an earlier record with the same id; its log stays.
    /// Idempotent.
    async fn save_task(&self, task: &TaskRecord) -> Result<(), StoreError>;

    /// Adds `lines` to the end of the task's log, in order. The log then keeps its first
    /// [`TASK_LOG_HEAD`] lines and its most recent ones, [`TASK_LOG_LINES`] in all, as if the lines
    /// had been added one by one. `NotFound` for an unknown task.
    async fn append_task_log(&self, id: &str, lines: &[TaskLine]) -> Result<(), StoreError>;

    /// Up to `limit` tasks with their logs, the most recently started first.
    async fn recent_tasks(&self, limit: u32) -> Result<Vec<StoredTask>, StoreError>;

    /// Marks every [`TaskState::Running`] task [`TaskState::Interrupted`], finished at `at`, and
    /// returns them as now stored. Meant for the start of a process, when no task of this
    /// process runs yet.
    async fn interrupt_tasks(&self, at: Timestamp) -> Result<Vec<TaskRecord>, StoreError>;

    /// Removes the tasks that are not running, with their logs, except the `keep` most recently
    /// started of them. Returns how many were removed.
    async fn prune_tasks(&self, keep: u32) -> Result<u64, StoreError>;
}

/// Users and their sign-in sessions.
#[async_trait]
pub trait UserStore: Send + Sync {
    /// How many users exist.
    async fn count_users(&self) -> Result<u64, StoreError>;

    /// Creates a user. Fails with `Invalid` when another user has the same email, compared without
    /// regard to letter case.
    async fn create_user(&self, user: NewUser) -> Result<User, StoreError>;

    /// The user, or `None`.
    async fn get_user(&self, id: UserId) -> Result<Option<User>, StoreError>;

    /// The user with this email compared without regard to letter case, or `None`.
    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, StoreError>;

    /// Every user, oldest first.
    async fn list_users(&self) -> Result<Vec<User>, StoreError>;

    /// Replaces the user's email, name and role. `NotFound` when the user does not exist;
    /// `Invalid` when another user has the email, compared without regard to letter case.
    async fn update_user(&self, user: &User) -> Result<(), StoreError>;

    /// Removes the user with their sessions, grants, devices, drafts, identity grants and default identities. `NotFound` when the user does not exist.
    async fn delete_user(&self, id: UserId) -> Result<(), StoreError>;

    /// The archives the user was granted, oldest archive first. Empty for an unknown user.
    async fn grants(&self, user: UserId) -> Result<Vec<ArchiveId>, StoreError>;

    /// Replaces the user's grants with `archives`; unknown archives are skipped. Archives that stay
    /// granted keep their [`GrantRole`]; new ones are granted as [`GrantRole::Reader`]. `NotFound`
    /// when the user does not exist.
    async fn set_grants(&self, user: UserId, archives: &[ArchiveId]) -> Result<(), StoreError>;

    /// The user's PHC password hash; `None` when the user has no password. `NotFound` when the
    /// user does not exist.
    async fn password_hash(&self, user: UserId) -> Result<Option<String>, StoreError>;

    /// Replaces the user's password hash. `NotFound` when the user does not exist.
    async fn set_password_hash(&self, user: UserId, hash: &str) -> Result<(), StoreError>;

    /// Records a session of the user valid until `expires_at`. `NotFound` when the user does not
    /// exist; `Invalid` when a session with this hash exists.
    async fn create_session(
        &self,
        hash: &SessionHash,
        user: UserId,
        expires_at: Timestamp,
    ) -> Result<(), StoreError>;

    /// The user of the session, or `None` when there is no such session or it expired at or
    /// before `now`.
    async fn session_user(
        &self,
        hash: &SessionHash,
        now: Timestamp,
    ) -> Result<Option<User>, StoreError>;

    /// When the session started, or `None` when there is no such session. An expired session
    /// that has not been removed still answers.
    async fn session_started_at(&self, hash: &SessionHash)
    -> Result<Option<Timestamp>, StoreError>;

    /// Ends the session. Idempotent.
    async fn delete_session(&self, hash: &SessionHash) -> Result<(), StoreError>;

    /// Ends every session of the user. Idempotent.
    async fn delete_user_sessions(&self, user: UserId) -> Result<(), StoreError>;

    /// Removes sessions that expired at or before `now`; returns how many.
    async fn delete_expired_sessions(&self, now: Timestamp) -> Result<u64, StoreError>;
}

/// Saved copies of remote images, shared by every message and archive that links the same URL.
#[async_trait]
pub trait RemoteImageStore: Send + Sync {
    /// The copy or failure recorded for `key`, if any.
    async fn remote_copy(&self, key: &str) -> Result<Option<RemoteCopy>, StoreError>;

    /// Records a copy or a failure for `copy.key`, replacing what was recorded before.
    async fn save_remote_copy(&self, copy: &RemoteCopy) -> Result<(), StoreError>;

    /// Up to `limit` messages of `archive` archived `when`, whose remote images were never looked
    /// at, oldest first.
    async fn unscanned_messages(
        &self,
        archive: ArchiveId,
        when: ArchivedWhen,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError>;

    /// How many messages [`RemoteImageStore::unscanned_messages`] would return without a limit.
    async fn count_unscanned(
        &self,
        archive: ArchiveId,
        when: ArchivedWhen,
    ) -> Result<u64, StoreError>;

    /// The saved and failed images the archive's messages link to, each counted once.
    async fn remote_copy_totals(&self, archive: ArchiveId) -> Result<RemoteCopyTotals, StoreError>;

    /// Up to `limit` failed copies (no saved bytes) the archive's messages link to, last tried
    /// before `tried_before`, oldest attempt first. Saving a new attempt moves a copy past
    /// `tried_before`, so a retry run can page through them once.
    async fn failed_remote_copies(
        &self,
        archive: ArchiveId,
        tried_before: Timestamp,
        limit: u32,
    ) -> Result<Vec<RemoteCopy>, StoreError>;

    /// Marks `message` as looked at and links it to the remote copies in `keys`, which were saved
    /// with [`RemoteImageStore::save_remote_copy`] first; unknown keys are ignored. Idempotent.
    async fn record_remote_scan(
        &self,
        message: MessageId,
        keys: &[String],
    ) -> Result<(), StoreError>;

    /// The remote copies with saved bytes that `message` links to, in no particular order.
    async fn message_remote_copies(
        &self,
        message: MessageId,
    ) -> Result<Vec<RemoteCopy>, StoreError>;
}

/// Instance-wide settings, as text values by key.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    /// The value stored under `key`, `None` when unset.
    async fn setting(&self, key: &str) -> Result<Option<String>, StoreError>;

    /// Stores `value` under `key`, replacing any earlier value; `None` removes it. Idempotent.
    async fn set_setting(&self, key: &str, value: Option<&str>) -> Result<(), StoreError>;
}

/// Files derived from attachment blobs, such as thumbnails and extracted text, kept as blobs of
/// their own. Everything here can be dropped and made again from the source blobs.
#[async_trait]
pub trait DerivedStore: Send + Sync {
    /// What was recorded for `source` and `kind`, or `None` when nothing was.
    async fn derived(
        &self,
        source: &BlobId,
        kind: DerivedKind,
    ) -> Result<Option<Derived>, StoreError>;

    /// Records what was made from `source`, replacing any earlier record of that kind.
    /// Idempotent.
    async fn set_derived(
        &self,
        source: &BlobId,
        kind: DerivedKind,
        derived: &Derived,
    ) -> Result<(), StoreError>;

    /// Forgets every record of `kind`, so it is made again on next use. Returns the derived blobs
    /// that nothing references any more, so the caller can delete them.
    async fn clear_derived(&self, kind: DerivedKind) -> Result<Vec<BlobId>, StoreError>;

    /// Forgets what was recorded for `source` and `kind`, so it is made again on next use.
    /// Returns the derived blob when nothing references it any more, so the caller can delete
    /// it. Nothing recorded is not an error.
    async fn forget_derived(
        &self,
        source: &BlobId,
        kind: DerivedKind,
    ) -> Result<Option<BlobId>, StoreError>;
}

/// Address books and calendars with every version of their items, as raw blobs, and the
/// normalized fields of each item's latest version.
#[async_trait]
pub trait PimStore: Send + Sync {
    /// The collection of the archive with this account, kind and `remote_id`, created when
    /// missing. Idempotent; a different name or color replaces the stored one, and a collection
    /// marked removed is no longer. `NotFound` when the archive, or a given account, does not
    /// exist; `Invalid` when the account belongs to another archive.
    async fn upsert_collection(&self, new: NewCollection) -> Result<Collection, StoreError>;

    /// The collection, or `None`.
    async fn get_collection(&self, id: CollectionId) -> Result<Option<Collection>, StoreError>;

    /// Collections of the archive with item counts, removed ones included, by kind, then name,
    /// then id.
    async fn list_collections(
        &self,
        archive: ArchiveId,
    ) -> Result<Vec<CollectionSummary>, StoreError>;

    /// Replaces the collection's sync token. `NotFound` for an unknown collection.
    async fn set_sync_token(
        &self,
        collection: CollectionId,
        token: Option<&str>,
    ) -> Result<(), StoreError>;

    /// Records a finished sync of the collection from its source: its sync token and tag as of
    /// that sync (each replacing the stored one, `None` clearing it) and when it ended.
    /// `NotFound` for an unknown collection.
    async fn set_collection_synced(
        &self,
        collection: CollectionId,
        token: Option<&str>,
        ctag: Option<&str>,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Marks the collection and every item of it not yet removed as removed at `at`. Nothing is
    /// deleted. Idempotent: earlier removal times are kept. `NotFound` for an unknown collection.
    async fn mark_collection_removed(
        &self,
        collection: CollectionId,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Records that the item `remote_id` of the collection had these bytes at `seen_at`.
    ///
    /// The item is created when missing. A new version is added only when `blob_id` differs from
    /// the latest version's; then the item's kind and key and its normalized fields follow the new
    /// version. The same bytes again change nothing, except that an item marked removed is no
    /// longer and the item's current tag (see [`PimStore::item_etags`]) becomes `etag`. Versions
    /// are never removed or rewritten. A created item, a new version or a return from removal
    /// writes an `item` change-log row with no item change in the same transaction; the same
    /// bytes again write none. `NotFound` for an unknown collection.
    async fn record_version(&self, version: NewItemVersion) -> Result<RecordedVersion, StoreError>;

    /// Records that message `message_id` carried these bytes of the item `remote_id`, as an
    /// invitation, update, reply or cancellation does.
    ///
    /// The item is created when missing. When one of its versions already has these bytes, no
    /// version is added and the message is linked to that version (idempotent). Otherwise the
    /// version is placed among the item's versions by `mail.sequence`, then `mail.dtstamp`, then
    /// `mail.sent_at` (a missing value before any present one), then blob id, and the versions
    /// are numbered 1, 2, … in that order: versions after the new one move up by one. So the same
    /// messages give the same numbered versions in whatever order they are recorded. When the
    /// added version is the latest, the item's kind, key and normalized fields follow `version`;
    /// callers
    /// that derive the fields from several versions replace them afterwards with
    /// [`PimStore::set_pim_index`]. An item marked removed is no longer. A created item, an added
    /// version or a return from removal writes an `item` change-log row with no item change in
    /// the same transaction; linking another message to stored bytes writes none. `NotFound` for
    /// an unknown collection or message; `Invalid` when the message is in another archive.
    async fn record_mail_version(
        &self,
        version: NewMailVersion,
    ) -> Result<RecordedVersion, StoreError>;

    /// The messages the item's versions were found in, by version number, then message id.
    /// Empty for an item not read from mail, or an unknown one.
    async fn version_messages(&self, item: ItemId) -> Result<Vec<VersionSource>, StoreError>;

    /// The item versions found in the message, by item id, then version number. Empty when
    /// there are none.
    async fn message_versions(&self, message: MessageId) -> Result<Vec<VersionSource>, StoreError>;

    /// The `remote_id`s of the collection's items that are not marked removed, sorted.
    async fn live_remote_ids(&self, collection: CollectionId) -> Result<Vec<String>, StoreError>;

    /// The `remote_id`s of the collection's items that are not marked removed, each with the
    /// source's tag given with the last [`PimStore::record_version`] of it, sorted by
    /// `remote_id`. Empty for an unknown collection.
    async fn item_etags(
        &self,
        collection: CollectionId,
    ) -> Result<Vec<(String, Option<String>)>, StoreError>;

    /// Marks the collection's items with these `remote_id`s as removed at `at`, unless they
    /// already are. Unknown ids are ignored. Returns how many were newly marked; each writes an
    /// `item` change-log row with no item change in the same transaction.
    async fn mark_items_removed(
        &self,
        collection: CollectionId,
        remote_ids: &[String],
        at: Timestamp,
    ) -> Result<u64, StoreError>;

    /// The item with its normalized fields, or `None`.
    async fn get_item(&self, id: ItemId) -> Result<Option<ItemSummary>, StoreError>;

    /// Every version of the item by number, oldest first; empty for an unknown item.
    async fn item_versions(&self, id: ItemId) -> Result<Vec<ItemVersion>, StoreError>;

    /// Items of the archive in the query's order, narrowed by its filters, a page at a time.
    /// `Invalid` for a cursor from another query.
    async fn list_items(&self, query: &ItemQuery) -> Result<Page<ItemSummary>, StoreError>;

    /// Replaces the item's normalized fields, e.g. when rebuilding them. Fields made from an older
    /// version than the item's latest (`index.version` below it) are ignored, so a rebuild that
    /// raced a new version never overwrites it. `NotFound` for an unknown item.
    async fn set_pim_index(&self, item: ItemId, index: &PimIndex) -> Result<(), StoreError>;

    /// Up to `limit` item ids of every archive in ascending order, starting after `after`, for
    /// rebuilding what is derived from them. A page shorter than `limit` is the last.
    async fn item_ids(&self, after: Option<ItemId>, limit: u32) -> Result<Vec<ItemId>, StoreError>;
}

/// People derived from contacts and mail, and what the mail says about a set of addresses.
/// The people are derived data: whoever derives them replaces them whole, per archive.
#[async_trait]
pub trait PeopleStore: Send + Sync {
    /// Every address of the archive's mail with its counts, and the owner's addresses. Empty
    /// for an archive without mail or an unknown one. A reverse alias recorded with a sender
    /// (see [`AliasStore::set_message_alias`]) counts as that sender, with the sender's name, and
    /// one recorded by [`AliasStore::put_reverse_alias`] counts as its contact; the
    /// archive's aliases (known or seen in mail) are among the owner's addresses.
    async fn mail_addresses(&self, archive: ArchiveId) -> Result<MailAddresses, StoreError>;

    /// Replaces every person of the archive with `people`, in one step: readers see the old
    /// people or the new ones, never a mix. A person of another archive in `people` is
    /// `Invalid`; `NotFound` for an unknown archive.
    async fn replace_people(&self, archive: ArchiveId, people: &[Person])
    -> Result<(), StoreError>;

    /// People of the archive by name without regard to letter case, people without a name
    /// last, then by id, narrowed by the query's filters, a page at a time. `Invalid` for a
    /// cursor from another query.
    async fn list_people(&self, query: &PersonQuery) -> Result<Page<Person>, StoreError>;

    /// How many people [`PeopleStore::list_people`] lists for the query, ignoring its paging.
    async fn count_people(&self, query: &PersonQuery) -> Result<u64, StoreError>;

    /// The person, or `None`.
    async fn get_person(&self, id: PersonId) -> Result<Option<Person>, StoreError>;

    /// The person of each of `emails` that belongs to one in the archive, compared without
    /// regard to letter case, as `(email as given, person)` in the order of `emails`.
    async fn people_by_email(
        &self,
        archive: ArchiveId,
        emails: &[String],
    ) -> Result<Vec<(String, PersonId)>, StoreError>;

    /// The archive's mail with any of `emails` (compared without regard to letter case) in
    /// `From`, `To`, `Cc` or `Bcc`, or a reverse alias recorded for one of them (from mail or [`AliasStore::put_reverse_alias`]): counts, years, at most `threads` conversations and at most
    /// `attachments` attachments. Empty for no addresses.
    async fn correspondence(
        &self,
        archive: ArchiveId,
        emails: &[String],
        threads: u32,
        attachments: u32,
    ) -> Result<Correspondence, StoreError>;

    /// Up to `limit` items of the archive of `kind`, removed ones too, whose normalized emails
    /// include one of `emails`, the latest start first, items without a start last, then by id.
    async fn items_by_email(
        &self,
        archive: ArchiveId,
        kind: ItemKind,
        emails: &[String],
        limit: u32,
    ) -> Result<Vec<ItemSummary>, StoreError>;

    /// Stores a split or join made by hand. `NotFound` for an unknown archive; `Invalid` when its
    /// id is already stored, a join has fewer than two keys, or a split has no keys or no `from`.
    async fn add_person_override(&self, value: &PersonOverride) -> Result<(), StoreError>;

    /// The archive's splits and joins, oldest first (by id). Empty for an unknown archive.
    async fn person_overrides(&self, archive: ArchiveId)
    -> Result<Vec<PersonOverride>, StoreError>;

    /// The split or join, or `None`.
    async fn get_person_override(
        &self,
        id: PersonOverrideId,
    ) -> Result<Option<PersonOverride>, StoreError>;

    /// Forgets the split or join; whether it was stored. The people stay as they are until they
    /// are rebuilt.
    async fn remove_person_override(&self, id: PersonOverrideId) -> Result<bool, StoreError>;
}

/// The derived calendar: each event item's group of copies and its instances. Whoever derives
/// them replaces them per item; nothing here is the truth.
#[async_trait]
pub trait CalendarStore: Send + Sync {
    /// Replaces what the calendar knows about the item: its series facts and its stored
    /// instances, in one step. `None` forgets both, for an item that is no event or cannot be
    /// read. `NotFound` for an unknown item; `Invalid` when `series` names another item.
    async fn replace_series(
        &self,
        item: ItemId,
        series: Option<&EventSeries>,
        occurrences: &[NewOccurrence],
    ) -> Result<(), StoreError>;

    /// The series facts of the item, or `None`.
    async fn series(&self, item: ItemId) -> Result<Option<EventSeries>, StoreError>;

    /// Stored instances of the archive's items that overlap the query's span, by start, then
    /// item id, then `recurrence_id`, narrowed by its filters. Items whose stored instances do
    /// not cover the whole span (see [`EventSeries::window`]) are left out; see
    /// [`CalendarStore::uncovered_series`].
    async fn occurrences(&self, query: &OccurrenceQuery) -> Result<Vec<Occurrence>, StoreError>;

    /// Items of the archive, narrowed by the query's filters, that repeat into the query's span
    /// (by [`EventSeries::starts_at`] and [`EventSeries::ends_at`]) but whose stored instances do
    /// not cover all of it, by item id. Their instances there have to be worked out from their
    /// normalized fields.
    async fn uncovered_series(
        &self,
        query: &OccurrenceQuery,
    ) -> Result<Vec<UncoveredSeries>, StoreError>;

    /// Every item of the group with its series facts, removed ones too, by item id. Empty for
    /// an unknown group.
    async fn group_members(&self, group: EventGroupId) -> Result<Vec<GroupMember>, StoreError>;
}

/// What the store references in the blob store, for backups and consistency checks.
#[async_trait]
pub trait InventoryStore: Send + Sync {
    /// Up to `limit` blobs that some record references: a message, an attachment, an item
    /// version or its picture, a saved remote image, an outbox message, a kept draft version, an append waiting in the mail changes, the bytes of a contact or calendar change, or a derived file. Each blob is listed once, in ascending order of id, starting after
    /// `after` (from the first when `None`), so paging with the last id of each page visits every
    /// referenced blob exactly once. A page shorter than `limit` is the last one.
    async fn referenced_blobs(
        &self,
        after: Option<&BlobId>,
        limit: u32,
    ) -> Result<Vec<BlobId>, StoreError>;

    /// Whether some record references `blob`, by the same rule as
    /// [`InventoryStore::referenced_blobs`]: `true` exactly when that listing holds it.
    async fn is_referenced(&self, blob: &BlobId) -> Result<bool, StoreError>;
}

/// The whole metadata store.
pub trait Store:
    ArchiveStore
    + MessageStore
    + LabelStore
    + SyncStore
    + UserStore
    + RemoteImageStore
    + SettingsStore
    + DerivedStore
    + InventoryStore
    + TaskStore
    + PimStore
    + PeopleStore
    + CalendarStore
    + LedgerStore
    + DeletionStore
    + WebhookStore
    + AliasStore
    + InsightStore
    + CategoryStore
    + WriteStore
    + SendStore
    + MailChangeStore
    + ItemChangeStore
    + DeviceStore
    + ChangeStore
    + MailViewStore
    + ServeStore
    + ScheduleStore
    + RuleStore
{
}

impl<T> Store for T where
    T: ArchiveStore
        + MessageStore
        + LabelStore
        + SyncStore
        + UserStore
        + RemoteImageStore
        + SettingsStore
        + DerivedStore
        + InventoryStore
        + TaskStore
        + PimStore
        + PeopleStore
        + CalendarStore
        + LedgerStore
        + DeletionStore
        + WebhookStore
        + AliasStore
        + InsightStore
        + CategoryStore
        + WriteStore
        + SendStore
        + MailChangeStore
        + ItemChangeStore
        + DeviceStore
        + ChangeStore
        + MailViewStore
        + ServeStore
        + ScheduleStore
        + RuleStore
{
}
