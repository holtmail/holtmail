use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{
    ArchiveId, CollectionId, ItemChangeId, ItemId, MessageId, OutboxId, Timestamp, UserId,
};
use serde::{Deserialize, Serialize};

use crate::{Collection, ItemSummary, ItemVersion, NewItemVersion, StoreError};

macro_rules! names {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// Every value, in a fixed order.
            pub const ALL: &'static [$name] = &[$(Self::$variant),+];

            /// The lowercase name used in storage and the API.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// The value named by `as_str`, or `None`.
            pub fn parse(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == name)
            }
        }
    };
}

/// Whether and how a collection takes edits from devices and the web app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Nothing may be changed. The default for collections no account owns ("From mail") and
    /// those of import accounts (imported files).
    ReadOnly,
    /// Edits are accepted and held in Holtmail; nothing is sent upstream. The default for
    /// collections read from a server account.
    Held,
    /// Edits are accepted, held, and sent upstream in order by the writer.
    WriteThrough,
}

names!(WriteMode {
    ReadOnly => "read_only",
    Held => "held",
    WriteThrough => "write_through",
});

/// Where a stored item version came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionOrigin {
    /// What a source showed: a sync, an import or a message.
    #[default]
    Upstream,
    /// What the upstream server holds after Holtmail sent it a change
    /// ([`ItemVersion::change_id`] names the change).
    Writeback,
}

names!(VersionOrigin {
    Upstream => "upstream",
    Writeback => "writeback",
});

/// What a change does to its item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOp {
    /// A new item under a client's href.
    Create,
    /// New content for an existing item, or for a held create.
    Update,
    /// Removes the item from what is served.
    Delete,
}

names!(ChangeOp {
    Create => "create",
    Update => "update",
    Delete => "delete",
});

/// What a change amounts to, compared with the upstream version it is based on. Computed by the
/// caller (`pim-edit`); the store only checks that it fits the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// Nothing that matters changed (a client rewrote `DTSTAMP` or `PRODID`).
    Noop,
    /// Only alarms, acknowledgements and client properties changed.
    Personal,
    /// Only the user's own answer to an invitation changed.
    Reply,
    /// Anything else in an existing item.
    Edit,
    /// A new item.
    Create,
    /// A removed item.
    Delete,
}

names!(ChangeClass {
    Noop => "noop",
    Personal => "personal",
    Reply => "reply",
    Edit => "edit",
    Create => "create",
    Delete => "delete",
});

impl ChangeClass {
    /// Whether a change of this class is ever sent upstream: `noop` and `personal` changes are
    /// stored and served but never sent.
    pub fn is_sent(self) -> bool {
        !matches!(self, Self::Noop | Self::Personal)
    }
}

/// Where a change is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemChangeState {
    /// Accepted and served; not sent upstream (yet), or waiting for `next_attempt_at`.
    Held,
    /// Being sent upstream by the writer.
    Sending,
    /// Upstream has it: sent by the writer, made upstream too, or sent by the client itself.
    Applied,
    /// A later change of the same item took its place.
    Superseded,
    /// Upstream changed the same fields, or deleted the item; still served until someone
    /// decides.
    Conflict,
    /// Upstream refused it; still served until someone decides.
    Rejected,
    /// Someone discarded it (also "take theirs"); no longer served. The row and its bytes stay.
    Discarded,
}

names!(ItemChangeState {
    Held => "held",
    Sending => "sending",
    Applied => "applied",
    Superseded => "superseded",
    Conflict => "conflict",
    Rejected => "rejected",
    Discarded => "discarded",
});

impl ItemChangeState {
    /// Whether the change is served: held, sending, conflict or rejected.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Self::Held | Self::Sending | Self::Conflict | Self::Rejected
        )
    }
}

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOrigin {
    /// A CalDAV or CardDAV client.
    Dav,
    /// The web app or another API client.
    Api,
    /// The scheduler: an attendee's iTIP `REPLY` merged into the organizer's copy. The only
    /// origin that keeps the item's [`crate::Item::schedule_tag`].
    Itip,
}

