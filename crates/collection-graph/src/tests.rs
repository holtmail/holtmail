use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use holtmail_collection::testing::{Remote, conformance};
use serde_json::json;

use super::*;

const TOKEN: &str = "graph-access-token";

#[derive(Default)]
struct Coll {
    name: String,
    parent: Option<String>,
    color: Option<String>,
    items: BTreeMap<String, (u64, Vec<u8>)>,
    log: Vec<(u64, String)>,
    keys: BTreeMap<String, String>,
    read_only: bool,
}

#[derive(Default)]
struct Fake {
    port: u16,
    page: usize,
    collections: BTreeMap<String, Coll>,
    seq: u64,
    generation: u64,
    throttle: u32,
    foreign_next: bool,
    stuck_next: bool,
    requests: Vec<String>,
    created: u64,
    transactions: BTreeMap<(String, String), String>,
    patches: Vec<(serde_json::Value, Option<String>)>,
    responses: Vec<(String, String, bool)>,
    deny_writes: bool,
    race: bool,
}

type Shared = Arc<Mutex<Fake>>;

fn item_id(name: &str) -> String {
    format!("AAMk-{name}")
}

fn name_of(id: &str) -> &str {
    id.strip_prefix("AAMk-").unwrap_or(id)
}

impl Fake {
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}/v1.0", self.port)
    }

    fn put(&mut self, collection: &str, name: &str, raw: &[u8]) {
        self.seq += 1;
        let seq = self.seq;
        let coll = self.collections.get_mut(collection).expect("a collection");
        coll.items.insert(name.into(), (seq, raw.to_vec()));
        coll.log.push((seq, name.into()));
    }

    fn delete(&mut self, collection: &str, name: &str) {
        self.seq += 1;
        let seq = self.seq;
        let coll = self.collections.get_mut(collection).expect("a collection");
        if coll.items.remove(name).is_some() {
            coll.log.push((seq, name.into()));
        }
    }

    fn page_of(
        &self,
        url: &str,
        all: Vec<serde_json::Value>,
        offset: usize,
        end: serde_json::Value,
    ) -> Response {
        let page = self.page.max(1);
        let slice: Vec<_> = all.iter().skip(offset).take(page).cloned().collect();
        let mut body = json!({ "value": slice });
        if offset + page < all.len() {
            let next = if self.stuck_next {
                let sep = if url.contains('?') { '&' } else { '?' };
                format!("{url}{sep}$skiptoken=stuck")
            } else if self.foreign_next {
                format!(
                    "http://localhost:{}/v1.0/me/calendars?$skiptoken=leak",
                    self.port
                )
            } else {
                let sep = if url.contains('?') { '&' } else { '?' };
                format!("{url}{sep}$skiptoken={}", offset + page)
            };
            body["@odata.nextLink"] = json!(next);
        } else if let serde_json::Value::String(delta) = end {
            body["@odata.deltaLink"] = json!(delta);
        }
        axum::Json(body).into_response()
    }
}

fn query(uri: &Uri) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

fn without_skip(base: &str, uri: &Uri) -> String {
    let kept: Vec<(String, String)> =
        url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
            .into_owned()
            .filter(|(k, _)| k != "$skiptoken")
            .collect();
    let path = uri.path().trim_start_matches("/v1.0");
    if kept.is_empty() {
        format!("{base}{path}")
    } else {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(kept)
            .finish();
        format!("{base}{path}?{q}")
    }
}

