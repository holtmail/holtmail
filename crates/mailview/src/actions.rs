use bytes::Bytes;
use holtmail_core::Timestamp;

use crate::message::EmailRef;

/// Which lock refused, in the order they are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lock {
    /// An instance switch is off.
    Switch,
    /// The account does not allow it.
    Capability,
    /// The viewer's grant on the archive does not allow it.
    Grant,
    /// The viewer's credentials do not allow it.
    Scope,
    /// The account's sign-in lacks the consent to change mail.
    Consent,
}

/// Why an action was refused for one email.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// A lock is closed (IMAP `NO [CANNOT]`, JMAP `forbidden`).
    Locked(Lock),
    /// The action has no form here: into `All Mail`, an account without a folder for the role,
    /// a move or delete of a message with no place left, `\Deleted` as a local flag
    /// (IMAP `NO [CANNOT]`, JMAP `forbidden`).
    Cannot,
    /// The email changed after the `MODSEQ` the client gave (IMAP `[MODIFIED]`).
    Modified,
    /// The email is not in the mailbox.
    NotFound,
}

/// What became of an action on one email.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Recorded as a mail change on its way upstream, and shown at once. Writeback may still
    /// lose to the server, and the view then shows the server's state with a new `MODSEQ`.
    Held,
    /// Done in Holtmail and final: a local flag, a local place, a draft, or nothing to do.
    Applied,
    /// Not done; nothing was written for it.
    Refused(Refusal),
}

impl Outcome {
    /// Whether it was held or applied.
    pub fn accepted(&self) -> bool {
        !matches!(self, Self::Refused(_))
    }
}

/// Flags to add and remove, spelled as IMAP spells them (`\Seen`, `$Forwarded`) and compared
/// ignoring ASCII case. A flag in both lists is invalid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlagChange {
    /// Flags to add.
    pub add: Vec<String>,
    /// Flags to remove.
    pub remove: Vec<String>,
}

/// What a flag change did to one email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagResult {
    /// The email.
    pub email: EmailRef,
    /// What became of it.
    pub outcome: Outcome,
    /// The flags it shows afterwards, in the mailbox asked about.
    pub flags: Vec<String>,
    /// Its `MODSEQ` afterwards in that mailbox, or the view's state with no mailbox.
    pub modseq: u64,
}

/// What a move, copy or remove did to one email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceResult {
    /// The email.
    pub email: EmailRef,
    /// What became of it.
    pub outcome: Outcome,
    /// Its UID in the target mailbox of a move or copy that was not refused (`COPYUID`).
    pub uid: Option<u32>,
}

/// A client's message to take in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEmail {
    /// The bytes as the client sent them.
    pub raw: Bytes,
    /// The flags it gets.
    pub flags: Vec<String>,
    /// Its received time; now when `None`.
    pub received_at: Option<Timestamp>,
}

/// What an append did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    /// What became of it.
    pub outcome: Outcome,
    /// The email it became, unless refused.
    pub email: Option<EmailRef>,
    /// Its UID in the mailbox (`APPENDUID`), unless refused.
    pub uid: Option<u32>,
}
