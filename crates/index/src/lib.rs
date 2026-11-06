//! Contract for full-text search over archived messages, and over contacts and calendar events
//! as documents of their own.
//!
//! The index is derived data: it can be dropped and rebuilt from the store and the blobs at any
//! time. Searches always run inside one archive. Messages and items (contacts and events) live
//! side by side: [`SearchIndex::search`] only ever finds messages and
//! [`SearchIndex::search_items`] only items, with the same [`Clause`]s.

#[cfg(feature = "testing")]
pub mod testing;

use async_trait::async_trait;
use holtmail_core::{Address, ArchiveId, ItemId, MessageId, ThreadId, Timestamp};
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
    /// Labels, e.g. `Inbox`, `Projects/2025`.
    pub labels: Vec<String>,
    /// The date lists sort by.
    pub date: Timestamp,
    /// Whether the message has attachments that are not inline.
    pub has_attachments: bool,
}

/// What a document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// A message.
    Mail,
    /// A calendar event.
    Event,
    /// A contact card.
    Contact,
}

/// What the index knows about one contact or event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDocument {
    /// The item.
    pub item_id: ItemId,
    /// Its archive.
    pub archive_id: ArchiveId,
    /// [`DocumentKind::Event`] or [`DocumentKind::Contact`].
    pub kind: DocumentKind,
    /// An event's summary or a contact's name; searched like a subject.
    pub title: String,
    /// An event's organizer; searched like a sender.
    pub organizer: Vec<Address>,
    /// An event's attendees, or a contact's own names and addresses; searched like recipients.
    pub people: Vec<Address>,
    /// Everything else worth finding: description and location, or organization, title, phone
    /// numbers and notes; searched like a body.
    pub text: String,
    /// An event's start; `None` for contacts.
    pub date: Option<Timestamp>,
}

/// One condition of a search. A message matches a [`SearchQuery`] when it matches every clause.
///
/// Contacts and events are searched with the same clauses: their title as the subject, an event's
/// organizer as the sender, its attendees (or a contact's own names and addresses) as the
/// recipients, their other text as the body and an event's start as the date. Clauses about
/// folders, flags, labels and attachments never match them.
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
    /// Words in text extracted from attachments. Every word must match.
    Attachment(String),
    /// Has an attachment that is not inline.
    HasAttachment,
    /// In a folder with this path or role, case-insensitive.
    In(String),
    /// Has this flag, e.g. `\Seen`.
    Flag(String),
    /// Carries this label, compared whole and case-insensitive: `projects` does not find
    /// `Projects/2025`.
    Label(String),
    /// Dated before this time.
    Before(Timestamp),
    /// Dated at or after this time.
    After(Timestamp),
    /// Is a document of this kind: [`DocumentKind::Mail`] matches every message and no item.
    Kind(DocumentKind),
    /// Words in the name or address of anyone involved: a message's sender or recipients, an
    /// event's organizer or attendees, a contact's own names and addresses.
    With(String),
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
    /// The hits that match only through text extracted from their attachments: without
    /// [`IndexDocument::attachment_text`] they would not match. A subset of `hits`, in the same
    /// order; empty when the query has no [`Clause::Text`] or [`Clause::Phrase`].
    pub attachment_hits: Vec<MessageId>,
}

/// A contact or event found by [`SearchIndex::search_items`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemHit {
    /// The item.
    pub item_id: ItemId,
    /// [`DocumentKind::Event`] or [`DocumentKind::Contact`].
    pub kind: DocumentKind,
}

/// One page of contacts and events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemResults {
    /// Matching items in the requested order.
    pub hits: Vec<ItemHit>,
    /// How many items match in total.
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

    /// Adds contact and event documents, replacing any with the same `item_id`. Visible to
    /// searches after [`SearchIndex::commit`].
    async fn put_items(&self, documents: Vec<ItemDocument>) -> Result<(), IndexError>;

    /// Removes the items' documents. Missing ones are ignored. Visible after
    /// [`SearchIndex::commit`].
    async fn remove_items(&self, ids: &[ItemId]) -> Result<(), IndexError>;

    /// Makes every earlier `put`, `put_items`, `remove` and `remove_items` visible and durable.
    async fn commit(&self) -> Result<(), IndexError>;

    /// Messages of `archive` that match `query`, skipping `offset` and returning at most `limit`.
    ///
    /// Words match after case folding and accent removal, so `Zoë` finds `zoe`. Outside phrases,
    /// a word of three letters or more also finds words that start with it, so `quart` finds
    /// `quarterly`; shorter words match whole words only.
    async fn search(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        sort: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<SearchResults, IndexError>;

    /// Contacts and events of `archive` that match `query`, skipping `offset` and returning at
    /// most `limit`. Words match as in [`SearchIndex::search`]. [`SortOrder::Newest`] lists
    /// events by start, the latest first, then contacts; [`SortOrder::Relevance`] the best match
    /// first.
    async fn search_items(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        sort: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<ItemResults, IndexError>;
}
