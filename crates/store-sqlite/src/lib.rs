//! Holtmail's metadata [`Store`](holtmail_store::Store) on SQLite.

mod rows;

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{AccountId, ArchiveId, FolderId, MessageId, ThreadId, Timestamp, UserId};
use holtmail_store::*;
use rows::*;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{AssertSqlSafe, SqlitePool};
use tokio::sync::Mutex;

const MAX_PAGE: u32 = 500;

/// A store in one SQLite database file.
///
/// Reads run in parallel; writes are serialized inside the process, so one database file must be
/// opened by one process at a time.
pub struct SqliteStore {
    pool: SqlitePool,
    writer: Mutex<()>,
}

impl SqliteStore {
    /// Opens the database at `path`, creating it and applying migrations as needed.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .map_err(backend)?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|error| StoreError::Backend(Box::new(error)))?;
        Ok(Self {
            pool,
            writer: Mutex::new(()),
        })
    }
}

#[async_trait]
impl ArchiveStore for SqliteStore {
    async fn create_archive(&self, name: &str) -> Result<Archive, StoreError> {
        let archive = Archive {
            id: ArchiveId::new(),
            name: name.to_owned(),
            created_at: Timestamp::now(),
        };
        let _write = self.writer.lock().await;
        sqlx::query("INSERT INTO archives (id, name, created_at) VALUES (?, ?, ?)")
            .bind(archive.id.to_string())
            .bind(&archive.name)
            .bind(archive.created_at.as_millis())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(archive)
    }

    async fn get_archive(&self, id: ArchiveId) -> Result<Option<Archive>, StoreError> {
        sqlx::query("SELECT * FROM archives WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| archive(&row))
            .transpose()
    }

    async fn list_archives(&self) -> Result<Vec<Archive>, StoreError> {
        sqlx::query("SELECT * FROM archives ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(archive)
            .collect()
    }

    async fn rename_archive(&self, id: ArchiveId, name: &str) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("UPDATE archives SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("archive"));
        }
        Ok(())
    }

    async fn delete_archive(&self, id: ArchiveId) -> Result<Vec<BlobId>, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let blobs: Vec<String> = sqlx::query_scalar(
            "SELECT blob_id FROM messages WHERE archive_id = ?1
             UNION SELECT a.blob_id FROM attachments a JOIN messages m ON m.id = a.message_id
             WHERE m.archive_id = ?1",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        for statement in [
            "DELETE FROM sync_failures WHERE folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM folder_cursors WHERE folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM locations WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)
             OR folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM folders WHERE account_id IN (SELECT id FROM accounts WHERE archive_id = ?1)",
            "DELETE FROM accounts WHERE archive_id = ?1",
            "DELETE FROM attachments WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)",
            "DELETE FROM message_parents WHERE archive_id = ?1",
            "DELETE FROM messages WHERE archive_id = ?1",
        ] {
            sqlx::query(statement)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        let done = sqlx::query("DELETE FROM archives WHERE id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("archive"));
        }
        let mut orphans = Vec::new();
        for blob in blobs {
            let used: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM messages WHERE blob_id = ?1
                 UNION ALL SELECT 1 FROM attachments WHERE blob_id = ?1 LIMIT 1",
            )
            .bind(&blob)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            if used.is_none() {
                orphans.push(parse(&blob)?);
            }
        }
        tx.commit().await.map_err(backend)?;
        orphans.sort();
        Ok(orphans)
    }

