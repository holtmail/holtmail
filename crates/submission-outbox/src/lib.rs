//! [`Submission`] over the outbox: every front end's send goes through the same five locks,
//! identity checks, per-user limits and leak guard, and into the same durable queue.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_access::{Access, AccessError, Scopes};
use holtmail_blob::BlobStore;
use holtmail_core::{ArchiveId, OutboxId, Timestamp};
use holtmail_mime::ParsedMessage;
use holtmail_outbox::{Outbox, OutboxError, Queue};
use holtmail_route::{Context, Recipients, Route, Routed, Strategy, Thread};
use holtmail_store::{
    AuditKind, Identity, NewAuditEntry, OutboxEntry, OutboxState, RecipientState, Store,
    StoreError, Transport,
};
use holtmail_submission::{
    Actor, DEFAULT_MAX_SIZE, RecipientDelivery, RecipientStatus, SendIdentity, Submission,
    SubmissionError, SubmissionLimits, SubmissionState, Submit, Submitted,
};
use holtmail_submit::Envelope;

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);
const THREAD_LIMIT: usize = 200;
const REFERENCE_LIMIT: usize = 50;

/// What every user's devices may submit, together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Messages per user in any hour.
    pub messages_per_hour: u32,
    /// Envelope recipients per user in any day.
    pub recipients_per_day: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            messages_per_hour: 200,
            recipients_per_day: 2000,
        }
    }
}

/// Takes messages into the outbox. Safe to share between tasks; every answer reflects the store
/// at the moment of the call.
pub struct OutboxSubmission {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    access: Access,
    outbox: Arc<Outbox>,
    options: Options,
}

fn unavailable(error: impl std::fmt::Display) -> SubmissionError {
    SubmissionError::Unavailable(error.to_string())
}

fn store_error(error: StoreError) -> SubmissionError {
    unavailable(error)
}

fn outbox_error(error: OutboxError) -> SubmissionError {
    match error {
        OutboxError::Invalid(reason) => SubmissionError::Invalid(reason),
        OutboxError::NotFound(what) => SubmissionError::NotFound(what),
        OutboxError::State(state) => SubmissionError::TooLate(state_of(state)),
        other => unavailable(other),
    }
}

fn state_of(state: OutboxState) -> SubmissionState {
    match state {
        OutboxState::Draft | OutboxState::Queued => SubmissionState::Queued,
        OutboxState::Sending | OutboxState::Retrying => SubmissionState::Sending,
        OutboxState::Sent => SubmissionState::Sent,
        OutboxState::Failed => SubmissionState::Failed,
        OutboxState::Uncertain => SubmissionState::Uncertain,
        OutboxState::Bounced => SubmissionState::Bounced,
        OutboxState::Cancelled => SubmissionState::Cancelled,
    }
}

fn submitted(entry: &OutboxEntry) -> Submitted {
    Submitted {
        id: entry.id,
        archive: entry.new.archive_id,
        identity: entry.new.identity_id,
        state: state_of(entry.state),
        send_at: entry.new.send_at,
        recipients: entry
            .recipients
            .iter()
            .map(|recipient| RecipientStatus {
                address: recipient.address.clone(),
                delivery: match &recipient.state {
                    RecipientState::Pending => RecipientDelivery::Pending,
                    RecipientState::Accepted => RecipientDelivery::Accepted,
                    RecipientState::Refused {
                        permanent,
                        code,
                        text,
                    } => RecipientDelivery::Refused {
                        permanent: *permanent,
                        code: *code,
                        text: text.clone(),
                    },
                    RecipientState::Bounced { status, text } => RecipientDelivery::Bounced {
                        status: status.clone(),
                        text: text.clone(),
                    },
                },
            })
            .collect(),
        rfc_message_id: entry.new.rfc_message_id.clone(),
    }
}

fn send_identity(identity: &Identity) -> SendIdentity {
    SendIdentity {
        id: identity.id,
        address: identity.fields.address.clone(),
        name: identity.fields.name.clone(),
    }
}

/// Whether `address` is the identity's address or, when it allows them, a plus address of it.
fn covers(identity: &Identity, address: &str) -> bool {
    let own = &identity.fields.address;
    if address.eq_ignore_ascii_case(own) {
        return true;
    }
    let (Some((local, domain)), true) = (address.rsplit_once('@'), identity.fields.plus_addresses)
    else {
        return false;
    };
    local.split_once('+').is_some_and(|(base, tag)| {
        !tag.is_empty() && format!("{base}@{domain}").eq_ignore_ascii_case(own)
    })
}

/// Whether the message may name `address` as its sender: the address of the route a front end
/// chose, which may be an alias of the identity, or else the identity's own.
fn sends_as(identity: &Identity, route: Option<&Route>, address: &str) -> bool {
    match route {
        Some(route) => address.eq_ignore_ascii_case(&route.from),
        None => covers(identity, address),
    }
}