names!(ChangeOrigin {
    Dav => "dav",
    Api => "api",
    Itip => "itip",
});

/// What a change is about: an existing item, or an href in a collection. An href names the item
/// whose DAV name ([`crate::Item::dav_name`], else its `remote_id`) it is, and otherwise the
/// changes made under that href before any item existed (a held create).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeTarget {
    /// An item the store knows.
    Item {
        /// The item.
        item: ItemId,
    },
    /// An href in a collection.
    Href {
        /// The collection.
        collection: CollectionId,
        /// The href's last segment, as the client wrote it.
        href: String,
    },
}

/// The exact bytes a client sent, already in the blob store. Never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeContent {
    /// The bytes.
    pub blob_id: BlobId,
    /// Their media type, such as `text/calendar`.
    pub content_type: String,
}

/// What the client saw before it made a change, from [`Effective::state`]. Two equal states
/// serve the same bytes, so a caller maps a client's ETag to the state it computed it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServedState {
    /// The number of the item's latest upstream version; `None` when there is no item.
    pub version: Option<u32>,
    /// The open change served on top of it; `None` when none is.
    pub change: Option<ItemChangeId>,
}

/// The precondition of [`ItemChangeStore::record_change`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// No precondition: the last writer wins.
    Any,
    /// Nothing may be served under the target (`If-None-Match: *`), and no item may ever have
    /// had it as its href.
    Absent,
    /// The target must still be in this state (`If-Match` with the served ETag).
    State(ServedState),
}

/// A change to record.
#[derive(Debug, Clone, PartialEq)]
pub struct NewItemChange {
    /// What it changes.
    pub target: ChangeTarget,
    /// What it does.
    pub op: ChangeOp,
    /// What it amounts to: `create` exactly for a create, `delete` exactly for a delete, one of
    /// `noop`, `personal`, `reply` and `edit` for an update.
    pub class: ChangeClass,
    /// The client's bytes; required for a create or an update, absent for a delete.
    pub content: Option<ChangeContent>,
    /// The patch against the base, as `pim-edit` serializes it; opaque to the store.
    pub patch: Option<serde_json::Value>,
    /// The number of the upstream version the change is based on; required when the target is
    /// an item, absent when there is none.
    pub base_version: Option<u32>,
    /// Who made it.
    pub origin: ChangeOrigin,
    /// The person who made it.
    pub user_id: Option<UserId>,
    /// The device it came from (opaque); `None` for the web app.
    pub device: Option<String>,
    /// When it was made.
    pub at: Timestamp,
    /// The change does not go upstream before this (its `next_attempt_at`); `None` for now.
    pub not_before: Option<Timestamp>,
    /// Outbox messages held for this change: recorded with it in one step, so a restart can
    /// tell which held messages belong to a recorded change
    /// ([`ItemChangeStore::held_change_messages`]).
    pub messages: Vec<OutboxId>,
    /// For an `itip` change: the `Schedule-Tag` the item keeps, pinned in the same step as the
    /// change is recorded, so no other change can slip in between. Ignored for other origins,
    /// which always unpin it.
    pub schedule_tag: Option<String>,
}

/// An incoming iMIP reply that could not be acted on yet and waits for another try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredReply {
    /// The message carrying the reply.
    pub message: MessageId,
    /// Its archive.
    pub archive: ArchiveId,
    /// When it was first deferred.
    pub since: Timestamp,
    /// How many times it was deferred.
    pub attempts: u32,
    /// When it is due again.
    pub next_attempt_at: Timestamp,
}

