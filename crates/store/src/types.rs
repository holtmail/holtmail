use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, Address, ArchiveId, FolderId, MessageId, ThreadId, Timestamp, UserId,
};
use serde::{Deserialize, Serialize};

/// A named collection of accounts; the unit people are given access to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archive {
    /// Id.
    pub id: ArchiveId,
    /// Name shown to people.
    pub name: String,
    /// When it was created.
    pub created_at: Timestamp,
    /// Whether remote images linked from its messages are downloaded and kept with the archive.
    pub save_remote_images: bool,
    /// When saving was turned on; set exactly while `save_remote_images` is. Messages archived
    /// earlier are processed only on request.
    pub save_remote_images_since: Option<Timestamp>,
}

/// What kind of source an account reads mail from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// An IMAP server.
    Imap,
    /// Microsoft Graph.
    Graph,
    /// A JMAP server.
    Jmap,
    /// No server: messages only arrive through imports.
    Import,
}

impl AccountKind {
    /// The lowercase name used in storage and the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Imap => "imap",
            Self::Graph => "graph",
            Self::Jmap => "jmap",
            Self::Import => "import",
        }
    }

    /// Parses [`AccountKind::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "imap" => Self::Imap,
            "graph" => Self::Graph,
            "jmap" => Self::Jmap,
            "import" => Self::Import,
            _ => return None,
        })
    }
}

/// A mail account archived into an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Id.
    pub id: AccountId,
    /// The archive the account's mail goes to.
    pub archive_id: ArchiveId,
    /// Where the mail comes from.
    pub kind: AccountKind,
    /// Name shown to people.
    pub name: String,
    /// The mailbox address, when known.
    pub address: Option<String>,
    /// Source settings that are not secret (host, port, folder rules), owned by the source module.
    pub settings: serde_json::Value,
    /// Credentials sealed by `holtmail-secret`; opaque to the store.
    #[serde(skip)]
    pub sealed_secret: Option<Vec<u8>>,
    /// Whether scheduled sync runs for this account.
    pub enabled: bool,
    /// When it was created.
    pub created_at: Timestamp,
}

/// Fields of a new account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    /// The archive the account's mail goes to.
    pub archive_id: ArchiveId,
    /// Where the mail comes from.
    pub kind: AccountKind,
    /// Name shown to people.
    pub name: String,
    /// The mailbox address, when known.
    pub address: Option<String>,
    /// Source settings that are not secret.
    pub settings: serde_json::Value,
    /// Credentials sealed by `holtmail-secret`.
    pub sealed_secret: Option<Vec<u8>>,
    /// Whether scheduled sync runs for this account.
    pub enabled: bool,
}

/// What a folder is for, from IMAP special-use attributes or the provider's well-known names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderRole {
    /// Incoming mail.
    Inbox,
    /// Sent mail.
    Sent,
    /// Drafts.
    Drafts,
    /// Deleted mail.
    Trash,
    /// Spam.
    Junk,
    /// Archived mail.
    Archive,
    /// Every message of the account, like Gmail's All Mail.
    All,
    /// Starred or flagged mail.
    Flagged,
    /// Gmail's Important.
    Important,
}

impl FolderRole {
    /// The lowercase name used in storage and the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
            Self::All => "all",
            Self::Flagged => "flagged",
            Self::Important => "important",
        }
    }

    /// The role a folder name usually means, in the languages and clients seen most often.
    /// `path` may use `/` or `.` between levels; only the last level counts.
    pub fn guess(path: &str) -> Option<Self> {
        let name = path
            .rsplit(['/', '.'])
            .next()
            .unwrap_or(path)
            .trim()
            .to_lowercase();
        let full = path.to_lowercase();
        Some(match name.as_str() {
            "inbox" | "входящие" | "posteingang" | "boîte de réception" => Self::Inbox,
            "sent"
            | "sent items"
            | "sent mail"
            | "sent messages"
            | "отправленные"
            | "gesendet"
            | "envoyés" => Self::Sent,
            "drafts" | "draft" | "черновики" | "entwürfe" | "brouillons" => Self::Drafts,
            "trash" | "deleted items" | "deleted messages" | "bin" | "корзина" | "удаленные"
            | "удалённые" | "papierkorb" | "corbeille" => Self::Trash,
            "junk" | "spam" | "junk e-mail" | "junk email" | "спам" | "нежелательная почта" => {
                Self::Junk
            }
            "archive" | "archives" | "архив" | "archiv" => Self::Archive,
            "all mail" | "вся почта" if full.contains("gmail") || name == "all mail" => {
                Self::All
            }
            "starred" | "flagged" | "помеченные" => Self::Flagged,
            "important" | "важное" => Self::Important,
            _ => return None,
        })
    }

    /// Parses [`FolderRole::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "inbox" => Self::Inbox,
            "sent" => Self::Sent,
            "drafts" => Self::Drafts,
            "trash" => Self::Trash,
            "junk" => Self::Junk,
            "archive" => Self::Archive,
            "all" => Self::All,
            "flagged" => Self::Flagged,
            "important" => Self::Important,
            _ => return None,
        })
    }
}

