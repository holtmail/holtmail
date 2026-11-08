use async_trait::async_trait;
use holtmail_core::Timestamp;

use crate::SourceError;

/// A flag or keyword on a message, as IMAP names them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Flag {
    /// `\Seen`: read.
    Seen,
    /// `\Answered`: replied to.
    Answered,
    /// `\Flagged`: starred or flagged for follow-up.
    Flagged,
    /// `\Draft`: not finished.
    Draft,
    /// Any other flag or keyword, such as `$Forwarded` or `$Junk`, spelled as the server spells
    /// it.
    Keyword(String),
}

impl Flag {
    /// The flag as IMAP spells it, e.g. `\Seen` or `$Forwarded`.
    pub fn as_imap(&self) -> &str {
        match self {
            Self::Seen => "\\Seen",
            Self::Answered => "\\Answered",
            Self::Flagged => "\\Flagged",
            Self::Draft => "\\Draft",
            Self::Keyword(name) => name,
        }
    }

    /// The flag IMAP spells `name`. The four system flags are matched ignoring ASCII case;
    /// anything else, `\Deleted` included, is a [`Flag::Keyword`] with `name` unchanged.
    pub fn parse(name: &str) -> Self {
        [Self::Seen, Self::Answered, Self::Flagged, Self::Draft]
            .into_iter()
            .find(|flag| flag.as_imap().eq_ignore_ascii_case(name))
            .unwrap_or_else(|| Self::Keyword(name.to_owned()))
    }

    /// Whether `name`, as IMAP spells flags, is this flag, ignoring ASCII case.
    pub fn is(&self, name: &str) -> bool {
        self.as_imap().eq_ignore_ascii_case(name)
    }
}

/// What a [`MailboxWriter`] can do on its server. Every writer can move, expunge and append;
/// the rest depends on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxWriteCaps {
    /// The system flags (not [`Flag::Keyword`]) it can set and clear. Microsoft Graph has only
    /// [`Flag::Seen`] and [`Flag::Flagged`].
    pub flags: Vec<Flag>,
    /// Whether it can set and clear [`Flag::Keyword`]s.
    pub keywords: bool,
    /// Whether [`MailboxWriter::set_flags`] accepts `unchanged_since` (IMAP `CONDSTORE`).
    pub conditional: bool,
    /// Whether the server has labels and [`MailboxWriter::set_labels`] works (Gmail).
    pub labels: bool,
    /// Whether [`MailboxWriter::move_to`] reports the UIDs the messages got in the target
    /// (IMAP `UIDPLUS`; JMAP and Graph keep or derive ids).
    pub move_uids: bool,
    /// Whether [`MailboxWriter::append`] reports the UID of the new message.
    pub append_uid: bool,
    /// Whether [`MailboxWriter::delete`] can mark messages deleted without expunging them
    /// (IMAP `\Deleted`).
    pub mark_deleted: bool,
}

impl MailboxWriteCaps {
    /// Everything: an IMAP server with `CONDSTORE`, `UIDPLUS`, keywords and labels.
    pub fn full() -> Self {
        Self {
            flags: vec![Flag::Seen, Flag::Answered, Flag::Flagged, Flag::Draft],
            keywords: true,
            conditional: true,
            labels: true,
            move_uids: true,
            append_uid: true,
            mark_deleted: true,
        }
    }

    /// Whether the writer can set and clear `flag`.
    pub fn supports(&self, flag: &Flag) -> bool {
        match flag {
            Flag::Keyword(_) => self.keywords,
            system => self.flags.contains(system),
        }
    }
}

/// What [`MailboxWriter::set_flags`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlagResult {
    /// The UIDs whose flags now include every added flag and none of the removed ones,
    /// ascending, including those that already had them.
    pub updated: Vec<u64>,
    /// The UIDs left unchanged because they changed on the server after `unchanged_since`,
    /// ascending (IMAP `MODIFIED`).
    pub modified: Vec<u64>,
}

/// What [`MailboxWriter::move_to`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moved {
    /// Each moved message as its UID in the source folder and its UID in the target when the
    /// server reports it, ascending by the first.
    pub uids: Vec<(u64, Option<u64>)>,
}

/// Changes messages on one account's server: flags, labels, folders, new and deleted messages.
///
/// Separate from [`crate::Source`], which stays read-only; the composition root hands one out
/// only for accounts whose owner allowed writing. Every method works on its own connection or
/// request and may be called from several tasks at once. A call that fails with
/// `Unsupported` or `Missing` changed nothing. Any other failure may leave the call partly
/// applied: some messages changed and others not, or a flags change with only its added flags
/// set. Every method except [`MailboxWriter::append`] is idempotent, so calling again with
/// the same arguments finishes the job. UIDs are those the account's [`crate::Source`]
/// reports for the folder.
#[async_trait]
pub trait MailboxWriter: Send + Sync {
    /// What this writer can do. Stays the same for the writer's lifetime.
    fn capabilities(&self) -> MailboxWriteCaps;

