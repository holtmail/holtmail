//! The outbox: a durable queue between "send" and a submission server.
//!
//! Sending is never fire-and-forget. [`Outbox::queue`] stores the exact bytes as a blob and
//! puts a row in the store; a worker ([`Outbox::run`]) hands due rows to their transport, one at
//! a time per transport, and records what became of every recipient. Delivery is at most once:
//! a send whose outcome is unknown becomes [`OutboxState::Uncertain`] and only a person sends it
//! again.

mod deliver;
mod dsn;
mod observe;
mod operations;
mod sent_copy;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::{BlobError, BlobStore};
use holtmail_core::{ArchiveId, DeviceId, FolderId, MessageId, OutboxId, Timestamp, UserId};
use holtmail_route::Route;
use holtmail_store::{
    AuditKind, FolderRole, Identity, NewAuditEntry, NewOutboxEntry, OutboxEntry, OutboxState,
    Store, StoreError, Transport,
};
use holtmail_submit::{Envelope, SubmitError, Submitter};
use tokio::sync::Notify;

pub use observe::{Arrival, Observed};

/// The keyword on the Sent location of a message Holtmail sent, until sync reads the server's
/// own flags for it.
pub const SENT_BY_HOLTMAIL: &str = "$HoltmailSent";

/// Puts bytes into an archive as a message, the way sync does.
#[async_trait]
pub trait Archiver: Send + Sync {
    /// Archives `raw` in `archive` and returns the message. Idempotent per bytes: archiving the
    /// same bytes again returns the same message. With `folder`, the message is also located in
    /// that folder, without a UID, with `flags`.
    async fn archive(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        folder: Option<FolderId>,
        flags: &[String],
    ) -> Result<MessageId, OutboxError>;
}

/// Builds the submitter of a stored transport.
#[async_trait]
pub trait Transports: Send + Sync {
    /// A submitter for `transport`. [`SubmitError::Invalid`] or [`SubmitError::Auth`] when it
    /// cannot be built as configured (the message fails); any other error is tried again later,
    /// so a lock that is closed for a while (an instance switch) should answer with a retryable
    /// error.
    async fn submitter(&self, transport: &Transport) -> Result<Arc<dyn Submitter>, SubmitError>;

    /// Whether `user` may still send from `archive` by their grant. Asked again before every
    /// attempt of a message the user queued; `false` fails the message, an error tries again
    /// later.
    async fn may_send(&self, archive: ArchiveId, user: UserId) -> Result<bool, OutboxError>;
}

/// How the outbox times its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxConfig {
    /// How long a queued message waits before it may be sent, so it can be undone.
    pub undo_window: Duration,
    /// How long after its send time a message is retried before it fails.
    pub retry_for: Duration,
    /// The wait before the first retry; it doubles with every attempt.
    pub first_retry: Duration,
    /// The longest wait between two attempts.
    pub max_retry: Duration,
    /// How often the worker looks for due messages when nothing wakes it.
    pub poll: Duration,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            undo_window: Duration::from_secs(10),
            retry_for: Duration::from_secs(24 * 3600),
            first_retry: Duration::from_secs(60),
            max_retry: Duration::from_secs(3600),
            poll: Duration::from_secs(1),
        }
    }
}

/// Why an outbox operation failed.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    /// The message may not be queued as asked: the reason names a field, never a value.
    #[error("refused: {0}")]
    Invalid(String),
    /// A referenced record does not exist.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// The operation is not possible in the message's current state.
    #[error("not possible while {}", .0.as_str())]
    State(OutboxState),
    /// Archiving the bytes failed.
    #[error("archiving failed: {0}")]
    Archive(String),
    /// The store failed.
    #[error(transparent)]
    Store(StoreError),
    /// The blob store failed.
    #[error(transparent)]
    Blob(#[from] BlobError),
}

impl From<StoreError> for OutboxError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound(what) => Self::NotFound(what),
            other => Self::Store(other),
        }
    }
}

/// A message to put in the outbox.
#[derive(Debug, Clone)]
pub struct Queue {
    /// The archive it is sent from.
    pub archive: ArchiveId,
    /// Who sends it; `None` for the server itself, such as a rule.
    pub user: Option<UserId>,
    /// The device it was submitted from; `None` for the web app and the server itself.
    pub device: Option<DeviceId>,
    /// The route the router chose; its identity and transport are used.
    pub route: Route,
    /// The envelope, Bcc recipients included.
    pub envelope: Envelope,
    /// The exact bytes, without Bcc.
    pub bytes: Bytes,
    /// When to send, at the earliest; never before the undo window ends.
    pub send_at: Option<Timestamp>,
    /// Hold it as [`OutboxState::Draft`] until released.
    pub hold: bool,
    /// The rule run that queued it.
    pub flow_run: Option<String>,
    /// A key that makes queueing idempotent per user.
    pub idempotency_key: Option<String>,
}

/// The outbox over a store, a blob store, the transports and an archiver.
pub struct Outbox {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    transports: Arc<dyn Transports>,
    archiver: Arc<dyn Archiver>,
    config: OutboxConfig,
    clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    wake: Notify,
}

