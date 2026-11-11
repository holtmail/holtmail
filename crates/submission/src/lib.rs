//! Contract for accepting a message from a person or a client and sending it later.
//!
//! A [`Submission`] checks that the sender may send this message as this identity and takes it
//! into a durable queue. It is the one way in for the web app, SMTP submission and JMAP, so each
//! of them is held to the same locks. Nothing is delivered here: a message leaves later, through
//! a configured transport, at most once.

#[cfg(feature = "testing")]
pub mod testing;

use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, DeviceId, DeviceScope, IdentityId, OutboxId, Timestamp, UserId};
use holtmail_route::Route;

/// The size advertised to someone who can send through no transport, in bytes: 25 MiB.
pub const DEFAULT_MAX_SIZE: u64 = 25 * 1024 * 1024;

/// Who is submitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// The person.
    pub user: UserId,
    /// The device they use; `None` for the web app.
    pub device: Option<DeviceId>,
    /// What the credentials allow; sending needs [`DeviceScope::SendMail`].
    pub scopes: BTreeSet<DeviceScope>,
}

impl Actor {
    /// A person in the web app, whose session carries every scope.
    pub fn web(user: UserId) -> Self {
        Self {
            user,
            device: None,
            scopes: DeviceScope::ALL.into_iter().collect(),
        }
    }
}

/// An identity someone may send as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendIdentity {
    /// Id.
    pub id: IdentityId,
    /// The address.
    pub address: String,
    /// The display name, when it has one.
    pub name: Option<String>,
}

/// What a sender's messages must fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmissionLimits {
    /// The largest message in bytes: the smallest limit among the transports of the identities
    /// the sender may use, or [`DEFAULT_MAX_SIZE`] when none says one.
    pub max_size: u64,
    /// Whether every one of those transports takes internationalized addresses. False when
    /// there are none.
    pub smtputf8: bool,
}

/// A message to take in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    /// Who submits it.
    pub actor: Actor,
    /// The archive it is sent from.
    pub archive: ArchiveId,
    /// The identity it is sent as, from [`Submission::sender`] or [`Submission::identities`].
    pub identity: IdentityId,
    /// The return path: the identity's address or a plus address of it.
    pub mail_from: String,
    /// Every recipient, Bcc included, in order.
    pub recipients: Vec<String>,
    /// The exact bytes to send: with `Date` and `Message-ID`, without `Bcc`, and with one `From`
    /// address that is the identity's or a plus address of it.
    pub message: Bytes,
    /// When to send it, at the earliest.
    pub send_at: Option<Timestamp>,
    /// How long it stays undoable; `None` for the instance's default.
    pub undo_window: Option<Duration>,
    /// Makes submitting idempotent per user: a second call with the same user and key returns
    /// the first submission.
    pub idempotency_key: Option<String>,
    /// Sends even though a recipient would see the real mailbox behind an alias. Only for a
    /// person who confirmed it.
    pub allow_leaks: bool,
    /// The route a front end already chose and showed to the person, kept with the message.
    /// With a route the leak guard is not run again, because that front end ran it with more
    /// than the message tells, and `allow_leaks` says that it found leaks the person confirmed.
    /// `mail_from` and the header `From` must then be the route's `from`, which may be an alias
    /// of the identity, and the route must name that identity. Protocols leave this `None`.
    pub route: Option<Route>,
    /// Keeps the message held: it is checked and taken in like any other, but it never leaves
    /// until [`Submission::release`] lets it go, and [`Submission::undo`] can cancel it until
    /// then. For a message that belongs to something recorded after it is queued; `send_at`
    /// does not apply to it.
    pub hold: bool,
}

/// Where a submitted message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubmissionState {
    /// Waiting; it can still be undone.
    Queued,
    /// Being handed over, or waiting to be tried again.
    Sending,
    /// A transport took it for at least one recipient.
    Sent,
    /// Given up.
    Failed,
    /// The connection was lost after the whole message went out; it may have been delivered.
    Uncertain,
    /// Reported undeliverable after it was sent.
    Bounced,
    /// Undone or cancelled before it was sent.
    Cancelled,
}

/// What became of one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientDelivery {
    /// Not handed over yet, or the outcome is unknown.
    Pending,
    /// A server took the message for it.
    Accepted,
    /// A server refused it.
    Refused {
        /// A permanent refusal.
        permanent: bool,
        /// The SMTP code or its closest equivalent.
        code: u16,
        /// The server's words. They may name addresses: never log them.
        text: String,
    },
    /// Taken, then reported undeliverable.
    Bounced {
        /// The enhanced status code, when the report gave one.
        status: Option<String>,
        /// The remote server's words. They may name addresses: never log them.
        text: Option<String>,
    },
}

