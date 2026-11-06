//! Reading address books and calendars from CardDAV (RFC 6352) and CalDAV (RFC 4791) servers
//! behind the [`CollectionSource`] contract.
//!
//! Discovery follows RFC 6764 (`/.well-known/carddav`, `/.well-known/caldav`), then the
//! principal and its home sets. Changes come from `sync-collection` (RFC 6578); bodies from
//! `addressbook-multiget` and `calendar-multiget` in batches, or one `GET` each on servers without
//! them. Read-only: only `PROPFIND`, `REPORT` and `GET` are sent.

mod http;
mod path;
mod xml;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
pub use holtmail_collection::Credentials;
use holtmail_collection::{
    Changes, CollectionError, CollectionKind, CollectionSource, ItemTag, RemoteCollection,
    RemoteItem,
};
use reqwest::StatusCode;
use url::Url;

use crate::http::{Http, MAX_XML, allowed_scheme, status_error};
use crate::xml::{CALDAV, CARDDAV, DAV, Element};

/// Items asked for in one multiget.
pub const BATCH: usize = 50;
const MAX_SYNC_ROUNDS: usize = 100;
const MAX_ITEMS: usize = 1_000_000;

/// Where to find a CardDAV and a CalDAV server and how to sign in.
#[derive(Debug, Clone)]
pub struct DavConfig {
    /// Where address books are looked for: a server (`https://dav.example.com`, whose
    /// well-known address is asked), a DAV root such as `https://host/remote.php/dav`, a
    /// principal, a home set or one address book. `None` reads no contacts.
    pub contacts: Option<String>,
    /// The same for calendars. `None` reads no calendars.
    pub calendars: Option<String>,
    /// How to sign in.
    pub credentials: Credentials,
    /// Longest wait for one answer.
    pub timeout: Duration,
}

impl DavConfig {
    /// A config for both kinds at `url`, as Nextcloud, Radicale and Baïkal serve them, with a
    /// one-minute timeout.
    pub fn at(url: &str, credentials: Credentials) -> Self {
        Self {
            contacts: Some(url.to_owned()),
            calendars: Some(url.to_owned()),
            credentials,
            timeout: Duration::from_secs(60),
        }
    }
}

/// A CardDAV and CalDAV client for one account.
///
/// Credentials are only sent over https (plain http only to loopback addresses), to the
/// configured servers, to where their well-known addresses lead, and to other hosts of the same
/// provider family (iCloud's partition hosts); a redirect anywhere else is refused. Answers are
/// capped at 64 MiB for listings and 16 MiB for one item; busy servers (`429`, `503`, `504`)
/// are waited out as `Retry-After` says, two minutes at most, five times at most.
pub struct DavSource {
    http: Http,
    contacts: Option<Url>,
    calendars: Option<Url>,
    found: Mutex<HashMap<String, (Url, CollectionKind)>>,
}

fn parse_url(raw: &str) -> Result<Url, CollectionError> {
    let raw = raw.trim();
    let with_scheme = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    let url = Url::parse(&with_scheme)
        .map_err(|_| CollectionError::Config(format!("{raw:?} is not an address")))?;
    if !allowed_scheme(&url) || url.host_str().is_none() {
        return Err(CollectionError::Config(
            "the contacts and calendars address must use https".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CollectionError::Config(
            "the address must not hold a user name or password".into(),
        ));
    }
    Ok(url)
}

enum Start {
    Homes(Vec<Url>),
    Collection(Url),
}

fn kind_names(kind: CollectionKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        CollectionKind::Contacts => (CARDDAV, "addressbook-home-set", "addressbook"),
        CollectionKind::Calendar => (CALDAV, "calendar-home-set", "calendar"),
    }
}

fn well_known_path(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Contacts => "/.well-known/carddav",
        CollectionKind::Calendar => "/.well-known/caldav",
    }
}

/// The `prop` elements of a response whose `propstat` says 200, merged.
fn ok_props(response: &Element) -> Vec<&Element> {
    response
        .children(DAV, "propstat")
        .filter(|p| {
            xml::status_code(p.child(DAV, "status")).is_none_or(|s| (200..300).contains(&s))
        })
        .filter_map(|p| p.child(DAV, "prop"))
        .collect()
}

