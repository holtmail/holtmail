use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{
    ArchiveId, CollectionId, DraftId, FolderId, ItemChangeId, ItemId, MessageId, OutboxId,
    ThreadId, Timestamp, UserId,
};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// What a row of the change log is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A contact or calendar item, or an edit of one.
    Item,
    /// A message entered or left the archive.
    Message,
    /// A message gained or lost a place: a location, an open mail change that moves, deletes
    /// or appends, or a user's local place.
    Place,
    /// Flags or labels of a message changed: synced, held or a user's local flags.
    Flags,
    /// A message left a thread because threads merged.
    Thread,
    /// A folder was added, changed role or went away, or a user's subscription changed.
    Mailbox,
    /// An outbox message changed.
    Submission,
    /// A user's draft was started, saved or deleted.
    Draft,
}

impl ChangeKind {
    /// Every kind, in a fixed order.
    pub const ALL: [ChangeKind; 8] = [
        Self::Item,
        Self::Message,
        Self::Place,
        Self::Flags,
        Self::Thread,
        Self::Mailbox,
        Self::Submission,
        Self::Draft,
    ];

    /// The lowercase name used in storage.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Message => "message",
            Self::Place => "place",
            Self::Flags => "flags",
            Self::Thread => "thread",
            Self::Mailbox => "mailbox",
            Self::Submission => "submission",
            Self::Draft => "draft",
        }
    }

    /// The kind named by [`ChangeKind::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// One row of the change log: something served changed. Rows carry ids only, never content.
///
/// Which ids are set depends on the kind: `item` sets the collection, item or href, and the item
/// change when an edit was recorded or changed state (an upstream version, removal or return of an
/// item, written by `record_version`, `record_mail_version` and `mark_items_removed`, has none;
/// an item deleted for good by `delete_messages`, because every version came from deleted mail,
/// carries the item and the href it was last served under, and an item that only lost versions
/// that way carries the item alone);
/// `message` the message (and its thread when it entered); `place` and `flags` the message and,
/// for a place in a folder, the folder; `thread` the message and the thread it
/// left; `mailbox` the folder, or nothing for a subscription; `submission` the outbox message;
/// `draft` the draft. Rows of one user's own state (local flags, local places, subscriptions,
/// drafts) carry `user_id`; every other row concerns everyone who reads the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLogEntry {
    /// Position in the log, counting up across the instance without reuse.
    pub seq: u64,
    /// The archive.
    pub archive_id: ArchiveId,
    /// What the row is about.
    pub kind: ChangeKind,
    /// The collection of an item.
    pub collection_id: Option<CollectionId>,
    /// The item, when there is one.
    pub item_id: Option<ItemId>,
    /// The href of an item change with no item, or the last served href of an item deleted
    /// for good.
    pub href: Option<String>,
    /// The item change that was recorded or changed state.
    pub item_change_id: Option<ItemChangeId>,
    /// The message.
    pub message_id: Option<MessageId>,
    /// The folder.
    pub folder_id: Option<FolderId>,
    /// The thread a message entered or left.
    pub thread_id: Option<ThreadId>,
    /// The outbox message.
    pub outbox_id: Option<OutboxId>,
    /// The draft.
    pub draft_id: Option<DraftId>,
    /// The user whose own state changed; `None` for a change everyone reading the archive sees.
    pub user_id: Option<UserId>,
    /// When.
    pub at: Timestamp,
}

/// How often a [`ChangeWatch`] looks at the log by itself, so a missed wake-up delays a
/// listener by at most this long.
pub const CHANGE_POLL: Duration = Duration::from_secs(30);

/// A running watch on the change log. Not shared between tasks.
#[async_trait]
pub trait ChangeWatch: Send {
    /// Waits until the log holds a row newer than the watch has returned so far (at its start,
    /// newer than the log's head then) and returns the highest committed `seq`. It wakes after
    /// each commit that wrote a row, and looks by itself every [`CHANGE_POLL`] in case a
    /// wake-up was missed, so a commit is noticed at the latest one interval later. Dropping
    /// the future while it waits loses nothing: the next call still sees the commit.
    async fn next(&mut self) -> u64;
}

/// The change log that drives every served state: IMAP `MODSEQ`, JMAP states and DAV sync
/// tokens.
///
/// Every writer of something served writes its row in the same transaction as the change, and
/// only when something changed: writing what is already stored adds no row. `seq` order is
/// visibility order: once a read returned a row, no row with a lower `seq` becomes visible
/// later, so a reader that remembers the highest `seq` it saw never misses a row.
#[async_trait]
pub trait ChangeStore: Send + Sync {
    /// The highest `seq` ever handed out, `0` before the first row. It never goes down, also
    /// when rows are deleted with their archive.
    async fn latest_seq(&self) -> Result<u64, StoreError>;

    /// The archive's rows after `after` (from the start when `None`), oldest first, at most
    /// `limit` (capped at 1 000). Rows of every user are included; readers filter by
    /// [`ChangeLogEntry::user_id`].
    async fn changes_since(
        &self,
        archive: ArchiveId,
        after: Option<u64>,
        limit: u32,
    ) -> Result<Vec<ChangeLogEntry>, StoreError>;

    /// Rows of every archive after `after`, oldest first, at most `limit` (capped at 1 000).
    async fn all_changes_since(
        &self,
        after: u64,
        limit: u32,
    ) -> Result<Vec<ChangeLogEntry>, StoreError>;

    /// The highest `seq` of each kind among the archive's rows that concern everyone or `user`
    /// (rows of other users' own state are left out); a kind with no such row is missing. Cheap
    /// enough to ask on every request: JMAP states come from it.
    async fn archive_heads(
        &self,
        archive: ArchiveId,
        user: Option<UserId>,
    ) -> Result<HashMap<ChangeKind, u64>, StoreError>;

    /// Starts a watch on the log.
    async fn watch_changes(&self) -> Result<Box<dyn ChangeWatch>, StoreError>;

    /// Deletes the oldest rows of every archive: each row up to, not including, the first one
    /// written at or after `before`, so what stays is always every row after one `seq`.
    /// Returns [`ChangeStore::pruned_through`] as it is afterwards. Raises the mark before any
    /// row goes, so a reader never finds rows missing without the mark saying so. Idempotent;
    /// never lowers the mark, never changes [`ChangeStore::latest_seq`].
    async fn prune_changes(&self, before: Timestamp) -> Result<u64, StoreError>;

    /// The highest `seq` pruning may have deleted; `0` before the first prune. A reader whose
    /// remembered `seq` is lower can no longer be told everything that changed since.
    async fn pruned_through(&self) -> Result<u64, StoreError>;
}
