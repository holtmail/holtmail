use std::time::Duration;

use holtmail_core::{Timestamp, TransportId};
use holtmail_store::{
    AuditKind, NewAuditEntry, OutboxChange, OutboxEntry, OutboxRecipient, OutboxState,
    RecipientState, Transport,
};
use holtmail_submit::{Envelope, RecipientOutcome, SubmitError, SubmitReport};

use holtmail_route::Route;

use crate::{Outbox, OutboxError, SENT_BY_HOLTMAIL, check_message};

/// Where an attempt failed: before anything reached the transport, or after it may have.
enum Failure {
    Before(OutboxError),
    After(OutboxError),
}

const HOUR_MS: i64 = 3600 * 1000;

impl Outbox {
    /// Makes one attempt at the earliest due message of every transport that has one, at most
    /// one message in flight per transport, transports in parallel. A disabled transport, or one
    /// that reached its hourly limit, is skipped and its messages wait. Returns how many
    /// messages were attempted.
    pub async fn send_due(&self) -> Result<usize, OutboxError> {
        let now = self.now();
        let due = self.store.due_transports(now).await?;
        let attempts = due
            .into_iter()
            .map(|transport| self.send_one(transport, now));
        let results = futures::future::join_all(attempts).await;
        let mut sent = 0;
        for result in results {
            match result {
                Ok(true) => sent += 1,
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "outbox attempt failed"),
            }
        }
        Ok(sent)
    }

    async fn send_one(
        &self,
        transport_id: TransportId,
        now: Timestamp,
    ) -> Result<bool, OutboxError> {
        let transport = self.store.get_transport(transport_id).await?;
        if let Some(transport) = &transport {
            if !transport.enabled {
                return Ok(false);
            }
            if let Some(limit) = transport.limits.per_hour {
                let since = Timestamp::from_millis(now.as_millis() - HOUR_MS + 1);
                if self
                    .store
                    .outbox_attempts_since(transport.id, since)
                    .await?
                    >= u64::from(limit)
                {
                    return Ok(false);
                }
            }
        }
        let Some(entry) = self.store.claim_outbox(transport_id, now).await? else {
            return Ok(false);
        };
        let claimed = entry.clone();
        match self.deliver(entry, transport).await {
            Ok(done) => {
                self.report_undelivered(&done).await;
                Ok(true)
            }
            Err(Failure::Before(error)) => {
                tracing::warn!(outbox = %claimed.id, %error, "attempt failed before the transport");
                let done = self
                    .retry_or_fail(&claimed, format!("not sent: {error}"), true)
                    .await?;
                self.report_undelivered(&done).await;
                Ok(true)
            }
            Err(Failure::After(error)) => {
                tracing::warn!(outbox = %claimed.id, %error, "outcome of a send not recorded");
                let change = OutboxChange {
                    error: Some(
                        "the transport was used but the outcome could not be recorded".into(),
                    ),
                    ..OutboxChange::to(OutboxState::Uncertain, self.now())
                };
                self.apply(&claimed, &change).await?;
                Err(error)
            }
        }
    }

    async fn deliver(
        &self,
        entry: OutboxEntry,
        transport: Option<Transport>,
    ) -> Result<OutboxEntry, Failure> {
        let Some(transport) = transport else {
            return self
                .fail(&entry, "the transport no longer exists")
                .await
                .map_err(Failure::Before);
        };
        let bytes = match self.blobs.get(&entry.new.blob_id).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                return self
                    .fail(&entry, "the message bytes are missing")
                    .await
                    .map_err(Failure::Before);
            }
            Err(error) => return Err(Failure::Before(error.into())),
        };
        if let Some(why) = self
            .still_allowed(&entry, &transport, &bytes)
            .await
            .map_err(Failure::Before)?
        {
            return self.fail(&entry, &why).await.map_err(Failure::Before);
        }
        let entry = if entry.message_id.is_none() {
            match self
                .archiver
                .archive(entry.new.archive_id, bytes.clone(), None, &[])
                .await
            {
                Ok(message) => self
                    .store
                    .change_outbox(
                        entry.id,
                        &[OutboxState::Sending],
                        &OutboxChange {
                            message_id: Some(message),
                            send_at: None,
                            ..OutboxChange::to(OutboxState::Sending, self.now())
                        },
                    )
                    .await
                    .map_err(|e| Failure::Before(e.into()))?
                    .ok_or(Failure::Before(OutboxError::State(OutboxState::Sending)))?,
                Err(error) => {
                    tracing::warn!(outbox = %entry.id, "sent bytes not archived");
                    return self
                        .retry_or_fail(&entry, format!("not archived: {error}"), true)
                        .await
                        .map_err(Failure::Before);
                }
            }
        } else {
            entry
        };
        let submitter = match self.transports.submitter(&transport).await {
            Ok(submitter) => submitter,
            Err(error) => {
                let retryable = !matches!(error, SubmitError::Invalid(_) | SubmitError::Auth(_));
                return self
                    .retry_or_fail(&entry, error.to_string(), retryable)
                    .await
                    .map_err(Failure::Before);
            }
        };
        let envelope = Envelope {
            mail_from: entry.new.mail_from.clone(),
            recipients: entry
                .recipients
                .iter()
                .filter(|r| r.state != RecipientState::Accepted)
                .map(|r| r.address.clone())
                .collect(),
        };
        self.record_attempt(&entry, &transport, envelope.recipients.len())
            .await
            .map_err(Failure::Before)?;
        match submitter.submit(&envelope, &bytes).await {
            Ok(report) => self
                .sent(&entry, &transport, bytes, &report)
                .await
                .map_err(Failure::After),
            Err(SubmitError::Uncertain(reason)) => {
                tracing::warn!(outbox = %entry.id, transport = %transport.id,
                    "connection lost after the message was sent");
                let change = OutboxChange {
                    error: Some(format!("the outcome is unknown: {reason}")),
                    ..OutboxChange::to(OutboxState::Uncertain, self.now())
                };
                self.apply(&entry, &change).await.map_err(Failure::After)
            }
            Err(error) => {
                let recipients = match &error {
                    SubmitError::RecipientsRefused(results) => {
                        Some(merge(&entry.recipients, results))
                    }
                    _ => None,
                };
                let retryable = error.is_retryable();
                tracing::info!(outbox = %entry.id, transport = %transport.id,
                    attempt = entry.attempts, retryable, "transport did not take the message");
                self.retry_or_fail_with(&entry, error.to_string(), retryable, recipients)
                    .await
                    .map_err(Failure::Before)
            }
        }
    }

    /// Runs the checks of [`Outbox::queue`] again for a message about to be handed over, since
    /// rights may have been taken away while it waited. `Some(reason)` when one no longer holds.
    async fn still_allowed(
        &self,
        entry: &OutboxEntry,
        transport: &Transport,
        bytes: &[u8],
    ) -> Result<Option<String>, OutboxError> {
        let Some(identity) = self.store.get_identity(entry.new.identity_id).await? else {
            return Ok(Some("the identity was removed".into()));
        };
        if identity.fields.transport_id != transport.id {
            return Ok(Some("the identity now uses another transport".into()));
        }
        match self
            .usable(&identity, entry.new.archive_id, entry.new.user_id)
            .await
        {
            Ok(_) => {}
            Err(OutboxError::Invalid(why)) => return Ok(Some(why)),
            Err(error) => return Err(error),
        }
        if let Some(user) = entry.new.user_id
            && !self.transports.may_send(entry.new.archive_id, user).await?
        {
            return Ok(Some(
                "the sender may no longer send from this archive".into(),
            ));
        }
        let Ok(route) = serde_json::from_value::<Route>(entry.new.route.clone()) else {
            return Ok(Some("the stored route cannot be read".into()));
        };
        let envelope = Envelope {
            mail_from: entry.new.mail_from.clone(),
            recipients: entry.new.recipients.clone(),
        };
        match check_message(transport, &route, &envelope, bytes) {
            Ok(_) => Ok(None),
            Err(OutboxError::Invalid(why)) => Ok(Some(why)),
            Err(error) => Err(error),
        }
    }

    /// Records the attempt in the audit log before anything is handed to the transport, so no
    /// message leaves unrecorded.
    async fn record_attempt(
        &self,
        entry: &OutboxEntry,
        transport: &Transport,
        recipients: usize,
    ) -> Result<(), OutboxError> {
        let identity = self.store.get_identity(entry.new.identity_id).await?;
        let mut targets = vec![entry.id.to_string()];
        targets.extend(entry.message_id.map(|m| m.to_string()));
        self.store
            .append_audit(&NewAuditEntry {
                at: self.now(),
                user: entry.new.user_id,
                kind: AuditKind::MailSend,
                archive: Some(entry.new.archive_id),
                account: identity
                    .and_then(|i| i.fields.account_id)
                    .or(transport.account_id),
                targets,
                count: recipients as u64,
            })
            .await?;
        Ok(())
    }

    async fn sent(
        &self,
        entry: &OutboxEntry,
        transport: &Transport,
        bytes: bytes::Bytes,
        report: &SubmitReport,
    ) -> Result<OutboxEntry, OutboxError> {
        let now = self.now();
        let recipients = merge(&entry.recipients, &report.recipients);
        let accepted = recipients
            .iter()
            .filter(|r| r.state == RecipientState::Accepted)
            .count();
        let change = OutboxChange {
            recipients: Some(recipients),
            sent_at: Some(now),
            ..OutboxChange::to(OutboxState::Sent, now)
        };
        let sent = self.apply(entry, &change).await?;
        tracing::info!(outbox = %sent.id, transport = %transport.id, accepted,
            refused = sent.recipients.len() - accepted, "message sent");
        match self.sent_folder(&sent, Some(transport)).await {
            Ok(Some(folder)) => {
                let flags = ["\\Seen".to_owned(), SENT_BY_HOLTMAIL.to_owned()];
                if let Err(error) = self
                    .archiver
                    .archive(sent.new.archive_id, bytes, Some(folder), &flags)
                    .await
                {
                    tracing::warn!(outbox = %sent.id, %error, "sent location not recorded");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(outbox = %sent.id, %error, "sent folder not found"),
        }
        self.save_sent_copy(&sent, transport).await;
        Ok(sent)
    }

    pub(crate) async fn apply(
        &self,
        entry: &OutboxEntry,
        change: &OutboxChange,
    ) -> Result<OutboxEntry, OutboxError> {
        self.store
            .change_outbox(entry.id, &[OutboxState::Sending], change)
            .await?
            .ok_or(OutboxError::State(OutboxState::Sending))
    }

    async fn fail(&self, entry: &OutboxEntry, why: &str) -> Result<OutboxEntry, OutboxError> {
        tracing::warn!(outbox = %entry.id, reason = why, "message failed");
        let change = OutboxChange {
            error: Some(why.to_owned()),
            ..OutboxChange::to(OutboxState::Failed, self.now())
        };
        self.apply(entry, &change).await
    }

    async fn retry_or_fail(
        &self,
        entry: &OutboxEntry,
        error: String,
        retryable: bool,
    ) -> Result<OutboxEntry, OutboxError> {
        self.retry_or_fail_with(entry, error, retryable, None).await
    }

    async fn retry_or_fail_with(
        &self,
        entry: &OutboxEntry,
        error: String,
        retryable: bool,
        recipients: Option<Vec<OutboxRecipient>>,
    ) -> Result<OutboxEntry, OutboxError> {
        let now = self.now();
        let next = Timestamp::from_millis(now.as_millis() + millis(self.backoff(entry.attempts)));
        let deadline = entry.new.send_at.as_millis() + millis(self.config.retry_for);
        let change = if retryable && next.as_millis() <= deadline {
            OutboxChange {
                next_attempt_at: Some(next),
                error: Some(error),
                recipients,
                ..OutboxChange::to(OutboxState::Retrying, now)
            }
        } else {
            OutboxChange {
                error: Some(error),
                recipients,
                ..OutboxChange::to(OutboxState::Failed, now)
            }
        };
        self.apply(entry, &change).await
    }

    pub(crate) fn backoff(&self, attempts: u32) -> Duration {
        let doubled = self
            .config
            .first_retry
            .saturating_mul(2u32.saturating_pow(attempts.saturating_sub(1)));
        doubled.min(self.config.max_retry)
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn merge(
    current: &[OutboxRecipient],
    results: &[holtmail_submit::RecipientResult],
) -> Vec<OutboxRecipient> {
    current
        .iter()
        .map(|recipient| {
            let result = results
                .iter()
                .find(|r| r.address.eq_ignore_ascii_case(&recipient.address));
            let state = match result.map(|r| &r.outcome) {
                Some(RecipientOutcome::Accepted) => RecipientState::Accepted,
                Some(RecipientOutcome::Refused(refusal)) => RecipientState::Refused {
                    permanent: refusal.permanent,
                    code: refusal.code,
                    text: refusal.text.clone(),
                },
                None => recipient.state.clone(),
            };
            OutboxRecipient {
                address: recipient.address.clone(),
                state,
            }
        })
        .collect()
}
