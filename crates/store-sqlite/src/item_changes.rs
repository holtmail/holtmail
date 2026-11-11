use async_trait::async_trait;
use holtmail_core::{
    ArchiveId, CollectionId, ItemChangeId, ItemId, MessageId, OutboxId, Timestamp,
};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use crate::SqliteStore;
use crate::pim::{ITEM_COLUMNS, item_summary, version, write_index};
use crate::rows::{backend, get, json, parse};

const MAX_PAGE: u32 = 500;
const OPEN: &str = "('held', 'sending', 'conflict', 'rejected')";

type Tx<'a> = Transaction<'a, Sqlite>;

fn invalid(reason: &str) -> StoreError {
    StoreError::Invalid(reason.to_owned())
}

fn optional<T: std::str::FromStr>(row: &SqliteRow, column: &str) -> Result<Option<T>, StoreError> {
    get::<Option<String>>(row, column)?
        .map(|v| parse(&v))
        .transpose()
}

fn number(value: Option<i64>) -> Option<u32> {
    value.map(|v| u32::try_from(v).unwrap_or(u32::MAX))
}

fn name<T>(row: &SqliteRow, column: &str, parse: fn(&str) -> Option<T>) -> Result<T, StoreError> {
    let text: String = get(row, column)?;
    parse(&text).ok_or_else(|| StoreError::Invalid(format!("unknown {column} {text:?}")))
}

fn change(row: &SqliteRow) -> Result<ItemChange, StoreError> {
    let content = match get::<Option<String>>(row, "blob_id")? {
        Some(blob) => Some(ChangeContent {
            blob_id: parse(&blob)?,
            content_type: get::<Option<String>>(row, "content_type")?.unwrap_or_default(),
        }),
        None => None,
    };
    Ok(ItemChange {
        id: parse(&get::<String>(row, "id")?)?,
        collection_id: parse(&get::<String>(row, "collection_id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        item_id: optional(row, "item_id")?,
        href: get(row, "href")?,
        op: name(row, "op", ChangeOp::parse)?,
        class: name(row, "class", ChangeClass::parse)?,
        content,
        patch: get::<Option<String>>(row, "patch")?
            .map(|p| serde_json::from_str(&p))
            .transpose()
            .map_err(backend)?,
        base_version: number(get(row, "base_version")?),
        base_etag: get(row, "base_etag")?,
        supersedes: optional(row, "supersedes")?,
        origin: name(row, "origin", ChangeOrigin::parse)?,
        user_id: optional(row, "user_id")?,
        device: get(row, "device")?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        state: name(row, "state", ItemChangeState::parse)?,
        reason: get(row, "reason")?,
        upstream_changed: get::<i64>(row, "upstream_changed")? != 0,
        upstream_deleted: get::<i64>(row, "upstream_deleted")? != 0,
        rebased_onto: number(get(row, "rebased_onto")?),
        attempts: u32::try_from(get::<i64>(row, "attempts")?).unwrap_or(u32::MAX),
        next_attempt_at: get::<Option<i64>>(row, "next_attempt_at")?.map(Timestamp::from_millis),
        last_error: get(row, "last_error")?,
        updated_at: Timestamp::from_millis(get(row, "updated_at")?),
    })
}

fn deferred(row: &SqliteRow) -> Result<DeferredReply, StoreError> {
    Ok(DeferredReply {
        message: parse(&get::<String>(row, "message_id")?)?,
        archive: parse(&get::<String>(row, "archive_id")?)?,
        since: Timestamp::from_millis(get(row, "since")?),
        attempts: u32::try_from(get::<i64>(row, "attempts")?).unwrap_or(u32::MAX),
        next_attempt_at: Timestamp::from_millis(get(row, "next_attempt_at")?),
    })
}

async fn fetch(tx: &mut Tx<'_>, id: &str) -> Result<Option<ItemChange>, StoreError> {
    sqlx::query("SELECT * FROM item_changes WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?
        .map(|row| change(&row))
        .transpose()
}

async fn log(tx: &mut Tx<'_>, change: &ItemChange, at: Timestamp) -> Result<(), StoreError> {
    crate::changes::log(
        tx,
        crate::changes::Row::new(ChangeKind::Item, change.archive_id).item(
            change.collection_id,
            change.item_id.map(|i| i.to_string()),
            change.href.clone(),
            change.id,
        ),
        at,
    )
    .await
}

/// Flags the open changes of `item` after a newer upstream version landed; `restored` also
/// clears `upstream_deleted`, since the item is back.
pub(crate) async fn flag_upstream(
    tx: &mut Tx<'_>,
    item: &str,
    restored: bool,
    at: Timestamp,
) -> Result<(), StoreError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE item_changes SET upstream_changed = 1,
         upstream_deleted = CASE WHEN ?1 THEN 0 ELSE upstream_deleted END, updated_at = ?2
         WHERE item_id = ?3 AND state IN {OPEN}"
    )))
    .bind(restored)
    .bind(at.as_millis())
    .bind(item)
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}

