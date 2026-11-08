//! Contract for reading mail from an account's server: IMAP, Microsoft Graph, JMAP.
//!
//! The shape is IMAP's: folders, numeric UIDs per folder and a `UIDVALIDITY`. Sources for
//! other protocols derive stable UIDs from the server's ids and say whether they ascend with
//! arrival in [`FolderStatus::uids_ascend`]. Sources that can tell cheaply that a folder did not
//! change (Graph delta links) offer [`SourceSession::state`] and
//! [`SourceSession::unchanged_since`].
//!
//! Sources whose server can push offer [`Source::watch`]: a [`SourceWatch`] that returns when
//! something may have changed. A watch is a hint to sync, never a record of what changed.
//!
//! A source is read-only. It never changes, moves, deletes or marks messages on the server; for
//! IMAP that means `EXAMINE` and `BODY.PEEK[]`, so archiving leaves unread mail unread.
//! Changing the mailbox is the job of a separate [`MailboxWriter`], handed out only for accounts
//! whose owner allowed it.

#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod writer;

pub use writer::*;

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
    /// The server or this implementation cannot do what was asked, such as a keyword on a server
    /// without keywords. Nothing was changed. Retrying will not help.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// A folder the operation names does not exist on the server. Nothing was changed. Retrying
    /// will not help until it exists.
    #[error("missing on the server: {0}")]
    Missing(String),
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

    /// Starts watching the account for changes, signed in on connections of its own. `folders`
    /// names the folder paths to watch; a source that can only watch the whole account watches
    /// that and reports [`Changed::folders`] as `None`. `Ok(None)` (the default) when the server
    /// cannot push. Fails like [`Source::connect`] when it cannot sign in.
    async fn watch(&self, folders: &[String]) -> Result<Option<Box<dyn SourceWatch>>, SourceError> {
        let _ = folders;
        Ok(None)
    }
}

/// What a [`SourceWatch`] noticed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changed {
    /// The folder paths that may have changed, each once; `None` when the source cannot tell
    /// which, so every folder should be looked at.
    pub folders: Option<Vec<String>>,
}

impl Changed {
    /// Every folder may have changed.
    pub fn all() -> Self {
        Self { folders: None }
    }

    /// Joins two notices into one that covers both.
    pub fn merge(self, other: Changed) -> Changed {
        match (self.folders, other.folders) {
            (Some(mut a), Some(b)) => {
                for path in b {
                    if !a.contains(&path) {
                        a.push(path);
                    }
                }
                Changed { folders: Some(a) }
            }
            _ => Changed::all(),
        }
    }
}

/// A running watch on one account. Not shared between tasks.
#[async_trait]
pub trait SourceWatch: Send {
    /// Waits until something in the watched folders may have changed since the watch started or
    /// since the last call returned, and says where. A change made while the watch is alive is
    /// never swallowed; a return without a change is allowed. An error ends the watch: the
    /// caller starts a new one, and changes made in between are only found by a sync.
    /// Dropping the future while it waits gives the watch up: drop the watch with it.
    async fn next(&mut self) -> Result<Changed, SourceError>;

    /// Ends the watch and signs out. Dropping a watch without it is allowed.
    async fn close(self: Box<Self>);
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

#[cfg(test)]
mod tests {
    use super::testing::{FakeSource, WriteFolders, watch_conformance, writer_conformance};
    use super::*;

    #[test]
    fn merging_notices_keeps_each_folder_once() {
        let a = Changed {
            folders: Some(vec!["INBOX".into(), "Sent".into()]),
        };
        let b = Changed {
            folders: Some(vec!["Sent".into(), "Work".into()]),
        };
        assert_eq!(
            a.clone().merge(b).folders,
            Some(vec!["INBOX".into(), "Sent".into(), "Work".into()])
        );
        assert_eq!(a.merge(Changed::all()), Changed::all());
    }

    #[tokio::test]
    async fn the_fake_keeps_the_watch_contract() {
        let source = FakeSource::new();
        source.add_folder("INBOX", Some(FolderRole::Inbox));
        source.add_folder("Archive", None);
        assert!(
            source.watch(&[]).await.unwrap().is_none(),
            "no push by default"
        );
        source.enable_watch();
        watch_conformance(&source, &source, "INBOX", "Archive").await;
        assert_eq!(source.watches(), 1);
    }

    fn write_server() -> FakeSource {
        let source = FakeSource::new();
        source.add_folder("INBOX", Some(FolderRole::Inbox));
        source.add_folder("Archive", None);
        source.add_folder("All Mail", Some(FolderRole::All));
        source
    }

    #[tokio::test]
    async fn the_fake_writer_keeps_the_writer_contract() {
        let source = write_server();
        writer_conformance(
            &source.writer(),
            &source,
            WriteFolders {
                folder: "INBOX",
                target: "Archive",
                all: Some("All Mail"),
            },
        )
        .await;

        let limited = write_server();
        limited.set_write_caps(MailboxWriteCaps {
            flags: vec![Flag::Seen, Flag::Flagged],
            keywords: false,
            conditional: false,
            labels: false,
            move_uids: false,
            append_uid: false,
            mark_deleted: false,
        });
        writer_conformance(
            &limited.writer(),
            &limited,
            WriteFolders {
                folder: "INBOX",
                target: "Archive",
                all: None,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn writes_show_in_sessions_and_wake_watches() {
        let source = write_server();
        source.enable_watch();
        let uid = source.deliver("INBOX", b"Subject: one\r\n\r\nHi\r\n", &[]);
        let mut watch = source.watch(&["INBOX".into()]).await.unwrap().unwrap();
        source
            .writer()
            .set_flags("INBOX", &[uid], &[Flag::Seen], &[], None)
            .await
            .unwrap();
        assert_eq!(
            watch.next().await.unwrap().folders,
            Some(vec!["INBOX".to_owned()])
        );
        let mut session = source.connect().await.unwrap();
        session.select("INBOX").await.unwrap();
        let flags = session.flags(1..=u64::MAX, None).await.unwrap();
        assert_eq!(flags[0].flags, vec!["\\Seen".to_owned()]);

        source.fail_writes(Some("cut"));
        assert!(matches!(
            source.writer().delete("INBOX", &[uid], true).await,
            Err(SourceError::Connection(_))
        ));
        assert_eq!(source.writes(), 2);
        source.fail_writes(None);
        assert!(!SourceError::Missing("x".into()).is_transient());
        assert!(!SourceError::Unsupported("x".into()).is_transient());
    }

    #[tokio::test]
    async fn a_broken_watch_ends_with_a_connection_error() {
        let source = FakeSource::new();
        source.add_folder("INBOX", Some(FolderRole::Inbox));
        source.enable_watch();
        let mut watch = source.watch(&["INBOX".into()]).await.unwrap().unwrap();
        source.break_watches(Some("cut"));
        assert!(matches!(
            watch.next().await,
            Err(SourceError::Connection(_))
        ));
        assert!(source.watch(&[]).await.is_err());
        source.break_watches(None);
        source.reject_watch_sign_in(Some("no"));
        assert!(matches!(source.watch(&[]).await, Err(SourceError::Auth(_))));
    }
}
