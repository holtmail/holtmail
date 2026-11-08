//! Reading the contacts and calendars of a Microsoft 365, Outlook.com or Hotmail account through
//! Microsoft Graph, behind the [`CollectionSource`] contract.
//!
//! Address books are the default contacts folder and every contact folder below it; calendars are
//! the account's calendars. Each item is stored as the JSON Graph answers for it
//! (`application/vnd.microsoft.graph.contact+json`, `application/vnd.microsoft.graph.event+json`).
//!
//! Contacts change through delta queries, whose final delta link is the sync token. Graph has
//! delta for events only on the primary calendar's `calendarView`, which expands series into
//! occurrences and needs a time window, so calendars cannot list changes: each sync lists every
//! single event and series master with its `changeKey` as the version tag, and only events whose
//! tag changed are fetched. A series master is fetched with `cancelledOccurrences` and its
//! `exceptionOccurrences` expanded, so the stored JSON holds the whole series.
//!
//! The same type is also a [`holtmail_collection::CollectionWriter`]: changes arrive as iCalendar
//! or vCard and are sent as Graph JSON, only the fields the change lists. Reading never writes;
//! writes happen only through the writer, which the server hands out for accounts allowed to
//! write.

mod convert;
mod http;
mod write;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use holtmail_collection::{
    Changes, CollectionError, CollectionKind, CollectionSource, Credentials, ItemTag,
    RemoteCollection, RemoteItem, media,
};
use serde_json::Value;
use url::Url;

use http::{Answer, Http, Scope};

/// Where Microsoft Graph answers, for accounts in the global cloud.
pub const GRAPH_URL: &str = "https://graph.microsoft.com/v1.0";

/// The collection id of the default contacts folder.
pub const DEFAULT_CONTACTS: &str = "contacts";

const DOWNLOADS: usize = 4;
const MAX_FOLDERS: usize = 1_000;
const MAX_PAGES: usize = 10_000;
const MAX_ITEMS: usize = 1_000_000;
const EVENT_FIELDS: &str = "id,changeKey,iCalUId,subject,body,start,end,originalStartTimeZone,\
originalEndTimeZone,isAllDay,isCancelled,isOrganizer,type,seriesMasterId,originalStart,recurrence,\
attendees,organizer,location,locations,showAs,sensitivity,importance,categories,responseStatus,\
createdDateTime,lastModifiedDateTime,onlineMeeting,hideAttendees,cancelledOccurrences";

/// Microsoft Graph contacts and calendars for one account, signed in as the owner of an access
/// token with `Contacts.Read` and `Calendars.Read` for reading, `Contacts.ReadWrite` and
/// `Calendars.ReadWrite` for writing.
///
/// As a [`holtmail_collection::CollectionWriter`] it patches only the fields a change lists
/// (`field_patch`) and checks version tags (`changeKey`) by reading the item first and sending
/// `If-Match`. Creates are idempotent by key: events through Graph's `transactionId`, contacts
/// through a lookup of the key kept in an extended property. Answers to invitations go through
/// Graph's `accept`, `tentativelyAccept` and `decline`.
///
/// The token is sent only to the origin of the configured Graph address; links in answers that
/// point elsewhere, and redirects, are refused. Listings are capped at 64 MiB, one item at
/// 16 MiB; throttling (`429`) and short outages (`503`, `504`) are waited out as `Retry-After`
/// says, two minutes at most, five tries at most.
pub struct GraphCollections {
    base: Url,
    http: Http,
    types: Mutex<HashMap<String, String>>,
    listed: Mutex<Option<HashMap<String, bool>>>,
}

enum Place {
    Contacts(Option<String>),
    Calendar(String),
}

fn place(collection: &str) -> Result<Place, CollectionError> {
    if collection == DEFAULT_CONTACTS {
        return Ok(Place::Contacts(None));
    }
    if let Some(id) = collection.strip_prefix("contacts/") {
        return Ok(Place::Contacts(Some(id.to_owned())));
    }
    if let Some(id) = collection.strip_prefix("calendars/") {
        return Ok(Place::Calendar(id.to_owned()));
    }
    Err(CollectionError::Gone)
}

/// The address of the next page, refused when a listing already went there.
fn next_page(
    http: &Http,
    links: &mut HashSet<String>,
    next: String,
) -> Result<Url, CollectionError> {
    let url = http.url(&next)?;
    if !links.insert(url.to_string()) {
        return Err(protocol("a Graph listing named a page it already gave"));
    }
    Ok(url)
}

