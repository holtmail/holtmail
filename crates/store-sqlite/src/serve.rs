use std::collections::HashMap;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, Timestamp};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Sqlite, Transaction};

use crate::SqliteStore;
use crate::rows::{backend, get, parse, unsigned};

type Tx<'a> = Transaction<'a, Sqlite>;
type Query<'q> = sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments>;

const CHUNK: usize = 200;
const MAX_LIST: u32 = 10_000;
const RESCAN_BACKLOG: i64 = 100_000;
const UID_MAX: i64 = u32::MAX as i64;

const OPEN: &str = "c.state IN ('held', 'sending')";
const TAKES_AWAY: &str =
    "(c.kind = 'move' OR (c.kind = 'delete' AND json_extract(c.payload, '$.expunge') = 1))";
const APPLIED_AT: &str = "COALESCE(c.sent_at, c.updated_at)";
const TARGET: &str =
    "(CASE c.kind WHEN 'move' THEN json_extract(c.payload, '$.target') ELSE c.folder_id END)";

#[derive(Debug, Clone)]
enum Bind {
    Text(String),
    Int(i64),
}

#[derive(Debug, Clone, Default)]
struct Sql {
    text: String,
    binds: Vec<Bind>,
}

impl Sql {
    fn push(&mut self, text: &str) -> &mut Self {
        self.text.push_str(text);
        self
    }

    fn text(&mut self, value: impl ToString) -> &mut Self {
        self.text.push('?');
        self.binds.push(Bind::Text(value.to_string()));
        self
    }

    fn int(&mut self, value: i64) -> &mut Self {
        self.text.push('?');
        self.binds.push(Bind::Int(value));
        self
    }

    fn list(&mut self, values: &[String]) -> &mut Self {
        self.push("(");
        for (n, value) in values.iter().enumerate() {
            if n > 0 {
                self.push(", ");
            }
            self.text(value);
        }
        self.push(")")
    }

    fn append(&mut self, other: &Sql) -> &mut Self {
        self.text.push_str(&other.text);
        self.binds.extend(other.binds.iter().cloned());
        self
    }

    fn query(&self) -> Query<'_> {
        let mut query = sqlx::query(AssertSqlSafe(self.text.clone()));
        for bind in &self.binds {
            query = match bind {
                Bind::Text(value) => query.bind(value.clone()),
                Bind::Int(value) => query.bind(*value),
            };
        }
        query
    }
}

/// The folders a view's messages live in; `None` for every folder of the archive.
fn folder_set(archive: &str, key: &ServedKey) -> Option<Sql> {
    let mut sql = Sql::default();
    match key {
        ServedKey::Folder(folder) => {
            sql.push("= ").text(folder);
        }
        ServedKey::Role(role) => {
            sql.push(
                "IN (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id
                 WHERE a.archive_id = ",
            )
            .text(archive)
            .push(" AND f.role = ")
            .text(role.as_str())
            .push(")");
        }
        ServedKey::Drafts(_) => {
            sql.push(
                "IN (SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id
                 WHERE a.archive_id = ",
            )
            .text(archive)
            .push(" AND f.role = 'drafts')");
        }
        ServedKey::All | ServedKey::Kept => return None,
    }
    Some(sql)
}

fn placed(folders: Option<&Sql>) -> Sql {
    let mut sql = Sql::default();
    sql.push(
        "EXISTS (SELECT 1 FROM locations l WHERE l.message_id = m.id AND l.removed_at IS NULL",
    );
    if let Some(folders) = folders {
        sql.push(" AND l.folder_id ").append(folders);
    }
    sql.push(&format!(
        " AND NOT EXISTS (SELECT 1 FROM mail_changes c WHERE c.message_id = m.id
           AND c.folder_id = l.folder_id AND {TAKES_AWAY}
           AND ({OPEN} OR (c.state = 'applied' AND l.last_seen_at <= {APPLIED_AT}))))"
    ));
    sql
}

