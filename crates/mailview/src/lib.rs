//! Contract for the served mail view: what IMAP, JMAP and the web app show of one archive to one
//! person, and the one way they change it.
//!
//! A [`MailView`] computes the mailbox tree, assigns served UIDs and `MODSEQ`, merges flags
//! across places, hands out raw bytes and their MIME structure, searches and reports changes. A
//! [`MailActions`] turns a client's flag change, move, copy, delete or append into a held mail
//! change, a local flag, a local place or a draft, behind the five write locks, and says per
//! message whether it was held, applied or refused. Neither ever deletes a message from the
//! archive. The design is in SERVE.md, "The mail view".

mod actions;
mod mailbox;
mod message;
mod search;
#[cfg(feature = "testing")]
pub mod testing;

use std::collections::{BTreeSet, HashMap};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, DeviceId, DeviceScope, ThreadId, UserId};

pub use actions::{
    Appended, FlagChange, FlagResult, Lock, NewEmail, Outcome, PlaceResult, Refusal,
};
pub use holtmail_store::FolderRole;
pub use mailbox::{
    Mailbox, MailboxChanges, MailboxId, MailboxRights, MailboxStatus, Member, MemberFlags, Snapshot,
};
pub use message::{ByteRange, EmailRef, Envelope, Part};
pub use search::{Filter, TextField};

/// Who is looking: a person, through a device or the web app, at one archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// The person.
    pub user: UserId,
    /// The device they use; `None` for the web app.
    pub device: Option<DeviceId>,
    /// The archive the view is of.
    pub archive: ArchiveId,
    /// What the credentials allow. Reading needs [`DeviceScope::Read`]; changes upstream need
    /// [`DeviceScope::WriteMail`] too.
    pub scopes: BTreeSet<DeviceScope>,
}

impl Viewer {
    /// A person in the web app, whose session carries every scope.
    pub fn web(user: UserId, archive: ArchiveId) -> Self {
        Self {
            user,
            device: None,
            archive,
            scopes: DeviceScope::ALL.into_iter().collect(),
        }
    }
}

/// The view's states per JMAP type: the highest change log `seq` behind each, counting rows of
/// other users' own state not at all. They never go down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewState {
    /// Emails: messages entering or leaving the archive, their places and flags, drafts.
    pub email: u64,
    /// Mailboxes: the tree, subscriptions, and anything that changes counts.
    pub mailbox: u64,
    /// Threads: emails entering threads, and merges.
    pub thread: u64,
}

impl ViewState {
    /// The highest of the three.
    pub fn max(&self) -> u64 {
        self.email.max(self.mailbox).max(self.thread)
    }
}

/// Ids that changed in a window of the change log. An id is in at most one list: one that
/// appeared and went away inside the window is in none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed<T> {
    /// Ids that did not exist at the start of the window.
    pub created: Vec<T>,
    /// Ids that existed at the start and at the end, and changed.
    pub updated: Vec<T>,
    /// Ids that existed at the start and no longer do.
    pub destroyed: Vec<T>,
}

impl<T> Default for Changed<T> {
    fn default() -> Self {
        Self {
            created: Vec::new(),
            updated: Vec::new(),
            destroyed: Vec::new(),
        }
    }
}

impl<T> Changed<T> {
    /// Whether all three lists are empty.
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.updated.is_empty() && self.destroyed.is_empty()
    }
}

/// What changed in the view after a state, for JMAP `/changes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewChanges {
    /// The state the window ends at; pass it as `since` to continue.
    pub upto: u64,
    /// Whether more changes follow after `upto`.
    pub more: bool,
    /// Emails. An email exists while its message is in the archive (or its draft version is the
    /// draft's newest); place and flag changes make it updated.
    pub emails: Changed<EmailRef>,
    /// Mailboxes added, renamed, re-roled, removed, or whose subscription or rights changed.
    pub mailboxes: Changed<MailboxId>,
    /// Whether any email's places or flags changed, so mailbox counts may differ.
    pub counts_changed: bool,
    /// Threads; a thread merged into another is destroyed and its emails updated.
    pub threads: Changed<ThreadId>,
}

