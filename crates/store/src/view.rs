use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, FolderId, MessageId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

use crate::{FolderRole, StoreError};

/// The flag that is never kept as a local flag: deleting has no local form.
pub const DELETED_FLAG: &str = "\\Deleted";

/// Which messages [`MailViewStore::effective_flags`] reads, and through whose eyes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveFlagsQuery {
    /// The reader, whose local flags apply.
    pub user: UserId,
    /// The messages.
    pub messages: Vec<MessageId>,
    /// Only places in these folders count; every place of the message when `None`.
    pub folders: Option<Vec<FolderId>>,
    /// Whether local flags apply; `false` when the server runs with `[serve] local_flags =
    /// false`.
    pub local_flags: bool,
}

/// A user's own setting of one flag on one message, kept by Holtmail and never sent upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFlag {
    /// The flag as it was first written, e.g. `\Seen`.
    pub flag: String,
    /// `true` when the user set it, `false` when the user cleared it.
    pub present: bool,
}

/// One message's local flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFlags {
    /// The message.
    pub message_id: MessageId,
    /// Its local flags, by flag compared ignoring ASCII case.
    pub flags: Vec<LocalFlag>,
}

/// A message shown in one role mailbox for one user only, with no upstream folder behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPlace {
    /// Whose view it is in.
    pub user_id: UserId,
    /// The message's archive.
    pub archive_id: ArchiveId,
    /// The role mailbox it shows in.
    pub role: FolderRole,
    /// The message.
    pub message_id: MessageId,
    /// When it was added.
    pub created_at: Timestamp,
}

/// A user's explicit choice to subscribe to a served mailbox or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The mailbox's key, as the mail view names it.
    pub mailbox: String,
    /// Whether it is subscribed.
    pub subscribed: bool,
}

/// How many messages a folder holds, by its live places.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderCounts {
    /// Messages with a place there that is not removed.
    pub messages: u64,
    /// Those of them whose synced flags lack `\Seen`.
    pub unseen: u64,
}

/// What the served mail view reads and keeps per user: flags merged across places, local
/// flags, local places, subscriptions and folder counts (SERVE.md, "The mail view").
///
/// Every write that changes something writes a change log row of the user's own
/// ([`ChangeLogEntry::user_id`](crate::ChangeLogEntry) set) in the same transaction; a write
/// that changes nothing writes none. Flags compare ignoring ASCII case.
#[async_trait]
pub trait MailViewStore: Send + Sync {
    /// The flags each message among the query's messages shows, read in one consistent
    /// snapshot: the synced flags of each place, with the open (held or sending) flags changes
    /// of that place applied on top oldest first, joined across places (an open append counts
    /// as a live place in its folder with the flags it carries, until a location is there); then, when the query
    /// asks for them, the user's local flags (a set one is added, a cleared one removed).
    /// Without `folders`, the places are the message's live locations, or every location it
    /// ever had when none is live, so a message only in Holtmail keeps its last flags. With
    /// `folders`, a message with no place there gets only its local flags. Unknown messages
    /// are left out; flags are deduplicated ignoring case and sorted.
    async fn effective_flags(
        &self,
        query: &EffectiveFlagsQuery,
    ) -> Result<HashMap<MessageId, Vec<String>>, StoreError>;

    /// Sets (`add`) and clears (`remove`) flags as the user's local flags on the message,
    /// replacing what the user set before for those flags, and returns whether anything
    /// changed. The store does not check `[serve] local_flags`, rights or scopes; callers do.
    /// `Invalid` for `\Deleted`, an empty flag or a flag both added and removed; `NotFound`
    /// for an unknown user or message.
    async fn set_local_flags(
        &self,
        user: UserId,
        message: MessageId,
        add: &[String],
        remove: &[String],
        at: Timestamp,
    ) -> Result<bool, StoreError>;

    /// The user's local flags on the message, by flag; empty when there are none.
    async fn local_flags(
        &self,
        user: UserId,
        message: MessageId,
    ) -> Result<Vec<LocalFlag>, StoreError>;

    /// Every message of the archive with local flags of the user, by message id.
    async fn local_flagged(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Vec<LocalFlags>, StoreError>;

    /// Drops the user's local flags in the archive, of one message or of all; returns how many
    /// messages had some. Idempotent.
    async fn drop_local_flags(
        &self,
        user: UserId,
        archive: ArchiveId,
        message: Option<MessageId>,
        at: Timestamp,
    ) -> Result<u64, StoreError>;

    /// Shows the message in the user's role mailbox; whether it was not there yet. `NotFound`
    /// for an unknown user or message.
    async fn add_local_place(
        &self,
        user: UserId,
        role: FolderRole,
        message: MessageId,
        at: Timestamp,
    ) -> Result<bool, StoreError>;

    /// Takes the message out of the user's role mailbox; whether it was there.
    async fn remove_local_place(
        &self,
        user: UserId,
        role: FolderRole,
        message: MessageId,
        at: Timestamp,
    ) -> Result<bool, StoreError>;

    /// The user's local places in the archive, of one role or of all, oldest first, then by
    /// message id.
    async fn local_places(
        &self,
        user: UserId,
        archive: ArchiveId,
        role: Option<FolderRole>,
    ) -> Result<Vec<LocalPlace>, StoreError>;

    /// Records the user's choice for the mailbox; whether it differs from the stored one (a
    /// first choice always does). The store keeps choices only: which mailboxes are subscribed
    /// by default is the mail view's rule. `Invalid` for an empty key; `NotFound` for an
    /// unknown user or archive.
    async fn set_subscription(
        &self,
        user: UserId,
        archive: ArchiveId,
        mailbox: &str,
        subscribed: bool,
        at: Timestamp,
    ) -> Result<bool, StoreError>;

    /// The user's explicit choices in the archive, by mailbox key.
    async fn subscriptions(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Vec<Subscription>, StoreError>;

    /// Counts of each folder among `folders` that exists; a folder with no live place counts
    /// zero.
    async fn folder_counts(
        &self,
        folders: &[FolderId],
    ) -> Result<HashMap<FolderId, FolderCounts>, StoreError>;
}