/// One recipient of a submitted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientStatus {
    /// The address as submitted.
    pub address: String,
    /// What became of it.
    pub delivery: RecipientDelivery,
}

/// A submitted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    /// Id.
    pub id: OutboxId,
    /// The archive it is sent from.
    pub archive: ArchiveId,
    /// The identity it is sent as.
    pub identity: IdentityId,
    /// Where it is.
    pub state: SubmissionState,
    /// When it may leave, at the earliest.
    pub send_at: Timestamp,
    /// Each recipient, in submitted order.
    pub recipients: Vec<RecipientStatus>,
    /// The `Message-ID` in its bytes, without angle brackets.
    pub rfc_message_id: String,
}

/// Why a submission failed. Reasons name fields and rules, never addresses or content.
#[derive(Debug, thiserror::Error)]
pub enum SubmissionError {
    /// The sender may not send this: no such identity for them, a missing grant or scope, a
    /// closed switch or capability, or a null return path.
    #[error("not allowed: {0}")]
    Forbidden(String),
    /// The header `From` is not exactly one address of the identity.
    #[error("the From header is not the identity's address")]
    FromMismatch,
    /// A recipient would see the real mailbox behind an alias the thread came through.
    #[error("a recipient would see the real mailbox behind the alias this thread arrived through")]
    Leak,
    /// Larger than a transport of this identity takes.
    #[error("the message is larger than the {limit} bytes allowed")]
    TooLarge {
        /// The limit in bytes.
        limit: u64,
    },
    /// The sender used up their submission limits; a later try may pass.
    #[error("too many messages, try again in {} seconds", .retry_after.as_secs())]
    RateLimited {
        /// When to try again; at least one second.
        retry_after: Duration,
    },
    /// The message or the request cannot be sent as given.
    #[error("invalid: {0}")]
    Invalid(String),
    /// No such submission for this sender.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// The message already left the queue.
    #[error("the message can no longer be undone")]
    TooLate(SubmissionState),
    /// The queue could not be reached; a later try may pass.
    #[error("submission is unavailable: {0}")]
    Unavailable(String),
}

/// Takes messages in and reports on them. Safe to share between tasks.
#[async_trait]
pub trait Submission: Send + Sync {
    /// The identities `actor` may send as in `archive` right now, by address. Empty when any
    /// lock is closed for them.
    async fn identities(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<SendIdentity>, SubmissionError>;

    /// What `actor`'s messages in `archive` must fit. Answers for someone who may not send too.
    async fn limits(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<SubmissionLimits, SubmissionError>;

    /// The identity for the return path `address`: an active identity of the actor in `archive`
    /// whose address is `address` or has it as a plus address, ignoring letter case, with every
    /// lock open. [`SubmissionError::Forbidden`] otherwise, also for the null return path (empty);
    /// [`SubmissionError::RateLimited`] when the sender's limits are used up.
    async fn sender(
        &self,
        actor: &Actor,
        archive: ArchiveId,
        address: &str,
    ) -> Result<SendIdentity, SubmissionError>;

    /// Takes the message into the queue.
    ///
    /// A repeated idempotency key returns the first submission at once, whatever changed since,
    /// and counts once against the limits. Otherwise, in this order: the checks of
    /// [`Submission::sender`] with the identity named; the header `From`; the shape of the
    /// message and the recipients; the size; the leak guard unless `allow_leaks`; and the
    /// transport's own rules. A refused message leaves nothing behind. Repeating a `From` or
    /// `Sender` field is refused like a wrong one, whichever field is the identity's.
    ///
    /// When [`SubmissionError::Unavailable`] answers a request with an idempotency key, the same
    /// request may be repeated: it picks up where this one stopped, and until then nothing leaves.
    ///
    /// Limits apply to device submissions only; a person in the web app is not counted. The
    /// message leaves no earlier than `send_at` and the end of its undo window.
    async fn queue(&self, request: Submit) -> Result<Submitted, SubmissionError>;

    /// Undoes a message that has not left: it becomes [`SubmissionState::Cancelled`] and never
    /// leaves. Undoing it again returns it unchanged. [`SubmissionError::TooLate`] once it left
    /// the queue, [`SubmissionError::NotFound`] unless `actor`'s user submitted it.
    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError>;

    /// Lets a held message go: it leaves once `undo_window` (`None` for the instance's default)
    /// has passed from now. A message that is not held (queued, cancelled or gone) is returned
    /// unchanged, so releasing twice is harmless. [`SubmissionError::NotFound`] unless `actor`'s
    /// user submitted it.
    async fn release(
        &self,
        actor: &Actor,
        id: OutboxId,
        undo_window: Option<Duration>,
    ) -> Result<Submitted, SubmissionError>;

    /// The message's state and what became of each recipient. [`SubmissionError::NotFound`]
    /// unless `actor`'s user submitted it.
    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError>;
}
