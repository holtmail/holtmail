//! Reading address books and calendars from a JMAP server behind the [`CollectionSource`]
//! contract: JMAP for Contacts (RFC 9610) and, when the session offers it, JMAP Calendars
//! (draft-ietf-jmap-calendars).
//!
//! Address books are `AddressBook`s and calendars `Calendar`s of the session's primary accounts
//! for `urn:ietf:params:jmap:contacts` and `urn:ietf:params:jmap:calendars`. Each card is stored
//! as the JSContact object (`application/jscontact+json`) and each event as the JSCalendar object
//! (`application/jscalendar+json`) exactly as they appear in the server's `/get` answer.
//! Changes come from `ContactCard/changes` and `CalendarEvent/changes`. Reading calls only
//! `/get`, `/query` and `/changes` methods; the same type is also a [`CollectionWriter`]
//! (`ContactCard/set`, `CalendarEvent/set`), which the server hands out only for accounts
//! allowed to write. Push comes from the session's `eventSourceUrl` (RFC 8620 §7.3).
//!
//! [`CollectionWriter`]: holtmail_collection::CollectionWriter

mod http;
mod write;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
pub use holtmail_collection::Credentials;
use holtmail_collection::{
    Changes, CollectionError, CollectionKind, CollectionSource, CollectionWatch,
    CollectionsChanged, ItemTag, RemoteCollection, RemoteItem, media,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use url::Url;

use http::Http;

/// The JMAP core capability.
pub const CORE: &str = "urn:ietf:params:jmap:core";
/// JMAP for Contacts (RFC 9610).
pub const CONTACTS: &str = "urn:ietf:params:jmap:contacts";
/// JMAP Calendars (draft-ietf-jmap-calendars).
pub const CALENDARS: &str = "urn:ietf:params:jmap:calendars";

const MAX_OBJECTS: usize = 500;
const MAX_CHANGES: u64 = 1_000;
const QUERY_PAGE: usize = 1_000;
const MAX_ROUNDS: usize = 10_000;
const MAX_ITEMS: usize = 1_000_000;

/// One kind of collection and how JMAP names its parts.
struct Kind {
    kind: CollectionKind,
    capability: &'static str,
    collection: &'static str,
    item: &'static str,
    member: &'static str,
    filter: &'static str,
    prefix: &'static str,
    media: &'static str,
    properties: &'static [&'static str],
}

const BOOKS: Kind = Kind {
    kind: CollectionKind::Contacts,
    capability: CONTACTS,
    collection: "AddressBook",
    item: "ContactCard",
    member: "addressBookIds",
    filter: "inAddressBook",
    prefix: "addressbook/",
    media: media::JSCONTACT,
    properties: &["id", "name"],
};

const CALENDARS_KIND: Kind = Kind {
    kind: CollectionKind::Calendar,
    capability: CALENDARS,
    collection: "Calendar",
    item: "CalendarEvent",
    member: "calendarIds",
    filter: "inCalendar",
    prefix: "calendar/",
    media: media::JSCALENDAR,
    properties: &["id", "name", "color"],
};

fn split(collection: &str) -> Result<(&'static Kind, &str), CollectionError> {
    for kind in [&BOOKS, &CALENDARS_KIND] {
        if let Some(id) = collection.strip_prefix(kind.prefix) {
            return Ok((kind, id));
        }
    }
    Err(CollectionError::Gone)
}

fn protocol(what: &str) -> CollectionError {
    CollectionError::Protocol(what.to_owned())
}

#[derive(Clone)]
struct Session {
    api: Url,
    contacts: Option<String>,
    calendars: Option<String>,
    max_objects: usize,
    events: Option<String>,
    username: Option<String>,
}

impl Session {
    fn account(&self, kind: &Kind) -> Option<&str> {
        match kind.kind {
            CollectionKind::Contacts => self.contacts.as_deref(),
            CollectionKind::Calendar => self.calendars.as_deref(),
        }
    }
}

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(borrow, rename = "methodResponses")]
    responses: Vec<(String, &'a RawValue, String)>,
}

