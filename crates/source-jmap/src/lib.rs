//! A [`Source`] for JMAP servers (RFC 8620, RFC 8621).
//!
//! Mailboxes are folders. An email in several mailboxes is a message in each of those folders,
//! as an IMAP copy is. JMAP has no UIDs, so each email gets one derived from its `receivedAt` and
//! its id: the same in every mailbox and every session, ascending with the time the server
//! received it, but not with the time it entered a mailbox, so folders report
//! [`FolderStatus::uids_ascend`] `false`. Each message's original bytes are downloaded through
//! the session's `downloadUrl`; keywords become flags.

mod http;
mod time;

use std::collections::{BTreeMap, HashMap};
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
use serde_json::{Value, json};
use url::Url;

use http::Http;

const CORE: &str = "urn:ietf:params:jmap:core";
const MAIL: &str = "urn:ietf:params:jmap:mail";
const MAX_OBJECTS: usize = 500;
const MAX_CALLS: usize = 16;
const DOWNLOADS: usize = 4;

/// How the account signs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JmapAuth {
    /// HTTP Basic with the user name and a password or app password.
    #[default]
    Password,
    /// A bearer token, such as a Fastmail API token; the user name is not sent.
    Token,
}

/// Account settings stored in `Account.settings`; the password or token is sealed separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JmapSettings {
    /// The session resource, e.g. `https://api.fastmail.com/jmap/session`, or a server's address,
    /// where `/.well-known/jmap` is asked; see [`session_url`].
    pub url: String,
    /// User name for [`JmapAuth::Password`], often the address; may be empty for a token.
    #[serde(default)]
    pub username: String,
    /// How it signs in.
    #[serde(default)]
    pub auth: JmapAuth,
}

impl JmapSettings {
    /// Reads settings from an account's JSON.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, SourceError> {
        serde_json::from_value(value.clone()).map_err(|e| SourceError::Config(e.to_string()))
    }
}

/// The session resource for what someone typed: `https://` is added when no scheme is given,
/// and an address without a path gets `/.well-known/jmap`. Only `https` is accepted, except
/// plain `http` to a loopback address. `Config` for anything else.
pub fn session_url(input: &str) -> Result<String, SourceError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SourceError::Config("the JMAP address is empty".into()));
    }
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|_| SourceError::Config("the JMAP address is not a web address".into()))?;
    if !http::allowed(&url) {
        return Err(SourceError::Config(
            "the JMAP address must use https".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SourceError::Config(
            "put the user name and password in their own fields, not in the address".into(),
        ));
    }
    url.set_fragment(None);
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/.well-known/jmap");
    }
    Ok(url.to_string())
}

/// A JMAP account.
pub struct JmapSource {
    settings: JmapSettings,
    http: Http,
}

impl JmapSource {
    /// Also trusts `roots` for TLS, besides the built-in roots. `Config` when one of them is
    /// not a usable certificate.
    pub fn with_roots(mut self, roots: &holtmail_core::TlsRoots) -> Result<Self, SourceError> {
        self.http = self.http.with_roots(roots)?;
        Ok(self)
    }

    /// A source for the account in `settings`. A password signs in with HTTP Basic as the user
    /// name in `settings`, an access token as a bearer token. `Config` when the address is not
    /// usable or a password comes without a user name.
    pub fn new(settings: JmapSettings, credentials: Credentials) -> Result<Self, SourceError> {
        session_url(&settings.url)?;
        let auth = match credentials {
            Credentials::Password(password) => {
                if settings.username.trim().is_empty() {
                    return Err(SourceError::Config(
                        "signing in with a password needs a user name".into(),
                    ));
                }
                http::Auth::Basic(settings.username.trim().to_owned(), password)
            }
            Credentials::AccessToken(token) => http::Auth::Bearer(token),
        };
        Ok(Self {
            http: Http::new(auth, Duration::from_secs(300))?,
            settings,
        })
    }
}

#[async_trait]
impl Source for JmapSource {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError> {
        let url = session_url(&self.settings.url)?;
        let mut base = Url::parse(&url).map_err(|e| SourceError::Config(e.to_string()))?;
        if base.path().starts_with("/.well-known/") {
            base = self.http.resolve(base).await?;
        }
        let session = self.http.session(&base).await?;
        Ok(Box::new(JmapSession::from_session(
            self.http.clone(),
            &base,
            &session,
        )?))
    }
}

struct Mailbox {
    id: String,
    path: String,
    role: Option<FolderRole>,
    total: u64,
}

struct Selected {
    mailbox: String,
    uids: Option<BTreeMap<u64, String>>,
}

struct JmapSession {
    http: Http,
    api_url: String,
    download_url: String,
    account: String,
    max_objects: usize,
    max_calls: usize,
    mailboxes: Option<Vec<Mailbox>>,
    selected: Option<Selected>,
}

