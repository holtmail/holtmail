//! Contract for reading mail from an account's server: IMAP, Microsoft Graph, JMAP.
//!
//! The shape is IMAP's: folders, numeric UIDs per folder and a `UIDVALIDITY`. Sources for
//! other protocols derive stable UIDs from the server's ids and say whether they ascend with
//! arrival in [`FolderStatus::uids_ascend`]. Sources that can tell cheaply that a folder did not
//! change (Graph delta links) offer [`SourceSession::state`] and
//! [`SourceSession::unchanged_since`].
//!
//! A source is read-only. It never changes, moves, deletes or marks messages on the server; for
//! IMAP that means `EXAMINE` and `BODY.PEEK[]`, so archiving leaves unread mail unread.

#[cfg(feature = "testing")]
pub mod testing;

use std::ops::RangeInclusive;

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
    /// A label shown as a folder, as Gmail does. Every message in it is also in the account's
    /// folder with the [`FolderRole::All`] role, where it carries this label in
    /// [`RemoteMessage::labels`], so archiving the folder again only repeats those messages.
    pub label: bool,
}

/// The state of a selected folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderStatus {
    /// UIDs are only comparable while this stays the same.
    pub uid_validity: u64,
    /// The UID the next new message will get, when the server says.
    pub uid_next: Option<u64>,
    /// `HIGHESTMODSEQ` when the server supports `CONDSTORE`. When set, the session answers
    /// [`SourceSession::flags`] with `changed_since`.
    pub highest_modseq: Option<u64>,
    /// Number of messages in the folder.
    pub exists: u64,
    /// Whether a message added to the folder always gets a UID above every UID the folder had
    /// before, as IMAP promises. When `false`, as on JMAP where UIDs are derived from the
    /// server's own ids, a new message can appear below archived UIDs, so a sync looks for new
    /// messages among every UID it has not archived yet instead of above a watermark.
    pub uids_ascend: bool,
}

/// A message fetched from the selected folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMessage {
    /// Its UID in the folder.
    pub uid: u64,
    /// Flags and keywords, e.g. `\Seen`, `\Flagged`, `$Forwarded`.
    pub flags: Vec<String>,
    /// The message's labels on servers that have them (Gmail), decoded to readable names, e.g.
    /// `Inbox`, `Starred`, `Projects/2025`; `None` on servers without labels.
    pub labels: Option<Vec<String>>,
    /// An id the server keeps for the message in every folder (Gmail `X-GM-MSGID`), when it has
    /// one.
    pub server_id: Option<u64>,
    /// When the server received it.
    pub received_at: Option<Timestamp>,
    /// The complete message as stored on the server.
    pub raw: Bytes,
}

/// The current flags of a message in the selected folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFlags {
    /// Its UID in the folder.
    pub uid: u64,
    /// Flags and keywords, as in [`RemoteMessage::flags`].
    pub flags: Vec<String>,
    /// Labels, as in [`RemoteMessage::labels`].
    pub labels: Option<Vec<String>>,
}

/// How a source signs in. `Debug` never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A password or app password.
    Password(String),
    /// An OAuth 2.0 access token, e.g. for IMAP `AUTHENTICATE XOAUTH2`. It expires, so a source
    /// is built with a fresh one before each sync.
    AccessToken(String),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Password(_) => "Password(..)",
            Self::AccessToken(_) => "AccessToken(..)",
        })
    }
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

    /// An opaque token for the selected folder as the last [`SourceSession::uids`] listed it,
    /// such as a Graph delta link, to hand back to [`SourceSession::unchanged_since`] in a later
    /// session. `None` (the default) when the source keeps no such state, or before a listing.
    /// It holds no credentials.
    fn state(&self) -> Option<String> {
        None
    }

    /// Whether nothing in the selected folder changed since `state` was taken: no message
    /// added, removed or changed, flags included. `Some` with the token to keep when nothing
    /// changed; `None` when something did, when the token is no longer accepted, or (the
    /// default) when the source keeps no state. Never changes the selected folder.
    async fn unchanged_since(&mut self, state: &str) -> Result<Option<String>, SourceError> {
        let _ = state;
        Ok(None)
    }

    /// The messages with these UIDs, ascending by UID. UIDs that no longer exist are left out.
    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError>;

    /// Flags (and labels, where the server has them) of the messages whose UIDs fall in `uids`,
    /// ascending by UID, without fetching their content and without changing anything on the
    /// server. With `changed_since`, only messages whose flags or labels changed after that
    /// mod-sequence; that needs a folder whose [`FolderStatus::highest_modseq`] is set, and fails
    /// with `Protocol` otherwise.
    async fn flags(
        &mut self,
        uids: RangeInclusive<u64>,
        changed_since: Option<u64>,
    ) -> Result<Vec<RemoteFlags>, SourceError>;

    /// Signs out. Dropping a session without it is allowed.
    async fn logout(&mut self) -> Result<(), SourceError>;
}
