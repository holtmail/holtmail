//! Contract for Holtmail's metadata: archives, accounts, folders, messages, threads, sync state,
//! users and their sessions.
//!
//! The store never holds message bytes; those live in a `BlobStore` and are referenced by
//! [`holtmail_blob::BlobId`]. Everything in the store except accounts, archives and sync state can
//! be rebuilt from the blobs.

mod types;

#[cfg(feature = "testing")]
pub mod testing;

use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{AccountId, ArchiveId, FolderId, MessageId, ThreadId, Timestamp, UserId};

pub use types::*;

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

    /// Removes the archive with its accounts, folders, messages, attachments, sync state and
    /// grants, and the remote copies no remaining message links to. Returns the blobs that no
    /// message, attachment or remote copy references any more, so the caller can delete them.
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

    /// Removes the account with its folders, locations and sync state. Its messages stay in the
    /// archive. `NotFound` when the account does not exist.
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
    /// the result does not depend on the order messages arrive in.
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

    /// Messages newest first by `date`, ties broken by id.
    async fn list_messages(&self, query: &MessageQuery) -> Result<Page<Message>, StoreError>;

    /// Flags, thread size and folders of each message among `ids` that exists, for showing a list
    /// in one call. With `folder`, `flags` are the message's flags in that folder (empty when it
    /// is not located there); otherwise their union over every location not removed.
    async fn message_extras(
        &self,
        ids: &[MessageId],
        folder: Option<FolderId>,
    ) -> Result<HashMap<MessageId, MessageExtras>, StoreError>;

    /// Messages of the thread, oldest first.
    async fn thread_messages(&self, thread: ThreadId) -> Result<Vec<Message>, StoreError>;

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

    /// UIDs of the folder's locations that are not removed, ascending.
    async fn folder_uids(&self, folder: FolderId) -> Result<Vec<u64>, StoreError>;

    /// The message at `uid` in the folder, removed or not, or `None`.
    async fn message_at_uid(
        &self,
        folder: FolderId,
        uid: u64,
    ) -> Result<Option<MessageId>, StoreError>;

    /// Marks the folder's locations with these UIDs as removed at `at`. Messages stay archived.
    async fn mark_removed(
        &self,
        folder: FolderId,
        uids: &[u64],
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Forgets every UID in the folder, for when the server changed its `UIDVALIDITY`. Locations
    /// stay; the next sync re-attaches UIDs.
    async fn forget_uids(&self, folder: FolderId) -> Result<(), StoreError>;
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

    /// Removes the user with their sessions and grants. `NotFound` when the user does not exist.
    async fn delete_user(&self, id: UserId) -> Result<(), StoreError>;

    /// The archives the user was granted, oldest archive first. Empty for an unknown user.
    async fn grants(&self, user: UserId) -> Result<Vec<ArchiveId>, StoreError>;

    /// Replaces the user's grants with `archives`; unknown archives are skipped. `NotFound` when
    /// the user does not exist.
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

/// The whole metadata store.
pub trait Store:
    ArchiveStore + MessageStore + SyncStore + UserStore + RemoteImageStore + SettingsStore
{
}

impl<T> Store for T where
    T: ArchiveStore + MessageStore + SyncStore + UserStore + RemoteImageStore + SettingsStore
{
}