fn targeted(folders: Option<&Sql>) -> Sql {
    let mut sql = Sql::default();
    sql.push(
        "EXISTS (SELECT 1 FROM mail_changes c WHERE c.message_id = m.id
         AND c.kind IN ('move', 'append')",
    );
    if let Some(folders) = folders {
        sql.push(&format!(" AND {TARGET} ")).append(folders);
    }
    sql.push(&format!(
        " AND ({OPEN} OR (c.state = 'applied' AND NOT EXISTS (SELECT 1 FROM locations l2
             WHERE l2.message_id = m.id AND l2.folder_id = {TARGET}
             AND l2.last_seen_at > {APPLIED_AT})))
           AND NOT EXISTS (SELECT 1 FROM mail_changes c3 WHERE c3.message_id = m.id
             AND c3.folder_id = {TARGET} AND c3.id > c.id
             AND (c3.kind = 'move' OR (c3.kind = 'delete'
                  AND json_extract(c3.payload, '$.expunge') = 1))
             AND c3.state IN ('held', 'sending', 'applied')))"
    ));
    sql
}

/// The view's current members as `member` and `ord` (archived or saved time in milliseconds),
/// narrowed to the given messages and drafts when filters are given.
fn members(
    archive: &str,
    key: &ServedKey,
    messages: Option<&[String]>,
    drafts: Option<&[String]>,
) -> Sql {
    let mut parts = Vec::new();
    if messages.is_none_or(|m| !m.is_empty()) {
        let mut sql = Sql::default();
        sql.push(
            "SELECT m.id AS member, m.archived_at AS ord FROM messages m WHERE m.archive_id = ",
        )
        .text(archive);
        if let Some(messages) = messages {
            sql.push(" AND m.id IN ").list(messages);
        }
        let folders = folder_set(archive, key);
        match key {
            ServedKey::All => {}
            ServedKey::Kept => {
                sql.push(" AND NOT ")
                    .append(&placed(None))
                    .push(" AND NOT ")
                    .append(&targeted(None));
            }
            _ => {
                sql.push(" AND (")
                    .append(&placed(folders.as_ref()))
                    .push(" OR ")
                    .append(&targeted(folders.as_ref()));
                if let ServedKey::Role(role) = key {
                    sql.push(
                        " OR EXISTS (SELECT 1 FROM local_places lp WHERE lp.message_id = m.id
                         AND lp.role = ",
                    )
                    .text(role.as_str())
                    .push(")");
                }
                sql.push(")");
            }
        }
        parts.push(sql);
    }
    if let ServedKey::Drafts(user) = key
        && drafts.is_none_or(|d| !d.is_empty())
    {
        let mut sql = Sql::default();
        sql.push(
            "SELECT d.id || '.' || d.version AS member, d.saved_at AS ord FROM drafts d
             WHERE d.archive_id = ",
        )
        .text(archive)
        .push(" AND d.user_id = ")
        .text(user);
        if let Some(drafts) = drafts {
            sql.push(" AND d.id IN ").list(drafts);
        }
        parts.push(sql);
    }
    let mut sql = Sql::default();
    sql.push("SELECT member, ord FROM (");
    if parts.is_empty() {
        sql.push("SELECT NULL AS member, NULL AS ord WHERE 0");
    }
    for (n, part) in parts.iter().enumerate() {
        if n > 0 {
            sql.push(" UNION ALL ");
        }
        sql.append(part);
    }
    sql.push(")");
    sql
}

fn member_text(member: &ServedMember) -> String {
    match member {
        ServedMember::Message(message) => message.to_string(),
        ServedMember::Draft { draft, version } => format!("{draft}.{version}"),
    }
}

fn parse_member(text: &str) -> Result<ServedMember, StoreError> {
    match text.split_once('.') {
        Some((draft, version)) => Ok(ServedMember::Draft {
            draft: parse(draft)?,
            version: parse(version)?,
        }),
        None => Ok(ServedMember::Message(parse(text)?)),
    }
}

fn uid32(value: i64) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::Invalid(format!("corrupt UID {value}")))
}