/// Why a call of the view failed. Reasons name fields and rules, never addresses or content.
#[derive(Debug, thiserror::Error)]
pub enum MailViewError {
    /// No such thing for this viewer: `"archive"` (also when the viewer may not read it),
    /// `"mailbox"`, `"email"`, `"thread"` or `"blob"`.
    #[error("no such {0}")]
    NotFound(&'static str),
    /// The mailbox's served UIDs are still being built for the first time; try again later.
    #[error("the mailbox is being prepared ({done} of {total})")]
    Building {
        /// Members done.
        done: u64,
        /// Members in all.
        total: u64,
    },
    /// The state is older than the change log keeps.
    #[error("changes since this state are no longer known")]
    CannotCalculateChanges,
    /// The message is larger than the view takes.
    #[error("the message is larger than the {limit} bytes allowed")]
    TooLarge {
        /// The limit in bytes.
        limit: u64,
    },
    /// A search text matched more messages in the archive than the view checks at once, even
    /// narrowed to the mailbox's dates; the answer would be incomplete, so none is given
    /// (IMAP `NO [LIMIT]`). A narrower search may pass.
    #[error("the search matches more than {limit} messages")]
    TooMany {
        /// How many matches the view checks at once.
        limit: u64,
    },
    /// The request cannot be served as given.
    #[error("invalid: {0}")]
    Invalid(String),
    /// The archive could not be reached; a later try may pass.
    #[error("the mail view is unavailable: {0}")]
    Unavailable(String),
}

/// Reads the served mail view of one archive through one viewer's eyes. Safe to share between
/// tasks.
///
/// Every method answers [`MailViewError::NotFound`]`("archive")` for a viewer who may not read
/// the archive or lacks the `read` scope, so nothing tells such a viewer the archive exists.
/// Mailbox reads catch the mailbox's served UIDs up with the change log first.
#[async_trait]
pub trait MailView: Send + Sync {
    /// The mailbox tree in the archive's layout, parents before children, each with the
    /// viewer's subscription and rights. Paths are unique ignoring ASCII case, so `INBOX` is
    /// always the inbox. Never reads a blob.
    async fn tree(&self, viewer: &Viewer) -> Result<Vec<Mailbox>, MailViewError>;

    /// Counts and served-UID state of a mailbox, without its members.
    async fn status(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
    ) -> Result<MailboxStatus, MailViewError>;

    /// Every member of a mailbox with its served UID and `MODSEQ`, ascending by UID. UIDs are
    /// assigned to new members in the order their messages were archived; a UID is never given
    /// to another email and never reused, also after the email leaves and comes back (it then
    /// gets a new one). `uidvalidity` stays the same for as long as the served tables last.
    /// `NotFound("mailbox")` for an unknown or unselectable mailbox.
    async fn select(&self, viewer: &Viewer, mailbox: &MailboxId)
    -> Result<Snapshot, MailViewError>;

    /// The members whose `MODSEQ` is above `since`, with their flags, and the UIDs that left the
    /// mailbox after `since`, both ascending: what CONDSTORE and QRESYNC need. A UID that left
    /// may be reported without the viewer ever having seen it.
    async fn mailbox_changes(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        since: u64,
    ) -> Result<MailboxChanges, MailViewError>;

    /// The effective flags of each email: in `mailbox`, the synced flags of the places behind
    /// it with open mail changes on top, joined, then the viewer's local flags; with `None`,
    /// the same over every place of the email in the archive. Unknown emails are left out;
    /// flags are deduplicated ignoring ASCII case and sorted. A draft shows `\Draft` and
    /// `\Seen`.
    async fn flags(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
    ) -> Result<HashMap<EmailRef, Vec<String>>, MailViewError>;

    /// The stored fields of each email, in the order asked; unknown emails are left out. Never
    /// reads more than the store and derived data, except for fields the store lacks.
    async fn envelopes(
        &self,
        viewer: &Viewer,
        emails: &[EmailRef],
    ) -> Result<Vec<Envelope>, MailViewError>;

    /// The email's bytes exactly as archived (or as the draft was saved or rendered); their
    /// BLAKE3 hash is [`Envelope::blob`].
    async fn raw(&self, viewer: &Viewer, email: &EmailRef) -> Result<Bytes, MailViewError>;

    /// The MIME structure of the email's raw bytes; the same bytes give the same structure.
    async fn structure(&self, viewer: &Viewer, email: &EmailRef) -> Result<Part, MailViewError>;

    /// The bytes of a blob that is the raw form of an email the viewer can read.
    /// `NotFound("blob")` for any other blob.
    async fn blob(&self, viewer: &Viewer, blob: &BlobId) -> Result<Bytes, MailViewError>;

    /// The members of `mailbox` that match `filter`, ascending by UID. Text is matched by
    /// words, ignoring case, not by substring: a value matches when every word of it starts a
    /// word of the field (an implementation may also match other forms of the word, such as
    /// its stem). A piece from the middle of a word never matches. The answer is never cut
    /// short: when it cannot be complete the search fails with [`MailViewError::TooMany`].
    async fn search(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        filter: &Filter,
    ) -> Result<Vec<Member>, MailViewError>;

    /// The emails of a thread the viewer can see, oldest first. `NotFound("thread")` when none.
    async fn thread(
        &self,
        viewer: &Viewer,
        thread: ThreadId,
    ) -> Result<Vec<EmailRef>, MailViewError>;

    /// The view's current states.
    async fn state(&self, viewer: &Viewer) -> Result<ViewState, MailViewError>;

