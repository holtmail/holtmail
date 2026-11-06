//! A [`Source`] for Microsoft 365, Outlook.com and Hotmail mailboxes through Microsoft Graph.
//!
//! Mail folders, hidden ones included, are folders; search folders are left out because their
//! messages live in other folders. Graph has no UIDs, so each message gets one derived from its
//! `receivedDateTime` and its immutable id, as the JMAP source does: stable across sessions, but
//! not ascending with arrival, so folders report [`FolderStatus::uids_ascend`] `false`. A folder
//! is listed with a delta query, whose final delta link is the folder's
//! [`SourceSession::state`]; [`SourceSession::unchanged_since`] asks that link whether anything
//! changed. Each message's original MIME bytes come from its `$value`; Graph's JSON rendering of
//! a message is never used. `isRead`, `flag` and `isDraft` become `\Seen`, `\Flagged` and
//! `\Draft`.
//!
//! Throttling (`429`) and short outages (`503`, `504`) are waited out as `Retry-After` says.

mod http;
mod time;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use holtmail_source::{
    Credentials, FolderStatus, RemoteFlags, RemoteFolder, RemoteMessage, Source, SourceError,
    SourceSession,
};
use holtmail_store::FolderRole;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use http::{Answer, Http, Scope};

/// Where Microsoft Graph answers, for accounts in the global cloud.
pub const GRAPH_URL: &str = "https://graph.microsoft.com/v1.0";

const DOWNLOADS: usize = 4;
const LIST_FIELDS: &str = "id,receivedDateTime,isRead,flag,isDraft";
const FOLDER_FIELDS: &str = "id,displayName,childFolderCount,totalItemCount";
const MAX_FOLDERS: usize = 10_000;
const WELL_KNOWN: [(&str, FolderRole); 6] = [
    ("inbox", FolderRole::Inbox),
    ("sentitems", FolderRole::Sent),
    ("drafts", FolderRole::Drafts),
    ("deleteditems", FolderRole::Trash),
    ("junkemail", FolderRole::Junk),
    ("archive", FolderRole::Archive),
];

/// Account settings stored in `Account.settings`; the sign-in is an OAuth refresh token kept
/// separately.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSettings {
    /// The Graph endpoint with its version, [`GRAPH_URL`] when missing. Only `https`, or plain
    /// `http` to a loopback address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

impl GraphSettings {
    /// Reads settings from an account's JSON; other keys are ignored.
    pub fn from_json(value: &Value) -> Result<Self, SourceError> {
        serde_json::from_value(value.clone()).map_err(|e| SourceError::Config(e.to_string()))
    }

    fn base(&self) -> Result<Url, SourceError> {
        let raw = self.api_url.as_deref().unwrap_or(GRAPH_URL).trim();
        let mut url = Url::parse(raw)
            .map_err(|_| SourceError::Config("the Graph address is not a web address".into()))?;
        if !http::allowed(&url) {
            return Err(SourceError::Config(
                "the Graph address must use https".into(),
            ));
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        Ok(url)
    }
}

/// A Microsoft Graph mailbox, signed in as the owner of the access token.
pub struct GraphSource {
    base: Url,
    http: Http,
}

impl GraphSource {
    /// Also trusts `roots` for TLS, besides the built-in roots. `Config` when one of them is
    /// not a usable certificate.
    pub fn with_roots(mut self, roots: &holtmail_core::TlsRoots) -> Result<Self, SourceError> {
        self.http = self.http.with_roots(roots)?;
        Ok(self)
    }

    /// A source for the mailbox behind `credentials`, which must be an OAuth access token with
    /// `Mail.Read`. `Config` for a password or an unusable Graph address.
    pub fn new(settings: &GraphSettings, credentials: Credentials) -> Result<Self, SourceError> {
        let Credentials::AccessToken(token) = credentials else {
            return Err(SourceError::Config(
                "Microsoft Graph signs in with Microsoft, not with a password".into(),
            ));
        };
        let base = settings.base()?;
        Ok(Self {
            http: Http::new(token, &base, Duration::from_secs(300))?,
            base,
        })
    }