impl Outbox {
    /// An outbox with these parts and timings, on the system clock.
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        transports: Arc<dyn Transports>,
        archiver: Arc<dyn Archiver>,
        config: OutboxConfig,
    ) -> Self {
        Self {
            store,
            blobs,
            transports,
            archiver,
            config,
            clock: Arc::new(Timestamp::now),
            wake: Notify::new(),
        }
    }

    /// The same outbox on another clock, for tests.
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> Timestamp + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Its timings.
    pub fn config(&self) -> OutboxConfig {
        self.config
    }

    fn now(&self) -> Timestamp {
        (self.clock)()
    }

    fn later(&self, by: Duration) -> Timestamp {
        Timestamp::from_millis(
            self.now()
                .as_millis()
                .saturating_add(i64::try_from(by.as_millis()).unwrap_or(i64::MAX)),
        )
    }

    /// Puts a message in the outbox.
    ///
    /// Checks first, refusing with [`OutboxError::Invalid`]: the route's identity is active, in
    /// the archive, uses the route's transport, and the user may send as it; the transport is
    /// enabled and the archive may use it; the envelope and bytes pass
    /// [`Envelope::validate`]; the bytes have a `Message-ID`, no `Bcc`, and every `From`
    /// address, the route's `From` and a non-empty return path are allowed by the transport;
    /// size and recipient count are within its limits. With the identity's Bcc-to-self its own
    /// address is added to the envelope.
    ///
    /// Then the bytes are stored as a blob before any row refers to them, and the row is added
    /// as queued (or held), due at `send_at` but not before the undo window ends. Idempotent per
    /// user and idempotency key. Nothing is archived yet: the message is archived when it is
    /// first handed to the transport.
    pub async fn queue(&self, request: Queue) -> Result<OutboxEntry, OutboxError> {
        let identity_id = request
            .route
            .identity
            .parse()
            .map_err(|_| OutboxError::Invalid("the route's identity is not an id".into()))?;
        let identity = self
            .store
            .get_identity(identity_id)
            .await?
            .ok_or(OutboxError::NotFound("identity"))?;
        if request.route.transport != identity.fields.transport_id.to_string() {
            return Err(OutboxError::Invalid(
                "the route's transport is not the identity's".into(),
            ));
        }
        let transport = self
            .usable(&identity, request.archive, request.user)
            .await?;
        let mut envelope = request.envelope.clone();
        if identity.fields.bcc_self
            && !envelope
                .recipients
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&identity.fields.address))
        {
            envelope.recipients.push(identity.fields.address.clone());
        }
        let message_id = check_message(&transport, &request.route, &envelope, &request.bytes)?;
        let blob_id = self.blobs.put(request.bytes.clone()).await?;
        let now = self.now();
        let earliest = self.later(self.config.undo_window);
        let send_at = request.send_at.map_or(earliest, |at| at.max(earliest));
        let (entry, created) = self
            .store
            .enqueue_outbox(&NewOutboxEntry {
                archive_id: request.archive,
                identity_id: identity.id,
                transport_id: transport.id,
                user_id: request.user,
                device_id: request.device,
                route: serde_json::to_value(&request.route)
                    .map_err(|_| OutboxError::Invalid("the route cannot be stored".into()))?,
                mail_from: envelope.mail_from.clone(),
                recipients: envelope.recipients.clone(),
                blob_id,
                size: request.bytes.len() as u64,
                rfc_message_id: message_id,
                send_at,
                state: if request.hold {
                    OutboxState::Draft
                } else {
                    OutboxState::Queued
                },
                flow_run: request.flow_run.clone(),
                idempotency_key: request.idempotency_key.clone(),
                resend_of: None,
                at: now,
            })
            .await?;
        if created {
            tracing::info!(outbox = %entry.id, archive = %entry.new.archive_id,
                recipients = entry.recipients.len(), "message queued");
            self.wake.notify_one();
        }
        Ok(entry)
    }

    async fn usable(
        &self,
        identity: &Identity,
        archive: ArchiveId,
        user: Option<UserId>,
    ) -> Result<Transport, OutboxError> {
        if identity.fields.archive_id != archive {
            return Err(OutboxError::Invalid(
                "the identity is in another archive".into(),
            ));
        }
        if !identity.fields.active {
            return Err(OutboxError::Invalid("the identity is not active".into()));
        }
        if let Some(user) = user
            && !self
                .store
                .identity_users(identity.id)
                .await?
                .contains(&user)
        {
            return Err(OutboxError::Invalid(
                "the user may not send as this identity".into(),
            ));
        }
        let transport = self
            .store
            .archive_transports(archive)
            .await?
            .into_iter()
            .find(|t| t.id == identity.fields.transport_id)
            .ok_or_else(|| OutboxError::Invalid("the archive may not use the transport".into()))?;
        if !transport.enabled {
            return Err(OutboxError::Invalid("the transport is turned off".into()));
        }
        Ok(transport)
    }

    async fn sent_folder(
        &self,
        entry: &OutboxEntry,
        transport: Option<&Transport>,
    ) -> Result<Option<FolderId>, OutboxError> {
        let identity = self.store.get_identity(entry.new.identity_id).await?;
        let account = identity
            .and_then(|i| i.fields.account_id)
            .or_else(|| transport.and_then(|t| t.account_id));
        let Some(account) = account else {
            return Ok(None);
        };
        Ok(self
            .store
            .list_folders(entry.new.archive_id)
            .await?
            .into_iter()
            .find(|f| f.folder.account_id == account && f.folder.role == Some(FolderRole::Sent))
            .map(|f| f.folder.id))
    }

    /// Releases into the undo window every held message whose item change was recorded before
    /// a stopped process could release it, so an answer or invitation the archive shows as made
    /// still leaves. A held message with no recorded change stays held: its request failed.
    /// Call once at start. Returns the ids released.
    pub async fn recover_held(&self) -> Result<Vec<OutboxId>, OutboxError> {
        let mut released = Vec::new();
        for id in self.store.held_change_messages().await? {
            match self.release(id, None).await {
                Ok(entry) => {
                    tracing::warn!(outbox = %entry.id, "held message of a recorded change released");
                    let audited = self
                        .store
                        .append_audit(&NewAuditEntry {
                            at: self.now(),
                            user: entry.new.user_id,
                            kind: AuditKind::OutboxChanged,
                            archive: Some(entry.new.archive_id),
                            account: None,
                            targets: vec![entry.id.to_string(), "recovered".to_owned()],
                            count: 1,
                        })
                        .await;
                    if let Err(error) = audited {
                        tracing::warn!(outbox = %entry.id, %error, "recovery not audited");
                    }
                    released.push(entry.id);
                }
                Err(error) => tracing::warn!(outbox = %id, %error, "held message not released"),
            }
        }
        Ok(released)
    }

    /// Moves every message left sending by a stopped process to [`OutboxState::Uncertain`]:
    /// after a restart nobody knows whether the server took it. Call once at start, before
    /// [`Outbox::run`]. Returns the ids.
    pub async fn recover(&self) -> Result<Vec<OutboxId>, OutboxError> {
        let entries = self
            .store
            .interrupt_sending(self.now(), "interrupted while sending")
            .await?;
        for entry in &entries {
            tracing::warn!(outbox = %entry.id, "send interrupted, outcome unknown");
        }
        Ok(entries.into_iter().map(|e| e.id).collect())
    }

    /// Sends what is due, then waits for new messages or the next poll; runs until the process
    /// ends.
    pub async fn run(self: Arc<Self>) {
        loop {
            match self.send_due().await {
                Ok(0) => {}
                Ok(_) => continue,
                Err(error) => tracing::warn!(%error, "outbox pass failed"),
            }
            let _ = tokio::time::timeout(self.config.poll, self.wake.notified()).await;
        }
    }
}

