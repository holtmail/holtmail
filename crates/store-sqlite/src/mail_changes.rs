use async_trait::async_trait;
use holtmail_core::{AccountId, FolderId, MailChangeId, MessageId, OutboxId, Timestamp};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use crate::SqliteStore;
use crate::changes::{self, Row};
use crate::rows::{backend, get, json, parse, signed, unsigned};

const MAX_PAGE: u32 = 500;

type Tx<'a> = Transaction<'a, Sqlite>;

type Query<'q> = sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>;

fn optional<T: std::str::FromStr>(row: &SqliteRow, column: &str) -> Result<Option<T>, StoreError> {
    get::<Option<String>>(row, column)?
        .map(|v| parse(&v))
        .transpose()
}

fn optional_time(row: &SqliteRow, column: &str) -> Result<Option<Timestamp>, StoreError> {
    Ok(get::<Option<i64>>(row, column)?.map(Timestamp::from_millis))
}

fn change(row: &SqliteRow) -> Result<MailChange, StoreError> {
    let state: String = get(row, "state")?;
    let origin: String = get(row, "origin")?;
    Ok(MailChange {
        id: parse(&get::<String>(row, "id")?)?,
        new: NewMailChange {
            account_id: parse(&get::<String>(row, "account_id")?)?,
            folder_id: parse(&get::<String>(row, "folder_id")?)?,
            message_id: optional(row, "message_id")?,
            uid: get::<Option<i64>>(row, "uid")?.map(unsigned),
            op: serde_json::from_str(&get::<String>(row, "payload")?).map_err(backend)?,
            origin: MailChangeOrigin::parse(&origin)
                .ok_or_else(|| StoreError::Invalid(format!("unknown origin {origin:?}")))?,
            user_id: optional(row, "user_id")?,
            device_id: optional(row, "device")?,
            outbox_id: optional(row, "outbox_id")?,
            at: Timestamp::from_millis(get(row, "created_at")?),
        },
        state: MailChangeState::parse(&state)
            .ok_or_else(|| StoreError::Invalid(format!("unknown change state {state:?}")))?,
        attempts: u32::try_from(get::<i64>(row, "attempts")?).unwrap_or(u32::MAX),
        next_attempt_at: optional_time(row, "next_attempt_at")?,
        last_error: get(row, "last_error")?,
        sent_at: optional_time(row, "sent_at")?,
        result_uid: get::<Option<i64>>(row, "result_uid")?.map(unsigned),
        updated_at: Timestamp::from_millis(get(row, "updated_at")?),
    })
}

