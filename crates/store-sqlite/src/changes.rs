use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, Timestamp, UserId};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::sync::watch;

use crate::SqliteStore;
use crate::rows::{backend, get, parse, unsigned};

const MAX_LOG: u32 = 1000;

type Tx<'a> = Transaction<'a, Sqlite>;

/// A change log row to write; ids are kept as their text.
#[derive(Debug, Clone, Default)]
pub(crate) struct Row {
    kind: &'static str,
    archive: String,
    collection: Option<String>,
    item: Option<String>,
    href: Option<String>,
    item_change: Option<String>,
    message: Option<String>,
    folder: Option<String>,
    thread: Option<String>,
    outbox: Option<String>,
    draft: Option<String>,
    user: Option<String>,
}

impl Row {
    pub(crate) fn new(kind: ChangeKind, archive: impl ToString) -> Self {
        Self {
            kind: kind.as_str(),
            archive: archive.to_string(),
            ..Self::default()
        }
    }

    pub(crate) fn item(
        mut self,
        collection: impl ToString,
        item: Option<String>,
        href: Option<String>,
        change: impl ToString,
    ) -> Self {
        self.collection = Some(collection.to_string());
        self.item = item;
        self.href = href;
        self.item_change = Some(change.to_string());
        self
    }

    /// An item whose served upstream state changed: a new version, a return or a removal.
    pub(crate) fn upstream_item(mut self, collection: impl ToString, item: impl ToString) -> Self {
        self.collection = Some(collection.to_string());
        self.item = Some(item.to_string());
        self
    }

    /// An item deleted for good, with the href it was last served under.
    pub(crate) fn purged_item(
        mut self,
        collection: impl ToString,
        item: impl ToString,
        href: String,
    ) -> Self {
        self.collection = Some(collection.to_string());
        self.item = Some(item.to_string());
        self.href = Some(href);
        self
    }

    pub(crate) fn message(mut self, message: impl ToString) -> Self {
        self.message = Some(message.to_string());
        self
    }

    pub(crate) fn folder(mut self, folder: impl ToString) -> Self {
        self.folder = Some(folder.to_string());
        self
    }

    pub(crate) fn thread(mut self, thread: impl ToString) -> Self {
        self.thread = Some(thread.to_string());
        self
    }

    pub(crate) fn outbox(mut self, outbox: impl ToString) -> Self {
        self.outbox = Some(outbox.to_string());
        self
    }

    pub(crate) fn draft(mut self, draft: impl ToString) -> Self {
        self.draft = Some(draft.to_string());
        self
    }

    pub(crate) fn user(mut self, user: impl ToString) -> Self {
        self.user = Some(user.to_string());
        self
    }
}

/// Writes the row in the caller's transaction, which must hold the writer lock.
pub(crate) async fn log(tx: &mut Tx<'_>, row: Row, at: Timestamp) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO changes (archive_id, kind, collection_id, item_id, href, item_change_id,
         message_id, folder_id, thread_id, outbox_id, draft_id, user_id, at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(row.archive)
    .bind(row.kind)
    .bind(row.collection)
    .bind(row.item)
    .bind(row.href)
    .bind(row.item_change)
    .bind(row.message)
    .bind(row.folder)
    .bind(row.thread)
    .bind(row.outbox)
    .bind(row.draft)
    .bind(row.user)
    .bind(at.as_millis())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}

/// The archive of the folder, or `NotFound`.
pub(crate) async fn folder_archive(tx: &mut Tx<'_>, folder: &str) -> Result<String, StoreError> {
    sqlx::query_scalar(
        "SELECT a.archive_id FROM folders f JOIN accounts a ON a.id = f.account_id WHERE f.id = ?",
    )
    .bind(folder)
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?
    .ok_or(StoreError::NotFound("folder"))
}

fn optional<T: std::str::FromStr>(row: &SqliteRow, column: &str) -> Result<Option<T>, StoreError> {
    get::<Option<String>>(row, column)?
        .map(|v| parse(&v))
        .transpose()
}

fn entry(row: &SqliteRow) -> Result<ChangeLogEntry, StoreError> {
    let kind: String = get(row, "kind")?;
    Ok(ChangeLogEntry {
        seq: unsigned(get(row, "seq")?),
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        kind: ChangeKind::parse(&kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown change kind {kind:?}")))?,
        collection_id: optional(row, "collection_id")?,
        item_id: optional(row, "item_id")?,
        href: get(row, "href")?,
        item_change_id: optional(row, "item_change_id")?,
        message_id: optional(row, "message_id")?,
        folder_id: optional(row, "folder_id")?,
        thread_id: optional(row, "thread_id")?,
        outbox_id: optional(row, "outbox_id")?,
        draft_id: optional(row, "draft_id")?,
        user_id: optional(row, "user_id")?,
        at: Timestamp::from_millis(get(row, "at")?),
    })
}

async fn latest(pool: &SqlitePool) -> Result<u64, StoreError> {
    let seq: Option<i64> =
        sqlx::query_scalar("SELECT seq FROM sqlite_sequence WHERE name = 'changes'")
            .fetch_optional(pool)
            .await
            .map_err(backend)?;
    Ok(seq.map_or(0, unsigned))
}

const PRUNED_KEY: &str = "change_log_pruned_through";
const PRUNE_CHUNK: i64 = 5_000;

async fn pruned(pool: &SqlitePool) -> Result<u64, StoreError> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(PRUNED_KEY)
        .fetch_optional(pool)
        .await
        .map_err(backend)?;
    Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
}

