use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, MessageId, PersonId, PersonOverrideId, Timestamp};
use holtmail_store::*;
use sqlx::AssertSqlSafe;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::pim::{ITEM_COLUMNS, item_summary};
use crate::rows::{attachment, backend, get, json, parse, unsigned};

const MAX_PAGE: u32 = 500;
const LAST_TEXT: &str = "\u{10FFFF}";

/// The form addresses are stored and compared in: trimmed and lowercased in ASCII, which the
/// migration that filled the table from older messages can do as well. `None` when it is not an
/// address.
pub(crate) fn address_key(email: &str) -> Option<String> {
    let email = email.trim();
    match email.find('@') {
        Some(at) if at > 0 => Some(email.to_ascii_lowercase()),
        _ => None,
    }
}

fn keys(emails: &[String]) -> Result<String, StoreError> {
    let mut keys: Vec<String> = emails.iter().filter_map(|e| address_key(e)).collect();
    keys.sort();
    keys.dedup();
    json(&keys)
}

fn name_key(person: &Person) -> String {
    person
        .name
        .as_deref()
        .map(|n| n.trim().to_lowercase())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| LAST_TEXT.to_owned())
}

fn search_text(person: &Person) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = &person.name {
        parts.push(name.to_lowercase());
    }
    parts.extend(person.emails.iter().map(|e| e.to_lowercase()));
    parts.extend(person.phones.iter().cloned());
    parts.join("\n")
}

fn person(row: &SqliteRow) -> Result<Person, StoreError> {
    serde_json::from_str(&get::<String>(row, "data")?).map_err(backend)
}

fn person_override(row: &SqliteRow) -> Result<PersonOverride, StoreError> {
    let kind: String = get(row, "kind")?;
    Ok(PersonOverride {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        kind: PersonOverrideKind::parse(&kind)
            .ok_or_else(|| StoreError::Invalid(format!("unknown override kind {kind:?}")))?,
        keys: serde_json::from_str(&get::<String>(row, "keys")?).map_err(backend)?,
        from: serde_json::from_str(&get::<String>(row, "from_keys")?).map_err(backend)?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        created_by: get::<Option<String>>(row, "created_by")?
            .map(|id| parse(&id))
            .transpose()?,
    })
}

const OVERRIDE_COLUMNS: &str = "id, archive_id, kind, keys, from_keys, created_at, created_by";

fn decode_cursor(cursor: &PageCursor) -> Result<(String, String), StoreError> {
    let invalid = || StoreError::Invalid("malformed cursor".into());
    let (id, key) = cursor.0.split_once('|').ok_or_else(invalid)?;
    id.parse::<PersonId>().map_err(|_| invalid())?;
    Ok((id.to_owned(), key.to_owned()))
}

struct Filter {
    sql: String,
    text: Option<String>,
    collection: Option<String>,
}

fn filter(query: &PersonQuery) -> Filter {
    let mut sql = String::from(
        "p.archive_id = ?1 AND (?2 IS NULL OR instr(p.search, ?2) > 0)
         AND (?3 IS NULL OR EXISTS (SELECT 1 FROM person_collections c
             WHERE c.person_id = p.id AND c.collection_id = ?3))",
    );
    let text = query
        .text
        .as_deref()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty());
    let mut collection = None;
    match query.source {
        None => {}
        Some(PersonSource::Contacts) => sql.push_str(" AND p.cards > 0"),
        Some(PersonSource::Mail) => sql.push_str(" AND p.cards = 0"),
        Some(PersonSource::Collection(id)) => collection = Some(id.to_string()),
    }
    if query.differences {
        sql.push_str(" AND p.differences = 1");
    }
    Filter {
        sql,
        text,
        collection,
    }
}

