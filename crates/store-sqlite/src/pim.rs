use async_trait::async_trait;
use holtmail_core::{ArchiveId, CollectionId, ItemId, MessageId, Timestamp};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse, unsigned};

const MAX_PAGE: u32 = 500;
const LAST_TEXT: &str = "\u{10FFFF}";

fn collection(row: &SqliteRow) -> Result<Collection, StoreError> {
    let kind: String = get(row, "kind")?;
    Ok(Collection {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        account_id: get::<Option<String>>(row, "account_id")?
            .map(|id| parse(&id))
            .transpose()?,
        kind: CollectionKind::parse(&kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown collection kind {kind:?}")))?,
        remote_id: get(row, "remote_id")?,
        name: get(row, "name")?,
        color: get(row, "color")?,
        sync_token: get(row, "sync_token")?,
        ctag: get(row, "ctag")?,
        synced_at: get::<Option<i64>>(row, "synced_at")?.map(Timestamp::from_millis),
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        removed_at: get::<Option<i64>>(row, "removed_at")?.map(Timestamp::from_millis),
    })
}

pub(crate) const ITEM_COLUMNS: &str =
    "i.id, i.collection_id, i.archive_id, i.remote_id, i.kind, i.key,
    i.first_seen_at, i.removed_at, i.versions, p.version AS p_version, p.readable, p.title,
    p.starts_at, p.ends_at, p.all_day, p.time_zone, p.recurring, p.sequence, p.uid, p.organizer,
    p.emails, p.phones, p.data";

pub(crate) fn item_summary(row: &SqliteRow) -> Result<ItemSummary, StoreError> {
    let kind: String = get(row, "kind")?;
    let item = Item {
        id: parse(&get::<String>(row, "id")?)?,
        collection_id: parse(&get::<String>(row, "collection_id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        remote_id: get(row, "remote_id")?,
        kind: ItemKind::parse(&kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown item kind {kind:?}")))?,
        key: get(row, "key")?,
        first_seen_at: Timestamp::from_millis(get(row, "first_seen_at")?),
        removed_at: get::<Option<i64>>(row, "removed_at")?.map(Timestamp::from_millis),
        versions: u32::try_from(get::<i64>(row, "versions")?).unwrap_or(0),
    };
    let index = match get::<Option<i64>>(row, "p_version")? {
        None => None,
        Some(version) => Some(PimIndex {
            version: u32::try_from(version).unwrap_or(0),
            readable: get::<i64>(row, "readable")? != 0,
            title: get(row, "title")?,
            starts_at: get::<Option<i64>>(row, "starts_at")?.map(Timestamp::from_millis),
            ends_at: get::<Option<i64>>(row, "ends_at")?.map(Timestamp::from_millis),
            all_day: get::<i64>(row, "all_day")? != 0,
            time_zone: get(row, "time_zone")?,
            recurring: get::<i64>(row, "recurring")? != 0,
            sequence: get(row, "sequence")?,
            uid: get(row, "uid")?,
            organizer: get(row, "organizer")?,
            emails: serde_json::from_str(&get::<String>(row, "emails")?).map_err(backend)?,
            phones: serde_json::from_str(&get::<String>(row, "phones")?).map_err(backend)?,
            data: serde_json::from_str(&get::<String>(row, "data")?).map_err(backend)?,
        }),
    };
    Ok(ItemSummary { item, index })
}

fn version(row: &SqliteRow) -> Result<ItemVersion, StoreError> {
    Ok(ItemVersion {
        item_id: parse(&get::<String>(row, "item_id")?)?,
        number: u32::try_from(get::<i64>(row, "number")?).unwrap_or(0),
        blob_id: parse(&get::<String>(row, "blob_id")?)?,
        content_type: get(row, "content_type")?,
        etag: get(row, "etag")?,
        seen_at: Timestamp::from_millis(get(row, "seen_at")?),
        photo_blob_id: get::<Option<String>>(row, "photo_blob_id")?
            .map(|id| parse(&id))
            .transpose()?,
        mail: if get::<i64>(row, "from_mail")? != 0 {
            Some(MailVersionInfo {
                method: get(row, "method")?,
                sequence: get(row, "sequence")?,
                dtstamp: get::<Option<i64>>(row, "dtstamp")?.map(Timestamp::from_millis),
                sent_at: get::<Option<i64>>(row, "sent_at")?.map(Timestamp::from_millis),
            })
        } else {
            None
        },
    })
}

type OrderKey = (Option<i64>, Option<i64>, Option<i64>, String);

fn order_key(mail: &MailVersionInfo, blob: &str) -> OrderKey {
    (
        mail.sequence,
        mail.dtstamp.map(|t| t.as_millis()),
        mail.sent_at.map(|t| t.as_millis()),
        blob.to_owned(),
    )
}

fn source(row: &SqliteRow) -> Result<VersionSource, StoreError> {
    Ok(VersionSource {
        item_id: parse(&get::<String>(row, "item_id")?)?,
        number: u32::try_from(get::<i64>(row, "number")?).unwrap_or(0),
        message_id: parse(&get::<String>(row, "message_id")?)?,
    })
}

async fn write_index(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    item: &str,
    index: &PimIndex,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO pim_index (item_id, version, readable, title, title_key, starts_at, ends_at,
             all_day, time_zone, recurring, sequence, uid, organizer, emails, phones, data)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (item_id) DO UPDATE SET version = excluded.version,
             readable = excluded.readable, title = excluded.title, title_key = excluded.title_key,
             starts_at = excluded.starts_at, ends_at = excluded.ends_at,
             all_day = excluded.all_day, time_zone = excluded.time_zone,
             recurring = excluded.recurring, sequence = excluded.sequence, uid = excluded.uid,
             organizer = excluded.organizer, emails = excluded.emails, phones = excluded.phones,
             data = excluded.data",
    )
    .bind(item)
    .bind(i64::from(index.version))
    .bind(index.readable)
    .bind(&index.title)
    .bind(index.title.as_ref().map(|t| t.trim().to_lowercase()))
    .bind(index.starts_at.map(|t| t.as_millis()))
    .bind(index.ends_at.map(|t| t.as_millis()))
    .bind(index.all_day)
    .bind(&index.time_zone)
    .bind(index.recurring)
    .bind(index.sequence)
    .bind(&index.uid)
    .bind(&index.organizer)
    .bind(json(&index.emails)?)
    .bind(json(&index.phones)?)
    .bind(json(&index.data)?)
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}

fn encode_cursor(order: ItemOrder, summary: &ItemSummary) -> PageCursor {
    let index = summary.index.as_ref();
    let value = match order {
        ItemOrder::Title => index
            .and_then(|i| i.title.as_ref())
            .map(|t| t.trim().to_lowercase())
            .unwrap_or_else(|| LAST_TEXT.to_owned()),
        ItemOrder::Start => index
            .and_then(|i| i.starts_at)
            .map_or(i64::MAX, |t| t.as_millis())
            .to_string(),
    };
    PageCursor(format!("{}|{value}", summary.item.id))
}

fn decode_cursor(cursor: &PageCursor) -> Result<(String, String), StoreError> {
    let invalid = || StoreError::Invalid("malformed cursor".into());
    let (id, value) = cursor.0.split_once('|').ok_or_else(invalid)?;
    id.parse::<ItemId>().map_err(|_| invalid())?;
    Ok((id.to_owned(), value.to_owned()))
}

#[async_trait]
impl PimStore for SqliteStore {
    async fn upsert_collection(&self, new: NewCollection) -> Result<Collection, StoreError> {
        if self.get_archive(new.archive_id).await?.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        if let Some(account) = new.account_id {
            match self.get_account(account).await? {
                None => return Err(StoreError::NotFound("account")),
                Some(a) if a.archive_id != new.archive_id => {
                    return Err(StoreError::Invalid(
                        "the account belongs to another archive".into(),
                    ));
                }
                Some(_) => {}
            }
        }
        let archive = new.archive_id.to_string();
        let account = new.account_id.map(|a| a.to_string());
        let _write = self.writer.lock().await;
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM collections WHERE archive_id = ? AND account_id IS ? AND kind = ?
             AND remote_id = ?",
        )
        .bind(&archive)
        .bind(&account)
        .bind(new.kind.as_str())
        .bind(&new.remote_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        let id = match existing {
            Some(id) => {
                sqlx::query(
                    "UPDATE collections SET name = ?, color = ?, removed_at = NULL WHERE id = ?",
                )
                .bind(&new.name)
                .bind(&new.color)
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(backend)?;
                id
            }
            None => {
                let id = CollectionId::new().to_string();
                sqlx::query(
                    "INSERT INTO collections (id, archive_id, account_id, kind, remote_id, name,
                     color, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(&archive)
                .bind(&account)
                .bind(new.kind.as_str())
                .bind(&new.remote_id)
                .bind(&new.name)
                .bind(&new.color)
                .bind(Timestamp::now().as_millis())
                .execute(&self.pool)
                .await
                .map_err(backend)?;
                id
            }
        };
        let row = sqlx::query("SELECT * FROM collections WHERE id = ?")
            .bind(&id)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        collection(&row)
    }

    async fn get_collection(&self, id: CollectionId) -> Result<Option<Collection>, StoreError> {
        sqlx::query("SELECT * FROM collections WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| collection(&row))
            .transpose()
    }

    async fn list_collections(
        &self,
        archive: ArchiveId,
    ) -> Result<Vec<CollectionSummary>, StoreError> {
        let rows = sqlx::query(
            "SELECT c.*,
                (SELECT COUNT(*) FROM items i WHERE i.collection_id = c.id AND i.removed_at IS NULL)
                    AS live,
                (SELECT COUNT(*) FROM items i WHERE i.collection_id = c.id
                    AND i.removed_at IS NOT NULL) AS gone
             FROM collections c WHERE c.archive_id = ? ORDER BY c.kind, c.name, c.id",
        )
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter()
            .map(|row| {
                Ok(CollectionSummary {
                    collection: collection(row)?,
                    items: unsigned(get(row, "live")?),
                    removed_items: unsigned(get(row, "gone")?),
                })
            })
            .collect()
    }

    async fn set_sync_token(
        &self,
        collection: CollectionId,
        token: Option<&str>,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("UPDATE collections SET sync_token = ? WHERE id = ?")
            .bind(token)
            .bind(collection.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("collection"));
        }
        Ok(())
    }

    async fn set_collection_synced(
        &self,
        collection: CollectionId,
        token: Option<&str>,
        ctag: Option<&str>,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE collections SET sync_token = ?, ctag = ?, synced_at = ? WHERE id = ?",
        )
        .bind(token)
        .bind(ctag)
        .bind(at.as_millis())
        .bind(collection.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("collection"));
        }
        Ok(())
    }

    async fn mark_collection_removed(
        &self,
        collection: CollectionId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let id = collection.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let done =
            sqlx::query("UPDATE collections SET removed_at = COALESCE(removed_at, ?) WHERE id = ?")
                .bind(at.as_millis())
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("collection"));
        }
        sqlx::query(
            "UPDATE items SET removed_at = ? WHERE collection_id = ? AND removed_at IS NULL",
        )
        .bind(at.as_millis())
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn record_version(&self, new: NewItemVersion) -> Result<RecordedVersion, StoreError> {
        let collection_id = new.collection_id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM collections WHERE id = ?")
                .bind(&collection_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        let Some(archive) = archive else {
            return Err(StoreError::NotFound("collection"));
        };
        let existing = sqlx::query(
            "SELECT i.id, i.versions, i.removed_at, v.blob_id FROM items i
             LEFT JOIN item_versions v ON v.item_id = i.id AND v.number = i.versions
             WHERE i.collection_id = ? AND i.remote_id = ?",
        )
        .bind(&collection_id)
        .bind(&new.remote_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let blob = new.blob_id.to_string();
        let (item, number, created_item, restored, same) = match existing {
            Some(row) => {
                let id: String = get(&row, "id")?;
                let versions: i64 = get(&row, "versions")?;
                let removed: Option<i64> = get(&row, "removed_at")?;
                let latest: Option<String> = get(&row, "blob_id")?;
                sqlx::query("UPDATE items SET removed_at = NULL, etag = ? WHERE id = ?")
                    .bind(&new.etag)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
                (
                    id,
                    versions,
                    false,
                    removed.is_some(),
                    latest.as_deref() == Some(blob.as_str()),
                )
            }
            None => {
                let id = ItemId::new().to_string();
                sqlx::query(
                    "INSERT INTO items (id, collection_id, archive_id, remote_id, kind, key,
                     first_seen_at, versions, etag) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?)",
                )
                .bind(&id)
                .bind(&collection_id)
                .bind(&archive)
                .bind(&new.remote_id)
                .bind(new.kind.as_str())
                .bind(&new.key)
                .bind(new.seen_at.as_millis())
                .bind(&new.etag)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
                (id, 0, true, false, false)
            }
        };
        let mut number = number;
        if !same {
            number += 1;
            sqlx::query(
                "INSERT INTO item_versions (item_id, number, blob_id, content_type, etag, seen_at,
                 photo_blob_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&item)
            .bind(number)
            .bind(&blob)
            .bind(&new.content_type)
            .bind(&new.etag)
            .bind(new.seen_at.as_millis())
            .bind(new.photo_blob_id.map(|b| b.to_string()))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query("UPDATE items SET versions = ?, kind = ?, key = ? WHERE id = ?")
                .bind(number)
                .bind(new.kind.as_str())
                .bind(&new.key)
                .bind(&item)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            let mut index = new.index.clone();
            index.version = u32::try_from(number).unwrap_or(u32::MAX);
            write_index(&mut tx, &item, &index).await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(RecordedVersion {
            item_id: parse(&item)?,
            number: u32::try_from(number).unwrap_or(u32::MAX),
            created_item,
            new_version: !same,
            restored,
        })
    }

    async fn record_mail_version(
        &self,
        new: NewMailVersion,
    ) -> Result<RecordedVersion, StoreError> {
        let collection_id = new.version.collection_id.to_string();
        let message_id = new.message_id.to_string();
        let blob = new.version.blob_id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM collections WHERE id = ?")
                .bind(&collection_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        let Some(archive) = archive else {
            return Err(StoreError::NotFound("collection"));
        };
        let message_archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
                .bind(&message_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        match message_archive {
            None => return Err(StoreError::NotFound("message")),
            Some(other) if other != archive => {
                return Err(StoreError::Invalid(
                    "the message belongs to another archive".into(),
                ));
            }
            Some(_) => {}
        }
        let existing = sqlx::query(
            "SELECT id, versions, removed_at FROM items WHERE collection_id = ? AND remote_id = ?",
        )
        .bind(&collection_id)
        .bind(&new.version.remote_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let (item, versions, created_item, restored) = match existing {
            Some(row) => {
                let id: String = get(&row, "id")?;
                let removed: Option<i64> = get(&row, "removed_at")?;
                if removed.is_some() {
                    sqlx::query("UPDATE items SET removed_at = NULL WHERE id = ?")
                        .bind(&id)
                        .execute(&mut *tx)
                        .await
                        .map_err(backend)?;
                }
                (id, get::<i64>(&row, "versions")?, false, removed.is_some())
            }
            None => {
                let id = ItemId::new().to_string();
                sqlx::query(
                    "INSERT INTO items (id, collection_id, archive_id, remote_id, kind, key,
                     first_seen_at, versions) VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
                )
                .bind(&id)
                .bind(&collection_id)
                .bind(&archive)
                .bind(&new.version.remote_id)
                .bind(new.version.kind.as_str())
                .bind(&new.version.key)
                .bind(new.version.seen_at.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
                (id, 0, true, false)
            }
        };
        let rows = sqlx::query("SELECT * FROM item_versions WHERE item_id = ? ORDER BY number")
            .bind(&item)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?;
        let stored = rows.iter().map(version).collect::<Result<Vec<_>, _>>()?;
        let same = stored.iter().any(|v| v.blob_id == new.version.blob_id);
        let mut latest = versions;
        if !same {
            let key = order_key(&new.mail, &blob);
            let at = stored
                .iter()
                .filter(|v| {
                    order_key(&v.mail.clone().unwrap_or_default(), &v.blob_id.to_string()) < key
                })
                .count() as i64
                + 1;
            sqlx::query(
                "UPDATE item_versions SET number = -number - 1 WHERE item_id = ? AND number >= ?",
            )
            .bind(&item)
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "UPDATE item_versions SET number = -number WHERE item_id = ? AND number < 0",
            )
            .bind(&item)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "INSERT INTO item_versions (item_id, number, blob_id, content_type, etag, seen_at,
                 photo_blob_id, from_mail, method, sequence, dtstamp, sent_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?)",
            )
            .bind(&item)
            .bind(at)
            .bind(&blob)
            .bind(&new.version.content_type)
            .bind(&new.version.etag)
            .bind(new.version.seen_at.as_millis())
            .bind(new.version.photo_blob_id.as_ref().map(|b| b.to_string()))
            .bind(&new.mail.method)
            .bind(new.mail.sequence)
            .bind(new.mail.dtstamp.map(|t| t.as_millis()))
            .bind(new.mail.sent_at.map(|t| t.as_millis()))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            latest += 1;
            if at == latest {
                sqlx::query("UPDATE items SET versions = ?, kind = ?, key = ? WHERE id = ?")
                    .bind(latest)
                    .bind(new.version.kind.as_str())
                    .bind(&new.version.key)
                    .bind(&item)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
                let mut index = new.version.index.clone();
                index.version = u32::try_from(latest).unwrap_or(u32::MAX);
                write_index(&mut tx, &item, &index).await?;
            } else {
                sqlx::query("UPDATE items SET versions = ? WHERE id = ?")
                    .bind(latest)
                    .bind(&item)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
        }
        sqlx::query(
            "INSERT INTO item_version_messages (item_id, blob_id, message_id) VALUES (?, ?, ?)
             ON CONFLICT DO NOTHING",
        )
        .bind(&item)
        .bind(&blob)
        .bind(&message_id)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(RecordedVersion {
            item_id: parse(&item)?,
            number: u32::try_from(latest).unwrap_or(u32::MAX),
            created_item,
            new_version: !same,
            restored,
        })
    }

    async fn version_messages(&self, item: ItemId) -> Result<Vec<VersionSource>, StoreError> {
        sqlx::query(
            "SELECT l.item_id, v.number, l.message_id FROM item_version_messages l
             JOIN item_versions v ON v.item_id = l.item_id AND v.blob_id = l.blob_id
             WHERE l.item_id = ? ORDER BY v.number, l.message_id",
        )
        .bind(item.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(source)
        .collect()
    }

    async fn message_versions(&self, message: MessageId) -> Result<Vec<VersionSource>, StoreError> {
        sqlx::query(
            "SELECT l.item_id, v.number, l.message_id FROM item_version_messages l
             JOIN item_versions v ON v.item_id = l.item_id AND v.blob_id = l.blob_id
             WHERE l.message_id = ? ORDER BY l.item_id, v.number",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(source)
        .collect()
    }

    async fn live_remote_ids(&self, collection: CollectionId) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT remote_id FROM items WHERE collection_id = ? AND removed_at IS NULL
             ORDER BY remote_id",
        )
        .bind(collection.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)
    }

    async fn item_etags(
        &self,
        collection: CollectionId,
    ) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let rows = sqlx::query(
            "SELECT remote_id, etag FROM items WHERE collection_id = ? AND removed_at IS NULL
             ORDER BY remote_id",
        )
        .bind(collection.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter()
            .map(|row| Ok((get(row, "remote_id")?, get(row, "etag")?)))
            .collect()
    }

    async fn mark_items_removed(
        &self,
        collection: CollectionId,
        remote_ids: &[String],
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let id = collection.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let mut marked = 0;
        for remote in remote_ids {
            marked += sqlx::query(
                "UPDATE items SET removed_at = ? WHERE collection_id = ? AND remote_id = ?
                 AND removed_at IS NULL",
            )
            .bind(at.as_millis())
            .bind(&id)
            .bind(remote)
            .execute(&mut *tx)
            .await
            .map_err(backend)?
            .rows_affected();
        }
        tx.commit().await.map_err(backend)?;
        Ok(marked)
    }

    async fn get_item(&self, id: ItemId) -> Result<Option<ItemSummary>, StoreError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ITEM_COLUMNS} FROM items i LEFT JOIN pim_index p ON p.item_id = i.id
             WHERE i.id = ?"
        )))
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .map(|row| item_summary(&row))
        .transpose()
    }

    async fn item_versions(&self, id: ItemId) -> Result<Vec<ItemVersion>, StoreError> {
        sqlx::query("SELECT * FROM item_versions WHERE item_id = ? ORDER BY number")
            .bind(id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(version)
            .collect()
    }

    async fn list_items(&self, query: &ItemQuery) -> Result<Page<ItemSummary>, StoreError> {
        let limit = query.limit.clamp(1, MAX_PAGE);
        let sort = match query.order {
            ItemOrder::Title => format!("IFNULL(p.title_key, '{LAST_TEXT}')"),
            ItemOrder::Start => format!("IFNULL(p.starts_at, {})", i64::MAX),
        };
        let removed = match query.removed {
            RemovedFilter::Hide => "AND i.removed_at IS NULL",
            RemovedFilter::Include => "",
            RemovedFilter::Only => "AND i.removed_at IS NOT NULL",
        };
        let after = query.after.as_ref().map(decode_cursor).transpose()?;
        let keyset = if after.is_some() {
            format!("AND ({sort} > ?5 OR ({sort} = ?5 AND i.id > ?4))")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM items i LEFT JOIN pim_index p ON p.item_id = i.id
             WHERE i.archive_id = ?1 AND (?2 IS NULL OR i.kind = ?2)
             AND (?3 IS NULL OR i.collection_id = ?3) {removed} {keyset}
             ORDER BY {sort}, i.id LIMIT ?6"
        );
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(query.archive_id.to_string())
            .bind(query.kind.map(|k| k.as_str()))
            .bind(query.collection_id.map(|c| c.to_string()));
        statement = match (&after, query.order) {
            (Some((id, value)), ItemOrder::Title) => statement.bind(id.clone()).bind(value.clone()),
            (Some((id, value)), ItemOrder::Start) => statement.bind(id.clone()).bind(
                value
                    .parse::<i64>()
                    .map_err(|_| StoreError::Invalid("malformed cursor".into()))?,
            ),
            (None, _) => statement.bind(None::<String>).bind(None::<String>),
        };
        let rows = statement
            .bind(i64::from(limit) + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let mut items = rows
            .iter()
            .map(item_summary)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|last| encode_cursor(query.order, last))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    async fn set_pim_index(&self, item: ItemId, index: &PimIndex) -> Result<(), StoreError> {
        let id = item.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let versions: Option<i64> = sqlx::query_scalar("SELECT versions FROM items WHERE id = ?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        let Some(versions) = versions else {
            return Err(StoreError::NotFound("item"));
        };
        if i64::from(index.version) < versions {
            return Ok(());
        }
        write_index(&mut tx, &id, index).await?;
        tx.commit().await.map_err(backend)
    }

    async fn item_ids(&self, after: Option<ItemId>, limit: u32) -> Result<Vec<ItemId>, StoreError> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM items WHERE id > ? ORDER BY id LIMIT ?")
                .bind(after.map(|a| a.to_string()).unwrap_or_default())
                .bind(i64::from(limit.min(10_000)))
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
        ids.iter().map(|id| parse(id)).collect()
    }
}