async fn fetch(tx: &mut Tx<'_>, id: &str) -> Result<Option<MailChange>, StoreError> {
    sqlx::query("SELECT * FROM mail_changes WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .map(|row| change(&row))
        .transpose()
}

async fn folder_account(tx: &mut Tx<'_>, folder: FolderId) -> Result<String, StoreError> {
    sqlx::query_scalar("SELECT account_id FROM folders WHERE id = ?")
        .bind(folder.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .ok_or(StoreError::NotFound("folder"))
}

/// Writes the rows a change's place or flags need: its folder, and a move's target too.
async fn log(tx: &mut Tx<'_>, change: &MailChange, at: Timestamp) -> Result<(), StoreError> {
    let archive: String = sqlx::query_scalar("SELECT archive_id FROM accounts WHERE id = ?")
        .bind(change.new.account_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .ok_or(StoreError::NotFound("account"))?;
    let kind = if change.new.op.needs_confirmation() {
        ChangeKind::Flags
    } else {
        ChangeKind::Place
    };
    let mut folders = vec![change.new.folder_id];
    if let MailOp::Move { target } = change.new.op {
        folders.push(target);
    }
    for folder in folders {
        let mut row = Row::new(kind, &archive).folder(folder);
        if let Some(message) = change.new.message_id {
            row = row.message(message);
        }
        changes::log(tx, row, at).await?;
    }
    Ok(())
}

fn invalid(reason: &str) -> StoreError {
    StoreError::Invalid(reason.to_owned())
}

async fn check(tx: &mut Tx<'_>, new: &NewMailChange) -> Result<(), StoreError> {
    let account = new.account_id.to_string();
    let archive: String = sqlx::query_scalar("SELECT archive_id FROM accounts WHERE id = ?")
        .bind(&account)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .ok_or(StoreError::NotFound("account"))?;
    if folder_account(tx, new.folder_id).await? != account {
        return Err(invalid("the folder is in another account"));
    }
    match (&new.op, new.uid) {
        (MailOp::Append { .. }, Some(_)) => return Err(invalid("an append has no UID")),
        (MailOp::Append { .. }, None) => {}
        (_, None) => return Err(invalid("the change needs a UID")),
        (_, Some(uid)) => {
            signed(uid)?;
        }
    }
    match &new.op {
        MailOp::Flags { add, remove } | MailOp::Labels { add, remove }
            if add.is_empty() && remove.is_empty() =>
        {
            return Err(invalid("the change adds and removes nothing"));
        }
        MailOp::Move { target } => {
            if *target == new.folder_id {
                return Err(invalid("a move to its own folder"));
            }
            if folder_account(tx, *target).await? != account {
                return Err(invalid("the target is in another account"));
            }
        }
        _ => {}
    }
    if let Some(message) = new.message_id {
        let theirs: String = sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
            .bind(message.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("message"))?;
        if theirs != archive {
            return Err(invalid("the message is in another archive"));
        }
    }
    Ok(())
}

async fn insert(tx: &mut Tx<'_>, new: &NewMailChange) -> Result<MailChange, StoreError> {
    let id = MailChangeId::new().to_string();
    let blob = match &new.op {
        MailOp::Append { blob_id, .. } => Some(blob_id.to_string()),
        _ => None,
    };
    check(tx, new).await?;
    sqlx::query(
        "INSERT INTO mail_changes (id, account_id, folder_id, message_id, uid, kind, payload,
         blob_id, origin, user_id, device, outbox_id, created_at, state, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'held', ?)",
    )
    .bind(&id)
    .bind(new.account_id.to_string())
    .bind(new.folder_id.to_string())
    .bind(new.message_id.map(|m| m.to_string()))
    .bind(new.uid.map(signed).transpose()?)
    .bind(new.op.kind())
    .bind(json(&new.op)?)
    .bind(blob)
    .bind(new.origin.as_str())
    .bind(new.user_id.map(|u| u.to_string()))
    .bind(new.device_id.map(|d| d.to_string()))
    .bind(new.outbox_id.map(|o| o.to_string()))
    .bind(new.at.as_millis())
    .bind(new.at.as_millis())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    let change = fetch(tx, &id)
        .await?
        .ok_or(StoreError::NotFound("mail change"))?;
    log(tx, &change, new.at).await?;
    Ok(change)
}

fn has(names: &[String], name: &str) -> bool {
    names.iter().any(|n| n.eq_ignore_ascii_case(name))
}

fn satisfied(op: &MailOp, flags: &[String], labels: Option<&[String]>) -> Option<bool> {
    match op {
        MailOp::Flags { add, remove } => {
            Some(add.iter().all(|f| has(flags, f)) && !remove.iter().any(|f| has(flags, f)))
        }
        MailOp::Labels { add, remove } => labels.map(|labels| {
            add.iter().all(|l| labels.contains(l)) && !remove.iter().any(|l| labels.contains(l))
        }),
        _ => None,
    }
}

impl SqliteStore {
    async fn transition(
        &self,
        id: MailChangeId,
        sql: &'static str,
        bind: impl for<'q> FnOnce(Query<'q>) -> Query<'q>,
    ) -> Result<Option<MailChange>, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(before) = fetch(&mut tx, &id).await? else {
            return Err(StoreError::NotFound("mail change"));
        };
        let done = bind(sqlx::query(sql).bind(id.clone()))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Ok(None);
        }
        let changed = fetch(&mut tx, &id).await?;
        if let Some(changed) = &changed
            && changed.state.is_open() != before.state.is_open()
        {
            log(&mut tx, changed, changed.updated_at).await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }
}

