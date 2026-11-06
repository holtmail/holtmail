use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes as Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use holtmail_collection::testing::{Remote, conformance};

use super::*;

const TOKEN: &str = "jmap-token";
const ACCOUNT: &str = "acc1";

#[derive(Clone)]
struct FakeColl {
    kind: CollectionKind,
    name: String,
    color: Option<String>,
}

#[derive(Clone)]
struct FakeItem {
    member: String,
    json: String,
}

#[derive(Default)]
struct Fake {
    calendars: bool,
    colls: BTreeMap<String, FakeColl>,
    items: HashMap<(CollectionKind, String), FakeItem>,
    log: Vec<(u64, CollectionKind, String)>,
    seq: u64,
    generation: u64,
    page: Option<usize>,
    api_url: Option<String>,
    stuck: bool,
    busy: u32,
    requests: Vec<(String, bool)>,
    calls: Vec<String>,
    port: u16,
}

type Shared = Arc<Mutex<Fake>>;

fn kind_of(prefix: &str) -> CollectionKind {
    if prefix.starts_with("calendar/") {
        CollectionKind::Calendar
    } else {
        CollectionKind::Contacts
    }
}

fn member_key(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Contacts => "addressBookIds",
        CollectionKind::Calendar => "calendarIds",
    }
}

fn item_id(name: &str) -> String {
    name.replace('.', "-")
}

fn wrap(kind: CollectionKind, collection: &str, name: &str, raw: &[u8]) -> String {
    let jmap = collection.split_once('/').map_or(collection, |(_, id)| id);
    format!(
        "{{\"{}\":{{{}:true}},\"id\":{},\"x-raw\":{}}}",
        member_key(kind),
        serde_json::to_string(jmap).unwrap(),
        serde_json::to_string(&item_id(name)).unwrap(),
        serde_json::to_string(&String::from_utf8_lossy(raw)).unwrap()
    )
}

impl Fake {
    fn state(&self) -> String {
        format!("{}-{}", self.generation, self.seq)
    }

    fn put(&mut self, collection: &str, name: &str, json: String) {
        let kind = kind_of(collection);
        self.seq += 1;
        let jmap = collection.split_once('/').unwrap().1.to_owned();
        let id = item_id(name);
        self.items
            .insert((kind, id.clone()), FakeItem { member: jmap, json });
        self.log.push((self.seq, kind, id));
    }

