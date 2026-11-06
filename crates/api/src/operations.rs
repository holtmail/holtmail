use async_trait::async_trait;
use holtmail_core::{ArchiveId, MessageId, Timestamp, UserId, WebhookId};
use holtmail_store::{LedgerEntry, LedgerHash, Message, Webhook};
use serde::{Deserialize, Serialize};

use crate::{MailboxError, Task};

/// Instance-wide work the API hands to the server: the archive ledger, deleting messages and
/// webhook secrets.
#[async_trait]
pub trait Operations: Send + Sync {
    /// The newest ledger entry and how the last verification ended.
    async fn ledger(&self) -> Result<LedgerStatus, MailboxError>;

    /// Starts verifying the ledger in the background and returns its task; returns the running
    /// verification instead when one is running. The result is kept for [`Operations::ledger`]
    /// and the task finishes `failed` when anything is wrong.
    async fn verify_ledger(&self, request: VerifyLedger) -> Result<Task, MailboxError>;

    /// Works out which messages of `archive` a deletion would remove, freezes that list and
    /// returns it for review. Nothing is deleted. The review can be confirmed once, by the user
    /// who asked for it, within [`DeletionReview::expires_at`]. `Invalid` for an unknown archive,
    /// a query that does not parse, nothing to delete, or more than [`MAX_DELETION`] messages.
    async fn review_deletion(
        &self,
        archive: ArchiveId,
        selection: DeletionSelection,
        actor: UserId,
    ) -> Result<DeletionReview, MailboxError>;

    /// Deletes the messages of a review in the background and returns the task: their bytes,
    /// attachments and everything derived from them, keeping tombstones so they are not archived
    /// again, and entering each in the ledger. `count` must repeat the review's count. `Invalid`
    /// for an unknown, used or expired review, another user's review, or another count.
    async fn confirm_deletion(
        &self,
        review: &str,
        count: u64,
        actor: UserId,
    ) -> Result<Task, MailboxError>;

    /// Adds a webhook for `events` and returns it with its signing secret, which is not shown
    /// again. `Invalid` for an address or event [`holtmail_hooks::check`] refuses.
    async fn create_webhook(
        &self,
        url: &str,
        events: &[String],
    ) -> Result<(Webhook, String), MailboxError>;

    /// Gives the webhook a new signing secret and returns it; the old one stops at once.
    /// `Invalid` for an unknown webhook.
    async fn rotate_webhook_secret(&self, id: WebhookId) -> Result<String, MailboxError>;

    /// Queues a `webhook.test` delivery for the webhook, on or off. `Invalid` for an unknown
    /// webhook.
    async fn test_webhook(&self, id: WebhookId) -> Result<(), MailboxError>;

    /// The SMTP listener as configured and what it took since the server started; `None` when
    /// the config has none.
    fn smtp(&self) -> Option<SmtpStatus>;
}

/// The SMTP listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpStatus {
    /// Where it listens.
    pub listen: String,
    /// The name in its greeting.
    pub hostname: String,
    /// Client networks it accepts.
    pub allow: Vec<String>,
    /// `STARTTLS` is offered.
    pub starttls: bool,
    /// Mail is refused before `STARTTLS`.
    pub require_tls: bool,
    /// Clients sign in with `AUTH`.
    pub auth: bool,
    /// The largest message in MiB.
    pub max_size_mb: u64,
    /// Where mail goes.
    pub routes: Vec<SmtpRoute>,
    /// Messages archived since the start.
    pub received: u64,
    /// Messages left out because they were deleted from the archive earlier.
    pub deleted: u64,
    /// Recipients refused.
    pub refused_recipients: u64,
    /// Messages refused because they could not be stored or read.
    pub failed: u64,
    /// When the last message came.
    pub last_received_at: Option<Timestamp>,
}

/// One SMTP route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpRoute {
    /// An address or `*@domain`.
    pub recipient: String,
    /// The archive.
    pub archive_id: ArchiveId,
    /// The folder of the archive's "Received by SMTP" account.
    pub folder: String,
    /// Exchange journal reports are unwrapped.
    pub journal: bool,
}

/// The most messages one deletion removes.
pub const MAX_DELETION: u64 = 100_000;

/// Which messages to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionSelection {
    /// Every message the search finds, as the search box reads it.
    Query(String),
    /// These messages; those not in the archive are left out.
    Messages(Vec<MessageId>),
}

/// A frozen list of messages waiting to be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionReview {
    /// Confirms this review.
    pub id: String,
    /// The archive.
    pub archive_id: ArchiveId,
    /// The search it came from.
    pub query: Option<String>,
    /// Messages it would delete.
    pub count: u64,
    /// Their size together, in bytes.
    pub bytes: u64,
    /// The oldest message's date.
    pub oldest: Option<Timestamp>,
    /// The newest message's date.
    pub newest: Option<Timestamp>,
    /// Up to 20 of them, newest first.
    pub sample: Vec<Message>,
    /// When it can no longer be confirmed.
    pub expires_at: Timestamp,
}

/// What a verification looks at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifyLedger {
    /// Also read every message's bytes and compare their hash with the id they are stored
    /// under; takes as long as reading the whole archive.
    pub blobs: bool,
    /// A position and hash written down earlier that the entry there must still have.
    pub anchor: Option<(u64, LedgerHash)>,
}

/// The ledger as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerStatus {
    /// The newest entry; `None` while nothing was archived.
    pub head: Option<LedgerEntry>,
    /// The last finished verification.
    pub last: Option<LedgerVerification>,
}

/// How a verification ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerVerification {
    /// When it started.
    pub started_at: Timestamp,
    /// When it ended.
    pub finished_at: Timestamp,
    /// Nothing was wrong.
    pub ok: bool,
    /// Entries checked.
    pub entries: u64,
    /// The newest entry checked.
    pub head_seq: Option<u64>,
    /// Its hash.
    pub head_hash: Option<LedgerHash>,
    /// The position of the anchor it was given, if any.
    pub anchor_seq: Option<u64>,
    /// Message bytes read and compared, when it was asked to.
    pub blobs_checked: Option<u64>,
    /// What was wrong, in words, at most 100 lines; ids and positions only.
    pub problems: Vec<String>,
    /// How many problems there were.
    pub problem_count: u64,
}