/// A recorded change. Its payload never changes; only its state and bookkeeping do.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemChange {
    /// Id; later changes have greater ids.
    pub id: ItemChangeId,
    /// The collection.
    pub collection_id: CollectionId,
    /// The collection's archive.
    pub archive_id: ArchiveId,
    /// The item; `None` for changes under an href no item had yet, filled in when the create
    /// reaches upstream.
    pub item_id: Option<ItemId>,
    /// The client's href for a change made before the item existed; `None` otherwise.
    pub href: Option<String>,
    /// What it does.
    pub op: ChangeOp,
    /// What it amounts to.
    pub class: ChangeClass,
    /// The client's bytes.
    pub content: Option<ChangeContent>,
    /// The patch against the base.
    pub patch: Option<serde_json::Value>,
    /// The upstream version it is based on.
    pub base_version: Option<u32>,
    /// That version's upstream ETag, the precondition for sending it.
    pub base_etag: Option<String>,
    /// The open change it took the place of.
    pub supersedes: Option<ItemChangeId>,
    /// Who made it.
    pub origin: ChangeOrigin,
    /// The person.
    pub user_id: Option<UserId>,
    /// The device.
    pub device: Option<String>,
    /// When it was made.
    pub created_at: Timestamp,
    /// Where it is.
    pub state: ItemChangeState,
    /// A short code for why it is in a conflict or was refused, such as `upstream_deleted`,
    /// `reply_obsolete` or `refused`; never server text, names or addresses.
    pub reason: Option<String>,
    /// A newer upstream version landed since the change was last looked at.
    pub upstream_changed: bool,
    /// The item disappeared upstream while the change was open.
    pub upstream_deleted: bool,
    /// The upstream version the patch was last re-applied on, after `base_version`.
    pub rebased_onto: Option<u32>,
    /// How many times it was claimed for sending.
    pub attempts: u32,
    /// When a held change may be sent again after a temporary failure.
    pub next_attempt_at: Option<Timestamp>,
    /// A short code for the last temporary failure, such as `connection`.
    pub last_error: Option<String>,
    /// When its state or bookkeeping last changed.
    pub updated_at: Timestamp,
}

impl ItemChange {
    /// Whether the change needs a look before it can be sent: an upstream version landed or
    /// the item disappeared upstream since it was recorded or last rebased.
    pub fn flagged(&self) -> bool {
        self.upstream_changed || self.upstream_deleted
    }
}

/// What [`ItemChangeStore::record_change`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordedChange {
    /// The change was recorded.
    Recorded(Box<ItemChange>),
    /// The precondition did not hold; nothing was recorded.
    PreconditionFailed {
        /// What is served now.
        current: ServedState,
    },
}

/// What is served under a target: the latest upstream version with the open change on top.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    /// The collection.
    pub collection_id: CollectionId,
    /// The href it is served under: the item's DAV name, else its `remote_id`, else the href of
    /// the held create.
    pub href: String,
    /// The item, when there is one.
    pub item: Option<ItemSummary>,
    /// Its latest upstream version.
    pub latest: Option<ItemVersion>,
    /// The open change served on top of it, when there is one.
    pub change: Option<ItemChange>,
    /// The state to compare against in [`Expect::State`].
    pub state: ServedState,
    /// Whether anything is served: `false` for a deleted item, a held delete, or a create that
    /// was deleted before it reached upstream. An item removed upstream stays served while a
    /// change other than a delete is open.
    pub served: bool,
}

/// How a person or the post-batch step decides an open change that is not being sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The held change still holds on top of upstream version `onto`; clears
    /// `upstream_changed` when `onto` is the latest. Only from `held`.
    Rebased {
        /// The upstream version its patch was re-applied on.
        onto: u32,
    },
    /// Upstream already has it (the same edit made upstream, or a reply the client sent
    /// itself). From `held`, `conflict` or `rejected`.
    Applied,
    /// It clashes with upstream; clears `upstream_changed` and `upstream_deleted`. From `held`
    /// or `rejected`.
    Conflict {
        /// A short code, such as `overlap`, `reply_obsolete` or `upstream_deleted`.
        reason: String,
    },
    /// Discard, or take theirs. From `held`, `conflict` or `rejected`.
    Discard,
    /// Send a refused change again: back to `held` with no attempts. Only from `rejected`.
    Retry,
}

