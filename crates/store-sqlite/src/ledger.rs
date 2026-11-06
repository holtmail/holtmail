use async_trait::async_trait;
use holtmail_core::{MessageId, Timestamp};
use holtmail_store::*;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, parse, signed, unsigned};

const COLUMNS: &str = "seq, at, event, archive_id, message_id, blob_id, actor, prev, hash";

/// Seals `new` after the newest entry and stores it. The caller holds the write lock and its
/// transaction, so entries are numbered in commit order.
pub(crate) async fn append(
    conn: &mut SqliteConnection,
    new: NewEntry,
) -> Result<LedgerEntry, StoreError> {
    let last = sqlx::query("SELECT seq, hash FROM ledger ORDER BY seq DESC LIMIT 1")
        .fetch_optional(&mut *conn)
        .await
        .map_err(backend)?;
    let (seq, prev) = match last {
        Some(row) => (
            unsigned(get::<i64>(&row, "seq")?) + 1,
            parse::<LedgerHash>(&get::<String>(&row, "hash")?)?,
        ),
        None => (1, LedgerHash::ZERO),
    };
    let entry = new.seal(seq, prev);
    sqlx::query(
        "INSERT INTO ledger (seq, at, event, archive_id, message_id, blob_id, actor, prev, hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(signed(entry.seq)?)
    .bind(entry.at.as_millis())
    .bind(entry.event.as_str())
    .bind(entry.archive_id.to_string())
    .bind(entry.message_id.map(|m| m.to_string()))
    .bind(entry.blob_id.map(|b| b.to_string()))
    .bind(entry.actor.map(|u| u.to_string()))
    .bind(entry.prev.to_string())
    .bind(entry.hash.to_string())
    .execute(&mut *conn)
    .await
    .map_err(backend)?;
    Ok(entry)
}

fn entry(row: &SqliteRow) -> Result<LedgerEntry, StoreError> {
    let event: String = get(row, "event")?;
    Ok(LedgerEntry {
        seq: unsigned(get(row, "seq")?),
        at: Timestamp::from_millis(get(row, "at")?),
        event: LedgerEvent::parse(&event)
            .ok_or_else(|| StoreError::Invalid(format!("unknown ledger event {event}")))?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        message_id: get::<Option<String>>(row, "message_id")?
            .map(|m| parse(&m))
            .transpose()?,
        blob_id: get::<Option<String>>(row, "blob_id")?
            .map(|b| parse(&b))
            .transpose()?,
        actor: get::<Option<String>>(row, "actor")?
            .map(|u| parse(&u))
            .transpose()?,
        prev: parse(&get::<String>(row, "prev")?)?,
        hash: parse(&get::<String>(row, "hash")?)?,
    })
}

const UNENTERED: &str = "FROM messages m WHERE NOT EXISTS
    (SELECT 1 FROM ledger l WHERE l.message_id = m.id AND l.event = 'archived')";
const CHANGED: &str = "FROM messages m WHERE EXISTS
    (SELECT 1 FROM ledger l WHERE l.message_id = m.id AND l.event = 'archived')
    AND NOT EXISTS (SELECT 1 FROM ledger l WHERE l.message_id = m.id AND l.event = 'archived'
                    AND l.blob_id = m.blob_id AND l.archive_id = m.archive_id)";
const VANISHED: &str = "FROM ledger l WHERE l.event = 'archived'
    AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = l.message_id)
    AND NOT EXISTS (SELECT 1 FROM ledger d WHERE d.event = 'deleted' AND d.message_id = l.message_id)
    AND NOT EXISTS (SELECT 1 FROM ledger a WHERE a.event = 'archive_deleted'
                    AND a.archive_id = l.archive_id AND a.seq > l.seq)";

impl SqliteStore {
    async fn sample(
        &self,
        from: &str,
        id: &str,
        limit: u32,
    ) -> Result<(Vec<MessageId>, u64), StoreError> {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(DISTINCT {id}) {from}"
        )))
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        let ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT {id} {from} ORDER BY {id} LIMIT ?"
        )))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let ids = ids.iter().map(|id| parse(id)).collect::<Result<_, _>>()?;
        Ok((ids, unsigned(count)))
    }
}

#[async_trait]
impl LedgerStore for SqliteStore {
    async fn ledger_head(&self) -> Result<Option<LedgerEntry>, StoreError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM ledger ORDER BY seq DESC LIMIT 1"
        )))
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .as_ref()
        .map(entry)
        .transpose()
    }

    async fn ledger_entries(&self, after: u64, limit: u32) -> Result<Vec<LedgerEntry>, StoreError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM ledger WHERE seq > ? ORDER BY seq LIMIT ?"
        )))
        .bind(signed(after)?)
        .bind(i64::from(limit.min(10_000)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(entry)
        .collect()
    }

    async fn enter_unentered(&self, limit: u32) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT m.id, m.archive_id, m.blob_id {UNENTERED} ORDER BY m.archived_at, m.id LIMIT ?"
        )))
        .bind(i64::from(limit.min(10_000)))
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let at = Timestamp::now();
        for row in &rows {
            append(
                &mut tx,
                NewEntry {
                    at,
                    event: LedgerEvent::Archived,
                    archive_id: parse(&get::<String>(row, "archive_id")?)?,
                    message_id: Some(parse(&get::<String>(row, "id")?)?),
                    blob_id: Some(parse(&get::<String>(row, "blob_id")?)?),
                    actor: None,
                },
            )
            .await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(rows.len() as u64)
    }

    async fn ledger_gaps(&self, limit: u32) -> Result<LedgerGaps, StoreError> {
        let limit = limit.min(10_000);
        let (unentered, unentered_count) = self.sample(UNENTERED, "m.id", limit).await?;
        let (changed, changed_count) = self.sample(CHANGED, "m.id", limit).await?;
        let (vanished, vanished_count) = self.sample(VANISHED, "l.message_id", limit).await?;
        Ok(LedgerGaps {
            unentered,
            unentered_count,
            changed,
            changed_count,
            vanished,
            vanished_count,
        })
    }
}