fn protocol(what: &str) -> SourceError {
    SourceError::Protocol(what.to_owned())
}

fn limit(core: &Value, key: &str, default: usize, max: usize) -> usize {
    core.get(key)
        .and_then(Value::as_u64)
        .map_or(default, |v| usize::try_from(v).unwrap_or(max))
        .clamp(1, max)
}

impl JmapSession {
    fn from_session(http: Http, base: &Url, session: &Value) -> Result<Self, SourceError> {
        let capabilities = session
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol("the address does not answer with a JMAP session"))?;
        if !capabilities.contains_key(MAIL) {
            return Err(SourceError::Config(
                "the JMAP server does not offer mail".into(),
            ));
        }
        let core = capabilities.get(CORE).cloned().unwrap_or(Value::Null);
        let account = session
            .pointer(&format!("/primaryAccounts/{}", MAIL.replace('/', "~1")))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SourceError::Config("the JMAP session has no mail account for this user".into())
            })?
            .to_owned();
        let resolve = |key: &str| -> Result<String, SourceError> {
            let raw = session
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| protocol(&format!("the JMAP session has no {key}")))?;
            if raw.contains('{') {
                return Ok(if raw.contains("://") {
                    raw.to_owned()
                } else {
                    format!("{}{}", base.origin().ascii_serialization(), raw)
                });
            }
            base.join(raw)
                .map(|u| u.to_string())
                .map_err(|_| protocol(&format!("the JMAP session's {key} is not an address")))
        };
        Ok(Self {
            api_url: resolve("apiUrl")?,
            download_url: resolve("downloadUrl")?,
            http,
            account,
            max_objects: limit(&core, "maxObjectsInGet", MAX_OBJECTS, MAX_OBJECTS),
            max_calls: limit(&core, "maxCallsInRequest", MAX_CALLS, MAX_CALLS),
            mailboxes: None,
            selected: None,
        })
    }

    /// Sends the calls in one request and returns their answers in order.
    async fn call(&self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>, SourceError> {
        let count = calls.len();
        let body = json!({
            "using": [CORE, MAIL],
            "methodCalls": calls
                .into_iter()
                .enumerate()
                .map(|(i, (name, args))| json!([name, args, i.to_string()]))
                .collect::<Vec<_>>(),
        });
        let answer = self.http.post_json(&self.api_url, &body).await?;
        let responses = answer
            .get("methodResponses")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol("the JMAP answer has no method responses"))?;
        let mut out = vec![None; count];
        for response in responses {
            let Some([name, args, id]) = response.as_array().map(Vec::as_slice) else {
                return Err(protocol("a JMAP method response is malformed"));
            };
            let Some(index) = id.as_str().and_then(|id| id.parse::<usize>().ok()) else {
                continue;
            };
            if name == "error" {
                let kind = args
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Err(SourceError::Protocol(format!(
                    "the JMAP server refused a request: {kind}"
                )));
            }
            if let Some(slot) = out.get_mut(index) {
                *slot = Some(args.clone());
            }
        }
        out.into_iter()
            .map(|args| args.ok_or_else(|| protocol("the JMAP server left a call unanswered")))
            .collect()
    }

    /// `Email/get` for `ids`, at most `maxObjectsInGet` per call and `maxCallsInRequest` calls
    /// per request. Ids the server no longer has are left out.
    async fn emails(&self, ids: &[String], properties: &[&str]) -> Result<Vec<Value>, SourceError> {
        let mut out = Vec::with_capacity(ids.len());
        for request in ids.chunks(self.max_objects * self.max_calls) {
            let calls = request
                .chunks(self.max_objects)
                .map(|chunk| {
                    (
                        "Email/get",
                        json!({ "accountId": self.account, "ids": chunk, "properties": properties }),
                    )
                })
                .collect();
            for answer in self.call(calls).await? {
                out.extend(list(&answer)?);
            }
        }
        Ok(out)
    }

    async fn load_mailboxes(&mut self) -> Result<&[Mailbox], SourceError> {
        if self.mailboxes.is_none() {
            let answer = self
                .call(vec![(
                    "Mailbox/get",
                    json!({
                        "accountId": self.account,
                        "ids": null,
                        "properties": ["id", "name", "parentId", "role", "totalEmails"],
                    }),
                )])
                .await?;
            let raw = list(&answer[0])?;
            let mut by_id = HashMap::new();
            for mailbox in &raw {
                let id = string(mailbox, "id")?;
                by_id.insert(
                    id,
                    (
                        string(mailbox, "name")?,
                        mailbox
                            .get("parentId")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    ),
                );
            }
            let mut mailboxes = Vec::with_capacity(raw.len());
            for mailbox in &raw {
                let id = string(mailbox, "id")?;
                mailboxes.push(Mailbox {
                    path: path(&id, &by_id)?,
                    role: mailbox
                        .get("role")
                        .and_then(Value::as_str)
                        .and_then(|r| FolderRole::parse(&r.to_ascii_lowercase())),
                    total: mailbox
                        .get("totalEmails")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    id,
                });
            }
            mailboxes.sort_by(|a, b| a.path.cmp(&b.path));
            self.mailboxes = Some(mailboxes);
        }
        Ok(self.mailboxes.as_deref().unwrap_or_default())
    }

    fn selected(&self) -> Result<&Selected, SourceError> {
        self.selected
            .as_ref()
            .ok_or_else(|| protocol("no folder selected"))
    }

    async fn uid_map(&mut self) -> Result<&BTreeMap<u64, String>, SourceError> {
        if self.selected()?.uids.is_none() {
            let mailbox = self.selected()?.mailbox.clone();
            let map = self.list_mailbox(&mailbox).await?;
            if let Some(selected) = self.selected.as_mut() {
                selected.uids = Some(map);
            }
        }
        self.selected()?
            .uids
            .as_ref()
            .ok_or_else(|| protocol("no folder selected"))
    }

    async fn list_mailbox(&self, mailbox: &str) -> Result<BTreeMap<u64, String>, SourceError> {
        let mut position = 0usize;
        let mut emails = Vec::new();
        loop {
            let query = json!({
                "accountId": self.account,
                "filter": { "inMailbox": mailbox },
                "sort": [{ "property": "receivedAt", "isAscending": true }],
                "position": position,
                "limit": self.max_objects,
                "calculateTotal": true,
            });
            let (answer, page) = if self.max_calls >= 2 {
                let mut answers = self
                    .call(vec![
                        ("Email/query", query),
                        (
                            "Email/get",
                            json!({
                                "accountId": self.account,
                                "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                                "properties": ["id", "receivedAt"],
                            }),
                        ),
                    ])
                    .await?
                    .into_iter();
                let answer = answers.next().unwrap_or_default();
                let page = list(&answers.next().unwrap_or_default())?;
                (answer, page)
            } else {
                let answer = self
                    .call(vec![("Email/query", query)])
                    .await?
                    .pop()
                    .unwrap_or_default();
                let ids = ids(&answer)?;
                let page = self.emails(&ids, &["id", "receivedAt"]).await?;
                (answer, page)
            };
            let found = ids(&answer)?.len();
            emails.extend(page);
            position += found;
            let total = answer
                .get("total")
                .and_then(Value::as_u64)
                .and_then(|t| usize::try_from(t).ok());
            if found == 0 || total.is_some_and(|total| position >= total) {
                break;
            }
        }
        let mut map: BTreeMap<u64, String> = BTreeMap::new();
        let mut clashes = 0u64;
        for email in emails {
            let id = string(&email, "id")?;
            let uid = uid(&id, received(&email));
            match map.get(&uid) {
                Some(other) if *other <= id => clashes += 1,
                Some(_) => {
                    clashes += 1;
                    map.insert(uid, id);
                }
                None => {
                    map.insert(uid, id);
                }
            }
        }
        if clashes > 0 {
            tracing::warn!(
                clashes,
                "JMAP emails share a derived UID; one of each is skipped"
            );
        }
        Ok(map)
    }

    async fn download(&self, blob: &str) -> Result<Option<Bytes>, SourceError> {
        let url = expand(
            &self.download_url,
            &[
                ("accountId", &self.account),
                ("blobId", blob),
                ("type", "message/rfc822"),
                ("name", "message.eml"),
            ],
        );
        self.http.get_bytes(&url).await
    }
}

