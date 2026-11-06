use async_trait::async_trait;
use holtmail_core::{Timestamp, WebhookId};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, parse, signed, unique, unsigned};

fn webhook(row: &SqliteRow) -> Result<Webhook, StoreError> {
    Ok(Webhook {
        id: parse(&get::<String>(row, "id")?)?,
        url: get(row, "url")?,
        events: serde_json::from_str(&get::<String>(row, "events")?)
            .map_err(|e| StoreError::Invalid(e.to_string()))?,
        sealed_secret: get(row, "sealed_secret")?,
        enabled: get(row, "enabled")?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
    })
}

fn delivery(row: &SqliteRow) -> Result<WebhookDelivery, StoreError> {
    let at = |column: &str| -> Result<Option<Timestamp>, StoreError> {
        Ok(get::<Option<i64>>(row, column)?.map(Timestamp::from_millis))
    };
    Ok(WebhookDelivery {
        id: unsigned(get(row, "id")?),
        webhook_id: parse(&get::<String>(row, "webhook_id")?)?,
        event: get(row, "event")?,
        event_id: get(row, "event_id")?,
        payload: get(row, "payload")?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        attempts: get::<i64>(row, "attempts")? as u32,
        next_at: at("next_at")?,
        last_status: get::<Option<i64>>(row, "last_status")?.map(|s| s as u16),
        last_error: get(row, "last_error")?,
        delivered_at: at("delivered_at")?,
    })
}

const DELIVERY: &str = "id, webhook_id, event, event_id, payload, created_at, attempts, next_at,
    last_status, last_error, delivered_at";

fn events(webhook: &Webhook) -> Result<String, StoreError> {
    let mut events = webhook.events.clone();
    events.sort();
    events.dedup();
    serde_json::to_string(&events).map_err(|e| StoreError::Invalid(e.to_string()))
}

#[async_trait]
impl WebhookStore for SqliteStore {
    async fn create_webhook(&self, hook: &Webhook) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO webhooks (id, url, events, sealed_secret, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(hook.id.to_string())
        .bind(&hook.url)
        .bind(events(hook)?)
        .bind(&hook.sealed_secret)
        .bind(hook.enabled)
        .bind(hook.created_at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(|e| unique(e, "the webhook id is taken"))?;
        Ok(())
    }

    async fn list_webhooks(&self) -> Result<Vec<Webhook>, StoreError> {
        sqlx::query("SELECT * FROM webhooks ORDER BY created_at, id")
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(webhook)
            .collect()
    }

    async fn get_webhook(&self, id: WebhookId) -> Result<Option<Webhook>, StoreError> {
        sqlx::query("SELECT * FROM webhooks WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(webhook)
            .transpose()
    }

    async fn update_webhook(&self, hook: &Webhook) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE webhooks SET url = ?, events = ?, sealed_secret = ?, enabled = ? WHERE id = ?",
        )
        .bind(&hook.url)
        .bind(events(hook)?)
        .bind(&hook.sealed_secret)
        .bind(hook.enabled)
        .bind(hook.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("webhook"));
        }
        Ok(())
    }

    async fn delete_webhook(&self, id: WebhookId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query("DELETE FROM webhook_deliveries WHERE webhook_id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        let done = sqlx::query("DELETE FROM webhooks WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("webhook"));
        }
        tx.commit().await.map_err(backend)
    }

    async fn enqueue_deliveries(&self, deliveries: &[NewDelivery]) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        for new in deliveries {
            let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM webhooks WHERE id = ?")
                .bind(new.webhook_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
            if known.is_none() {
                return Err(StoreError::NotFound("webhook"));
            }
            sqlx::query(
                "INSERT INTO webhook_deliveries (webhook_id, event, event_id, payload, created_at, next_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(new.webhook_id.to_string())
            .bind(&new.event)
            .bind(&new.event_id)
            .bind(&new.payload)
            .bind(new.at.as_millis())
            .bind(new.at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn due_deliveries(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<WebhookDelivery>, StoreError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DELIVERY} FROM webhook_deliveries
             WHERE next_at IS NOT NULL AND next_at <= ? ORDER BY id LIMIT ?"
        )))
        .bind(now.as_millis())
        .bind(i64::from(limit.min(1_000)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(delivery)
        .collect()
    }

    async fn record_attempt(
        &self,
        id: u64,
        status: Option<u16>,
        error: Option<&str>,
        next_at: Option<Timestamp>,
        delivered_at: Option<Timestamp>,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "UPDATE webhook_deliveries SET attempts = attempts + 1, last_status = ?, last_error = ?,
             next_at = ?, delivered_at = ? WHERE id = ?",
        )
        .bind(status.map(i64::from))
        .bind(error)
        .bind(next_at.map(|t| t.as_millis()))
        .bind(delivered_at.map(|t| t.as_millis()))
        .bind(signed(id)?)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn recent_deliveries(
        &self,
        webhook: WebhookId,
        limit: u32,
    ) -> Result<Vec<WebhookDelivery>, StoreError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DELIVERY} FROM webhook_deliveries WHERE webhook_id = ? ORDER BY id DESC LIMIT ?"
        )))
        .bind(webhook.to_string())
        .bind(i64::from(limit.min(1_000)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(delivery)
        .collect()
    }

    async fn prune_deliveries(&self, keep: u32) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "DELETE FROM webhook_deliveries WHERE next_at IS NULL AND id NOT IN (
                 SELECT id FROM (
                     SELECT id, ROW_NUMBER() OVER (PARTITION BY webhook_id ORDER BY id DESC) AS n
                     FROM webhook_deliveries WHERE next_at IS NULL
                 ) WHERE n <= ?
             )",
        )
        .bind(i64::from(keep))
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected())
    }
}