    async fn archive_stats(&self, id: ArchiveId) -> Result<ArchiveStats, StoreError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS messages, COALESCE(SUM(size), 0) AS bytes,
                    MIN(date) AS oldest, MAX(date) AS newest,
                    (SELECT COUNT(*) FROM attachments a JOIN messages m ON m.id = a.message_id
                     WHERE m.archive_id = ?1) AS attachments
             FROM messages WHERE archive_id = ?1",
        )
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(ArchiveStats {
            messages: unsigned(get(&row, "messages")?),
            attachments: unsigned(get(&row, "attachments")?),
            bytes: unsigned(get(&row, "bytes")?),
            oldest: get::<Option<i64>>(&row, "oldest")?.map(Timestamp::from_millis),
            newest: get::<Option<i64>>(&row, "newest")?.map(Timestamp::from_millis),
        })
    }

    async fn create_account(&self, new: NewAccount) -> Result<Account, StoreError> {
        if self.get_archive(new.archive_id).await?.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        let account = Account {
            id: AccountId::new(),
            archive_id: new.archive_id,
            kind: new.kind,
            name: new.name,
            address: new.address,
            settings: new.settings,
            sealed_secret: new.sealed_secret,
            enabled: new.enabled,
            created_at: Timestamp::now(),
        };
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO accounts
             (id, archive_id, kind, name, address, settings, sealed_secret, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account.id.to_string())
        .bind(account.archive_id.to_string())
        .bind(account.kind.as_str())
        .bind(&account.name)
        .bind(&account.address)
        .bind(account.settings.to_string())
        .bind(&account.sealed_secret)
        .bind(account.enabled)
        .bind(account.created_at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(account)
    }

    async fn get_account(&self, id: AccountId) -> Result<Option<Account>, StoreError> {
        sqlx::query("SELECT * FROM accounts WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| account(&row))
            .transpose()
    }

    async fn list_accounts(&self, archive: ArchiveId) -> Result<Vec<Account>, StoreError> {
        sqlx::query("SELECT * FROM accounts WHERE archive_id = ? ORDER BY id")
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(account)
            .collect()
    }

    async fn update_account(&self, account: &Account) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE accounts SET name = ?, address = ?, settings = ?, sealed_secret = ?, enabled = ?
             WHERE id = ?",
        )
        .bind(&account.name)
        .bind(&account.address)
        .bind(account.settings.to_string())
        .bind(&account.sealed_secret)
        .bind(account.enabled)
        .bind(account.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("account"));
        }
        Ok(())
    }

    async fn delete_account(&self, id: AccountId) -> Result<(), StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        for statement in [
            "DELETE FROM sync_failures WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM folder_cursors WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM locations WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM folders WHERE account_id = ?1",
        ] {
            sqlx::query(statement)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        let done = sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("account"));
        }
        tx.commit().await.map_err(backend)
    }

    async fn upsert_folder(
        &self,
        account: AccountId,
        path: &str,
        role: Option<FolderRole>,
    ) -> Result<Folder, StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO folders (id, account_id, path, role, created_at) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (account_id, path) DO UPDATE SET role = excluded.role",
        )
        .bind(FolderId::new().to_string())
        .bind(account.to_string())
        .bind(path)
        .bind(role.map(|r| r.as_str()))
        .bind(Timestamp::now().as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        let row = sqlx::query("SELECT * FROM folders WHERE account_id = ? AND path = ?")
            .bind(account.to_string())
            .bind(path)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        folder(&row)
    }

    async fn get_folder(&self, id: FolderId) -> Result<Option<Folder>, StoreError> {
        sqlx::query("SELECT * FROM folders WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| folder(&row))
            .transpose()
    }

    async fn list_folders(&self, archive: ArchiveId) -> Result<Vec<FolderSummary>, StoreError> {
        sqlx::query(
            "SELECT f.*, COUNT(l.message_id) AS messages,
                    COALESCE(SUM(l.message_id IS NOT NULL AND NOT EXISTS (
                        SELECT 1 FROM json_each(l.flags)
                        WHERE lower(json_each.value) = '\\seen')), 0) AS unread
             FROM folders f JOIN accounts a ON a.id = f.account_id
             LEFT JOIN locations l ON l.folder_id = f.id AND l.removed_at IS NULL
             WHERE a.archive_id = ? GROUP BY f.id ORDER BY a.id, f.path",
        )
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            Ok(FolderSummary {
                folder: folder(row)?,
                messages: unsigned(get(row, "messages")?),
                unread: unsigned(get(row, "unread")?),
            })
        })
        .collect()
    }
}

