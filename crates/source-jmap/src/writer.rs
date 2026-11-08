use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use holtmail_core::Timestamp;
use holtmail_source::{Flag, FlagResult, MailboxWriteCaps, MailboxWriter, Moved, SourceError};
use serde_json::{Map, Value, json};

use crate::{JmapSession, JmapSource, expand, ids, protocol, received, string, time, uid};

const ATTEMPTS: usize = 5;
const WINDOW_GAP: i64 = 3600;

/// Changes mail on a JMAP account: keywords, mailboxes, imported and destroyed emails.
///
/// Every call opens its own session. UIDs are those [`JmapSource`] derives, the same in every
/// mailbox, so a moved message keeps its UID and an imported one's is known at once. Mailboxes
/// are folders, never labels, so [`MailboxWriter::set_labels`] is `Unsupported`, and so are
/// `unchanged_since` (JMAP has no per-message mod-sequences) and marking deleted without
/// expunging (JMAP has no `\Deleted`).
pub struct JmapWriter {
    source: JmapSource,
}

impl JmapSource {
    /// A writer for the same account, signing in the same way.
    pub fn writer(&self) -> JmapWriter {
        JmapWriter {
            source: self.clone(),
        }
    }
}

struct Located {
    uid: u64,
    id: String,
    mailboxes: Vec<String>,
}

fn keyword(flag: &Flag) -> Result<String, SourceError> {
    let name = match flag {
        Flag::Seen => "$seen",
        Flag::Answered => "$answered",
        Flag::Flagged => "$flagged",
        Flag::Draft => "$draft",
        Flag::Keyword(name) => name,
    };
    let spellable = !name.is_empty()
        && name
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !b"(){]%*\"\\".contains(&b));
    if spellable {
        Ok(name.to_ascii_lowercase())
    } else {
        Err(SourceError::Unsupported(
            "a keyword JMAP cannot spell".into(),
        ))
    }
}

fn keyword_patch(add: &[Flag], remove: &[Flag]) -> Result<Map<String, Value>, SourceError> {
    let mut patch = Map::new();
    for flag in add {
        patch.insert(format!("keywords/{}", keyword(flag)?), json!(true));
    }
    for flag in remove {
        patch.insert(format!("keywords/{}", keyword(flag)?), Value::Null);
    }
    Ok(patch)
}

fn windows(uids: &[u64]) -> Vec<(i64, i64)> {
    let seconds: BTreeSet<i64> = uids
        .iter()
        .map(|uid| (uid >> crate::UID_HASH_BITS) as i64)
        .collect();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for second in seconds {
        match out.last_mut() {
            Some((_, end)) if second - *end <= WINDOW_GAP => *end = second,
            _ => out.push((second, second)),
        }
    }
    out
}

fn failures(answer: &Value, key: &str) -> Result<Vec<String>, SourceError> {
    let mut missing = Vec::new();
    if let Some(refused) = answer.get(key).and_then(Value::as_object) {
        for (id, error) in refused {
            match error.get("type").and_then(Value::as_str) {
                Some("notFound") => missing.push(id.clone()),
                kind => {
                    return Err(SourceError::Protocol(format!(
                        "the JMAP server refused to change an email: {}",
                        kind.unwrap_or("unknown")
                    )));
                }
            }
        }
    }
    Ok(missing)
}

enum Change {
    Update(String, Map<String, Value>),
    Destroy(String),
}

impl JmapSession {
    async fn mailbox_id(&mut self, path: &str) -> Result<String, SourceError> {
        self.load_mailboxes()
            .await?
            .iter()
            .find(|m| m.path == path)
            .map(|m| m.id.clone())
            .ok_or_else(|| SourceError::Missing(format!("there is no mailbox {path}")))
    }

