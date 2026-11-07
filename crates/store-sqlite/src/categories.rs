use std::collections::{BTreeMap, BTreeSet, HashMap};

use async_trait::async_trait;
use holtmail_category::{CategorySource, MessageKind, Topic, TopicSet, TrustState};
use holtmail_core::{AccountId, Address, ArchiveId, MessageId, Timestamp};
use holtmail_store::*;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse};

const WRITE_BATCH: usize = 500;
const MAX_DERIVE_PAGE: u32 = 10_000;
const MAX_AUTH_FACTS: u32 = 1_000;
const RECOMPUTE_CHUNK: usize = 5_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct OverrideIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<CategoryOverrideId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topics: Option<CategoryOverrideId>,
}

impl OverrideIds {
    fn to_column(self) -> Result<Option<String>, StoreError> {
        if self.kind.is_none() && self.topics.is_none() {
            Ok(None)
        } else {
            json(&self).map(Some)
        }
    }

    fn from_column(text: Option<&str>) -> Self {
        text.and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default()
    }
}

struct Effective {
    kind: MessageKind,
    topics: TopicSet,
    ids: OverrideIds,
}

fn effective(
    derived_kind: MessageKind,
    derived_topics: TopicSet,
    trust: &TrustState,
    address: Option<&CategoryOverride>,
    domain: Option<&CategoryOverride>,
    policy: CategoryPolicy,
) -> Effective {
    let layers = [address, domain];
    let kind = layers
        .iter()
        .flatten()
        .find_map(|o| o.kind.map(|kind| (kind, o.id)));
    let topics = layers
        .iter()
        .flatten()
        .find_map(|o| o.topics.map(|topics| (topics, o.id)));
    Effective {
        kind: kind.map_or(derived_kind, |(kind, _)| kind),
        topics: match topics {
            Some((topics, _)) => topics.gated(trust, policy.health),
            None => derived_topics.known(),
        },
        ids: OverrideIds {
            kind: kind.map(|(_, id)| id),
            topics: topics.map(|(_, id)| id),
        },
    }
}

fn trust_columns(trust: &TrustState) -> (&'static str, Option<&str>) {
    match trust {
        TrustState::Aligned { domain } => ("aligned", Some(domain)),
        TrustState::Relayed { provider } => ("relayed", Some(provider)),
        TrustState::Unverified => ("unverified", None),
        TrustState::Unknown => ("unknown", None),
    }
}

fn trust_state(name: &str, domain: Option<String>) -> Result<TrustState, StoreError> {
    Ok(match name {
        "aligned" => TrustState::Aligned {
            domain: domain.unwrap_or_default(),
        },
        "relayed" => TrustState::Relayed {
            provider: domain.unwrap_or_default(),
        },
        "unverified" => TrustState::Unverified,
        "unknown" => TrustState::Unknown,
        other => return Err(StoreError::Invalid(format!("unknown trust {other:?}"))),
    })
}

fn kind_named(name: &str) -> Result<MessageKind, StoreError> {
    MessageKind::from_name(name)
        .ok_or_else(|| StoreError::Invalid(format!("unknown message kind {name:?}")))
}

fn source_named(name: &str) -> Result<CategorySource, StoreError> {
    CategorySource::from_name(name)
        .ok_or_else(|| StoreError::Invalid(format!("unknown category source {name:?}")))
}

fn sources_column(sources: &BTreeMap<Topic, CategorySource>) -> Result<String, StoreError> {
    let named: BTreeMap<&str, &str> = sources
        .iter()
        .map(|(topic, source)| (topic.name(), source.name()))
        .collect();
    json(&named)
}

fn sources_from(text: &str) -> BTreeMap<Topic, CategorySource> {
    serde_json::from_str::<BTreeMap<String, String>>(text)
        .unwrap_or_default()
        .iter()
        .filter_map(|(topic, source)| {
            Some((Topic::from_name(topic)?, CategorySource::from_name(source)?))
        })
        .collect()
}