    /// The address of the signed-in mailbox: its primary SMTP address, else its user principal
    /// name. `None` when Graph names neither. Needs `User.Read`.
    pub async fn address(&self) -> Result<Option<String>, SourceError> {
        let url = endpoint(
            &self.base,
            &["me"],
            &[("$select", "mail,userPrincipalName")],
        )?;
        let Answer::Found(me) = self.http.get_json(url, Scope::Mailbox).await? else {
            return Ok(None);
        };
        Ok(["mail", "userPrincipalName"]
            .iter()
            .filter_map(|key| me.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .find(|a| a.contains('@'))
            .map(str::to_owned))
    }
}

#[async_trait]
impl Source for GraphSource {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError> {
        Ok(Box::new(GraphSession {
            base: self.base.clone(),
            http: self.http.clone(),
            folders: None,
            selected: None,
        }))
    }
}

fn endpoint(base: &Url, segments: &[&str], query: &[(&str, &str)]) -> Result<Url, SourceError> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| SourceError::Config("the Graph address cannot take a path".into()))?
        .pop_if_empty()
        .extend(segments);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url)
}

fn protocol(what: &str) -> SourceError {
    SourceError::Protocol(what.to_owned())
}

struct Folder {
    id: String,
    path: String,
    role: Option<FolderRole>,
    total: u64,
}

struct Entry {
    id: String,
    received_at: Option<holtmail_core::Timestamp>,
    flags: Vec<String>,
}

struct Selected {
    folder: String,
    listing: Option<BTreeMap<u64, Entry>>,
    state: Option<String>,
}

struct GraphSession {
    base: Url,
    http: Http,
    folders: Option<Vec<Folder>>,
    selected: Option<Selected>,
}

/// One page of a collection: its items, and the next page or the delta link.
struct Page {
    items: Vec<Value>,
    next: Option<String>,
    delta: Option<String>,
}

fn page(answer: &Value) -> Result<Page, SourceError> {
    let link = |key: &str| answer.get(key).and_then(Value::as_str).map(str::to_owned);
    Ok(Page {
        items: answer
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| protocol("a Graph answer has no value"))?,
        next: link("@odata.nextLink"),
        delta: link("@odata.deltaLink"),
    })
}

fn string(object: &Value, key: &str) -> Result<String, SourceError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| SourceError::Protocol(format!("a Graph object has no {key}")))
}

fn flags(message: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if message.get("isRead").and_then(Value::as_bool) == Some(true) {
        out.push("\\Seen".to_owned());
    }
    if message
        .pointer("/flag/flagStatus")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("flagged"))
    {
        out.push("\\Flagged".to_owned());
    }
    if message.get("isDraft").and_then(Value::as_bool) == Some(true) {
        out.push("\\Draft".to_owned());
    }
    out.sort();
    out
}