    /// The emails in `mailbox` with these UIDs, ascending by UID, and an `Email` state no newer
    /// than what they show. Where two emails share a UID, the one with the smaller id, as
    /// listing keeps it.
    async fn locate(
        &self,
        mailbox: &str,
        uids: &[u64],
    ) -> Result<(Vec<Located>, Option<String>), SourceError> {
        let wanted: BTreeSet<u64> = uids.iter().copied().collect();
        let mut found = Vec::new();
        for (start, end) in windows(uids) {
            let mut position = 0usize;
            loop {
                let answer = self
                    .call(vec![(
                        "Email/query",
                        json!({
                            "accountId": self.account,
                            "filter": {
                                "inMailbox": mailbox,
                                "after": time::format_utc(Timestamp::from_secs(start - 1)),
                                "before": time::format_utc(Timestamp::from_secs(end + 1)),
                            },
                            "sort": [{ "property": "receivedAt", "isAscending": true }],
                            "position": position,
                            "limit": self.max_objects,
                        }),
                    )])
                    .await?
                    .pop()
                    .unwrap_or_default();
                let page = ids(&answer)?;
                if page.is_empty() {
                    break;
                }
                position += page.len();
                found.extend(page);
            }
        }
        let (emails, state) = self
            .emails_with_state(&found, &["id", "receivedAt", "mailboxIds"])
            .await?;
        let mut located: BTreeMap<u64, Located> = BTreeMap::new();
        for email in emails {
            let id = string(&email, "id")?;
            let mailboxes: Vec<String> = email
                .get("mailboxIds")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter(|(_, on)| on.as_bool().unwrap_or(false))
                        .map(|(id, _)| id.clone())
                        .collect()
                })
                .unwrap_or_default();
            let uid = uid(&id, received(&email));
            if !wanted.contains(&uid) || !mailboxes.iter().any(|m| m == mailbox) {
                continue;
            }
            if located.get(&uid).is_some_and(|other| other.id <= id) {
                continue;
            }
            located.insert(uid, Located { uid, id, mailboxes });
        }
        Ok((located.into_values().collect(), state))
    }

    /// Applies the changes with `Email/set`, at most `maxObjectsInSet` per call, each call
    /// guarded by `state` when given. `Ok(None)` when the state no longer matched (earlier calls
    /// were applied); otherwise the ids the server no longer had.
    async fn apply(
        &self,
        changes: Vec<Change>,
        mut state: Option<String>,
    ) -> Result<Option<Vec<String>>, SourceError> {
        let mut missing = Vec::new();
        let mut changes = changes.into_iter().peekable();
        while changes.peek().is_some() {
            let mut update = Map::new();
            let mut destroy = Vec::new();
            for change in changes.by_ref().take(self.max_set) {
                match change {
                    Change::Update(id, patch) => {
                        update.insert(id, Value::Object(patch));
                    }
                    Change::Destroy(id) => destroy.push(id),
                }
            }
            let mut args = json!({ "accountId": self.account });
            if !update.is_empty() {
                args["update"] = Value::Object(update);
            }
            if !destroy.is_empty() {
                args["destroy"] = json!(destroy);
            }
            if let Some(state) = &state {
                args["ifInState"] = json!(state);
            }
            let answer = match self.answers(vec![("Email/set", args)]).await?.pop() {
                Some(Ok(answer)) => answer,
                Some(Err(kind)) if kind == "stateMismatch" => return Ok(None),
                Some(Err(kind)) => {
                    return Err(SourceError::Protocol(format!(
                        "the JMAP server refused a request: {kind}"
                    )));
                }
                None => return Err(protocol("the JMAP server left a call unanswered")),
            };
            missing.extend(failures(&answer, "notUpdated")?);
            missing.extend(failures(&answer, "notDestroyed")?);
            if state.is_some() {
                state = answer
                    .get("newState")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        Ok(Some(missing))
    }

    /// Locates the UIDs in `mailbox`, plans the changes and applies them guarded by the state
    /// they were planned at, planning again when the account changed in between.
    async fn guarded<F>(
        &self,
        mailbox: &str,
        uids: &[u64],
        mut plan: F,
    ) -> Result<Vec<u64>, SourceError>
    where
        F: FnMut(&Located) -> Change + Send,
    {
        for _ in 0..ATTEMPTS {
            let (located, state) = self.locate(mailbox, uids).await?;
            if located.is_empty() {
                return Ok(Vec::new());
            }
            let state = state.ok_or_else(|| protocol("the JMAP server gave no Email state"))?;
            let changes = located.iter().map(&mut plan).collect();
            if let Some(missing) = self.apply(changes, Some(state)).await? {
                return Ok(located
                    .into_iter()
                    .filter(|l| !missing.contains(&l.id))
                    .map(|l| l.uid)
                    .collect());
            }
        }
        Err(SourceError::Connection(
            "the JMAP account kept changing while it was being written to".into(),
        ))
    }
}

#[async_trait]
impl MailboxWriter for JmapWriter {
    fn capabilities(&self) -> MailboxWriteCaps {
        MailboxWriteCaps {
            flags: vec![Flag::Seen, Flag::Answered, Flag::Flagged, Flag::Draft],
            keywords: true,
            conditional: false,
            labels: false,
            move_uids: true,
            append_uid: true,
            mark_deleted: false,
        }
    }

    async fn set_flags(
        &self,
        folder: &str,
        uids: &[u64],
        add: &[Flag],
        remove: &[Flag],
        unchanged_since: Option<u64>,
    ) -> Result<FlagResult, SourceError> {
        if unchanged_since.is_some() {
            return Err(SourceError::Unsupported(
                "JMAP keeps no mod-sequences per message".into(),
            ));
        }
        let patch = keyword_patch(add, remove)?;
        let mut session = self.source.open().await?;
        let mailbox = session.mailbox_id(folder).await?;
        if uids.is_empty() {
            return Ok(FlagResult::default());
        }
        let (located, _) = session.locate(&mailbox, uids).await?;
        let changes = located
            .iter()
            .map(|l| Change::Update(l.id.clone(), patch.clone()))
            .collect();
        let missing = session.apply(changes, None).await?.unwrap_or_default();
        Ok(FlagResult {
            updated: located
                .into_iter()
                .filter(|l| !missing.contains(&l.id))
                .map(|l| l.uid)
                .collect(),
            modified: Vec::new(),
        })
    }

    async fn set_labels(
        &self,
        _uids: &[u64],
        _add: &[String],
        _remove: &[String],
    ) -> Result<(), SourceError> {
        Err(SourceError::Unsupported(
            "JMAP mailboxes are folders, not labels".into(),
        ))
    }

    async fn move_to(
        &self,
        folder: &str,
        uids: &[u64],
        target: &str,
    ) -> Result<Moved, SourceError> {
        let mut session = self.source.open().await?;
        let from = session.mailbox_id(folder).await?;
        let to = session.mailbox_id(target).await?;
        if uids.is_empty() {
            return Ok(Moved::default());
        }
        let moved = if from == to {
            session
                .locate(&from, uids)
                .await?
                .0
                .into_iter()
                .map(|l| l.uid)
                .collect()
        } else {
            session
                .guarded(&from, uids, |l| {
                    let mut patch = Map::new();
                    patch.insert(format!("mailboxIds/{from}"), Value::Null);
                    patch.insert(format!("mailboxIds/{to}"), json!(true));
                    Change::Update(l.id.clone(), patch)
                })
                .await?
        };
        Ok(Moved {
            uids: moved.into_iter().map(|uid| (uid, Some(uid))).collect(),
        })
    }

    async fn append(
        &self,
        folder: &str,
        raw: &[u8],
        flags: &[Flag],
        date: Timestamp,
    ) -> Result<Option<u64>, SourceError> {
        let keywords: Map<String, Value> = flags
            .iter()
            .map(|f| keyword(f).map(|k| (k, json!(true))))
            .collect::<Result<_, _>>()?;
        let mut session = self.source.open().await?;
        let mailbox = session.mailbox_id(folder).await?;
        let template = session
            .upload_url
            .clone()
            .ok_or_else(|| protocol("the JMAP session has no uploadUrl"))?;
        let url = expand(&template, &[("accountId", &session.account)]);
        let uploaded = session
            .http
            .post_bytes(&url, "message/rfc822", raw.to_vec())
            .await?;
        let blob = string(&uploaded, "blobId")?;
        let answer = session
            .call(vec![(
                "Email/import",
                json!({
                    "accountId": session.account,
                    "emails": {
                        "m": {
                            "blobId": blob,
                            "mailboxIds": { mailbox: true },
                            "keywords": keywords,
                            "receivedAt": time::format_utc(date),
                        }
                    },
                }),
            )])
            .await?
            .pop()
            .unwrap_or_default();
        let Some(id) = answer.pointer("/created/m/id").and_then(Value::as_str) else {
            let kind = answer
                .pointer("/notCreated/m/type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(SourceError::Protocol(format!(
                "the JMAP server refused to import the message: {kind}"
            )));
        };
        let id = id.to_owned();
        let email = session
            .emails(std::slice::from_ref(&id), &["id", "receivedAt"])
            .await?;
        let received_at = email
            .first()
            .and_then(received)
            .unwrap_or(Timestamp::from_secs(date.as_secs()));
        Ok(Some(uid(&id, Some(received_at))))
    }

    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError> {
        if !expunge {
            return Err(SourceError::Unsupported(
                "JMAP cannot mark mail deleted without removing it".into(),
            ));
        }
        let mut session = self.source.open().await?;
        let mailbox = session.mailbox_id(folder).await?;
        if uids.is_empty() {
            return Ok(());
        }
        session
            .guarded(&mailbox, uids, |l| {
                if l.mailboxes.iter().any(|m| *m != mailbox) {
                    let mut patch = Map::new();
                    patch.insert(format!("mailboxIds/{mailbox}"), Value::Null);
                    Change::Update(l.id.clone(), patch)
                } else {
                    Change::Destroy(l.id.clone())
                }
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_flags_as_jmap_keywords() {
        assert_eq!(keyword(&Flag::Seen).unwrap(), "$seen");
        assert_eq!(keyword(&Flag::Draft).unwrap(), "$draft");
        assert_eq!(
            keyword(&Flag::Keyword("$Forwarded".into())).unwrap(),
            "$forwarded"
        );
        for refused in ["\\Deleted", "", "two words", "a(b"] {
            assert!(matches!(
                keyword(&Flag::Keyword(refused.into())),
                Err(SourceError::Unsupported(_))
            ));
        }
        let patch = keyword_patch(&[Flag::Seen, Flag::Flagged], &[Flag::Seen]).unwrap();
        assert_eq!(patch["keywords/$seen"], Value::Null, "removing wins");
        assert_eq!(patch["keywords/$flagged"], json!(true));
    }

    #[test]
    fn groups_uids_into_received_windows() {
        let at = |secs: u64, hash: u64| (secs << crate::UID_HASH_BITS) | hash;
        assert_eq!(
            windows(&[at(100, 1), at(100, 2), at(200, 3), at(10_000, 4)]),
            [(100, 200), (10_000, 10_000)]
        );
        assert!(windows(&[]).is_empty());
    }
}