fn topics_from(list: Option<&str>) -> TopicSet {
    list.unwrap_or_default()
        .split(',')
        .filter_map(Topic::from_name)
        .collect()
}

fn bits(value: u64) -> i64 {
    value as i64
}

fn unbits(value: i64) -> u64 {
    value as u64
}

fn ids_json(ids: impl IntoIterator<Item = String>) -> Result<String, StoreError> {
    json(&ids.into_iter().collect::<Vec<_>>())
}

pub(crate) fn override_from(row: &SqliteRow) -> Result<CategoryOverride, StoreError> {
    let scope: String = get(row, "scope")?;
    Ok(CategoryOverride {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        scope: OverrideScope::parse(&scope)
            .ok_or_else(|| StoreError::Invalid(format!("unknown override scope {scope:?}")))?,
        key: get(row, "key")?,
        kind: get::<Option<String>>(row, "kind")?
            .as_deref()
            .map(kind_named)
            .transpose()?,
        topics: get::<Option<i64>>(row, "topics")?.map(|t| TopicSet::from_bits(unbits(t))),
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        created_by: parse(&get::<String>(row, "created_by")?)?,
    })
}

fn category_row(row: &SqliteRow) -> Result<CategoryRow, StoreError> {
    let ids = OverrideIds::from_column(get::<Option<String>>(row, "override_ids")?.as_deref());
    Ok(CategoryRow {
        write: CategoryWrite {
            message: parse(&get::<String>(row, "message_id")?)?,
            archive: parse(&get::<String>(row, "archive_id")?)?,
            derive_fp: get(row, "derive_fp")?,
            trust: trust_state(&get::<String>(row, "trust")?, get(row, "trust_domain")?)?,
            trust_authserv: get(row, "trust_authserv")?,
            one_click_ok: get::<i64>(row, "one_click_ok")? != 0,
            own: get::<i64>(row, "own")? != 0,
            sender_address: get(row, "sender_address")?,
            sender_domain: get(row, "sender_domain")?,
            derived_kind: kind_named(&get::<String>(row, "derived_kind")?)?,
            derived_kind_source: source_named(&get::<String>(row, "derived_kind_source")?)?,
            derived_topics: TopicSet::from_bits(unbits(get(row, "derived_topics")?)),
            derived_sources: sources_from(&get::<String>(row, "derived_sources")?),
        },
        kind: kind_named(&get::<String>(row, "kind")?)?,
        topics: topics_from(get::<Option<String>>(row, "topics")?.as_deref()),
        list_topic: get::<Option<String>>(row, "list_topic")?
            .as_deref()
            .and_then(Topic::from_name),
        kind_override: ids.kind,
        topics_override: ids.topics,
    })
}

const CATEGORY_COLUMNS: &str = "c.*, (SELECT group_concat(t.topic) FROM message_topics t
     WHERE t.message_id = c.message_id) AS topics";

fn alias_from(row: &SqliteRow) -> Result<MessageAlias, StoreError> {
    Ok(MessageAlias {
        provider: get(row, "provider")?,
        alias: get(row, "alias")?,
        sender: get::<Option<String>>(row, "sender_email")?.map(|email| Address {
            name: get(row, "sender_name").ok().flatten(),
            email,
        }),
        reverse_alias: get(row, "reverse_alias")?,
        signed: get(row, "signed")?,
    })
}

struct Stored {
    message: MessageId,
    trust: TrustState,
    sender_address: Option<String>,
    sender_domain: Option<String>,
    derived_kind: MessageKind,
    derived_topics: TopicSet,
    kind: MessageKind,
    topics: TopicSet,
    ids: OverrideIds,
}

type Overrides = HashMap<(OverrideScope, String), CategoryOverride>;

type EffectiveColumns = (&'static str, Option<&'static str>, Option<String>);

fn applying(
    overrides: &Overrides,
    address: Option<&String>,
    domain: Option<&String>,
) -> (Option<CategoryOverride>, Option<CategoryOverride>) {
    let find = |scope, key: Option<&String>| {
        key.and_then(|key| overrides.get(&(scope, key.clone())).cloned())
    };
    (
        find(OverrideScope::Address, address),
        find(OverrideScope::Domain, domain),
    )
}

