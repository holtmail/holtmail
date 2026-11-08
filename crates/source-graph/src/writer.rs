use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::StreamExt;
use holtmail_core::Timestamp;
use holtmail_source::{Flag, FlagResult, MailboxWriteCaps, MailboxWriter, Moved, SourceError};
use reqwest::Method;
use serde_json::{Value, json};
use url::Url;

use crate::http::{Answer, Body, Http, Scope};
use crate::{DOWNLOADS, GraphSession, UID_HASH_BITS, endpoint, protocol, string, time, uid};

pub(crate) const WRITE_FIELDS: &str = "id,receivedDateTime,isRead,flag";

/// The largest MIME upload Graph takes in one request, counted in base64.
const MAX_UPLOAD: usize = 4 * 1024 * 1024;

/// Changes a Microsoft Graph mailbox, from [`crate::GraphSource::writer`].
///
/// Graph has only `isRead` and `flag` of the IMAP flags, so it sets [`Flag::Seen`] and
/// [`Flag::Flagged`]; other flags, keywords, labels, `unchanged_since` and marking deleted are
/// `Unsupported` before any request. Messages are found by the UIDs the source derives, one
/// filtered request per received second. Each message is changed by a request of its own, so a
/// call that fails part way may have changed some of its messages; every message is changed
/// wholly or not at all, and calling again is harmless except for [`MailboxWriter::append`].
pub struct GraphWriter {
    pub(crate) base: Url,
    pub(crate) http: Http,
}

struct Found {
    id: String,
    received_at: Option<Timestamp>,
    read: bool,
    flagged: bool,
}

fn missing(path: &str) -> SourceError {
    SourceError::Missing(format!("there is no folder {path}"))
}

fn received(item: &Value) -> Option<Timestamp> {
    item.get("receivedDateTime")
        .and_then(Value::as_str)
        .and_then(time::parse_utc)
}

fn is_flagged(item: &Value) -> bool {
    item.pointer("/flag/flagStatus")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("flagged"))
}

fn flag_json(flagged: bool) -> Value {
    json!({ "flagStatus": if flagged { "flagged" } else { "notFlagged" } })
}

/// The state `flag` should end up in: `Some(false)` when removed (also when added too),
/// `Some(true)` when only added, `None` when untouched.
fn wanted(flag: &Flag, add: &[Flag], remove: &[Flag]) -> Option<bool> {
    if remove.contains(flag) {
        Some(false)
    } else if add.contains(flag) {
        Some(true)
    } else {
        None
    }
}

fn check(caps: &MailboxWriteCaps, flags: &[&[Flag]]) -> Result<(), SourceError> {
    match flags
        .iter()
        .flat_map(|f| f.iter())
        .find(|f| !caps.supports(f))
    {
        Some(flag) => Err(SourceError::Unsupported(format!(
            "Microsoft Graph has no flag {}",
            flag.as_imap()
        ))),
        None => Ok(()),
    }
}

impl GraphWriter {
    async fn folder_ids<const N: usize>(
        &self,
        paths: [&str; N],
    ) -> Result<[String; N], SourceError> {
        let mut session = GraphSession {
            base: self.base.clone(),
            http: self.http.clone(),
            folders: None,
            selected: None,
            roles: false,
        };
        let folders = session.load_folders().await?;
        let mut out = paths.map(|_| String::new());
        for (slot, path) in out.iter_mut().zip(paths) {
            *slot = folders
                .iter()
                .find(|f| f.path == path)
                .map(|f| f.id.clone())
                .ok_or_else(|| missing(path))?;
        }
        Ok(out)
    }