/// A folder of an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Id.
    pub id: FolderId,
    /// The account it belongs to.
    pub account_id: AccountId,
    /// The full path on the server with `/` between levels, e.g. `Projects/2025`.
    pub path: String,
    /// What the folder is for, when known.
    pub role: Option<FolderRole>,
    /// When it was first seen.
    pub created_at: Timestamp,
}

/// A folder with how many archived messages are in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSummary {
    /// The folder.
    #[serde(flatten)]
    pub folder: Folder,
    /// Messages located in the folder and not removed from it.
    pub messages: u64,
    /// Of those, the ones whose flags in this folder lack `\Seen` (compared ignoring case).
    pub unread: u64,
}

/// Where a folder's sync stopped, saved after each batch so a sync can resume.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCursor {
    /// IMAP `UIDVALIDITY` the UIDs below belong to.
    pub uid_validity: Option<u64>,
    /// The highest UID archived.
    pub last_uid: Option<u64>,
    /// IMAP `HIGHESTMODSEQ` up to which flag changes were taken in.
    pub highest_modseq: Option<u64>,
    /// When the flags of every archived message in the folder were last read again, for servers
    /// without mod-sequences.
    pub flags_checked_at: Option<Timestamp>,
    /// An opaque provider state: a JMAP state string or a Graph delta link.
    pub state: Option<String>,
    /// When the folder last finished syncing.
    pub synced_at: Option<Timestamp>,
}

/// A message that could not be archived, kept so the sync can move on and retry it later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailure {
    /// The folder.
    pub folder_id: FolderId,
    /// The message's UID in the folder.
    pub uid: u64,
    /// Why it failed, without message content.
    pub reason: String,
    /// How many times it failed.
    pub attempts: u32,
    /// When it last failed.
    pub failed_at: Timestamp,
}

/// A part of a message stored as its own blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Position among the message's attachments, from 0.
    pub ordinal: u32,
    /// The decoded content.
    pub blob_id: BlobId,
    /// File name.
    pub filename: Option<String>,
    /// Lowercase media type.
    pub content_type: String,
    /// Size of the decoded content in bytes.
    pub size: u64,
    /// `Content-ID` without angle brackets.
    pub content_id: Option<String>,
    /// Shown inside the body rather than listed as a file.
    pub inline: bool,
}

/// A message to add to an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    /// The archive.
    pub archive_id: ArchiveId,
    /// The raw message bytes.
    pub blob_id: BlobId,
    /// Size of the raw message in bytes.
    pub size: u64,
    /// `Message-ID` without angle brackets.
    pub rfc_message_id: Option<String>,
    /// `In-Reply-To` and `References` ids, used for threading.
    pub parent_ids: Vec<String>,
    /// Subject.
    pub subject: Option<String>,
    /// `From`.
    pub from: Vec<Address>,
    /// `To`.
    pub to: Vec<Address>,
    /// `Cc`.
    pub cc: Vec<Address>,
    /// `Bcc`.
    pub bcc: Vec<Address>,
    /// `Date` header.
    pub sent_at: Option<Timestamp>,
    /// When the server received it (IMAP `INTERNALDATE`), when known.
    pub received_at: Option<Timestamp>,
    /// One line from the start of the body.
    pub preview: String,
    /// Attachments, already stored as blobs.
    pub attachments: Vec<Attachment>,
}

/// An archived message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Id.
    pub id: MessageId,
    /// The archive.
    pub archive_id: ArchiveId,
    /// The conversation.
    pub thread_id: ThreadId,
    /// The raw message bytes.
    pub blob_id: BlobId,
    /// Size of the raw message in bytes.
    pub size: u64,
    /// `Message-ID` without angle brackets.
    pub rfc_message_id: Option<String>,
    /// Subject.
    pub subject: Option<String>,
    /// `From`.
    pub from: Vec<Address>,
    /// `To`.
    pub to: Vec<Address>,
    /// `Cc`.
    pub cc: Vec<Address>,
    /// `Bcc`.
    pub bcc: Vec<Address>,
    /// `Date` header.
    pub sent_at: Option<Timestamp>,
    /// When the server received it.
    pub received_at: Option<Timestamp>,
    /// When Holtmail archived it.
    pub archived_at: Timestamp,
    /// The time lists sort by: `sent_at`, else `received_at`, else `archived_at`.
    pub date: Timestamp,
    /// One line from the start of the body.
    pub preview: String,
    /// Number of attachments that are not inline.
    pub attachment_count: u32,
}

