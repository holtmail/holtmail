use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use holtmail_core::{ArchiveId, FolderId, MessageId, Timestamp, UserId};
use holtmail_store::*;
use sqlx::{AssertSqlSafe, Sqlite, Transaction};

use crate::SqliteStore;
use crate::changes::{Row, log};
use crate::rows::{backend, get, parse, unsigned};

type Tx<'a> = Transaction<'a, Sqlite>;

const CHUNK: usize = 200;

fn add_flag(flags: &mut Vec<String>, flag: &str) {
    if !flags.iter().any(|f| f.eq_ignore_ascii_case(flag)) {
        flags.push(flag.to_owned());
    }
}

fn remove_flag(flags: &mut Vec<String>, flag: &str) {
    flags.retain(|f| !f.eq_ignore_ascii_case(flag));
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(", ")
}

async fn message_archive(tx: &mut Tx<'_>, message: MessageId) -> Result<String, StoreError> {
    sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
        .bind(message.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .ok_or(StoreError::NotFound("message"))
}

async fn require(
    tx: &mut Tx<'_>,
    sql: &'static str,
    id: String,
    what: &'static str,
) -> Result<(), StoreError> {
    let found: Option<i64> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?;
    found.map(|_| ()).ok_or(StoreError::NotFound(what))
}

async fn require_user(tx: &mut Tx<'_>, user: UserId) -> Result<(), StoreError> {
    require(
        tx,
        "SELECT 1 FROM users WHERE id = ?",
        user.to_string(),
        "user",
    )
    .await
}

#[derive(Default)]
struct Snapshot {
    known: HashSet<String>,
    places: HashMap<String, Vec<(String, Vec<String>, bool)>>,
    held: HashMap<(String, String), Vec<MailOp>>,
    local: HashMap<String, Vec<(String, bool)>>,
}

async fn snapshot(
    tx: &mut Tx<'_>,
    ids: &[String],
    user: &str,
    local: bool,
    out: &mut Snapshot,
) -> Result<(), StoreError> {
    let list = marks(ids.len());
    let mut query = sqlx::query_scalar::<_, String>(AssertSqlSafe(format!(
        "SELECT id FROM messages WHERE id IN ({list})"
    )));
    for id in ids {
        query = query.bind(id);
    }
    out.known
        .extend(query.fetch_all(&mut **tx).await.map_err(backend)?);

    let mut query = sqlx::query(AssertSqlSafe(format!(
        "SELECT message_id, folder_id, flags, removed_at FROM locations
         WHERE message_id IN ({list}) ORDER BY first_seen_at, folder_id"
    )));
    for id in ids {
        query = query.bind(id);
    }
    for row in query.fetch_all(&mut **tx).await.map_err(backend)? {
        let message: String = get(&row, "message_id")?;
        let flags: Vec<String> =
            serde_json::from_str(&get::<String>(&row, "flags")?).map_err(backend)?;
        let live = get::<Option<i64>>(&row, "removed_at")?.is_none();
        out.places
            .entry(message)
            .or_default()
            .push((get(&row, "folder_id")?, flags, live));
    }

    let mut query = sqlx::query(AssertSqlSafe(format!(
        "SELECT message_id, folder_id, payload FROM mail_changes
         WHERE message_id IN ({list}) AND kind = 'append' AND state IN ('held', 'sending')
         ORDER BY id"
    )));
    for id in ids {
        query = query.bind(id);
    }
    for row in query.fetch_all(&mut **tx).await.map_err(backend)? {
        let message: String = get(&row, "message_id")?;
        let folder: String = get(&row, "folder_id")?;
        let MailOp::Append { flags, .. } =
            serde_json::from_str(&get::<String>(&row, "payload")?).map_err(backend)?
        else {
            continue;
        };
        let places = out.places.entry(message).or_default();
        if !places.iter().any(|(f, _, live)| *live && *f == folder) {
            places.push((folder, flags, true));
        }
    }

    let mut query = sqlx::query(AssertSqlSafe(format!(
        "SELECT message_id, folder_id, payload FROM mail_changes
         WHERE message_id IN ({list}) AND kind = 'flags' AND state IN ('held', 'sending')
         ORDER BY id"
    )));
    for id in ids {
        query = query.bind(id);
    }
    for row in query.fetch_all(&mut **tx).await.map_err(backend)? {
        let op: MailOp = serde_json::from_str(&get::<String>(&row, "payload")?).map_err(backend)?;
        out.held
            .entry((get(&row, "message_id")?, get(&row, "folder_id")?))
            .or_default()
            .push(op);
    }

    let mut query = sqlx::query(AssertSqlSafe(format!(
        "SELECT c.message_id, c.folder_id, json_extract(c.payload, '$.target') AS target
         FROM mail_changes c
         WHERE c.message_id IN ({list}) AND c.kind = 'move'
         AND (c.state IN ('held', 'sending') OR (c.state = 'applied' AND NOT EXISTS (
             SELECT 1 FROM locations l WHERE l.message_id = c.message_id
             AND l.folder_id = json_extract(c.payload, '$.target')
             AND l.last_seen_at > COALESCE(c.sent_at, c.updated_at))))
         ORDER BY c.id"
    )));
    for id in ids {
        query = query.bind(id);
    }
    for row in query.fetch_all(&mut **tx).await.map_err(backend)? {
        let message: String = get(&row, "message_id")?;
        let source: String = get(&row, "folder_id")?;
        let target: String = get(&row, "target")?;
        let places = out.places.entry(message.clone()).or_default();
        if places.iter().any(|(f, _, live)| *live && *f == target) {
            continue;
        }
        let Some((_, synced, _)) = places.iter().find(|(f, _, _)| *f == source) else {
            continue;
        };
        let mut flags = synced.clone();
        apply_held(&out.held, &message, &source, &mut flags);
        places.push((target, flags, true));
    }

    if local {
        let mut query = sqlx::query(AssertSqlSafe(format!(
            "SELECT message_id, flag, present FROM local_flags
             WHERE user_id = ? AND message_id IN ({list}) ORDER BY flag_key"
        )))
        .bind(user);
        for id in ids {
            query = query.bind(id);
        }
        for row in query.fetch_all(&mut **tx).await.map_err(backend)? {
            out.local
                .entry(get(&row, "message_id")?)
                .or_default()
                .push((get(&row, "flag")?, get::<i64>(&row, "present")? != 0));
        }
    }
    Ok(())
}

fn apply_held(
    held: &HashMap<(String, String), Vec<MailOp>>,
    message: &str,
    folder: &str,
    flags: &mut Vec<String>,
) {
    for op in held
        .get(&(message.to_owned(), folder.to_owned()))
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if let MailOp::Flags { add, remove } = op {
            for flag in add {
                add_flag(flags, flag);
            }
            for flag in remove {
                remove_flag(flags, flag);
            }
        }
    }
}

