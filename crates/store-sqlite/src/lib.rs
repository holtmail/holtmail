//! Holtmail's metadata [`Store`](holtmail_store::Store) on SQLite.

mod aliases;
mod calendar;
mod categories;
mod changes;
mod deletion;
mod devices;
mod insights;
mod item_changes;
mod ledger;
mod mail_changes;
mod people;
mod pim;
mod rows;
mod rules;
mod schedule;
mod send;
mod serve;
mod view;
mod webhooks;
mod write;

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
use tokio::sync::{Mutex, MutexGuard, watch};

const MAX_PAGE: u32 = 500;

/// A store in one SQLite database file.
///
/// Reads run in parallel; writes are serialized inside the process, so one database file must be
/// opened by one process at a time.
pub struct SqliteStore {
    pool: SqlitePool,
    writer: Writer,
    poll: Duration,
}

/// Serializes writes and ticks after each one, so change watches look at the log.
struct Writer {
    lock: Mutex<()>,
    tick: watch::Sender<u64>,
}

impl Writer {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            tick: watch::Sender::new(0),
        }
    }

    async fn lock(&self) -> WriteGuard<'_> {
        WriteGuard {
            _lock: self.lock.lock().await,
            tick: &self.tick,
        }
    }
}

struct WriteGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    tick: &'a watch::Sender<u64>,
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.tick.send_modify(|n| *n = n.wrapping_add(1));
    }
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
            writer: Writer::new(),
            poll: CHANGE_POLL,
        })
    }

    /// Closes every connection, waiting for them to finish, so the database file is complete on
    /// disk. Calls after this fail.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Writes a consistent copy of the whole database to `path`, which must not exist yet. Safe
    /// while other tasks read and write: the copy holds every write committed before it started
    /// and none after.
    pub async fn snapshot(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        if path.exists() {
            return Err(StoreError::Invalid(format!("{} exists", path.display())));
        }
        let target = path
            .to_str()
            .ok_or_else(|| StoreError::Invalid("the snapshot path is not UTF-8".into()))?;
        sqlx::query("VACUUM INTO ?")
            .bind(target)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}