async fn handle(
    State(fake): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let mut f = fake.lock().unwrap();
    f.requests.push(format!("{method} {uri}"));
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOKEN}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if f.throttle > 0 {
        f.throttle -= 1;
        return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "0")]).into_response();
    }
    let q = query(&uri);
    let offset: usize = q
        .get("$skiptoken")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let path = uri.path().trim_start_matches("/v1.0/me/").to_owned();
    let segments: Vec<&str> = path.split('/').collect();
    if method != Method::GET {
        return writes::handle(&mut f, &method, &segments, &headers, &body);
    }
    let base = f.base();
    let here = without_skip(&base, &uri);
    let folder_key = |id: &str| format!("contacts/{id}");
    let folders = |f: &Fake, parent: Option<&str>| -> Vec<serde_json::Value> {
        f.collections
            .iter()
            .filter(|(k, c)| k.starts_with("contacts/") && c.parent.as_deref() == parent)
            .map(|(k, c)| json!({"id": k.trim_start_matches("contacts/"), "displayName": c.name}))
            .collect()
    };
    let contact_coll: Option<(String, &[&str])> = match segments.as_slice() {
        ["contacts", rest @ ..] => Some(("contacts".into(), rest)),
        ["contactFolders", id, "contacts", rest @ ..] => Some((folder_key(id), rest)),
        _ => None,
    };
    if let Some((key, rest)) = contact_coll {
        let Some(coll) = f.collections.get(&key) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match rest {
            [] => {
                let wanted = q.get("$filter").map(|f| writes::filtered_key(f));
                let all = coll
                    .items
                    .iter()
                    .filter(|(n, _)| {
                        wanted
                            .as_ref()
                            .is_none_or(|k| coll.keys.get(*n) == k.as_ref())
                    })
                    .map(|(n, (seq, _))| json!({"id": item_id(n), "changeKey": seq.to_string()}))
                    .collect();
                return f.page_of(&here, all, offset, serde_json::Value::Null);
            }
            ["delta"] => {
                let since = match q.get("$deltatoken") {
                    Some(token) => {
                        let Some((generation, seq)) = token.split_once('.') else {
                            return StatusCode::BAD_REQUEST.into_response();
                        };
                        if generation.parse::<u64>().ok() != Some(f.generation) {
                            return (
                                StatusCode::GONE,
                                axum::Json(json!({"error": {"code": "syncStateNotFound"}})),
                            )
                                .into_response();
                        }
                        seq.parse::<u64>().unwrap_or(0)
                    }
                    None => 0,
                };
                let touched: std::collections::BTreeSet<&String> = coll
                    .log
                    .iter()
                    .filter(|(s, _)| *s > since)
                    .map(|(_, n)| n)
                    .collect();
                let all: Vec<_> = touched
                    .into_iter()
                    .map(|n| match coll.items.get(n) {
                        Some((seq, _)) => json!({"id": item_id(n), "changeKey": seq.to_string()}),
                        None if since > 0 => {
                            json!({"id": item_id(n), "@removed": {"reason": "deleted"}})
                        }
                        None => json!(null),
                    })
                    .filter(|v| !v.is_null())
                    .collect();
                let delta_path = uri.path().trim_start_matches("/v1.0");
                let delta = format!("{base}{delta_path}?$deltatoken={}.{}", f.generation, f.seq);
                return f.page_of(&here, all, offset, json!(delta));
            }
            [id] => {
                return match coll.items.get(name_of(id)) {
                    Some((_, raw)) => raw.clone().into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                };
            }
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
    }
    match segments.as_slice() {
        ["contactFolders"] => {
            let all = folders(&f, None);
            f.page_of(&here, all, offset, serde_json::Value::Null)
        }
        ["contactFolders", id, "childFolders"] => {
            let all = folders(&f, Some(id));
            f.page_of(&here, all, offset, serde_json::Value::Null)
        }
        ["calendars"] => {
            let all = f
                .collections
                .iter()
                .filter(|(k, _)| k.starts_with("calendars/"))
                .map(|(k, c)| {
                    json!({"id": k.trim_start_matches("calendars/"), "name": c.name,
                        "hexColor": c.color.clone().unwrap_or_default(), "canEdit": !c.read_only})
                })
                .collect();
            f.page_of(&here, all, offset, serde_json::Value::Null)
        }
        ["calendars", id, "events", rest @ ..] => {
            let Some(coll) = f.collections.get(&format!("calendars/{id}")) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            match rest {
                [] => {
                    let all = coll
                        .items
                        .iter()
                        .map(|(n, (seq, raw))| {
                            let kind = serde_json::from_slice::<serde_json::Value>(raw)
                                .ok()
                                .and_then(|v| v.get("type").cloned())
                                .unwrap_or(json!("singleInstance"));
                            json!({"id": item_id(n), "changeKey": seq.to_string(), "type": kind})
                        })
                        .collect();
                    f.page_of(&here, all, offset, serde_json::Value::Null)
                }
                [event] => match coll.items.get(name_of(event)) {
                    Some((_, raw)) => raw.clone().into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                },
                [event, "instances"] => match coll.items.get(name_of(event)) {
                    Some((_, raw)) => {
                        let all = writes::instances(raw, &q);
                        f.page_of(&here, all, offset, serde_json::Value::Null)
                    }
                    None => StatusCode::NOT_FOUND.into_response(),
                },
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Clone)]
struct FakeGraph(Shared);

impl FakeGraph {
    async fn start() -> Self {
        let shared: Shared = Arc::new(Mutex::new(Fake {
            page: 2,
            ..Fake::default()
        }));
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        shared.lock().unwrap().port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self(shared)
    }

    fn add(&self, id: &str, name: &str, parent: Option<&str>, color: Option<&str>) {
        self.0.lock().unwrap().collections.insert(
            id.into(),
            Coll {
                name: name.into(),
                parent: parent.map(Into::into),
                color: color.map(Into::into),
                ..Coll::default()
            },
        );
    }

    async fn standard() -> Self {
        let fake = Self::start().await;
        fake.add(DEFAULT_CONTACTS, "Contacts", None, None);
        fake.add("calendars/AQMkCal", "Calendar", None, Some("#336699"));
        fake
    }

    fn source(&self) -> GraphCollections {
        GraphCollections::new(
            Some(&self.0.lock().unwrap().base()),
            Credentials::Bearer(TOKEN.into()),
            Duration::from_secs(10),
        )
        .unwrap()
    }

    fn requests(&self) -> Vec<String> {
        self.0.lock().unwrap().requests.clone()
    }
}

#[async_trait]
impl Remote for FakeGraph {
    async fn put(&self, collection: &str, name: &str, raw: &[u8]) {
        self.0.lock().unwrap().put(collection, name, raw);
    }

    async fn delete(&self, collection: &str, name: &str) {
        self.0.lock().unwrap().delete(collection, name);
    }

    async fn expire_tokens(&self) {
        self.0.lock().unwrap().generation += 1;
    }
}

#[tokio::test]
async fn keeps_the_contract() {
    let fake = FakeGraph::standard().await;
    conformance(&fake.source(), &fake).await;
}

#[tokio::test]
async fn lists_folders_with_paths_and_calendars_with_colors() {
    let fake = FakeGraph::standard().await;
    fake.add("contacts/F1", "Work", None, None);
    fake.add("contacts/F2", "Clients & Co", Some("F1"), None);
    fake.add("calendars/AQMkHome", "Home", None, None);
    let listed = fake.source().collections().await.unwrap();
    let names: Vec<(&str, &str, CollectionKind, Option<&str>)> = listed
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str(), c.kind, c.color.as_deref()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("contacts", "Contacts", CollectionKind::Contacts, None),
            ("contacts/F1", "Work", CollectionKind::Contacts, None),
            (
                "contacts/F2",
                "Work/Clients & Co",
                CollectionKind::Contacts,
                None
            ),
            (
                "calendars/AQMkCal",
                "Calendar",
                CollectionKind::Calendar,
                Some("#336699")
            ),
            ("calendars/AQMkHome", "Home", CollectionKind::Calendar, None),
        ]
    );
}

