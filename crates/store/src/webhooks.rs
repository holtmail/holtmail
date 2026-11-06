use async_trait::async_trait;
use holtmail_core::{Timestamp, WebhookId};

use crate::StoreError;

/// An address the server tells about events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Webhook {
    /// Id, chosen by the caller so the secret can be sealed for it before storing.
    pub id: WebhookId,
    /// An `http` or `https` address.
    pub url: String,
    /// Event names it wants, sorted; `webhook.test` is sent whatever they are.
    pub events: Vec<String>,
    /// The signing secret, sealed by the caller.
    pub sealed_secret: Vec<u8>,
    /// Off stops new deliveries; waiting ones still go.
    pub enabled: bool,
    /// When it was added.
    pub created_at: Timestamp,
}

/// A delivery to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDelivery {
    /// The webhook.
    pub webhook_id: WebhookId,
    /// The event's name.
    pub event: String,
    /// The event's id, the same for every webhook told about it.
    pub event_id: String,
    /// The body to send.
    pub payload: Vec<u8>,
    /// When the event happened; the first attempt is due then.
    pub at: Timestamp,
}

/// A delivery with its attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDelivery {
    /// Increasing in the order deliveries were added.
    pub id: u64,
    /// The webhook.
    pub webhook_id: WebhookId,
    /// The event's name.
    pub event: String,
    /// The event's id.
    pub event_id: String,
    /// The body to send.
    pub payload: Vec<u8>,
    /// When the event happened.
    pub created_at: Timestamp,
    /// Attempts made.
    pub attempts: u32,
    /// When the next attempt is due; `None` once delivered or given up.
    pub next_at: Option<Timestamp>,
    /// The status of the last answer.
    pub last_status: Option<u16>,
    /// Why the last attempt got no answer, in words without content.
    pub last_error: Option<String>,
    /// When it was delivered.
    pub delivered_at: Option<Timestamp>,
}

/// Webhooks and their deliveries, which survive restarts.
#[async_trait]
pub trait WebhookStore: Send + Sync {
    /// Adds a webhook. `Invalid` when the id is taken.
    async fn create_webhook(&self, webhook: &Webhook) -> Result<(), StoreError>;

    /// Every webhook, oldest first.
    async fn list_webhooks(&self) -> Result<Vec<Webhook>, StoreError>;

    /// The webhook, or `None`.
    async fn get_webhook(&self, id: WebhookId) -> Result<Option<Webhook>, StoreError>;

    /// Replaces a webhook's address, events, secret and switch. `NotFound` for an unknown id.
    async fn update_webhook(&self, webhook: &Webhook) -> Result<(), StoreError>;

    /// Removes a webhook with all its deliveries. `NotFound` for an unknown id.
    async fn delete_webhook(&self, id: WebhookId) -> Result<(), StoreError>;

    /// Adds deliveries, all due at their event's time, in the order given. Deliveries for an
    /// unknown webhook are `NotFound` and nothing is added.
    async fn enqueue_deliveries(&self, deliveries: &[NewDelivery]) -> Result<(), StoreError>;

    /// Up to `limit` deliveries due at or before `now`, in the order they were added.
    async fn due_deliveries(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<WebhookDelivery>, StoreError>;

    /// Records one attempt: counts it, keeps `status` or `error`, and sets when the next is due
    /// (`None` when delivered or given up) and when it was delivered. Unknown ids are ignored.
    async fn record_attempt(
        &self,
        id: u64,
        status: Option<u16>,
        error: Option<&str>,
        next_at: Option<Timestamp>,
        delivered_at: Option<Timestamp>,
    ) -> Result<(), StoreError>;

    /// The webhook's newest deliveries first, up to `limit`.
    async fn recent_deliveries(
        &self,
        webhook: WebhookId,
        limit: u32,
    ) -> Result<Vec<WebhookDelivery>, StoreError>;

    /// Keeps the newest `keep` finished deliveries per webhook and removes older finished ones;
    /// waiting ones stay. Returns how many were removed.
    async fn prune_deliveries(&self, keep: u32) -> Result<u64, StoreError>;
}
