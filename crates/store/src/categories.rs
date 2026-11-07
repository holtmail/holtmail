use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use holtmail_category::{CategorySource, MessageKind, Topic, TopicSet, TrustState};
use holtmail_core::{AccountId, ArchiveId, InvalidId, MessageId, Timestamp, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{MessageAlias, StoreError};

/// A category override made by hand in an archive.
///
/// A UUIDv7: ids created later sort after ids created earlier. Displays, parses and serializes as
/// the hyphenated lowercase form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryOverrideId(Uuid);

impl CategoryOverrideId {
    /// A new id, unique and later than every id this process created before.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// The id as a UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for CategoryOverrideId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl fmt::Display for CategoryOverrideId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for CategoryOverrideId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CategoryOverrideId({})", self.0.hyphenated())
    }
}

impl FromStr for CategoryOverrideId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s).map(Self).map_err(|_| InvalidId)
    }
}

/// What a category override matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideScope {
    /// Messages whose sender address equals the key.
    Address,
    /// Messages whose sender's registrable domain equals the key.
    Domain,
}

impl OverrideScope {
    /// The stable lowercase name: `address` or `domain`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Domain => "domain",
        }
    }

    /// The scope named by [`OverrideScope::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "address" => Self::Address,
            "domain" => Self::Domain,
            _ => return None,
        })
    }
}

/// A user's choice of kind, topics or both for every message of a sender. User state: rebuilding
/// derived data never touches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryOverride {
    /// Its id.
    pub id: CategoryOverrideId,
    /// The archive it applies in.
    pub archive_id: ArchiveId,
    /// What it matches.
    pub scope: OverrideScope,
    /// The address or registrable domain, normalised by the caller: lowercase, A-label domain, no
    /// trailing dot. The store compares it exactly and never normalises it.
    pub key: String,
    /// The kind it sets; `None` keeps the derived kind.
    pub kind: Option<MessageKind>,
    /// The topics it sets, replacing the derived set; `None` keeps the derived topics. Every bit
    /// is kept, unknown ones included.
    pub topics: Option<TopicSet>,
    /// When it was first made. Kept when the override is replaced.
    pub created_at: Timestamp,
    /// Who first made it. Kept when the override is replaced.
    pub created_by: UserId,
}

/// Settings of the archive's categories that gate overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CategoryPolicy {
    /// Whether `health` may be applied. When `false`, the health topic of an override is kept
    /// stored but not applied.
    pub health: bool,
}

/// The stage-B result for one message, as the runner derived it before any override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryWrite {
    /// The message.
    pub message: MessageId,
    /// Its archive.
    pub archive: ArchiveId,
    /// The derive fingerprint it was computed with.
    pub derive_fp: String,
    /// The trust state of its sender.
    pub trust: TrustState,
    /// The trusted authserv-id whose field decided the trust, if any.
    pub trust_authserv: Option<String>,
    /// One-click unsubscribe is covered by an aligned signature.
    pub one_click_ok: bool,
    /// The From is one of the archive's own addresses.
    pub own: bool,
    /// The sender address that overrides match, normalised.
    pub sender_address: Option<String>,
    /// The sender's registrable domain that overrides match, normalised.
    pub sender_domain: Option<String>,
    /// The kind before overrides.
    pub derived_kind: MessageKind,
    /// The layer the derived kind comes from.
    pub derived_kind_source: CategorySource,
    /// The topics before overrides, trust gate and health flag already applied. Every bit is
    /// kept.
    pub derived_topics: TopicSet,
    /// The layer each derived topic comes from.
    pub derived_sources: BTreeMap<Topic, CategorySource>,
}

/// The stored stage-B row of one message: what was derived and what applies after overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRow {
    /// What was written. `trust` reads back with the stored domain or provider.
    pub write: CategoryWrite,
    /// The effective kind.
    pub kind: MessageKind,
    /// The effective topics: known topics only.
    pub topics: TopicSet,
    /// The first effective topic in taxonomy order other than `health`.
    pub list_topic: Option<Topic>,
    /// The override that set the kind, if one did.
    pub kind_override: Option<CategoryOverrideId>,
    /// The override that set the topics, if one did.
    pub topics_override: Option<CategoryOverrideId>,
}