/// What became of a change the writer sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Settle {
    /// Upstream has it. `version` is what upstream now holds, recorded with origin
    /// `writeback`; `None` for a delete, or when the writer could not read it back.
    Applied {
        /// The upstream state after the write.
        version: Option<Box<NewItemVersion>>,
    },
    /// Upstream refused it for good.
    Rejected {
        /// A short code.
        reason: String,
    },
    /// Upstream changed meanwhile (a failed precondition) and the change cannot simply be sent
    /// again.
    Conflict {
        /// A short code.
        reason: String,
    },
    /// A temporary failure: held again until `next_attempt_at`.
    Retry {
        /// When it may go again.
        next_attempt_at: Timestamp,
        /// A short code, such as `connection`.
        error: String,
    },
}

/// Which open changes to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemChangeQuery {
    /// Only this archive's.
    pub archive_id: ArchiveId,
    /// Only this collection's.
    pub collection_id: Option<CollectionId>,
    /// Only these states, all open ones when empty; states that are not open match nothing.
    pub states: Vec<ItemChangeState>,
    /// Only changes with `upstream_changed` or `upstream_deleted`.
    pub flagged_only: bool,
    /// Only changes after this one; pages come in id order, so passing the last id of a page
    /// gives the next page.
    pub after: Option<ItemChangeId>,
    /// At most this many; implementations cap it at 500.
    pub limit: u32,
}

/// Edits to contacts and events from devices and the web app (CLIENTS.md, "Edits from native
/// apps"), the collections' write modes, and what the writer needs to send them upstream.
///
/// Upstream versions stay what sources showed; edits are rows of their own that are never
/// rewritten or deleted (only their state changes). A second edit of the same item supersedes
/// the first. Every recorded change and every state change writes a [`ChangeLogEntry`](crate::ChangeLogEntry) of kind `item` in the
/// same transaction, and bytes referenced by changes count as referenced blobs. The store never
/// parses the bytes: classification, patches and rebasing belong to the caller. It flags an
/// open change when a newer upstream version of its item lands (`upstream_changed`) or the item
/// disappears upstream (`upstream_deleted`); a version recorded by a writeback flags the item's
/// other open changes only, and rebases the changes that superseded the written one onto it
/// instead of flagging them.
#[async_trait]
pub trait ItemChangeStore: Send + Sync {
    /// Sets the collection's write mode and returns it. `Invalid` for a mode other than
    /// `read_only` on a collection no account owns or one of an import account (imported files
    /// stay read-only); `NotFound` for an unknown collection. A different mode moves the old one
    /// to `previous_write_mode` and sets `write_mode_changed_at` to now; the same mode again
    /// changes nothing.
    async fn set_write_mode(
        &self,
        collection: CollectionId,
        mode: WriteMode,
    ) -> Result<Collection, StoreError>;

    /// Records a change atomically against `expect`, superseding the target's open change: a
    /// held, conflicting or refused one becomes `superseded`, one being sent stays sending and
    /// is superseded when it settles unless it is applied. The new change is `held`, except a
    /// delete of a create that never reached upstream (no item, nothing being sent), which is
    /// `applied` at once. The base ETag is taken from the base version.
    ///
    /// A change of an item with any origin but [`ChangeOrigin::Itip`] clears the item's
    /// [`crate::Item::schedule_tag`] in the same transaction.
    ///
    /// The store does not check the write mode, roles or scopes; callers do. `Invalid` when the
    /// class, content or base do not fit the operation, a create names an existing item, an
    /// update or delete names an href with nothing under it, the base version does not exist,
    /// or the collection has no account; `NotFound` for an unknown item or collection.
    async fn record_change(
        &self,
        change: &NewItemChange,
        expect: Expect,
    ) -> Result<RecordedChange, StoreError>;