fn prop<'a>(props: &[&'a Element], ns: &str, name: &str) -> Option<&'a Element> {
    props.iter().find_map(|p| p.child(ns, name))
}

fn multistatus(answer: &http::Answer) -> Result<Element, CollectionError> {
    let root = xml::parse(&answer.body).ok_or_else(|| {
        CollectionError::Protocol(format!(
            "{} did not answer with WebDAV",
            answer.url.host_str().unwrap_or("the server")
        ))
    })?;
    if !root.is(DAV, "multistatus") {
        return Err(CollectionError::Protocol(format!(
            "{} did not answer with WebDAV",
            answer.url.host_str().unwrap_or("the server")
        )));
    }
    Ok(root)
}

fn is_multistatus(status: StatusCode) -> bool {
    status.as_u16() == 207
}

fn etag(props: &[&Element]) -> Option<String> {
    prop(props, DAV, "getetag")
        .and_then(Element::trimmed)
        .map(str::to_owned)
}

impl DavSource {
    /// Also trusts `roots` for TLS, besides the built-in roots. `Config` when one of them is
    /// not a usable certificate.
    pub fn with_roots(mut self, roots: &holtmail_core::TlsRoots) -> Result<Self, CollectionError> {
        self.http = self.http.with_roots(roots)?;
        Ok(self)
    }

    /// A client for `config`. `Config` for an address that is not https (or loopback http), or
    /// when neither kind is configured. Nothing is sent before the first call.
    pub fn new(config: DavConfig) -> Result<Self, CollectionError> {
        let contacts = config.contacts.as_deref().map(parse_url).transpose()?;
        let calendars = config.calendars.as_deref().map(parse_url).transpose()?;
        if contacts.is_none() && calendars.is_none() {
            return Err(CollectionError::Config(
                "no contacts or calendars address".into(),
            ));
        }
        let origins = contacts
            .iter()
            .chain(calendars.iter())
            .map(Url::origin)
            .collect();
        Ok(Self {
            http: Http::new(config.credentials, origins, config.timeout)?,
            contacts,
            calendars,
            found: Mutex::new(HashMap::new()),
        })
    }

