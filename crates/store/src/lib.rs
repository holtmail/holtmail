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

/// The whole metadata store.
pub trait Store: ArchiveStore + MessageStore + SyncStore + UserStore {}

impl<T: ArchiveStore + MessageStore + SyncStore + UserStore> Store for T {}