fn effective(snapshot: &Snapshot, message: &str, folders: Option<&HashSet<String>>) -> Vec<String> {
    let places = snapshot
        .places
        .get(message)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let chosen: Vec<&(String, Vec<String>, bool)> = match folders {
        Some(folders) => places
            .iter()
            .filter(|(folder, _, live)| *live && folders.contains(folder))
            .collect(),
        None if places.iter().any(|(_, _, live)| *live) => {
            places.iter().filter(|(_, _, live)| *live).collect()
        }
        None => places.iter().collect(),
    };
    let mut flags = Vec::new();
    for (folder, synced, _) in chosen {
        let mut place = Vec::new();
        for flag in synced {
            add_flag(&mut place, flag);
        }
        apply_held(&snapshot.held, message, folder, &mut place);
        for flag in &place {
            add_flag(&mut flags, flag);
        }
    }
    for (flag, present) in snapshot
        .local
        .get(message)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if *present {
            add_flag(&mut flags, flag);
        } else {
            remove_flag(&mut flags, flag);
        }
    }
    flags.sort();
    flags
}

#[async_trait]
impl MailViewStore for SqliteStore {
    async fn effective_flags(
        &self,
        query: &EffectiveFlagsQuery,
    ) -> Result<HashMap<MessageId, Vec<String>>, StoreError> {
        let folders: Option<HashSet<String>> = query
            .folders
            .as_ref()
            .map(|f| f.iter().map(ToString::to_string).collect());
        let mut ids: Vec<String> = query.messages.iter().map(ToString::to_string).collect();
        ids.sort();
        ids.dedup();
        let user = query.user.to_string();
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let mut snap = Snapshot::default();
        for chunk in ids.chunks(CHUNK) {
            snapshot(&mut tx, chunk, &user, query.local_flags, &mut snap).await?;
        }
        tx.commit().await.map_err(backend)?;
        let mut out = HashMap::new();
        for id in &ids {
            if snap.known.contains(id) {
                out.insert(parse(id)?, effective(&snap, id, folders.as_ref()));
            }
        }
        Ok(out)
    }