async fn overrides_of(
    tx: &mut Transaction<'_, Sqlite>,
    archive: &str,
) -> Result<Overrides, StoreError> {
    let rows = sqlx::query("SELECT * FROM category_overrides WHERE archive_id = ?")
        .bind(archive)
        .fetch_all(&mut **tx)
        .await
        .map_err(backend)?;
    rows.iter()
        .map(|row| {
            let o = override_from(row)?;
            Ok(((o.scope, o.key.clone()), o))
        })
        .collect()
}

async fn write_topics(
    tx: &mut Transaction<'_, Sqlite>,
    message: &str,
    archive: &str,
    topics: TopicSet,
    replace: bool,
) -> Result<(), StoreError> {
    if replace {
        sqlx::query("DELETE FROM message_topics WHERE message_id = ?")
            .bind(message)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
    }
    for topic in topics.iter() {
        sqlx::query("INSERT INTO message_topics (message_id, archive_id, topic) VALUES (?, ?, ?)")
            .bind(message)
            .bind(archive)
            .bind(topic.name())
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
    }
    Ok(())
}

async fn recompute(
    tx: &mut Transaction<'_, Sqlite>,
    archive: &str,
    scope: OverrideScope,
    key: &str,
    policy: CategoryPolicy,
) -> Result<Vec<MessageId>, StoreError> {
    let overrides = overrides_of(tx, archive).await?;
    let column = match scope {
        OverrideScope::Address => "sender_address",
        OverrideScope::Domain => "sender_domain",
    };
    let sql = format!(
        "SELECT c.message_id, c.trust, c.trust_domain, c.sender_address, c.sender_domain,
             c.derived_kind, c.derived_topics, c.kind, c.override_ids,
             (SELECT group_concat(t.topic) FROM message_topics t
              WHERE t.message_id = c.message_id) AS topics
         FROM message_categories c WHERE c.archive_id = ? AND c.{column} = ?"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(archive)
        .bind(key)
        .fetch_all(&mut **tx)
        .await
        .map_err(backend)?;
    let stored = rows
        .iter()
        .map(|row| {
            Ok(Stored {
                message: parse(&get::<String>(row, "message_id")?)?,
                trust: trust_state(&get::<String>(row, "trust")?, get(row, "trust_domain")?)?,
                sender_address: get(row, "sender_address")?,
                sender_domain: get(row, "sender_domain")?,
                derived_kind: kind_named(&get::<String>(row, "derived_kind")?)?,
                derived_topics: TopicSet::from_bits(unbits(get(row, "derived_topics")?)),
                kind: kind_named(&get::<String>(row, "kind")?)?,
                topics: topics_from(get::<Option<String>>(row, "topics")?.as_deref()),
                ids: OverrideIds::from_column(
                    get::<Option<String>>(row, "override_ids")?.as_deref(),
                ),
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let mut changed = Vec::new();
    let mut updates = Vec::new();
    let mut retopics = Vec::new();
    for row in stored {
        let (address, domain) = applying(
            &overrides,
            row.sender_address.as_ref(),
            row.sender_domain.as_ref(),
        );
        let next = effective(
            row.derived_kind,
            row.derived_topics,
            &row.trust,
            address.as_ref(),
            domain.as_ref(),
            policy,
        );
        let id = row.message.to_string();
        if next.kind != row.kind || next.topics != row.topics || next.ids != row.ids {
            updates.push((
                id.clone(),
                next.kind.name(),
                next.topics.list_topic().map(Topic::name),
                next.ids.to_column()?,
            ));
        }
        if next.topics != row.topics {
            retopics.push((id, next.topics));
        }
        if next.kind != row.kind || next.topics != row.topics {
            changed.push(row.message);
        }
    }
    let mut groups: HashMap<EffectiveColumns, Vec<String>> = HashMap::new();
    for (id, kind, list_topic, ids) in updates {
        groups.entry((kind, list_topic, ids)).or_default().push(id);
    }
    for ((kind, list_topic, ids), messages) in groups {
        for chunk in messages.chunks(RECOMPUTE_CHUNK) {
            sqlx::query(
                "UPDATE message_categories SET kind = ?, list_topic = ?, override_ids = ?
                 WHERE message_id IN (SELECT value FROM json_each(?))",
            )
            .bind(kind)
            .bind(list_topic)
            .bind(&ids)
            .bind(json(&chunk)?)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        }
    }
    for chunk in retopics.chunks(RECOMPUTE_CHUNK) {
        sqlx::query(
            "DELETE FROM message_topics WHERE message_id IN (SELECT value FROM json_each(?))",
        )
        .bind(ids_json(chunk.iter().map(|(id, _)| id.clone()))?)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
        let rows: Vec<(&str, &str)> = chunk
            .iter()
            .flat_map(|(id, topics)| topics.iter().map(move |t| (id.as_str(), t.name())))
            .collect();
        sqlx::query(
            "INSERT INTO message_topics (message_id, archive_id, topic)
             SELECT json_extract(value, '$[0]'), ?, json_extract(value, '$[1]') FROM json_each(?)",
        )
        .bind(archive)
        .bind(json(&rows)?)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    }
    changed.sort();
    Ok(changed)
}

async fn queue_in(tx: &mut Transaction<'_, Sqlite>, ids: &[MessageId]) -> Result<(), StoreError> {
    for chunk in ids.chunks(WRITE_BATCH) {
        sqlx::query(
            "INSERT INTO insight_reindex (message_id, archive_id, seq)
             SELECT id, archive_id, 1 FROM messages WHERE id IN (SELECT value FROM json_each(?))
             ON CONFLICT (message_id) DO UPDATE SET seq = seq + 1",
        )
        .bind(ids_json(chunk.iter().map(ToString::to_string))?)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    }
    Ok(())
}

impl SqliteStore {
    pub(crate) async fn category_views(
        &self,
        ids: &[MessageId],
    ) -> Result<HashMap<MessageId, CategoryView>, StoreError> {
        let mut rows = Vec::new();
        for chunk in ids.chunks(200) {
            let sql = format!(
                "SELECT {CATEGORY_COLUMNS} FROM message_categories c
                 WHERE c.message_id IN (SELECT value FROM json_each(?))"
            );
            let found = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(ids_json(chunk.iter().map(ToString::to_string))?)
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
            for row in &found {
                rows.push(category_row(row)?);
            }
        }
        let wanted: BTreeSet<String> = rows
            .iter()
            .flat_map(|r| [r.kind_override, r.topics_override])
            .flatten()
            .map(|id| id.to_string())
            .collect();
        let mut overrides: HashMap<CategoryOverrideId, CategoryOverride> = HashMap::new();
        let wanted: Vec<String> = wanted.into_iter().collect();
        for chunk in wanted.chunks(200) {
            let found = sqlx::query(
                "SELECT * FROM category_overrides WHERE id IN (SELECT value FROM json_each(?))",
            )
            .bind(ids_json(chunk.iter().cloned())?)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
            for row in &found {
                let o = override_from(row)?;
                overrides.insert(o.id, o);
            }
        }
        Ok(rows
            .into_iter()
            .map(|row| {
                let kind_override = row.kind_override.and_then(|id| overrides.get(&id).cloned());
                let topics_override = row
                    .topics_override
                    .and_then(|id| overrides.get(&id).cloned());
                (
                    row.write.message,
                    CategoryView {
                        row,
                        kind_override,
                        topics_override,
                    },
                )
            })
            .collect())
    }

    async fn write_categories(
        &self,
        writes: &[CategoryWrite],
        policy: CategoryPolicy,
        queue: bool,
    ) -> Result<Vec<MessageId>, StoreError> {
        let mut changed = Vec::new();
        for batch in writes.chunks(WRITE_BATCH) {
            let first = changed.len();
            let list = ids_json(batch.iter().map(|w| w.message.to_string()))?;
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            let mut archives: HashMap<String, String> = HashMap::new();
            for row in sqlx::query(
                "SELECT id, archive_id FROM messages WHERE id IN (SELECT value FROM json_each(?))",
            )
            .bind(&list)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?
            {
                archives.insert(get(&row, "id")?, get(&row, "archive_id")?);
            }
            let mut old: HashMap<String, (MessageKind, TopicSet)> = HashMap::new();
            for row in sqlx::query(
                "SELECT c.message_id, c.kind, (SELECT group_concat(t.topic) FROM message_topics t
                     WHERE t.message_id = c.message_id) AS topics
                 FROM message_categories c WHERE c.message_id IN (SELECT value FROM json_each(?))",
            )
            .bind(&list)
            .fetch_all(&mut *tx)
            .await
            .map_err(backend)?
            {
                old.insert(
                    get(&row, "message_id")?,
                    (
                        kind_named(&get::<String>(&row, "kind")?)?,
                        topics_from(get::<Option<String>>(&row, "topics")?.as_deref()),
                    ),
                );
            }
            let mut overrides: HashMap<String, Overrides> = HashMap::new();
            for write in batch {
                let id = write.message.to_string();
                let Some(archive) = archives.get(&id) else {
                    continue;
                };
                if *archive != write.archive.to_string() {
                    return Err(StoreError::Invalid(
                        "the categories row names another archive".into(),
                    ));
                }
                if !overrides.contains_key(archive) {
                    let loaded = overrides_of(&mut tx, archive).await?;
                    overrides.insert(archive.clone(), loaded);
                }
                let (address, domain) = applying(
                    &overrides[archive],
                    write.sender_address.as_ref(),
                    write.sender_domain.as_ref(),
                );
                let next = effective(
                    write.derived_kind,
                    write.derived_topics,
                    &write.trust,
                    address.as_ref(),
                    domain.as_ref(),
                    policy,
                );
                let ids = next.ids;
                let (trust, trust_domain) = trust_columns(&write.trust);
                sqlx::query(
                    "INSERT OR REPLACE INTO message_categories (message_id, archive_id, derive_fp,
                         trust, trust_domain, trust_authserv, one_click_ok, own, sender_address,
                         sender_domain, derived_kind, derived_kind_source, derived_topics,
                         derived_sources, kind, list_topic, override_ids)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(archive)
                .bind(&write.derive_fp)
                .bind(trust)
                .bind(trust_domain)
                .bind(&write.trust_authserv)
                .bind(write.one_click_ok)
                .bind(write.own)
                .bind(&write.sender_address)
                .bind(&write.sender_domain)
                .bind(write.derived_kind.name())
                .bind(write.derived_kind_source.name())
                .bind(bits(write.derived_topics.bits()))
                .bind(sources_column(&write.derived_sources)?)
                .bind(next.kind.name())
                .bind(next.topics.list_topic().map(Topic::name))
                .bind(ids.to_column()?)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
                let before = old.get(&id).copied();
                let topics_same = before.is_some_and(|(_, topics)| topics == next.topics);
                if !topics_same {
                    write_topics(&mut tx, &id, archive, next.topics, before.is_some()).await?;
                }
                if before != Some((next.kind, next.topics)) {
                    changed.push(write.message);
                }
                old.insert(id, (next.kind, next.topics));
            }
            if queue {
                queue_in(&mut tx, &changed[first..]).await?;
            }
            tx.commit().await.map_err(backend)?;
        }
        Ok(changed)
    }

    async fn derive_rows(&self, rows: Vec<SqliteRow>) -> Result<Vec<DeriveRow>, StoreError> {
        let mut out = rows
            .iter()
            .map(|row| {
                Ok(DeriveRow {
                    message: parse(&get::<String>(row, "message_id")?)?,
                    archive: parse(&get::<String>(row, "archive_id")?)?,
                    from_address: get(row, "from_address")?,
                    signals: unbits(get(row, "signals")?),
                    kinds: unbits(get(row, "kinds")?),
                    auth: get(row, "auth")?,
                    items: get(row, "items")?,
                    alias: None,
                    accounts: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let position: HashMap<MessageId, usize> = out
            .iter()
            .enumerate()
            .map(|(i, r)| (r.message, i))
            .collect();
        let ids: Vec<String> = out.iter().map(|r| r.message.to_string()).collect();
        for chunk in ids.chunks(1_000) {
            let list = ids_json(chunk.iter().cloned())?;
            let aliases = sqlx::query(
                "SELECT * FROM message_aliases WHERE message_id IN (SELECT value FROM json_each(?))",
            )
            .bind(&list)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
            for row in &aliases {
                let message: MessageId = parse(&get::<String>(row, "message_id")?)?;
                if let Some(&i) = position.get(&message) {
                    out[i].alias = Some(alias_from(row)?);
                }
            }
            let accounts = sqlx::query(
                "SELECT DISTINCT l.message_id, f.account_id FROM locations l
                 JOIN folders f ON f.id = l.folder_id
                 WHERE l.message_id IN (SELECT value FROM json_each(?))
                 ORDER BY l.message_id, f.account_id",
            )
            .bind(&list)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
            for row in &accounts {
                let message: MessageId = parse(&get::<String>(row, "message_id")?)?;
                if let Some(&i) = position.get(&message) {
                    out[i]
                        .accounts
                        .push(parse(&get::<String>(row, "account_id")?)?);
                }
            }
        }
        for row in &mut out {
            row.accounts.sort();
            row.accounts.dedup();
        }
        Ok(out)
    }
}

#[async_trait]
impl CategoryStore for SqliteStore {
    async fn derive_inputs(
        &self,
        archive: ArchiveId,
        derive_fp: &str,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<DeriveRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT i.message_id, i.archive_id, i.from_address, i.signals, i.kinds, i.auth, i.items
             FROM message_insights i
             LEFT JOIN message_categories c ON c.message_id = i.message_id
             WHERE +i.archive_id = ? AND i.message_id > ? AND i.outcome IN ('ok', 'partial')
               AND (c.message_id IS NULL OR c.derive_fp != ?)
             ORDER BY i.message_id LIMIT ?",
        )
        .bind(archive.to_string())
        .bind(after.map(|a| a.to_string()).unwrap_or_default())
        .bind(derive_fp)
        .bind(i64::from(limit.min(MAX_DERIVE_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        self.derive_rows(rows).await
    }

    async fn derive_input(&self, message: MessageId) -> Result<Option<DeriveRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT message_id, archive_id, from_address, signals, kinds, auth, items
             FROM message_insights WHERE message_id = ? AND outcome IN ('ok', 'partial')",
        )
        .bind(message.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(self.derive_rows(rows).await?.pop())
    }

    async fn count_needing_derive(
        &self,
        archive: ArchiveId,
        derive_fp: &str,
    ) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM message_insights i
             LEFT JOIN message_categories c ON c.message_id = i.message_id
             WHERE i.archive_id = ? AND i.outcome IN ('ok', 'partial')
               AND (c.message_id IS NULL OR c.derive_fp != ?)",
        )
        .bind(archive.to_string())
        .bind(derive_fp)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn invalidate_derived(&self, ids: &[MessageId]) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        for chunk in ids.chunks(WRITE_BATCH) {
            let list = ids_json(chunk.iter().map(ToString::to_string))?;
            for statement in [
                "DELETE FROM message_topics WHERE message_id IN (SELECT value FROM json_each(?))",
                "DELETE FROM message_categories WHERE message_id IN (SELECT value FROM json_each(?))",
            ] {
                sqlx::query(statement)
                    .bind(&list)
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)
    }

    async fn set_message_categories(
        &self,
        writes: &[CategoryWrite],
        policy: CategoryPolicy,
    ) -> Result<Vec<MessageId>, StoreError> {
        self.write_categories(writes, policy, false).await
    }

    async fn set_message_categories_queued(
        &self,
        writes: &[CategoryWrite],
        policy: CategoryPolicy,
    ) -> Result<Vec<MessageId>, StoreError> {
        self.write_categories(writes, policy, true).await
    }

    async fn put_category_override(
        &self,
        category_override: &CategoryOverride,
        policy: CategoryPolicy,
    ) -> Result<(CategoryOverride, Vec<MessageId>), StoreError> {
        let o = category_override;
        if o.kind.is_none() && o.topics.is_none() {
            return Err(StoreError::Invalid(
                "an override sets a kind, topics or both".into(),
            ));
        }
        if o.key.is_empty() {
            return Err(StoreError::Invalid("an override needs a key".into()));
        }
        let archive = o.archive_id.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let known: Option<String> = sqlx::query_scalar("SELECT id FROM archives WHERE id = ?")
            .bind(&archive)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if known.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        let taken: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM category_overrides
             WHERE id = ? AND NOT (archive_id = ? AND scope = ? AND key = ?)",
        )
        .bind(o.id.to_string())
        .bind(&archive)
        .bind(o.scope.as_str())
        .bind(&o.key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        if taken.is_some() {
            return Err(StoreError::Invalid(
                "the override id belongs to another override".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO category_overrides (id, archive_id, scope, key, kind, topics, created_at,
                 created_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (archive_id, scope, key)
             DO UPDATE SET kind = excluded.kind, topics = excluded.topics",
        )
        .bind(o.id.to_string())
        .bind(&archive)
        .bind(o.scope.as_str())
        .bind(&o.key)
        .bind(o.kind.map(MessageKind::name))
        .bind(o.topics.map(|t| bits(t.bits())))
        .bind(o.created_at.as_millis())
        .bind(o.created_by.to_string())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let stored = sqlx::query(
            "SELECT * FROM category_overrides WHERE archive_id = ? AND scope = ? AND key = ?",
        )
        .bind(&archive)
        .bind(o.scope.as_str())
        .bind(&o.key)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        let stored = override_from(&stored)?;
        let changed = recompute(&mut tx, &archive, o.scope, &o.key, policy).await?;
        queue_in(&mut tx, &changed).await?;
        tx.commit().await.map_err(backend)?;
        Ok((stored, changed))
    }

    async fn delete_category_override(
        &self,
        archive: ArchiveId,
        id: CategoryOverrideId,
        policy: CategoryPolicy,
    ) -> Result<Vec<MessageId>, StoreError> {
        let archive = archive.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let row = sqlx::query(
            "DELETE FROM category_overrides WHERE id = ? AND archive_id = ? RETURNING scope, key",
        )
        .bind(id.to_string())
        .bind(&archive)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?
        .ok_or(StoreError::NotFound("category override"))?;
        let scope: String = get(&row, "scope")?;
        let scope = OverrideScope::parse(&scope)
            .ok_or_else(|| StoreError::Invalid(format!("unknown override scope {scope:?}")))?;
        let key: String = get(&row, "key")?;
        let changed = recompute(&mut tx, &archive, scope, &key, policy).await?;
        queue_in(&mut tx, &changed).await?;
        tx.commit().await.map_err(backend)?;
        Ok(changed)
    }

    async fn category_overrides(
        &self,
        archive: ArchiveId,
    ) -> Result<Vec<CategoryOverride>, StoreError> {
        sqlx::query("SELECT * FROM category_overrides WHERE archive_id = ? ORDER BY scope, key")
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(override_from)
            .collect()
    }

    async fn category_counts(
        &self,
        archive: ArchiveId,
        include_health: bool,
    ) -> Result<CategoryCounts, StoreError> {
        let archive = archive.to_string();
        let kinds: HashMap<String, i64> = sqlx::query_as(
            "SELECT kind, COUNT(*) FROM message_categories WHERE archive_id = ? GROUP BY kind",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .into_iter()
        .collect();
        let topics: HashMap<String, i64> = sqlx::query_as(
            "SELECT topic, COUNT(*) FROM message_topics WHERE archive_id = ? GROUP BY topic",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .into_iter()
        .collect();
        let count = |n: Option<&i64>| n.map_or(0, |&n| u64::try_from(n).unwrap_or(0));
        Ok(CategoryCounts {
            kinds: MessageKind::ALL
                .iter()
                .map(|&kind| (kind, count(kinds.get(kind.name()))))
                .collect(),
            topics: Topic::ALL
                .iter()
                .filter(|&&topic| include_health || topic != Topic::Health)
                .map(|&topic| (topic, count(topics.get(topic.name()))))
                .collect(),
        })
    }

    async fn trusted_authserv_ids(
        &self,
        archive: ArchiveId,
    ) -> Result<BTreeMap<AccountId, Vec<String>>, StoreError> {
        sqlx::query(
            "SELECT a.id, s.ids FROM accounts a LEFT JOIN account_authserv s ON s.account_id = a.id
             WHERE a.archive_id = ?",
        )
        .bind(archive.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(|row| {
            let ids = match get::<Option<String>>(row, "ids")? {
                Some(text) => serde_json::from_str(&text).map_err(backend)?,
                None => Vec::new(),
            };
            Ok((parse(&get::<String>(row, "id")?)?, ids))
        })
        .collect()
    }

    async fn set_trusted_authserv_ids(
        &self,
        account: AccountId,
        ids: &[String],
    ) -> Result<(), StoreError> {
        let account = account.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let known: Option<String> = sqlx::query_scalar("SELECT id FROM accounts WHERE id = ?")
            .bind(&account)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if known.is_none() {
            return Err(StoreError::NotFound("account"));
        }
        sqlx::query("INSERT OR REPLACE INTO account_authserv (account_id, ids) VALUES (?, ?)")
            .bind(&account)
            .bind(json(&ids)?)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)
    }

    async fn recent_auth_facts(
        &self,
        account: AccountId,
        limit: u32,
    ) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT i.auth FROM messages m JOIN message_insights i ON i.message_id = m.id
             WHERE m.archive_id = (SELECT archive_id FROM accounts WHERE id = ?1)
               AND EXISTS (SELECT 1 FROM locations l JOIN folders f ON f.id = l.folder_id
                           WHERE l.message_id = m.id AND f.account_id = ?1)
             ORDER BY m.archived_at DESC, m.id DESC LIMIT ?2",
        )
        .bind(account.to_string())
        .bind(i64::from(limit.min(MAX_AUTH_FACTS)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)
    }

    async fn queue_reindex(&self, ids: &[MessageId]) -> Result<(), StoreError> {
        for chunk in ids.chunks(WRITE_BATCH) {
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            queue_in(&mut tx, chunk).await?;
            tx.commit().await.map_err(backend)?;
        }
        Ok(())
    }

    async fn reindex_queue(
        &self,
        archive: ArchiveId,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<(MessageId, u64)>, StoreError> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT message_id, seq FROM insight_reindex
             WHERE archive_id = ? AND message_id > ? ORDER BY message_id LIMIT ?",
        )
        .bind(archive.to_string())
        .bind(after.map(|a| a.to_string()).unwrap_or_default())
        .bind(i64::from(limit.min(MAX_DERIVE_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .into_iter()
        .map(|(id, seq)| Ok((parse(&id)?, unbits(seq))))
        .collect()
    }

    async fn dequeue_reindex(&self, done: &[(MessageId, u64)]) -> Result<(), StoreError> {
        for chunk in done.chunks(WRITE_BATCH) {
            let rows: Vec<(String, i64)> = chunk
                .iter()
                .map(|(id, seq)| (id.to_string(), bits(*seq)))
                .collect();
            let _write = self.writer.lock().await;
            sqlx::query(
                "DELETE FROM insight_reindex WHERE (message_id, seq) IN
                 (SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]')
                  FROM json_each(?))",
            )
            .bind(json(&rows)?)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        }
        Ok(())
    }

    async fn count_reindex_queue(&self, archive: ArchiveId) -> Result<u64, StoreError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM insight_reindex WHERE archive_id = ?")
                .bind(archive.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(backend)?;
        Ok(u64::try_from(count).unwrap_or(0))
    }
}