    async fn second(&self, folder: &str, secs: u64) -> Result<Vec<Value>, SourceError> {
        let secs = secs as i64;
        let filter = format!(
            "receivedDateTime ge {} and receivedDateTime lt {}",
            time::format_utc(secs),
            time::format_utc(secs + 1)
        );
        let mut url = endpoint(
            &self.base,
            &["me", "mailFolders", folder, "messages"],
            &[
                ("$filter", filter.as_str()),
                ("$select", WRITE_FIELDS),
                ("$top", "100"),
            ],
        )?;
        let mut items = Vec::new();
        loop {
            let Answer::Found(answer) = self.http.get_json(url, Scope::Item).await? else {
                return Err(SourceError::Missing("the folder is gone".into()));
            };
            let page = crate::page(&answer)?;
            items.extend(page.items);
            match page.next {
                Some(next) => url = self.http.url(&next)?,
                None => return Ok(items),
            }
        }
    }

    /// The messages of `folder` with these UIDs, keeping the source's rule for UIDs two
    /// messages share: the smaller id wins.
    async fn find(&self, folder: &str, uids: &[u64]) -> Result<BTreeMap<u64, Found>, SourceError> {
        let wanted: BTreeSet<u64> = uids.iter().copied().collect();
        let seconds: BTreeSet<u64> = wanted.iter().map(|u| u >> UID_HASH_BITS).collect();
        let pages: Vec<_> = futures::stream::iter(seconds)
            .map(|secs| self.second(folder, secs))
            .buffer_unordered(DOWNLOADS)
            .collect()
            .await;
        let mut out: BTreeMap<u64, Found> = BTreeMap::new();
        for items in pages {
            for item in items? {
                let id = string(&item, "id")?;
                let received_at = received(&item);
                if received_at.is_none() {
                    continue;
                }
                let key = uid(&id, received_at);
                if !wanted.contains(&key) || out.get(&key).is_some_and(|other| other.id <= id) {
                    continue;
                }
                out.insert(
                    key,
                    Found {
                        read: item.get("isRead").and_then(Value::as_bool) == Some(true),
                        flagged: is_flagged(&item),
                        received_at,
                        id,
                    },
                );
            }
        }
        Ok(out)
    }

    fn message(&self, id: &str, action: Option<&str>) -> Result<Url, SourceError> {
        match action {
            Some(action) => endpoint(&self.base, &["me", "messages", id, action], &[]),
            None => endpoint(&self.base, &["me", "messages", id], &[]),
        }
    }

    async fn purge(&self, id: &str) -> Result<(), SourceError> {
        let url = self.message(id, Some("permanentDelete"))?;
        self.http
            .write(Method::POST, url, Body::Empty, true)
            .await
            .map(drop)
    }
}

/// Runs `each` on every found message, a few at a time, and returns the UIDs it kept,
/// ascending.
async fn each<T, F, Fut>(found: BTreeMap<u64, Found>, each: F) -> Result<Vec<T>, SourceError>
where
    F: Fn(u64, Found) -> Fut,
    Fut: Future<Output = Result<Option<T>, SourceError>>,
    T: Ord,
{
    let results: Vec<_> = futures::stream::iter(found)
        .map(|(uid, found)| each(uid, found))
        .buffer_unordered(DOWNLOADS)
        .collect()
        .await;
    let mut out = Vec::new();
    for result in results {
        out.extend(result?);
    }
    out.sort();
    Ok(out)
}