fn list(answer: &Value) -> Result<Vec<Value>, SourceError> {
    answer
        .get("list")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| protocol("a JMAP get answer has no list"))
}

fn ids(answer: &Value) -> Result<Vec<String>, SourceError> {
    answer
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol("a JMAP query answer has no ids"))?
        .iter()
        .map(|id| {
            id.as_str()
                .map(str::to_owned)
                .ok_or_else(|| protocol("a JMAP id is not a string"))
        })
        .collect()
}

fn string(object: &Value, key: &str) -> Result<String, SourceError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| SourceError::Protocol(format!("a JMAP object has no {key}")))
}

fn received(email: &Value) -> Option<holtmail_core::Timestamp> {
    email
        .get("receivedAt")
        .and_then(Value::as_str)
        .and_then(time::parse_utc)
}

fn path(
    id: &str,
    by_id: &HashMap<String, (String, Option<String>)>,
) -> Result<String, SourceError> {
    let mut names = Vec::new();
    let mut next = Some(id.to_owned());
    while let Some(current) = next {
        if names.len() > by_id.len() {
            return Err(protocol("the JMAP mailboxes form a loop"));
        }
        let Some((name, parent)) = by_id.get(&current) else {
            break;
        };
        names.push(name.clone());
        next = parent.clone();
    }
    names.reverse();
    Ok(names.join("/"))
}

