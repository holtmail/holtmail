use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, ArchiveId, DraftId, IdentityId, MessageId, OutboxId, Timestamp, TransportId, UserId,
};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse, signed, unique, unsigned};

const MAX_OUTBOX_PAGE: u32 = 500;

type Tx<'a> = Transaction<'a, Sqlite>;

fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(backend)
}

fn optional_id<T: std::str::FromStr>(
    row: &SqliteRow,
    column: &str,
) -> Result<Option<T>, StoreError> {
    get::<Option<String>>(row, column)?
        .map(|v| parse(&v))
        .transpose()
}

fn optional_time(row: &SqliteRow, column: &str) -> Result<Option<Timestamp>, StoreError> {
    Ok(get::<Option<i64>>(row, column)?.map(Timestamp::from_millis))
}

fn optional_u32(row: &SqliteRow, column: &str) -> Result<Option<u32>, StoreError> {
    Ok(get::<Option<i64>>(row, column)?.map(|v| u32::try_from(v).unwrap_or(u32::MAX)))
}

fn transport(row: &SqliteRow) -> Result<Transport, StoreError> {
    let kind: String = get(row, "kind")?;
    Ok(Transport {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: optional_id(row, "archive_id")?,
        name: get(row, "name")?,
        kind: TransportKind::parse(&kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown transport kind {kind:?}")))?,
        account_id: optional_id(row, "account_id")?,
        settings: from_json(&get::<String>(row, "settings")?)?,
        sealed_secret: get(row, "sealed_secret")?,
        allowed_from: from_json(&get::<String>(row, "allowed_from")?)?,
        limits: TransportLimits {
            max_size: get::<Option<i64>>(row, "max_size")?.map(unsigned),
            max_recipients: optional_u32(row, "max_recipients")?,
            per_hour: optional_u32(row, "per_hour")?,
        },
        enabled: get(row, "enabled")?,
        smtputf8: get(row, "smtputf8")?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
    })
}

fn identity(row: &SqliteRow) -> Result<Identity, StoreError> {
    let sent_copy: String = get(row, "sent_copy")?;
    Ok(Identity {
        id: parse(&get::<String>(row, "id")?)?,
        fields: NewIdentity {
            archive_id: parse(&get::<String>(row, "archive_id")?)?,
            address: get(row, "address")?,
            name: get(row, "name")?,
            signature_html: get(row, "signature_html")?,
            signature_text: get(row, "signature_text")?,
            reply_to: get(row, "reply_to")?,
            transport_id: parse(&get::<String>(row, "transport_id")?)?,
            sent_copy: SentCopyPolicy::from_stored(&sent_copy).ok_or_else(|| {
                StoreError::Invalid(format!("unknown sent-copy policy {sent_copy:?}"))
            })?,
            bcc_self: get(row, "bcc_self")?,
            account_id: optional_id(row, "account_id")?,
            alias: get(row, "alias")?,
            plus_addresses: get(row, "plus_addresses")?,
            active: get(row, "active")?,
            created_by: optional_id(row, "created_by")?,
        },
        rewritten_from: match (
            get::<Option<String>>(row, "rewritten_from")?,
            optional_time(row, "rewritten_at")?,
        ) {
            (Some(seen), Some(at)) => Some(RewrittenFrom { seen, at }),
            _ => None,
        },
        created_at: Timestamp::from_millis(get(row, "created_at")?),
    })
}

fn draft(row: &SqliteRow) -> Result<Draft, StoreError> {
    Ok(Draft {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        user_id: parse(&get::<String>(row, "user_id")?)?,
        current: draft_version(row)?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
    })
}

fn draft_version(row: &SqliteRow) -> Result<DraftVersion, StoreError> {
    Ok(DraftVersion {
        version: u32::try_from(get::<i64>(row, "version")?).unwrap_or(u32::MAX),
        payload: from_json(&get::<String>(row, "payload")?)?,
        blobs: from_json(&get::<String>(row, "blobs")?)?,
        saved_at: Timestamp::from_millis(get(row, "saved_at")?),
    })
}

fn queued_state(name: &str) -> Result<OutboxState, StoreError> {
    OutboxState::parse(name)
        .ok_or_else(|| StoreError::Invalid(format!("unknown outbox state {name:?}")))
}

fn outbox(row: &SqliteRow) -> Result<OutboxEntry, StoreError> {
    let state: String = get(row, "state")?;
    let recipients: Vec<OutboxRecipient> = from_json(&get::<String>(row, "recipients")?)?;
    Ok(OutboxEntry {
        id: parse(&get::<String>(row, "id")?)?,
        new: NewOutboxEntry {
            archive_id: parse(&get::<String>(row, "archive_id")?)?,
            identity_id: parse(&get::<String>(row, "identity_id")?)?,
            transport_id: parse(&get::<String>(row, "transport_id")?)?,
            user_id: optional_id(row, "user_id")?,
            device_id: optional_id(row, "device")?,
            route: from_json(&get::<String>(row, "route")?)?,
            mail_from: get(row, "mail_from")?,
            recipients: recipients.iter().map(|r| r.address.clone()).collect(),
            blob_id: parse(&get::<String>(row, "blob_id")?)?,
            size: unsigned(get(row, "size")?),
            rfc_message_id: get(row, "rfc_message_id")?,
            send_at: Timestamp::from_millis(get(row, "send_at")?),
            state: queued_state(&get::<String>(row, "queued_state")?)?,
            flow_run: get(row, "flow_run")?,
            idempotency_key: get(row, "idempotency_key")?,
            resend_of: optional_id(row, "resend_of")?,
            at: Timestamp::from_millis(get(row, "created_at")?),
        },
        state: queued_state(&state)?,
        recipients,
        message_id: optional_id(row, "message_id")?,
        attempts: u32::try_from(get::<i64>(row, "attempts")?).unwrap_or(u32::MAX),
        last_attempt_at: optional_time(row, "last_attempt_at")?,
        next_attempt_at: optional_time(row, "next_attempt_at")?,
        last_error: get(row, "last_error")?,
        sent_at: optional_time(row, "sent_at")?,
        synced_copy: optional_id(row, "synced_copy")?,
        note: get(row, "note")?,
        updated_at: Timestamp::from_millis(get(row, "updated_at")?),
    })
}

async fn exists(tx: &mut Tx<'_>, sql: &'static str, id: &str) -> Result<bool, StoreError> {
    let found: Option<i64> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?;
    Ok(found.is_some())
}

async fn require(
    tx: &mut Tx<'_>,
    sql: &'static str,
    id: &str,
    what: &'static str,
) -> Result<(), StoreError> {
    if exists(tx, sql, id).await? {
        Ok(())
    } else {
        Err(StoreError::NotFound(what))
    }
}

const ARCHIVE: &str = "SELECT 1 FROM archives WHERE id = ?";
const USER: &str = "SELECT 1 FROM users WHERE id = ?";

async fn account_archive(tx: &mut Tx<'_>, account: AccountId) -> Result<ArchiveId, StoreError> {
    let archive: Option<String> =
        sqlx::query_scalar("SELECT archive_id FROM accounts WHERE id = ?")
            .bind(account.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
    parse(&archive.ok_or(StoreError::NotFound("account"))?)
}

async fn check_transport(tx: &mut Tx<'_>, transport: &Transport) -> Result<(), StoreError> {
    if let Some(archive) = transport.archive_id {
        require(tx, ARCHIVE, &archive.to_string(), "archive").await?;
    }
    match (transport.kind, transport.archive_id, transport.account_id) {
        (TransportKind::Account, None, _) => Err(StoreError::Invalid(
            "an instance transport cannot use an account".into(),
        )),
        (TransportKind::Account, Some(_), None) => Err(StoreError::Invalid(
            "an account transport needs an account".into(),
        )),
        (TransportKind::Account, Some(archive), Some(account)) => {
            if account_archive(tx, account).await? == archive {
                Ok(())
            } else {
                Err(StoreError::Invalid(
                    "the account is in another archive".into(),
                ))
            }
        }
        (TransportKind::Smtp | TransportKind::Jmap | TransportKind::Graph, _, Some(_)) => Err(
            StoreError::Invalid("only an account transport names an account".into()),
        ),
        (TransportKind::Smtp | TransportKind::Jmap | TransportKind::Graph, _, None) => Ok(()),
    }
}

async fn transport_usable(
    tx: &mut Tx<'_>,
    transport: TransportId,
    archive: ArchiveId,
) -> Result<(), StoreError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT archive_id FROM transports WHERE id = ?")
            .bind(transport.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
    let usable = match row.ok_or(StoreError::NotFound("transport"))? {
        Some(owner) => owner == archive.to_string(),
        None => {
            let granted: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM transport_grants WHERE transport_id = ? AND archive_id = ?",
            )
            .bind(transport.to_string())
            .bind(archive.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
            granted.is_some()
        }
    };
    if usable {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "the archive may not use this transport".into(),
        ))
    }
}

