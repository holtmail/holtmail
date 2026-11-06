use std::collections::BTreeMap;

use async_trait::async_trait;
use holtmail_core::{AccountId, Address, AliasServiceId, ArchiveId, MessageId, Timestamp};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse, signed, unique, unsigned};

fn service(row: &SqliteRow) -> Result<AliasService, StoreError> {
    Ok(AliasService {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        provider: get(row, "provider")?,
        name: get(row, "name")?,
        base_url: get(row, "base_url")?,
        sealed_key: get(row, "sealed_key")?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        synced_at: get::<Option<i64>>(row, "synced_at")?.map(Timestamp::from_millis),
        sync_error: get(row, "sync_error")?,
    })
}

fn known(row: &SqliteRow) -> Result<KnownAlias, StoreError> {
    let count = |column: &str| -> Result<Option<u64>, StoreError> {
        Ok(get::<Option<i64>>(row, column)?.map(unsigned))
    };
    Ok(KnownAlias {
        address: get(row, "address")?,
        provider: get(row, "provider")?,
        service_id: get::<Option<String>>(row, "service_id")?
            .map(|id| parse(&id))
            .transpose()?,
        remote_id: get(row, "remote_id")?,
        enabled: get::<Option<i64>>(row, "enabled")?.map(|e| e != 0),
        note: get(row, "note")?,
        name: get(row, "name")?,
        for_domain: get(row, "for_domain")?,
        mailboxes: serde_json::from_str(&get::<String>(row, "mailboxes")?).map_err(backend)?,
        created_at: get::<Option<i64>>(row, "created_at")?.map(Timestamp::from_millis),
        forwarded: count("forwarded")?,
        blocked: count("blocked")?,
        replied: count("replied")?,
    })
}