#[async_trait]
impl MessageStore for SqliteStore {
    async fn insert_message(&self, new: NewMessage) -> Result<Inserted, StoreError> {
        let archive = new.archive_id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;

        if let Some(row) =
            sqlx::query("SELECT id, thread_id FROM messages WHERE archive_id = ? AND blob_id = ?")
                .bind(&archive)
                .bind(new.blob_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?
        {
            return Ok(Inserted {
                message_id: parse(&get::<String>(&row, "id")?)?,
                thread_id: parse(&get::<String>(&row, "thread_id")?)?,
                created: false,
            });
        }

        let mut parents = new.parent_ids.clone();
        parents.sort();
        parents.dedup();
        let mut threads: Vec<String> = Vec::new();
        if !parents.is_empty() {
            let marks = vec!["?"; parents.len()].join(", ");
            let sql = format!(
                "SELECT thread_id FROM messages WHERE archive_id = ? AND rfc_message_id IN ({marks})
                 UNION
                 SELECT m.thread_id FROM message_parents p JOIN messages m ON m.id = p.message_id
                 WHERE p.archive_id = ? AND p.parent_rfc_id IN ({marks})"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql)).bind(&archive);
            for parent in &parents {
                query = query.bind(parent);
            }
            query = query.bind(&archive);
            for parent in &parents {
                query = query.bind(parent);
            }
            for row in query.fetch_all(&mut *tx).await.map_err(backend)? {
                threads.push(get(&row, "thread_id")?);
            }
        }
        if let Some(own) = &new.rfc_message_id {
            for row in sqlx::query(
                "SELECT m.thread_id FROM message_parents p JOIN messages m ON m.id = p.message_id
                 WHERE p.archive_id = ? AND p.parent_rfc_id = ?",
            )
            .bind(&archive)
            .bind(own)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?
            {
                threads.push(get(&row, "thread_id")?);
            }
        }
        threads.sort();
        threads.dedup();

        let thread = match threads.first() {
            Some(oldest) => parse::<ThreadId>(oldest)?,
            None => ThreadId::new(),
        };
        for other in threads.iter().skip(1) {
            sqlx::query("UPDATE messages SET thread_id = ? WHERE archive_id = ? AND thread_id = ?")
                .bind(thread.to_string())
                .bind(&archive)
                .bind(other)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }

        let id = MessageId::new();
        let archived_at = Timestamp::now();
        let date = new.sent_at.or(new.received_at).unwrap_or(archived_at);
        let attachment_count = new.attachments.iter().filter(|a| !a.inline).count();
        sqlx::query(
            "INSERT INTO messages
             (id, archive_id, thread_id, blob_id, size, rfc_message_id, subject, from_json, to_json,
              cc_json, bcc_json, sent_at, received_at, archived_at, date, preview, attachment_count)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(&archive)
        .bind(thread.to_string())
        .bind(new.blob_id.to_string())
        .bind(signed(new.size)?)
        .bind(&new.rfc_message_id)
        .bind(&new.subject)
        .bind(json(&new.from)?)
        .bind(json(&new.to)?)
        .bind(json(&new.cc)?)
        .bind(json(&new.bcc)?)
        .bind(new.sent_at.map(|t| t.as_millis()))
        .bind(new.received_at.map(|t| t.as_millis()))
        .bind(archived_at.as_millis())
        .bind(date.as_millis())
        .bind(&new.preview)
        .bind(attachment_count as i64)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;

        for parent in &parents {
            sqlx::query(
                "INSERT INTO message_parents (message_id, archive_id, parent_rfc_id) VALUES (?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(&archive)
            .bind(parent)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        for attachment in &new.attachments {
            sqlx::query(
                "INSERT INTO attachments
                 (message_id, ordinal, blob_id, filename, content_type, size, content_id, inline)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(i64::from(attachment.ordinal))
            .bind(attachment.blob_id.to_string())
            .bind(&attachment.filename)
            .bind(&attachment.content_type)
            .bind(signed(attachment.size)?)
            .bind(&attachment.content_id)
            .bind(attachment.inline)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }

        tx.commit().await.map_err(backend)?;
        Ok(Inserted {
            message_id: id,
            thread_id: thread,
            created: true,
        })
    }

    async fn get_message(&self, id: MessageId) -> Result<Option<Message>, StoreError> {
        sqlx::query("SELECT * FROM messages WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| message(&row))
            .transpose()
    }

    async fn get_messages(&self, ids: &[MessageId]) -> Result<Vec<Message>, StoreError> {
        let mut found = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(200) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!("SELECT * FROM messages WHERE id IN ({marks})");
            let mut query = sqlx::query(AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                found.push(message(&row)?);
            }
        }
        Ok(ids
            .iter()
            .filter_map(|id| found.iter().find(|m| m.id == *id).cloned())
            .collect())
    }

    async fn find_by_blob(
        &self,
        archive: ArchiveId,
        blob: &BlobId,
    ) -> Result<Option<MessageId>, StoreError> {
        sqlx::query("SELECT id FROM messages WHERE archive_id = ? AND blob_id = ?")
            .bind(archive.to_string())
            .bind(blob.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| parse(&get::<String>(&row, "id")?))
            .transpose()
    }

    async fn attachments(&self, message: MessageId) -> Result<Vec<Attachment>, StoreError> {
        sqlx::query("SELECT * FROM attachments WHERE message_id = ? ORDER BY ordinal")
            .bind(message.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(attachment)
            .collect()
    }

    async fn list_messages(&self, query: &MessageQuery) -> Result<Page<Message>, StoreError> {
        let limit = query.limit.clamp(1, MAX_PAGE);
        let (date, id) = match &query.after {
            Some(cursor) => decode_cursor(cursor)?,
            None => (i64::MAX, String::from("~")),
        };
        let rows = match query.folder_id {
            Some(folder) => {
                sqlx::query(
                    "SELECT m.* FROM messages m JOIN locations l ON l.message_id = m.id
                     WHERE l.folder_id = ? AND l.removed_at IS NULL AND m.archive_id = ?
                       AND (m.date, m.id) < (?, ?)
                     ORDER BY m.date DESC, m.id DESC LIMIT ?",
                )
                .bind(folder.to_string())
                .bind(query.archive_id.to_string())
                .bind(date)
                .bind(&id)
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT * FROM messages WHERE archive_id = ? AND (date, id) < (?, ?)
                     ORDER BY date DESC, id DESC LIMIT ?",
                )
                .bind(query.archive_id.to_string())
                .bind(date)
                .bind(&id)
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(backend)?;
        let mut items = rows.iter().map(message).collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(encode_cursor)
        } else {
            None
        };
        Ok(Page { items, next })
    }

    async fn message_extras(
        &self,
        ids: &[MessageId],
        folder: Option<FolderId>,
    ) -> Result<HashMap<MessageId, MessageExtras>, StoreError> {
        let mut out: HashMap<MessageId, MessageExtras> = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(200) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let threads = format!(
                "SELECT m.id, (SELECT COUNT(*) FROM messages t WHERE t.thread_id = m.thread_id)
                   AS thread_count
                 FROM messages m WHERE m.id IN ({marks})"
            );
            let mut query = sqlx::query(AssertSqlSafe(threads));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                out.insert(
                    parse(&get::<String>(&row, "id")?)?,
                    MessageExtras {
                        thread_count: u32::try_from(get::<i64>(&row, "thread_count")?)
                            .unwrap_or(u32::MAX),
                        ..MessageExtras::default()
                    },
                );
            }
            let places = format!(
                "SELECT message_id, folder_id, flags FROM locations
                 WHERE removed_at IS NULL AND message_id IN ({marks}) ORDER BY folder_id"
            );
            let mut query = sqlx::query(AssertSqlSafe(places));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                let message: MessageId = parse(&get::<String>(&row, "message_id")?)?;
                let folder_id: FolderId = parse(&get::<String>(&row, "folder_id")?)?;
                let Some(extras) = out.get_mut(&message) else {
                    continue;
                };
                if folder.is_none_or(|f| f == folder_id) {
                    extras.flags.extend(flags(&row)?);
                }
                extras.folder_ids.push(folder_id);
            }
        }
        for extras in out.values_mut() {
            extras.flags.sort();
            extras.flags.dedup();
        }
        Ok(out)
    }

    async fn thread_messages(&self, thread: ThreadId) -> Result<Vec<Message>, StoreError> {
        sqlx::query("SELECT * FROM messages WHERE thread_id = ? ORDER BY date, id")
            .bind(thread.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(message)
            .collect()
    }

    async fn record_location(
        &self,
        message: MessageId,
        folder: FolderId,
        uid: Option<u64>,
        flags: &[String],
        seen_at: Timestamp,
    ) -> Result<(), StoreError> {
        let uid = uid.map(signed).transpose()?;
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        if let Some(uid) = uid {
            sqlx::query(
                "UPDATE locations SET uid = NULL WHERE folder_id = ? AND uid = ? AND message_id != ?",
            )
            .bind(folder.to_string())
            .bind(uid)
            .bind(message.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        sqlx::query(
            "INSERT INTO locations (folder_id, message_id, uid, flags, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT (folder_id, message_id) DO UPDATE SET
               uid = excluded.uid, flags = excluded.flags,
               last_seen_at = excluded.last_seen_at, removed_at = NULL",
        )
        .bind(folder.to_string())
        .bind(message.to_string())
        .bind(uid)
        .bind(json(&flags)?)
        .bind(seen_at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn locations(&self, message: MessageId) -> Result<Vec<Location>, StoreError> {
        sqlx::query(
            "SELECT * FROM locations WHERE message_id = ? ORDER BY first_seen_at, folder_id",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(location)
        .collect()
    }

    async fn folder_uids(&self, folder: FolderId) -> Result<Vec<u64>, StoreError> {
        sqlx::query(
            "SELECT uid FROM locations WHERE folder_id = ? AND uid IS NOT NULL AND removed_at IS NULL
             ORDER BY uid",
        )
        .bind(folder.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| Ok(unsigned(get(row, "uid")?)))
        .collect()
    }

    async fn message_at_uid(
        &self,
        folder: FolderId,
        uid: u64,
    ) -> Result<Option<MessageId>, StoreError> {
        sqlx::query("SELECT message_id FROM locations WHERE folder_id = ? AND uid = ?")
            .bind(folder.to_string())
            .bind(signed(uid)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| parse(&get::<String>(&row, "message_id")?))
            .transpose()
    }

    async fn mark_removed(
        &self,
        folder: FolderId,
        uids: &[u64],
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        for uid in uids {
            sqlx::query(
                "UPDATE locations SET removed_at = ?
                 WHERE folder_id = ? AND uid = ? AND removed_at IS NULL",
            )
            .bind(at.as_millis())
            .bind(folder.to_string())
            .bind(signed(*uid)?)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn forget_uids(&self, folder: FolderId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("UPDATE locations SET uid = NULL WHERE folder_id = ?")
            .bind(folder.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}

#[async_trait]
impl SyncStore for SqliteStore {
    async fn cursor(&self, folder: FolderId) -> Result<FolderCursor, StoreError> {
        let Some(row) = sqlx::query("SELECT * FROM folder_cursors WHERE folder_id = ?")
            .bind(folder.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
        else {
            return Ok(FolderCursor::default());
        };
        Ok(FolderCursor {
            uid_validity: get::<Option<i64>>(&row, "uid_validity")?.map(unsigned),
            last_uid: get::<Option<i64>>(&row, "last_uid")?.map(unsigned),
            highest_modseq: get::<Option<i64>>(&row, "highest_modseq")?.map(unsigned),
            state: get(&row, "state")?,
            synced_at: get::<Option<i64>>(&row, "synced_at")?.map(Timestamp::from_millis),
        })
    }

    async fn set_cursor(&self, folder: FolderId, cursor: &FolderCursor) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO folder_cursors
             (folder_id, uid_validity, last_uid, highest_modseq, state, synced_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (folder_id) DO UPDATE SET
               uid_validity = excluded.uid_validity, last_uid = excluded.last_uid,
               highest_modseq = excluded.highest_modseq, state = excluded.state,
               synced_at = excluded.synced_at",
        )
        .bind(folder.to_string())
        .bind(cursor.uid_validity.map(signed).transpose()?)
        .bind(cursor.last_uid.map(signed).transpose()?)
        .bind(cursor.highest_modseq.map(signed).transpose()?)
        .bind(&cursor.state)
        .bind(cursor.synced_at.map(|t| t.as_millis()))
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn record_failure(
        &self,
        folder: FolderId,
        uid: u64,
        reason: &str,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO sync_failures (folder_id, uid, reason, attempts, failed_at)
             VALUES (?, ?, ?, 1, ?)
             ON CONFLICT (folder_id, uid) DO UPDATE SET
               reason = excluded.reason, attempts = attempts + 1, failed_at = excluded.failed_at",
        )
        .bind(folder.to_string())
        .bind(signed(uid)?)
        .bind(reason)
        .bind(at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn clear_failure(&self, folder: FolderId, uid: u64) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM sync_failures WHERE folder_id = ? AND uid = ?")
            .bind(folder.to_string())
            .bind(signed(uid)?)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn failures(&self, folder: FolderId) -> Result<Vec<SyncFailure>, StoreError> {
        sqlx::query("SELECT * FROM sync_failures WHERE folder_id = ? ORDER BY uid")
            .bind(folder.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(|row| {
                Ok(SyncFailure {
                    folder_id: parse(&get::<String>(row, "folder_id")?)?,
                    uid: unsigned(get(row, "uid")?),
                    reason: get(row, "reason")?,
                    attempts: u32::try_from(get::<i64>(row, "attempts")?).unwrap_or(u32::MAX),
                    failed_at: Timestamp::from_millis(get(row, "failed_at")?),
                })
            })
            .collect()
    }
}

#[async_trait]
impl UserStore for SqliteStore {
    async fn count_users(&self) -> Result<u64, StoreError> {
        let row = sqlx::query("SELECT COUNT(*) AS users FROM users")
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        Ok(unsigned(get(&row, "users")?))
    }

    async fn create_user(&self, new: NewUser) -> Result<User, StoreError> {
        let user = User {
            id: UserId::new(),
            email: new.email,
            name: new.name,
            role: new.role,
            created_at: Timestamp::now(),
        };
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO users (id, email, email_key, name, role, password_hash, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(user.id.to_string())
        .bind(&user.email)
        .bind(email_key(&user.email))
        .bind(&user.name)
        .bind(user.role.as_str())
        .bind(&new.password_hash)
        .bind(user.created_at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(|error| unique(error, "a user with this email exists"))?;
        Ok(user)
    }

    async fn get_user(&self, id: UserId) -> Result<Option<User>, StoreError> {
        sqlx::query("SELECT * FROM users WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| user(&row))
            .transpose()
    }

    async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        sqlx::query("SELECT * FROM users WHERE email_key = ?")
            .bind(email_key(email))
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| user(&row))
            .transpose()
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        sqlx::query("SELECT * FROM users ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(user)
            .collect()
    }

    async fn password_hash(&self, id: UserId) -> Result<Option<String>, StoreError> {
        let row = sqlx::query("SELECT password_hash FROM users WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("user"))?;
        get(&row, "password_hash")
    }

    async fn update_user(&self, user: &User) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE users SET email = ?, email_key = ?, name = ?, role = ? WHERE id = ?",
        )
        .bind(&user.email)
        .bind(email_key(&user.email))
        .bind(&user.name)
        .bind(user.role.as_str())
        .bind(user.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|error| unique(error, "a user with this email exists"))?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("user"));
        }
        Ok(())
    }

    async fn delete_user(&self, id: UserId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("user"));
        }
        Ok(())
    }

    async fn grants(&self, user: UserId) -> Result<Vec<ArchiveId>, StoreError> {
        sqlx::query_scalar::<_, String>(
            "SELECT archive_id FROM archive_grants WHERE user_id = ? ORDER BY archive_id",
        )
        .bind(user.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|id| parse(id))
        .collect()
    }

    async fn set_grants(&self, user: UserId, archives: &[ArchiveId]) -> Result<(), StoreError> {
        if self.get_user(user).await?.is_none() {
            return Err(StoreError::NotFound("user"));
        }
        let user = user.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query("DELETE FROM archive_grants WHERE user_id = ?")
            .bind(&user)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        for archive in archives {
            sqlx::query(
                "INSERT OR IGNORE INTO archive_grants (user_id, archive_id)
                 SELECT ?, id FROM archives WHERE id = ?",
            )
            .bind(&user)
            .bind(archive.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn set_password_hash(&self, id: UserId, hash: &str) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("user"));
        }
        Ok(())
    }

    async fn create_session(
        &self,
        hash: &SessionHash,
        id: UserId,
        expires_at: Timestamp,
    ) -> Result<(), StoreError> {
        if self.get_user(id).await?.is_none() {
            return Err(StoreError::NotFound("user"));
        }
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&hash[..])
        .bind(id.to_string())
        .bind(Timestamp::now().as_millis())
        .bind(expires_at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(|error| unique(error, "a session with this hash exists"))?;
        Ok(())
    }

    async fn session_user(
        &self,
        hash: &SessionHash,
        now: Timestamp,
    ) -> Result<Option<User>, StoreError> {
        sqlx::query(
            "SELECT u.* FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ? AND s.expires_at > ?",
        )
        .bind(&hash[..])
        .bind(now.as_millis())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .map(|row| user(&row))
        .transpose()
    }

    async fn delete_session(&self, hash: &SessionHash) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(&hash[..])
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn delete_user_sessions(&self, id: UserId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn delete_expired_sessions(&self, now: Timestamp) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now.as_millis())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected())
    }
}

fn encode_cursor(message: &Message) -> PageCursor {
    PageCursor(format!("{}.{}", message.date.as_millis(), message.id))
}

fn decode_cursor(cursor: &PageCursor) -> Result<(i64, String), StoreError> {
    let invalid = || StoreError::Invalid("malformed page cursor".into());
    let (date, id) = cursor.0.split_once('.').ok_or_else(invalid)?;
    let date = date.parse().map_err(|_| invalid())?;
    MessageId::from_str(id).map_err(|_| invalid())?;
    Ok((date, id.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_store_contract() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        holtmail_store::testing::conformance(Arc::new(store)).await;
    }

    #[tokio::test]
    async fn reopens_an_existing_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("holtmail.db");
        let id = SqliteStore::open(&path)
            .await
            .unwrap()
            .create_archive("Kept")
            .await
            .unwrap()
            .id;
        let store = SqliteStore::open(&path).await.unwrap();
        assert_eq!(store.get_archive(id).await.unwrap().unwrap().name, "Kept");
    }

    #[tokio::test]
    async fn rejects_a_malformed_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        let archive = store.create_archive("A").await.unwrap().id;
        let result = store
            .list_messages(&MessageQuery {
                archive_id: archive,
                folder_id: None,
                after: Some(PageCursor("garbage".into())),
                limit: 10,
            })
            .await;
        assert!(matches!(result, Err(StoreError::Invalid(_))));
    }
}