async fn check_identity(tx: &mut Tx<'_>, identity: &NewIdentity) -> Result<(), StoreError> {
    require(tx, ARCHIVE, &identity.archive_id.to_string(), "archive").await?;
    transport_usable(tx, identity.transport_id, identity.archive_id).await?;
    if let Some(account) = identity.account_id
        && account_archive(tx, account).await? != identity.archive_id
    {
        return Err(StoreError::Invalid(
            "the account is in another archive".into(),
        ));
    }
    if identity.address.trim().is_empty() {
        return Err(StoreError::Invalid("the identity has no address".into()));
    }
    Ok(())
}

fn bind_identity<'q>(
    query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>,
    identity: &'q NewIdentity,
) -> sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments> {
    query
        .bind(&identity.address)
        .bind(&identity.name)
        .bind(&identity.signature_html)
        .bind(&identity.signature_text)
        .bind(&identity.reply_to)
        .bind(identity.transport_id.to_string())
        .bind(identity.sent_copy.to_stored())
        .bind(identity.bcc_self)
        .bind(identity.account_id.map(|id| id.to_string()))
        .bind(&identity.alias)
        .bind(identity.plus_addresses)
        .bind(identity.active)
}

async fn fetch_outbox(tx: &mut Tx<'_>, id: &str) -> Result<Option<OutboxEntry>, StoreError> {
    sqlx::query("SELECT * FROM outbox WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .as_ref()
        .map(outbox)
        .transpose()
}

async fn fetch_draft(tx: &mut Tx<'_>, id: &str) -> Result<Option<Draft>, StoreError> {
    sqlx::query(
        "SELECT d.id, d.archive_id, d.user_id, d.created_at, v.version, v.payload, v.blobs,
                v.saved_at
         FROM drafts d JOIN draft_versions v ON v.draft_id = d.id AND v.version = d.version
         WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?
    .as_ref()
    .map(draft)
    .transpose()
}

async fn insert_version(
    tx: &mut Tx<'_>,
    id: &str,
    version: u32,
    payload: &serde_json::Value,
    blobs: &[BlobId],
    at: Timestamp,
) -> Result<(), StoreError> {
    let mut blobs = blobs.to_vec();
    blobs.sort();
    blobs.dedup();
    sqlx::query(
        "INSERT INTO draft_versions (draft_id, version, payload, blobs, saved_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(i64::from(version))
    .bind(payload.to_string())
    .bind(json(&blobs)?)
    .bind(at.as_millis())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    for blob in &blobs {
        sqlx::query("INSERT INTO draft_blobs (draft_id, version, blob_id) VALUES (?, ?, ?)")
            .bind(id)
            .bind(i64::from(version))
            .bind(blob.to_string())
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
    }
    Ok(())
}

#[async_trait]
impl SendStore for SqliteStore {
    async fn create_transport(&self, transport: &Transport) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        check_transport(&mut tx, transport).await?;
        sqlx::query(
            "INSERT INTO transports (id, archive_id, name, kind, account_id, settings,
             sealed_secret, allowed_from, max_size, max_recipients, per_hour, enabled, smtputf8,
             created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(transport.id.to_string())
        .bind(transport.archive_id.map(|id| id.to_string()))
        .bind(&transport.name)
        .bind(transport.kind.as_str())
        .bind(transport.account_id.map(|id| id.to_string()))
        .bind(transport.settings.to_string())
        .bind(&transport.sealed_secret)
        .bind(json(&transport.allowed_from)?)
        .bind(transport.limits.max_size.map(signed).transpose()?)
        .bind(transport.limits.max_recipients.map(i64::from))
        .bind(transport.limits.per_hour.map(i64::from))
        .bind(transport.enabled)
        .bind(transport.smtputf8)
        .bind(transport.created_at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(|error| unique(error, "a transport with this id exists"))?;
        tx.commit().await.map_err(backend)
    }

    async fn set_transport_smtputf8(
        &self,
        id: TransportId,
        smtputf8: bool,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("UPDATE transports SET smtputf8 = ? WHERE id = ?")
            .bind(smtputf8)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("transport"));
        }
        Ok(())
    }

    async fn get_transport(&self, id: TransportId) -> Result<Option<Transport>, StoreError> {
        sqlx::query("SELECT * FROM transports WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(transport)
            .transpose()
    }

    async fn archive_transports(&self, archive: ArchiveId) -> Result<Vec<Transport>, StoreError> {
        sqlx::query(
            "SELECT * FROM transports WHERE archive_id = ?1
             OR (archive_id IS NULL AND id IN
                 (SELECT transport_id FROM transport_grants WHERE archive_id = ?1))
             ORDER BY created_at, id",
        )
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(transport)
        .collect()
    }

    async fn instance_transports(&self) -> Result<Vec<Transport>, StoreError> {
        sqlx::query("SELECT * FROM transports WHERE archive_id IS NULL ORDER BY created_at, id")
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(transport)
            .collect()
    }

    async fn update_transport(&self, update: &Transport) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let stored = sqlx::query("SELECT * FROM transports WHERE id = ?")
            .bind(update.id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .as_ref()
            .map(transport)
            .transpose()?
            .ok_or(StoreError::NotFound("transport"))?;
        let merged = Transport {
            archive_id: stored.archive_id,
            kind: stored.kind,
            created_at: stored.created_at,
            ..update.clone()
        };
        check_transport(&mut tx, &merged).await?;
        sqlx::query(
            "UPDATE transports SET name = ?, account_id = ?, settings = ?, sealed_secret = ?,
             allowed_from = ?, max_size = ?, max_recipients = ?, per_hour = ?, enabled = ?
             WHERE id = ?",
        )
        .bind(&merged.name)
        .bind(merged.account_id.map(|id| id.to_string()))
        .bind(merged.settings.to_string())
        .bind(&merged.sealed_secret)
        .bind(json(&merged.allowed_from)?)
        .bind(merged.limits.max_size.map(signed).transpose()?)
        .bind(merged.limits.max_recipients.map(i64::from))
        .bind(merged.limits.per_hour.map(i64::from))
        .bind(merged.enabled)
        .bind(merged.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn delete_transport(&self, id: TransportId) -> Result<(), StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require(
            &mut tx,
            "SELECT 1 FROM transports WHERE id = ?",
            &id,
            "transport",
        )
        .await?;
        if exists(
            &mut tx,
            "SELECT 1 FROM identities WHERE transport_id = ? LIMIT 1",
            &id,
        )
        .await?
        {
            return Err(StoreError::Invalid(
                "an identity uses this transport".into(),
            ));
        }
        sqlx::query("DELETE FROM transports WHERE id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn transport_grants(&self, id: TransportId) -> Result<Vec<ArchiveId>, StoreError> {
        sqlx::query_scalar::<_, String>(
            "SELECT archive_id FROM transport_grants WHERE transport_id = ? ORDER BY archive_id",
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|id| parse(id))
        .collect()
    }

    async fn set_transport_grants(
        &self,
        id: TransportId,
        archives: &[ArchiveId],
    ) -> Result<(), StoreError> {
        let transport_id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let owner: Option<Option<String>> =
            sqlx::query_scalar("SELECT archive_id FROM transports WHERE id = ?")
                .bind(&transport_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        if owner.ok_or(StoreError::NotFound("transport"))?.is_some() {
            return Err(StoreError::Invalid(
                "only an instance transport is granted to archives".into(),
            ));
        }
        for archive in archives {
            require(&mut tx, ARCHIVE, &archive.to_string(), "archive").await?;
        }
        let keep = json(&archives.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        let in_use: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM identities WHERE transport_id = ?1
             AND archive_id NOT IN (SELECT value FROM json_each(?2)) LIMIT 1",
        )
        .bind(&transport_id)
        .bind(&keep)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        if in_use.is_some() {
            return Err(StoreError::Invalid(
                "an identity of a removed archive uses this transport".into(),
            ));
        }
        sqlx::query("DELETE FROM transport_grants WHERE transport_id = ?")
            .bind(&transport_id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        for archive in archives {
            sqlx::query(
                "INSERT OR IGNORE INTO transport_grants (transport_id, archive_id) VALUES (?, ?)",
            )
            .bind(&transport_id)
            .bind(archive.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn create_identity(&self, new: NewIdentity) -> Result<Identity, StoreError> {
        let identity = Identity {
            id: IdentityId::new(),
            fields: new,
            rewritten_from: None,
            created_at: Timestamp::now(),
        };
        let id = identity.id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        check_identity(&mut tx, &identity.fields).await?;
        let insert = sqlx::query(
            "INSERT INTO identities (address, name, signature_html, signature_text, reply_to,
             transport_id, sent_copy, bcc_self, account_id, alias, plus_addresses, active,
             id, archive_id, created_by, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        );
        bind_identity(insert, &identity.fields)
            .bind(&id)
            .bind(identity.fields.archive_id.to_string())
            .bind(identity.fields.created_by.map(|u| u.to_string()))
            .bind(identity.created_at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if let Some(user) = identity.fields.created_by
            && exists(&mut tx, USER, &user.to_string()).await?
        {
            sqlx::query("INSERT INTO identity_users (identity_id, user_id) VALUES (?, ?)")
                .bind(&id)
                .bind(user.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(identity)
    }

    async fn get_identity(&self, id: IdentityId) -> Result<Option<Identity>, StoreError> {
        sqlx::query("SELECT * FROM identities WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(identity)
            .transpose()
    }

    async fn list_identities(&self, archive: ArchiveId) -> Result<Vec<Identity>, StoreError> {
        sqlx::query("SELECT * FROM identities WHERE archive_id = ? ORDER BY created_at, id")
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(identity)
            .collect()
    }

    async fn update_identity(&self, update: &Identity) -> Result<(), StoreError> {
        let id = update.id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let stored = sqlx::query("SELECT * FROM identities WHERE id = ?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .as_ref()
            .map(identity)
            .transpose()?
            .ok_or(StoreError::NotFound("identity"))?;
        let fields = NewIdentity {
            archive_id: stored.fields.archive_id,
            created_by: stored.fields.created_by,
            ..update.fields.clone()
        };
        check_identity(&mut tx, &fields).await?;
        let query = sqlx::query(
            "UPDATE identities SET address = ?, name = ?, signature_html = ?, signature_text = ?,
             reply_to = ?, transport_id = ?, sent_copy = ?, bcc_self = ?, account_id = ?,
             alias = ?, plus_addresses = ?, active = ?,
             rewritten_from = CASE WHEN ? THEN rewritten_from END,
             rewritten_at = CASE WHEN ? THEN rewritten_at END
             WHERE id = ?",
        );
        let same_address = stored.fields.address.eq_ignore_ascii_case(&fields.address);
        bind_identity(query, &fields)
            .bind(same_address)
            .bind(same_address)
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn delete_identity(&self, id: IdentityId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("DELETE FROM identities WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("identity"));
        }
        Ok(())
    }

    async fn identity_users(&self, id: IdentityId) -> Result<Vec<UserId>, StoreError> {
        sqlx::query_scalar::<_, String>(
            "SELECT user_id FROM identity_users WHERE identity_id = ? ORDER BY user_id",
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|id| parse(id))
        .collect()
    }

    async fn set_identity_users(&self, id: IdentityId, users: &[UserId]) -> Result<(), StoreError> {
        let identity_id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require(
            &mut tx,
            "SELECT 1 FROM identities WHERE id = ?",
            &identity_id,
            "identity",
        )
        .await?;
        for user in users {
            require(&mut tx, USER, &user.to_string(), "user").await?;
        }
        sqlx::query("DELETE FROM identity_users WHERE identity_id = ?")
            .bind(&identity_id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        for user in users {
            sqlx::query(
                "INSERT OR IGNORE INTO identity_users (identity_id, user_id) VALUES (?, ?)",
            )
            .bind(&identity_id)
            .bind(user.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)
    }

    async fn user_identities(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Vec<Identity>, StoreError> {
        sqlx::query(
            "SELECT i.* FROM identities i JOIN identity_users u ON u.identity_id = i.id
             WHERE u.user_id = ? AND i.archive_id = ? ORDER BY i.created_at, i.id",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(identity)
        .collect()
    }

    async fn default_identity(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Option<IdentityId>, StoreError> {
        sqlx::query_scalar::<_, String>(
            "SELECT identity_id FROM identity_defaults WHERE user_id = ? AND archive_id = ?",
        )
        .bind(user.to_string())
        .bind(archive.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .map(|id| parse(&id))
        .transpose()
    }

    async fn set_default_identity(
        &self,
        user: UserId,
        archive: ArchiveId,
        identity: Option<IdentityId>,
    ) -> Result<(), StoreError> {
        let (user, archive) = (user.to_string(), archive.to_string());
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require(&mut tx, USER, &user, "user").await?;
        require(&mut tx, ARCHIVE, &archive, "archive").await?;
        match identity {
            Some(identity) => {
                let owner: Option<String> =
                    sqlx::query_scalar("SELECT archive_id FROM identities WHERE id = ?")
                        .bind(identity.to_string())
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(backend)?;
                if owner.ok_or(StoreError::NotFound("identity"))? != archive {
                    return Err(StoreError::Invalid(
                        "the identity is in another archive".into(),
                    ));
                }
                sqlx::query(
                    "INSERT INTO identity_defaults (user_id, archive_id, identity_id)
                     VALUES (?, ?, ?)
                     ON CONFLICT (user_id, archive_id) DO UPDATE SET identity_id = excluded.identity_id",
                )
                .bind(&user)
                .bind(&archive)
                .bind(identity.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            None => {
                sqlx::query("DELETE FROM identity_defaults WHERE user_id = ? AND archive_id = ?")
                    .bind(&user)
                    .bind(&archive)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)
    }

    async fn create_draft(
        &self,
        archive: ArchiveId,
        user: UserId,
        payload: &serde_json::Value,
        blobs: &[BlobId],
        at: Timestamp,
    ) -> Result<Draft, StoreError> {
        let id = DraftId::new().to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require(&mut tx, ARCHIVE, &archive.to_string(), "archive").await?;
        require(&mut tx, USER, &user.to_string(), "user").await?;
        sqlx::query(
            "INSERT INTO drafts (id, archive_id, user_id, version, created_at, saved_at)
             VALUES (?, ?, ?, 1, ?, ?)",
        )
        .bind(&id)
        .bind(archive.to_string())
        .bind(user.to_string())
        .bind(at.as_millis())
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        insert_version(&mut tx, &id, 1, payload, blobs, at).await?;
        let draft = fetch_draft(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("draft"))?;
        tx.commit().await.map_err(backend)?;
        Ok(draft)
    }

    async fn get_draft(&self, id: DraftId) -> Result<Option<Draft>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        fetch_draft(&mut tx, &id.to_string()).await
    }

    async fn save_draft(
        &self,
        id: DraftId,
        expected: u32,
        payload: &serde_json::Value,
        blobs: &[BlobId],
        at: Timestamp,
    ) -> Result<DraftSave, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let current = fetch_draft(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("draft"))?;
        if current.current.version != expected {
            return Ok(DraftSave::Conflict(current));
        }
        let version = expected + 1;
        insert_version(&mut tx, &id, version, payload, blobs, at).await?;
        sqlx::query("UPDATE drafts SET version = ?, saved_at = ? WHERE id = ?")
            .bind(i64::from(version))
            .bind(at.as_millis())
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        sqlx::query("DELETE FROM draft_versions WHERE draft_id = ? AND version <= ?")
            .bind(&id)
            .bind(i64::from(version) - i64::from(DRAFT_VERSIONS_KEPT))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        let draft = fetch_draft(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("draft"))?;
        tx.commit().await.map_err(backend)?;
        Ok(DraftSave::Saved(draft))
    }

    async fn draft_versions(&self, id: DraftId) -> Result<Vec<DraftVersion>, StoreError> {
        sqlx::query("SELECT * FROM draft_versions WHERE draft_id = ? ORDER BY version DESC")
            .bind(id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(draft_version)
            .collect()
    }

    async fn list_drafts(
        &self,
        archive: ArchiveId,
        user: UserId,
    ) -> Result<Vec<Draft>, StoreError> {
        sqlx::query(
            "SELECT d.id, d.archive_id, d.user_id, d.created_at, v.version, v.payload, v.blobs,
                    v.saved_at
             FROM drafts d JOIN draft_versions v ON v.draft_id = d.id AND v.version = d.version
             WHERE d.archive_id = ? AND d.user_id = ? ORDER BY d.saved_at DESC, d.id DESC",
        )
        .bind(archive.to_string())
        .bind(user.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(draft)
        .collect()
    }

    async fn delete_draft(&self, id: DraftId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM drafts WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn enqueue_outbox(
        &self,
        entry: &NewOutboxEntry,
    ) -> Result<(OutboxEntry, bool), StoreError> {
        if !matches!(entry.state, OutboxState::Queued | OutboxState::Draft) {
            return Err(StoreError::Invalid(
                "an outbox message starts queued or held".into(),
            ));
        }
        if entry.recipients.is_empty() {
            return Err(StoreError::Invalid("the envelope has no recipient".into()));
        }
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        require(&mut tx, ARCHIVE, &entry.archive_id.to_string(), "archive").await?;
        if let (Some(user), Some(key)) = (entry.user_id, &entry.idempotency_key) {
            let found: Option<String> = sqlx::query_scalar(
                "SELECT id FROM outbox WHERE user_id = ? AND idempotency_key = ?",
            )
            .bind(user.to_string())
            .bind(key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
            if let Some(found) = found {
                let existing = fetch_outbox(&mut tx, &found)
                    .await?
                    .ok_or(StoreError::NotFound("outbox message"))?;
                return Ok((existing, false));
            }
        }
        let id = OutboxId::new().to_string();
        let recipients: Vec<OutboxRecipient> = entry
            .recipients
            .iter()
            .map(|address| OutboxRecipient {
                address: address.clone(),
                state: RecipientState::Pending,
            })
            .collect();
        sqlx::query(
            "INSERT INTO outbox (id, archive_id, identity_id, transport_id, user_id, device, route,
             mail_from, recipients, blob_id, size, rfc_message_id, send_at, state, queued_state,
             flow_run, idempotency_key, resend_of, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(entry.archive_id.to_string())
        .bind(entry.identity_id.to_string())
        .bind(entry.transport_id.to_string())
        .bind(entry.user_id.map(|u| u.to_string()))
        .bind(entry.device_id.map(|d| d.to_string()))
        .bind(entry.route.to_string())
        .bind(&entry.mail_from)
        .bind(json(&recipients)?)
        .bind(entry.blob_id.to_string())
        .bind(signed(entry.size)?)
        .bind(&entry.rfc_message_id)
        .bind(entry.send_at.as_millis())
        .bind(entry.state.as_str())
        .bind(entry.state.as_str())
        .bind(&entry.flow_run)
        .bind(&entry.idempotency_key)
        .bind(entry.resend_of.map(|id| id.to_string()))
        .bind(entry.at.as_millis())
        .bind(entry.at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let stored = fetch_outbox(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("outbox message"))?;
        tx.commit().await.map_err(backend)?;
        Ok((stored, true))
    }

    async fn outbox_by_idempotency_key(
        &self,
        user: UserId,
        key: &str,
    ) -> Result<Option<OutboxEntry>, StoreError> {
        sqlx::query("SELECT * FROM outbox WHERE user_id = ? AND idempotency_key = ?")
            .bind(user.to_string())
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(outbox)
            .transpose()
    }

    async fn device_sends_since(
        &self,
        user: UserId,
        since: Timestamp,
    ) -> Result<DeviceSends, StoreError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS messages, COALESCE(SUM(json_array_length(recipients)), 0)
             AS recipients, MIN(created_at) AS oldest FROM outbox
             WHERE user_id = ? AND created_at >= ? AND device IS NOT NULL
             AND state <> 'cancelled'",
        )
        .bind(user.to_string())
        .bind(since.as_millis())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(DeviceSends {
            messages: unsigned(get(&row, "messages")?),
            recipients: unsigned(get(&row, "recipients")?),
            oldest: optional_time(&row, "oldest")?,
        })
    }

    async fn get_outbox(&self, id: OutboxId) -> Result<Option<OutboxEntry>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        fetch_outbox(&mut tx, &id.to_string()).await
    }

    async fn list_outbox(&self, query: &OutboxQuery) -> Result<Vec<OutboxEntry>, StoreError> {
        let states = json(&query.states.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
        sqlx::query(
            "SELECT * FROM outbox WHERE archive_id = ?1
             AND (?2 = '[]' OR state IN (SELECT value FROM json_each(?2)))
             AND (?3 IS NULL OR id < ?3)
             AND (?5 IS NULL OR user_id = ?5)
             ORDER BY id DESC LIMIT ?4",
        )
        .bind(query.archive.to_string())
        .bind(states)
        .bind(query.before.map(|id| id.to_string()))
        .bind(i64::from(query.limit.min(MAX_OUTBOX_PAGE)))
        .bind(query.user.map(|u| u.to_string()))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(outbox)
        .collect()
    }

    async fn change_outbox(
        &self,
        id: OutboxId,
        from: &[OutboxState],
        change: &OutboxChange,
    ) -> Result<Option<OutboxEntry>, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let current = fetch_outbox(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("outbox message"))?;
        if !from.contains(&current.state) {
            return Ok(None);
        }
        let recipients = match &change.recipients {
            Some(recipients) => {
                let same = recipients.len() == current.recipients.len()
                    && recipients
                        .iter()
                        .zip(&current.recipients)
                        .all(|(a, b)| a.address == b.address);
                if !same {
                    return Err(StoreError::Invalid(
                        "the recipients are not the envelope's".into(),
                    ));
                }
                Some(json(recipients)?)
            }
            None => None,
        };
        sqlx::query(
            "UPDATE outbox SET state = ?, updated_at = ?, next_attempt_at = ?, last_error = ?,
             send_at = COALESCE(?, send_at), recipients = COALESCE(?, recipients),
             sent_at = COALESCE(?, sent_at), message_id = COALESCE(?, message_id)
             WHERE id = ?",
        )
        .bind(change.state.as_str())
        .bind(change.at.as_millis())
        .bind(change.next_attempt_at.map(|t| t.as_millis()))
        .bind(&change.error)
        .bind(change.send_at.map(|t| t.as_millis()))
        .bind(recipients)
        .bind(change.sent_at.map(|t| t.as_millis()))
        .bind(change.message_id.map(|m| m.to_string()))
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let changed = fetch_outbox(&mut tx, &id).await?;
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }

    async fn due_transports(&self, now: Timestamp) -> Result<Vec<TransportId>, StoreError> {
        sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT transport_id FROM outbox
             WHERE (state = 'queued' AND send_at <= ?1)
                OR (state = 'retrying' AND next_attempt_at <= ?1)
             ORDER BY transport_id",
        )
        .bind(now.as_millis())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|id| parse(id))
        .collect()
    }

    async fn claim_outbox(
        &self,
        transport: TransportId,
        now: Timestamp,
    ) -> Result<Option<OutboxEntry>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let claimed: Option<String> = sqlx::query_scalar(
            "UPDATE outbox SET state = 'sending', attempts = attempts + 1, last_attempt_at = ?2,
             next_attempt_at = NULL, updated_at = ?2
             WHERE id = (SELECT id FROM outbox WHERE transport_id = ?1
                         AND ((state = 'queued' AND send_at <= ?2)
                              OR (state = 'retrying' AND next_attempt_at <= ?2))
                         ORDER BY CASE state WHEN 'queued' THEN send_at ELSE next_attempt_at END, id
                         LIMIT 1)
             AND NOT EXISTS (SELECT 1 FROM outbox WHERE transport_id = ?1 AND state = 'sending')
             RETURNING id",
        )
        .bind(transport.to_string())
        .bind(now.as_millis())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(id) = claimed else {
            return Ok(None);
        };
        let entry = fetch_outbox(&mut tx, &id).await?;
        tx.commit().await.map_err(backend)?;
        Ok(entry)
    }

    async fn outbox_attempts_since(
        &self,
        transport: TransportId,
        since: Timestamp,
    ) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM outbox WHERE transport_id = ? AND last_attempt_at >= ?",
        )
        .bind(transport.to_string())
        .bind(since.as_millis())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(unsigned(count))
    }

    async fn interrupt_sending(
        &self,
        at: Timestamp,
        error: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let ids: Vec<String> = sqlx::query_scalar(
            "UPDATE outbox SET state = 'uncertain', last_error = ?, updated_at = ?
             WHERE state = 'sending' RETURNING id",
        )
        .bind(error)
        .bind(at.as_millis())
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        let mut entries = Vec::with_capacity(ids.len());
        for id in &ids {
            entries.extend(fetch_outbox(&mut tx, id).await?);
        }
        tx.commit().await.map_err(backend)?;
        entries.sort_by_key(|e| e.id);
        Ok(entries)
    }

    async fn outbox_by_rfc_message_id(
        &self,
        archive: ArchiveId,
        rfc_message_id: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError> {
        sqlx::query("SELECT * FROM outbox WHERE archive_id = ? AND rfc_message_id = ? ORDER BY id")
            .bind(archive.to_string())
            .bind(rfc_message_id)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(outbox)
            .collect()
    }

    async fn outbox_for_message(&self, message: MessageId) -> Result<Vec<OutboxEntry>, StoreError> {
        sqlx::query(
            "SELECT * FROM outbox WHERE message_id = ?1
             UNION SELECT * FROM outbox WHERE synced_copy = ?1 ORDER BY id",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(outbox)
        .collect()
    }

    async fn link_outbox_copy(
        &self,
        id: OutboxId,
        message: MessageId,
        at: Timestamp,
    ) -> Result<OutboxEntry, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let current = fetch_outbox(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("outbox message"))?;
        let archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM messages WHERE id = ?")
                .bind(message.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        let archive = archive.ok_or(StoreError::NotFound("message"))?;
        if archive != current.new.archive_id.to_string() {
            return Err(StoreError::Invalid(
                "the message is in another archive".into(),
            ));
        }
        sqlx::query("UPDATE outbox SET synced_copy = ?, updated_at = ? WHERE id = ?")
            .bind(message.to_string())
            .bind(at.as_millis())
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        let changed = fetch_outbox(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("outbox message"))?;
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }

    async fn set_outbox_note(
        &self,
        id: OutboxId,
        note: Option<&str>,
        at: Timestamp,
    ) -> Result<OutboxEntry, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let done = sqlx::query("UPDATE outbox SET note = ?, updated_at = ? WHERE id = ?")
            .bind(note)
            .bind(at.as_millis())
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("outbox message"));
        }
        let changed = fetch_outbox(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("outbox message"))?;
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }

    async fn set_identity_rewritten_from(
        &self,
        id: IdentityId,
        rewritten: Option<&RewrittenFrom>,
    ) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let done =
            sqlx::query("UPDATE identities SET rewritten_from = ?, rewritten_at = ? WHERE id = ?")
                .bind(rewritten.map(|r| r.seen.as_str()))
                .bind(rewritten.map(|r| r.at.as_millis()))
                .bind(id.to_string())
                .execute(&self.pool)
                .await
                .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("identity"));
        }
        Ok(())
    }
}
