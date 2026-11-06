use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, MessageId, Timestamp};

use crate::StoreError;

/// How computing the insights of a message ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InsightOutcome {
    /// Every extractor ran.
    Ok,
    /// An extractor failed; the items of the others are kept.
    Partial,
    /// The stored bytes no longer parse. Not listed as needing insights again until the
    /// fingerprint changes.
    Unparsable,
    /// The blob is missing or unreadable. Not listed as needing insights again until the
    /// fingerprint changes.
    Unreadable,
}

impl InsightOutcome {
    /// The stable lowercase name: `ok`, `partial`, `unparsable` or `unreadable`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Unparsable => "unparsable",
            Self::Unreadable => "unreadable",
        }
    }

    /// The outcome named by [`InsightOutcome::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "ok" => Self::Ok,
            "partial" => Self::Partial,
            "unparsable" => Self::Unparsable,
            "unreadable" => Self::Unreadable,
            _ => return None,
        })
    }
}

/// The stored insights of one message. Derived from its raw bytes and replaced whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsightRow {
    /// The message.
    pub message: MessageId,
    /// Its archive.
    pub archive: ArchiveId,
    /// The extraction fingerprint the row was computed with.
    pub fingerprint: String,
    /// How computing ended.
    pub outcome: InsightOutcome,
    /// Some input hit a size cap.
    pub truncated: bool,
    /// The sender address, lowercase with an A-label domain.
    pub from_address: Option<String>,
    /// Header signal bits; all 64 bits are kept.
    pub signals: u64,
    /// Bits of the kinds found; all 64 bits are kept.
    pub kinds: u64,
    /// Authentication facts as JSON, opaque to the store and returned byte for byte.
    pub auth: String,
    /// The items as JSON, opaque to the store and returned byte for byte.
    pub items: String,
    /// When it was computed.
    pub computed_at: Timestamp,
}

/// Everything known about the insights of one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsightView {
    /// The stored insights row.
    pub insights: InsightRow,
}

/// What a message list shows about the insights of a message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InsightSummary {
    /// Bits of the kinds found.
    pub kinds: u64,
}

/// Insights derived from messages: one row per message, replaced whole, rebuilt whenever the
/// extraction fingerprint changes.
///
/// Deleting a message or an archive removes its rows.
#[async_trait]
pub trait InsightStore: Send + Sync {
    /// Stores `row` as the insights of `message`, replacing any earlier row whole. Idempotent.
    /// `NotFound` for an unknown message; `Invalid` when `row.message` is not `message` or
    /// `row.archive` is not its archive.
    async fn set_message_insights(
        &self,
        message: MessageId,
        row: &InsightRow,
    ) -> Result<(), StoreError>;

    /// The insights of `message`, or `None` when none were stored or the message is unknown.
    async fn insight_view(&self, message: MessageId) -> Result<Option<InsightView>, StoreError>;

    /// The insights of every message among `ids` that has them; others are left out. Any number
    /// of ids.
    async fn insight_views_among(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, InsightView>, StoreError>;

    /// The list summary of every message among `ids` that has insights; others are left out. Any
    /// number of ids.
    async fn insights_among(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, InsightSummary>, StoreError>;

    /// Up to `limit` messages of `archive` that need insights computed, ascending by id and
    /// starting after `after`: those without a row and those whose row has a fingerprint other
    /// than `fingerprint`, whatever its outcome. A row with `fingerprint` is never listed, so
    /// unreadable and unparsable messages are not retried until the fingerprint changes. Paging
    /// with the last id of each page visits every such message once while nothing is written.
    /// `limit` is capped at 10,000.
    async fn messages_needing_insights(
        &self,
        archive: ArchiveId,
        fingerprint: &str,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError>;

    /// How many messages [`InsightStore::messages_needing_insights`] would list for `archive`
    /// and `fingerprint` from the start; 0 for an unknown archive.
    async fn count_needing_insights(
        &self,
        archive: ArchiveId,
        fingerprint: &str,
    ) -> Result<u64, StoreError>;
}