#[derive(Deserialize)]
struct GetList<'a> {
    #[serde(borrow, default)]
    list: Vec<&'a RawValue>,
    #[serde(default)]
    state: String,
}

/// A JMAP account's contacts and calendars, signed in with the account's password (HTTP Basic)
/// or token (bearer).
///
/// A session address ending in `/.well-known/jmap` is followed through its redirects without
/// credentials first; the credentials then go to where it led and to the API address the
/// session names, on any host, but only over https (plain http only to a loopback address).
/// Redirects are not followed with credentials. Answers are capped
/// at 1 MiB for the session and 64 MiB for one API answer; `429`, `503` and `504` are waited out
/// as `Retry-After` says, two minutes at most, five tries at most.
pub struct JmapCollections {
    url: Url,
    http: Http,
    session: tokio::sync::Mutex<Option<Session>>,
    ping: Duration,
}

/// How often the event source is asked to ping, unless [`JmapCollections::with_push_ping`]
/// says otherwise.
const PING: Duration = Duration::from_secs(60);

impl JmapCollections {
    /// Also trusts `roots` for TLS, besides the built-in roots. `Config` when one of them is
    /// not a usable certificate.
    pub fn with_roots(mut self, roots: &holtmail_core::TlsRoots) -> Result<Self, CollectionError> {
        self.http = self.http.with_roots(roots)?;
        Ok(self)
    }

