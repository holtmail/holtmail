use std::collections::BTreeSet;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, Timestamp, UserId};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse};

type Tx<'a> = Transaction<'a, Sqlite>;

fn names<T: Ord>(
    row: &SqliteRow,
    column: &str,
    read: impl Fn(&str) -> Option<T>,
) -> Result<BTreeSet<T>, StoreError> {
    let list: Vec<String> = serde_json::from_str(&get::<String>(row, column)?).map_err(backend)?;
    list.iter()
        .map(|name| {
            read(name).ok_or_else(|| StoreError::Invalid(format!("unknown {column} {name:?}")))
        })
        .collect()
}

fn device(row: &SqliteRow, listed: Option<BTreeSet<ArchiveId>>) -> Result<Device, StoreError> {
    Ok(Device {
        id: parse(&get::<String>(row, "id")?)?,
        user_id: parse(&get::<String>(row, "user_id")?)?,
        settings: DeviceSettings {
            name: get(row, "name")?,
            mail_archive: get::<Option<String>>(row, "mail_archive")?
                .map(|a| parse(&a))
                .transpose()?,
            archives: listed,
            protocols: names(row, "protocols", DeviceProtocol::parse)?,
            scopes: names(row, "scopes", DeviceScope::parse)?,
            implicit_seen: get::<i64>(row, "implicit_seen")? != 0,
        },
        has_secret: get::<Option<Vec<u8>>>(row, "secret_hash")?.is_some(),
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        last_used_day: get::<Option<i64>>(row, "last_used_day")?.map(Timestamp::from_millis),
        last_protocol: get::<Option<String>>(row, "last_protocol")?
            .map(|p| {
                DeviceProtocol::parse(&p)
                    .ok_or_else(|| StoreError::Invalid(format!("unknown protocol {p:?}")))
            })
            .transpose()?,
        revoked_at: get::<Option<i64>>(row, "revoked_at")?.map(Timestamp::from_millis),
        raised_at: get::<Option<i64>>(row, "raised_at")?.map(Timestamp::from_millis),
    })
}

fn check_rules(settings: &DeviceSettings) -> Result<(), StoreError> {
    if settings.mail_archive.is_none() && settings.protocols.iter().any(|p| p.needs_mail_archive())
    {
        return Err(StoreError::Invalid(
            "IMAP and SMTP need a mail archive".into(),
        ));
    }
    Ok(())
}

async fn check_archives(tx: &mut Tx<'_>, settings: &DeviceSettings) -> Result<(), StoreError> {
    let listed = settings.archives.iter().flatten();
    for archive in settings.mail_archive.iter().chain(listed) {
        let found = sqlx::query_scalar::<_, i64>("SELECT 1 FROM archives WHERE id = ?")
            .bind(archive.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
        if found.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
    }
    Ok(())
}

async fn replace_archives(
    tx: &mut Tx<'_>,
    id: &str,
    settings: &DeviceSettings,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM device_archives WHERE device_id = ?")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    for archive in settings.archives.iter().flatten() {
        sqlx::query("INSERT INTO device_archives (device_id, archive_id) VALUES (?, ?)")
            .bind(id)
            .bind(archive.to_string())
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
    }
    Ok(())
}

fn protocol_names(settings: &DeviceSettings) -> Result<String, StoreError> {
    json(
        &settings
            .protocols
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
    )
}

fn scope_names(settings: &DeviceSettings) -> Result<String, StoreError> {
    json(
        &settings
            .scopes
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
    )
}

impl SqliteStore {
    async fn with_archives(&self, row: &SqliteRow) -> Result<Device, StoreError> {
        if get::<i64>(row, "all_archives")? != 0 {
            return device(row, None);
        }
        let listed: Vec<String> =
            sqlx::query_scalar("SELECT archive_id FROM device_archives WHERE device_id = ?")
                .bind(get::<String>(row, "id")?)
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
        let listed = listed
            .iter()
            .map(|a| parse(a))
            .collect::<Result<BTreeSet<ArchiveId>, _>>()?;
        device(row, Some(listed))
    }
}