#[tokio::test]
async fn folder_contacts_change_by_delta_and_items_are_raw_json() {
    let fake = FakeGraph::standard().await;
    fake.add("contacts/F1", "Work", None, None);
    let source = fake.source();
    let ada = br#"{"id":"AAMk-ada","changeKey":"x","displayName":"Ada Quill"}"#;
    fake.put("contacts/F1", "ada", ada).await;
    let first = source.changes("contacts/F1", None).await.unwrap();
    assert_eq!(first.changed.len(), 1);
    assert!(first.token.contains("/me/contactFolders/F1/contacts/delta"));
    let items = source
        .fetch("contacts/F1", &[first.changed[0].id.clone()])
        .await
        .unwrap();
    assert_eq!(
        items[0].raw.as_ref(),
        ada.as_slice(),
        "the JSON as Graph sent it"
    );
    assert_eq!(items[0].content_type, media::GRAPH_CONTACT);
    assert_eq!(items[0].etag.as_deref(), Some("x"));
    fake.delete("contacts/F1", "ada").await;
    let second = source
        .changes("contacts/F1", Some(&first.token))
        .await
        .unwrap();
    assert_eq!(second.removed, vec!["AAMk-ada".to_owned()]);
}

#[tokio::test]
async fn calendars_list_and_fetch_series_with_their_exceptions() {
    let fake = FakeGraph::standard().await;
    let cal = "calendars/AQMkCal";
    let source = fake.source();
    assert!(matches!(
        source.changes(cal, None).await,
        Err(CollectionError::Unsupported)
    ));
    let series =
        br#"{"id":"AAMk-series","changeKey":"1","type":"seriesMaster","subject":"Standup"}"#;
    let single =
        br#"{"id":"AAMk-single","changeKey":"2","type":"singleInstance","subject":"Review"}"#;
    fake.put(cal, "series", series).await;
    fake.put(cal, "single", single).await;
    let listing = source.list(cal).await.unwrap();
    assert_eq!(listing.len(), 2);
    let ids: Vec<String> = listing.iter().map(|t| t.id.clone()).collect();
    let mut items = source.fetch(cal, &ids).await.unwrap();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(items[0].raw.as_ref(), series.as_slice());
    assert_eq!(items[0].content_type, media::GRAPH_EVENT);
    let requests = fake.requests();
    let master = requests
        .iter()
        .find(|r| r.contains("events/AAMk-series"))
        .expect("the series was fetched");
    assert!(master.contains("exceptionOccurrences"), "{master}");
    assert!(master.contains("cancelledOccurrences"), "{master}");
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.contains("events/AAMk-series"))
            .count(),
        1,
        "the listing told it is a series"
    );
    let plain = requests
        .iter()
        .find(|r| r.contains("events/AAMk-single"))
        .unwrap();
    assert!(!plain.contains("exceptionOccurrences"));

    let fresh = fake.source();
    let again = fresh.fetch(cal, &["AAMk-series".into()]).await.unwrap();
    assert_eq!(again.len(), 1);
    let series_requests: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.contains("events/AAMk-series"))
        .collect();
    assert_eq!(
        series_requests.len(),
        3,
        "without a listing it asks once plainly, then with the exceptions"
    );
    assert!(series_requests[2].contains("exceptionOccurrences"));
}