    /// A source for the session resource at `session_url`. `Config` for an address that is not
    /// https (plain http only to a loopback address) or carries a user name.
    pub fn new(
        session_url: &str,
        credentials: Credentials,
        timeout: Duration,
    ) -> Result<Self, CollectionError> {
        let url = Url::parse(session_url.trim())
            .map_err(|_| CollectionError::Config("the JMAP address is not a web address".into()))?;
        if !http::allowed(&url) || url.host_str().is_none() {
            return Err(CollectionError::Config(
                "the JMAP address must use https".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(CollectionError::Config(
                "the JMAP address must not hold a user name or password".into(),
            ));
        }
        Ok(Self {
            url,
            http: Http::new(credentials, timeout)?,
            session: tokio::sync::Mutex::new(None),
            ping: PING,
        })
    }

    /// Asks the event source for a ping every `ping` instead of every minute; a watch that hears
    /// nothing for two and a half pings ends.
    pub fn with_push_ping(mut self, ping: Duration) -> Self {
        self.ping = ping.max(Duration::from_secs(1));
        self
    }

    /// Whether the session offers contacts (`urn:ietf:params:jmap:contacts`) for this user.
    pub async fn offers_contacts(&self) -> Result<bool, CollectionError> {
        Ok(self.session().await?.contacts.is_some())
    }

    /// Whether the session offers calendars (`urn:ietf:params:jmap:calendars`) for this user.
    pub async fn offers_calendars(&self) -> Result<bool, CollectionError> {
        Ok(self.session().await?.calendars.is_some())
    }

    async fn session(&self) -> Result<Session, CollectionError> {
        let mut cached = self.session.lock().await;
        if let Some(session) = cached.as_ref() {
            return Ok(session.clone());
        }
        let url = if self.url.path().starts_with("/.well-known/") {
            self.http.resolve(self.url.clone()).await?
        } else {
            self.url.clone()
        };
        let body = self.http.request(&url, None, http::MAX_SESSION).await?;
        let session: Value = serde_json::from_slice(&body)
            .map_err(|_| protocol("the address does not answer with a JMAP session"))?;
        let capabilities = session
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| protocol("the address does not answer with a JMAP session"))?;
        let primary = |capability: &str| {
            capabilities.contains_key(capability).then_some(())?;
            session
                .get("primaryAccounts")?
                .get(capability)?
                .as_str()
                .map(str::to_owned)
        };
        let api = session
            .get("apiUrl")
            .and_then(Value::as_str)
            .and_then(|a| url.join(a).ok())
            .ok_or_else(|| protocol("the JMAP session has no API address"))?;
        if !http::allowed(&api) {
            return Err(protocol(
                "the JMAP session names an API address without https",
            ));
        }
        let max_objects = capabilities
            .get(CORE)
            .and_then(|c| c.get("maxObjectsInGet"))
            .and_then(Value::as_u64)
            .map_or(MAX_OBJECTS, |n| usize::try_from(n).unwrap_or(MAX_OBJECTS))
            .clamp(1, MAX_OBJECTS);
        let events = match session.get("eventSourceUrl").and_then(Value::as_str) {
            Some(raw) if raw.contains("://") => Some(raw.to_owned()),
            Some(raw) => Some(format!("{}{raw}", url.origin().ascii_serialization())),
            None => None,
        };
        let found = Session {
            api,
            contacts: primary(CONTACTS),
            calendars: primary(CALENDARS),
            max_objects,
            events,
            username: session
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        *cached = Some(found.clone());
        Ok(found)
    }

    /// Sends `calls` in one request and gives each answer's arguments, in order; the first
    /// method error fails the whole request.
    async fn call(
        &self,
        session: &Session,
        kind: &Kind,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Box<RawValue>>, CollectionError> {
        self.call_each(session, kind, calls)
            .await?
            .into_iter()
            .map(|answer| answer.map_err(|kind| method_error(&kind)))
            .collect()
    }

    /// Sends `calls` in one request and gives each answer's arguments or method error type, in
    /// order.
    async fn call_each(
        &self,
        session: &Session,
        kind: &Kind,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Box<RawValue>, String>>, CollectionError> {
        let count = calls.len();
        let body = json!({
            "using": [CORE, kind.capability],
            "methodCalls": calls
                .into_iter()
                .enumerate()
                .map(|(i, (name, args))| json!([name, args, i.to_string()]))
                .collect::<Vec<_>>(),
        });
        let answer = self
            .http
            .request(
                &session.api,
                Some(body.to_string().as_bytes()),
                http::MAX_ANSWER,
            )
            .await?;
        let envelope: Envelope<'_> = serde_json::from_slice(&answer)
            .map_err(|_| protocol("the JMAP server did not answer with JMAP"))?;
        let mut out: Vec<Option<Result<Box<RawValue>, String>>> = vec![None; count];
        for (name, args, id) in envelope.responses {
            let Some(index) = id.parse::<usize>().ok().filter(|i| *i < count) else {
                continue;
            };
            if out[index].is_some() {
                continue;
            }
            out[index] = Some(if name == "error" {
                Err(error_type(args))
            } else {
                Ok(args.to_owned())
            });
        }
        out.into_iter()
            .map(|a| a.ok_or_else(|| protocol("the JMAP server left a call unanswered")))
            .collect()
    }

    fn account<'a>(session: &'a Session, kind: &Kind) -> Result<&'a str, CollectionError> {
        session.account(kind).ok_or(CollectionError::Gone)
    }

    /// The state of the kind's items and whether collection `id` exists.
    async fn state(
        &self,
        session: &Session,
        kind: &Kind,
        id: &str,
    ) -> Result<String, CollectionError> {
        let account = Self::account(session, kind)?;
        let answers = self
            .call(
                session,
                kind,
                vec![
                    (
                        format!("{}/get", kind.collection),
                        json!({ "accountId": account, "ids": [id], "properties": ["id"] }),
                    ),
                    (
                        format!("{}/get", kind.item),
                        json!({ "accountId": account, "ids": [], "properties": ["id"] }),
                    ),
                ],
            )
            .await?;
        let collections: GetList<'_> = parse(&answers[0])?;
        if collections.list.is_empty() {
            return Err(CollectionError::Gone);
        }
        let items: GetList<'_> = parse(&answers[1])?;
        Ok(items.state)
    }

    async fn query(
        &self,
        session: &Session,
        kind: &Kind,
        id: &str,
    ) -> Result<Vec<String>, CollectionError> {
        let account = Self::account(session, kind)?;
        let mut ids: Vec<String> = Vec::new();
        let mut last: Option<Vec<String>> = None;
        for _ in 0..MAX_ROUNDS {
            let answers = self
                .call(
                    session,
                    kind,
                    vec![(
                        format!("{}/query", kind.item),
                        json!({
                            "accountId": account,
                            "filter": { kind.filter: id },
                            "position": ids.len(),
                            "limit": QUERY_PAGE,
                            "calculateTotal": true,
                        }),
                    )],
                )
                .await?;
            let page: Value = parse(&answers[0])?;
            let found: Vec<String> = page
                .get("ids")
                .and_then(Value::as_array)
                .ok_or_else(|| protocol("a JMAP query answer has no ids"))?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let total = page
                .get("total")
                .and_then(Value::as_u64)
                .map(|t| t as usize);
            let done = found.is_empty() || total.is_some_and(|t| ids.len() + found.len() >= t);
            if !done && last.as_ref() == Some(&found) {
                return Err(protocol("a JMAP query answered the same page twice"));
            }
            ids.extend(found.iter().cloned());
            if ids.len() > MAX_ITEMS {
                return Err(protocol("a JMAP query listed more than a million items"));
            }
            last = Some(found);
            if done {
                ids.sort();
                ids.dedup();
                return Ok(ids);
            }
        }
        Err(protocol("a JMAP query does not end"))
    }

    /// The items with these ids as the server returned them, in `/get` order; with `full` false
    /// only their id and collections.
    async fn get(
        &self,
        session: &Session,
        kind: &Kind,
        ids: &[String],
        full: bool,
    ) -> Result<Vec<(String, BTreeSet<String>, Bytes)>, CollectionError> {
        let account = Self::account(session, kind)?;
        let mut out = Vec::new();
        for batch in ids.chunks(session.max_objects) {
            let mut args = json!({ "accountId": account, "ids": batch });
            if !full {
                args["properties"] = json!(["id", kind.member]);
            }
            let answers = self
                .call(session, kind, vec![(format!("{}/get", kind.item), args)])
                .await?;
            let list: GetList<'_> = parse(&answers[0])?;
            for raw in list.list {
                let value: Value = serde_json::from_str(raw.get())
                    .map_err(|_| protocol("the JMAP server sent an invalid object"))?;
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let member: BTreeSet<String> = value
                    .get(kind.member)
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .filter(|(_, v)| v.as_bool() != Some(false))
                            .map(|(k, _)| k.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let id = id.to_owned();
                let bytes = serde_json::to_vec(&canonical(value))
                    .map_err(|_| protocol("the JMAP server sent an invalid object"))?;
                out.push((id, member, Bytes::from(bytes)));
            }
        }
        Ok(out)
    }
}

/// The event source address for `types` from the session's template (RFC 8620 §7.3); a plain
/// address gets the parameters as a query.
fn event_url(template: &str, types: &[&str], ping: Duration) -> String {
    let types = types.join(",");
    let ping = ping.as_secs().to_string();
    if template.contains("{types}") {
        template
            .replace("{types}", &types.replace(',', "%2C"))
            .replace("{closeafter}", "no")
            .replace("{ping}", &ping)
    } else {
        let joiner = if template.contains('?') { '&' } else { '?' };
        format!(
            "{template}{joiner}types={}&closeafter=no&ping={ping}",
            types.replace(',', "%2C")
        )
    }
}

struct JmapWatch {
    response: reqwest::Response,
    parser: holtmail_sse::Parser,
    accounts: Vec<String>,
    types: Vec<&'static str>,
    silence: Duration,
}

impl JmapWatch {
    fn relevant(&self, data: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return false;
        };
        if value.get("@type").and_then(Value::as_str) != Some("StateChange") {
            return false;
        }
        let Some(changed) = value.get("changed").and_then(Value::as_object) else {
            return false;
        };
        self.accounts.iter().any(|account| {
            changed
                .get(account)
                .and_then(Value::as_object)
                .is_some_and(|kinds| self.types.iter().any(|t| kinds.contains_key(*t)))
        })
    }
}

#[async_trait]
impl CollectionWatch for JmapWatch {
    async fn next(&mut self) -> Result<CollectionsChanged, CollectionError> {
        loop {
            let chunk = tokio::time::timeout(self.silence, self.response.chunk())
                .await
                .map_err(|_| CollectionError::Connection("the event source fell silent".into()))?
                .map_err(|_| CollectionError::Connection("the event source broke off".into()))?
                .ok_or_else(|| CollectionError::Connection("the event source closed".into()))?;
            let events = self
                .parser
                .push(&chunk)
                .map_err(|e| CollectionError::Protocol(e.to_string()))?;
            if events
                .iter()
                .any(|e| (e.event == "state" || e.event == "message") && self.relevant(&e.data))
            {
                return Ok(CollectionsChanged { collections: None });
            }
        }
    }

    async fn close(self: Box<Self>) {}
}

fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// The `type` of a JMAP error object, when it is a plain name; else `unknown`.
fn error_type(args: &RawValue) -> String {
    serde_json::from_str::<Value>(args.get())
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
        .filter(|k| k.len() <= 64 && k.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "unknown".into())
}

fn method_error(kind: &str) -> CollectionError {
    match kind {
        "cannotCalculateChanges" => CollectionError::TokenExpired,
        "forbidden" | "accountNotFound" | "accountNotSupportedByMethod" => {
            CollectionError::Auth(format!("the JMAP server refused access: {kind}"))
        }
        _ => CollectionError::Protocol(format!("the JMAP server refused a request: {kind}")),
    }
}

fn parse<'a, T: Deserialize<'a>>(raw: &'a RawValue) -> Result<T, CollectionError> {
    serde_json::from_str(raw.get()).map_err(|_| protocol("the JMAP server sent an invalid answer"))
}

#[async_trait]
impl CollectionSource for JmapCollections {
    /// Listens to the session's event source for changes to cards, address books, events and
    /// calendars of the session's accounts, reported as every collection; `None` when the session
    /// names no event source or offers neither contacts nor calendars.
    async fn watch(&self) -> Result<Option<Box<dyn CollectionWatch>>, CollectionError> {
        let session = self.session().await?;
        let Some(template) = session.events.clone() else {
            return Ok(None);
        };
        let mut types = Vec::new();
        let mut accounts = Vec::new();
        if let Some(account) = &session.contacts {
            types.extend(["ContactCard", "AddressBook"]);
            accounts.push(account.clone());
        }
        if let Some(account) = &session.calendars {
            types.extend(["CalendarEvent", "Calendar"]);
            accounts.push(account.clone());
        }
        if types.is_empty() {
            return Ok(None);
        }
        let url = Url::parse(&event_url(&template, &types, self.ping))
            .map_err(|_| protocol("the JMAP session names an invalid event source"))?;
        if !http::allowed(&url) {
            return Err(protocol(
                "the JMAP session names an event source without https",
            ));
        }
        let response = self.http.stream(&url).await?;
        Ok(Some(Box::new(JmapWatch {
            response,
            parser: holtmail_sse::Parser::default(),
            accounts,
            types,
            silence: self.ping * 5 / 2,
        })))
    }

    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError> {
        let session = self.session().await?;
        let mut out = Vec::new();
        for kind in [&BOOKS, &CALENDARS_KIND] {
            let Some(account) = session.account(kind) else {
                continue;
            };
            let answers = self
                .call(
                    &session,
                    kind,
                    vec![
                        (
                            format!("{}/get", kind.collection),
                            json!({ "accountId": account, "ids": null, "properties": kind.properties }),
                        ),
                        (
                            format!("{}/get", kind.item),
                            json!({ "accountId": account, "ids": [], "properties": ["id"] }),
                        ),
                    ],
                )
                .await?;
            let list: GetList<'_> = parse(&answers[0])?;
            let items: GetList<'_> = parse(&answers[1])?;
            for raw in list.list {
                let value: Value = serde_json::from_str(raw.get())
                    .map_err(|_| protocol("the JMAP server sent an invalid object"))?;
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let text = |key: &str| {
                    value
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                };
                out.push(RemoteCollection {
                    id: format!("{}{id}", kind.prefix),
                    kind: kind.kind,
                    name: text("name").unwrap_or_else(|| id.to_owned()),
                    color: text("color"),
                    components: match kind.kind {
                        CollectionKind::Calendar => vec!["VEVENT".into(), "VTODO".into()],
                        CollectionKind::Contacts => Vec::new(),
                    },
                    tag: (!items.state.is_empty()).then(|| items.state.clone()),
                });
            }
        }
        Ok(out)
    }

    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError> {
        let (kind, id) = split(collection)?;
        let session = self.session().await?;
        let Some(token) = token else {
            let state = self.state(&session, kind, id).await?;
            let ids = self.query(&session, kind, id).await?;
            return Ok(Changes {
                changed: ids
                    .into_iter()
                    .map(|id| ItemTag { id, etag: None })
                    .collect(),
                removed: Vec::new(),
                token: state,
            });
        };
        let account = Self::account(&session, kind)?;
        let mut since = token.to_owned();
        let mut touched: BTreeMap<String, bool> = BTreeMap::new();
        for _ in 0..MAX_ROUNDS {
            let answers = self
                .call(
                    &session,
                    kind,
                    vec![(
                        format!("{}/changes", kind.item),
                        json!({ "accountId": account, "sinceState": since, "maxChanges": MAX_CHANGES }),
                    )],
                )
                .await?;
            let answer: Value = parse(&answers[0])?;
            let ids = |key: &str| -> Vec<String> {
                answer
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for id in ids("created").into_iter().chain(ids("updated")) {
                touched.insert(id, true);
            }
            for id in ids("destroyed") {
                touched.insert(id, false);
            }
            if touched.len() > MAX_ITEMS {
                return Err(protocol("JMAP changes listed more than a million items"));
            }
            let state = answer
                .get("newState")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol("a JMAP changes answer has no new state"))?
                .to_owned();
            let more = answer.get("hasMoreChanges").and_then(Value::as_bool) == Some(true);
            if more && state == since {
                return Err(protocol("JMAP changes did not move past their state"));
            }
            since = state;
            if !more {
                let alive: Vec<String> = touched
                    .iter()
                    .filter(|(_, alive)| **alive)
                    .map(|(id, _)| id.clone())
                    .collect();
                let members: BTreeMap<String, BTreeSet<String>> = self
                    .get(&session, kind, &alive, false)
                    .await?
                    .into_iter()
                    .map(|(id, member, _)| (id, member))
                    .collect();
                let mut changes = Changes {
                    token: since,
                    ..Changes::default()
                };
                for (item, alive) in touched {
                    let here = alive && members.get(&item).is_some_and(|m| m.contains(id));
                    if here {
                        changes.changed.push(ItemTag {
                            id: item,
                            etag: None,
                        });
                    } else {
                        changes.removed.push(item);
                    }
                }
                return Ok(changes);
            }
        }
        Err(protocol("JMAP changes do not end"))
    }

    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<RemoteItem>, CollectionError> {
        let (kind, id) = split(collection)?;
        let session = self.session().await?;
        let mut unique: Vec<String> = Vec::new();
        for item in ids {
            if !unique.contains(item) {
                unique.push(item.clone());
            }
        }
        let mut seen = BTreeSet::new();
        Ok(self
            .get(&session, kind, &unique, true)
            .await?
            .into_iter()
            .filter(|(item, member, _)| member.contains(id) && seen.insert(item.clone()))
            .map(|(item, _, raw)| RemoteItem {
                id: item,
                etag: None,
                content_type: kind.media.to_owned(),
                raw,
            })
            .collect())
    }

    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError> {
        let (kind, id) = split(collection)?;
        let session = self.session().await?;
        self.state(&session, kind, id).await?;
        Ok(self
            .query(&session, kind, id)
            .await?
            .into_iter()
            .map(|id| ItemTag { id, etag: None })
            .collect())
    }
}

#[cfg(test)]
mod tests;