#[async_trait]
impl ArchiveStore for SqliteStore {
    async fn create_archive(&self, name: &str) -> Result<Archive, StoreError> {
        let archive = Archive {
            id: ArchiveId::new(),
            name: name.to_owned(),
            created_at: Timestamp::now(),
            save_remote_images: false,
            save_remote_images_since: None,
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

    async fn set_save_remote_images(&self, id: ArchiveId, save: bool) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE archives SET save_remote_images = ?1,
             save_remote_images_since = CASE WHEN ?1 = 0 THEN NULL
             ELSE COALESCE(save_remote_images_since, ?2) END
             WHERE id = ?3",
        )
        .bind(i64::from(save))
        .bind(Timestamp::now().as_millis())
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
        let mut blobs: Vec<String> = sqlx::query_scalar(
            "SELECT blob_id FROM messages WHERE archive_id = ?1
             UNION SELECT a.blob_id FROM attachments a JOIN messages m ON m.id = a.message_id
             WHERE m.archive_id = ?1",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let versions: Vec<String> = sqlx::query_scalar(
            "SELECT v.blob_id FROM item_versions v JOIN items i ON i.id = v.item_id
             WHERE i.archive_id = ?1
             UNION SELECT v.photo_blob_id FROM item_versions v JOIN items i ON i.id = v.item_id
             WHERE i.archive_id = ?1 AND v.photo_blob_id IS NOT NULL",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        blobs.extend(versions);
        let outgoing: Vec<String> = sqlx::query_scalar(
            "SELECT blob_id FROM outbox WHERE archive_id = ?1
             UNION SELECT b.blob_id FROM draft_blobs b JOIN drafts d ON d.id = b.draft_id
             WHERE d.archive_id = ?1",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        blobs.extend(outgoing);
        let appends: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT c.blob_id FROM mail_changes c JOIN accounts a ON a.id = c.account_id
             WHERE a.archive_id = ?1 AND c.blob_id IS NOT NULL",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        blobs.extend(appends);
        let edits: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT blob_id FROM item_changes WHERE archive_id = ?1 AND blob_id IS NOT NULL",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        blobs.extend(edits);
        let linked: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT l.key FROM message_remote_images l JOIN messages m ON m.id = l.message_id
             WHERE m.archive_id = ?1",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        for statement in [
            "DELETE FROM mail_changes WHERE account_id IN (SELECT id FROM accounts WHERE archive_id = ?1)",
            "DELETE FROM item_change_messages WHERE change_id IN (SELECT id FROM item_changes WHERE archive_id = ?1)",
            "DELETE FROM deferred_replies WHERE archive_id = ?1",
            "DELETE FROM flow_actions WHERE flow_id IN (SELECT id FROM flows WHERE archive_id = ?1)",
            "DELETE FROM flow_runs WHERE archive_id = ?1",
            "DELETE FROM flow_versions WHERE flow_id IN (SELECT id FROM flows WHERE archive_id = ?1)",
            "DELETE FROM flows WHERE archive_id = ?1",
            "DELETE FROM rule_events WHERE archive_id = ?1",
            "DELETE FROM schedule_inbox_hidden WHERE message_id IN (SELECT message_id FROM schedule_inbox WHERE archive_id = ?1)",
            "DELETE FROM schedule_inbox WHERE archive_id = ?1",
            "DELETE FROM item_changes WHERE archive_id = ?1",
            "DELETE FROM changes WHERE archive_id = ?1",
            "DELETE FROM local_flags WHERE archive_id = ?1",
            "DELETE FROM local_places WHERE archive_id = ?1",
            "DELETE FROM served_subscriptions WHERE archive_id = ?1",
            "DELETE FROM outbox WHERE archive_id = ?1",
            "DELETE FROM drafts WHERE archive_id = ?1",
            "DELETE FROM identity_defaults WHERE archive_id = ?1",
            "DELETE FROM identities WHERE archive_id = ?1",
            "DELETE FROM transport_grants WHERE archive_id = ?1",
            "DELETE FROM transports WHERE archive_id = ?1",
            "DELETE FROM deleted_locations WHERE folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM deleted_blobs WHERE archive_id = ?1",
            "DELETE FROM sync_failures WHERE folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM folder_cursors WHERE folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM locations WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)
             OR folder_id IN
             (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE a.archive_id = ?1)",
            "DELETE FROM folders WHERE account_id IN (SELECT id FROM accounts WHERE archive_id = ?1)",
            "DELETE FROM account_runs WHERE account_id IN (SELECT id FROM accounts WHERE archive_id = ?1)",
            "DELETE FROM account_authserv WHERE account_id IN (SELECT id FROM accounts WHERE archive_id = ?1)",
            "DELETE FROM account_capabilities WHERE account_id IN (SELECT id FROM accounts WHERE archive_id = ?1)",
            "DELETE FROM person_emails WHERE archive_id = ?1",
            "DELETE FROM person_collections WHERE person_id IN (SELECT id FROM people WHERE archive_id = ?1)",
            "DELETE FROM people WHERE archive_id = ?1",
            "DELETE FROM person_overrides WHERE archive_id = ?1",
            "DELETE FROM message_addresses WHERE archive_id = ?1",
            "DELETE FROM message_aliases WHERE archive_id = ?1",
            "DELETE FROM aliases WHERE archive_id = ?1",
            "DELETE FROM alias_services WHERE archive_id = ?1",
            "DELETE FROM alias_strategies WHERE archive_id = ?1",
            "DELETE FROM reverse_aliases WHERE archive_id = ?1",
            "DELETE FROM item_version_messages WHERE item_id IN (SELECT id FROM items WHERE archive_id = ?1)",
            "DELETE FROM occurrences WHERE archive_id = ?1",
            "DELETE FROM event_series WHERE archive_id = ?1",
            "DELETE FROM pim_index WHERE item_id IN (SELECT id FROM items WHERE archive_id = ?1)",
            "DELETE FROM item_versions WHERE item_id IN (SELECT id FROM items WHERE archive_id = ?1)",
            "DELETE FROM items WHERE archive_id = ?1",
            "DELETE FROM collections WHERE archive_id = ?1",
            "DELETE FROM accounts WHERE archive_id = ?1",
            "DELETE FROM attachments WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)",
            "DELETE FROM message_parents WHERE archive_id = ?1",
            "DELETE FROM message_labels WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)",
            "DELETE FROM labels WHERE archive_id = ?1",
            "DELETE FROM message_remote_images WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)",
            "DELETE FROM remote_scans WHERE message_id IN (SELECT id FROM messages WHERE archive_id = ?1)",
            "DELETE FROM message_insights WHERE archive_id = ?1",
            "DELETE FROM message_topics WHERE archive_id = ?1",
            "DELETE FROM message_categories WHERE archive_id = ?1",
            "DELETE FROM category_overrides WHERE archive_id = ?1",
            "DELETE FROM insight_reindex WHERE archive_id = ?1",
            "DELETE FROM unsubscribe_requests WHERE archive_id = ?1",
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
        ledger::append(
            &mut tx,
            NewEntry {
                at: Timestamp::now(),
                event: LedgerEvent::ArchiveDeleted,
                archive_id: parse(&id)?,
                message_id: None,
                blob_id: None,
                actor: None,
            },
        )
        .await?;
        blobs.extend(drop_unlinked_remote_images(&mut tx, linked).await?);
        let orphans = release(&mut tx, blobs).await?;
        tx.commit().await.map_err(backend)?;
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
        let now = Timestamp::now();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let gone = sqlx::query(
            "SELECT f.id, a.archive_id FROM folders f JOIN accounts a ON a.id = f.account_id
             WHERE f.account_id = ? ORDER BY f.id",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        for row in &gone {
            changes::log(
                &mut tx,
                changes::Row::new(ChangeKind::Mailbox, get::<String>(row, "archive_id")?)
                    .folder(get::<String>(row, "id")?),
                now,
            )
            .await?;
        }
        for statement in [
            "DELETE FROM mail_changes WHERE account_id = ?1",
            "DELETE FROM sync_failures WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM folder_cursors WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM locations WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM deleted_locations WHERE folder_id IN (SELECT id FROM folders WHERE account_id = ?1)",
            "DELETE FROM folders WHERE account_id = ?1",
            "DELETE FROM account_runs WHERE account_id = ?1",
            "DELETE FROM account_authserv WHERE account_id = ?1",
            "DELETE FROM account_capabilities WHERE account_id = ?1",
            "UPDATE collections SET account_id = NULL, write_mode = 'read_only' WHERE account_id = ?1",
            "UPDATE transports SET account_id = NULL WHERE account_id = ?1",
            "UPDATE identities SET account_id = NULL WHERE account_id = ?1",
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
        let now = Timestamp::now();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let before = sqlx::query("SELECT * FROM folders WHERE account_id = ? AND path = ?")
            .bind(account.to_string())
            .bind(path)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .map(|row| folder(&row))
            .transpose()?;
        sqlx::query(
            "INSERT INTO folders (id, account_id, path, role, created_at) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (account_id, path) DO UPDATE SET role = excluded.role",
        )
        .bind(FolderId::new().to_string())
        .bind(account.to_string())
        .bind(path)
        .bind(role.map(|r| r.as_str()))
        .bind(now.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let row = sqlx::query("SELECT * FROM folders WHERE account_id = ? AND path = ?")
            .bind(account.to_string())
            .bind(path)
            .fetch_one(&mut *tx)
            .await
            .map_err(backend)?;
        let stored = folder(&row)?;
        if before.as_ref().map(|f| f.role) != Some(stored.role) {
            let archive = changes::folder_archive(&mut tx, &stored.id.to_string()).await?;
            changes::log(
                &mut tx,
                changes::Row::new(ChangeKind::Mailbox, archive).folder(stored.id),
                now,
            )
            .await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(stored)
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
        let archived_at = Timestamp::now();
        for other in threads.iter().skip(1) {
            let moved: Vec<String> = sqlx::query_scalar(
                "UPDATE messages SET thread_id = ? WHERE archive_id = ? AND thread_id = ?
                 RETURNING id",
            )
            .bind(thread.to_string())
            .bind(&archive)
            .bind(other)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?;
            for message in moved {
                changes::log(
                    &mut tx,
                    changes::Row::new(ChangeKind::Thread, &archive)
                        .message(message)
                        .thread(other),
                    archived_at,
                )
                .await?;
            }
        }

        let id = MessageId::new();
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
        for (field, list) in [
            ("from", &new.from),
            ("to", &new.to),
            ("cc", &new.cc),
            ("bcc", &new.bcc),
        ] {
            for address in list {
                let Some(email) = people::address_key(&address.email) else {
                    continue;
                };
                sqlx::query(
                    "INSERT OR IGNORE INTO message_addresses
                     (message_id, archive_id, field, email, name, date) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(id.to_string())
                .bind(&archive)
                .bind(field)
                .bind(email)
                .bind(
                    address
                        .name
                        .as_deref()
                        .map(str::trim)
                        .filter(|n| !n.is_empty()),
                )
                .bind(date.as_millis())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
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

        ledger::append(
            &mut tx,
            NewEntry {
                at: archived_at,
                event: LedgerEvent::Archived,
                archive_id: new.archive_id,
                message_id: Some(id),
                blob_id: Some(new.blob_id),
                actor: None,
            },
        )
        .await?;
        changes::log(
            &mut tx,
            changes::Row::new(ChangeKind::Message, &archive)
                .message(id)
                .thread(thread),
            archived_at,
        )
        .await?;

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
        let mut sql = String::from("SELECT m.* FROM messages m");
        if query.folder_id.is_some() {
            sql.push_str(" JOIN locations l ON l.message_id = m.id AND l.removed_at IS NULL AND l.folder_id = ?");
        }
        if query.label.is_some() {
            sql.push_str(
                " JOIN message_labels ml ON ml.message_id = m.id
                  JOIN labels lb ON lb.id = ml.label_id AND lb.name = ?",
            );
        }
        match query.category {
            Some(CategoryFilter::Kind(_)) => {
                sql.push_str(" JOIN message_categories mc ON mc.message_id = m.id AND mc.kind = ?")
            }
            Some(CategoryFilter::Topic(_)) => {
                sql.push_str(" JOIN message_topics mt ON mt.message_id = m.id AND mt.topic = ?")
            }
            None => {}
        }
        sql.push_str(
            " WHERE m.archive_id = ? AND (m.date, m.id) < (?, ?)
              ORDER BY m.date DESC, m.id DESC LIMIT ?",
        );
        let mut statement = sqlx::query(AssertSqlSafe(sql));
        if let Some(folder) = query.folder_id {
            statement = statement.bind(folder.to_string());
        }
        if let Some(label) = &query.label {
            statement = statement.bind(label.clone());
        }
        match query.category {
            Some(CategoryFilter::Kind(kind)) => statement = statement.bind(kind.name()),
            Some(CategoryFilter::Topic(topic)) => statement = statement.bind(topic.name()),
            None => {}
        }
        let rows = statement
            .bind(query.archive_id.to_string())
            .bind(date)
            .bind(&id)
            .bind(i64::from(limit) + 1)
            .fetch_all(&self.pool)
            .await
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
            let named = format!(
                "SELECT ml.message_id, lb.name FROM message_labels ml
                 JOIN labels lb ON lb.id = ml.label_id WHERE ml.message_id IN ({marks})"
            );
            let mut query = sqlx::query(AssertSqlSafe(named));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                let message: MessageId = parse(&get::<String>(&row, "message_id")?)?;
                if let Some(extras) = out.get_mut(&message) {
                    extras.labels.push(get(&row, "name")?);
                }
            }
            let kinds = format!(
                "SELECT message_id, kinds FROM message_insights WHERE message_id IN ({marks})"
            );
            let mut query = sqlx::query(AssertSqlSafe(kinds));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                let message: MessageId = parse(&get::<String>(&row, "message_id")?)?;
                if let Some(extras) = out.get_mut(&message) {
                    extras.insight_kinds = get::<i64>(&row, "kinds")? as u64;
                }
            }
            let categories = format!(
                "SELECT message_id, kind, list_topic FROM message_categories
                 WHERE message_id IN ({marks})"
            );
            let mut query = sqlx::query(AssertSqlSafe(categories));
            for id in chunk {
                query = query.bind(id.to_string());
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                let message: MessageId = parse(&get::<String>(&row, "message_id")?)?;
                if let Some(extras) = out.get_mut(&message) {
                    extras.kind = get::<Option<String>>(&row, "kind")?
                        .as_deref()
                        .and_then(holtmail_category::MessageKind::from_name);
                    extras.topic = get::<Option<String>>(&row, "list_topic")?
                        .as_deref()
                        .and_then(holtmail_category::Topic::from_name);
                }
            }
        }
        for extras in out.values_mut() {
            extras.flags.sort();
            extras.flags.dedup();
            extras.labels.sort();
        }
        Ok(out)
    }

    async fn messages_by_rfc_id(
        &self,
        archive: ArchiveId,
        ids: &[String],
    ) -> Result<Vec<Message>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query(
            "SELECT * FROM messages WHERE archive_id = ?1
             AND rfc_message_id IN (SELECT value FROM json_each(?2)) ORDER BY date, id",
        )
        .bind(archive.to_string())
        .bind(json(&ids)?)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(message)
        .collect()
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
        let archive = changes::folder_archive(&mut tx, &folder.to_string()).await?;
        let before = sqlx::query(
            "SELECT flags, removed_at FROM locations WHERE folder_id = ? AND message_id = ?",
        )
        .bind(folder.to_string())
        .bind(message.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let kind = match &before {
            None => Some(ChangeKind::Place),
            Some(row) if get::<Option<i64>>(row, "removed_at")?.is_some() => {
                Some(ChangeKind::Place)
            }
            Some(row) => (flag_set(rows::flags(row)?) != flag_set(flags.to_vec()))
                .then_some(ChangeKind::Flags),
        };
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
        if let Some(kind) = kind {
            changes::log(
                &mut tx,
                changes::Row::new(kind, archive)
                    .message(message)
                    .folder(folder),
                seen_at,
            )
            .await?;
        }
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
            "SELECT uid FROM locations WHERE folder_id = ?1 AND uid IS NOT NULL AND removed_at IS NULL
             UNION SELECT uid FROM deleted_locations WHERE folder_id = ?1
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
        let mut archive = None;
        for uid in uids {
            let removed: Option<String> = sqlx::query_scalar(
                "UPDATE locations SET removed_at = ?
                 WHERE folder_id = ? AND uid = ? AND removed_at IS NULL RETURNING message_id",
            )
            .bind(at.as_millis())
            .bind(folder.to_string())
            .bind(signed(*uid)?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            if let Some(message) = removed {
                let archive = match &archive {
                    Some(archive) => archive,
                    None => {
                        archive.insert(changes::folder_archive(&mut tx, &folder.to_string()).await?)
                    }
                };
                changes::log(
                    &mut tx,
                    changes::Row::new(ChangeKind::Place, archive.as_str())
                        .message(message)
                        .folder(folder),
                    at,
                )
                .await?;
            }
            sqlx::query("DELETE FROM deleted_locations WHERE folder_id = ? AND uid = ?")
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
        let mut tx = self.pool.begin().await.map_err(backend)?;
        for statement in [
            "UPDATE locations SET uid = NULL WHERE folder_id = ?",
            "DELETE FROM deleted_locations WHERE folder_id = ?",
        ] {
            sqlx::query(statement)
                .bind(folder.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(())
    }

    async fn set_flags(
        &self,
        folder: FolderId,
        uid: u64,
        flags: &[String],
    ) -> Result<bool, StoreError> {
        let wanted = flag_set(flags.to_vec());
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(row) = sqlx::query(
            "SELECT message_id, flags, removed_at FROM locations WHERE folder_id = ? AND uid = ?",
        )
        .bind(folder.to_string())
        .bind(signed(uid)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?
        else {
            return Ok(false);
        };
        if flag_set(rows::flags(&row)?) == wanted {
            return Ok(false);
        }
        sqlx::query("UPDATE locations SET flags = ? WHERE folder_id = ? AND uid = ?")
            .bind(json(&wanted)?)
            .bind(folder.to_string())
            .bind(signed(uid)?)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if get::<Option<i64>>(&row, "removed_at")?.is_none() {
            let archive = changes::folder_archive(&mut tx, &folder.to_string()).await?;
            changes::log(
                &mut tx,
                changes::Row::new(ChangeKind::Flags, archive)
                    .message(get::<String>(&row, "message_id")?)
                    .folder(folder),
                Timestamp::now(),
            )
            .await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }
}

#[async_trait]
impl LabelStore for SqliteStore {
    async fn set_labels(&self, message: MessageId, labels: &[String]) -> Result<bool, StoreError> {
        let mut wanted: Vec<String> = labels.iter().filter(|l| !l.is_empty()).cloned().collect();
        wanted.sort();
        wanted.dedup();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: String = sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
            .bind(message.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("message"))?;
        let mut current: Vec<String> = sqlx::query_scalar(
            "SELECT lb.name FROM message_labels ml JOIN labels lb ON lb.id = ml.label_id
             WHERE ml.message_id = ?",
        )
        .bind(message.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        current.sort();
        if current == wanted {
            return Ok(false);
        }
        let dropped: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM message_labels WHERE message_id = ? RETURNING label_id",
        )
        .bind(message.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        for name in &wanted {
            sqlx::query(
                "INSERT INTO labels (archive_id, name) VALUES (?, ?)
                 ON CONFLICT (archive_id, name) DO NOTHING",
            )
            .bind(&archive)
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "INSERT INTO message_labels (message_id, label_id)
                 SELECT ?, id FROM labels WHERE archive_id = ? AND name = ?",
            )
            .bind(message.to_string())
            .bind(&archive)
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        for label in dropped {
            sqlx::query(
                "DELETE FROM labels WHERE id = ?1
                 AND NOT EXISTS (SELECT 1 FROM message_labels WHERE label_id = ?1)",
            )
            .bind(label)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        changes::log(
            &mut tx,
            changes::Row::new(ChangeKind::Flags, &archive).message(message),
            Timestamp::now(),
        )
        .await?;
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn labels(&self, message: MessageId) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT lb.name FROM message_labels ml JOIN labels lb ON lb.id = ml.label_id
             WHERE ml.message_id = ? ORDER BY lb.name",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)
    }

    async fn list_labels(&self, archive: ArchiveId) -> Result<Vec<LabelSummary>, StoreError> {
        sqlx::query(
            "SELECT lb.name, COUNT(*) AS messages,
                    SUM(NOT EXISTS (
                        SELECT 1 FROM locations l, json_each(l.flags)
                        WHERE l.message_id = ml.message_id AND l.removed_at IS NULL
                          AND lower(json_each.value) = '\\seen')) AS unread
             FROM labels lb JOIN message_labels ml ON ml.label_id = lb.id
             WHERE lb.archive_id = ? GROUP BY lb.id ORDER BY lb.name",
        )
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            Ok(LabelSummary {
                name: get(row, "name")?,
                messages: unsigned(get(row, "messages")?),
                unread: unsigned(get(row, "unread")?),
            })
        })
        .collect()
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
            flags_checked_at: get::<Option<i64>>(&row, "flags_checked_at")?
                .map(Timestamp::from_millis),
            state: get(&row, "state")?,
            synced_at: get::<Option<i64>>(&row, "synced_at")?.map(Timestamp::from_millis),
        })
    }

    async fn set_cursor(&self, folder: FolderId, cursor: &FolderCursor) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO folder_cursors
             (folder_id, uid_validity, last_uid, highest_modseq, flags_checked_at, state,
              synced_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (folder_id) DO UPDATE SET
               uid_validity = excluded.uid_validity, last_uid = excluded.last_uid,
               highest_modseq = excluded.highest_modseq,
               flags_checked_at = excluded.flags_checked_at, state = excluded.state,
               synced_at = excluded.synced_at",
        )
        .bind(folder.to_string())
        .bind(cursor.uid_validity.map(signed).transpose()?)
        .bind(cursor.last_uid.map(signed).transpose()?)
        .bind(cursor.highest_modseq.map(signed).transpose()?)
        .bind(cursor.flags_checked_at.map(|t| t.as_millis()))
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

    async fn set_last_run(&self, account: AccountId, run: &AccountRun) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "INSERT INTO account_runs (account_id, finished_at, new_messages, error)
             SELECT id, ?2, ?3, ?4 FROM accounts WHERE id = ?1
             ON CONFLICT (account_id) DO UPDATE SET
               finished_at = excluded.finished_at, new_messages = excluded.new_messages,
               error = excluded.error",
        )
        .bind(account.to_string())
        .bind(run.finished_at.as_millis())
        .bind(signed(run.new_messages)?)
        .bind(&run.error)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("account"));
        }
        Ok(())
    }

    async fn last_runs(&self) -> Result<HashMap<AccountId, AccountRun>, StoreError> {
        sqlx::query("SELECT * FROM account_runs")
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(|row| {
                Ok((
                    parse(&get::<String>(row, "account_id")?)?,
                    AccountRun {
                        finished_at: Timestamp::from_millis(get(row, "finished_at")?),
                        new_messages: unsigned(get(row, "new_messages")?),
                        error: get(row, "error")?,
                    },
                ))
            })
            .collect()
    }
}

#[async_trait]
impl TaskStore for SqliteStore {
    async fn save_task(&self, task: &TaskRecord) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO tasks
             (id, kind, archive_id, account_id, title, state, started_at, finished_at, done, total,
              summary, cancellable, job)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET
               kind = excluded.kind, archive_id = excluded.archive_id,
               account_id = excluded.account_id, title = excluded.title, state = excluded.state,
               started_at = excluded.started_at, finished_at = excluded.finished_at,
               done = excluded.done, total = excluded.total, summary = excluded.summary,
               cancellable = excluded.cancellable, job = excluded.job",
        )
        .bind(&task.id)
        .bind(task.kind.as_str())
        .bind(task.archive_id.map(|id| id.to_string()))
        .bind(task.account_id.map(|id| id.to_string()))
        .bind(&task.title)
        .bind(task.state.as_str())
        .bind(task.started_at.as_millis())
        .bind(task.finished_at.map(|t| t.as_millis()))
        .bind(signed(task.done)?)
        .bind(task.total.map(signed).transpose()?)
        .bind(&task.summary)
        .bind(task.cancellable)
        .bind(&task.job)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn append_task_log(&self, id: &str, lines: &[TaskLine]) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let next: i64 = sqlx::query_scalar("SELECT log_next FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("task"))?;
        let mut seq = next;
        for line in lines {
            sqlx::query(
                "INSERT INTO task_log (task_id, seq, at, level, text) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(seq)
            .bind(line.at.as_millis())
            .bind(line.level.as_str())
            .bind(&line.text)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            seq += 1;
        }
        sqlx::query("UPDATE tasks SET log_next = ? WHERE id = ?")
            .bind(seq)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        let head = TASK_LOG_HEAD as i64;
        let tail = (TASK_LOG_LINES - TASK_LOG_HEAD) as i64;
        sqlx::query("DELETE FROM task_log WHERE task_id = ? AND seq >= ? AND seq < ?")
            .bind(id)
            .bind(head)
            .bind(seq - tail)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn recent_tasks(&self, limit: u32) -> Result<Vec<StoredTask>, StoreError> {
        let rows = sqlx::query("SELECT * FROM tasks ORDER BY started_at DESC, id DESC LIMIT ?")
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let record = task_record(row)?;
            let log = sqlx::query("SELECT * FROM task_log WHERE task_id = ? ORDER BY seq")
                .bind(&record.id)
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?
                .iter()
                .map(|line| {
                    Ok(TaskLine {
                        at: Timestamp::from_millis(get(line, "at")?),
                        level: TaskLevel::parse(&get::<String>(line, "level")?)
                            .unwrap_or(TaskLevel::Info),
                        text: get(line, "text")?,
                    })
                })
                .collect::<Result<_, StoreError>>()?;
            out.push(StoredTask { record, log });
        }
        Ok(out)
    }

    async fn interrupt_tasks(&self, at: Timestamp) -> Result<Vec<TaskRecord>, StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "UPDATE tasks SET state = 'interrupted', finished_at = ? WHERE state = 'running'
             RETURNING *",
        )
        .bind(at.as_millis())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(task_record)
        .collect()
    }

    async fn prune_tasks(&self, keep: u32) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let old: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE state != 'running'
             ORDER BY started_at DESC, id DESC LIMIT -1 OFFSET ?",
        )
        .bind(i64::from(keep))
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        for id in &old {
            for statement in [
                "DELETE FROM task_log WHERE task_id = ?",
                "DELETE FROM tasks WHERE id = ?",
            ] {
                sqlx::query(statement)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(old.len() as u64)
    }
}

fn flag_set(mut flags: Vec<String>) -> Vec<String> {
    flags.sort();
    flags.dedup();
    flags
}

fn corrupt(value: &str) -> StoreError {
    StoreError::Invalid(format!("corrupt value {value:?} in the database"))
}

fn task_record(row: &sqlx::sqlite::SqliteRow) -> Result<TaskRecord, StoreError> {
    let kind: String = get(row, "kind")?;
    let state: String = get(row, "state")?;
    Ok(TaskRecord {
        id: get(row, "id")?,
        kind: TaskKind::parse(&kind).ok_or_else(|| corrupt(&kind))?,
        archive_id: get::<Option<String>>(row, "archive_id")?
            .map(|id| parse(&id))
            .transpose()?,
        account_id: get::<Option<String>>(row, "account_id")?
            .map(|id| parse(&id))
            .transpose()?,
        title: get(row, "title")?,
        state: TaskState::parse(&state).ok_or_else(|| corrupt(&state))?,
        started_at: Timestamp::from_millis(get(row, "started_at")?),
        finished_at: get::<Option<i64>>(row, "finished_at")?.map(Timestamp::from_millis),
        done: unsigned(get(row, "done")?),
        total: get::<Option<i64>>(row, "total")?.map(unsigned),
        summary: get(row, "summary")?,
        cancellable: get(row, "cancellable")?,
        job: get(row, "job")?,
    })
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
        let kept = json(&archives.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        sqlx::query(
            "DELETE FROM archive_grants WHERE user_id = ?
             AND archive_id NOT IN (SELECT value FROM json_each(?))",
        )
        .bind(&user)
        .bind(&kept)
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

    async fn session_started_at(
        &self,
        hash: &SessionHash,
    ) -> Result<Option<Timestamp>, StoreError> {
        let started: Option<i64> =
            sqlx::query_scalar("SELECT created_at FROM sessions WHERE token_hash = ?")
                .bind(&hash[..])
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        Ok(started.map(Timestamp::from_millis))
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

#[async_trait]
impl RemoteImageStore for SqliteStore {
    async fn remote_copy(&self, key: &str) -> Result<Option<RemoteCopy>, StoreError> {
        sqlx::query("SELECT * FROM remote_images WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| remote_copy(&row))
            .transpose()
    }

    async fn save_remote_copy(&self, copy: &RemoteCopy) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO remote_images (key, url, blob_id, content_type, size, fetched_at, error)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (key) DO UPDATE SET url = excluded.url, blob_id = excluded.blob_id,
             content_type = excluded.content_type, size = excluded.size,
             fetched_at = excluded.fetched_at, error = excluded.error",
        )
        .bind(&copy.key)
        .bind(&copy.url)
        .bind(copy.blob_id.as_ref().map(ToString::to_string))
        .bind(&copy.content_type)
        .bind(signed(copy.size)?)
        .bind(copy.fetched_at.as_millis())
        .bind(&copy.error)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn unscanned_messages(
        &self,
        archive: ArchiveId,
        when: ArchivedWhen,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError> {
        let (since, before) = archived_bounds(when);
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT m.id FROM messages m WHERE m.archive_id = ?1
             AND m.archived_at >= ?2 AND m.archived_at < ?3
             AND NOT EXISTS (SELECT 1 FROM remote_scans s WHERE s.message_id = m.id)
             ORDER BY m.archived_at, m.id LIMIT ?4",
        )
        .bind(archive.to_string())
        .bind(since)
        .bind(before)
        .bind(limit.min(MAX_PAGE))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        ids.iter().map(|id| parse(id)).collect()
    }

    async fn count_unscanned(
        &self,
        archive: ArchiveId,
        when: ArchivedWhen,
    ) -> Result<u64, StoreError> {
        let (since, before) = archived_bounds(when);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages m WHERE m.archive_id = ?1
             AND m.archived_at >= ?2 AND m.archived_at < ?3
             AND NOT EXISTS (SELECT 1 FROM remote_scans s WHERE s.message_id = m.id)",
        )
        .bind(archive.to_string())
        .bind(since)
        .bind(before)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(unsigned(count))
    }

    async fn remote_copy_totals(&self, archive: ArchiveId) -> Result<RemoteCopyTotals, StoreError> {
        let row = sqlx::query(
            "SELECT COUNT(blob_id) AS images, COALESCE(SUM(size), 0) AS bytes,
                    COUNT(*) - COUNT(blob_id) AS failed
             FROM remote_images WHERE key IN
             (SELECT l.key FROM message_remote_images l JOIN messages m ON m.id = l.message_id
              WHERE m.archive_id = ?)",
        )
        .bind(archive.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(RemoteCopyTotals {
            images: unsigned(get(&row, "images")?),
            bytes: unsigned(get(&row, "bytes")?),
            failed: unsigned(get(&row, "failed")?),
        })
    }

    async fn failed_remote_copies(
        &self,
        archive: ArchiveId,
        tried_before: Timestamp,
        limit: u32,
    ) -> Result<Vec<RemoteCopy>, StoreError> {
        sqlx::query(
            "SELECT * FROM remote_images WHERE blob_id IS NULL AND fetched_at < ?2 AND key IN
             (SELECT l.key FROM message_remote_images l JOIN messages m ON m.id = l.message_id
              WHERE m.archive_id = ?1)
             ORDER BY fetched_at, key LIMIT ?3",
        )
        .bind(archive.to_string())
        .bind(tried_before.as_millis())
        .bind(limit.min(MAX_PAGE))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(remote_copy)
        .collect()
    }

    async fn record_remote_scan(
        &self,
        message: MessageId,
        keys: &[String],
    ) -> Result<(), StoreError> {
        let message = message.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let done = sqlx::query(
            "INSERT INTO remote_scans (message_id, scanned_at)
             SELECT id, ? FROM messages WHERE id = ?
             ON CONFLICT (message_id) DO UPDATE SET scanned_at = excluded.scanned_at",
        )
        .bind(Timestamp::now().as_millis())
        .bind(&message)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("message"));
        }
        for key in keys {
            sqlx::query(
                "INSERT OR IGNORE INTO message_remote_images (message_id, key)
                 SELECT ?, key FROM remote_images WHERE key = ?",
            )
            .bind(&message)
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn message_remote_copies(
        &self,
        message: MessageId,
    ) -> Result<Vec<RemoteCopy>, StoreError> {
        sqlx::query(
            "SELECT r.* FROM remote_images r JOIN message_remote_images l ON l.key = r.key
             WHERE l.message_id = ? AND r.blob_id IS NOT NULL",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(remote_copy)
        .collect()
    }
}

#[async_trait]
impl SettingsStore for SqliteStore {
    async fn setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)
    }

    async fn set_setting(&self, key: &str, value: Option<&str>) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let query = match value {
            Some(value) => sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value),
            None => sqlx::query("DELETE FROM settings WHERE key = ?").bind(key),
        };
        query.execute(&self.pool).await.map_err(backend)?;
        Ok(())
    }
}

/// Removes the saved remote images of `keys` that no message links to any more and returns
/// their blobs.
pub(crate) async fn drop_unlinked_remote_images(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    keys: Vec<String>,
) -> Result<Vec<String>, StoreError> {
    let mut blobs = Vec::new();
    for key in keys {
        let gone: Option<Option<String>> = sqlx::query_scalar(
            "DELETE FROM remote_images WHERE key = ?1
             AND NOT EXISTS (SELECT 1 FROM message_remote_images WHERE key = ?1)
             RETURNING blob_id",
        )
        .bind(&key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?;
        blobs.extend(gone.flatten());
    }
    Ok(blobs)
}

/// Of `blobs`, those nothing references any more, with what was derived from them when nothing
/// else uses that; their derived records are dropped. Sorted, each once.
pub(crate) async fn release(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mut blobs: Vec<String>,
) -> Result<Vec<BlobId>, StoreError> {
    blobs.sort();
    blobs.dedup();
    let mut orphans = Vec::new();
    for blob in blobs {
        if blob_used(tx, &blob).await? {
            continue;
        }
        let made: Vec<Option<String>> =
            sqlx::query_scalar("DELETE FROM derived WHERE source_blob = ? RETURNING blob_id")
                .bind(&blob)
                .fetch_all(&mut **tx)
                .await
                .map_err(backend)?;
        orphans.push(parse(&blob)?);
        for derived in made.into_iter().flatten() {
            if !blob_used(tx, &derived).await? {
                orphans.push(parse(&derived)?);
            }
        }
    }
    orphans.sort();
    orphans.dedup();
    Ok(orphans)
}

async fn blob_used(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    blob: &str,
) -> Result<bool, StoreError> {
    let used: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM messages WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM attachments WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM remote_images WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM derived WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM item_versions WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM item_versions WHERE photo_blob_id = ?1
         UNION ALL SELECT 1 FROM outbox WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM draft_blobs WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM mail_changes WHERE blob_id = ?1
         UNION ALL SELECT 1 FROM item_changes WHERE blob_id = ?1 LIMIT 1",
    )
    .bind(blob)
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(used.is_some())
}

#[async_trait]
impl DerivedStore for SqliteStore {
    async fn derived(
        &self,
        source: &BlobId,
        kind: DerivedKind,
    ) -> Result<Option<Derived>, StoreError> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT blob_id FROM derived WHERE source_blob = ? AND kind = ?")
                .bind(source.to_string())
                .bind(kind.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        row.map(|blob| match blob {
            Some(blob) => parse(&blob).map(Derived::Blob),
            None => Ok(Derived::Nothing),
        })
        .transpose()
    }

    async fn set_derived(
        &self,
        source: &BlobId,
        kind: DerivedKind,
        derived: &Derived,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let blob = match derived {
            Derived::Blob(blob) => Some(blob.to_string()),
            Derived::Nothing => None,
        };
        sqlx::query(
            "INSERT INTO derived (source_blob, kind, blob_id, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (source_blob, kind) DO UPDATE
             SET blob_id = excluded.blob_id, created_at = excluded.created_at",
        )
        .bind(source.to_string())
        .bind(kind.as_str())
        .bind(blob)
        .bind(Timestamp::now().as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn clear_derived(&self, kind: DerivedKind) -> Result<Vec<BlobId>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let rows: Vec<Option<String>> =
            sqlx::query_scalar("DELETE FROM derived WHERE kind = ? RETURNING blob_id")
                .bind(kind.as_str())
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?;
        let mut made: Vec<String> = rows.into_iter().flatten().collect();
        made.sort();
        made.dedup();
        let mut orphans = Vec::new();
        for blob in made {
            if !blob_used(&mut tx, &blob).await? {
                orphans.push(parse(&blob)?);
            }
        }
        tx.commit().await.map_err(backend)?;
        orphans.sort();
        Ok(orphans)
    }

    async fn forget_derived(
        &self,
        source: &BlobId,
        kind: DerivedKind,
    ) -> Result<Option<BlobId>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let made: Option<Option<String>> = sqlx::query_scalar(
            "DELETE FROM derived WHERE source_blob = ? AND kind = ? RETURNING blob_id",
        )
        .bind(source.to_string())
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let orphan = match made.flatten() {
            Some(blob) if !blob_used(&mut tx, &blob).await? => Some(parse(&blob)?),
            _ => None,
        };
        tx.commit().await.map_err(backend)?;
        Ok(orphan)
    }
}

#[async_trait]
impl InventoryStore for SqliteStore {
    async fn referenced_blobs(
        &self,
        after: Option<&BlobId>,
        limit: u32,
    ) -> Result<Vec<BlobId>, StoreError> {
        let after = after.map(BlobId::to_string).unwrap_or_default();
        let limit = i64::from(limit.min(10_000));
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT blob_id FROM (
                 SELECT * FROM (SELECT DISTINCT blob_id FROM messages WHERE blob_id > ?1
                                ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM attachments WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM remote_images WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM derived WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM item_versions WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT photo_blob_id FROM item_versions
                                      WHERE photo_blob_id > ?1 ORDER BY photo_blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM outbox WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM draft_blobs WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM mail_changes WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
                 UNION SELECT * FROM (SELECT DISTINCT blob_id FROM item_changes WHERE blob_id > ?1
                                      ORDER BY blob_id LIMIT ?2)
             ) ORDER BY blob_id LIMIT ?2",
        )
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter().map(|blob| parse(blob)).collect()
    }

    async fn is_referenced(&self, blob: &BlobId) -> Result<bool, StoreError> {
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM attachments WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM remote_images WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM derived WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM item_versions WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM item_versions WHERE photo_blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM outbox WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM draft_blobs WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM mail_changes WHERE blob_id = ?1)
                 OR EXISTS (SELECT 1 FROM item_changes WHERE blob_id = ?1)",
        )
        .bind(blob.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(found)
    }
}

fn archived_bounds(when: ArchivedWhen) -> (i64, i64) {
    match when {
        ArchivedWhen::Any => (i64::MIN, i64::MAX),
        ArchivedWhen::Since(at) => (at.as_millis(), i64::MAX),
        ArchivedWhen::Before(at) => (i64::MIN, at.as_millis()),
    }
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

    #[tokio::test(flavor = "multi_thread")]
    async fn times_category_counts_on_a_generated_archive() {
        use holtmail_category::{CategorySource, MessageKind, TopicSet, TrustState};
        use std::time::Instant;

        const MESSAGES: i64 = if cfg!(debug_assertions) {
            100_000
        } else {
            500_000
        };
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        let archive = store.create_archive("Generated").await.unwrap().id;
        for statement in [
            "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < ?2)
             INSERT INTO messages (id, archive_id, thread_id, blob_id, size, from_json, to_json,
                 cc_json, bcc_json, archived_at, date, preview, attachment_count)
             SELECT printf('00000000-0000-7000-8000-%012d', n), ?1,
                 printf('00000000-0000-7000-9000-%012d', n), printf('%064d', n), 100,
                 '[]', '[]', '[]', '[]', n, n, '', 0 FROM s",
            "INSERT INTO message_categories (message_id, archive_id, derive_fp, trust, one_click_ok,
                 own, sender_address, sender_domain, derived_kind, derived_kind_source,
                 derived_topics, derived_sources, kind)
             SELECT id, archive_id, 'd1', 'unknown', 0, 0,
                 CASE WHEN date % 10 = 0 THEN 'news@big.example.com'
                      ELSE 'a' || (date % 1000) || '@example.org' END,
                 CASE WHEN date % 10 = 0 THEN 'big.example.com' ELSE 'example.org' END,
                 'bulk', 'headers', 0, '{}',
                 CASE date % 4 WHEN 0 THEN 'personal' WHEN 1 THEN 'list' WHEN 2 THEN 'bulk'
                      ELSE 'notification' END
             FROM messages WHERE archive_id = ?1 AND ?2 > 0",
            "INSERT INTO message_topics (message_id, archive_id, topic)
             SELECT id, archive_id, 'purchases' FROM messages WHERE archive_id = ?1 AND date % 3 = 0
             UNION ALL SELECT id, archive_id, 'travel' FROM messages
                 WHERE archive_id = ?1 AND date % 5 = 0
             UNION ALL SELECT id, archive_id, 'health' FROM messages
                 WHERE archive_id = ?1 AND date % 7 = 0 AND ?2 > 0",
        ] {
            sqlx::query(statement)
                .bind(archive.to_string())
                .bind(MESSAGES)
                .execute(&store.pool)
                .await
                .unwrap();
        }

        let started = Instant::now();
        let counts = store.category_counts(archive, false).await.unwrap();
        println!(
            "category_counts over {MESSAGES} messages: {:?}",
            started.elapsed()
        );
        assert_eq!(
            counts.kinds.iter().map(|(_, n)| *n).sum::<u64>(),
            MESSAGES as u64
        );

        let started = Instant::now();
        let (_, changed) = store
            .put_category_override(
                &CategoryOverride {
                    id: CategoryOverrideId::new(),
                    archive_id: archive,
                    scope: OverrideScope::Domain,
                    key: "big.example.com".into(),
                    kind: Some(MessageKind::Personal),
                    topics: Some(TopicSet::EMPTY),
                    created_at: Timestamp::from_secs(1),
                    created_by: UserId::new(),
                },
                CategoryPolicy::default(),
            )
            .await
            .unwrap();
        println!(
            "override on a {}-message domain: {:?}",
            MESSAGES / 10,
            started.elapsed()
        );
        assert_eq!(changed.len(), (MESSAGES / 10) as usize);

        let writes: Vec<CategoryWrite> = (1..=500)
            .map(|n| CategoryWrite {
                message: format!("00000000-0000-7000-8000-{n:012}").parse().unwrap(),
                archive,
                derive_fp: "d2".into(),
                trust: TrustState::Unknown,
                trust_authserv: None,
                one_click_ok: false,
                own: false,
                sender_address: Some(format!("a{n}@example.org")),
                sender_domain: Some("example.org".into()),
                derived_kind: MessageKind::Notification,
                derived_kind_source: CategorySource::Headers,
                derived_topics: TopicSet::EMPTY,
                derived_sources: Default::default(),
            })
            .collect();
        let started = Instant::now();
        store
            .set_message_categories(&writes, CategoryPolicy::default())
            .await
            .unwrap();
        println!("set_message_categories, 500 rows: {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn names_messages_the_ledger_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        let archive = store.create_archive("Ledger").await.unwrap().id;
        let mut ids = Vec::new();
        for n in 1..=4 {
            let new = holtmail_store::testing::message(archive, n, 1_741_600_000 + i64::from(n));
            ids.push(store.insert_message(new).await.unwrap().message_id);
        }
        sqlx::query("DELETE FROM ledger")
            .execute(&store.pool)
            .await
            .unwrap();
        let gaps = store.ledger_gaps(2).await.unwrap();
        assert_eq!((gaps.unentered.len(), gaps.unentered_count), (2, 4));
        assert_eq!(store.enter_unentered(3).await.unwrap(), 3);
        assert_eq!(store.enter_unentered(3).await.unwrap(), 1);
        assert_eq!(store.enter_unentered(3).await.unwrap(), 0);
        assert!(store.ledger_gaps(10).await.unwrap().is_empty());
        let entered: Vec<_> = store
            .ledger_entries(0, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.message_id.unwrap())
            .collect();
        assert_eq!(entered, ids, "not entered oldest first");

        sqlx::query("UPDATE messages SET blob_id = ? WHERE id = ?")
            .bind(BlobId::of(b"swapped bytes").to_string())
            .bind(ids[0].to_string())
            .execute(&store.pool)
            .await
            .unwrap();
        for table in ["attachments", "message_addresses", "message_parents"] {
            sqlx::query(AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE message_id = ?"
            )))
            .bind(ids[1].to_string())
            .execute(&store.pool)
            .await
            .unwrap();
        }
        sqlx::query("DELETE FROM messages WHERE id = ?")
            .bind(ids[1].to_string())
            .execute(&store.pool)
            .await
            .unwrap();
        let gaps = store.ledger_gaps(10).await.unwrap();
        assert_eq!(gaps.changed, [ids[0]]);
        assert_eq!(gaps.vanished, [ids[1]]);
        assert_eq!(
            (
                gaps.unentered_count,
                gaps.changed_count,
                gaps.vanished_count
            ),
            (0, 1, 1)
        );
    }