/// What [`crate::MessageStore::insert_message`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inserted {
    /// The message, new or existing.
    pub message_id: MessageId,
    /// Its thread.
    pub thread_id: ThreadId,
    /// `false` when the archive already had a message with these bytes.
    pub created: bool,
}

/// Where a message was seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// The folder.
    pub folder_id: FolderId,
    /// UID in the folder under the folder's current `UIDVALIDITY`, when the source has UIDs.
    pub uid: Option<u64>,
    /// Flags and keywords as the source reported them, e.g. `\Seen`, `\Flagged`, `$label1`.
    pub flags: Vec<String>,
    /// When it was first seen in the folder.
    pub first_seen_at: Timestamp,
    /// When it was last seen in the folder.
    pub last_seen_at: Timestamp,
    /// When it was found missing from the folder on the server. The archive keeps the message.
    pub removed_at: Option<Timestamp>,
}

/// What a message list shows about a message besides the message itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageExtras {
    /// Flags of the message, sorted and without duplicates: those of its location in the folder
    /// asked for, or the union over every location not removed.
    pub flags: Vec<String>,
    /// Messages in its thread, itself included.
    pub thread_count: u32,
    /// Folders it is located in and not removed from, ordered by id.
    pub folder_ids: Vec<FolderId>,
    /// Its labels, sorted.
    pub labels: Vec<String>,
}

/// A label of an archive with how many messages carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelSummary {
    /// The label, e.g. `Inbox` or `Projects/2025`.
    pub name: String,
    /// Messages carrying it.
    pub messages: u64,
    /// Of those, the ones without `\Seen` (compared ignoring case) in any location not removed.
    pub unread: u64,
}

/// Which messages to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageQuery {
    /// The archive.
    pub archive_id: ArchiveId,
    /// Only messages located in this folder and not removed from it.
    pub folder_id: Option<FolderId>,
    /// Only messages carrying this label, compared exactly.
    pub label: Option<String>,
    /// Continue after this point of a previous page.
    pub after: Option<PageCursor>,
    /// At most this many messages; implementations cap it at 500.
    pub limit: u32,
}

/// An opaque position in a list, valid only for the same query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PageCursor(pub String);

/// One page of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items in list order.
    pub items: Vec<T>,
    /// Where the next page starts; `None` on the last page.
    pub next: Option<PageCursor>,
}

/// Totals of an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArchiveStats {
    /// Messages.
    pub messages: u64,
    /// Attachments, inline ones included.
    pub attachments: u64,
    /// Sum of raw message sizes in bytes.
    pub bytes: u64,
    /// Date of the oldest message.
    pub oldest: Option<Timestamp>,
    /// Date of the newest message.
    pub newest: Option<Timestamp>,
}

/// What a user may do with the instance itself. Roles never grant reading mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceRole {
    /// Everything, including transferring ownership.
    Owner,
    /// Users, settings, storage and sync health.
    Admin,
    /// Only their own archives.
    Member,
}

impl InstanceRole {
    /// The lowercase name used in storage and the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    /// Parses [`InstanceRole::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "owner" => Self::Owner,
            "admin" => Self::Admin,
            "member" => Self::Member,
            _ => return None,
        })
    }
}

/// A person who signs in. The password hash is kept apart and read only through
/// [`crate::UserStore::password_hash`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Id.
    pub id: UserId,
    /// Sign-in address, as it was entered.
    pub email: String,
    /// Name shown to people.
    pub name: String,
    /// What the user may do with the instance.
    pub role: InstanceRole,
    /// When it was created.
    pub created_at: Timestamp,
}

/// Fields of a new user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    /// Sign-in address; unique regardless of letter case.
    pub email: String,
    /// Name shown to people.
    pub name: String,
    /// What the user may do with the instance.
    pub role: InstanceRole,
    /// A PHC password hash, or `None` for a user without a password.
    pub password_hash: Option<String>,
}

/// The hash of a session token. Stores never see the token itself.
pub type SessionHash = [u8; 32];

/// What the archive knows about one remote image: a saved copy, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCopy {
    /// The key the URL is known by; see `holtmail_sanitize::remote_key`.
    pub key: String,
    /// The absolute URL.
    pub url: String,
    /// The saved bytes; `None` when fetching failed.
    pub blob_id: Option<BlobId>,
    /// Media type of the saved bytes.
    pub content_type: Option<String>,
    /// Size of the saved bytes; 0 without a copy.
    pub size: u64,
    /// When it was fetched or the fetch failed.
    pub fetched_at: Timestamp,
    /// Why fetching failed, without any message content.
    pub error: Option<String>,
}