fn sendable_from(transport: &Transport, address: &str) -> bool {
    transport
        .allowed_from
        .iter()
        .any(|pattern| holtmail_route::allows(pattern, address))
}

fn forbidden(reason: &str) -> SubmissionError {
    SubmissionError::Forbidden(reason.to_owned())
}

fn lower(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

fn wait_until(oldest: Option<Timestamp>, window: Duration, now: Timestamp) -> Duration {
    let ends = oldest.map_or(now.as_millis(), |at| {
        at.as_millis()
            .saturating_add(i64::try_from(window.as_millis()).unwrap_or(i64::MAX))
    });
    let left = ends.saturating_sub(now.as_millis()).max(1000);
    Duration::from_millis(u64::try_from(left).unwrap_or(1000))
}

impl OutboxSubmission {
    /// Submits over these parts, with these limits on devices.
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        access: Access,
        outbox: Arc<Outbox>,
        options: Options,
    ) -> Self {
        Self {
            store,
            blobs,
            access,
            outbox,
            options,
        }
    }

    /// The active identities of the actor in the archive whose transport they may use now.
    async fn usable(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<(Identity, Transport)>, SubmissionError> {
        let Some(user) = self.store.get_user(actor.user).await.map_err(store_error)? else {
            return Ok(Vec::new());
        };
        let scopes = Scopes::of_device(&actor.scopes);
        let identities = self
            .store
            .user_identities(user.id, archive)
            .await
            .map_err(store_error)?;
        let transports = self
            .store
            .archive_transports(archive)
            .await
            .map_err(store_error)?;
        let mut usable = Vec::new();
        for identity in identities.into_iter().filter(|i| i.fields.active) {
            let Some(transport) = transports
                .iter()
                .find(|t| t.id == identity.fields.transport_id && t.enabled)
            else {
                continue;
            };
            match self
                .access
                .may_send(&user, &scopes, archive, transport)
                .await
            {
                Ok(()) => usable.push((identity, transport.clone())),
                Err(AccessError::Store(error)) => return Err(store_error(error)),
                Err(_) => {}
            }
        }
        Ok(usable)
    }

    /// Refuses when the actor's device has used up its limits and `recipients` more would not
    /// fit. The web app is not limited.
    async fn check_limits(&self, actor: &Actor, recipients: usize) -> Result<(), SubmissionError> {
        if actor.device.is_none() {
            return Ok(());
        }
        let now = Timestamp::now();
        let since = |window: Duration| {
            Timestamp::from_millis(
                now.as_millis()
                    .saturating_sub(i64::try_from(window.as_millis()).unwrap_or(i64::MAX)),
            )
        };
        let hour = self
            .store
            .device_sends_since(actor.user, since(HOUR))
            .await
            .map_err(store_error)?;
        if hour.messages >= u64::from(self.options.messages_per_hour) {
            return Err(SubmissionError::RateLimited {
                retry_after: wait_until(hour.oldest, HOUR, now),
            });
        }
        let day = self
            .store
            .device_sends_since(actor.user, since(DAY))
            .await
            .map_err(store_error)?;
        if day.recipients + recipients as u64 > u64::from(self.options.recipients_per_day) {
            return Err(SubmissionError::RateLimited {
                retry_after: wait_until(day.oldest, DAY, now),
            });
        }
        Ok(())
    }

    /// What the thread `parsed` belongs to tells the leak guard, `None` when it answers nothing
    /// in this archive.
    async fn thread_of(
        &self,
        archive: ArchiveId,
        parsed: &ParsedMessage,
    ) -> Result<Option<Thread>, SubmissionError> {
        let mut ids: Vec<String> = parsed.in_reply_to.clone();
        ids.extend(
            parsed
                .references
                .iter()
                .rev()
                .take(REFERENCE_LIMIT)
                .cloned(),
        );
        let answered = self
            .store
            .messages_by_rfc_id(archive, &ids)
            .await
            .map_err(store_error)?;
        let Some(message) = answered.last() else {
            return Ok(None);
        };
        let mut members = self
            .store
            .thread_messages(message.thread_id)
            .await
            .map_err(store_error)?;
        if members.len() > THREAD_LIMIT {
            members.drain(..members.len() - THREAD_LIMIT);
        }
        let mut thread = Thread::default();
        for member in members {
            let Some(hit) = self
                .store
                .message_alias(member.id)
                .await
                .map_err(store_error)?
            else {
                continue;
            };
            let alias = lower(&hit.alias);
            if !thread.aliases.contains(&alias) {
                thread.aliases.push(alias);
            }
            if !hit.signed {
                continue;
            }
            thread
                .reverse_aliases
                .extend(hit.reverse_alias.as_deref().map(lower));
            let raw = self.blobs.get(&member.blob_id).await.map_err(unavailable)?;
            if let Some(raw) = raw
                && let Ok(original) = holtmail_mime::parse(&raw)
            {
                thread
                    .reverse_aliases
                    .extend(original.reply_to.first().map(|a| lower(&a.email)));
            }
        }
        Ok(Some(thread))
    }

    /// Whether a recipient of this message, sent from `route`, would see the real mailbox behind
    /// an alias its thread arrived through.
    async fn leaks(
        &self,
        request: &Submit,
        parsed: &ParsedMessage,
        route: &Route,
    ) -> Result<bool, SubmissionError> {
        let Some(mut thread) = self.thread_of(request.archive, parsed).await? else {
            return Ok(false);
        };
        let listed = |list: &[holtmail_core::Address], address: &str| {
            list.iter().any(|a| lower(&a.email) == address)
        };
        let (mut to, mut cc, mut bcc) = (Vec::new(), Vec::new(), Vec::new());
        for recipient in request.recipients.iter().map(|r| lower(r)) {
            let recorded = self
                .store
                .reverse_alias_contact(request.archive, &recipient)
                .await
                .map_err(store_error)?;
            if recorded.is_some() {
                thread.reverse_aliases.push(recipient.clone());
            }
            if listed(&parsed.to, &recipient) {
                to.push(recipient);
            } else if listed(&parsed.cc, &recipient) {
                cc.push(recipient);
            } else {
                bcc.push(recipient);
            }
        }
        let routed = Routed {
            route: route.clone(),
            to: None,
            alias: None,
        };
        let context = Context::Reply(holtmail_route::Original::default());
        let found = holtmail_route::leaks(
            &routed,
            &context,
            Recipients {
                to: &to,
                cc: &cc,
                bcc: &bcc,
            },
            &thread,
        );
        Ok(!found.is_empty())
    }

    async fn audit(
        &self,
        entry: &OutboxEntry,
        account: Option<holtmail_core::AccountId>,
        action: &str,
    ) -> Result<(), StoreError> {
        self.store
            .append_audit(&NewAuditEntry {
                at: Timestamp::now(),
                user: entry.new.user_id,
                kind: AuditKind::OutboxChanged,
                archive: Some(entry.new.archive_id),
                account,
                targets: vec![entry.id.to_string(), action.to_owned()],
                count: 2,
            })
            .await?;
        Ok(())
    }

    async fn mine(&self, actor: &Actor, id: OutboxId) -> Result<OutboxEntry, SubmissionError> {
        self.store
            .get_outbox(id)
            .await
            .map_err(store_error)?
            .filter(|entry| entry.new.user_id == Some(actor.user))
            .ok_or(SubmissionError::NotFound("submission"))
    }

    async fn named(&self, request: &Submit) -> Result<(Identity, Transport), SubmissionError> {
        let (identity, transport) = self
            .usable(&request.actor, request.archive)
            .await?
            .into_iter()
            .find(|(identity, _)| identity.id == request.identity)
            .ok_or_else(|| forbidden("no such identity for this sender"))?;
        if request.mail_from.is_empty() {
            return Err(forbidden("a null return path cannot be submitted"));
        }
        if request
            .route
            .as_ref()
            .is_some_and(|route| route.identity != identity.id.to_string())
        {
            return Err(forbidden("the route is not the identity's"));
        }
        if !sends_as(&identity, request.route.as_ref(), &request.mail_from) {
            return Err(forbidden("the return path is not the identity's address"));
        }
        if request.route.is_none() && !sendable_from(&transport, &request.mail_from) {
            return Err(forbidden("the transport may not use this return path"));
        }
        Ok((identity, transport))
    }
}

fn check_shape(
    request: &Submit,
    identity: &Identity,
    parsed: &ParsedMessage,
) -> Result<String, SubmissionError> {
    if parsed.from.len() != 1
        || parsed.field_count("From") != 1
        || !sends_as(identity, request.route.as_ref(), &parsed.from[0].email)
    {
        return Err(SubmissionError::FromMismatch);
    }
    if parsed.field_count("Sender") > 1 {
        return Err(SubmissionError::Invalid(
            "the message has more than one Sender".into(),
        ));
    }
    if !parsed.bcc.is_empty() {
        return Err(SubmissionError::Invalid(
            "the message has a Bcc header".into(),
        ));
    }
    if parsed.sent_at.is_none() {
        return Err(SubmissionError::Invalid("the message has no Date".into()));
    }
    let message_id = parsed
        .message_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| SubmissionError::Invalid("the message has no Message-ID".into()))?;
    if request.recipients.is_empty() {
        return Err(SubmissionError::Invalid("there are no recipients".into()));
    }
    Ok(message_id)
}