    /// The change, or `None`.
    async fn get_change(&self, id: ItemChangeId) -> Result<Option<ItemChange>, StoreError>;

    /// What is served under the target, or `None` when the target names no item and no change.
    async fn effective(&self, target: &ChangeTarget) -> Result<Option<Effective>, StoreError>;

    /// Open changes matching the query, oldest first.
    async fn open_changes(&self, query: &ItemChangeQuery) -> Result<Vec<ItemChange>, StoreError>;

    /// Every change of the target in any state, oldest first; empty when it names nothing.
    async fn changes_of(&self, target: &ChangeTarget) -> Result<Vec<ItemChange>, StoreError>;

    /// Decides an open change that is not being sent; see [`Resolution`] for the states each
    /// one starts from. `None` when the change is in another state; `NotFound` for an unknown
    /// id.
    async fn resolve_change(
        &self,
        id: ItemChangeId,
        resolution: &Resolution,
        at: Timestamp,
    ) -> Result<Option<ItemChange>, StoreError>;

    /// `write_through` collections for which [`ItemChangeStore::claim_change`] at `now` would
    /// find a change, ordered by id.
    async fn due_change_collections(&self, now: Timestamp)
    -> Result<Vec<CollectionId>, StoreError>;

    /// Atomically takes the collection's oldest due change and moves it to `sending`, counting
    /// an attempt. Due means: `held`, a class that [`ChangeClass::is_sent`], not flagged, past
    /// its `next_attempt_at`, the head of its supersede chain, and no change of the same item
    /// (or href) being sent. Changes of other items pass a waiting one. `None` when nothing can
    /// go, also for a collection that is not `write_through`. A change with no item is sent as
    /// a create, whatever its operation.
    async fn claim_change(
        &self,
        collection: CollectionId,
        now: Timestamp,
    ) -> Result<Option<ItemChange>, StoreError>;

    /// Records what became of a `sending` change. `Applied` with a version records it as the
    /// item's newest version with origin `writeback` (creating the item with the change's href
    /// as its DAV name when the change had none, and filling in the item on every change under
    /// that href); a version with the same bytes as the latest adds none. A new version sets
    /// `upstream_changed` on the item's other open changes, except the changes whose supersede
    /// chain leads back to this one (the user's own later edits): their `rebased_onto` becomes
    /// the new version. `Applied` without a version for a delete marks the item removed.
    /// `Conflict` and `Rejected` clear the change's `upstream_changed` and `upstream_deleted`.
    /// Any other outcome of a change a later one superseded makes it `superseded`. `None` when it is not sending; `Invalid` when the
    /// version is for another collection, or for another item than the change's; `NotFound`
    /// for an unknown id.
    async fn settle_change(
        &self,
        id: ItemChangeId,
        outcome: &Settle,
        at: Timestamp,
    ) -> Result<Option<ItemChange>, StoreError>;

    /// Held (`draft`) outbox messages of recorded changes, oldest first: what a start after the
    /// process stopped between recording a change and releasing its messages must release.
    async fn held_change_messages(&self) -> Result<Vec<OutboxId>, StoreError>;

    /// Queues the reply in `message` for another try at `next_attempt_at`. Idempotent per
    /// message: deferring it again counts an attempt, moves its time and keeps `since`.
    async fn defer_reply(
        &self,
        archive: ArchiveId,
        message: MessageId,
        at: Timestamp,
        next_attempt_at: Timestamp,
    ) -> Result<DeferredReply, StoreError>;

    /// Deferred replies due at `now`, earliest first, at most `limit`.
    async fn due_replies(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<DeferredReply>, StoreError>;

    /// Stops trying the reply in `message`. Idempotent.
    async fn forget_reply(&self, message: MessageId) -> Result<(), StoreError>;

    /// Puts every `sending` change back to `held`, for a start after the process stopped
    /// mid-send, and returns them oldest first.
    async fn release_changes(&self, at: Timestamp) -> Result<Vec<ItemChange>, StoreError>;
}