/// The stage-B row of a message with the overrides that apply to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryView {
    /// The row.
    pub row: CategoryRow,
    /// The override named by `row.kind_override`.
    pub kind_override: Option<CategoryOverride>,
    /// The override named by `row.topics_override`.
    pub topics_override: Option<CategoryOverride>,
}

/// What stage B reads about one message: its stored stage-A columns, its alias row and where it
/// was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeriveRow {
    /// The message.
    pub message: MessageId,
    /// Its archive.
    pub archive: ArchiveId,
    /// The stored From address of its insights row.
    pub from_address: Option<String>,
    /// The stored header signal bits.
    pub signals: u64,
    /// The stored kind bits.
    pub kinds: u64,
    /// The stored auth facts JSON, byte for byte.
    pub auth: String,
    /// The stored items JSON, byte for byte.
    pub items: String,
    /// What alias detection found in it.
    pub alias: Option<MessageAlias>,
    /// The distinct accounts owning a folder it is located in, removed or not, sorted.
    pub accounts: Vec<AccountId>,
}

/// How many messages of an archive have each effective kind and topic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryCounts {
    /// Every kind in [`MessageKind::ALL`] order, zeros included.
    pub kinds: Vec<(MessageKind, u64)>,
    /// Every topic in [`Topic::ALL`] order, zeros included; `health` only when asked for.
    pub topics: Vec<(Topic, u64)>,
}

/// A category to narrow a message list to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CategoryFilter {
    /// Messages with this effective kind.
    Kind(MessageKind),
    /// Messages with this effective topic.
    Topic(Topic),
}

