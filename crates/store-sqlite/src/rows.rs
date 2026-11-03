use std::str::FromStr;

use holtmail_core::{Address, Timestamp};
use holtmail_store::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite};

pub(crate) fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Backend(Box::new(error))
}

pub(crate) fn get<'r, T>(row: &'r SqliteRow, column: &str) -> Result<T, StoreError>
where
    T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite>,
{
    row.try_get(column).map_err(backend)
}

pub(crate) fn parse<T: FromStr>(s: &str) -> Result<T, StoreError> {
    s.parse()
        .map_err(|_| StoreError::Invalid(format!("corrupt value {s:?} in the database")))
}

pub(crate) fn unsigned(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(crate) fn signed(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Invalid(format!("{value} is too large")))
}

pub(crate) fn json(value: &impl Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(backend)
}

fn from_json<T: DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(backend)
}

fn timestamp(row: &SqliteRow, column: &str) -> Result<Timestamp, StoreError> {
    Ok(Timestamp::from_millis(get(row, column)?))
}

fn optional_timestamp(row: &SqliteRow, column: &str) -> Result<Option<Timestamp>, StoreError> {
    Ok(get::<Option<i64>>(row, column)?.map(Timestamp::from_millis))
}

pub(crate) fn archive(row: &SqliteRow) -> Result<Archive, StoreError> {
    Ok(Archive {
        id: parse(&get::<String>(row, "id")?)?,
        name: get(row, "name")?,
        created_at: timestamp(row, "created_at")?,
        save_remote_images: get::<i64>(row, "save_remote_images")? != 0,
    })
}

pub(crate) fn remote_copy(row: &SqliteRow) -> Result<RemoteCopy, StoreError> {
    Ok(RemoteCopy {
        key: get(row, "key")?,
        url: get(row, "url")?,
        blob_id: get::<Option<String>>(row, "blob_id")?
            .map(|id| parse(&id))
            .transpose()?,
        content_type: get(row, "content_type")?,
        size: unsigned(get(row, "size")?),
        fetched_at: timestamp(row, "fetched_at")?,
        error: get(row, "error")?,
    })
}

pub(crate) fn account(row: &SqliteRow) -> Result<Account, StoreError> {
    let kind: String = get(row, "kind")?;
    Ok(Account {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        kind: AccountKind::parse(&kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown account kind {kind:?}")))?,
        name: get(row, "name")?,
        address: get(row, "address")?,
        settings: from_json(&get::<String>(row, "settings")?)?,
        sealed_secret: get(row, "sealed_secret")?,
        enabled: get(row, "enabled")?,
        created_at: timestamp(row, "created_at")?,
    })
}

pub(crate) fn folder(row: &SqliteRow) -> Result<Folder, StoreError> {
    Ok(Folder {
        id: parse(&get::<String>(row, "id")?)?,
        account_id: parse(&get::<String>(row, "account_id")?)?,
        path: get(row, "path")?,
        role: get::<Option<String>>(row, "role")?
            .as_deref()
            .and_then(FolderRole::parse),
        created_at: timestamp(row, "created_at")?,
    })
}

pub(crate) fn message(row: &SqliteRow) -> Result<Message, StoreError> {
    let addresses = |column: &str| -> Result<Vec<Address>, StoreError> {
        from_json(&get::<String>(row, column)?)
    };
    Ok(Message {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        thread_id: parse(&get::<String>(row, "thread_id")?)?,
        blob_id: parse(&get::<String>(row, "blob_id")?)?,
        size: unsigned(get(row, "size")?),
        rfc_message_id: get(row, "rfc_message_id")?,
        subject: get(row, "subject")?,
        from: addresses("from_json")?,
        to: addresses("to_json")?,
        cc: addresses("cc_json")?,
        bcc: addresses("bcc_json")?,
        sent_at: optional_timestamp(row, "sent_at")?,
        received_at: optional_timestamp(row, "received_at")?,
        archived_at: timestamp(row, "archived_at")?,
        date: timestamp(row, "date")?,
        preview: get(row, "preview")?,
        attachment_count: u32::try_from(get::<i64>(row, "attachment_count")?).unwrap_or(u32::MAX),
    })
}

pub(crate) fn attachment(row: &SqliteRow) -> Result<Attachment, StoreError> {
    Ok(Attachment {
        ordinal: u32::try_from(get::<i64>(row, "ordinal")?).unwrap_or(u32::MAX),
        blob_id: parse(&get::<String>(row, "blob_id")?)?,
        filename: get(row, "filename")?,
        content_type: get(row, "content_type")?,
        size: unsigned(get(row, "size")?),
        content_id: get(row, "content_id")?,
        inline: get(row, "inline")?,
    })
}

pub(crate) fn location(row: &SqliteRow) -> Result<Location, StoreError> {
    Ok(Location {
        folder_id: parse(&get::<String>(row, "folder_id")?)?,
        uid: get::<Option<i64>>(row, "uid")?.map(unsigned),
        flags: from_json(&get::<String>(row, "flags")?)?,
        first_seen_at: timestamp(row, "first_seen_at")?,
        last_seen_at: timestamp(row, "last_seen_at")?,
        removed_at: optional_timestamp(row, "removed_at")?,
    })
}

pub(crate) fn flags(row: &SqliteRow) -> Result<Vec<String>, StoreError> {
    from_json(&get::<String>(row, "flags")?)
}

pub(crate) fn user(row: &SqliteRow) -> Result<User, StoreError> {
    let role: String = get(row, "role")?;
    Ok(User {
        id: parse(&get::<String>(row, "id")?)?,
        email: get(row, "email")?,
        name: get(row, "name")?,
        role: InstanceRole::parse(&role)
            .ok_or_else(|| StoreError::Invalid(format!("unknown role {role:?}")))?,
        created_at: timestamp(row, "created_at")?,
    })
}

pub(crate) fn email_key(email: &str) -> String {
    email.trim().to_lowercase()
}

pub(crate) fn unique(error: sqlx::Error, message: &str) -> StoreError {
    match error.as_database_error() {
        Some(db) if db.is_unique_violation() => StoreError::Invalid(message.into()),
        _ => backend(error),
    }
}
