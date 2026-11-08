use async_trait::async_trait;
use holtmail_core::{AccountId, ArchiveId, Timestamp, UserId};
use holtmail_store::*;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse, signed, unsigned};

const MAX_AUDIT_PAGE: u32 = 500;

fn audit_entry(row: &SqliteRow) -> Result<AuditEntry, StoreError> {
    let kind: String = get(row, "kind")?;
    let id = |column: &str| -> Result<Option<String>, StoreError> { get(row, column) };
    Ok(AuditEntry {
        seq: unsigned(get(row, "seq")?),
        entry: NewAuditEntry {
            at: Timestamp::from_millis(get(row, "at")?),
            user: id("user_id")?.map(|v| parse(&v)).transpose()?,
            kind: AuditKind::parse(&kind)
                .ok_or_else(|| StoreError::Invalid(format!("unknown audit kind {kind:?}")))?,
            archive: id("archive_id")?.map(|v| parse(&v)).transpose()?,
            account: id("account_id")?.map(|v| parse(&v)).transpose()?,
            targets: serde_json::from_str(&get::<String>(row, "targets")?).map_err(backend)?,
            count: unsigned(get(row, "count")?),
        },
    })
}

#[async_trait]
impl WriteStore for SqliteStore {
    async fn account_capabilities(&self, account: AccountId) -> Result<Capabilities, StoreError> {
        let names: Vec<String> =
            sqlx::query_scalar("SELECT capability FROM account_capabilities WHERE account_id = ?")
                .bind(account.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
        Ok(names
            .iter()
            .filter_map(|name| Capability::parse(name))
            .collect())
    }

    async fn set_account_capabilities(
        &self,
        account: AccountId,
        capabilities: &Capabilities,
    ) -> Result<(), StoreError> {
        let account = account.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM accounts WHERE id = ?")
            .bind(&account)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Err(StoreError::NotFound("account"));
        }
        sqlx::query("DELETE FROM account_capabilities WHERE account_id = ?")
            .bind(&account)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        for capability in capabilities.iter() {
            sqlx::query("INSERT INTO account_capabilities (account_id, capability) VALUES (?, ?)")
                .bind(&account)
                .bind(capability.as_str())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn grant_roles(&self, user: UserId) -> Result<Vec<Grant>, StoreError> {
        sqlx::query(
            "SELECT archive_id, role FROM archive_grants WHERE user_id = ? ORDER BY archive_id",
        )
        .bind(user.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            let role: String = get(row, "role")?;
            Ok(Grant {
                archive: parse(&get::<String>(row, "archive_id")?)?,
                role: GrantRole::parse(&role)
                    .ok_or_else(|| StoreError::Invalid(format!("unknown grant role {role:?}")))?,
            })
        })
        .collect()
    }

    async fn set_grant_role(
        &self,
        user: UserId,
        archive: ArchiveId,
        role: Option<GrantRole>,
    ) -> Result<(), StoreError> {
        let (user, archive) = (user.to_string(), archive.to_string());
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let user_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
            .bind(&user)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if user_exists.is_none() {
            return Err(StoreError::NotFound("user"));
        }
        let archive_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM archives WHERE id = ?")
            .bind(&archive)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if archive_exists.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        match role {
            Some(role) => sqlx::query(
                "INSERT INTO archive_grants (user_id, archive_id, role) VALUES (?, ?, ?)
                 ON CONFLICT (user_id, archive_id) DO UPDATE SET role = excluded.role",
            )
            .bind(&user)
            .bind(&archive)
            .bind(role.as_str()),
            None => sqlx::query("DELETE FROM archive_grants WHERE user_id = ? AND archive_id = ?")
                .bind(&user)
                .bind(&archive),
        }
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn append_audit(&self, entry: &NewAuditEntry) -> Result<AuditEntry, StoreError> {
        let _write = self.writer.lock().await;
        let seq: i64 = sqlx::query_scalar(
            "INSERT INTO audit_log (at, user_id, kind, archive_id, account_id, targets, count)
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING seq",
        )
        .bind(entry.at.as_millis())
        .bind(entry.user.map(|id| id.to_string()))
        .bind(entry.kind.as_str())
        .bind(entry.archive.map(|id| id.to_string()))
        .bind(entry.account.map(|id| id.to_string()))
        .bind(json(&entry.targets)?)
        .bind(signed(entry.count)?)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(AuditEntry {
            seq: unsigned(seq),
            entry: entry.clone(),
        })
    }

    async fn list_audit(&self, query: &AuditQuery) -> Result<Vec<AuditEntry>, StoreError> {
        sqlx::query(
            "SELECT * FROM audit_log
             WHERE (?1 IS NULL OR archive_id = ?1) AND (?2 IS NULL OR seq < ?2)
             ORDER BY seq DESC LIMIT ?3",
        )
        .bind(query.archive.map(|id| id.to_string()))
        .bind(query.before.map(signed).transpose()?)
        .bind(i64::from(query.limit.min(MAX_AUDIT_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(audit_entry)
        .collect()
    }

    async fn claim_idempotency_key(
        &self,
        key: &IdempotencyKey,
        fingerprint: &str,
        at: Timestamp,
        since: Timestamp,
        abandoned_before: Timestamp,
    ) -> Result<IdempotencyClaim, StoreError> {
        let user = key.user.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let existing = sqlx::query(
            "SELECT fingerprint, created_at, status, content_type, body FROM idempotency_keys
             WHERE user_id = ? AND key = ? AND route = ?",
        )
        .bind(&user)
        .bind(&key.key)
        .bind(&key.route)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let live =
            |row: &SqliteRow| -> Result<bool, StoreError> {
                let created = row.try_get::<i64, _>("created_at").map_err(backend)?;
                let finished = get::<Option<i64>>(row, "status")?.is_some();
                Ok(created > since.as_millis()
                    && (finished || created >= abandoned_before.as_millis()))
            };
        if let Some(row) = existing
            && live(&row)?
        {
            if get::<String>(&row, "fingerprint")? != fingerprint {
                return Ok(IdempotencyClaim::Mismatch);
            }
            return Ok(match get::<Option<i64>>(&row, "status")? {
                None => IdempotencyClaim::InFlight,
                Some(status) => IdempotencyClaim::Done(StoredResponse {
                    status: u16::try_from(status).unwrap_or(500),
                    content_type: get(&row, "content_type")?,
                    body: get::<Option<Vec<u8>>>(&row, "body")?.unwrap_or_default(),
                }),
            });
        }
        sqlx::query(
            "INSERT INTO idempotency_keys (user_id, key, route, fingerprint, created_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (user_id, key, route) DO UPDATE SET fingerprint = excluded.fingerprint,
             created_at = excluded.created_at, status = NULL, content_type = NULL, body = NULL",
        )
        .bind(&user)
        .bind(&key.key)
        .bind(&key.route)
        .bind(fingerprint)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(IdempotencyClaim::Claimed)
    }

    async fn complete_idempotency_key(
        &self,
        key: &IdempotencyKey,
        response: &StoredResponse,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE idempotency_keys SET status = ?, content_type = ?, body = ?
             WHERE user_id = ? AND key = ? AND route = ?",
        )
        .bind(i64::from(response.status))
        .bind(&response.content_type)
        .bind(&response.body)
        .bind(key.user.to_string())
        .bind(&key.key)
        .bind(&key.route)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("idempotency key"));
        }
        Ok(())
    }

    async fn release_idempotency_key(&self, key: &IdempotencyKey) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM idempotency_keys WHERE user_id = ? AND key = ? AND route = ?")
            .bind(key.user.to_string())
            .bind(&key.key)
            .bind(&key.route)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn prune_idempotency_keys(&self, before: Timestamp) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("DELETE FROM idempotency_keys WHERE created_at < ?")
            .bind(before.as_millis())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected())
    }
}
