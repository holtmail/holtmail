use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, FolderId, ItemId, MessageId, Timestamp, UserId};
use holtmail_store::*;

use crate::rows::{backend, parse, signed};
use crate::{SqliteStore, drop_unlinked_remote_images, ledger, release};

#[async_trait]
impl DeletionStore for SqliteStore {
    async fn delete_messages(
        &self,
        archive: ArchiveId,
        ids: &[MessageId],
        actor: Option<UserId>,
    ) -> Result<DeletedMessages, StoreError> {
        let archive_text = archive.to_string();
        let now = Timestamp::now();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let mut out = DeletedMessages::default();
        let mut blobs: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        let mut items: Vec<String> = Vec::new();
        let mut labels: Vec<i64> = Vec::new();

        for id in ids {
            let message = id.to_string();
            let Some(blob) = sqlx::query_scalar::<_, String>(
                "SELECT blob_id FROM messages WHERE id = ? AND archive_id = ?",
            )
            .bind(&message)
            .bind(&archive_text)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            else {
                continue;
            };

            sqlx::query(
                "INSERT OR IGNORE INTO deleted_blobs (archive_id, blob_id, deleted_at) VALUES (?, ?, ?)",
            )
            .bind(&archive_text)
            .bind(&blob)
            .bind(now.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "INSERT OR IGNORE INTO deleted_locations (folder_id, uid, deleted_at)
                 SELECT folder_id, uid, ? FROM locations WHERE message_id = ? AND uid IS NOT NULL",
            )
            .bind(now.as_millis())
            .bind(&message)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;

            blobs.push(blob.clone());
            blobs.extend(
                sqlx::query_scalar::<_, String>(
                    "SELECT blob_id FROM attachments WHERE message_id = ?",
                )
                .bind(&message)
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?,
            );
            keys.extend(
                sqlx::query_scalar::<_, String>(
                    "SELECT key FROM message_remote_images WHERE message_id = ?",
                )
                .bind(&message)
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?,
            );
            labels.extend(
                sqlx::query_scalar::<_, i64>(
                    "SELECT label_id FROM message_labels WHERE message_id = ?",
                )
                .bind(&message)
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?,
            );
            items.extend(
                sqlx::query_scalar::<_, String>(
                    "DELETE FROM item_version_messages WHERE message_id = ? RETURNING item_id",
                )
                .bind(&message)
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?,
            );

            for statement in [
                "DELETE FROM locations WHERE message_id = ?",
                "DELETE FROM attachments WHERE message_id = ?",
                "DELETE FROM message_labels WHERE message_id = ?",
                "DELETE FROM message_addresses WHERE message_id = ?",
                "DELETE FROM message_aliases WHERE message_id = ?",
                "DELETE FROM message_parents WHERE message_id = ?",
                "DELETE FROM message_remote_images WHERE message_id = ?",
                "DELETE FROM remote_scans WHERE message_id = ?",
                "DELETE FROM message_insights WHERE message_id = ?",
                "DELETE FROM messages WHERE id = ?",
            ] {
                sqlx::query(statement)
                    .bind(&message)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
            ledger::append(
                &mut tx,
                NewEntry {
                    at: now,
                    event: LedgerEvent::Deleted,
                    archive_id: archive,
                    message_id: Some(*id),
                    blob_id: Some(parse(&blob)?),
                    actor,
                },
            )
            .await?;
            out.messages.push(*id);
        }

        labels.sort();
        labels.dedup();
        for label in labels {
            sqlx::query(
                "DELETE FROM labels WHERE id = ?1
                 AND NOT EXISTS (SELECT 1 FROM message_labels WHERE label_id = ?1)",
            )
            .bind(label)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }

        items.sort();
        items.dedup();
        for item in items {
            let unlinked: Vec<(i64, String)> = sqlx::query_as(
                "SELECT v.number, v.blob_id FROM item_versions v
                 WHERE v.item_id = ?1 AND v.from_mail = 1
                 AND NOT EXISTS (SELECT 1 FROM item_version_messages l
                                 WHERE l.item_id = ?1 AND l.blob_id = v.blob_id)",
            )
            .bind(&item)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?;
            if unlinked.is_empty() {
                continue;
            }
            for (number, blob) in &unlinked {
                sqlx::query("DELETE FROM item_versions WHERE item_id = ? AND number = ?")
                    .bind(&item)
                    .bind(number)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
                blobs.push(blob.clone());
            }
            let left: Vec<i64> = sqlx::query_scalar(
                "SELECT number FROM item_versions WHERE item_id = ? ORDER BY number",
            )
            .bind(&item)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?;
            let id: ItemId = parse(&item)?;
            if left.is_empty() {
                for statement in [
                    "DELETE FROM occurrences WHERE item_id = ?",
                    "DELETE FROM event_series WHERE item_id = ?",
                    "DELETE FROM pim_index WHERE item_id = ?",
                    "DELETE FROM items WHERE id = ?",
                ] {
                    sqlx::query(statement)
                        .bind(&item)
                        .execute(&mut *tx)
                        .await
                        .map_err(backend)?;
                }
                out.removed_items.push(id);
                continue;
            }
            sqlx::query("UPDATE item_versions SET number = -number WHERE item_id = ?")
                .bind(&item)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            for (at, old) in left.iter().enumerate() {
                sqlx::query("UPDATE item_versions SET number = ? WHERE item_id = ? AND number = ?")
                    .bind(at as i64 + 1)
                    .bind(&item)
                    .bind(-old)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
            sqlx::query("UPDATE items SET versions = ? WHERE id = ?")
                .bind(left.len() as i64)
                .bind(&item)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            out.changed_items.push(id);
        }

        blobs.extend(drop_unlinked_remote_images(&mut tx, keys).await?);
        out.orphans = release(&mut tx, blobs).await?;
        tx.commit().await.map_err(backend)?;
        Ok(out)
    }

    async fn is_deleted(&self, archive: ArchiveId, blob: &BlobId) -> Result<bool, StoreError> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM deleted_blobs WHERE archive_id = ? AND blob_id = ?")
                .bind(archive.to_string())
                .bind(blob.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        Ok(found.is_some())
    }

    async fn record_deleted_location(&self, folder: FolderId, uid: u64) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT OR IGNORE INTO deleted_locations (folder_id, uid, deleted_at) VALUES (?, ?, ?)",
        )
        .bind(folder.to_string())
        .bind(signed(uid)?)
        .bind(Timestamp::now().as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }
}