fn protocol(what: &str) -> CollectionError {
    CollectionError::Protocol(what.to_owned())
}

fn json(body: &[u8]) -> Result<Value, CollectionError> {
    serde_json::from_slice(body).map_err(|_| protocol("Microsoft Graph did not answer with JSON"))
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

struct Page {
    items: Vec<Value>,
    next: Option<String>,
    delta: Option<String>,
}

fn page(answer: &Value) -> Result<Page, CollectionError> {
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

impl GraphCollections {
    /// Also trusts `roots` for TLS, besides the built-in roots. `Config` when one of them is
    /// not a usable certificate.
    pub fn with_roots(mut self, roots: &holtmail_core::TlsRoots) -> Result<Self, CollectionError> {
        self.http = self.http.with_roots(roots)?;
        Ok(self)
    }

    /// A source for the account behind `credentials`, which must be an OAuth access token, at
    /// `api_url` ([`GRAPH_URL`] when `None`). `Config` for a password or an unusable address: only
    /// `https`, or plain `http` to a loopback address.
    pub fn new(
        api_url: Option<&str>,
        credentials: Credentials,
        timeout: Duration,
    ) -> Result<Self, CollectionError> {
        let Credentials::Bearer(token) = credentials else {
            return Err(CollectionError::Config(
                "Microsoft Graph signs in with Microsoft, not with a password".into(),
            ));
        };
        let raw = api_url.unwrap_or(GRAPH_URL).trim();
        let mut base = Url::parse(raw).map_err(|_| {
            CollectionError::Config("the Graph address is not a web address".into())
        })?;
        if !http::allowed(&base) || base.host_str().is_none() {
            return Err(CollectionError::Config(
                "the Graph address must use https".into(),
            ));
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        Ok(Self {
            http: Http::new(token, &base, timeout)?,
            base,
            types: Mutex::new(HashMap::new()),
            listed: Mutex::new(None),
        })
    }

    fn endpoint(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<Url, CollectionError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| CollectionError::Config("the Graph address cannot take a path".into()))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn value(&self, url: Url, scope: Scope) -> Result<Option<Value>, CollectionError> {
        match self.http.get(url, scope, false).await? {
            Answer::Found(body) => json(&body).map(Some),
            Answer::Missing | Answer::Gone => Ok(None),
        }
    }

    /// Every item of a listing, following `@odata.nextLink`; `None` when the first page is gone.
    async fn all(&self, first: Url, scope: Scope) -> Result<Option<Vec<Value>>, CollectionError> {
        let mut items = Vec::new();
        let mut links = HashSet::from([first.to_string()]);
        let mut url = first;
        for _ in 0..MAX_PAGES {
            let Some(answer) = self.value(url, scope).await? else {
                return Ok(None);
            };
            let page = page(&answer)?;
            items.extend(page.items);
            if items.len() > MAX_ITEMS {
                return Err(protocol("a Graph listing has more than a million items"));
            }
            match page.next {
                Some(next) => url = next_page(&self.http, &mut links, next)?,
                None => return Ok(Some(items)),
            }
        }
        Err(protocol("a Graph listing does not end"))
    }

    fn contacts_path(folder: &Option<String>) -> Vec<&str> {
        match folder {
            None => vec!["me", "contacts"],
            Some(id) => vec!["me", "contactFolders", id, "contacts"],
        }
    }

    async fn contact_folders(&self) -> Result<Vec<RemoteCollection>, CollectionError> {
        let query = [
            ("$select", "id,displayName,parentFolderId"),
            ("$top", "100"),
        ];
        let mut out = Vec::new();
        let mut queue = VecDeque::from([(
            self.endpoint(&["me", "contactFolders"], &query)?,
            String::new(),
        )]);
        while let Some((url, parent)) = queue.pop_front() {
            let Some(items) = self.all(url, Scope::Account).await? else {
                continue;
            };
            for item in items {
                let Some(id) = string(&item, "id") else {
                    continue;
                };
                let name = string(&item, "displayName").unwrap_or_else(|| "Contacts".into());
                let path = if parent.is_empty() {
                    name
                } else {
                    format!("{parent}/{name}")
                };
                queue.push_back((
                    self.endpoint(&["me", "contactFolders", &id, "childFolders"], &query)?,
                    path.clone(),
                ));
                out.push(RemoteCollection {
                    id: format!("contacts/{id}"),
                    kind: CollectionKind::Contacts,
                    name: path,
                    color: None,
                    components: Vec::new(),
                    tag: None,
                });
                if out.len() > MAX_FOLDERS {
                    return Err(protocol("the account has too many contact folders"));
                }
            }
        }
        Ok(out)
    }

    async fn contact_changes(
        &self,
        folder: &Option<String>,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError> {
        let mut url = match token {
            Some(token) => self
                .http
                .url(token)
                .map_err(|_| CollectionError::TokenExpired)?,
            None => {
                let mut path = Self::contacts_path(folder);
                path.push("delta");
                self.endpoint(&path, &[("$select", "id,changeKey")])?
            }
        };
        let mut state: BTreeMap<String, Option<Option<String>>> = BTreeMap::new();
        let mut links = HashSet::from([url.to_string()]);
        for _ in 0..MAX_PAGES {
            let answer = match self.http.get(url, Scope::Account, false).await? {
                Answer::Found(body) => json(&body)?,
                Answer::Gone if token.is_some() => return Err(CollectionError::TokenExpired),
                Answer::Gone | Answer::Missing => return Err(CollectionError::Gone),
            };
            let page = page(&answer)?;
            for item in page.items {
                let Some(id) = string(&item, "id") else {
                    continue;
                };
                if item.get("@removed").is_some() {
                    state.insert(id, None);
                } else {
                    state.insert(id, Some(string(&item, "changeKey")));
                }
            }
            if state.len() > MAX_ITEMS {
                return Err(protocol("a Graph delta has more than a million items"));
            }
            match (page.next, page.delta) {
                (Some(next), _) => url = next_page(&self.http, &mut links, next)?,
                (None, Some(delta)) => {
                    self.http.url(&delta)?;
                    let mut changes = Changes {
                        token: delta,
                        ..Changes::default()
                    };
                    for (id, entry) in state {
                        match entry {
                            Some(etag) => changes.changed.push(ItemTag { id, etag }),
                            None if token.is_some() => changes.removed.push(id),
                            None => {}
                        }
                    }
                    return Ok(changes);
                }
                (None, None) => return Err(protocol("a Graph delta ended without a delta link")),
            }
        }
        Err(protocol("a Graph delta does not end"))
    }

    async fn contact_listing(
        &self,
        folder: &Option<String>,
    ) -> Result<Vec<ItemTag>, CollectionError> {
        let url = self.endpoint(
            &Self::contacts_path(folder),
            &[("$select", "id,changeKey"), ("$top", "500")],
        )?;
        let items = self
            .all(url, Scope::Account)
            .await?
            .ok_or(CollectionError::Gone)?;
        let mut tags: Vec<ItemTag> = items
            .iter()
            .filter_map(|i| {
                Some(ItemTag {
                    id: string(i, "id")?,
                    etag: string(i, "changeKey"),
                })
            })
            .collect();
        tags.sort();
        tags.dedup_by(|a, b| a.id == b.id);
        Ok(tags)
    }

    async fn event_listing(&self, calendar: &str) -> Result<Vec<ItemTag>, CollectionError> {
        let url = self.endpoint(
            &["me", "calendars", calendar, "events"],
            &[("$select", "id,changeKey,type"), ("$top", "500")],
        )?;
        let items = self
            .all(url, Scope::Account)
            .await?
            .ok_or(CollectionError::Gone)?;
        let mut tags = Vec::with_capacity(items.len());
        {
            let mut types = self.types.lock().expect("types lock");
            for item in &items {
                let Some(id) = string(item, "id") else {
                    continue;
                };
                if let Some(kind) = string(item, "type") {
                    types.insert(id.clone(), kind);
                }
                tags.push(ItemTag {
                    id,
                    etag: string(item, "changeKey"),
                });
            }
        }
        tags.sort();
        tags.dedup_by(|a, b| a.id == b.id);
        Ok(tags)
    }

    async fn item(&self, url: Url) -> Result<Option<bytes::Bytes>, CollectionError> {
        match self.http.get(url, Scope::Item, true).await? {
            Answer::Found(body) => Ok(Some(body)),
            Answer::Missing | Answer::Gone => Ok(None),
        }
    }

    fn master_url(&self, calendar: &str, id: &str) -> Result<Url, CollectionError> {
        self.endpoint(
            &["me", "calendars", calendar, "events", id],
            &[
                ("$select", EVENT_FIELDS),
                ("$expand", "exceptionOccurrences"),
            ],
        )
    }

    async fn event(&self, calendar: &str, id: &str) -> Result<Option<RemoteItem>, CollectionError> {
        let known = self.types.lock().expect("types lock").get(id).cloned();
        let raw = if known.as_deref() == Some("seriesMaster") {
            self.item(self.master_url(calendar, id)?).await?
        } else {
            let plain = self
                .item(self.endpoint(&["me", "calendars", calendar, "events", id], &[])?)
                .await?;
            let master = plain
                .as_deref()
                .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                .and_then(|v| string(&v, "type"))
                .is_some_and(|t| t == "seriesMaster");
            if master {
                self.item(self.master_url(calendar, id)?).await?
            } else {
                plain
            }
        };
        Ok(raw.map(|raw| RemoteItem {
            id: id.to_owned(),
            etag: serde_json::from_slice::<Value>(&raw)
                .ok()
                .and_then(|v| string(&v, "changeKey")),
            content_type: media::GRAPH_EVENT.to_owned(),
            raw,
        }))
    }

    async fn fetch_one(
        &self,
        place: &Place,
        id: String,
    ) -> Result<Option<RemoteItem>, CollectionError> {
        match place {
            Place::Contacts(folder) => self.contact(folder, &id).await,
            Place::Calendar(calendar) => self.event(calendar, &id).await,
        }
    }

    async fn contact(
        &self,
        folder: &Option<String>,
        id: &str,
    ) -> Result<Option<RemoteItem>, CollectionError> {
        let mut path = Self::contacts_path(folder);
        path.push(id);
        let raw = self.item(self.endpoint(&path, &[])?).await?;
        Ok(raw.map(|raw| RemoteItem {
            id: id.to_owned(),
            etag: serde_json::from_slice::<Value>(&raw)
                .ok()
                .and_then(|v| string(&v, "changeKey")),
            content_type: media::GRAPH_CONTACT.to_owned(),
            raw,
        }))
    }
}

#[async_trait]
impl CollectionSource for GraphCollections {
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError> {
        let mut out = vec![RemoteCollection {
            id: DEFAULT_CONTACTS.into(),
            kind: CollectionKind::Contacts,
            name: "Contacts".into(),
            color: None,
            components: Vec::new(),
            tag: None,
        }];
        out.extend(self.contact_folders().await?);
        let url = self.endpoint(
            &["me", "calendars"],
            &[("$select", "id,name,hexColor,canEdit"), ("$top", "100")],
        )?;
        let mut editable: HashMap<String, bool> =
            out.iter().map(|c| (c.id.clone(), true)).collect();
        for calendar in self.all(url, Scope::Account).await?.unwrap_or_default() {
            let Some(id) = string(&calendar, "id") else {
                continue;
            };
            editable.insert(
                format!("calendars/{id}"),
                calendar.get("canEdit").and_then(Value::as_bool) != Some(false),
            );
            out.push(RemoteCollection {
                name: string(&calendar, "name").unwrap_or_else(|| "Calendar".into()),
                color: string(&calendar, "hexColor"),
                id: format!("calendars/{id}"),
                kind: CollectionKind::Calendar,
                components: vec!["VEVENT".into()],
                tag: None,
            });
        }
        *self.listed.lock().unwrap_or_else(|e| e.into_inner()) = Some(editable);
        Ok(out)
    }

    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError> {
        match place(collection)? {
            Place::Contacts(folder) => self.contact_changes(&folder, token).await,
            Place::Calendar(_) => Err(CollectionError::Unsupported),
        }
    }

    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<RemoteItem>, CollectionError> {
        let place = place(collection)?;
        let mut unique: Vec<String> = Vec::new();
        for id in ids {
            if !unique.contains(id) {
                unique.push(id.clone());
            }
        }
        let futures: Vec<_> = unique
            .into_iter()
            .map(|id| self.fetch_one(&place, id))
            .collect();
        futures::stream::iter(futures)
            .buffer_unordered(DOWNLOADS)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(Result::transpose)
            .collect()
    }

    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError> {
        match place(collection)? {
            Place::Contacts(folder) => self.contact_listing(&folder).await,
            Place::Calendar(calendar) => self.event_listing(&calendar).await,
        }
    }
}

#[cfg(test)]
mod tests;