enum Key {
    Item(String),
    Href(String),
}

struct Resolved {
    collection: String,
    archive: String,
    has_account: bool,
    key: Key,
}

impl Resolved {
    fn item(&self) -> Option<&str> {
        match &self.key {
            Key::Item(id) => Some(id),
            Key::Href(_) => None,
        }
    }
}

async fn resolve(tx: &mut Tx<'_>, target: &ChangeTarget) -> Result<Option<Resolved>, StoreError> {
    match target {
        ChangeTarget::Item { item } => {
            let row = sqlx::query(
                "SELECT i.collection_id, i.archive_id, c.account_id FROM items i
                 JOIN collections c ON c.id = i.collection_id WHERE i.id = ?",
            )
            .bind(item.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
            row.map(|row| {
                Ok(Resolved {
                    collection: get(&row, "collection_id")?,
                    archive: get(&row, "archive_id")?,
                    has_account: get::<Option<String>>(&row, "account_id")?.is_some(),
                    key: Key::Item(item.to_string()),
                })
            })
            .transpose()
        }
        ChangeTarget::Href { collection, href } => {
            let id = collection.to_string();
            let Some(row) =
                sqlx::query("SELECT archive_id, account_id FROM collections WHERE id = ?")
                    .bind(&id)
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(backend)?
            else {
                return Ok(None);
            };
            let item: Option<String> = sqlx::query_scalar(
                "SELECT id FROM items WHERE collection_id = ?1
                 AND (dav_name = ?2 OR (dav_name IS NULL AND remote_id = ?2))
                 ORDER BY dav_name IS NULL LIMIT 1",
            )
            .bind(&id)
            .bind(href)
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
            Ok(Some(Resolved {
                collection: id,
                archive: get(&row, "archive_id")?,
                has_account: get::<Option<String>>(&row, "account_id")?.is_some(),
                key: match item {
                    Some(item) => Key::Item(item),
                    None => Key::Href(href.clone()),
                },
            }))
        }
    }
}

async fn chain(tx: &mut Tx<'_>, at: &Resolved, open: bool) -> Result<Vec<ItemChange>, StoreError> {
    let only_open = if open {
        format!("AND state IN {OPEN}")
    } else {
        String::new()
    };
    let rows = match &at.key {
        Key::Item(item) => sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT * FROM item_changes WHERE item_id = ? {only_open} ORDER BY id"
        )))
        .bind(item)
        .fetch_all(&mut **tx)
        .await
        .map_err(backend)?,
        Key::Href(href) => sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT * FROM item_changes WHERE collection_id = ? AND href = ? AND item_id IS NULL
             {only_open} ORDER BY id"
        )))
        .bind(&at.collection)
        .bind(href)
        .fetch_all(&mut **tx)
        .await
        .map_err(backend)?,
    };
    rows.iter().map(change).collect()
}

struct ItemState {
    versions: u32,
    removed: bool,
}

async fn item_state(tx: &mut Tx<'_>, item: &str) -> Result<Option<ItemState>, StoreError> {
    let row = sqlx::query("SELECT versions, removed_at FROM items WHERE id = ?")
        .bind(item)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?;
    row.map(|row| {
        Ok(ItemState {
            versions: u32::try_from(get::<i64>(&row, "versions")?).unwrap_or(u32::MAX),
            removed: get::<Option<i64>>(&row, "removed_at")?.is_some(),
        })
    })
    .transpose()
}

