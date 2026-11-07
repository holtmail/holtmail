use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_category::{MessageKind, Topic};
use holtmail_core::{ArchiveId, MessageId, Timestamp, UserId};
use holtmail_store::*;
use sqlx::AssertSqlSafe;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, parse, unsigned};

const CHUNK: usize = 200;

fn bits(value: u64) -> i64 {
    value as i64
}

fn unbits(value: i64) -> u64 {
    value as u64
}

fn insight_row(row: &SqliteRow) -> Result<InsightRow, StoreError> {
    let outcome: String = get(row, "outcome")?;
    Ok(InsightRow {
        message: parse(&get::<String>(row, "message_id")?)?,
        archive: parse(&get::<String>(row, "archive_id")?)?,
        fingerprint: get(row, "fingerprint")?,
        outcome: InsightOutcome::parse(&outcome)
            .ok_or_else(|| StoreError::Invalid(format!("unknown insight outcome {outcome:?}")))?,
        truncated: get::<i64>(row, "truncated")? != 0,
        from_address: get(row, "from_address")?,
        signals: unbits(get(row, "signals")?),
        kinds: unbits(get(row, "kinds")?),
        auth: get(row, "auth")?,
        items: get(row, "items")?,
        computed_at: Timestamp::from_millis(get(row, "computed_at")?),
    })
}

fn unsubscribe_row(row: &SqliteRow) -> Result<UnsubscribeRequest, StoreError> {
    let outcome: String = get(row, "outcome")?;
    Ok(UnsubscribeRequest {
        id: parse(&get::<String>(row, "id")?)?,
        archive: parse(&get::<String>(row, "archive_id")?)?,
        message: get::<Option<String>>(row, "message_id")?
            .map(|id| parse(&id))
            .transpose()?,
        list_key: get(row, "list_key")?,
        requested_at: Timestamp::from_millis(get(row, "requested_at")?),
        requested_by: parse::<UserId>(&get::<String>(row, "requested_by")?)?,
        outcome: UnsubscribeOutcome::parse(&outcome).ok_or_else(|| {
            StoreError::Invalid(format!("unknown unsubscribe outcome {outcome:?}"))
        })?,
        status: get::<Option<i64>>(row, "status")?
            .map(|status| {
                u16::try_from(status)
                    .map_err(|_| StoreError::Invalid(format!("corrupt status {status}")))
            })
            .transpose()?,
    })
}

impl SqliteStore {
    async fn insight_rows(
        &self,
        columns: &str,
        ids: &[MessageId],
    ) -> Result<Vec<SqliteRow>, StoreError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql =
                format!("SELECT {columns} FROM message_insights WHERE message_id IN ({marks})");
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            out.extend(query.fetch_all(&self.pool).await.map_err(backend)?);
        }
        Ok(out)
    }
}