/// Which messages of an archive, by when they were archived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivedWhen {
    /// Every message.
    Any,
    /// Messages archived at or after this time.
    Since(Timestamp),
    /// Messages archived before this time.
    Before(Timestamp),
}

/// Saved remote images an archive's messages link to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoteCopyTotals {
    /// Distinct saved images.
    pub images: u64,
    /// Their size in bytes.
    pub bytes: u64,
    /// Distinct images that could not be fetched.
    pub failed: u64,
}

/// What is derived from an attachment's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivedKind {
    /// A small preview image.
    Thumbnail,
    /// Plain text for the search index.
    Text,
}

impl DerivedKind {
    /// The stable name of the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
            Self::Text => "text",
        }
    }
}

/// The outcome of deriving something from a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derived {
    /// Made and stored as this blob.
    Blob(BlobId),
    /// Nothing can be made from the source, e.g. an image that does not decode or a file
    /// without text. Recorded so it is not tried again.
    Nothing,
}

/// Lines a task log keeps from its start when it grows past [`TASK_LOG_LINES`].
pub const TASK_LOG_HEAD: usize = 50;

/// Lines a task log keeps in all: the first [`TASK_LOG_HEAD`] and the most recent ones.
pub const TASK_LOG_LINES: usize = 500;

/// What a background task does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Syncing an account.
    Sync,
    /// Importing an uploaded file.
    Import,
    /// Saving remote images.
    RemoteImages,
    /// Syncing an account's contacts and calendars.
    ContactsCalendars,
}

impl TaskKind {
    /// The stable name of the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Import => "import",
            Self::RemoteImages => "remote_images",
            Self::ContactsCalendars => "contacts_calendars",
        }
    }

    /// The kind named `name`, as [`TaskKind::as_str`] writes it.
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Sync,
            Self::Import,
            Self::RemoteImages,
            Self::ContactsCalendars,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == name)
    }
}

/// Where a task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Still working.
    Running,
    /// Finished without errors.
    Done,
    /// Stopped by an error, or finished with some.
    Failed,
    /// Stopped on request.
    Cancelled,
    /// Stopped because the server stopped while it ran.
    Interrupted,
}

impl TaskState {
    /// The stable name of the state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    /// The state named `name`, as [`TaskState::as_str`] writes it.
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Running,
            Self::Done,
            Self::Failed,
            Self::Cancelled,
            Self::Interrupted,
        ]
        .into_iter()
        .find(|state| state.as_str() == name)
    }
}

/// How serious a task log line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLevel {
    /// Progress.
    Info,
    /// Something was skipped.
    Warning,
    /// Something failed.
    Error,
}

impl TaskLevel {
    /// The stable name of the level.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// The level named `name`, as [`TaskLevel::as_str`] writes it.
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Info, Self::Warning, Self::Error]
            .into_iter()
            .find(|level| level.as_str() == name)
    }
}

/// One line of a task's log. Holds counts, names and reasons, never message content or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLine {
    /// When it was written.
    pub at: Timestamp,
    /// How serious it is.
    pub level: TaskLevel,
    /// The text.
    pub text: String,
}

/// A background task as kept across restarts, without its log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Unique among tasks.
    pub id: String,
    /// What it does.
    pub kind: TaskKind,
    /// Its archive.
    pub archive_id: Option<ArchiveId>,
    /// Its account, for syncs and imports.
    pub account_id: Option<AccountId>,
    /// What it works on, e.g. the account or archive name.
    pub title: String,
    /// Where it is.
    pub state: TaskState,
    /// When it started.
    pub started_at: Timestamp,
    /// When it ended.
    pub finished_at: Option<Timestamp>,
    /// Steps done.
    pub done: u64,
    /// Steps in all, when known.
    pub total: Option<u64>,
    /// One line on where it stands.
    pub summary: String,
    /// Whether it can be asked to stop.
    pub cancellable: bool,
    /// What the task was started with, opaque to the store, so interrupted work can be started
    /// again.
    pub job: Option<String>,
}

/// A stored task with its log, oldest line first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTask {
    /// The task.
    pub record: TaskRecord,
    /// Its log, bounded as [`TASK_LOG_HEAD`] and [`TASK_LOG_LINES`] say.
    pub log: Vec<TaskLine>,
}

/// How an account's last sync or import ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRun {
    /// When it ended.
    pub finished_at: Timestamp,
    /// Messages it added.
    pub new_messages: u64,
    /// Why it failed, when it did. Safe to show: never holds mail or secrets.
    pub error: Option<String>,
}