struct Current {
    head: Option<ItemChange>,
    item: Option<ItemState>,
    sending: bool,
}

impl Current {
    fn state(&self) -> ServedState {
        ServedState {
            version: self.item.as_ref().map(|i| i.versions),
            change: self.head.as_ref().map(|c| c.id),
        }
    }

    fn served(&self) -> bool {
        match (&self.head, &self.item) {
            (Some(head), _) => head.op != ChangeOp::Delete,
            (None, Some(item)) => !item.removed,
            (None, None) => false,
        }
    }
}

async fn current(tx: &mut Tx<'_>, at: &Resolved) -> Result<Current, StoreError> {
    let open = chain(tx, at, true).await?;
    let item = match at.item() {
        Some(item) => item_state(tx, item).await?,
        None => None,
    };
    Ok(Current {
        sending: open.iter().any(|c| c.state == ItemChangeState::Sending),
        head: open.into_iter().last(),
        item,
    })
}

fn fits(change: &NewItemChange) -> Result<(), StoreError> {
    let class_fits = match change.op {
        ChangeOp::Create => change.class == ChangeClass::Create,
        ChangeOp::Delete => change.class == ChangeClass::Delete,
        ChangeOp::Update => matches!(
            change.class,
            ChangeClass::Noop | ChangeClass::Personal | ChangeClass::Reply | ChangeClass::Edit
        ),
    };
    if !class_fits {
        return Err(invalid("the class does not fit the operation"));
    }
    match (change.op, &change.content) {
        (ChangeOp::Delete, Some(_)) => Err(invalid("a delete carries no bytes")),
        (ChangeOp::Create | ChangeOp::Update, None) => Err(invalid("the change needs bytes")),
        _ => Ok(()),
    }
}

async fn set_state(
    tx: &mut Tx<'_>,
    id: &str,
    state: ItemChangeState,
    at: Timestamp,
) -> Result<ItemChange, StoreError> {
    sqlx::query("UPDATE item_changes SET state = ?, updated_at = ? WHERE id = ?")
        .bind(state.as_str())
        .bind(at.as_millis())
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    let changed = fetch(tx, id)
        .await?
        .ok_or(StoreError::NotFound("item change"))?;
    log(tx, &changed, at).await?;
    Ok(changed)
}

async fn has_successor(tx: &mut Tx<'_>, id: &str) -> Result<bool, StoreError> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM item_changes WHERE supersedes = ? LIMIT 1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
    Ok(found.is_some())
}

fn due(alias: &str) -> String {
    format!(
        "{a}.state = 'held' AND {a}.class NOT IN ('noop', 'personal')
         AND {a}.upstream_changed = 0 AND {a}.upstream_deleted = 0
         AND ({a}.next_attempt_at IS NULL OR {a}.next_attempt_at <= ?1)
         AND NOT EXISTS (SELECT 1 FROM item_changes n WHERE n.supersedes = {a}.id)
         AND NOT EXISTS (SELECT 1 FROM item_changes s WHERE s.state = 'sending'
             AND s.collection_id = {a}.collection_id
             AND ((s.item_id IS NOT NULL AND s.item_id = {a}.item_id)
                  OR (s.item_id IS NULL AND {a}.item_id IS NULL AND s.href = {a}.href)))",
        a = alias
    )
}