fn fnv(data: &str) -> u64 {
    data.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

const UID_HASH_BITS: u32 = 22;

/// The UID of a message: its received second above 22 bits of its id's hash. Stays below
/// `2^62`, so it fits a signed 64-bit column.
fn uid(id: &str, received_at: Option<holtmail_core::Timestamp>) -> u64 {
    let secs = received_at
        .map(|t| t.as_secs())
        .unwrap_or(0)
        .clamp(0, (1 << 40) - 1) as u64;
    (secs << UID_HASH_BITS) | (fnv(id) & ((1 << UID_HASH_BITS) - 1))
}

fn uid_validity(folder: &str) -> u64 {
    (fnv(folder) & 0xffff_ffff).max(1)
}

impl GraphSession {
    async fn json(&self, url: Url, scope: Scope) -> Result<Value, SourceError> {
        match self.http.get_json(url, scope).await? {
            Answer::Found(value) => Ok(value),
            Answer::Missing => Err(protocol("Microsoft Graph no longer has an item it listed")),
        }
    }

    /// Every item of a collection, following `@odata.nextLink`.
    async fn all(
        &self,
        first: Url,
        scope: Scope,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        let mut items = Vec::new();
        let mut url = first;
        loop {
            let page = page(&self.json(url, scope).await?)?;
            items.extend(page.items);
            match page.next {
                Some(next) => url = self.http.url(&next)?,
                None => return Ok((items, page.delta)),
            }
        }
    }

    async fn roles(&self) -> Result<HashMap<String, FolderRole>, SourceError> {
        let mut roles = HashMap::new();
        for (name, role) in WELL_KNOWN {
            let url = endpoint(
                &self.base,
                &["me", "mailFolders", name],
                &[("$select", "id")],
            )?;
            if let Answer::Found(folder) = self.http.get_json(url, Scope::Mailbox).await? {
                roles.insert(string(&folder, "id")?, role);
            }
        }
        Ok(roles)
    }

    async fn load_folders(&mut self) -> Result<&[Folder], SourceError> {
        if self.folders.is_none() {
            let roles = self.roles().await?;
            let query = [
                ("includeHiddenFolders", "true"),
                ("$top", "250"),
                ("$select", FOLDER_FIELDS),
            ];
            let mut folders = Vec::new();
            let mut queue = VecDeque::from([(
                endpoint(&self.base, &["me", "mailFolders"], &query)?,
                String::new(),
            )]);
            while let Some((url, parent)) = queue.pop_front() {
                let (items, _) = self.all(url, Scope::Mailbox).await?;
                for item in items {
                    if item
                        .get("@odata.type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.ends_with("mailSearchFolder"))
                    {
                        continue;
                    }
                    let id = string(&item, "id")?;
                    let name = string(&item, "displayName")?;
                    let path = if parent.is_empty() {
                        name
                    } else {
                        format!("{parent}/{name}")
                    };
                    if item
                        .get("childFolderCount")
                        .and_then(Value::as_u64)
                        .is_some_and(|n| n > 0)
                    {
                        queue.push_back((
                            endpoint(
                                &self.base,
                                &["me", "mailFolders", &id, "childFolders"],
                                &query,
                            )?,
                            path.clone(),
                        ));
                    }
                    folders.push(Folder {
                        role: roles.get(&id).copied(),
                        total: item
                            .get("totalItemCount")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        id,
                        path,
                    });
                    if folders.len() > MAX_FOLDERS {
                        return Err(protocol("the mailbox has too many folders"));
                    }
                }
            }
            folders.sort_by(|a, b| a.path.cmp(&b.path));
            self.folders = Some(folders);
        }
        Ok(self.folders.as_deref().unwrap_or_default())
    }

    fn selected(&self) -> Result<&Selected, SourceError> {
        self.selected
            .as_ref()
            .ok_or_else(|| protocol("no folder selected"))
    }

    async fn listing(&mut self) -> Result<&BTreeMap<u64, Entry>, SourceError> {
        if self.selected()?.listing.is_none() {
            let folder = self.selected()?.folder.clone();
            let url = endpoint(
                &self.base,
                &["me", "mailFolders", &folder, "messages", "delta"],
                &[("$select", LIST_FIELDS)],
            )?;
            let (items, delta) = self.all(url, Scope::Item).await?;
            let mut map: BTreeMap<u64, Entry> = BTreeMap::new();
            let mut clashes = 0u64;
            for item in items {
                if item.get("@removed").is_some() {
                    continue;
                }
                let id = string(&item, "id")?;
                let received_at = item
                    .get("receivedDateTime")
                    .and_then(Value::as_str)
                    .and_then(time::parse_utc);
                let entry = Entry {
                    flags: flags(&item),
                    received_at,
                    id,
                };
                let key = uid(&entry.id, received_at);
                match map.get(&key) {
                    Some(other) if other.id <= entry.id => clashes += 1,
                    Some(_) => {
                        clashes += 1;
                        map.insert(key, entry);
                    }
                    None => {
                        map.insert(key, entry);
                    }
                }
            }
            if clashes > 0 {
                tracing::warn!(
                    clashes,
                    "Graph messages share a derived UID; one of each is skipped"
                );
            }
            if let Some(selected) = self.selected.as_mut() {
                selected.listing = Some(map);
                selected.state = delta;
            }
        }
        self.selected()?
            .listing
            .as_ref()
            .ok_or_else(|| protocol("no folder selected"))
    }

    async fn download(&self, id: &str) -> Result<Option<Bytes>, SourceError> {
        let url = endpoint(&self.base, &["me", "messages", id, "$value"], &[])?;
        Ok(match self.http.get_bytes(url).await? {
            Answer::Found(raw) => Some(raw),
            Answer::Missing => None,
        })
    }
}

#[async_trait]
impl SourceSession for GraphSession {
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError> {
        Ok(self
            .load_folders()
            .await?
            .iter()
            .map(|f| RemoteFolder {
                path: f.path.clone(),
                role: f.role,
                selectable: true,
                label: false,
            })
            .collect())
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError> {
        let folder = self
            .load_folders()
            .await?
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| SourceError::Protocol(format!("there is no folder {path}")))?;
        let status = FolderStatus {
            uid_validity: uid_validity(&folder.id),
            uid_next: None,
            highest_modseq: None,
            exists: folder.total,
            uids_ascend: false,
        };
        self.selected = Some(Selected {
            folder: folder.id.clone(),
            listing: None,
            state: None,
        });
        Ok(status)
    }

    async fn uids(&mut self) -> Result<Vec<u64>, SourceError> {
        if let Some(selected) = self.selected.as_mut() {
            selected.listing = None;
            selected.state = None;
        }
        Ok(self.listing().await?.keys().copied().collect())
    }

    fn state(&self) -> Option<String> {
        self.selected.as_ref().and_then(|s| s.state.clone())
    }

    async fn unchanged_since(&mut self, state: &str) -> Result<Option<String>, SourceError> {
        self.selected()?;
        let Ok(url) = self.http.url(state) else {
            return Ok(None);
        };
        let answer = match self.http.get_json(url, Scope::Item).await {
            Ok(Answer::Found(answer)) => answer,
            Ok(Answer::Missing) => return Ok(None),
            Err(SourceError::Protocol(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let page = page(&answer)?;
        Ok(match (page.items.is_empty(), page.next, page.delta) {
            (true, None, Some(delta)) if self.http.url(&delta).is_ok() => Some(delta),
            _ => None,
        })
    }

    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError> {
        let listing = self.listing().await?;
        let wanted: Vec<(u64, String, Option<holtmail_core::Timestamp>, Vec<String>)> = uids
            .iter()
            .filter_map(|uid| {
                listing
                    .get(uid)
                    .map(|e| (*uid, e.id.clone(), e.received_at, e.flags.clone()))
            })
            .collect();
        let this = &*self;
        let mut out: Vec<RemoteMessage> = futures::stream::iter(wanted)
            .map(|(uid, id, received_at, flags)| async move {
                let Some(raw) = this.download(&id).await? else {
                    return Ok(None);
                };
                Ok::<_, SourceError>(Some(RemoteMessage {
                    uid,
                    flags,
                    labels: None,
                    server_id: None,
                    received_at,
                    raw,
                }))
            })
            .buffer_unordered(DOWNLOADS)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(Result::transpose)
            .collect::<Result<_, _>>()?;
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    async fn flags(
        &mut self,
        uids: RangeInclusive<u64>,
        changed_since: Option<u64>,
    ) -> Result<Vec<RemoteFlags>, SourceError> {
        if changed_since.is_some() {
            return Err(protocol("Graph folders have no mod-sequences"));
        }
        Ok(self
            .listing()
            .await?
            .range(uids)
            .map(|(uid, entry)| RemoteFlags {
                uid: *uid,
                flags: entry.flags.clone(),
                labels: None,
            })
            .collect())
    }

    async fn logout(&mut self) -> Result<(), SourceError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