fn seq(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn served_uid(row: &SqliteRow) -> Result<ServedUid, StoreError> {
    Ok(ServedUid {
        uid: uid32(get(row, "uid")?)?,
        member: parse_member(&get::<String>(row, "member")?)?,
        modseq: unsigned(get(row, "modseq")?),
    })
}

/// A view's row as read inside the transaction that changes it.
#[derive(Debug, Clone)]
struct State {
    id: i64,
    archive: String,
    key: ServedKey,
    uidvalidity: i64,
    uidnext: i64,
    highest: i64,
    synced: i64,
    lowest: i64,
    built: bool,
    stale: bool,
    build_ord: Option<i64>,
    build_member: Option<String>,
    build_total: i64,
    created_at: i64,
}

impl State {
    fn from_row(row: &SqliteRow) -> Result<Self, StoreError> {
        let name: String = get(row, "key")?;
        let user = get::<Option<String>>(row, "user_id")?
            .map(|u| parse(&u))
            .transpose()?;
        Ok(Self {
            id: get(row, "id")?,
            archive: get(row, "archive_id")?,
            key: ServedKey::parse(&name, user)
                .ok_or_else(|| StoreError::Invalid(format!("corrupt served key {name:?}")))?,
            uidvalidity: get(row, "uidvalidity")?,
            uidnext: get(row, "uidnext")?,
            highest: get(row, "highest_modseq")?,
            synced: get(row, "synced_seq")?,
            lowest: get(row, "lowest_modseq")?,
            built: get::<i64>(row, "built")? != 0,
            stale: get::<i64>(row, "stale")? != 0,
            build_ord: get(row, "build_ord")?,
            build_member: get(row, "build_member")?,
            build_total: get(row, "build_total")?,
            created_at: get(row, "created_at")?,
        })
    }

    fn mailbox(&self) -> Result<ServedMailbox, StoreError> {
        let assigned = unsigned(self.uidnext - 1);
        let total = unsigned(self.build_total);
        Ok(ServedMailbox {
            archive_id: parse(&self.archive)?,
            key: self.key,
            uidvalidity: u32::try_from(self.uidvalidity)
                .map_err(|_| StoreError::Invalid("corrupt UIDVALIDITY".into()))?,
            uidnext: uid32(self.uidnext)?,
            highest_modseq: unsigned(self.highest),
            synced_seq: unsigned(self.synced),
            lowest_modseq: unsigned(self.lowest),
            built: self.built,
            progress: if self.built {
                BuildProgress {
                    assigned: total,
                    total,
                }
            } else {
                BuildProgress {
                    assigned,
                    total: total.max(assigned),
                }
            },
            created_at: Timestamp::from_millis(self.created_at),
        })
    }

    fn next_uid(&mut self) -> Result<i64, StoreError> {
        if self.uidnext > UID_MAX {
            return Err(StoreError::Invalid(
                "the served mailbox ran out of UIDs".into(),
            ));
        }
        let uid = self.uidnext;
        self.uidnext += 1;
        Ok(uid)
    }
}

async fn find(
    executor: impl sqlx::SqliteExecutor<'_>,
    archive: &str,
    key: &ServedKey,
) -> Result<Option<State>, StoreError> {
    sqlx::query("SELECT * FROM served_mailboxes WHERE archive_id = ? AND key = ? AND user_id IS ?")
        .bind(archive)
        .bind(key.name())
        .bind(key.user().map(|u| u.to_string()))
        .fetch_optional(executor)
        .await
        .map_err(backend)?
        .map(|row| State::from_row(&row))
        .transpose()
}

async fn require(
    executor: impl sqlx::SqliteExecutor<'_>,
    archive: &str,
    key: &ServedKey,
) -> Result<State, StoreError> {
    find(executor, archive, key)
        .await?
        .ok_or(StoreError::NotFound("served mailbox"))
}

async fn head(tx: &mut Tx<'_>) -> Result<i64, StoreError> {
    let seq: Option<i64> =
        sqlx::query_scalar("SELECT seq FROM sqlite_sequence WHERE name = 'changes'")
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
    Ok(seq.unwrap_or(0))
}

async fn save(tx: &mut Tx<'_>, old: &State, new: &State, at: Timestamp) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE served_mailboxes SET uidnext = ?, highest_modseq = ?, synced_seq = ?,
         lowest_modseq = ?, built = ?, stale = ?, build_ord = ?, build_member = ?,
         build_total = ?, updated_at = ?
         WHERE id = ? AND synced_seq = ? AND uidnext = ?",
    )
    .bind(new.uidnext)
    .bind(new.highest)
    .bind(new.synced)
    .bind(new.lowest)
    .bind(new.built)
    .bind(new.stale)
    .bind(new.build_ord)
    .bind(&new.build_member)
    .bind(new.build_total)
    .bind(at.as_millis())
    .bind(old.id)
    .bind(old.synced)
    .bind(old.uidnext)
    .execute(&mut **tx)
    .await
    .map_err(backend)?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "the served mailbox changed during catch-up".into(),
        ))
    }
}

async fn insert_uid(
    tx: &mut Tx<'_>,
    state: &mut State,
    member: &str,
    modseq: i64,
) -> Result<(), StoreError> {
    let uid = state.next_uid()?;
    sqlx::query("INSERT INTO served_uids (mailbox_id, uid, member, modseq) VALUES (?, ?, ?, ?)")
        .bind(state.id)
        .bind(uid)
        .bind(member)
        .bind(modseq)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    state.highest = state.highest.max(modseq);
    Ok(())
}

