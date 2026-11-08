use std::time::Duration;

use holtmail_core::{OutboxId, Timestamp, UserId};
use holtmail_store::{NewOutboxEntry, OutboxChange, OutboxEntry, OutboxState, RecipientState};

use crate::{Outbox, OutboxError};

impl Outbox {
    async fn entry(&self, id: OutboxId) -> Result<OutboxEntry, OutboxError> {
        self.store
            .get_outbox(id)
            .await?
            .ok_or(OutboxError::NotFound("outbox message"))
    }

    async fn change(
        &self,
        id: OutboxId,
        from: &[OutboxState],
        change: OutboxChange,
    ) -> Result<OutboxEntry, OutboxError> {
        match self.store.change_outbox(id, from, &change).await? {
            Some(entry) => {
                tracing::info!(outbox = %id, state = entry.state.as_str(), "outbox message changed");
                self.wake.notify_one();
                Ok(entry)
            }
            None => Err(OutboxError::State(self.entry(id).await?.state)),
        }
    }

    /// Takes a queued message back before it is sent and holds it as
    /// [`OutboxState::Draft`]. Only from [`OutboxState::Queued`]; a message already handed to
    /// the transport cannot be undone.
    pub async fn undo(&self, id: OutboxId) -> Result<OutboxEntry, OutboxError> {
        self.change(
            id,
            &[OutboxState::Queued],
            OutboxChange::to(OutboxState::Draft, self.now()),
        )
        .await
    }

    /// Queues a held message, due at `send_at` but not before a new undo window ends. Only from
    /// [`OutboxState::Draft`].
    pub async fn release(
        &self,
        id: OutboxId,
        send_at: Option<Timestamp>,
    ) -> Result<OutboxEntry, OutboxError> {
        self.release_within(id, send_at, self.config.undo_window)
            .await
    }

    /// [`Outbox::release`] with this undo window instead of the configured one.
    pub async fn release_within(
        &self,
        id: OutboxId,
        send_at: Option<Timestamp>,
        window: Duration,
    ) -> Result<OutboxEntry, OutboxError> {
        let earliest = self.later(window);
        let change = OutboxChange {
            send_at: Some(send_at.map_or(earliest, |at| at.max(earliest))),
            ..OutboxChange::to(OutboxState::Queued, self.now())
        };
        self.change(id, &[OutboxState::Draft], change).await
    }

    /// Cancels a message that was not handed over yet: from held, queued or retrying.
    pub async fn cancel(&self, id: OutboxId) -> Result<OutboxEntry, OutboxError> {
        self.change(
            id,
            &[
                OutboxState::Draft,
                OutboxState::Queued,
                OutboxState::Retrying,
            ],
            OutboxChange::to(OutboxState::Cancelled, self.now()),
        )
        .await
    }

    /// Tries the same message again now, to every recipient that has not accepted it: from
    /// retrying, failed, or uncertain (when a person decided to risk a second copy). A new
    /// retry window starts now. The identity checks of [`Outbox::queue`] apply again for `user`,
    /// the one asking (`None` for the server itself).
    pub async fn retry(
        &self,
        id: OutboxId,
        user: Option<UserId>,
    ) -> Result<OutboxEntry, OutboxError> {
        let original = self.entry(id).await?;
        let identity = self
            .store
            .get_identity(original.new.identity_id)
            .await?
            .ok_or(OutboxError::NotFound("identity"))?;
        if identity.fields.transport_id != original.new.transport_id {
            return Err(OutboxError::Invalid(
                "the identity now uses another transport".into(),
            ));
        }
        self.usable(&identity, original.new.archive_id, user)
            .await?;
        let now = self.now();
        let change = OutboxChange {
            send_at: Some(now),
            ..OutboxChange::to(OutboxState::Queued, now)
        };
        self.change(
            id,
            &[
                OutboxState::Retrying,
                OutboxState::Failed,
                OutboxState::Uncertain,
            ],
            change,
        )
        .await
    }

    /// Queues the same bytes again as a new outbox message, to `recipients` or, without them,
    /// to the recipients that refused it (every recipient when none accepted). Only after the
    /// message left the queue: sent, failed, uncertain, bounced or cancelled. The recipients
    /// must be among the original envelope's, and the identity checks of
    /// [`Outbox::queue`] apply again for `user`.
    pub async fn resend(
        &self,
        id: OutboxId,
        user: Option<UserId>,
        recipients: Option<Vec<String>>,
    ) -> Result<OutboxEntry, OutboxError> {
        let original = self.entry(id).await?;
        if !matches!(
            original.state,
            OutboxState::Sent
                | OutboxState::Failed
                | OutboxState::Uncertain
                | OutboxState::Bounced
                | OutboxState::Cancelled
        ) {
            return Err(OutboxError::State(original.state));
        }
        let recipients = match recipients {
            Some(list) => {
                if let Some(stranger) = list.iter().position(|r| {
                    !original
                        .recipients
                        .iter()
                        .any(|o| o.address.eq_ignore_ascii_case(r))
                }) {
                    return Err(OutboxError::Invalid(format!(
                        "recipient {} was not in the original envelope",
                        stranger + 1
                    )));
                }
                list
            }
            None => {
                let refused: Vec<String> = original
                    .recipients
                    .iter()
                    .filter(|r| matches!(r.state, RecipientState::Refused { .. }))
                    .map(|r| r.address.clone())
                    .collect();
                let none_accepted = original
                    .recipients
                    .iter()
                    .all(|r| r.state != RecipientState::Accepted);
                if none_accepted {
                    original.new.recipients.clone()
                } else {
                    refused
                }
            }
        };
        if recipients.is_empty() {
            return Err(OutboxError::Invalid("there is no one to send to".into()));
        }
        let identity = self
            .store
            .get_identity(original.new.identity_id)
            .await?
            .ok_or(OutboxError::NotFound("identity"))?;
        if identity.fields.transport_id != original.new.transport_id {
            return Err(OutboxError::Invalid(
                "the identity now uses another transport".into(),
            ));
        }
        self.usable(&identity, original.new.archive_id, user)
            .await?;
        let now = self.now();
        let (entry, _) = self
            .store
            .enqueue_outbox(&NewOutboxEntry {
                user_id: user,
                device_id: None,
                recipients,
                send_at: self.later(self.config.undo_window),
                state: OutboxState::Queued,
                flow_run: None,
                idempotency_key: None,
                resend_of: Some(original.id),
                at: now,
                ..original.new.clone()
            })
            .await?;
        tracing::info!(outbox = %entry.id, resend_of = %original.id, "message queued again");
        self.wake.notify_one();
        Ok(entry)
    }
}
