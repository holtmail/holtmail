//! Contract for full-text search over archived messages.
//!
//! The index is derived data: it can be dropped and rebuilt from the store and the blobs at any
//! time. Searches always run inside one archive.

#[cfg(feature = "testing")]
pub mod testing;

use async_trait::async_trait;
use holtmail_core::{Address, ArchiveId, MessageId, ThreadId, Timestamp};
use serde::{Deserialize, Serialize};

/// What the index knows about one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDocument {
    /// The message.
    pub message_id: MessageId,
    /// Its archive.
    pub archive_id: ArchiveId,
    /// Its thread.
    pub thread_id: ThreadId,
    /// Subject.
    pub subject: String,
    /// `From`.
    pub from: Vec<Address>,
    /// `To`, `Cc` and `Bcc` together.
    pub recipients: Vec<Address>,
    /// Plain text of the body.
    pub body: String,
    /// File names of attachments.
    pub attachment_names: Vec<String>,
    /// Text extracted from attachments, when available.
    pub attachment_text: String,
    /// Paths of the folders the message is in, and their roles (`inbox`, `sent`, ...).
    pub folders: Vec<String>,
    /// Flags from the source, e.g. `\Seen`, `\Flagged`.
    pub flags: Vec<String>,
    /// The date lists sort by.
    pub date: Timestamp,
    /// Whether the message has attachments that are not inline.
    pub has_attachments: bool,
}

/// One condition of a search. A message matches a [`SearchQuery`] when it matches every clause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Clause {
    /// Words anywhere: subject, addresses, body, attachment names and text. Every word must match.
    Text(String),
    /// These words next to each other, in order, anywhere.
    Phrase(String),
    /// Words in the sender's name or address.
    From(String),
    /// Words in a recipient's name or address.
    To(String),
    /// Words in the subject.
    Subject(String),
    /// Words in the body.
    Body(String),
    /// Words in an attachment's file name.
    Filename(String),
    /// Has an attachment that is not inline.
    HasAttachment,
    /// In a folder with this path or role, case-insensitive.
    In(String),
    /// Has this flag, e.g. `\Seen`.
    Flag(String),
    /// Dated before this time.
    Before(Timestamp),
    /// Dated at or after this time.
    After(Timestamp),
    /// The inner clause must not match.
    Not(Box<Clause>),
}

/// A search: clauses that must all match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Clauses; an empty list matches every message.
    pub clauses: Vec<Clause>,
}

/// How to order results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Newest first.
    #[default]
    Newest,
    /// Best match first.
    Relevance,
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResults {
    /// Matching messages in the requested order.
    pub hits: Vec<MessageId>,
    /// How many messages match in total.
    pub total: u64,
}

/// Why an index operation failed.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The query cannot run, e.g. it is too large.
    #[error("invalid query: {0}")]
    Invalid(String),
    /// The backend failed; the operation may succeed if retried.
    #[error("index backend failed: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A full-text index of messages.
#[async_trait]
pub trait SearchIndex: Send + Sync {
    /// Adds documents, replacing any with the same `message_id`. Visible to searches after
    /// [`SearchIndex::commit`].
    async fn put(&self, documents: Vec<IndexDocument>) -> Result<(), IndexError>;

    /// Removes the messages. Missing ones are ignored. Visible after [`SearchIndex::commit`].
    async fn remove(&self, ids: &[MessageId]) -> Result<(), IndexError>;

    /// Makes every earlier `put` and `remove` visible and durable.
    async fn commit(&self) -> Result<(), IndexError>;

    /// Messages of `archive` that match `query`, skipping `offset` and returning at most `limit`.
    ///
    /// Words match whole words after case folding and accent removal, so `Zoë` finds `zoe`.
    async fn search(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        sort: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<SearchResults, IndexError>;
}
