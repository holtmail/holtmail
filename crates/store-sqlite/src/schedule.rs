use async_trait::async_trait;
use holtmail_core::{ArchiveId, ItemId, MessageId, Timestamp, UserId};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, parse};

fn entry(row: &SqliteRow) -> Result<ScheduleInboxEntry, StoreError> {
    let note: String = get(row, "note")?;
    Ok(ScheduleInboxEntry {
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        message_id: parse(&get::<String>(row, "message_id")?)?,
        part: u32::try_from(get::<i64>(row, "part")?).unwrap_or(0),
        method: get(row, "method")?,
        uid: get(row, "uid")?,
        note: InboxNote::parse(&note)
            .ok_or_else(|| StoreError::Invalid(format!("unknown inbox note {note:?}")))?,
        received_at: Timestamp::from_millis(get(row, "received_at")?),
    })
}

#[async_trait]
impl ScheduleStore for SqliteStore {
    async fn set_schedule_tag(&self, item: ItemId, tag: Option<&str>) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("UPDATE items SET schedule_tag = ? WHERE id = ?")
            .bind(tag)
            .bind(item.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        match done.rows_affected() {
            0 => Err(StoreError::NotFound("item")),
            _ => Ok(()),
        }
    }

    async fn put_inbox_entry(&self, entry: &ScheduleInboxEntry) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO schedule_inbox (message_id, part, archive_id, method, uid, note, received_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (message_id, part) DO UPDATE SET
               method = excluded.method, uid = excluded.uid, note = excluded.note",
        )
        .bind(entry.message_id.to_string())
        .bind(i64::from(entry.part))
        .bind(entry.archive_id.to_string())
        .bind(&entry.method)
        .bind(&entry.uid)
        .bind(entry.note.as_str())
        .bind(entry.received_at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn inbox_entries(
        &self,
        archive: ArchiveId,
        user: UserId,
    ) -> Result<Vec<ScheduleInboxEntry>, StoreError> {
        sqlx::query(
            "SELECT s.* FROM schedule_inbox s
             JOIN messages m ON m.id = s.message_id
             WHERE s.archive_id = ?1 AND NOT EXISTS (
               SELECT 1 FROM schedule_inbox_hidden h
               WHERE h.message_id = s.message_id AND h.part = s.part AND h.user_id = ?2)
             ORDER BY s.received_at DESC, s.message_id DESC, s.part
             LIMIT ?3",
        )
        .bind(archive.to_string())
        .bind(user.to_string())
        .bind(i64::from(MAX_INBOX))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(entry)
        .collect()
    }

    async fn inbox_entries_of(
        &self,
        message: MessageId,
    ) -> Result<Vec<ScheduleInboxEntry>, StoreError> {
        sqlx::query(
            "SELECT s.* FROM schedule_inbox s JOIN messages m ON m.id = s.message_id
             WHERE s.message_id = ? ORDER BY s.part",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(entry)
        .collect()
    }

    async fn hide_inbox_entry(
        &self,
        user: UserId,
        message: MessageId,
        part: u32,
    ) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM schedule_inbox WHERE message_id = ? AND part = ?")
                .bind(message.to_string())
                .bind(i64::from(part))
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        if exists.is_none() {
            return Ok(false);
        }
        sqlx::query(
            "INSERT OR IGNORE INTO schedule_inbox_hidden (message_id, part, user_id) VALUES (?, ?, ?)",
        )
        .bind(message.to_string())
        .bind(i64::from(part))
        .bind(user.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(true)
    }
}