    fn method(&mut self, name: &str, args: &Value) -> Result<String, &'static str> {
        let (type_name, verb) = name.split_once('/').ok_or("unknownMethod")?;
        let kind = match type_name {
            "AddressBook" | "ContactCard" => CollectionKind::Contacts,
            "Calendar" | "CalendarEvent" => CollectionKind::Calendar,
            _ => return Err("unknownMethod"),
        };
        if kind == CollectionKind::Calendar && !self.calendars {
            return Err("unknownMethod");
        }
        if args["accountId"] != ACCOUNT {
            return Err("accountNotFound");
        }
        let state = serde_json::to_string(&self.state()).unwrap();
        match (type_name, verb) {
            ("AddressBook" | "Calendar", "get") => {
                let wanted: Option<Vec<String>> = args["ids"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                });
                let list: Vec<String> = self
                    .colls
                    .iter()
                    .filter(|(_, c)| c.kind == kind)
                    .map(|(k, c)| (k.split_once('/').unwrap().1, c))
                    .filter(|(id, _)| wanted.as_ref().is_none_or(|w| w.iter().any(|x| x == id)))
                    .map(|(id, c)| {
                        json!({ "id": id, "name": c.name, "color": c.color }).to_string()
                    })
                    .collect();
                Ok(format!(
                    "{{\"accountId\":\"{ACCOUNT}\",\"state\":\"c\",\"list\":[{}],\"notFound\":[]}}",
                    list.join(",")
                ))
            }
            (_, "get") => {
                let ids: Vec<String> = args["ids"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let small = args.get("properties").is_some_and(|p| p.is_array());
                let mut list = Vec::new();
                let mut missing = Vec::new();
                for id in ids {
                    match self.items.get(&(kind, id.clone())) {
                        Some(item) if small => list.push(
                            json!({ "id": id, member_key(kind): { item.member.clone(): true } })
                                .to_string(),
                        ),
                        Some(item) => list.push(item.json.clone()),
                        None => missing.push(id),
                    }
                }
                Ok(format!(
                    "{{\"accountId\":\"{ACCOUNT}\",\"state\":{state},\"list\":[{}],\"notFound\":{}}}",
                    list.join(","),
                    serde_json::to_string(&missing).unwrap()
                ))
            }
            (_, "query") => {
                let filter = args["filter"]
                    .as_object()
                    .and_then(|f| f.values().next())
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let all: Vec<String> = self
                    .items
                    .iter()
                    .filter(|((k, _), item)| *k == kind && item.member == filter)
                    .map(|((_, id), _)| id.clone())
                    .collect();
                let position = args["position"].as_u64().unwrap_or(0) as usize;
                let limit = self.page.unwrap_or(usize::MAX);
                if self.stuck {
                    let page: Vec<&String> = all.iter().take(limit).collect();
                    return Ok(
                        json!({ "accountId": ACCOUNT, "ids": page, "position": 0 }).to_string()
                    );
                }
                let page: Vec<&String> = all.iter().skip(position).take(limit).collect();
                Ok(json!({ "accountId": ACCOUNT, "ids": page, "total": all.len(), "position": position })
                    .to_string())
            }
            (_, "changes") => {
                let since = args["sinceState"].as_str().unwrap_or_default();
                let (generation, seq) = since.split_once('-').ok_or("cannotCalculateChanges")?;
                if generation.parse::<u64>().ok() != Some(self.generation) {
                    return Err("cannotCalculateChanges");
                }
                let since: u64 = seq.parse().map_err(|_| "cannotCalculateChanges")?;
                if self.stuck {
                    return Ok(json!({
                        "accountId": ACCOUNT, "oldState": args["sinceState"], "newState": args["sinceState"],
                        "hasMoreChanges": true, "created": [], "updated": [], "destroyed": [],
                    })
                    .to_string());
                }
                let mut entries: Vec<&(u64, CollectionKind, String)> = self
                    .log
                    .iter()
                    .filter(|(s, k, _)| *s > since && *k == kind)
                    .collect();
                let more = self.page.is_some_and(|p| entries.len() > p);
                if let Some(p) = self.page {
                    entries.truncate(p);
                }
                let last = entries.last().map_or(self.seq, |(s, _, _)| *s);
                let new_state = if more {
                    format!("{}-{last}", self.generation)
                } else {
                    self.state()
                };
                let mut created = Vec::new();
                let mut updated = Vec::new();
                let mut destroyed = Vec::new();
                for (_, _, id) in entries {
                    let exists = self.items.contains_key(&(kind, id.clone()));
                    for list in [&mut created, &mut updated, &mut destroyed] {
                        list.retain(|x: &String| x != id);
                    }
                    if exists {
                        updated.push(id.clone());
                    } else {
                        destroyed.push(id.clone());
                    }
                }
                Ok(json!({
                    "accountId": ACCOUNT, "oldState": since.to_string(), "newState": new_state,
                    "hasMoreChanges": more, "created": created, "updated": updated, "destroyed": destroyed,
                })
                .to_string())
            }
            _ => Err("unknownMethod"),
        }
    }
}

async fn handle(
    State(fake): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let mut f = fake.lock().unwrap();
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    f.requests.push((uri.path().to_owned(), auth.is_some()));
    if uri.path() == "/.well-known/jmap" {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [("location", "/jmap/session")],
        )
            .into_response();
    }
    if auth != Some(&format!("Bearer {TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if f.busy > 0 {
        f.busy -= 1;
        return (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "0")]).into_response();
    }
    match (method, uri.path()) {
        (Method::GET, "/jmap/session") => {
            let mut capabilities = json!({ CORE: { "maxObjectsInGet": 2 }, CONTACTS: {} });
            let mut primary = json!({ CONTACTS: ACCOUNT });
            if f.calendars {
                capabilities[CALENDARS] = json!({});
                primary[CALENDARS] = json!(ACCOUNT);
            }
            let api = f.api_url.clone().unwrap_or_else(|| "/jmap/api".to_owned());
            axum::Json(json!({
                "capabilities": capabilities, "primaryAccounts": primary, "apiUrl": api,
                "accounts": { ACCOUNT: { "name": "ada@example.com" } }, "state": "s1",
            }))
            .into_response()
        }
        (Method::POST, "/jmap/api") => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            let mut responses = Vec::new();
            for call in request["methodCalls"].as_array().unwrap() {
                let name = call[0].as_str().unwrap();
                f.calls.push(name.to_owned());
                let id = serde_json::to_string(&call[2]).unwrap();
                match f.method(name, &call[1]) {
                    Ok(args) => responses.push(format!("[\"{name}\",{args},{id}]")),
                    Err(kind) => {
                        responses.push(format!("[\"error\",{{\"type\":\"{kind}\"}},{id}]"))
                    }
                }
            }
            (
                [("content-type", "application/json")],
                format!(
                    "{{\"methodResponses\":[{}],\"sessionState\":\"s1\"}}",
                    responses.join(",")
                ),
            )
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Clone)]
struct FakeJmap(Shared);