    #[tokio::test]
    async fn snapshots_while_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        let id = store.create_archive("Snap").await.unwrap().id;
        let copy = dir.path().join("copy.db");
        store.snapshot(&copy).await.unwrap();
        store.create_archive("After").await.unwrap();
        assert!(store.snapshot(&copy).await.is_err(), "never overwrites");
        let snapshot = SqliteStore::open(&copy).await.unwrap();
        let archives = snapshot.list_archives().await.unwrap();
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].id, id);
    }

    #[tokio::test]
    async fn fills_message_addresses_of_older_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        let archive = store.create_archive("A").await.unwrap().id;
        let mut message = holtmail_store::testing::message(archive, 1, 1_700_000_000);
        message.cc = vec![holtmail_core::Address {
            name: Some(" Cleo ".into()),
            email: " Cleo@Example.ORG ".into(),
        }];
        message.bcc = vec![
            holtmail_core::Address {
                name: None,
                email: "not an address".into(),
            },
            holtmail_core::Address {
                name: Some("\tDee\r\n".into()),
                email: "\t\r\nDee@Example.com\n\u{a0}".into(),
            },
            holtmail_core::Address {
                name: Some("\u{3000}".into()),
                email: "\n@example.com".into(),
            },
        ];
        store.insert_message(message).await.unwrap();
        let rows = |store: &SqliteStore| {
            let pool = store.pool.clone();
            async move {
                sqlx::query_as::<_, (String, String, Option<String>)>(
                    "SELECT field, email, name FROM message_addresses ORDER BY field, email",
                )
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let inserted = rows(&store).await;
        assert_eq!(
            inserted,
            vec![
                ("bcc".into(), "dee@example.com".into(), Some("Dee".into())),
                ("cc".into(), "cleo@example.org".into(), Some("Cleo".into())),
                ("from".into(), "ada@example.com".into(), Some("Ada".into())),
                ("to".into(), "bob@example.org".into(), None),
            ]
        );
        sqlx::query("DELETE FROM message_addresses")
            .execute(&store.pool)
            .await
            .unwrap();
        let migration = include_str!("../migrations/0013_people.sql");
        let fill = migration
            .split(';')
            .find(|statement| statement.contains("INSERT OR IGNORE INTO message_addresses"))
            .unwrap();
        sqlx::query(AssertSqlSafe(fill.to_owned()))
            .execute(&store.pool)
            .await
            .unwrap();
        assert_eq!(
            rows(&store).await,
            inserted,
            "the migration fills the same rows"
        );
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
                label: None,
                category: None,
                archive_id: archive,
                folder_id: None,
                after: Some(PageCursor("garbage".into())),
                limit: 10,
            })
            .await;
        assert!(matches!(result, Err(StoreError::Invalid(_))));
    }
}