/// Diffs the whole membership: tombstones members that left, appends those that joined, and
/// with `bump` gives every member `modseq`.
async fn reconcile(
    tx: &mut Tx<'_>,
    state: &mut State,
    modseq: i64,
    bump: bool,
) -> Result<(), StoreError> {
    let all = members(&state.archive, &state.key, None, None);
    let mut sql = Sql::default();
    sql.push("UPDATE served_uids SET expunged_modseq = ")
        .int(modseq)
        .push(" WHERE mailbox_id = ")
        .int(state.id)
        .push(" AND expunged_modseq IS NULL AND member NOT IN (SELECT member FROM (")
        .append(&all)
        .push("))");
    let left = sql
        .query()
        .execute(&mut **tx)
        .await
        .map_err(backend)?
        .rows_affected();
    if bump {
        sqlx::query(
            "UPDATE served_uids SET modseq = ?
             WHERE mailbox_id = ? AND expunged_modseq IS NULL AND modseq < ?",
        )
        .bind(modseq)
        .bind(state.id)
        .bind(modseq)
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
        state.highest = state.highest.max(modseq);
    }
    let mut sql = Sql::default();
    sql.push("INSERT INTO served_uids (mailbox_id, uid, member, modseq) SELECT ")
        .int(state.id)
        .push(", ")
        .int(state.uidnext)
        .push(" + ROW_NUMBER() OVER (ORDER BY ord, member) - 1, member, ")
        .int(modseq)
        .push(" FROM (")
        .append(&all)
        .push(
            ") WHERE member NOT IN (SELECT member FROM served_uids WHERE expunged_modseq IS NULL
             AND mailbox_id = ",
        )
        .int(state.id)
        .push(") ORDER BY ord, member");
    let joined = i64::try_from(
        sql.query()
            .execute(&mut **tx)
            .await
            .map_err(backend)?
            .rows_affected(),
    )
    .unwrap_or(i64::MAX);
    if state.uidnext.saturating_add(joined) > UID_MAX + 1 {
        return Err(StoreError::Invalid(
            "the served mailbox ran out of UIDs".into(),
        ));
    }
    state.uidnext += joined;
    if left > 0 || joined > 0 {
        state.highest = state.highest.max(modseq);
    }
    Ok(())
}

#[derive(Default)]
struct Touched {
    messages: HashMap<String, i64>,
    drafts: HashMap<String, i64>,
    folders: bool,
}

fn touch(map: &mut HashMap<String, i64>, id: String, seq: i64) {
    let entry = map.entry(id).or_insert(seq);
    *entry = (*entry).max(seq);
}

async fn touched(tx: &mut Tx<'_>, state: &State) -> Result<Touched, StoreError> {
    let own = state.key.user().map(|u| u.to_string());
    let rows = sqlx::query(
        "SELECT seq, kind, message_id, draft_id, user_id FROM changes
         WHERE archive_id = ? AND seq > ? ORDER BY seq",
    )
    .bind(&state.archive)
    .bind(state.synced)
    .fetch_all(&mut **tx)
    .await
    .map_err(backend)?;
    let mut out = Touched::default();
    for row in rows {
        let seq: i64 = get(&row, "seq")?;
        let kind: String = get(&row, "kind")?;
        let user: Option<String> = get(&row, "user_id")?;
        let mine = user.is_none() || user == own;
        match (kind.as_str(), get::<Option<String>>(&row, "message_id")?) {
            ("message" | "place" | "thread", Some(message)) if user.is_none() => {
                touch(&mut out.messages, message, seq)
            }
            ("place", Some(message)) if matches!(state.key, ServedKey::Role(_)) => {
                touch(&mut out.messages, message, seq)
            }
            ("flags", Some(message)) if mine || own.is_none() => {
                touch(&mut out.messages, message, seq)
            }
            ("draft", _) if own.is_some() && user == own => {
                if let Some(draft) = get::<Option<String>>(&row, "draft_id")? {
                    touch(&mut out.drafts, draft, seq);
                }
            }
            ("mailbox", _) if user.is_none() => out.folders = true,
            _ => {}
        }
    }
    Ok(out)
}