#[async_trait]
impl PeopleStore for SqliteStore {
    async fn mail_addresses(&self, archive: ArchiveId) -> Result<MailAddresses, StoreError> {
        let archive = archive.to_string();
        let mut own: Vec<String> = sqlx::query_scalar::<_, Option<String>>(
            "SELECT address FROM accounts WHERE archive_id = ? AND address IS NOT NULL",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .into_iter()
        .flatten()
        .filter_map(|a| address_key(&a))
        .collect();
        let sent: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT a.email FROM resolved_addresses a
             WHERE a.archive_id = ?1 AND a.field = 'from' AND a.message_id IN (
                 SELECT l.message_id FROM locations l
                 JOIN folders f ON f.id = l.folder_id
                 JOIN accounts ac ON ac.id = f.account_id
                 WHERE ac.archive_id = ?1 AND f.role = 'sent'
                 UNION
                 SELECT ml.message_id FROM message_labels ml
                 JOIN labels lb ON lb.id = ml.label_id
                 WHERE lb.archive_id = ?1 AND lb.name = 'Sent')",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        own.extend(sent);
        let aliases: Vec<String> = sqlx::query_scalar(
            "SELECT address FROM aliases WHERE archive_id = ?1
             UNION SELECT alias FROM message_aliases WHERE archive_id = ?1",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        own.extend(aliases.iter().filter_map(|a| address_key(a)));
        own.sort();
        own.dedup();

        let mut names: HashMap<String, (String, i64)> = HashMap::new();
        for row in sqlx::query(
            "SELECT email, name, COUNT(*) AS uses FROM resolved_addresses
             WHERE archive_id = ? AND name IS NOT NULL GROUP BY email, name",
        )
        .bind(&archive)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        {
            let email: String = get(&row, "email")?;
            let name: String = get(&row, "name")?;
            let uses: i64 = get(&row, "uses")?;
            match names.get(&email) {
                Some((best, most)) if *most > uses || (*most == uses && *best <= name) => {}
                _ => {
                    names.insert(email, (name, uses));
                }
            }
        }

        let rows = sqlx::query(
            "WITH sent AS (
                 SELECT DISTINCT message_id FROM resolved_addresses
                 WHERE archive_id = ?1 AND field = 'from'
                 AND email IN (SELECT value FROM json_each(?2)))
             SELECT a.email,
                 COUNT(DISTINCT a.message_id) AS messages,
                 COUNT(DISTINCT CASE WHEN a.field = 'from' THEN a.message_id END) AS from_them,
                 COUNT(DISTINCT CASE WHEN a.field != 'from' AND s.message_id IS NOT NULL
                     THEN a.message_id END) AS from_me,
                 MIN(a.date) AS first_at, MAX(a.date) AS last_at
             FROM resolved_addresses a LEFT JOIN sent s ON s.message_id = a.message_id
             WHERE a.archive_id = ?1 GROUP BY a.email ORDER BY a.email",
        )
        .bind(&archive)
        .bind(json(&own)?)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut addresses = Vec::with_capacity(rows.len());
        for row in rows {
            let email: String = get(&row, "email")?;
            addresses.push(MailAddress {
                name: names.remove(&email).map(|(name, _)| name),
                email,
                messages: unsigned(get(&row, "messages")?),
                from_them: unsigned(get(&row, "from_them")?),
                from_me: unsigned(get(&row, "from_me")?),
                first_at: Timestamp::from_millis(get(&row, "first_at")?),
                last_at: Timestamp::from_millis(get(&row, "last_at")?),
            });
        }
        Ok(MailAddresses { own, addresses })
    }

    async fn replace_people(
        &self,
        archive: ArchiveId,
        people: &[Person],
    ) -> Result<(), StoreError> {
        if people.iter().any(|p| p.archive_id != archive) {
            return Err(StoreError::Invalid(
                "a person belongs to another archive".into(),
            ));
        }
        let id = archive.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM archives WHERE id = ?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        for statement in [
            "DELETE FROM person_emails WHERE archive_id = ?1",
            "DELETE FROM person_collections WHERE person_id IN (SELECT id FROM people WHERE archive_id = ?1)",
            "DELETE FROM people WHERE archive_id = ?1",
        ] {
            sqlx::query(statement)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        for person in people {
            let person_id = person.id.to_string();
            sqlx::query(
                "INSERT INTO people (id, archive_id, name_key, search, cards, differences, data)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO UPDATE SET archive_id = excluded.archive_id,
                     name_key = excluded.name_key, search = excluded.search,
                     cards = excluded.cards, differences = excluded.differences,
                     data = excluded.data",
            )
            .bind(&person_id)
            .bind(&id)
            .bind(name_key(person))
            .bind(search_text(person))
            .bind(i64::try_from(person.cards.len()).unwrap_or(i64::MAX))
            .bind(person.differences)
            .bind(json(person)?)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            for email in person.emails.iter().filter_map(|e| address_key(e)) {
                sqlx::query(
                    "INSERT OR IGNORE INTO person_emails (archive_id, email, person_id)
                     VALUES (?, ?, ?)",
                )
                .bind(&id)
                .bind(email)
                .bind(&person_id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            for card in &person.cards {
                sqlx::query(
                    "INSERT OR IGNORE INTO person_collections (person_id, collection_id)
                     VALUES (?, ?)",
                )
                .bind(&person_id)
                .bind(card.collection_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)
    }

    async fn list_people(&self, query: &PersonQuery) -> Result<Page<Person>, StoreError> {
        let limit = query.limit.clamp(1, MAX_PAGE);
        let after = query.after.as_ref().map(decode_cursor).transpose()?;
        let filter = filter(query);
        let sql = format!(
            "SELECT p.id, p.name_key, p.data FROM people p WHERE {}
             AND (?4 IS NULL OR p.name_key > ?5 OR (p.name_key = ?5 AND p.id > ?4))
             ORDER BY p.name_key, p.id LIMIT ?6",
            filter.sql
        );
        let (after_id, after_key) = after.unzip();
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(query.archive_id.to_string())
            .bind(filter.text)
            .bind(filter.collection)
            .bind(after_id)
            .bind(after_key)
            .bind(i64::from(limit) + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let mut keys = Vec::with_capacity(rows.len());
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            keys.push(get::<String>(row, "name_key")?);
            items.push(person(row)?);
        }
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items
                .last()
                .map(|last| PageCursor(format!("{}|{}", last.id, keys[items.len() - 1])))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    async fn count_people(&self, query: &PersonQuery) -> Result<u64, StoreError> {
        let filter = filter(query);
        let sql = format!("SELECT COUNT(*) FROM people p WHERE {}", filter.sql);
        let count: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(query.archive_id.to_string())
            .bind(filter.text)
            .bind(filter.collection)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        Ok(unsigned(count))
    }

    async fn get_person(&self, id: PersonId) -> Result<Option<Person>, StoreError> {
        sqlx::query("SELECT data FROM people WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| person(&row))
            .transpose()
    }

    async fn add_person_override(&self, value: &PersonOverride) -> Result<(), StoreError> {
        let valid = match value.kind {
            PersonOverrideKind::Join => value.keys.len() >= 2,
            PersonOverrideKind::Split => !value.keys.is_empty() && !value.from.is_empty(),
        };
        if !valid {
            return Err(StoreError::Invalid(format!(
                "a {} needs more keys",
                value.kind.as_str()
            )));
        }
        let _write = self.writer.lock().await;
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM archives WHERE id = ?")
            .bind(value.archive_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        if exists.is_none() {
            return Err(StoreError::NotFound("archive"));
        }
        let done = sqlx::query(
            "INSERT OR IGNORE INTO person_overrides
             (id, archive_id, kind, keys, from_keys, created_at, created_by)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(value.id.to_string())
        .bind(value.archive_id.to_string())
        .bind(value.kind.as_str())
        .bind(json(&value.keys)?)
        .bind(json(&value.from)?)
        .bind(value.created_at.as_millis())
        .bind(value.created_by.map(|u| u.to_string()))
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if done.rows_affected() == 0 {
            return Err(StoreError::Invalid("the override is already stored".into()));
        }
        Ok(())
    }

    async fn person_overrides(
        &self,
        archive: ArchiveId,
    ) -> Result<Vec<PersonOverride>, StoreError> {
        let sql = format!(
            "SELECT {OVERRIDE_COLUMNS} FROM person_overrides WHERE archive_id = ? ORDER BY id"
        );
        sqlx::query(AssertSqlSafe(sql))
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(person_override)
            .collect()
    }

    async fn get_person_override(
        &self,
        id: PersonOverrideId,
    ) -> Result<Option<PersonOverride>, StoreError> {
        let sql = format!("SELECT {OVERRIDE_COLUMNS} FROM person_overrides WHERE id = ?");
        sqlx::query(AssertSqlSafe(sql))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .map(|row| person_override(&row))
            .transpose()
    }

    async fn remove_person_override(&self, id: PersonOverrideId) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query("DELETE FROM person_overrides WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn people_by_email(
        &self,
        archive: ArchiveId,
        emails: &[String],
    ) -> Result<Vec<(String, PersonId)>, StoreError> {
        if emails.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT email, person_id FROM person_emails
             WHERE archive_id = ? AND email IN (SELECT value FROM json_each(?))",
        )
        .bind(archive.to_string())
        .bind(keys(emails)?)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let mut found: HashMap<String, PersonId> = HashMap::new();
        for row in rows {
            found.insert(
                get(&row, "email")?,
                parse(&get::<String>(&row, "person_id")?)?,
            );
        }
        Ok(emails
            .iter()
            .filter_map(|e| {
                let key = address_key(e)?;
                found.get(&key).map(|id| (e.clone(), *id))
            })
            .collect())
    }

    async fn correspondence(
        &self,
        archive: ArchiveId,
        emails: &[String],
        threads: u32,
        attachments: u32,
    ) -> Result<Correspondence, StoreError> {
        let wanted = keys(emails)?;
        if wanted == "[]" {
            return Ok(Correspondence::default());
        }
        let archive = archive.to_string();
        let hits = "WITH hits AS (
                 SELECT message_id,
                     MAX(field = 'from') AS from_them,
                     MAX(field != 'from') AS to_them
                 FROM message_addresses
                 WHERE archive_id = ?1 AND (email IN (SELECT value FROM json_each(?2))
                     OR email IN (SELECT reverse_alias FROM message_aliases
                         WHERE archive_id = ?1 AND reverse_alias IS NOT NULL
                         AND sender_email IN (SELECT value FROM json_each(?2)))
                     OR email IN (SELECT reverse_alias FROM reverse_aliases
                         WHERE archive_id = ?1
                         AND contact IN (SELECT value FROM json_each(?2))))
                 GROUP BY message_id)";
        let totals = sqlx::query(AssertSqlSafe(format!(
            "{hits} SELECT COUNT(*) AS messages, COALESCE(SUM(h.from_them), 0) AS from_them,
                 COALESCE(SUM(h.to_them), 0) AS to_them, MIN(m.date) AS first_at,
                 MAX(m.date) AS last_at
             FROM hits h JOIN messages m ON m.id = h.message_id"
        )))
        .bind(&archive)
        .bind(&wanted)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        let mut out = Correspondence {
            messages: unsigned(get(&totals, "messages")?),
            from_them: unsigned(get(&totals, "from_them")?),
            to_them: unsigned(get(&totals, "to_them")?),
            first_at: get::<Option<i64>>(&totals, "first_at")?.map(Timestamp::from_millis),
            last_at: get::<Option<i64>>(&totals, "last_at")?.map(Timestamp::from_millis),
            ..Correspondence::default()
        };
        if out.messages == 0 {
            return Ok(out);
        }
        for row in sqlx::query(AssertSqlSafe(format!(
            "{hits} SELECT CAST(strftime('%Y', m.date / 1000, 'unixepoch') AS INTEGER) AS year,
                 COUNT(*) AS messages
             FROM hits h JOIN messages m ON m.id = h.message_id GROUP BY year ORDER BY year"
        )))
        .bind(&archive)
        .bind(&wanted)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        {
            out.years.push(YearCount {
                year: i32::try_from(get::<i64>(&row, "year")?).unwrap_or(0),
                messages: unsigned(get(&row, "messages")?),
            });
        }
        for row in sqlx::query(AssertSqlSafe(format!(
            "{hits} SELECT m.thread_id, COUNT(*) AS messages, MAX(m.date) AS last_at, m.id AS latest
             FROM hits h JOIN messages m ON m.id = h.message_id
             GROUP BY m.thread_id ORDER BY last_at DESC, m.thread_id LIMIT ?3"
        )))
        .bind(&archive)
        .bind(&wanted)
        .bind(i64::from(threads.min(MAX_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        {
            out.threads.push(ThreadActivity {
                thread_id: parse(&get::<String>(&row, "thread_id")?)?,
                messages: unsigned(get(&row, "messages")?),
                latest: parse::<MessageId>(&get::<String>(&row, "latest")?)?,
                last_at: Timestamp::from_millis(get(&row, "last_at")?),
            });
        }
        for row in sqlx::query(AssertSqlSafe(format!(
            "{hits} SELECT a.*, m.date AS message_date, h.from_them AS sent_by_them
             FROM hits h JOIN messages m ON m.id = h.message_id
             JOIN attachments a ON a.message_id = m.id
             WHERE a.inline = 0 AND a.content_type NOT IN ('text/calendar', 'application/ics')
             ORDER BY m.date DESC, m.id DESC, a.ordinal LIMIT ?3"
        )))
        .bind(&archive)
        .bind(&wanted)
        .bind(i64::from(attachments.min(MAX_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        {
            out.attachments.push(ExchangedAttachment {
                message_id: parse(&get::<String>(&row, "message_id")?)?,
                date: Timestamp::from_millis(get(&row, "message_date")?),
                from_them: get::<i64>(&row, "sent_by_them")? != 0,
                attachment: attachment(&row)?,
            });
        }
        Ok(out)
    }

    async fn items_by_email(
        &self,
        archive: ArchiveId,
        kind: ItemKind,
        emails: &[String],
        limit: u32,
    ) -> Result<Vec<ItemSummary>, StoreError> {
        let wanted = keys(emails)?;
        if wanted == "[]" {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM items i JOIN pim_index p ON p.item_id = i.id
             WHERE i.archive_id = ?1 AND i.kind = ?2
             AND EXISTS (SELECT 1 FROM json_each(p.emails) e
                 WHERE lower(e.value) IN (SELECT value FROM json_each(?3)))
             ORDER BY p.starts_at IS NULL, p.starts_at DESC, i.id LIMIT ?4"
        );
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(archive.to_string())
            .bind(kind.as_str())
            .bind(&wanted)
            .bind(i64::from(limit.min(MAX_PAGE)))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        rows.iter().map(item_summary).collect()
    }
}