    /// What changed after `since` (a value from [`ViewState`] or [`ViewChanges::upto`]), in at
    /// most `limit` change log rows. Rows of other users' own state are skipped.
    /// [`MailViewError::CannotCalculateChanges`] when `since` is older than the log keeps.
    async fn changes(
        &self,
        viewer: &Viewer,
        since: u64,
        limit: u32,
    ) -> Result<ViewChanges, MailViewError>;
}

/// Changes the mail view the way a client asks, and only through the archive's own paths: a
/// held mail change for an upstream server, a local flag, a local place, a draft or a
/// subscription. Safe to share between tasks.
///
/// Each method checks the five locks (instance switch, account capability, archive grant,
/// viewer scope, sign-in consent) per place, before anything is written, and answers one
/// result per distinct email asked about, in the order asked. A held change shows in the view
/// at once. A message is never deleted from the archive; at most it loses its places and shows
/// in `Only in Holtmail`. Every change raises the `MODSEQ` of what it touched.
///
/// Every method answers [`MailViewError::NotFound`]`("archive")` like [`MailView`] does, and
/// `NotFound("mailbox")` for an unknown mailbox.
#[async_trait]
pub trait MailActions: Send + Sync {
    /// Adds and removes flags. In `mailbox`, every place behind it is written; with `None`,
    /// every place of the email. A place whose locks are open gets a held flags change; a place
    /// whose locks are closed gets the viewer's local flag instead when local flags are on and
    /// the change does not touch `\Deleted`, which has no local form; a message with no place
    /// left gets local flags only. The result is `Held` when any place got a mail change,
    /// `Applied` when only local flags were written, and refused otherwise, with the flags the
    /// email shows afterwards. With `unchanged_since`, an email whose `MODSEQ` in `mailbox` is
    /// above it is refused as [`Refusal::Modified`] and left alone. Setting what is already
    /// shown is `Applied` and writes nothing. Flags on a draft are fixed: `Applied`, unchanged.
    async fn set_flags(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
        change: &FlagChange,
        unchanged_since: Option<u64>,
    ) -> Result<Vec<FlagResult>, MailViewError>;

    /// Moves emails from `from` to `to`: within an account a held move (into the trash, a
    /// delete), into another account an append of the archived bytes there and a delete here.
    /// Into a unified role mailbox each place goes to that role's folder in its own account.
    /// Refused as [`Refusal::Cannot`] into `All Mail`, `Only in Holtmail`, Drafts or a
    /// placeholder, out of `All Mail`, `Only in Holtmail` or Drafts, and where the account has no folder
    /// for the role. The result carries the email's UID in `to` when it is held there.
    async fn move_to(
        &self,
        viewer: &Viewer,
        from: &MailboxId,
        emails: &[EmailRef],
        to: &MailboxId,
    ) -> Result<Vec<PlaceResult>, MailViewError>;

    /// Copies emails from `from` into `to` as a held append of the archived bytes in the target
    /// folder's account; an email already in the target is `Applied` with its UID there.
    /// Refused as [`Refusal::Cannot`] into `All Mail`, `Only in Holtmail`, Drafts or a
    /// placeholder.
    async fn copy_to(
        &self,
        viewer: &Viewer,
        from: &MailboxId,
        emails: &[EmailRef],
        to: &MailboxId,
    ) -> Result<Vec<PlaceResult>, MailViewError>;

    /// Removes emails from `mailbox` (IMAP `EXPUNGE`): each place behind it gets a held delete,
    /// to the account's trash or, in the trash or without one, expunged. A local place is
    /// dropped and a draft deleted (`Applied`). With `None` (JMAP destroy) every place is
    /// expunged and the email shows in `Only in Holtmail`. Refused as [`Refusal::Cannot`] in
    /// `All Mail` and `Only in Holtmail`, and for an email with no place left.
    async fn remove(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
    ) -> Result<Vec<PlaceResult>, MailViewError>;

    /// Takes a client's message into `mailbox`. Into Drafts it becomes a new draft version
    /// (`Applied`, a new UID each time). Into a folder, or a role mailbox (the first account by
    /// name that takes it), it is archived and gets a held append; the same bytes again are the
    /// same email with the same UID. Into Sent where no account takes it, it becomes the
    /// viewer's local place there (`Applied`), because clients retry a refused Sent copy
    /// forever. Refused as [`Refusal::Cannot`] into `All Mail`, `Only in Holtmail` or a
    /// placeholder; a refused append archives nothing. [`MailViewError::TooLarge`] above
    /// [`MailActions::append_limit`].
    async fn append(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        email: NewEmail,
    ) -> Result<Appended, MailViewError>;

    /// The largest message [`MailActions::append`] takes, in bytes.
    async fn append_limit(&self, viewer: &Viewer) -> Result<u64, MailViewError>;

    /// Records the viewer's subscription to a mailbox; whether it differs from what the viewer
    /// had, the default included. Subscriptions are per user and never leave Holtmail.
    async fn subscribe(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        subscribed: bool,
    ) -> Result<bool, MailViewError>;
}