    async fn set_local_flags(
        &self,
        user: UserId,
        message: MessageId,
        add: &[String],
        remove: &[String],
        at: Timestamp,
    ) -> Result<bool, StoreError> {
        for flag in add.iter().chain(remove) {
            if flag.trim().is_empty() {
                return Err(StoreError::Invalid("an empty flag".into()));
            }
            if flag.eq_ignore_ascii_case(DELETED_FLAG) {
                return Err(StoreError::Invalid("deleting has no local form".into()));
            }
        }
        if add
            .iter()
            .any(|a| remove.iter().any(|r| r.eq_ignore_ascii_case(a)))
        {
            return Err(StoreError::Invalid(
                "a flag is both added and removed".into(),
            ));
        }
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require_user(&mut tx, user).await?;
        let archive = message_archive(&mut tx, message).await?;
        let mut changed = false;
        for (flag, present) in add
            .iter()
            .map(|f| (f, true))
            .chain(remove.iter().map(|f| (f, false)))
        {
            let key = flag.to_ascii_lowercase();
            let stored: Option<i64> = sqlx::query_scalar(
                "SELECT present FROM local_flags WHERE user_id = ? AND message_id = ? AND flag_key = ?",
            )
            .bind(user.to_string())
            .bind(message.to_string())
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            if stored.map(|p| p != 0) == Some(present) {
                continue;
            }
            sqlx::query(
                "INSERT INTO local_flags
                 (user_id, message_id, archive_id, flag_key, flag, present, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (user_id, message_id, flag_key) DO UPDATE SET
                   present = excluded.present, updated_at = excluded.updated_at",
            )
            .bind(user.to_string())
            .bind(message.to_string())
            .bind(&archive)
            .bind(&key)
            .bind(flag)
            .bind(present)
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            changed = true;
        }
        if changed {
            log(
                &mut tx,
                Row::new(ChangeKind::Flags, &archive)
                    .message(message)
                    .user(user),
                at,
            )
            .await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }

    async fn local_flags(
        &self,
        user: UserId,
        message: MessageId,
    ) -> Result<Vec<LocalFlag>, StoreError> {
        sqlx::query(
            "SELECT flag, present FROM local_flags WHERE user_id = ? AND message_id = ?
             ORDER BY flag_key",
        )
        .bind(user.to_string())
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            Ok(LocalFlag {
                flag: get(row, "flag")?,
                present: get::<i64>(row, "present")? != 0,
            })
        })
        .collect()
    }

