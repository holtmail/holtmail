use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, FolderId, ItemId, MessageId, UserId};

use crate::StoreError;

/// What [`DeletionStore::delete_messages`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeletedMessages {
    /// The messages removed, in the order asked; ids not in the archive are left out.
    pub messages: Vec<MessageId>,
    /// Blobs nothing references any more: message bytes, attachments, remote images, item
    /// versions read from the mail and what was derived from them. The caller deletes them.
    pub orphans: Vec<BlobId>,
    /// Calendar items read from the mail that lost versions and still have others; their
    /// normalized fields have to be rebuilt.
    pub changed_items: Vec<ItemId>,
    /// Calendar items read only from the deleted mail, now removed with their versions and
    /// calendar instances.
    pub removed_items: Vec<ItemId>,
}

/// Deleting messages on purpose, and keeping them deleted.
///
/// The archive otherwise never forgets: a message removed on the server stays. A deleted message
/// leaves a tombstone of its bytes' hash in its archive and of every folder and UID it was seen
/// at, so syncs and imports do not bring it back.
#[async_trait]
pub trait DeletionStore: Send + Sync {
    /// Removes the messages of `archive` among `ids` in one transaction, with their locations,
    /// attachments, labels (a label left without messages goes too), addresses, thread links,
    /// remote image links, insights, the links of calendar versions to them, and calendar
    /// versions read only from them. Records tombstones for their bytes and for every folder and UID they had,
    /// and enters each message in the ledger as `deleted` by `actor`. Idempotent: ids already
    /// gone or in another archive are skipped.
    async fn delete_messages(
        &self,
        archive: ArchiveId,
        ids: &[MessageId],
        actor: Option<UserId>,
    ) -> Result<DeletedMessages, StoreError>;

    /// Whether bytes with this hash were deleted from `archive` and must not be archived there
    /// again.
    async fn is_deleted(&self, archive: ArchiveId, blob: &BlobId) -> Result<bool, StoreError>;

    /// Records that `uid` in `folder` holds a deleted message, so
    /// [`crate::MessageStore::folder_uids`] lists it and sources do not offer it again.
    /// Idempotent.
    async fn record_deleted_location(&self, folder: FolderId, uid: u64) -> Result<(), StoreError>;
}