#[async_trait]
impl Submission for OutboxSubmission {
    async fn identities(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<SendIdentity>, SubmissionError> {
        let usable = self.usable(actor, archive).await?;
        Ok(usable
            .iter()
            .filter(|(identity, transport)| sendable_from(transport, &identity.fields.address))
            .map(|(identity, _)| send_identity(identity))
            .collect())
    }

    async fn limits(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<SubmissionLimits, SubmissionError> {
        let usable = self.usable(actor, archive).await?;
        let transports: Vec<&Transport> = usable
            .iter()
            .filter(|(identity, transport)| sendable_from(transport, &identity.fields.address))
            .map(|(_, transport)| transport)
            .collect();
        Ok(SubmissionLimits {
            max_size: transports
                .iter()
                .filter_map(|t| t.limits.max_size)
                .min()
                .unwrap_or(DEFAULT_MAX_SIZE),
            smtputf8: !transports.is_empty() && transports.iter().all(|t| t.smtputf8),
        })
    }

    async fn sender(
        &self,
        actor: &Actor,
        archive: ArchiveId,
        address: &str,
    ) -> Result<SendIdentity, SubmissionError> {
        if address.is_empty() {
            return Err(forbidden("a null return path cannot be submitted"));
        }
        let usable = self.usable(actor, archive).await?;
        let (identity, _) = usable
            .iter()
            .find(|(identity, transport)| {
                covers(identity, address) && sendable_from(transport, address)
            })
            .ok_or_else(|| forbidden("no such identity for this sender"))?;
        self.check_limits(actor, 1).await?;
        Ok(send_identity(identity))
    }

    async fn queue(&self, request: Submit) -> Result<Submitted, SubmissionError> {
        if let Some(key) = &request.idempotency_key {
            let first = self
                .store
                .outbox_by_idempotency_key(request.actor.user, key)
                .await
                .map_err(store_error)?;
            if let Some(first) = first.filter(|e| e.state != OutboxState::Draft) {
                return Ok(submitted(&first));
            }
        }
        let (identity, transport) = self.named(&request).await?;
        self.check_limits(&request.actor, request.recipients.len())
            .await?;
        let parsed = holtmail_mime::parse(&request.message)
            .map_err(|_| SubmissionError::Invalid("the message cannot be parsed".into()))?;
        check_shape(&request, &identity, &parsed)?;
        if let Some(limit) = transport.limits.max_size
            && request.message.len() as u64 > limit
        {
            return Err(SubmissionError::TooLarge { limit });
        }
        let route = match &request.route {
            Some(route) => route.clone(),
            None => {
                let route = Route {
                    identity: identity.id.to_string(),
                    from: parsed.from[0].email.clone(),
                    name: identity.fields.name.clone(),
                    transport: transport.id.to_string(),
                    strategy: Strategy::Direct,
                    reasons: Vec::new(),
                };
                if !request.allow_leaks && self.leaks(&request, &parsed, &route).await? {
                    return Err(SubmissionError::Leak);
                }
                route
            }
        };
        let window = request
            .undo_window
            .unwrap_or_else(|| self.outbox.config().undo_window);
        let entry = self
            .outbox
            .queue(Queue {
                archive: request.archive,
                user: Some(request.actor.user),
                device: request.actor.device,
                route,
                envelope: Envelope {
                    mail_from: request.mail_from.clone(),
                    recipients: request.recipients.clone(),
                },
                bytes: request.message.clone(),
                send_at: request.send_at,
                hold: true,
                flow_run: None,
                idempotency_key: request.idempotency_key.clone(),
            })
            .await
            .map_err(outbox_error)?;
        if entry.state != OutboxState::Draft {
            return Ok(submitted(&entry));
        }
        let action = if request.allow_leaks {
            "queued_despite_leaks"
        } else {
            "queued"
        };
        if let Err(error) = self.audit(&entry, transport.account_id, action).await {
            if request.idempotency_key.is_none() {
                let _ = self.outbox.cancel(entry.id).await;
            }
            return Err(store_error(error));
        }
        let released = self
            .outbox
            .release_within(entry.id, request.send_at, window)
            .await
            .map_err(outbox_error)?;
        Ok(submitted(&released))
    }

    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        let entry = self.mine(actor, id).await?;
        match entry.state {
            OutboxState::Cancelled => return Ok(submitted(&entry)),
            OutboxState::Queued | OutboxState::Draft => {}
            other => return Err(SubmissionError::TooLate(state_of(other))),
        }
        self.audit(&entry, None, "undone")
            .await
            .map_err(store_error)?;
        let cancelled = self.outbox.cancel(id).await.map_err(outbox_error)?;
        Ok(submitted(&cancelled))
    }

    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        Ok(submitted(&self.mine(actor, id).await?))
    }
}

#[cfg(test)]
mod tests;
