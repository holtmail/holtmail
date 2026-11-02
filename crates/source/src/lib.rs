//! Contract for reading mail from an account's server: IMAP, Microsoft Graph, JMAP.
//!
//! A source is read-only. It never changes, moves, deletes or marks messages on the server; for
//! IMAP that means `EXAMINE` and `BODY.PEEK[]`, so archiving leaves unread mail unread.

#[cfg(feature = "testing")]
pub mod testing;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::Timestamp;
use holtmail_store::FolderRole;

/// A folder on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    /// Full path with `/` between levels, whatever delimiter the server uses.
    pub path: String,
    /// What the folder is for, from special-use attributes or well-known names.
    pub role: Option<FolderRole>,
    /// Whether it can hold messages; `false` for pure parents like IMAP `\Noselect`.
    pub selectable: bool,
}

/// The state of a selected folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderStatus {
    /// UIDs are only comparable while this stays the same.
    pub uid_validity: u64,
    /// The UID the next new message will get, when the server says.
    pub uid_next: Option<u64>,
    /// `HIGHESTMODSEQ` when the server supports `CONDSTORE`.
    pub highest_modseq: Option<u64>,
    /// Number of messages in the folder.
    pub exists: u64,
}

/// A message fetched from the selected folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMessage {
    /// Its UID in the folder.
    pub uid: u64,
    /// Flags and keywords, e.g. `\Seen`, `$Forwarded`, Gmail labels as `X-GM-LABELS` values.
    pub flags: Vec<String>,
    /// When the server received it.
    pub received_at: Option<Timestamp>,
    /// The complete message as stored on the server.
    pub raw: Bytes,
}

/// Why talking to the server failed.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The server rejected the credentials. Retrying will not help until they change.
    #[error("sign-in rejected: {0}")]
    Auth(String),
    /// The server could not be reached or the connection broke. Retrying later may help.
    #[error("connection failed: {0}")]
    Connection(String),
    /// The server answered something unexpected or refused a command.
    #[error("server error: {0}")]
    Protocol(String),
    /// The account's settings are incomplete or invalid.
    #[error("invalid account settings: {0}")]
    Config(String),
}

impl SourceError {
    /// Whether retrying the same operation later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connection(_) | Self::Protocol(_))
    }
}

/// Opens sessions to one account's server.
#[async_trait]
pub trait Source: Send + Sync {
    /// Connects and signs in.
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError>;
}

/// One signed-in connection. Not shared between tasks.
#[async_trait]
pub trait SourceSession: Send {
    /// Every folder of the account.
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError>;

    /// Selects a folder read-only; later calls work on it.
    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError>;

    /// Every UID in the selected folder, ascending.
    async fn uids(&mut self) -> Result<Vec<u64>, SourceError>;

    /// The messages with these UIDs, ascending by UID. UIDs that no longer exist are left out.
    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError>;

    /// Signs out. Dropping a session without it is allowed.
    async fn logout(&mut self) -> Result<(), SourceError>;
}