pub(crate) fn check_message(
    transport: &Transport,
    route: &Route,
    envelope: &Envelope,
    bytes: &[u8],
) -> Result<String, OutboxError> {
    envelope
        .validate(bytes)
        .map_err(|e| OutboxError::Invalid(e.to_string()))?;
    let parsed = holtmail_mime::parse(bytes)
        .map_err(|_| OutboxError::Invalid("the message cannot be parsed".into()))?;
    let message_id = parsed
        .message_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| OutboxError::Invalid("the message has no Message-ID".into()))?;
    if !parsed.bcc.is_empty() {
        return Err(OutboxError::Invalid("the message has a Bcc header".into()));
    }
    let allowed = |address: &str| {
        transport
            .allowed_from
            .iter()
            .any(|p| holtmail_route::allows(p, address))
    };
    if parsed.from.is_empty() {
        return Err(OutboxError::Invalid("the message has no From".into()));
    }
    if parsed.field_count("From") > 1 || parsed.field_count("Sender") > 1 {
        return Err(OutboxError::Invalid(
            "the message repeats a From or Sender field".into(),
        ));
    }
    if !parsed
        .from
        .iter()
        .any(|a| a.email.eq_ignore_ascii_case(&route.from))
    {
        return Err(OutboxError::Invalid(
            "the message's From is not the route's".into(),
        ));
    }
    if !allowed(&route.from) || parsed.from.iter().any(|a| !allowed(&a.email)) {
        return Err(OutboxError::Invalid(
            "the transport may not send as the From address".into(),
        ));
    }
    if !envelope.mail_from.is_empty() && !allowed(&envelope.mail_from) {
        return Err(OutboxError::Invalid(
            "the transport may not use the return path".into(),
        ));
    }
    if let Some(max) = transport.limits.max_size
        && bytes.len() as u64 > max
    {
        return Err(OutboxError::Invalid(
            "the message is larger than the transport takes".into(),
        ));
    }
    if let Some(max) = transport.limits.max_recipients
        && envelope.recipients.len() > max as usize
    {
        return Err(OutboxError::Invalid(
            "the message has more recipients than the transport takes".into(),
        ));
    }
    Ok(message_id)
}

#[cfg(test)]
mod tests;