async fn live(tx: &mut Tx<'_>, state: &State, filter: &Sql) -> Result<Vec<SqliteRow>, StoreError> {
    let mut sql = Sql::default();
    sql.push("SELECT uid, member, modseq FROM served_uids WHERE mailbox_id = ")
        .int(state.id)
        .push(" AND expunged_modseq IS NULL AND ")
        .append(filter);
    sql.query().fetch_all(&mut **tx).await.map_err(backend)
}

/// Applies the rows naming members: joins, leaves and `modseq` raises.
async fn incremental(
    tx: &mut Tx<'_>,
    state: &mut State,
    touched: &Touched,
) -> Result<(), StoreError> {
    let mut joined: Vec<(i64, String, i64)> = Vec::new();
    let mut left: Vec<(i64, i64)> = Vec::new();
    let mut raised: Vec<(i64, i64)> = Vec::new();
    let messages: Vec<String> = touched.messages.keys().cloned().collect();
    let drafts: Vec<String> = touched.drafts.keys().cloned().collect();
    let mut batches: Vec<(Sql, Sql, bool)> = Vec::new();
    for chunk in messages.chunks(CHUNK) {
        let mut filter = Sql::default();
        filter.push("member IN ").list(chunk);
        batches.push((
            members(&state.archive, &state.key, Some(chunk), Some(&[])),
            filter,
            false,
        ));
    }
    for chunk in drafts.chunks(CHUNK) {
        let mut filter = Sql::default();
        filter
            .push("instr(member, '.') > 0 AND substr(member, 1, instr(member, '.') - 1) IN ")
            .list(chunk);
        batches.push((
            members(&state.archive, &state.key, Some(&[]), Some(chunk)),
            filter,
            true,
        ));
    }
    let seq_of = |member: &str, draft: bool| -> i64 {
        if draft {
            let id = member.split_once('.').map_or(member, |(d, _)| d);
            touched.drafts.get(id).copied().unwrap_or(0)
        } else {
            touched.messages.get(member).copied().unwrap_or(0)
        }
    };
    for (current, filter, draft) in &batches {
        let now: HashMap<String, i64> = current
            .query()
            .fetch_all(&mut **tx)
            .await
            .map_err(backend)?
            .iter()
            .map(|row| Ok((get::<String>(row, "member")?, get::<i64>(row, "ord")?)))
            .collect::<Result<_, StoreError>>()?;
        let mut present = std::collections::HashSet::new();
        for row in live(tx, state, filter).await? {
            let uid: i64 = get(&row, "uid")?;
            let member: String = get(&row, "member")?;
            let modseq: i64 = get(&row, "modseq")?;
            let seq = seq_of(&member, *draft);
            if now.contains_key(&member) {
                if seq > modseq {
                    raised.push((uid, seq));
                }
                present.insert(member);
            } else {
                left.push((uid, seq));
            }
        }
        for (member, ord) in now {
            if !present.contains(&member) {
                let seq = seq_of(&member, *draft);
                joined.push((ord, member, seq));
            }
        }
    }
    for (uid, seq) in left {
        sqlx::query("UPDATE served_uids SET expunged_modseq = ? WHERE mailbox_id = ? AND uid = ?")
            .bind(seq)
            .bind(state.id)
            .bind(uid)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        state.highest = state.highest.max(seq);
    }
    for (uid, seq) in raised {
        sqlx::query("UPDATE served_uids SET modseq = ? WHERE mailbox_id = ? AND uid = ?")
            .bind(seq)
            .bind(state.id)
            .bind(uid)
            .execute(&mut **tx)
            .await
            .map_err(backend)?;
        state.highest = state.highest.max(seq);
    }
    joined.sort();
    for (_, member, seq) in joined {
        insert_uid(tx, state, &member, seq).await?;
    }
    Ok(())
}

async fn catch_up(tx: &mut Tx<'_>, state: &mut State) -> Result<(), StoreError> {
    let head = head(tx).await?;
    if head <= state.synced {
        return Ok(());
    }
    let backlog: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM changes WHERE archive_id = ? AND seq > ?")
            .bind(&state.archive)
            .bind(state.synced)
            .fetch_one(&mut **tx)
            .await
            .map_err(backend)?;
    if state.stale || backlog > RESCAN_BACKLOG {
        reconcile(tx, state, head, true).await?;
    } else {
        let touched = touched(tx, state).await?;
        if touched.folders {
            reconcile(tx, state, head, false).await?;
        }
        incremental(tx, state, &touched).await?;
    }
    state.synced = head;
    state.stale = false;
    Ok(())
}