async fn record_writeback(
    tx: &mut Tx<'_>,
    change: &ItemChange,
    new: &NewItemVersion,
    at: Timestamp,
) -> Result<String, StoreError> {
    if new.collection_id != change.collection_id {
        return Err(invalid("the version is for another collection"));
    }
    let collection = change.collection_id.to_string();
    let found = sqlx::query(
        "SELECT i.id, i.versions, i.dav_name, v.blob_id FROM items i
         LEFT JOIN item_versions v ON v.item_id = i.id AND v.number = i.versions
         WHERE i.collection_id = ? AND i.remote_id = ?",
    )
    .bind(&collection)
    .bind(&new.remote_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    let (item, versions, dav_name, latest) = match found {
        Some(row) => (
            get::<String>(&row, "id")?,
            get::<i64>(&row, "versions")?,
            get::<Option<String>>(&row, "dav_name")?,
            get::<Option<String>>(&row, "blob_id")?,
        ),
        None => {
            if change.item_id.is_some() {
                return Err(invalid("the version is for another item"));
            }
            let id = ItemId::new().to_string();
            sqlx::query(
                "INSERT INTO items (id, collection_id, archive_id, remote_id, kind, key,
                 first_seen_at, versions, etag) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?)",
            )
            .bind(&id)
            .bind(&collection)
            .bind(change.archive_id.to_string())
            .bind(&new.remote_id)
            .bind(new.kind.as_str())
            .bind(&new.key)
            .bind(new.seen_at.as_millis())
            .bind(&new.etag)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
            (id, 0, None, None)
        }
    };
    if let Some(own) = change.item_id
        && own.to_string() != item
    {
        return Err(invalid("the version is for another item"));
    }
    if dav_name.is_none()
        && let Some(href) = &change.href
    {
        sqlx::query(
            "UPDATE items SET dav_name = ?1 WHERE id = ?2 AND NOT EXISTS
             (SELECT 1 FROM items o WHERE o.collection_id = ?3 AND o.dav_name = ?1)",
        )
        .bind(href)
        .bind(&item)
        .bind(&collection)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    }
    if let Some(href) = &change.href {
        sqlx::query(
            "UPDATE item_changes SET item_id = ? WHERE collection_id = ? AND href = ?
             AND item_id IS NULL",
        )
        .bind(&item)
        .bind(&collection)
        .bind(href)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    }
    sqlx::query("UPDATE items SET removed_at = NULL, etag = ? WHERE id = ?")
        .bind(&new.etag)
        .bind(&item)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    let blob = new.blob_id.to_string();
    if latest.as_deref() != Some(blob.as_str()) {
        let number = versions + 1;
        sqlx::query(
            "INSERT INTO item_versions (item_id, number, blob_id, content_type, etag, seen_at,
             photo_blob_id, origin, change_id) VALUES (?, ?, ?, ?, ?, ?, ?, 'writeback', ?)",
        )
        .bind(&item)
        .bind(number)
        .bind(&blob)
        .bind(&new.content_type)
        .bind(&new.etag)
        .bind(new.seen_at.as_millis())
        .bind(new.photo_blob_id.map(|b| b.to_string()))
        .bind(change.id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
        sqlx::query("UPDATE items SET versions = ?, kind = ?, key = ? WHERE id = ?")
            .bind(number)
            .bind(new.kind.as_str())
            .bind(&new.key)
            .bind(&item)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        let mut index = new.index.clone();
        index.version = u32::try_from(number).unwrap_or(u32::MAX);
        write_index(tx, &item, &index).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH RECURSIVE later(id) AS (
                 SELECT id FROM item_changes WHERE supersedes = ?3
                 UNION SELECT c.id FROM item_changes c JOIN later l ON c.supersedes = l.id)
             UPDATE item_changes SET
                 upstream_changed = CASE WHEN id IN later THEN upstream_changed ELSE 1 END,
                 rebased_onto = CASE WHEN id IN later THEN ?4 ELSE rebased_onto END,
                 updated_at = ?1
             WHERE item_id = ?2 AND id != ?3 AND state IN {OPEN}"
        )))
        .bind(at.as_millis())
        .bind(&item)
        .bind(change.id.to_string())
        .bind(number)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    }
    Ok(item)
}

#[async_trait]
impl ItemChangeStore for SqliteStore {
    async fn set_write_mode(
        &self,
        collection: CollectionId,
        mode: WriteMode,
    ) -> Result<Collection, StoreError> {
        let id = collection.to_string();
        {
            let _write = self.writer.lock().await;
            let account: Option<(Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT c.account_id, a.kind FROM collections c
                 LEFT JOIN accounts a ON a.id = c.account_id WHERE c.id = ?",
            )
            .bind(&id)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
            match account {
                None => return Err(StoreError::NotFound("collection")),
                Some((None, _)) if mode != WriteMode::ReadOnly => {
                    return Err(invalid("a collection no account owns stays read-only"));
                }
                Some((_, Some(kind))) if kind == "import" && mode != WriteMode::ReadOnly => {
                    return Err(invalid("an imported collection stays read-only"));
                }
                Some(_) => {}
            }
            sqlx::query(
                "UPDATE collections SET previous_write_mode = write_mode, write_mode = ?1,
                 write_mode_changed_at = ?2 WHERE id = ?3 AND write_mode <> ?1",
            )
            .bind(mode.as_str())
            .bind(Timestamp::now().as_millis())
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        }
        self.get_collection(collection)
            .await?
            .ok_or(StoreError::NotFound("collection"))
    }