#[async_trait]
impl MailChangeStore for SqliteStore {
    async fn enqueue_mail_change(&self, new: &NewMailChange) -> Result<MailChange, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let change = insert(&mut tx, new).await?;
        tx.commit().await.map_err(backend)?;
        Ok(change)
    }

    async fn replace_held_mail_changes(
        &self,
        cancel: &[MailChangeId],
        error: &str,
        enqueue: &[NewMailChange],
        at: Timestamp,
    ) -> Result<Option<Vec<MailChange>>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        for id in cancel {
            let id = id.to_string();
            if fetch(&mut tx, &id).await?.is_none() {
                return Err(StoreError::NotFound("mail change"));
            }
            let done = sqlx::query(
                "UPDATE mail_changes SET state = 'failed', next_attempt_at = NULL, last_error = ?2,
                 dismissed_at = ?3, updated_at = ?3
                 WHERE id = ?1 AND state = 'held'",
            )
            .bind(&id)
            .bind(error)
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            if done.rows_affected() == 0 {
                return Ok(None);
            }
            let failed = fetch(&mut tx, &id)
                .await?
                .ok_or(StoreError::NotFound("mail change"))?;
            log(&mut tx, &failed, at).await?;
        }
        let mut out = Vec::with_capacity(enqueue.len());
        for new in enqueue {
            out.push(insert(&mut tx, new).await?);
        }
        tx.commit().await.map_err(backend)?;
        Ok(Some(out))
    }

    async fn get_mail_change(&self, id: MailChangeId) -> Result<Option<MailChange>, StoreError> {
        sqlx::query("SELECT * FROM mail_changes WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| change(&row))
            .transpose()
    }

    async fn due_mail_accounts(&self, now: Timestamp) -> Result<Vec<AccountId>, StoreError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT c.account_id FROM mail_changes c
             WHERE c.state = 'held' AND (c.next_attempt_at IS NULL OR c.next_attempt_at <= ?1)
             AND (c.message_id IS NULL OR NOT EXISTS (
                 SELECT 1 FROM mail_changes o
                 WHERE o.message_id = c.message_id AND o.account_id = c.account_id
                 AND (o.state = 'sending' OR (o.state = 'held' AND o.id < c.id))))
             ORDER BY c.account_id",
        )
        .bind(now.as_millis())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        ids.iter().map(|id| parse(id)).collect()
    }

    async fn claim_mail_change(
        &self,
        account: AccountId,
        now: Timestamp,
    ) -> Result<Option<MailChange>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let claimed: Option<String> = sqlx::query_scalar(
            "UPDATE mail_changes SET state = 'sending', attempts = attempts + 1,
             next_attempt_at = NULL, updated_at = ?2
             WHERE id = (SELECT c.id FROM mail_changes c
                         WHERE c.account_id = ?1 AND c.state = 'held'
                         AND (c.next_attempt_at IS NULL OR c.next_attempt_at <= ?2)
                         AND (c.message_id IS NULL OR NOT EXISTS (
                             SELECT 1 FROM mail_changes o
                             WHERE o.message_id = c.message_id AND o.account_id = c.account_id
                             AND (o.state = 'sending' OR (o.state = 'held' AND o.id < c.id))))
                         ORDER BY c.id LIMIT 1)
             RETURNING id",
        )
        .bind(account.to_string())
        .bind(now.as_millis())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(id) = claimed else {
            return Ok(None);
        };
        let change = fetch(&mut tx, &id).await?;
        tx.commit().await.map_err(backend)?;
        Ok(change)
    }

    async fn mark_mail_change_sent(
        &self,
        id: MailChangeId,
        result_uid: Option<u64>,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError> {
        let uid = result_uid.map(signed).transpose()?;
        self.transition(
            id,
            "UPDATE mail_changes SET
             state = CASE WHEN kind IN ('flags', 'labels') THEN 'sending' ELSE 'applied' END,
             sent_at = CASE WHEN kind IN ('flags', 'labels') THEN ?2 ELSE NULL END,
             result_uid = ?3, last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND state = 'sending' AND sent_at IS NULL",
            |q| q.bind(at.as_millis()).bind(uid),
        )
        .await
    }

    async fn applied_move_from(
        &self,
        message: MessageId,
        folder: FolderId,
        uid: u64,
        before: MailChangeId,
    ) -> Result<Option<MailChange>, StoreError> {
        sqlx::query(
            "SELECT * FROM mail_changes WHERE message_id = ? AND folder_id = ? AND uid = ?
             AND kind = 'move' AND state = 'applied' AND id < ? ORDER BY id DESC LIMIT 1",
        )
        .bind(message.to_string())
        .bind(folder.to_string())
        .bind(signed(uid)?)
        .bind(before.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .map(|row| change(&row))
        .transpose()
    }

    async fn retarget_mail_change(
        &self,
        id: MailChangeId,
        folder: FolderId,
        uid: u64,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let found = fetch(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("mail change"))?;
        if folder_account(&mut tx, folder).await? != found.new.account_id.to_string() {
            return Err(invalid("the folder is in another account"));
        }
        match found.new.op {
            MailOp::Append { .. } => return Err(invalid("an append has no place to follow")),
            MailOp::Move { target } if target == folder => {
                return Err(invalid("a move to its own folder"));
            }
            _ => {}
        }
        let done = sqlx::query(
            "UPDATE mail_changes SET folder_id = ?2, uid = ?3, updated_at = ?4
             WHERE id = ?1 AND state = 'sending' AND sent_at IS NULL",
        )
        .bind(&id)
        .bind(folder.to_string())
        .bind(signed(uid)?)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Ok(None);
        }
        let changed = fetch(&mut tx, &id).await?;
        if let Some(changed) = &changed {
            log(&mut tx, &found, at).await?;
            log(&mut tx, changed, at).await?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }

    async fn retry_mail_change(
        &self,
        id: MailChangeId,
        next_attempt_at: Timestamp,
        error: &str,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError> {
        let error = error.to_owned();
        self.transition(
            id,
            "UPDATE mail_changes SET state = 'held', next_attempt_at = ?2, last_error = ?3,
             updated_at = ?4
             WHERE id = ?1 AND state = 'sending' AND sent_at IS NULL",
            |q| {
                q.bind(next_attempt_at.as_millis())
                    .bind(error)
                    .bind(at.as_millis())
            },
        )
        .await
    }

    async fn fail_mail_change(
        &self,
        id: MailChangeId,
        error: &str,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError> {
        let error = error.to_owned();
        self.transition(
            id,
            "UPDATE mail_changes SET state = 'failed', next_attempt_at = NULL, last_error = ?2,
             updated_at = ?3
             WHERE id = ?1 AND state IN ('held', 'sending')",
            |q| q.bind(error).bind(at.as_millis()),
        )
        .await
    }

    async fn confirm_mail_flags(
        &self,
        folder: FolderId,
        uid: u64,
        flags: &[String],
        labels: Option<&[String]>,
        read_at: Timestamp,
    ) -> Result<Vec<MailChange>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let open = sqlx::query(
            "SELECT * FROM mail_changes WHERE folder_id = ? AND uid = ?
             AND state IN ('held', 'sending') AND kind IN ('flags', 'labels') ORDER BY id",
        )
        .bind(folder.to_string())
        .bind(signed(uid)?)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?
        .iter()
        .map(change)
        .collect::<Result<Vec<_>, _>>()?;
        let mut closed = Vec::new();
        for found in open {
            if found.state == MailChangeState::Sending && found.sent_at.is_none() {
                break;
            }
            let next = match satisfied(&found.new.op, flags, labels) {
                Some(true) => MailChangeState::Applied,
                Some(false) if found.sent_at.is_some_and(|sent| sent < read_at) => {
                    MailChangeState::SupersededByServer
                }
                _ => break,
            };
            let id = found.id.to_string();
            sqlx::query(
                "UPDATE mail_changes SET state = ?, next_attempt_at = NULL, updated_at = ?
                 WHERE id = ?",
            )
            .bind(next.as_str())
            .bind(read_at.as_millis())
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            if let Some(change) = fetch(&mut tx, &id).await? {
                log(&mut tx, &change, read_at).await?;
                closed.push(change);
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(closed)
    }

    async fn settle_mail_changes(
        &self,
        account: AccountId,
        sent_before: Timestamp,
        at: Timestamp,
    ) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let ids: Vec<String> = sqlx::query_scalar(
            "UPDATE mail_changes SET state = 'applied', updated_at = ?3
             WHERE account_id = ?1 AND state = 'sending' AND sent_at < ?2 RETURNING id",
        )
        .bind(account.to_string())
        .bind(sent_before.as_millis())
        .bind(at.as_millis())
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        for id in &ids {
            if let Some(change) = fetch(&mut tx, id).await? {
                log(&mut tx, &change, at).await?;
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(ids.len() as u64)
    }

    async fn release_mail_changes(&self, at: Timestamp) -> Result<Vec<MailChange>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let ids: Vec<String> = sqlx::query_scalar(
            "UPDATE mail_changes SET state = 'held', updated_at = ?
             WHERE state = 'sending' AND sent_at IS NULL RETURNING id",
        )
        .bind(at.as_millis())
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let mut released = Vec::with_capacity(ids.len());
        for id in &ids {
            released.extend(fetch(&mut tx, id).await?);
        }
        tx.commit().await.map_err(backend)?;
        released.sort_by_key(|c| c.id);
        Ok(released)
    }

    async fn pending_mail_changes(
        &self,
        message: MessageId,
    ) -> Result<Vec<MailChange>, StoreError> {
        sqlx::query(
            "SELECT * FROM mail_changes WHERE message_id = ? AND state IN ('held', 'sending')
             ORDER BY id",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(change)
        .collect()
    }

    async fn failed_mail_changes(&self, message: MessageId) -> Result<Vec<MailChange>, StoreError> {
        sqlx::query(
            "SELECT * FROM mail_changes WHERE message_id = ? AND state = 'failed'
             AND dismissed_at IS NULL ORDER BY id",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(change)
        .collect()
    }

    async fn dismiss_mail_change(
        &self,
        id: MailChangeId,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError> {
        self.transition(
            id,
            "UPDATE mail_changes SET dismissed_at = ?2, updated_at = ?2
             WHERE id = ?1 AND state = 'failed' AND dismissed_at IS NULL",
            |q| q.bind(at.as_millis()),
        )
        .await
    }

    async fn outbox_mail_changes(&self, outbox: OutboxId) -> Result<Vec<MailChange>, StoreError> {
        sqlx::query("SELECT * FROM mail_changes WHERE outbox_id = ? ORDER BY id")
            .bind(outbox.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(change)
            .collect()
    }

    async fn retention_candidates(
        &self,
        account: AccountId,
        before: Timestamp,
        skip_trash: bool,
        limit: u32,
    ) -> Result<Vec<RetentionCandidate>, StoreError> {
        let rows = sqlx::query(
            "SELECT l.message_id, l.folder_id, l.uid FROM locations l
             JOIN folders f ON f.id = l.folder_id
             JOIN messages m ON m.id = l.message_id
             WHERE f.account_id = ?1 AND l.uid IS NOT NULL AND l.removed_at IS NULL
             AND COALESCE(m.received_at, m.date) < ?2
             AND (?3 = 0 OR f.role IS NULL OR f.role <> 'trash')
             AND NOT EXISTS (SELECT 1 FROM mail_changes c WHERE c.folder_id = l.folder_id
                             AND c.uid = l.uid AND c.state IN ('held', 'sending'))
             AND NOT EXISTS (SELECT 1 FROM mail_changes c WHERE c.message_id = l.message_id
                             AND c.folder_id = l.folder_id AND c.uid = l.uid
                             AND c.origin = 'retention')
             ORDER BY COALESCE(m.received_at, m.date), l.message_id, l.folder_id LIMIT ?4",
        )
        .bind(account.to_string())
        .bind(before.as_millis())
        .bind(skip_trash)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter()
            .map(|row| {
                Ok(RetentionCandidate {
                    message_id: parse(&get::<String>(row, "message_id")?)?,
                    folder_id: parse(&get::<String>(row, "folder_id")?)?,
                    uid: unsigned(get::<i64>(row, "uid")?),
                })
            })
            .collect()
    }

    async fn list_mail_changes(
        &self,
        query: &MailChangeQuery,
    ) -> Result<Vec<MailChange>, StoreError> {
        let states: Vec<&str> = query.states.iter().map(|s| s.as_str()).collect();
        sqlx::query(
            "SELECT * FROM mail_changes WHERE account_id = ?1
             AND (json_array_length(?2) = 0 OR state IN (SELECT value FROM json_each(?2)))
             ORDER BY id DESC LIMIT ?3",
        )
        .bind(query.account.to_string())
        .bind(json(&states)?)
        .bind(i64::from(query.limit.min(MAX_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(change)
        .collect()
    }
}