async fn build_step(tx: &mut Tx<'_>, state: &mut State, limit: u32) -> Result<(), StoreError> {
    let all = members(&state.archive, &state.key, None, None);
    if state.build_ord.is_none() && state.uidnext == 1 {
        let mut count = Sql::default();
        count
            .push("SELECT COUNT(*) AS n FROM (")
            .append(&all)
            .push(")");
        let row = count.query().fetch_one(&mut **tx).await.map_err(backend)?;
        state.build_total = get(&row, "n")?;
    }
    let mut sql = Sql::default();
    sql.push("SELECT member, ord FROM (")
        .append(&all)
        .push(") WHERE ");
    if let (Some(ord), Some(member)) = (state.build_ord, &state.build_member) {
        sql.push("(ord, member) > (")
            .int(ord)
            .push(", ")
            .text(member)
            .push(") AND ");
    }
    sql.push(
        "member NOT IN (SELECT member FROM served_uids WHERE expunged_modseq IS NULL
         AND mailbox_id = ",
    )
    .int(state.id)
    .push(") ORDER BY ord, member LIMIT ")
    .int(i64::from(limit) + 1);
    let rows = sql.query().fetch_all(&mut **tx).await.map_err(backend)?;
    let creation = state.synced.max(1);
    for row in rows.iter().take(limit as usize) {
        let member: String = get(row, "member")?;
        insert_uid(tx, state, &member, creation).await?;
        state.build_ord = Some(get(row, "ord")?);
        state.build_member = Some(member);
    }
    if rows.len() <= limit as usize {
        state.built = true;
        state.build_ord = None;
        state.build_member = None;
        state.build_total = state.uidnext - 1;
        catch_up(tx, state).await?;
    }
    Ok(())
}

async fn check_key(tx: &mut Tx<'_>, archive: &str, key: &ServedKey) -> Result<(), StoreError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM archives WHERE id = ?")
        .bind(archive)
        .fetch_optional(&mut **tx)
        .await
        .map_err(backend)?;
    found.ok_or(StoreError::NotFound("archive"))?;
    match key {
        ServedKey::Folder(folder) => {
            let found: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM folders f JOIN accounts a ON a.id = f.account_id
                 WHERE f.id = ? AND a.archive_id = ?",
            )
            .bind(folder.to_string())
            .bind(archive)
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
            found.ok_or(StoreError::NotFound("folder"))?;
        }
        ServedKey::Drafts(user) => {
            let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
                .bind(user.to_string())
                .fetch_optional(&mut **tx)
                .await
                .map_err(backend)?;
            found.ok_or(StoreError::NotFound("user"))?;
        }
        _ => {}
    }
    Ok(())
}

impl SqliteStore {
    async fn served_has_news(&self, state: &State) -> Result<bool, StoreError> {
        if !state.built {
            return Ok(false);
        }
        if state.stale {
            return Ok(true);
        }
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM changes WHERE archive_id = ? AND seq > ? LIMIT 1")
                .bind(&state.archive)
                .bind(state.synced)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        Ok(found.is_some())
    }
}