fn after(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

struct Watch {
    pool: SqlitePool,
    tick: watch::Receiver<u64>,
    poll: Duration,
    seen: u64,
}

#[async_trait]
impl ChangeWatch for Watch {
    async fn next(&mut self) -> u64 {
        loop {
            if let Ok(seq) = latest(&self.pool).await
                && seq > self.seen
            {
                self.seen = seq;
                return seq;
            }
            tokio::select! {
                changed = self.tick.changed() => {
                    if changed.is_err() {
                        tokio::time::sleep(self.poll).await;
                    }
                }
                () = tokio::time::sleep(self.poll) => {}
            }
        }
    }
}

impl SqliteStore {
    /// Sets how often change watches started later look at the log by themselves, instead of
    /// [`CHANGE_POLL`]; for tests.
    pub fn set_change_poll(&mut self, poll: Duration) {
        self.poll = poll;
    }
}

#[async_trait]
impl ChangeStore for SqliteStore {
    async fn latest_seq(&self) -> Result<u64, StoreError> {
        latest(&self.pool).await
    }

    async fn changes_since(
        &self,
        archive: ArchiveId,
        since: Option<u64>,
        limit: u32,
    ) -> Result<Vec<ChangeLogEntry>, StoreError> {
        sqlx::query("SELECT * FROM changes WHERE archive_id = ? AND seq > ? ORDER BY seq LIMIT ?")
            .bind(archive.to_string())
            .bind(after(since.unwrap_or(0)))
            .bind(i64::from(limit.clamp(1, MAX_LOG)))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(entry)
            .collect()
    }

    async fn all_changes_since(
        &self,
        since: u64,
        limit: u32,
    ) -> Result<Vec<ChangeLogEntry>, StoreError> {
        sqlx::query("SELECT * FROM changes WHERE seq > ? ORDER BY seq LIMIT ?")
            .bind(after(since))
            .bind(i64::from(limit.clamp(1, MAX_LOG)))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(entry)
            .collect()
    }

    async fn archive_heads(
        &self,
        archive: ArchiveId,
        user: Option<UserId>,
    ) -> Result<HashMap<ChangeKind, u64>, StoreError> {
        let mut heads = HashMap::new();
        for kind in ChangeKind::ALL {
            let seq: Option<i64> = sqlx::query_scalar(
                "SELECT MAX(seq) FROM changes WHERE archive_id = ? AND kind = ?
                 AND (user_id IS NULL OR user_id = ?)",
            )
            .bind(archive.to_string())
            .bind(kind.as_str())
            .bind(user.map(|u| u.to_string()))
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
            if let Some(seq) = seq {
                heads.insert(kind, unsigned(seq));
            }
        }
        Ok(heads)
    }

    async fn prune_changes(&self, before: Timestamp) -> Result<u64, StoreError> {
        let first_recent: Option<i64> =
            sqlx::query_scalar("SELECT MIN(seq) FROM changes WHERE at >= ?")
                .bind(before.as_millis())
                .fetch_one(&self.pool)
                .await
                .map_err(backend)?;
        let through = match first_recent {
            Some(seq) => unsigned(seq - 1),
            None => latest(&self.pool).await?,
        };
        let marked = {
            let _write = self.writer.lock().await;
            let old = pruned(&self.pool).await?;
            if through > old {
                sqlx::query(
                    "INSERT INTO settings (key, value) VALUES (?, ?)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                )
                .bind(PRUNED_KEY)
                .bind(through.to_string())
                .execute(&self.pool)
                .await
                .map_err(backend)?;
            }
            old.max(through)
        };
        loop {
            let _write = self.writer.lock().await;
            let done = sqlx::query(
                "DELETE FROM changes WHERE seq IN
                 (SELECT seq FROM changes WHERE seq <= ? ORDER BY seq LIMIT ?)",
            )
            .bind(after(marked))
            .bind(PRUNE_CHUNK)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
            if done.rows_affected() == 0 {
                break;
            }
        }
        Ok(marked)
    }

    async fn pruned_through(&self) -> Result<u64, StoreError> {
        pruned(&self.pool).await
    }

    async fn watch_changes(&self) -> Result<Box<dyn ChangeWatch>, StoreError> {
        let mut tick = self.writer.tick.subscribe();
        tick.mark_unchanged();
        let seen = latest(&self.pool).await?;
        Ok(Box::new(Watch {
            pool: self.pool.clone(),
            tick,
            poll: self.poll,
            seen,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use holtmail_core::Timestamp;
    use holtmail_store::*;

    use super::{Row, log};
    use crate::SqliteStore;

    #[tokio::test]
    async fn a_watch_polls_when_no_writer_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(dir.path().join("db.sqlite"))
            .await
            .unwrap();
        store.set_change_poll(Duration::from_millis(50));
        let archive = store.create_archive("Poll").await.unwrap().id;
        let mut watch = store.watch_changes().await.unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        log(
            &mut tx,
            Row::new(ChangeKind::Message, archive),
            Timestamp::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let seq = tokio::time::timeout(Duration::from_secs(5), watch.next())
            .await
            .expect("the poll finds a row no writer announced");
        assert_eq!(seq, store.latest_seq().await.unwrap());
        assert!(seq > 0);
    }
}
