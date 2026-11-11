use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use holtmail_core::{Timestamp, WebhookId};
use holtmail_hooks::{
    DELIVERY_HEADER, EVENT_HEADER, Event, SIGNATURE_HEADER, delivered, new_secret, payload,
    retry_after, sign,
};
use holtmail_secret::SecretBox;
use holtmail_store::{NewDelivery, Store, Webhook};
use holtmail_webhook::{WebhookError, WebhookRequest, WebhookSender};
use tokio::sync::Notify;

const POLL: Duration = Duration::from_secs(5);
const DUE_PAGE: u32 = 50;
const KEEP_DELIVERIES: u32 = 200;

/// Webhooks: events are written to the store when they happen and delivered by
/// [`Hooks::run`], so an event from the command line goes out once `serve` runs.
pub struct Hooks {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
    sender: Arc<dyn WebhookSender>,
    wake: Notify,
}

fn context(id: WebhookId) -> String {
    format!("webhook:{id}")
}

/// Checks a webhook's address and events; see [`holtmail_hooks::check`].
pub fn check(url: &str, events: &[String]) -> anyhow::Result<(String, Vec<String>)> {
    holtmail_hooks::check(url, events).map_err(|reason| anyhow::anyhow!(reason))
}

impl Hooks {
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretBox>,
        sender: Arc<dyn WebhookSender>,
    ) -> Self {
        Self {
            store,
            secrets,
            sender,
            wake: Notify::new(),
        }
    }

    /// Adds a webhook and returns it with its signing secret, which is shown only now.
    pub async fn create(&self, url: &str, events: &[String]) -> anyhow::Result<(Webhook, String)> {
        let (url, events) = check(url, events)?;
        let id = WebhookId::new();
        let secret = new_secret();
        let hook = Webhook {
            id,
            url,
            events,
            sealed_secret: self.secrets.seal(&context(id), secret.as_bytes())?,
            enabled: true,
            created_at: Timestamp::now(),
        };
        self.store.create_webhook(&hook).await?;
        Ok((hook, secret))
    }

    /// Gives a webhook a new secret and returns it; the old one stops working at once.
    pub async fn rotate(&self, id: WebhookId) -> anyhow::Result<String> {
        let mut hook = self
            .store
            .get_webhook(id)
            .await?
            .context("There is no such webhook.")?;
        let secret = new_secret();
        hook.sealed_secret = self.secrets.seal(&context(id), secret.as_bytes())?;
        self.store.update_webhook(&hook).await?;
        Ok(secret)
    }

    /// Queues a `webhook.test` delivery for one webhook, enabled or not.
    pub async fn test(&self, id: WebhookId) -> anyhow::Result<()> {
        self.store
            .get_webhook(id)
            .await?
            .context("There is no such webhook.")?;
        self.enqueue(&Event::Test {}, &[id]).await
    }

    /// Queues `event` for every enabled webhook that wants it. Failures are logged, never
    /// returned: telling webhooks must not break the work that happened.
    pub async fn emit(&self, event: Event) {
        let wanted = match self.store.list_webhooks().await {
            Ok(hooks) => hooks
                .into_iter()
                .filter(|h| h.enabled && h.events.iter().any(|e| e == event.name()))
                .map(|h| h.id)
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::warn!(%error, event = event.name(), "webhooks not read");
                return;
            }
        };
        if wanted.is_empty() {
            return;
        }
        if let Err(error) = self.enqueue(&event, &wanted).await {
            tracing::warn!(%error, event = event.name(), "webhook deliveries not queued");
        }
    }

    /// The webhook `id` when rules may call it: it exists and subscribed to `flow.run`, so only
    /// those who manage webhooks decide which endpoints rules reach.
    pub async fn rule_target(&self, id: WebhookId) -> anyhow::Result<holtmail_store::Webhook> {
        let hook = self
            .store
            .get_webhook(id)
            .await?
            .context("There is no such webhook.")?;
        anyhow::ensure!(
            hook.events.iter().any(|e| e == "flow.run"),
            "The webhook is not subscribed to flow.run."
        );
        Ok(hook)
    }

    /// Queues `event` for the one webhook `id` if rules may call it ([`Hooks::rule_target`]) and
    /// it is enabled.
    pub async fn send_to(&self, id: WebhookId, event: Event) -> anyhow::Result<()> {
        let hook = self.rule_target(id).await?;
        anyhow::ensure!(hook.enabled, "The webhook is turned off.");
        self.enqueue(&event, &[id]).await
    }

    async fn enqueue(&self, event: &Event, hooks: &[WebhookId]) -> anyhow::Result<()> {
        let id = uuid::Uuid::now_v7().to_string();
        let at = Timestamp::now();
        let body = payload(&id, at, event);
        let deliveries: Vec<NewDelivery> = hooks
            .iter()
            .map(|hook| NewDelivery {
                webhook_id: *hook,
                event: event.name().to_owned(),
                event_id: id.clone(),
                payload: body.clone(),
                at,
            })
            .collect();
        self.store.enqueue_deliveries(&deliveries).await?;
        self.wake.notify_one();
        Ok(())
    }

    /// Delivers what is due, then waits for new events or the next retry; runs until the
    /// process ends.
    pub async fn run(self: Arc<Self>) {
        let mut rounds = 0u64;
        loop {
            match self.deliver_due().await {
                Ok(n) if n as u32 >= DUE_PAGE => continue,
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "webhook deliveries failed"),
            }
            rounds += 1;
            if rounds.is_multiple_of(720)
                && let Err(error) = self.store.prune_deliveries(KEEP_DELIVERIES).await
            {
                tracing::warn!(%error, "old webhook deliveries not pruned");
            }
            let _ = tokio::time::timeout(POLL, self.wake.notified()).await;
        }
    }

    /// Makes one attempt at every delivery due now, oldest first. Returns how many.
    pub async fn deliver_due(&self) -> anyhow::Result<usize> {
        self.deliver_due_at(Timestamp::now()).await
    }

    async fn deliver_due_at(&self, now: Timestamp) -> anyhow::Result<usize> {
        let due = self.store.due_deliveries(now, DUE_PAGE).await?;
        let mut hooks: HashMap<WebhookId, Option<Webhook>> = HashMap::new();
        for delivery in &due {
            let hook = match hooks.entry(delivery.webhook_id) {
                std::collections::hash_map::Entry::Occupied(known) => known.get().clone(),
                std::collections::hash_map::Entry::Vacant(slot) => slot
                    .insert(self.store.get_webhook(delivery.webhook_id).await?)
                    .clone(),
            };
            let Some(hook) = hook else {
                continue;
            };
            let secret = self.secrets.open(&context(hook.id), &hook.sealed_secret)?;
            let request = WebhookRequest {
                url: hook.url.clone(),
                headers: vec![
                    (EVENT_HEADER.into(), delivery.event.clone()),
                    (DELIVERY_HEADER.into(), delivery.event_id.clone()),
                    (
                        SIGNATURE_HEADER.into(),
                        sign(&secret, now.as_millis() / 1000, &delivery.payload),
                    ),
                ],
                body: Bytes::from(delivery.payload.clone()),
            };
            let attempts = delivery.attempts + 1;
            let (status, error) = match self.sender.send(&request).await {
                Ok(status) => (Some(status), None),
                Err(WebhookError::TimedOut) => (None, Some("no answer in time".to_owned())),
                Err(error) => (None, Some(error.to_string())),
            };
            let ok = status.is_some_and(delivered);
            let next = if ok {
                None
            } else {
                retry_after(attempts)
                    .map(|wait| Timestamp::from_millis(now.as_millis() + wait.as_millis() as i64))
            };
            tracing::info!(
                webhook = %hook.id,
                delivery = delivery.id,
                event = delivery.event.as_str(),
                status = status.unwrap_or_default(),
                attempts,
                delivered = ok,
                "webhook"
            );
            self.store
                .record_attempt(
                    delivery.id,
                    status,
                    error.as_deref(),
                    next,
                    ok.then_some(now),
                )
                .await?;
        }
        Ok(due.len())
    }
}

#[cfg(test)]
mod tests;