fn lower(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

fn count(value: Option<u64>) -> Result<Option<i64>, StoreError> {
    value.map(signed).transpose()
}

async fn put(
    tx: &mut sqlx::SqliteConnection,
    archive: &str,
    alias: &KnownAlias,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT OR REPLACE INTO aliases (archive_id, address, provider, service_id, remote_id,
             enabled, note, name, for_domain, mailboxes, created_at, forwarded, blocked, replied)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(archive)
    .bind(lower(&alias.address))
    .bind(&alias.provider)
    .bind(alias.service_id.map(|id| id.to_string()))
    .bind(&alias.remote_id)
    .bind(alias.enabled)
    .bind(&alias.note)
    .bind(&alias.name)
    .bind(&alias.for_domain)
    .bind(json(&alias.mailboxes)?)
    .bind(alias.created_at.map(|t| t.as_millis()))
    .bind(count(alias.forwarded)?)
    .bind(count(alias.blocked)?)
    .bind(count(alias.replied)?)
    .execute(&mut *tx)
    .await
    .map_err(backend)?;
    Ok(())
}

#[async_trait]
impl AliasStore for SqliteStore {
    async fn set_message_alias(
        &self,
        message: MessageId,
        alias: Option<&MessageAlias>,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
                .bind(message.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        let archive = archive.ok_or(StoreError::NotFound("message"))?;
        sqlx::query("DELETE FROM message_aliases WHERE message_id = ?")
            .bind(message.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if let Some(alias) = alias {
            sqlx::query(
                "INSERT INTO message_aliases (message_id, archive_id, provider, alias,
                     sender_email, sender_name, reverse_alias, signed)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(message.to_string())
            .bind(&archive)
            .bind(&alias.provider)
            .bind(lower(&alias.alias))
            .bind(alias.sender.as_ref().map(|s| lower(&s.email)))
            .bind(
                alias
                    .sender
                    .as_ref()
                    .and_then(|s| s.name.as_deref())
                    .map(str::trim)
                    .filter(|n| !n.is_empty()),
            )
            .bind(alias.reverse_alias.as_deref().map(lower))
            .bind(alias.signed)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn message_alias(&self, message: MessageId) -> Result<Option<MessageAlias>, StoreError> {
        sqlx::query("SELECT * FROM message_aliases WHERE message_id = ?")
            .bind(message.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| {
                Ok(MessageAlias {
                    provider: get(&row, "provider")?,
                    alias: get(&row, "alias")?,
                    sender: get::<Option<String>>(&row, "sender_email")?.map(|email| Address {
                        name: get(&row, "sender_name").ok().flatten(),
                        email,
                    }),
                    reverse_alias: get(&row, "reverse_alias")?,
                    signed: get(&row, "signed")?,
                })
            })
            .transpose()
    }

    async fn known_aliases_among(
        &self,
        archive: ArchiveId,
        addresses: &[String],
    ) -> Result<Vec<KnownAlias>, StoreError> {
        let wanted: Vec<String> = addresses.iter().map(|a| lower(a)).collect();
        sqlx::query(
            "SELECT * FROM aliases WHERE archive_id = ?
             AND address IN (SELECT value FROM json_each(?)) ORDER BY address",
        )
        .bind(archive.to_string())
        .bind(json(&wanted)?)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(known)
        .collect()
    }

    async fn aliases(&self, archive: ArchiveId) -> Result<Vec<AliasSummary>, StoreError> {
        let archive = archive.to_string();
        let mut out: BTreeMap<String, AliasSummary> = BTreeMap::new();
        for row in sqlx::query("SELECT * FROM aliases WHERE archive_id = ?")
            .bind(&archive)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
        {
            let alias = known(&row)?;
            out.insert(
                alias.address.clone(),
                AliasSummary {
                    address: alias.address.clone(),
                    provider: alias.provider.clone(),
                    known: Some(alias),
                    mail: AliasMail::default(),
                },
            );
        }
        let rows = sqlx::query(
            "WITH hits AS (
                 SELECT ma.message_id, ma.alias AS address, ma.provider, ma.signed,
                     COALESCE(ma.sender_email, (SELECT email FROM message_addresses f
                         WHERE f.message_id = ma.message_id AND f.field = 'from' LIMIT 1))
                         AS sender
                 FROM message_aliases ma WHERE ma.archive_id = ?1
                 UNION ALL
                 SELECT a.message_id, a.email, NULL, 0,
                     (SELECT email FROM message_addresses f
                         WHERE f.message_id = a.message_id AND f.field = 'from' LIMIT 1)
                 FROM message_addresses a
                 JOIN aliases k ON k.archive_id = a.archive_id AND k.address = a.email
                 WHERE a.archive_id = ?1 AND a.field IN ('to', 'cc', 'bcc')
                 AND NOT EXISTS (SELECT 1 FROM message_aliases x WHERE x.message_id = a.message_id)
             )
             SELECT h.address, MAX(h.provider) AS provider,
                 COUNT(DISTINCT h.message_id) AS messages,
                 COUNT(DISTINCT h.sender) AS senders,
                 COUNT(DISTINCT CASE WHEN h.signed THEN h.message_id END) AS signed,
                 MIN(m.date) AS first_at, MAX(m.date) AS last_at
             FROM hits h JOIN messages m ON m.id = h.message_id
             GROUP BY h.address",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        for row in rows {
            let address: String = get(&row, "address")?;
            let mail = AliasMail {
                messages: unsigned(get(&row, "messages")?),
                senders: unsigned(get(&row, "senders")?),
                signed: unsigned(get(&row, "signed")?),
                first_at: get::<Option<i64>>(&row, "first_at")?.map(Timestamp::from_millis),
                last_at: get::<Option<i64>>(&row, "last_at")?.map(Timestamp::from_millis),
                accounts: Vec::new(),
            };
            let provider: Option<String> = get(&row, "provider")?;
            out.entry(address.clone())
                .and_modify(|s| s.mail = mail.clone())
                .or_insert(AliasSummary {
                    address,
                    provider: provider.unwrap_or_default(),
                    known: None,
                    mail,
                });
        }
        let rows = sqlx::query(
            "WITH hits AS (
                 SELECT message_id, alias AS address FROM message_aliases WHERE archive_id = ?1
                 UNION
                 SELECT a.message_id, a.email FROM message_addresses a
                 JOIN aliases k ON k.archive_id = a.archive_id AND k.address = a.email
                 WHERE a.archive_id = ?1 AND a.field IN ('to', 'cc', 'bcc')
             )
             SELECT h.address, f.account_id, COUNT(DISTINCT h.message_id) AS messages
             FROM hits h
             JOIN locations l ON l.message_id = h.message_id
             JOIN folders f ON f.id = l.folder_id
             GROUP BY h.address, f.account_id
             ORDER BY messages DESC, f.account_id",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        for row in rows {
            let address: String = get(&row, "address")?;
            let account: AccountId = parse(&get::<String>(&row, "account_id")?)?;
            if let Some(summary) = out.get_mut(&address) {
                summary
                    .mail
                    .accounts
                    .push((account, unsigned(get(&row, "messages")?)));
            }
        }
        Ok(out.into_values().collect())
    }

    async fn put_alias(&self, archive: ArchiveId, alias: &KnownAlias) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut conn = self.pool.acquire().await.map_err(backend)?;
        put(&mut conn, &archive.to_string(), alias).await
    }

    async fn remove_alias(&self, archive: ArchiveId, address: &str) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("DELETE FROM aliases WHERE archive_id = ? AND address = ?")
            .bind(archive.to_string())
            .bind(lower(address))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn replace_service_aliases(
        &self,
        service: AliasServiceId,
        aliases: &[KnownAlias],
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let row = sqlx::query("SELECT archive_id, provider FROM alias_services WHERE id = ?")
            .bind(service.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .ok_or(StoreError::NotFound("alias service"))?;
        let archive: String = get(&row, "archive_id")?;
        let provider: String = get(&row, "provider")?;
        sqlx::query("DELETE FROM aliases WHERE service_id = ?")
            .bind(service.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        for alias in aliases {
            let alias = KnownAlias {
                provider: provider.clone(),
                service_id: Some(service),
                ..alias.clone()
            };
            put(&mut tx, &archive, &alias).await?;
        }
        sqlx::query("UPDATE alias_services SET synced_at = ?, sync_error = NULL WHERE id = ?")
            .bind(at.as_millis())
            .bind(service.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn create_alias_service(&self, s: &AliasService) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM archives WHERE id = ?")
            .bind(s.archive_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        sqlx::query(
            "INSERT INTO alias_services (id, archive_id, provider, name, base_url, sealed_key,
                 created_at, synced_at, sync_error) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(s.id.to_string())
        .bind(s.archive_id.to_string())
        .bind(&s.provider)
        .bind(&s.name)
        .bind(&s.base_url)
        .bind(&s.sealed_key)
        .bind(s.created_at.as_millis())
        .bind(s.synced_at.map(|t| t.as_millis()))
        .bind(&s.sync_error)
        .execute(&self.pool)
        .await
        .map_err(|e| unique(e, "the alias service id is taken"))?;
        Ok(())
    }

    async fn alias_services(&self, archive: ArchiveId) -> Result<Vec<AliasService>, StoreError> {
        sqlx::query("SELECT * FROM alias_services WHERE archive_id = ? ORDER BY created_at, id")
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(service)
            .collect()
    }

    async fn all_alias_services(&self) -> Result<Vec<AliasService>, StoreError> {
        sqlx::query("SELECT * FROM alias_services ORDER BY created_at, id")
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(service)
            .collect()
    }

    async fn get_alias_service(
        &self,
        id: AliasServiceId,
    ) -> Result<Option<AliasService>, StoreError> {
        sqlx::query("SELECT * FROM alias_services WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(service)
            .transpose()
    }

    async fn update_alias_service(&self, s: &AliasService) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE alias_services SET name = ?, base_url = ?, sealed_key = ?, synced_at = ?,
                 sync_error = ? WHERE id = ?",
        )
        .bind(&s.name)
        .bind(&s.base_url)
        .bind(&s.sealed_key)
        .bind(s.synced_at.map(|t| t.as_millis()))
        .bind(&s.sync_error)
        .bind(s.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("alias service"));
        }
        Ok(())
    }

    async fn delete_alias_service(
        &self,
        id: AliasServiceId,
        keep_aliases: bool,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let statement = if keep_aliases {
            "UPDATE aliases SET service_id = NULL, remote_id = NULL WHERE service_id = ?"
        } else {
            "DELETE FROM aliases WHERE service_id = ?"
        };
        sqlx::query(statement)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        let done = sqlx::query("DELETE FROM alias_services WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("alias service"));
        }
        tx.commit().await.map_err(backend)
    }

    async fn own_alias_addresses(&self, archive: ArchiveId) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT address FROM aliases WHERE archive_id = ?1
             UNION SELECT alias FROM message_aliases WHERE archive_id = ?1
             ORDER BY 1",
        )
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)
    }
}