    async fn propfind(
        &self,
        url: &Url,
        depth: &str,
        props: &str,
    ) -> Result<(Element, Url), CollectionError> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <d:propfind xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\" \
             xmlns:cal=\"urn:ietf:params:xml:ns:caldav\" xmlns:cs=\"http://calendarserver.org/ns/\" \
             xmlns:ical=\"http://apple.com/ns/ical/\"><d:prop>{props}</d:prop></d:propfind>"
        );
        let answer = self
            .http
            .dav("PROPFIND", url, Some(depth), Some(&body), MAX_XML)
            .await?;
        if !is_multistatus(answer.status) {
            return Err(status_error(answer.status, &answer.url));
        }
        Ok((multistatus(&answer)?, answer.url))
    }

    async fn start(
        &self,
        kind: CollectionKind,
        configured: &Url,
    ) -> Result<Start, CollectionError> {
        let mut url = configured.clone();
        if matches!(url.path(), "" | "/") {
            let mut probe = url.clone();
            probe.set_path(well_known_path(kind));
            if let Some(found) = self.http.well_known(&probe).await? {
                url = found;
            }
        } else if url.path().starts_with("/.well-known/") {
            url = self.http.well_known(&url).await?.ok_or_else(|| {
                CollectionError::Config(format!(
                    "{} names no server at its well-known address",
                    url.host_str().unwrap_or("the server")
                ))
            })?;
        }
        let (ns, home_set, collection) = kind_names(kind);
        let props = format!(
            "<d:current-user-principal/><d:resourcetype/><{p}:{home_set}/>",
            p = if ns == CARDDAV { "card" } else { "cal" }
        );
        let (root, url) = match self.propfind(&url, "0", &props).await {
            Ok(found) => found,
            Err(CollectionError::Gone) => {
                return Err(CollectionError::Config(format!(
                    "{} has no {} at this address",
                    url.host_str().unwrap_or("the server"),
                    if kind == CollectionKind::Contacts {
                        "CardDAV"
                    } else {
                        "CalDAV"
                    }
                )));
            }
            Err(error) => return Err(error),
        };
        let Some(response) = root.child(DAV, "response") else {
            return Ok(Start::Homes(vec![url]));
        };
        let props = ok_props(response);
        let homes = |props: &[&Element], base: &Url| -> Vec<Url> {
            prop(props, ns, home_set)
                .map(|set| {
                    set.children(DAV, "href")
                        .filter_map(Element::trimmed)
                        .filter_map(|h| base.join(h).ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        let found = homes(&props, &url);
        if !found.is_empty() {
            return Ok(Start::Homes(found));
        }
        if prop(&props, DAV, "resourcetype").is_some_and(|t| t.child(ns, collection).is_some()) {
            return Ok(Start::Collection(url));
        }
        if let Some(principal) = prop(&props, DAV, "current-user-principal")
            .and_then(|p| p.child(DAV, "href"))
            .and_then(Element::trimmed)
            .and_then(|h| url.join(h).ok())
        {
            let (root, principal) = self
                .propfind(&principal, "0", &props_for_home(ns, home_set))
                .await?;
            if let Some(response) = root.child(DAV, "response") {
                let found = homes(&ok_props(response), &principal);
                if !found.is_empty() {
                    return Ok(Start::Homes(found));
                }
            }
        }
        Ok(Start::Homes(vec![url]))
    }

    async fn collections_of(
        &self,
        kind: CollectionKind,
        configured: &Url,
    ) -> Result<Vec<(RemoteCollection, Url)>, CollectionError> {
        let (urls, depth) = match self.start(kind, configured).await? {
            Start::Homes(homes) => (homes, "1"),
            Start::Collection(url) => (vec![url], "0"),
        };
        let (ns, _, collection_type) = kind_names(kind);
        let mut out = Vec::new();
        for url in urls {
            let (root, base) = self
                .propfind(
                    &url,
                    depth,
                    "<d:resourcetype/><d:displayname/><cs:getctag/><d:sync-token/>\
                     <ical:calendar-color/><cal:supported-calendar-component-set/>",
                )
                .await?;
            for response in root.children(DAV, "response") {
                let Some(href) = response.child(DAV, "href").and_then(Element::trimmed) else {
                    continue;
                };
                let Ok(at) = base.join(href) else { continue };
                let props = ok_props(response);
                let is_kind = prop(&props, DAV, "resourcetype")
                    .is_some_and(|t| t.child(ns, collection_type).is_some());
                if !is_kind {
                    continue;
                }
                let id = path::id(&at);
                let name = prop(&props, DAV, "displayname")
                    .and_then(Element::trimmed)
                    .map(str::to_owned)
                    .unwrap_or_else(|| path::last_segment(&id));
                let components = prop(&props, CALDAV, "supported-calendar-component-set")
                    .map(|set| {
                        set.children(CALDAV, "comp")
                            .filter_map(|c| c.attr("name"))
                            .map(str::to_ascii_uppercase)
                            .collect()
                    })
                    .unwrap_or_default();
                let tag = props
                    .iter()
                    .find_map(|p| p.child_named("getctag"))
                    .and_then(Element::trimmed)
                    .or_else(|| prop(&props, DAV, "sync-token").and_then(Element::trimmed))
                    .map(str::to_owned);
                let color = props
                    .iter()
                    .find_map(|p| p.child_named("calendar-color"))
                    .and_then(Element::trimmed)
                    .map(str::to_owned);
                out.push((
                    RemoteCollection {
                        id,
                        kind,
                        name,
                        color,
                        components: if kind == CollectionKind::Calendar {
                            components
                        } else {
                            Vec::new()
                        },
                        tag,
                    },
                    at,
                ));
            }
        }
        Ok(out)
    }

    async fn collection(&self, id: &str) -> Result<(Url, CollectionKind), CollectionError> {
        if let Some(found) = self.found.lock().expect("found lock").get(id) {
            return Ok(found.clone());
        }
        self.collections().await?;
        self.found
            .lock()
            .expect("found lock")
            .get(id)
            .cloned()
            .ok_or(CollectionError::Gone)
    }

    async fn sync_round(
        &self,
        url: &Url,
        id: &str,
        token: Option<&str>,
    ) -> Result<(Element, bool), CollectionError> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <d:sync-collection xmlns:d=\"DAV:\"><d:sync-token>{}</d:sync-token>\
             <d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>",
            xml::escape(token.unwrap_or(""))
        );
        let answer = self
            .http
            .dav("REPORT", url, Some("0"), Some(&body), MAX_XML)
            .await?;
        if !is_multistatus(answer.status) {
            let says_token = String::from_utf8_lossy(&answer.body).contains("valid-sync-token");
            return Err(match answer.status.as_u16() {
                _ if says_token && token.is_some() => CollectionError::TokenExpired,
                400 | 403 | 409 | 412 if token.is_some() => CollectionError::TokenExpired,
                404 | 410 => CollectionError::Gone,
                400 | 403 | 405 | 409 | 412 | 415 | 422 | 501 => CollectionError::Unsupported,
                _ => status_error(answer.status, &answer.url),
            });
        }
        let root = multistatus(&answer)?;
        let truncated = root.children(DAV, "response").any(|r| {
            xml::status_code(r.child(DAV, "status")) == Some(507)
                && r.child(DAV, "href")
                    .and_then(Element::trimmed)
                    .and_then(|h| answer.url.join(h).ok())
                    .is_some_and(|u| path::id(&u) == id)
        });
        Ok((root, truncated))
    }
}

fn props_for_home(ns: &str, home_set: &str) -> String {
    format!(
        "<{p}:{home_set}/>",
        p = if ns == CARDDAV { "card" } else { "cal" }
    )
}

#[async_trait]
impl CollectionSource for DavSource {
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut found = HashMap::new();
        for (kind, configured) in [
            (CollectionKind::Contacts, &self.contacts),
            (CollectionKind::Calendar, &self.calendars),
        ] {
            let Some(configured) = configured else {
                continue;
            };
            for (collection, url) in self.collections_of(kind, configured).await? {
                if seen.insert(collection.id.clone()) {
                    found.insert(collection.id.clone(), (url, kind));
                    out.push(collection);
                }
            }
        }
        tracing::debug!(collections = out.len(), "DAV collections listed");
        *self.found.lock().expect("found lock") = found;
        Ok(out)
    }

    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError> {
        let (url, _) = self.collection(collection).await?;
        let mut items: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut removed: BTreeSet<String> = BTreeSet::new();
        let mut token = token.map(str::to_owned);
        for _ in 0..MAX_SYNC_ROUNDS {
            let (root, truncated) = self.sync_round(&url, collection, token.as_deref()).await?;
            for response in root.children(DAV, "response") {
                let Some(at) = response
                    .child(DAV, "href")
                    .and_then(Element::trimmed)
                    .and_then(|h| url.join(h).ok())
                else {
                    continue;
                };
                let id = path::id(&at);
                if id == collection || id.ends_with('/') {
                    continue;
                }
                let status = xml::status_code(response.child(DAV, "status"));
                if matches!(status, Some(404 | 410)) {
                    items.remove(&id);
                    removed.insert(id);
                } else {
                    let props = ok_props(response);
                    removed.remove(&id);
                    items.insert(id, etag(&props));
                }
            }
            let next = root
                .child(DAV, "sync-token")
                .and_then(Element::trimmed)
                .map(str::to_owned)
                .ok_or_else(|| CollectionError::Protocol("the server sent no sync token".into()))?;
            if items.len() + removed.len() > MAX_ITEMS {
                return Err(CollectionError::Protocol(
                    "the server listed more than a million changes".into(),
                ));
            }
            if truncated && token.as_deref() == Some(next.as_str()) {
                return Err(CollectionError::Protocol(
                    "the server sent the same sync token for the next page".into(),
                ));
            }
            token = Some(next);
            if !truncated {
                let first = items.len();
                tracing::debug!(changed = first, removed = removed.len(), "DAV changes read");
                return Ok(Changes {
                    changed: items
                        .into_iter()
                        .map(|(id, etag)| ItemTag { id, etag })
                        .collect(),
                    removed: removed.into_iter().collect(),
                    token: token.unwrap_or_default(),
                });
            }
        }
        Err(CollectionError::Protocol(
            "the server kept the change list going too long".into(),
        ))
    }

    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<RemoteItem>, CollectionError> {
        let (url, kind) = self.collection(collection).await?;
        let mut wanted: Vec<String> = Vec::new();
        for id in ids {
            if !wanted.contains(id) {
                wanted.push(id.clone());
            }
        }
        let (report, data, content_type) = match kind {
            CollectionKind::Contacts => (
                "card:addressbook-multiget",
                "card:address-data",
                "text/vcard",
            ),
            CollectionKind::Calendar => (
                "cal:calendar-multiget",
                "cal:calendar-data",
                "text/calendar",
            ),
        };
        let (data_ns, data_name) = match kind {
            CollectionKind::Contacts => (CARDDAV, "address-data"),
            CollectionKind::Calendar => (CALDAV, "calendar-data"),
        };
        let mut out = Vec::new();
        let mut multiget = true;
        for batch in wanted.chunks(BATCH) {
            if multiget {
                let hrefs: String = batch
                    .iter()
                    .map(|id| format!("<d:href>{}</d:href>", xml::escape(&path::encode(id))))
                    .collect();
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                     <{report} xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\" \
                     xmlns:cal=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:getetag/><{data}/>\
                     </d:prop>{hrefs}</{report}>"
                );
                let answer = self
                    .http
                    .dav("REPORT", &url, Some("1"), Some(&body), MAX_XML)
                    .await?;
                if is_multistatus(answer.status) {
                    let root = multistatus(&answer)?;
                    for response in root.children(DAV, "response") {
                        let Some(at) = response
                            .child(DAV, "href")
                            .and_then(Element::trimmed)
                            .and_then(|h| answer.url.join(h).ok())
                        else {
                            continue;
                        };
                        let id = path::id(&at);
                        if !batch.contains(&id) || out.iter().any(|i: &RemoteItem| i.id == id) {
                            continue;
                        }
                        let props = ok_props(response);
                        let Some(text) = prop(&props, data_ns, data_name).map(|e| e.text.as_str())
                        else {
                            continue;
                        };
                        if text.trim().is_empty() {
                            continue;
                        }
                        out.push(RemoteItem {
                            id,
                            etag: etag(&props),
                            content_type: content_type.into(),
                            raw: Bytes::copy_from_slice(text.as_bytes()),
                        });
                    }
                    continue;
                }
                match answer.status.as_u16() {
                    400 | 403 | 405 | 415 | 422 | 501 => multiget = false,
                    // sabre/dav answers a whole multiget with 500 when it cannot read one item.
                    500..=599 => {}
                    _ => return Err(status_error(answer.status, &answer.url)),
                }
            }
            let lenient = multiget;
            let mut left_out = 0usize;
            for id in batch {
                let Ok(at) = url.join(&path::encode(id)) else {
                    continue;
                };
                match self.http.get(&at).await {
                    Ok(Some(answer)) => out.push(RemoteItem {
                        id: id.clone(),
                        etag: None,
                        content_type: content_type.into(),
                        raw: answer.body,
                    }),
                    Ok(None) => {}
                    Err(CollectionError::Connection(_)) if lenient => left_out += 1,
                    Err(error) => return Err(error),
                }
            }
            if left_out > 0 {
                tracing::warn!(left_out, "DAV items the server failed to send");
            }
        }
        tracing::debug!(
            asked = wanted.len(),
            fetched = out.len(),
            "DAV items fetched"
        );
        Ok(out)
    }

    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError> {
        let (url, _) = self.collection(collection).await?;
        let (root, base) = self
            .propfind(
                &url,
                "1",
                "<d:getetag/><d:resourcetype/><d:getcontenttype/>",
            )
            .await?;
        let mut out = BTreeMap::new();
        for response in root.children(DAV, "response") {
            let Some(at) = response
                .child(DAV, "href")
                .and_then(Element::trimmed)
                .and_then(|h| base.join(h).ok())
            else {
                continue;
            };
            let id = path::id(&at);
            let props = ok_props(response);
            let is_collection = prop(&props, DAV, "resourcetype")
                .is_some_and(|t| t.child(DAV, "collection").is_some());
            if id == collection || id.ends_with('/') || is_collection {
                continue;
            }
            out.insert(id, etag(&props));
        }
        Ok(out
            .into_iter()
            .map(|(id, etag)| ItemTag { id, etag })
            .collect())
    }
}

#[cfg(test)]
mod tests;