    async fn record_change(
        &self,
        new: &NewItemChange,
        expect: Expect,
    ) -> Result<RecordedChange, StoreError> {
        fits(new)?;
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(at) = resolve(&mut tx, &new.target).await? else {
            return Err(StoreError::NotFound(match new.target {
                ChangeTarget::Item { .. } => "item",
                ChangeTarget::Href { .. } => "collection",
            }));
        };
        let now = current(&mut tx, &at).await?;
        let holds = match expect {
            Expect::Any => true,
            Expect::Absent => at.item().is_none() && !now.served(),
            Expect::State(seen) => seen == now.state(),
        };
        if !holds {
            return Ok(RecordedChange::PreconditionFailed {
                current: now.state(),
            });
        }
        if !at.has_account {
            return Err(invalid("the collection has no account"));
        }
        match (&at.key, new.op) {
            (Key::Item(_), ChangeOp::Create) => {
                return Err(invalid("a create names an existing item"));
            }
            (Key::Href(_), ChangeOp::Create) if now.served() => {
                return Err(invalid("the href is taken"));
            }
            (Key::Href(_), ChangeOp::Update | ChangeOp::Delete) if !now.served() => {
                return Err(invalid("nothing is served under the href"));
            }
            _ => {}
        }
        let base_etag = match (&at.key, new.base_version) {
            (Key::Item(item), Some(base)) => {
                let etag: Option<Option<String>> = sqlx::query_scalar(
                    "SELECT etag FROM item_versions WHERE item_id = ? AND number = ?",
                )
                .bind(item)
                .bind(i64::from(base))
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
                etag.ok_or_else(|| invalid("the base version does not exist"))?
            }
            (Key::Item(_), None) => return Err(invalid("a change of an item needs a base")),
            (Key::Href(_), Some(_)) => return Err(invalid("there is no base version")),
            (Key::Href(_), None) => None,
        };
        let supersedes = now.head.as_ref().map(|h| h.id);
        if let Some(head) = &now.head
            && head.state != ItemChangeState::Sending
        {
            set_state(
                &mut tx,
                &head.id.to_string(),
                ItemChangeState::Superseded,
                new.at,
            )
            .await?;
        }
        let state = match (&at.key, new.op) {
            (Key::Href(_), ChangeOp::Delete) if !now.sending => ItemChangeState::Applied,
            _ => ItemChangeState::Held,
        };
        let id = ItemChangeId::new().to_string();
        let (item, href) = match &at.key {
            Key::Item(item) => (Some(item.clone()), None),
            Key::Href(href) => (None, Some(href.clone())),
        };
        sqlx::query(
            "INSERT INTO item_changes (id, collection_id, archive_id, item_id, href, op, class,
             blob_id, content_type, patch, base_version, base_etag, supersedes, origin, user_id,
             device, created_at, state, updated_at, next_attempt_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&at.collection)
        .bind(&at.archive)
        .bind(item)
        .bind(href)
        .bind(new.op.as_str())
        .bind(new.class.as_str())
        .bind(new.content.as_ref().map(|c| c.blob_id.to_string()))
        .bind(new.content.as_ref().map(|c| c.content_type.clone()))
        .bind(new.patch.as_ref().map(json).transpose()?)
        .bind(new.base_version.map(i64::from))
        .bind(base_etag)
        .bind(supersedes.map(|s| s.to_string()))
        .bind(new.origin.as_str())
        .bind(new.user_id.map(|u| u.to_string()))
        .bind(&new.device)
        .bind(new.at.as_millis())
        .bind(state.as_str())
        .bind(new.at.as_millis())
        .bind(new.not_before.map(|t| t.as_millis()))
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        if let Key::Item(item) = &at.key {
            match (new.origin, &new.schedule_tag) {
                (ChangeOrigin::Itip, None) => {}
                (ChangeOrigin::Itip, Some(tag)) => {
                    sqlx::query("UPDATE items SET schedule_tag = ? WHERE id = ?")
                        .bind(tag)
                        .bind(item)
                        .execute(&mut *tx)
                        .await
                        .map_err(backend)?;
                }
                _ => {
                    sqlx::query(
                        "UPDATE items SET schedule_tag = NULL
                         WHERE id = ? AND schedule_tag IS NOT NULL",
                    )
                    .bind(item)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
                }
            }
        }
        for message in &new.messages {
            sqlx::query("INSERT INTO item_change_messages (outbox_id, change_id) VALUES (?, ?)")
                .bind(message.to_string())
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        let recorded = fetch(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("item change"))?;
        log(&mut tx, &recorded, new.at).await?;
        tx.commit().await.map_err(backend)?;
        Ok(RecordedChange::Recorded(Box::new(recorded)))
    }

    async fn held_change_messages(&self) -> Result<Vec<OutboxId>, StoreError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT m.outbox_id FROM item_change_messages m
             JOIN outbox o ON o.id = m.outbox_id WHERE o.state = 'draft' ORDER BY m.outbox_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        ids.iter().map(|id| parse(id)).collect()
    }