#[tokio::test]
async fn waits_when_throttled() {
    let fake = FakeGraph::standard().await;
    fake.0.lock().unwrap().throttle = 2;
    assert_eq!(fake.source().collections().await.unwrap().len(), 2);
}

#[tokio::test]
async fn links_to_another_host_are_refused_and_nothing_is_sent_there() {
    let fake = FakeGraph::standard().await;
    fake.add("calendars/AQMkB", "B", None, None);
    fake.add("calendars/AQMkC", "C", None, None);
    fake.0.lock().unwrap().foreign_next = true;
    let error = fake.source().collections().await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    assert!(!fake.requests().iter().any(|r| r.contains("leak")));
    let source = fake.source();
    let foreign = format!(
        "http://localhost:{}/v1.0/me/contacts/delta?$deltatoken=0.0",
        fake.0.lock().unwrap().port
    );
    assert!(matches!(
        source.changes(DEFAULT_CONTACTS, Some(&foreign)).await,
        Err(CollectionError::TokenExpired)
    ));
    assert!(!fake.requests().iter().any(|r| r.contains("deltatoken=0.0")));
}

#[tokio::test]
async fn a_next_link_that_does_not_move_on_is_refused() {
    let fake = FakeGraph::standard().await;
    for name in ["ada", "bob", "cleo"] {
        fake.put(DEFAULT_CONTACTS, name, br#"{"displayName":"Someone"}"#)
            .await;
    }
    fake.add("calendars/AQMkB", "B", None, None);
    fake.add("calendars/AQMkC", "C", None, None);
    fake.0.lock().unwrap().stuck_next = true;
    let source = fake.source();
    let error = source.collections().await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    let error = source.changes(DEFAULT_CONTACTS, None).await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    assert!(
        fake.requests().len() < 10,
        "stops at the first repeat: {:?}",
        fake.requests()
    );
}

#[tokio::test]
async fn refuses_passwords_plain_http_and_bad_tokens() {
    assert!(matches!(
        GraphCollections::new(
            None,
            Credentials::Password {
                username: "ada".into(),
                password: "secret".into()
            },
            Duration::from_secs(1)
        ),
        Err(CollectionError::Config(_))
    ));
    assert!(matches!(
        GraphCollections::new(
            Some("http://graph.example.com/v1.0"),
            Credentials::Bearer(TOKEN.into()),
            Duration::from_secs(1)
        ),
        Err(CollectionError::Config(_))
    ));
    let fake = FakeGraph::standard().await;
    let wrong = GraphCollections::new(
        Some(&fake.0.lock().unwrap().base()),
        Credentials::Bearer("stale".into()),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(matches!(
        wrong.collections().await,
        Err(CollectionError::Auth(_))
    ));
    assert!(matches!(
        fake.source().list("elsewhere").await,
        Err(CollectionError::Gone)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn trusts_extra_roots_only_when_given() {
    let ca = holtmail_core::tls::testing::TestCa::new(&["localhost"]);
    let server = ca.serve_https(401, "{}").await;
    let url = format!("https://localhost:{}/v1.0", server.port());
    let source = || {
        GraphCollections::new(
            Some(&url),
            Credentials::Bearer("token".into()),
            Duration::from_secs(5),
        )
        .unwrap()
    };
    let trusted = source().with_roots(&ca.roots()).unwrap();
    assert!(matches!(
        trusted.collections().await,
        Err(CollectionError::Auth(_))
    ));
    assert!(matches!(
        source().collections().await,
        Err(CollectionError::Connection(_))
    ));
}

mod writes;