impl FakeJmap {
    async fn start(calendars: bool) -> Self {
        let shared: Shared = Arc::new(Mutex::new(Fake {
            calendars,
            ..Fake::default()
        }));
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        shared.lock().unwrap().port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let fake = Self(shared);
        fake.add("addressbook/b1", "Contacts", None);
        if calendars {
            fake.add("calendar/k1", "Work & Life", Some("#336699"));
        }
        fake
    }

    fn add(&self, id: &str, name: &str, color: Option<&str>) {
        self.0.lock().unwrap().colls.insert(
            id.into(),
            FakeColl {
                kind: kind_of(id),
                name: name.into(),
                color: color.map(Into::into),
            },
        );
    }

    fn put_json(&self, collection: &str, name: &str, json: &str) {
        self.0
            .lock()
            .unwrap()
            .put(collection, name, json.to_owned());
    }

    fn source_at(&self, path: &str) -> JmapCollections {
        let port = self.0.lock().unwrap().port;
        JmapCollections::new(
            &format!("http://127.0.0.1:{port}{path}"),
            Credentials::Bearer(TOKEN.into()),
            Duration::from_secs(10),
        )
        .unwrap()
    }

    fn source(&self) -> JmapCollections {
        self.source_at("/jmap/session")
    }
}

#[async_trait]
impl Remote for FakeJmap {
    async fn put(&self, collection: &str, name: &str, raw: &[u8]) {
        let json = wrap(kind_of(collection), collection, name, raw);
        self.0.lock().unwrap().put(collection, name, json);
    }

    async fn delete(&self, collection: &str, name: &str) {
        let mut f = self.0.lock().unwrap();
        let kind = kind_of(collection);
        f.seq += 1;
        let seq = f.seq;
        let id = item_id(name);
        f.items.remove(&(kind, id.clone()));
        f.log.push((seq, kind, id));
    }

    async fn expire_tokens(&self) {
        self.0.lock().unwrap().generation += 1;
    }

    fn stored(&self, collection: &str, name: &str, raw: &[u8]) -> Vec<u8> {
        wrap(kind_of(collection), collection, name, raw).into_bytes()
    }
}

#[tokio::test]
async fn keeps_the_contract() {
    let fake = FakeJmap::start(true).await;
    conformance(&fake.source(), &fake).await;
}

#[tokio::test]
async fn keeps_the_contract_in_small_pages() {
    let fake = FakeJmap::start(true).await;
    fake.0.lock().unwrap().page = Some(1);
    conformance(&fake.source(), &fake).await;
}

#[tokio::test]
async fn contacts_only_when_the_server_has_no_calendars() {
    let fake = FakeJmap::start(false).await;
    let source = fake.source();
    assert!(source.offers_contacts().await.unwrap());
    let listed = source.collections().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "addressbook/b1");
    assert!(
        !fake
            .0
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|c| c.starts_with("Calendar"))
    );
    assert!(matches!(
        source.list("calendar/k1").await,
        Err(CollectionError::Gone)
    ));
}

#[tokio::test]
async fn names_colors_and_raw_json_kept() {
    let fake = FakeJmap::start(true).await;
    let card = r#"{"@type":"Card","version":"1.0","id":"c1","addressBookIds":{"b1":true},"uid":"bea-1","name":{"full":"Bea Example"}}"#;
    fake.put_json("addressbook/b1", "c1", card);
    let source = fake.source();
    let listed = source.collections().await.unwrap();
    let calendar = listed
        .iter()
        .find(|c| c.kind == CollectionKind::Calendar)
        .unwrap();
    assert_eq!(calendar.name, "Work & Life");
    assert_eq!(calendar.color.as_deref(), Some("#336699"));
    let items = source
        .fetch("addressbook/b1", &["c1".into()])
        .await
        .unwrap();
    assert_eq!(
        items[0].raw.as_ref(),
        br#"{"@type":"Card","addressBookIds":{"b1":true},"id":"c1","name":{"full":"Bea Example"},"uid":"bea-1","version":"1.0"}"#,
        "the object with its keys sorted"
    );
    let shuffled = r#"{"name":{"full":"Bea Example"},"uid":"bea-1","addressBookIds":{"b1":true},"id":"c1","version":"1.0","@type":"Card"}"#;
    fake.put_json("addressbook/b1", "c1", shuffled);
    let again = source
        .fetch("addressbook/b1", &["c1".into()])
        .await
        .unwrap();
    assert_eq!(
        again[0].raw, items[0].raw,
        "the same object in another key order reads the same"
    );
    assert_eq!(items[0].content_type, media::JSCONTACT);
    assert!(
        source
            .fetch("calendar/k1", &["c1".into()])
            .await
            .unwrap()
            .is_empty(),
        "a card is not in a calendar"
    );
}