#[async_trait]
impl MailboxWriter for GraphWriter {
    fn capabilities(&self) -> MailboxWriteCaps {
        MailboxWriteCaps {
            flags: vec![Flag::Seen, Flag::Flagged],
            keywords: false,
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
        check(&self.capabilities(), &[add, remove])?;
        if unchanged_since.is_some() {
            return Err(SourceError::Unsupported(
                "Microsoft Graph has no mod-sequences".into(),
            ));
        }
        let [folder] = self.folder_ids([folder]).await?;
        let read = wanted(&Flag::Seen, add, remove);
        let flagged = wanted(&Flag::Flagged, add, remove);
        let found = self.find(&folder, uids).await?;
        let updated = each(found, |uid, found| async move {
            let mut patch = serde_json::Map::new();
            if let Some(read) = read.filter(|r| *r != found.read) {
                patch.insert("isRead".into(), json!(read));
            }
            if let Some(flagged) = flagged.filter(|f| *f != found.flagged) {
                patch.insert("flag".into(), flag_json(flagged));
            }
            if patch.is_empty() {
                return Ok(Some(uid));
            }
            let url = self.message(&found.id, None)?;
            Ok(
                match self
                    .http
                    .write(Method::PATCH, url, Body::Json(Value::Object(patch)), true)
                    .await?
                {
                    Answer::Found(_) => Some(uid),
                    Answer::Missing => None,
                },
            )
        })
        .await?;
        Ok(FlagResult {
            updated,
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
            "Microsoft Graph has no labels".into(),
        ))
    }

    async fn move_to(
        &self,
        folder: &str,
        uids: &[u64],
        target: &str,
    ) -> Result<Moved, SourceError> {
        let [folder, target] = self.folder_ids([folder, target]).await?;
        let found = self.find(&folder, uids).await?;
        let target = &target;
        let uids = each(found, |uid, found| async move {
            let url = self.message(&found.id, Some("move"))?;
            let body = Body::Json(json!({ "destinationId": target }));
            match self.http.write(Method::POST, url, body, true).await? {
                Answer::Missing => Ok(None),
                Answer::Found(None) => Err(protocol("Graph did not return the moved message")),
                Answer::Found(Some(moved)) => {
                    let id = string(&moved, "id")?;
                    let received_at = received(&moved).or(found.received_at);
                    Ok(Some((uid, Some(crate::uid(&id, received_at)))))
                }
            }
        })
        .await?;
        Ok(Moved { uids })
    }

    async fn append(
        &self,
        folder: &str,
        raw: &[u8],
        flags: &[Flag],
        _date: Timestamp,
    ) -> Result<Option<u64>, SourceError> {
        check(&self.capabilities(), &[flags])?;
        let encoded = BASE64.encode(raw);
        if encoded.len() > MAX_UPLOAD {
            return Err(SourceError::Unsupported(
                "Microsoft Graph takes messages of at most 3 MiB".into(),
            ));
        }
        let path = folder;
        let [folder] = self.folder_ids([folder]).await?;
        let url = endpoint(&self.base, &["me", "mailFolders", &folder, "messages"], &[])?;
        let created = match self
            .http
            .write(Method::POST, url, Body::Mime(encoded), false)
            .await?
        {
            Answer::Missing => return Err(missing(path)),
            Answer::Found(None) => return Err(protocol("Graph did not return the new message")),
            Answer::Found(Some(created)) => created,
        };
        let id = string(&created, "id")?;
        let patch = json!({
            "isRead": flags.contains(&Flag::Seen),
            "flag": flag_json(flags.contains(&Flag::Flagged)),
        });
        let patched = match self
            .http
            .write(
                Method::PATCH,
                self.message(&id, None)?,
                Body::Json(patch),
                true,
            )
            .await
        {
            Ok(Answer::Found(patched)) => patched,
            Ok(Answer::Missing) => {
                return Err(protocol("Graph lost the message it just created"));
            }
            Err(error) => {
                if self.purge(&id).await.is_err() {
                    tracing::warn!("an uploaded message without its flags could not be removed");
                }
                return Err(error);
            }
        };
        let received_at = received(&created).or_else(|| patched.as_ref().and_then(received));
        Ok(received_at.map(|at| uid(&id, Some(at))))
    }

    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError> {
        if !expunge {
            return Err(SourceError::Unsupported(
                "Microsoft Graph cannot mark messages deleted; move them to Deleted Items".into(),
            ));
        }
        let [folder] = self.folder_ids([folder]).await?;
        let found = self.find(&folder, uids).await?;
        each(found, |_, found| async move {
            self.purge(&found.id).await.map(|()| None::<u64>)
        })
        .await
        .map(drop)
    }
}