#[async_trait]
impl InsightStore for SqliteStore {
    async fn set_message_insights(
        &self,
        message: MessageId,
        row: &InsightRow,
    ) -> Result<(), StoreError> {
        if row.message != message {
            return Err(StoreError::Invalid(
                "the insights row is for another message".into(),
            ));
        }
        let id = message.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: String = sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("message"))?;
        if archive != row.archive.to_string() {
            return Err(StoreError::Invalid(
                "the insights row names another archive".into(),
            ));
        }
        sqlx::query(
            "INSERT OR REPLACE INTO message_insights (message_id, archive_id, fingerprint, outcome,
                 truncated, from_address, signals, kinds, auth, items, computed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&archive)
        .bind(&row.fingerprint)
        .bind(row.outcome.as_str())
        .bind(row.truncated)
        .bind(&row.from_address)
        .bind(bits(row.signals))
        .bind(bits(row.kinds))
        .bind(&row.auth)
        .bind(&row.items)
        .bind(row.computed_at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn insight_view(&self, message: MessageId) -> Result<Option<InsightView>, StoreError> {
        let Some(row) = sqlx::query("SELECT * FROM message_insights WHERE message_id = ?")
            .bind(message.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
        else {
            return Ok(None);
        };
        let insights = insight_row(&row)?;
        let categories = self.category_views(&[message]).await?.remove(&message);
        Ok(Some(InsightView {
            insights,
            categories,
        }))
    }

    async fn insight_views_among(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, InsightView>, StoreError> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(CHUNK) {
            let mut categories = self.category_views(chunk).await?;
            for row in self.insight_rows("*", chunk).await? {
                let insights = insight_row(&row)?;
                let message = insights.message;
                out.insert(
                    message,
                    InsightView {
                        insights,
                        categories: categories.remove(&message),
                    },
                );
            }
        }
        Ok(out)
    }

    async fn insights_among(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, InsightSummary>, StoreError> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(CHUNK) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT i.message_id, i.kinds, c.kind, c.list_topic FROM message_insights i
                 LEFT JOIN message_categories c ON c.message_id = i.message_id
                 WHERE i.message_id IN ({marks})"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                out.insert(
                    parse(&get::<String>(&row, "message_id")?)?,
                    InsightSummary {
                        kinds: unbits(get(&row, "kinds")?),
                        kind: get::<Option<String>>(&row, "kind")?
                            .as_deref()
                            .and_then(MessageKind::from_name),
                        topic: get::<Option<String>>(&row, "list_topic")?
                            .as_deref()
                            .and_then(Topic::from_name),
                    },
                );
            }
        }
        Ok(out)
    }

    async fn messages_needing_insights(
        &self,
        archive: ArchiveId,
        fingerprint: &str,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT m.id FROM messages m
             LEFT JOIN message_insights i ON i.message_id = m.id
             WHERE m.archive_id = ? AND m.id > ? AND (i.message_id IS NULL OR i.fingerprint != ?)
             ORDER BY m.id LIMIT ?",
        )
        .bind(archive.to_string())
        .bind(after.map(|a| a.to_string()).unwrap_or_default())
        .bind(fingerprint)
        .bind(i64::from(limit.min(10_000)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        ids.iter().map(|id| parse(id)).collect()
    }

    async fn count_needing_insights(
        &self,
        archive: ArchiveId,
        fingerprint: &str,
    ) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM messages WHERE archive_id = ?1)
                  - (SELECT COUNT(*) FROM message_insights
                     WHERE archive_id = ?1 AND fingerprint = ?2)",
        )
        .bind(archive.to_string())
        .bind(fingerprint)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(unsigned(count))
    }

    async fn record_unsubscribe(&self, request: &UnsubscribeRequest) -> Result<(), StoreError> {
        let archive = request.archive.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM archives WHERE id = ?")
            .bind(&archive)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("archive"))?;
        if let Some(message) = request.message {
            let owner: String = sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
                .bind(message.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?
                .ok_or(StoreError::NotFound("message"))?;
            if owner != archive {
                return Err(StoreError::Invalid(
                    "the message is in another archive".into(),
                ));
            }
        }
        let done = sqlx::query(
            "INSERT OR IGNORE INTO unsubscribe_requests (id, archive_id, message_id, list_key,
                 requested_at, requested_by, outcome, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request.id.to_string())
        .bind(&archive)
        .bind(request.message.map(|m| m.to_string()))
        .bind(&request.list_key)
        .bind(request.requested_at.as_millis())
        .bind(request.requested_by.to_string())
        .bind(request.outcome.as_str())
        .bind(request.status.map(i64::from))
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::Invalid(
                "an unsubscribe request with this id exists".into(),
            ));
        }
        tx.commit().await.map_err(backend)
    }

    async fn last_unsubscribe(
        &self,
        archive: ArchiveId,
        list_key: &str,
    ) -> Result<Option<UnsubscribeRequest>, StoreError> {
        sqlx::query(
            "SELECT * FROM unsubscribe_requests WHERE archive_id = ? AND list_key = ?
             ORDER BY requested_at DESC, id DESC LIMIT 1",
        )
        .bind(archive.to_string())
        .bind(list_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .map(|row| unsubscribe_row(&row))
        .transpose()
    }

    async fn last_unsubscribes(
        &self,
        archive: ArchiveId,
        list_keys: &[String],
    ) -> Result<HashMap<String, UnsubscribeRequest>, StoreError> {
        let archive = archive.to_string();
        let mut out = HashMap::new();
        for chunk in list_keys.chunks(CHUNK) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT * FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY list_key
                     ORDER BY requested_at DESC, id DESC) AS rank
                 FROM unsubscribe_requests WHERE archive_id = ? AND list_key IN ({marks}))
                 WHERE rank = 1"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql)).bind(&archive);
            for key in chunk {
                query = query.bind(key);
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                let request = unsubscribe_row(&row)?;
                out.insert(request.list_key.clone(), request);
            }
        }
        Ok(out)
    }
}