    async fn defer_reply(
        &self,
        archive: ArchiveId,
        message: MessageId,
        at: Timestamp,
        next_attempt_at: Timestamp,
    ) -> Result<DeferredReply, StoreError> {
        let _write = self.writer.lock().await;
        let row = sqlx::query(
            "INSERT INTO deferred_replies (message_id, archive_id, since, next_attempt_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (message_id) DO UPDATE SET attempts = attempts + 1, next_attempt_at = ?4
             RETURNING *",
        )
        .bind(message.to_string())
        .bind(archive.to_string())
        .bind(at.as_millis())
        .bind(next_attempt_at.as_millis())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        deferred(&row)
    }

    async fn due_replies(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> Result<Vec<DeferredReply>, StoreError> {
        sqlx::query(
            "SELECT * FROM deferred_replies WHERE next_attempt_at <= ?
             ORDER BY next_attempt_at LIMIT ?",
        )
        .bind(now.as_millis())
        .bind(i64::from(limit.min(MAX_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(deferred)
        .collect()
    }

    async fn forget_reply(&self, message: MessageId) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM deferred_replies WHERE message_id = ?")
            .bind(message.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn get_change(&self, id: ItemChangeId) -> Result<Option<ItemChange>, StoreError> {
        sqlx::query("SELECT * FROM item_changes WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| change(&row))
            .transpose()
    }

    async fn effective(&self, target: &ChangeTarget) -> Result<Option<Effective>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(at) = resolve(&mut tx, target).await? else {
            return Ok(None);
        };
        let now = current(&mut tx, &at).await?;
        let (item, latest) = match at.item() {
            Some(id) => {
                let item = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "SELECT {ITEM_COLUMNS} FROM items i LEFT JOIN pim_index p ON p.item_id = i.id
                     WHERE i.id = ?"
                )))
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?
                .map(|row| item_summary(&row))
                .transpose()?;
                let latest = sqlx::query(
                    "SELECT v.* FROM item_versions v JOIN items i ON i.id = v.item_id
                     WHERE v.item_id = ? AND v.number = i.versions",
                )
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?
                .map(|row| version(&row))
                .transpose()?;
                (item, latest)
            }
            None => {
                if chain(&mut tx, &at, false).await?.is_empty() {
                    return Ok(None);
                }
                (None, None)
            }
        };
        let href = match (&item, &at.key) {
            (Some(item), _) => item.item.href().to_owned(),
            (None, Key::Href(href)) => href.clone(),
            (None, Key::Item(_)) => return Ok(None),
        };
        tx.commit().await.map_err(backend)?;
        Ok(Some(Effective {
            collection_id: parse(&at.collection)?,
            href,
            item,
            latest,
            state: now.state(),
            served: now.served(),
            change: now.head,
        }))
    }

    async fn open_changes(&self, query: &ItemChangeQuery) -> Result<Vec<ItemChange>, StoreError> {
        let states: Vec<&str> = if query.states.is_empty() {
            ItemChangeState::ALL
                .iter()
                .filter(|s| s.is_open())
                .map(|s| s.as_str())
                .collect()
        } else {
            query
                .states
                .iter()
                .filter(|s| s.is_open())
                .map(|s| s.as_str())
                .collect()
        };
        if states.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query(
            "SELECT * FROM item_changes WHERE archive_id = ?1
             AND (?2 IS NULL OR collection_id = ?2)
             AND state IN (SELECT value FROM json_each(?3))
             AND (?4 = 0 OR upstream_changed = 1 OR upstream_deleted = 1)
             AND (?6 IS NULL OR id > ?6)
             ORDER BY id LIMIT ?5",
        )
        .bind(query.archive_id.to_string())
        .bind(query.collection_id.map(|c| c.to_string()))
        .bind(json(&states)?)
        .bind(query.flagged_only)
        .bind(i64::from(query.limit.clamp(1, MAX_PAGE)))
        .bind(query.after.map(|a| a.to_string()))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(change)
        .collect()
    }

    async fn changes_of(&self, target: &ChangeTarget) -> Result<Vec<ItemChange>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(at) = resolve(&mut tx, target).await? else {
            return Ok(Vec::new());
        };
        let all = chain(&mut tx, &at, false).await?;
        tx.commit().await.map_err(backend)?;
        Ok(all)
    }

    async fn resolve_change(
        &self,
        id: ItemChangeId,
        resolution: &Resolution,
        at: Timestamp,
    ) -> Result<Option<ItemChange>, StoreError> {
        use ItemChangeState::*;
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(old) = fetch(&mut tx, &id).await? else {
            return Err(StoreError::NotFound("item change"));
        };
        let from: &[ItemChangeState] = match resolution {
            Resolution::Rebased { .. } => &[Held],
            Resolution::Applied | Resolution::Discard => &[Held, Conflict, Rejected],
            Resolution::Conflict { .. } => &[Held, Rejected],
            Resolution::Retry => &[Rejected],
        };
        if !from.contains(&old.state) {
            return Ok(None);
        }
        let millis = at.as_millis();
        match resolution {
            Resolution::Rebased { onto } => {
                let latest = match old.item_id {
                    Some(item) => item_state(&mut tx, &item.to_string()).await?,
                    None => None,
                }
                .ok_or_else(|| invalid("the change has no item to rebase on"))?
                .versions;
                if *onto == 0 || *onto > latest {
                    return Err(invalid("no such upstream version"));
                }
                sqlx::query(
                    "UPDATE item_changes SET rebased_onto = ?1,
                     upstream_changed = CASE WHEN ?1 >= ?2 THEN 0 ELSE upstream_changed END,
                     updated_at = ?3 WHERE id = ?4",
                )
                .bind(i64::from(*onto))
                .bind(i64::from(latest))
                .bind(millis)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            Resolution::Applied | Resolution::Discard => {
                let state = if *resolution == Resolution::Applied {
                    Applied
                } else {
                    Discarded
                };
                sqlx::query("UPDATE item_changes SET state = ?, updated_at = ? WHERE id = ?")
                    .bind(state.as_str())
                    .bind(millis)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
            Resolution::Conflict { reason } => {
                sqlx::query(
                    "UPDATE item_changes SET state = 'conflict', reason = ?, upstream_changed = 0,
                     upstream_deleted = 0, updated_at = ? WHERE id = ?",
                )
                .bind(reason)
                .bind(millis)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            Resolution::Retry => {
                sqlx::query(
                    "UPDATE item_changes SET state = 'held', reason = NULL, attempts = 0,
                     next_attempt_at = NULL, last_error = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(millis)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        let changed = fetch(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("item change"))?;
        log(&mut tx, &changed, at).await?;
        tx.commit().await.map_err(backend)?;
        Ok(Some(changed))
    }

    async fn due_change_collections(
        &self,
        now: Timestamp,
    ) -> Result<Vec<CollectionId>, StoreError> {
        let ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT c.collection_id FROM item_changes c
             JOIN collections k ON k.id = c.collection_id
             WHERE k.write_mode = 'write_through' AND {}
             ORDER BY c.collection_id",
            due("c")
        )))
        .bind(now.as_millis())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        ids.iter().map(|id| parse(id)).collect()
    }

    async fn claim_change(
        &self,
        collection: CollectionId,
        now: Timestamp,
    ) -> Result<Option<ItemChange>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let id: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "UPDATE item_changes SET state = 'sending', attempts = attempts + 1, updated_at = ?1
             WHERE id = (SELECT c.id FROM item_changes c
                 JOIN collections k ON k.id = c.collection_id
                 WHERE c.collection_id = ?2 AND k.write_mode = 'write_through' AND {}
                 ORDER BY c.id LIMIT 1)
             RETURNING id",
            due("c")
        )))
        .bind(now.as_millis())
        .bind(collection.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        let Some(id) = id else {
            return Ok(None);
        };
        let claimed = fetch(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("item change"))?;
        log(&mut tx, &claimed, now).await?;
        tx.commit().await.map_err(backend)?;
        Ok(Some(claimed))
    }

    async fn settle_change(
        &self,
        id: ItemChangeId,
        outcome: &Settle,
        at: Timestamp,
    ) -> Result<Option<ItemChange>, StoreError> {
        let id = id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let Some(old) = fetch(&mut tx, &id).await? else {
            return Err(StoreError::NotFound("item change"));
        };
        if old.state != ItemChangeState::Sending {
            return Ok(None);
        }
        let superseded = has_successor(&mut tx, &id).await?;
        let millis = at.as_millis();
        match outcome {
            Settle::Applied { version } => {
                if let Some(new) = version {
                    record_writeback(&mut tx, &old, new, at).await?;
                } else if old.op == ChangeOp::Delete
                    && let Some(item) = old.item_id
                {
                    sqlx::query(
                        "UPDATE items SET removed_at = COALESCE(removed_at, ?) WHERE id = ?",
                    )
                    .bind(millis)
                    .bind(item.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
                }
                sqlx::query(
                    "UPDATE item_changes SET state = 'applied', next_attempt_at = NULL,
                     last_error = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(millis)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            Settle::Rejected { reason } | Settle::Conflict { reason } => {
                let state = match (superseded, outcome) {
                    (true, _) => ItemChangeState::Superseded,
                    (false, Settle::Rejected { .. }) => ItemChangeState::Rejected,
                    (false, _) => ItemChangeState::Conflict,
                };
                sqlx::query(
                    "UPDATE item_changes SET state = ?, reason = ?, upstream_changed = 0,
                     upstream_deleted = 0, updated_at = ? WHERE id = ?",
                )
                .bind(state.as_str())
                .bind(reason)
                .bind(millis)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            Settle::Retry {
                next_attempt_at,
                error,
            } => {
                let state = if superseded {
                    ItemChangeState::Superseded
                } else {
                    ItemChangeState::Held
                };
                sqlx::query(
                    "UPDATE item_changes SET state = ?, next_attempt_at = ?, last_error = ?,
                     updated_at = ? WHERE id = ?",
                )
                .bind(state.as_str())
                .bind(next_attempt_at.as_millis())
                .bind(error)
                .bind(millis)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        let changed = fetch(&mut tx, &id)
            .await?
            .ok_or(StoreError::NotFound("item change"))?;
        log(&mut tx, &changed, at).await?;
        tx.commit().await.map_err(backend)?;
        Ok(Some(changed))
    }

    async fn release_changes(&self, at: Timestamp) -> Result<Vec<ItemChange>, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM item_changes WHERE state = 'sending' ORDER BY id")
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?;
        let mut released = Vec::new();
        for id in ids {
            let state = if has_successor(&mut tx, &id).await? {
                ItemChangeState::Superseded
            } else {
                ItemChangeState::Held
            };
            let changed = set_state(&mut tx, &id, state, at).await?;
            if state == ItemChangeState::Held {
                released.push(changed);
            }
        }
        tx.commit().await.map_err(backend)?;
        Ok(released)
    }
}