    async fn local_flagged(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Vec<LocalFlags>, StoreError> {
        let rows = sqlx::query(
            "SELECT message_id, flag, present FROM local_flags WHERE user_id = ? AND archive_id = ?
             ORDER BY message_id, flag_key",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut out: Vec<LocalFlags> = Vec::new();
        for row in &rows {
            let message: MessageId = parse(&get::<String>(row, "message_id")?)?;
            let flag = LocalFlag {
                flag: get(row, "flag")?,
                present: get::<i64>(row, "present")? != 0,
            };
            match out.last_mut() {
                Some(last) if last.message_id == message => last.flags.push(flag),
                _ => out.push(LocalFlags {
                    message_id: message,
                    flags: vec![flag],
                }),
            }
        }
        Ok(out)
    }

    async fn drop_local_flags(
        &self,
        user: UserId,
        archive: ArchiveId,
        message: Option<MessageId>,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let mut dropped: Vec<String> = sqlx::query_scalar(
            "DELETE FROM local_flags WHERE user_id = ?1 AND archive_id = ?2
             AND (?3 IS NULL OR message_id = ?3) RETURNING message_id",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .bind(message.map(|m| m.to_string()))
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        dropped.sort();
        dropped.dedup();
        for id in &dropped {
            log(
                &mut tx,
                Row::new(ChangeKind::Flags, archive).message(id).user(user),
                at,
            )
            .await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(dropped.len() as u64)
    }

    async fn add_local_place(
        &self,
        user: UserId,
        role: FolderRole,
        message: MessageId,
        at: Timestamp,
    ) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require_user(&mut tx, user).await?;
        let archive = message_archive(&mut tx, message).await?;
        let done = sqlx::query(
            "INSERT OR IGNORE INTO local_places (user_id, archive_id, role, message_id, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(user.to_string())
        .bind(&archive)
        .bind(role.as_str())
        .bind(message.to_string())
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let added = done.rows_affected() > 0;
        if added {
            log(
                &mut tx,
                Row::new(ChangeKind::Place, &archive)
                    .message(message)
                    .user(user),
                at,
            )
            .await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(added)
    }

    async fn remove_local_place(
        &self,
        user: UserId,
        role: FolderRole,
        message: MessageId,
        at: Timestamp,
    ) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: Option<String> = sqlx::query_scalar(
            "DELETE FROM local_places WHERE user_id = ? AND role = ? AND message_id = ?
             RETURNING archive_id",
        )
        .bind(user.to_string())
        .bind(role.as_str())
        .bind(message.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(archive) = archive else {
            return Ok(false);
        };
        log(
            &mut tx,
            Row::new(ChangeKind::Place, archive)
                .message(message)
                .user(user),
            at,
        )
        .await?;
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn local_places(
        &self,
        user: UserId,
        archive: ArchiveId,
        role: Option<FolderRole>,
    ) -> Result<Vec<LocalPlace>, StoreError> {
        sqlx::query(
            "SELECT * FROM local_places WHERE user_id = ?1 AND archive_id = ?2
             AND (?3 IS NULL OR role = ?3) ORDER BY created_at, message_id",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .bind(role.map(|r| r.as_str()))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            let role: String = get(row, "role")?;
            Ok(LocalPlace {
                user_id: parse(&get::<String>(row, "user_id")?)?,
                archive_id: parse(&get::<String>(row, "archive_id")?)?,
                role: FolderRole::parse(&role)
                    .ok_or_else(|| StoreError::Invalid(format!("unknown role {role:?}")))?,
                message_id: parse(&get::<String>(row, "message_id")?)?,
                created_at: Timestamp::from_millis(get(row, "created_at")?),
            })
        })
        .collect()
    }

    async fn set_subscription(
        &self,
        user: UserId,
        archive: ArchiveId,
        mailbox: &str,
        subscribed: bool,
        at: Timestamp,
    ) -> Result<bool, StoreError> {
        if mailbox.is_empty() {
            return Err(StoreError::Invalid("an empty mailbox key".into()));
        }
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require_user(&mut tx, user).await?;
        require(
            &mut tx,
            "SELECT 1 FROM archives WHERE id = ?",
            archive.to_string(),
            "archive",
        )
        .await?;
        let stored: Option<i64> = sqlx::query_scalar(
            "SELECT subscribed FROM served_subscriptions
             WHERE user_id = ? AND archive_id = ? AND mailbox = ?",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .bind(mailbox)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        if stored.map(|s| s != 0) == Some(subscribed) {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO served_subscriptions (user_id, archive_id, mailbox, subscribed, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (user_id, archive_id, mailbox) DO UPDATE SET
               subscribed = excluded.subscribed, updated_at = excluded.updated_at",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .bind(mailbox)
        .bind(subscribed)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        log(
            &mut tx,
            Row::new(ChangeKind::Mailbox, archive).user(user),
            at,
        )
        .await?;
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn subscriptions(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Vec<Subscription>, StoreError> {
        sqlx::query(
            "SELECT mailbox, subscribed FROM served_subscriptions
             WHERE user_id = ? AND archive_id = ? ORDER BY mailbox",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            Ok(Subscription {
                mailbox: get(row, "mailbox")?,
                subscribed: get::<i64>(row, "subscribed")? != 0,
            })
        })
        .collect()
    }

    async fn folder_counts(
        &self,
        folders: &[FolderId],
    ) -> Result<HashMap<FolderId, FolderCounts>, StoreError> {
        let mut ids: Vec<String> = folders.iter().map(ToString::to_string).collect();
        ids.sort();
        ids.dedup();
        let mut out = HashMap::new();
        for chunk in ids.chunks(CHUNK) {
            let list = marks(chunk.len());
            let mut query = sqlx::query(AssertSqlSafe(format!(
                "SELECT f.id,
                        (SELECT COUNT(*) FROM locations l
                         WHERE l.folder_id = f.id AND l.removed_at IS NULL) AS messages,
                        (SELECT COUNT(*) FROM locations l
                         WHERE l.folder_id = f.id AND l.removed_at IS NULL
                         AND NOT EXISTS (SELECT 1 FROM json_each(l.flags)
                                         WHERE lower(json_each.value) = '\\seen')) AS unseen
                 FROM folders f WHERE f.id IN ({list})"
            )));
            for id in chunk {
                query = query.bind(id);
            }
            for row in query.fetch_all(&self.pool).await.map_err(backend)? {
                out.insert(
                    parse(&get::<String>(&row, "id")?)?,
                    FolderCounts {
                        messages: unsigned(get(&row, "messages")?),
                        unseen: unsigned(get(&row, "unseen")?),
                    },
                );
            }
        }
        Ok(out)
    }
}