fn fnv(data: &str) -> u64 {
    data.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

const UID_HASH_BITS: u32 = 22;

/// The UID of an email: its received second above 22 bits of its id's hash. Stays below
/// `2^62`, so it fits a signed 64-bit column.
fn uid(id: &str, received_at: Option<holtmail_core::Timestamp>) -> u64 {
    let secs = received_at
        .map(|t| t.as_secs())
        .unwrap_or(0)
        .clamp(0, (1 << 40) - 1) as u64;
    (secs << UID_HASH_BITS) | (fnv(id) & ((1 << UID_HASH_BITS) - 1))
}

fn uid_validity(mailbox: &str) -> u64 {
    (fnv(mailbox) & 0xffff_ffff).max(1)
}

/// Fills a level-1 URI template (RFC 6570), percent-encoding each value.
fn expand(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in values {
        let encoded: String = url::form_urlencoded::byte_serialize(value.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        out = out.replace(&format!("{{{name}}}"), &encoded);
    }
    out
}

/// IMAP's names for the keywords both protocols share; others stay as JMAP writes them.
fn flag(keyword: &str) -> String {
    match keyword.to_ascii_lowercase().as_str() {
        "$seen" => "\\Seen".into(),
        "$flagged" => "\\Flagged".into(),
        "$answered" => "\\Answered".into(),
        "$draft" => "\\Draft".into(),
        _ => keyword.to_owned(),
    }
}

fn flags(email: &Value) -> Vec<String> {
    let mut out: Vec<String> = email
        .get("keywords")
        .and_then(Value::as_object)
        .map(|keywords| {
            keywords
                .iter()
                .filter(|(_, set)| set.as_bool().unwrap_or(false))
                .map(|(keyword, _)| flag(keyword))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[async_trait]
impl SourceSession for JmapSession {
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError> {
        Ok(self
            .load_mailboxes()
            .await?
            .iter()
            .map(|m| RemoteFolder {
                path: m.path.clone(),
                role: m.role,
                selectable: true,
                label: false,
            })
            .collect())
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError> {
        let mailbox = self
            .load_mailboxes()
            .await?
            .iter()
            .find(|m| m.path == path)
            .ok_or_else(|| SourceError::Protocol(format!("there is no mailbox {path}")))?;
        let status = FolderStatus {
            uid_validity: uid_validity(&mailbox.id),
            uid_next: None,
            highest_modseq: None,
            exists: mailbox.total,
            uids_ascend: false,
        };
        self.selected = Some(Selected {
            mailbox: mailbox.id.clone(),
            uids: None,
        });
        Ok(status)
    }

    async fn uids(&mut self) -> Result<Vec<u64>, SourceError> {
        if let Some(selected) = self.selected.as_mut() {
            selected.uids = None;
        }
        Ok(self.uid_map().await?.keys().copied().collect())
    }

    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError> {
        let map = self.uid_map().await?;
        let by_id: HashMap<String, u64> = uids
            .iter()
            .filter_map(|uid| map.get(uid).map(|id| (id.clone(), *uid)))
            .collect();
        let ids: Vec<String> = by_id.keys().cloned().collect();
        let emails = self
            .emails(&ids, &["id", "blobId", "keywords", "receivedAt"])
            .await?;
        let this = &*self;
        let mut out: Vec<RemoteMessage> = futures::stream::iter(emails)
            .map(|email| {
                let by_id = &by_id;
                async move {
                    let id = string(&email, "id")?;
                    let Some(uid) = by_id.get(&id).copied() else {
                        return Ok(None);
                    };
                    let Some(raw) = this.download(&string(&email, "blobId")?).await? else {
                        return Ok(None);
                    };
                    Ok::<_, SourceError>(Some(RemoteMessage {
                        uid,
                        flags: flags(&email),
                        labels: None,
                        server_id: None,
                        received_at: received(&email),
                        raw,
                    }))
                }
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
            return Err(protocol("JMAP folders have no mod-sequences"));
        }
        let map = self.uid_map().await?;
        let by_id: HashMap<String, u64> = map
            .range(uids)
            .map(|(uid, id)| (id.clone(), *uid))
            .collect();
        let ids: Vec<String> = by_id.keys().cloned().collect();
        let mut out = Vec::with_capacity(ids.len());
        for email in self.emails(&ids, &["id", "keywords"]).await? {
            if let Some(uid) = by_id.get(&string(&email, "id")?) {
                out.push(RemoteFlags {
                    uid: *uid,
                    flags: flags(&email),
                    labels: None,
                });
            }
        }
        out.sort_by_key(|f| f.uid);
        Ok(out)
    }

    async fn logout(&mut self) -> Result<(), SourceError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