/// Stage-B categories of messages, the overrides users make and the trust lists of accounts.
///
/// Rows of messages are derived and rebuilt whenever `derive_fp` changes; overrides and trust
/// lists are user state. Category changes queue their messages for reindexing until the caller
/// dequeues them. Deleting a message removes its rows and queue entry; deleting an archive removes
/// its rows, queue, overrides and trust lists; deleting an account removes its trust list.
#[async_trait]
pub trait CategoryStore: Send + Sync {
    /// Up to `limit` messages of `archive` whose insights row has outcome ok or partial and that
    /// have no categories row or one with a derive fingerprint other than `derive_fp`, ascending
    /// by id and starting after `after`. Paging with the last id of each page visits every such
    /// message once while nothing is written. `limit` is capped at 10,000.
    async fn derive_inputs(
        &self,
        archive: ArchiveId,
        derive_fp: &str,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<DeriveRow>, StoreError>;

    /// The stage-B input of `message` whatever its categories row holds, or `None` when it has no
    /// insights row with outcome ok or partial.
    async fn derive_input(&self, message: MessageId) -> Result<Option<DeriveRow>, StoreError>;

    /// How many messages [`CategoryStore::derive_inputs`] would list for `archive` and
    /// `derive_fp` from the start; 0 for an unknown archive.
    async fn count_needing_derive(
        &self,
        archive: ArchiveId,
        derive_fp: &str,
    ) -> Result<u64, StoreError>;

    /// Removes the categories rows and effective topics of `ids`, so stage B lists them again.
    /// Unknown ids and ids without rows are ignored. Idempotent.
    async fn invalidate_derived(&self, ids: &[MessageId]) -> Result<(), StoreError>;

    /// Stores each write as the categories row of its message, replacing any earlier row, with
    /// the effective values computed under `policy`: per axis, the address override that sets
    /// it, else the domain override that sets it, else the derived value. Override topics pass
    /// through [`TopicSet::gated`] with the row's trust; derived topics are stored as given.
    ///
    /// Writes are committed in transactions of up to 500 rows, in order. A write for a message
    /// that no longer exists is skipped. `Invalid` when a write names an archive other than its
    /// message's. Returns the ids whose effective kind or topics changed, a new row counting as
    /// changed, in the order of `writes`.
    async fn set_message_categories(
        &self,
        writes: &[CategoryWrite],
        policy: CategoryPolicy,
    ) -> Result<Vec<MessageId>, StoreError>;

    /// Exactly [`CategoryStore::set_message_categories`], and in the same transaction as each
    /// batch the ids it returns for that batch are queued for reindexing as by
    /// [`CategoryStore::queue_reindex`]. A changed category is never committed without its
    /// queued reindex.
    async fn set_message_categories_queued(
        &self,
        writes: &[CategoryWrite],
        policy: CategoryPolicy,
    ) -> Result<Vec<MessageId>, StoreError>;

    /// Stores the override, replacing the one with the same archive, scope and key while keeping
    /// its id and `created_at` and `created_by`, and recomputes the effective values of the rows
    /// whose sender address (address scope) or sender domain (domain scope) equals the key, in
    /// one transaction. The changed ids are queued for reindexing as by
    /// [`CategoryStore::queue_reindex`] in that transaction. Returns the override as stored and
    /// the ids whose effective kind or topics changed, ascending. `Invalid` when it sets neither
    /// kind nor topics or the key is empty; `NotFound` for an unknown archive.
    async fn put_category_override(
        &self,
        category_override: &CategoryOverride,
        policy: CategoryPolicy,
    ) -> Result<(CategoryOverride, Vec<MessageId>), StoreError>;

    /// Removes the override and recomputes the rows it matched: the derived values come back,
    /// then any remaining override applies per axis, in one transaction that also queues the
    /// changed ids for reindexing as by [`CategoryStore::queue_reindex`]. Returns the ids whose
    /// effective kind or topics changed, ascending. `NotFound` for an unknown id or one of
    /// another archive.
    async fn delete_category_override(
        &self,
        archive: ArchiveId,
        id: CategoryOverrideId,
        policy: CategoryPolicy,
    ) -> Result<Vec<MessageId>, StoreError>;

    /// The overrides of `archive`, ordered by scope (address first), then key.
    async fn category_overrides(
        &self,
        archive: ArchiveId,
    ) -> Result<Vec<CategoryOverride>, StoreError>;

    /// How many messages of `archive` have each effective kind and topic. Exact as soon as a
    /// write or override edit returns. `health` is left out unless `include_health`.
    async fn category_counts(
        &self,
        archive: ArchiveId,
        include_health: bool,
    ) -> Result<CategoryCounts, StoreError>;

    /// The trusted authserv-ids of every account of `archive`, empty for an account without
    /// any; an empty map for an unknown archive.
    async fn trusted_authserv_ids(
        &self,
        archive: ArchiveId,
    ) -> Result<BTreeMap<AccountId, Vec<String>>, StoreError>;

    /// Replaces the account's trusted authserv-ids with `ids`, kept exactly as given and in
    /// order. Idempotent. `NotFound` for an unknown account.
    async fn set_trusted_authserv_ids(
        &self,
        account: AccountId,
        ids: &[String],
    ) -> Result<(), StoreError>;

    /// The auth facts JSON of up to `limit` (capped at 1,000) messages located in a folder of
    /// `account`, removed or not, that have an insights row: the most recently archived first,
    /// by archive order (never the sent date). Empty for an unknown account.
    async fn recent_auth_facts(
        &self,
        account: AccountId,
        limit: u32,
    ) -> Result<Vec<String>, StoreError>;

    /// Queues `ids` for reindexing in the archive of each message. A message not yet queued gets
    /// sequence 1; one already queued keeps its row with the sequence raised, so a reindex that
    /// read the earlier sequence does not remove it. Unknown ids are ignored; a repeated id
    /// counts once. Committed in transactions of up to 500 ids, in order.
    async fn queue_reindex(&self, ids: &[MessageId]) -> Result<(), StoreError>;

    /// Up to `limit` (capped at 10,000) queued messages of `archive` with their current
    /// sequence, ascending by id and strictly after `after`. Paging with the last id of each page
    /// visits every queued message once while nothing is written. Empty for an unknown archive.
    async fn reindex_queue(
        &self,
        archive: ArchiveId,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<(MessageId, u64)>, StoreError>;

    /// Removes each queued message whose sequence still equals the one given. A message queued
    /// again since then (a higher sequence) stays queued; unknown ids and mismatched sequences
    /// are ignored. Idempotent.
    async fn dequeue_reindex(&self, done: &[(MessageId, u64)]) -> Result<(), StoreError>;

    /// How many messages of `archive` are queued for reindexing; 0 for an unknown archive.
    async fn count_reindex_queue(&self, archive: ArchiveId) -> Result<u64, StoreError>;
}