#[tokio::test]
async fn a_card_moved_to_another_book_leaves_the_first() {
    let fake = FakeJmap::start(false).await;
    fake.add("addressbook/b2", "Work", None);
    fake.put_json(
        "addressbook/b1",
        "c1",
        r#"{"id":"c1","addressBookIds":{"b1":true}}"#,
    );
    let source = fake.source();
    let first = source.changes("addressbook/b1", None).await.unwrap();
    assert_eq!(first.changed.len(), 1);
    fake.put_json(
        "addressbook/b2",
        "c1",
        r#"{"id":"c1","addressBookIds":{"b2":true}}"#,
    );
    let moved = source
        .changes("addressbook/b1", Some(&first.token))
        .await
        .unwrap();
    assert!(moved.changed.is_empty());
    assert_eq!(moved.removed, vec!["c1".to_owned()]);
    let there = source
        .changes("addressbook/b2", Some(&first.token))
        .await
        .unwrap();
    assert_eq!(there.changed[0].id, "c1");
}

#[tokio::test]
async fn pages_that_do_not_move_on_are_refused() {
    let fake = FakeJmap::start(false).await;
    for name in ["c1", "c2", "c3"] {
        fake.put_json(
            "addressbook/b1",
            name,
            &format!(r#"{{"id":"{name}","addressBookIds":{{"b1":true}}}}"#),
        );
    }
    let source = fake.source();
    let first = source.changes("addressbook/b1", None).await.unwrap();
    {
        let mut f = fake.0.lock().unwrap();
        f.page = Some(1);
        f.stuck = true;
    }
    let error = source.list("addressbook/b1").await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    let error = source
        .changes("addressbook/b1", Some(&first.token))
        .await
        .unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    assert!(fake.0.lock().unwrap().calls.len() < 20);
}

#[tokio::test]
async fn the_well_known_address_is_followed_without_credentials() {
    let fake = FakeJmap::start(false).await;
    let source = fake.source_at("/.well-known/jmap");
    assert_eq!(source.collections().await.unwrap().len(), 1);
    let requests = fake.0.lock().unwrap().requests.clone();
    assert_eq!(requests[0], ("/.well-known/jmap".to_owned(), false));
    assert!(
        !requests
            .iter()
            .any(|(path, auth)| path == "/.well-known/jmap" && *auth),
        "no credentials to the well-known address"
    );
    assert_eq!(
        requests
            .iter()
            .find(|(_, auth)| *auth)
            .map(|(p, _)| p.as_str()),
        Some("/jmap/session"),
        "credentials first go where it led"
    );
}

#[tokio::test]
async fn an_api_on_another_host_the_session_names_gets_the_credentials() {
    let fake = FakeJmap::start(false).await;
    let port = fake.0.lock().unwrap().port;
    fake.0.lock().unwrap().api_url = Some(format!("http://localhost:{port}/jmap/api"));
    assert_eq!(fake.source().collections().await.unwrap().len(), 1);
    let requests = fake.0.lock().unwrap().requests.clone();
    assert!(
        requests.iter().any(|(p, auth)| p == "/jmap/api" && *auth),
        "{requests:?}"
    );
}

#[tokio::test]
async fn an_api_without_https_is_refused_and_nothing_is_sent_there() {
    let fake = FakeJmap::start(false).await;
    fake.0.lock().unwrap().api_url = Some("http://jmap.example.org/jmap/api".into());
    let error = fake.source().collections().await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    assert!(
        !fake
            .0
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(p, _)| p == "/jmap/api")
    );
}

#[tokio::test]
async fn waits_when_busy_and_refuses_bad_credentials_and_addresses() {
    let fake = FakeJmap::start(false).await;
    fake.0.lock().unwrap().busy = 2;
    assert_eq!(fake.source().collections().await.unwrap().len(), 1);
    let port = fake.0.lock().unwrap().port;
    let wrong = JmapCollections::new(
        &format!("http://127.0.0.1:{port}/jmap/session"),
        Credentials::Password {
            username: "ada".into(),
            password: "wrong".into(),
        },
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(matches!(
        wrong.collections().await,
        Err(CollectionError::Auth(_))
    ));
    assert!(matches!(
        JmapCollections::new(
            "http://jmap.example.com/session",
            Credentials::Bearer(TOKEN.into()),
            Duration::from_secs(1)
        ),
        Err(CollectionError::Config(_))
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
    let url = format!("https://localhost:{}/jmap/session", server.port());
    let source = || {
        JmapCollections::new(
            &url,
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