#[async_trait]
impl ServeStore for SqliteStore {
    async fn open_served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        at: Timestamp,
    ) -> Result<ServedMailbox, StoreError> {
        let archive = archive.to_string();
        if let Some(state) = find(&self.pool, &archive, &key).await? {
            return state.mailbox();
        }
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        if let Some(state) = find(&mut *tx, &archive, &key).await? {
            return state.mailbox();
        }
        check_key(&mut tx, &archive, &key).await?;
        let start = head(&mut tx).await?.max(1);
        sqlx::query(
            "INSERT INTO served_mailboxes (archive_id, user_id, key, uidvalidity, uidnext,
             highest_modseq, synced_seq, lowest_modseq, built, created_at, updated_at)
             VALUES (?, ?, ?, (abs(random()) % 4294967295) + 1, 1, ?, ?, ?, 0, ?, ?)",
        )
        .bind(&archive)
        .bind(key.user().map(|u| u.to_string()))
        .bind(key.name())
        .bind(start)
        .bind(start)
        .bind(start)
        .bind(at.as_millis())
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let state = require(&mut *tx, &archive, &key).await?;
        tx.commit().await.map_err(backend)?;
        state.mailbox()
    }

    async fn served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
    ) -> Result<Option<ServedMailbox>, StoreError> {
        find(&self.pool, &archive.to_string(), &key)
            .await?
            .map(|state| state.mailbox())
            .transpose()
    }

    async fn served_mailboxes(&self, archive: ArchiveId) -> Result<Vec<ServedMailbox>, StoreError> {
        sqlx::query("SELECT * FROM served_mailboxes WHERE archive_id = ? ORDER BY key, user_id")
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(|row| State::from_row(row)?.mailbox())
            .collect()
    }

    async fn build_served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        limit: u32,
        at: Timestamp,
    ) -> Result<ServedMailbox, StoreError> {
        let archive = archive.to_string();
        let state = require(&self.pool, &archive, &key).await?;
        if state.built && !self.served_has_news(&state).await? {
            return state.mailbox();
        }
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let old = require(&mut *tx, &archive, &key).await?;
        let mut state = old.clone();
        if state.built {
            catch_up(&mut tx, &mut state).await?;
        } else {
            build_step(&mut tx, &mut state, limit).await?;
        }
        save(&mut tx, &old, &state, at).await?;
        tx.commit().await.map_err(backend)?;
        state.mailbox()
    }

    async fn catch_up_served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        at: Timestamp,
    ) -> Result<ServedMailbox, StoreError> {
        let archive = archive.to_string();
        let state = require(&self.pool, &archive, &key).await?;
        if !self.served_has_news(&state).await? {
            return state.mailbox();
        }
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let old = require(&mut *tx, &archive, &key).await?;
        if !old.built {
            return old.mailbox();
        }
        let mut state = old.clone();
        catch_up(&mut tx, &mut state).await?;
        save(&mut tx, &old, &state, at).await?;
        tx.commit().await.map_err(backend)?;
        state.mailbox()
    }

    async fn served_uids(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        query: &ServedUidQuery,
    ) -> Result<Vec<ServedUid>, StoreError> {
        let state = require(&self.pool, &archive.to_string(), &key).await?;
        sqlx::query(
            "SELECT uid, member, modseq FROM served_uids
             WHERE mailbox_id = ? AND expunged_modseq IS NULL AND uid > ? AND modseq > ?
             ORDER BY uid LIMIT ?",
        )
        .bind(state.id)
        .bind(i64::from(query.after_uid))
        .bind(query.changed_since.map_or(-1, seq))
        .bind(i64::from(query.limit.min(MAX_LIST)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(served_uid)
        .collect()
    }

    async fn served_uid_of(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        members: &[ServedMember],
    ) -> Result<HashMap<ServedMember, u32>, StoreError> {
        let state = require(&self.pool, &archive.to_string(), &key).await?;
        let texts: Vec<String> = members.iter().map(member_text).collect();
        let mut out = HashMap::new();
        for chunk in texts.chunks(CHUNK) {
            let mut sql = Sql::default();
            sql.push("SELECT uid, member, modseq FROM served_uids WHERE mailbox_id = ")
                .int(state.id)
                .push(" AND expunged_modseq IS NULL AND member IN ")
                .list(chunk);
            for row in sql.query().fetch_all(&self.pool).await.map_err(backend)? {
                let found = served_uid(&row)?;
                out.insert(found.member, found.uid);
            }
        }
        Ok(out)
    }

    async fn served_vanished(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        since: u64,
        after_uid: u32,
        limit: u32,
    ) -> Result<Vec<u32>, StoreError> {
        let state = require(&self.pool, &archive.to_string(), &key).await?;
        sqlx::query_scalar::<_, i64>(
            "SELECT uid FROM served_uids
             WHERE mailbox_id = ? AND expunged_modseq > ? AND uid > ?
             ORDER BY uid LIMIT ?",
        )
        .bind(state.id)
        .bind(seq(since))
        .bind(i64::from(after_uid))
        .bind(i64::from(limit.min(MAX_LIST)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .into_iter()
        .map(uid32)
        .collect()
    }

    async fn prune_served(&self, through: u64, at: Timestamp) -> Result<ServedPrune, StoreError> {
        let through = seq(through);
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query(
            "UPDATE served_mailboxes SET lowest_modseq = ?1, updated_at = ?2
             WHERE lowest_modseq < ?1 AND id IN (SELECT mailbox_id FROM served_uids
               WHERE expunged_modseq IS NOT NULL AND expunged_modseq <= ?1)",
        )
        .bind(through)
        .bind(at.as_millis())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        let tombstones = sqlx::query(
            "DELETE FROM served_uids WHERE expunged_modseq IS NOT NULL AND expunged_modseq <= ?",
        )
        .bind(through)
        .execute(&mut *tx)
        .await
        .map_err(backend)?
        .rows_affected();
        let stale = sqlx::query(
            "UPDATE served_mailboxes SET stale = 1, updated_at = ?
             WHERE synced_seq < ? AND stale = 0",
        )
        .bind(at.as_millis())
        .bind(through)
        .execute(&mut *tx)
        .await
        .map_err(backend)?
        .rows_affected();
        tx.commit().await.map_err(backend)?;
        Ok(ServedPrune { tombstones, stale })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use holtmail_store::{AccountKind, ArchiveStore, MessageStore, NewAccount, ServeStore};
    use serde_json::json;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn times_building_and_catching_up_a_generated_archive() {
        const MESSAGES: i64 = if cfg!(debug_assertions) {
            50_000
        } else {
            200_000
        };
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap();
        let archive = store.create_archive("Generated").await.unwrap().id;
        let account = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Imap,
                name: "Generated".into(),
                address: Some("ada@example.com".into()),
                settings: json!({}),
                sealed_secret: None,
                enabled: false,
            })
            .await
            .unwrap()
            .id;
        let inbox = store
            .upsert_folder(account, "INBOX", Some(FolderRole::Inbox))
            .await
            .unwrap()
            .id;
        for statement in [
            "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < ?2)
             INSERT INTO messages (id, archive_id, thread_id, blob_id, size, from_json, to_json,
                 cc_json, bcc_json, archived_at, date, preview, attachment_count)
             SELECT printf('00000000-0000-7000-8000-%012d', n), ?1,
                 printf('00000000-0000-7000-9000-%012d', n), printf('%064d', n), 100,
                 '[]', '[]', '[]', '[]', n, n, '', 0 FROM s",
            "INSERT INTO locations (folder_id, message_id, uid, flags, first_seen_at, last_seen_at)
             SELECT ?3, id, date, '[]', 0, 0 FROM messages
             WHERE archive_id = ?1 AND date % 2 = 0 AND ?2 > 0",
        ] {
            sqlx::query(statement)
                .bind(archive.to_string())
                .bind(MESSAGES)
                .bind(inbox.to_string())
                .execute(&store.pool)
                .await
                .unwrap();
        }

        let key = ServedKey::Folder(inbox);
        let started = Instant::now();
        store
            .open_served_mailbox(archive, key, Timestamp::from_secs(0))
            .await
            .unwrap();
        let mut steps = 0;
        loop {
            steps += 1;
            let view = store
                .build_served_mailbox(archive, key, 5000, Timestamp::from_secs(0))
                .await
                .unwrap();
            if view.built {
                assert_eq!(i64::from(view.uidnext) - 1, MESSAGES / 2);
                break;
            }
        }
        println!(
            "building a folder view of {} in {MESSAGES} messages, {steps} steps: {:?}",
            MESSAGES / 2,
            started.elapsed()
        );

        let started = Instant::now();
        store
            .open_served_mailbox(archive, ServedKey::Kept, Timestamp::from_secs(0))
            .await
            .unwrap();
        let kept = store
            .build_served_mailbox(
                archive,
                ServedKey::Kept,
                u32::MAX - 1,
                Timestamp::from_secs(0),
            )
            .await
            .unwrap();
        assert_eq!(i64::from(kept.uidnext) - 1, MESSAGES - MESSAGES / 2);
        println!(
            "building Only in Holtmail in one step: {:?}",
            started.elapsed()
        );

        for uid in (2..=200).step_by(2) {
            store
                .set_flags(inbox, uid, &["\\Seen".to_string()])
                .await
                .unwrap();
        }
        let started = Instant::now();
        let view = store
            .catch_up_served_mailbox(archive, key, Timestamp::from_secs(0))
            .await
            .unwrap();
        println!("catching up 100 flag changes: {:?}", started.elapsed());
        assert_eq!(view.synced_seq, store.latest_seq().await.unwrap());

        store
            .prune_served(view.synced_seq + 1, Timestamp::from_secs(0))
            .await
            .unwrap();
        store.set_flags(inbox, 2, &[]).await.unwrap();
        let started = Instant::now();
        store
            .catch_up_served_mailbox(archive, key, Timestamp::from_secs(0))
            .await
            .unwrap();
        println!("rescanning the folder view: {:?}", started.elapsed());
    }
}