    /// Adds `add` to and removes `remove` from the flags of the messages in `folder` with these
    /// UIDs. Idempotent: adding a flag a message has or removing one it lacks changes nothing
    /// and still counts the UID as updated. A flag in both lists ends up removed. UIDs not in
    /// the folder are ignored and reported in neither list.
    ///
    /// With `unchanged_since`, a message whose mod-sequence is above it is left alone and
    /// reported in [`FlagResult::modified`]; the others are updated.
    ///
    /// `Unsupported` before changing anything when a flag is not in
    /// [`MailboxWriteCaps::supports`] or `unchanged_since` is given without
    /// [`MailboxWriteCaps::conditional`]; `Missing` when the folder does not exist.
    async fn set_flags(
        &self,
        folder: &str,
        uids: &[u64],
        add: &[Flag],
        remove: &[Flag],
        unchanged_since: Option<u64>,
    ) -> Result<FlagResult, SourceError>;

    /// Adds and removes labels (readable names as in [`crate::RemoteMessage::labels`]) on the
    /// messages with these UIDs in the account's folder with the
    /// [`holtmail_store::FolderRole::All`] role. Idempotent, like
    /// [`MailboxWriter::set_flags`]; a label in both lists ends up removed; unknown UIDs are
    /// ignored. `Unsupported` without [`MailboxWriteCaps::labels`], and for a label that
    /// stands for a mailbox (such as Gmail's Inbox or Trash), since changing it would move or
    /// delete the message; `Missing` when the account has no such folder.
    async fn set_labels(
        &self,
        uids: &[u64],
        add: &[String],
        remove: &[String],
    ) -> Result<(), SourceError>;

    /// Moves the messages with these UIDs from `folder` to `target`, keeping their bytes and
    /// flags. They are gone from `folder` afterwards. UIDs not in `folder` are ignored and left
    /// out of the result, so moving again is harmless. `Missing` when either folder does not
    /// exist. Where the server has no move, the writer copies and expunges; a failure between
    /// the two can leave a message in both folders, never in neither.
    async fn move_to(&self, folder: &str, uids: &[u64], target: &str)
    -> Result<Moved, SourceError>;

    /// Saves `raw` unchanged as a new message in `folder` with these flags and `date` as its
    /// received time. Not idempotent: two calls make two messages. Returns the new message's
    /// UID when the server tells it, always with [`MailboxWriteCaps::append_uid`].
    /// `Unsupported` for a flag the writer cannot set; `Missing` when the folder does not exist.
    async fn append(
        &self,
        folder: &str,
        raw: &[u8],
        flags: &[Flag],
        date: Timestamp,
    ) -> Result<Option<u64>, SourceError>;

    /// Deletes the messages with these UIDs from `folder`. With `expunge` they are removed for
    /// good; without, they are only marked deleted (IMAP `\Deleted`) and stay listed until
    /// something expunges the folder, which needs [`MailboxWriteCaps::mark_deleted`] and is
    /// `Unsupported` otherwise. Idempotent; UIDs not in the folder are ignored. `Missing` when
    /// the folder does not exist. To move to Trash, use [`MailboxWriter::move_to`].
    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_system_names_ignoring_case() {
        assert_eq!(Flag::parse("\\seen"), Flag::Seen);
        assert_eq!(Flag::parse("\\FLAGGED"), Flag::Flagged);
        assert_eq!(
            Flag::parse("$Forwarded"),
            Flag::Keyword("$Forwarded".into())
        );
        assert_eq!(Flag::parse("\\Deleted"), Flag::Keyword("\\Deleted".into()));
        assert_eq!(Flag::Answered.as_imap(), "\\Answered");
        assert!(Flag::Draft.is("\\draft"));
    }

    #[test]
    fn caps_say_which_flags_work() {
        let graph = MailboxWriteCaps {
            flags: vec![Flag::Seen, Flag::Flagged],
            keywords: false,
            conditional: false,
            labels: false,
            move_uids: false,
            append_uid: false,
            mark_deleted: false,
        };
        assert!(graph.supports(&Flag::Seen));
        assert!(!graph.supports(&Flag::Answered));
        assert!(!graph.supports(&Flag::Keyword("$Junk".into())));
        assert!(MailboxWriteCaps::full().supports(&Flag::Keyword("$Junk".into())));
    }
}