#[async_trait]
impl DeviceStore for SqliteStore {
    async fn create_device(&self, new: NewDevice) -> Result<Device, StoreError> {
        check_rules(&new.settings)?;
        let id = DeviceId::new();
        let text = id.to_string();
        {
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            let user = sqlx::query_scalar::<_, i64>("SELECT 1 FROM users WHERE id = ?")
                .bind(new.user_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
            if user.is_none() {
                return Err(StoreError::NotFound("user"));
            }
            check_archives(&mut tx, &new.settings).await?;
            if let Some(hash) = &new.secret_hash {
                let taken =
                    sqlx::query_scalar::<_, i64>("SELECT 1 FROM devices WHERE secret_hash = ?")
                        .bind(&hash[..])
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(backend)?;
                if taken.is_some() {
                    return Err(StoreError::Invalid("another device has this secret".into()));
                }
            }
            sqlx::query(
                "INSERT INTO devices (id, user_id, name, mail_archive, all_archives, protocols,
                 scopes, implicit_seen, secret_hash, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&text)
            .bind(new.user_id.to_string())
            .bind(&new.settings.name)
            .bind(new.settings.mail_archive.map(|a| a.to_string()))
            .bind(i64::from(new.settings.archives.is_none()))
            .bind(protocol_names(&new.settings)?)
            .bind(scope_names(&new.settings)?)
            .bind(i64::from(new.settings.implicit_seen))
            .bind(new.secret_hash.as_ref().map(|h| h.to_vec()))
            .bind(new.at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            replace_archives(&mut tx, &text, &new.settings).await?;
            tx.commit().await.map_err(backend)?;
        }
        self.get_device(id)
            .await?
            .ok_or(StoreError::NotFound("device"))
    }

    async fn get_device(&self, id: DeviceId) -> Result<Option<Device>, StoreError> {
        let row = sqlx::query("SELECT * FROM devices WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        match row {
            Some(row) => Ok(Some(self.with_archives(&row).await?)),
            None => Ok(None),
        }
    }

    async fn find_device_by_secret(
        &self,
        hash: &SessionHash,
    ) -> Result<Option<Device>, StoreError> {
        let row = sqlx::query("SELECT * FROM devices WHERE secret_hash = ?")
            .bind(&hash[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        match row {
            Some(row) => Ok(Some(self.with_archives(&row).await?)),
            None => Ok(None),
        }
    }

    async fn list_devices(&self, user: UserId) -> Result<Vec<Device>, StoreError> {
        let rows = sqlx::query("SELECT * FROM devices WHERE user_id = ? ORDER BY id")
            .bind(user.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let mut devices = Vec::with_capacity(rows.len());
        for row in &rows {
            devices.push(self.with_archives(row).await?);
        }
        Ok(devices)
    }

    async fn update_device_settings(
        &self,
        id: DeviceId,
        settings: &DeviceSettings,
        raised_at: Option<Timestamp>,
    ) -> Result<Device, StoreError> {
        let text = id.to_string();
        {
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            let revoked =
                sqlx::query_scalar::<_, Option<i64>>("SELECT revoked_at FROM devices WHERE id = ?")
                    .bind(&text)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(backend)?
                    .ok_or(StoreError::NotFound("device"))?;
            if revoked.is_some() {
                return Err(StoreError::Invalid("the device is revoked".into()));
            }
            check_rules(settings)?;
            check_archives(&mut tx, settings).await?;
            sqlx::query(
                "UPDATE devices SET name = ?, mail_archive = ?, all_archives = ?, protocols = ?,
                 scopes = ?, implicit_seen = ?, raised_at = COALESCE(?, raised_at) WHERE id = ?",
            )
            .bind(&settings.name)
            .bind(settings.mail_archive.map(|a| a.to_string()))
            .bind(i64::from(settings.archives.is_none()))
            .bind(protocol_names(settings)?)
            .bind(scope_names(settings)?)
            .bind(i64::from(settings.implicit_seen))
            .bind(raised_at.map(|at| at.as_millis()))
            .bind(&text)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            replace_archives(&mut tx, &text, settings).await?;
            tx.commit().await.map_err(backend)?;
        }
        self.get_device(id)
            .await?
            .ok_or(StoreError::NotFound("device"))
    }

    async fn touch_device(
        &self,
        id: DeviceId,
        protocol: DeviceProtocol,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let day = start_of_day(at).as_millis();
        let _write = self.writer.lock().await;
        let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM devices WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Err(StoreError::NotFound("device"));
        }
        sqlx::query(
            "UPDATE devices SET last_used_day = ?1, last_protocol = ?2
             WHERE id = ?3 AND revoked_at IS NULL
               AND (last_used_day IS NULL OR last_used_day < ?1
                    OR (last_used_day = ?1 AND last_protocol IS NOT ?2))",
        )
        .bind(day)
        .bind(protocol.as_str())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn revoke_device(&self, id: DeviceId, at: Timestamp) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let done =
            sqlx::query("UPDATE devices SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
                .bind(at.as_millis())
                .bind(id.to_string())
                .execute(&self.pool)
                .await
                .map_err(backend)?;
        if done.rows_affected() == 1 {
            return Ok(true);
        }
        let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM devices WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        match exists {
            Some(_) => Ok(false),
            None => Err(StoreError::NotFound("device")),
        }
    }

    async fn revoke_user_devices(
        &self,
        user: UserId,
        at: Timestamp,
    ) -> Result<Vec<DeviceId>, StoreError> {
        let _write = self.writer.lock().await;
        let ids: Vec<String> = sqlx::query_scalar(
            "UPDATE devices SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL
             RETURNING id",
        )
        .bind(at.as_millis())
        .bind(user.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut ids = ids
            .iter()
            .map(|id| parse(id))
            .collect::<Result<Vec<DeviceId>, _>>()?;
        ids.sort();
        Ok(ids)
    }
}
