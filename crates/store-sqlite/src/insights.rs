use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, MessageId, Timestamp};
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
        sqlx::query("SELECT * FROM message_insights WHERE message_id = ?")
            .bind(message.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| {
                Ok(InsightView {
                    insights: insight_row(&row)?,
                })
            })
            .transpose()
    }

    async fn insight_views_among(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, InsightView>, StoreError> {
        self.insight_rows("*", ids)
            .await?
            .iter()
            .map(|row| {
                let insights = insight_row(row)?;
                Ok((insights.message, InsightView { insights }))
            })
            .collect()
    }

    async fn insights_among(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, InsightSummary>, StoreError> {
        self.insight_rows("message_id, kinds", ids)
            .await?
            .iter()
            .map(|row| {
                Ok((
                    parse(&get::<String>(row, "message_id")?)?,
                    InsightSummary {
                        kinds: unbits(get(row, "kinds")?),
                    },
                ))
            })
            .collect()
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
}
