use std::collections::HashMap as Map;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;

use super::*;

const TOKEN: &str = "graph-access-token";

struct FakeFolder {
    id: String,
    name: String,
    parent: Option<String>,
    well_known: Option<String>,
    hidden: bool,
    search: bool,
}

#[derive(Clone)]
struct FakeMessage {
    id: String,
    folder: String,
    received: String,
    read: bool,
    flagged: bool,
    draft: bool,
    raw: Vec<u8>,
}

#[derive(Default)]
struct Fake {
    port: u16,
    page: usize,
    folders: Vec<FakeFolder>,
    messages: Vec<FakeMessage>,
    versions: Map<String, u64>,
    throttle: u32,
    throttled: u32,
    requests: Vec<String>,
}

type Shared = Arc<Mutex<Fake>>;

impl Fake {
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}/v1.0", self.port)
    }

    fn bump(&mut self, folder: &str) {
        *self.versions.entry(folder.to_owned()).or_default() += 1;
    }

    fn folder_json(&self, folder: &FakeFolder) -> Value {
        let children = self
            .folders
            .iter()
            .filter(|f| f.parent.as_deref() == Some(folder.id.as_str()))
            .count();
        let total = self
            .messages
            .iter()
            .filter(|m| m.folder == folder.id)
            .count();
        let mut out = json!({
            "id": folder.id,
            "displayName": folder.name,
            "childFolderCount": children,
            "totalItemCount": total,
        });
        if folder.search {
            out["@odata.type"] = json!("#microsoft.graph.mailSearchFolder");
        }
        out
    }
}

fn gate(fake: &Shared, headers: &HeaderMap, path: &str) -> Option<Response> {
    let mut fake = fake.lock().unwrap();
    fake.requests.push(path.to_owned());
    let bearer = format!("Bearer {TOKEN}");
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(bearer.as_str()) {
        return Some(StatusCode::UNAUTHORIZED.into_response());
    }
    let prefer = headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(prefer.contains("IdType=\"ImmutableId\""), "{prefer}");
    if fake.throttled < fake.throttle {
        fake.throttled += 1;
        return Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "0")],
                "slow down",
            )
                .into_response(),
        );
    }
    None
}

fn paged(fake: &Fake, items: Vec<Value>, offset: usize, next: impl Fn(usize) -> String) -> Value {
    let page = fake.page.max(1);
    let slice: Vec<Value> = items.iter().skip(offset).take(page).cloned().collect();
    let mut out = json!({ "value": slice });
    if offset + page < items.len() {
        out["@odata.nextLink"] = json!(next(offset + page));
    }
    out
}

fn skip(query: &Map<String, String>) -> usize {
    query
        .get("$skip")
        .or_else(|| query.get("$skiptoken"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

async fn me(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(refused) = gate(&fake, &headers, "/me") {
        return refused;
    }
    axum::Json(json!({ "mail": null, "userPrincipalName": "ada@example.com" })).into_response()
}

async fn folders(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<Map<String, String>>,
) -> Response {
    children(fake, headers, None, query)
}

async fn child_folders(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(parent): Path<String>,
    Query(query): Query<Map<String, String>>,
) -> Response {
    children(fake, headers, Some(parent), query)
}

fn children(
    fake: Shared,
    headers: HeaderMap,
    parent: Option<String>,
    query: Map<String, String>,
) -> Response {
    if let Some(refused) = gate(&fake, &headers, "folders") {
        return refused;
    }
    let fake = fake.lock().unwrap();
    let hidden = query.get("includeHiddenFolders").map(String::as_str) == Some("true");
    let items: Vec<Value> = fake
        .folders
        .iter()
        .filter(|f| f.parent == parent && (hidden || !f.hidden))
        .map(|f| fake.folder_json(f))
        .collect();
    let base = fake.base();
    let path = match &parent {
        Some(parent) => format!("{base}/me/mailFolders/{parent}/childFolders"),
        None => format!("{base}/me/mailFolders"),
    };
    let hidden = if hidden {
        "&includeHiddenFolders=true"
    } else {
        ""
    };
    axum::Json(paged(&fake, items, skip(&query), |n| {
        format!("{path}?%24skip={n}{hidden}")
    }))
    .into_response()
}

async fn folder(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(refused) = gate(&fake, &headers, "folder") {
        return refused;
    }
    let fake = fake.lock().unwrap();
    match fake
        .folders
        .iter()
        .find(|f| f.id == id || f.well_known.as_deref() == Some(id.as_str()))
    {
        Some(folder) => axum::Json(fake.folder_json(folder)).into_response(),
        None => (StatusCode::NOT_FOUND, "ErrorItemNotFound").into_response(),
    }
}

async fn delta(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(folder): Path<String>,
    Query(query): Query<Map<String, String>>,
) -> Response {
    if let Some(refused) = gate(&fake, &headers, "delta") {
        return refused;
    }
    let fake = fake.lock().unwrap();
    let version = fake.versions.get(&folder).copied().unwrap_or(0);
    let base = format!("{}/me/mailFolders/{folder}/messages/delta", fake.base());
    let delta_link = format!("{base}?%24deltatoken={version}");
    if let Some(token) = query.get("$deltatoken") {
        if token == "expired" {
            return (StatusCode::GONE, "syncStateNotFound").into_response();
        }
        if *token == version.to_string() {
            return axum::Json(json!({ "value": [], "@odata.deltaLink": delta_link }))
                .into_response();
        }
    } else if !query.contains_key("$skiptoken") {
        assert_eq!(
            query.get("$select").map(String::as_str),
            Some(LIST_FIELDS),
            "the listing asked for more than it needs"
        );
    }
    let mut items: Vec<Value> = fake
        .messages
        .iter()
        .filter(|m| m.folder == folder)
        .map(|m| {
            json!({
                "id": m.id,
                "receivedDateTime": m.received,
                "isRead": m.read,
                "isDraft": m.draft,
                "flag": { "flagStatus": if m.flagged { "flagged" } else { "notFlagged" } },
            })
        })
        .collect();
    items.push(json!({ "id": "gone-before", "@removed": { "reason": "deleted" } }));
    let mut answer = paged(&fake, items, skip(&query), |n| {
        format!("{base}?%24skiptoken={n}")
    });
    if answer.get("@odata.nextLink").is_none() {
        answer["@odata.deltaLink"] = json!(delta_link);
    }
    axum::Json(answer).into_response()
}

async fn value(State(fake): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Some(refused) = gate(&fake, &headers, "value") {
        return refused;
    }
    let fake = fake.lock().unwrap();
    match fake.messages.iter().find(|m| m.id == id) {
        Some(m) => ([("content-type", "message/rfc822")], m.raw.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

struct Server {
    fake: Shared,
    settings: GraphSettings,
}

fn mail(n: u32) -> Vec<u8> {
    format!(
        "From: ada@example.com\r\nTo: bob@example.org\r\nSubject: Note {n}\r\nMessage-ID: <{n}@example.com>\r\n\r\nBody {n}\r\n"
    )
    .into_bytes()
}

fn folder_of(id: &str, name: &str, parent: Option<&str>, well_known: Option<&str>) -> FakeFolder {
    FakeFolder {
        id: id.into(),
        name: name.into(),
        parent: parent.map(Into::into),
        well_known: well_known.map(Into::into),
        hidden: false,
        search: false,
    }
}

async fn server() -> Server {
    let fake: Shared = Arc::new(Mutex::new(Fake {
        page: 2,
        ..Fake::default()
    }));
    {
        let mut f = fake.lock().unwrap();
        f.folders
            .push(folder_of("AAMkInbox", "Inbox", None, Some("inbox")));
        f.folders
            .push(folder_of("AAMkProj", "Projects", Some("AAMkInbox"), None));
        f.folders
            .push(folder_of("AAMk2025", "2025", Some("AAMkProj"), None));
        f.folders
            .push(folder_of("AAMkSent", "Sent Items", None, Some("sentitems")));
        f.folders
            .push(folder_of("AAMkJunk", "Junk Email", None, Some("junkemail")));
        let mut history = folder_of("AAMkHist", "Conversation History", None, None);
        history.hidden = true;
        f.folders.push(history);
        let mut search = folder_of("AAMkFind", "Unread Mail", None, None);
        search.search = true;
        f.folders.push(search);
        for (n, (id, folder, received, read, flagged)) in [
            ("m-1/a", "AAMkInbox", "2025-03-01T09:30:00Z", true, false),
            ("m-2", "AAMkInbox", "2025-02-01T09:30:00Z", false, true),
            ("m-3", "AAMkInbox", "2025-04-01T09:30:00Z", false, false),
            ("m-4", "AAMkSent", "2025-04-02T09:30:00Z", true, false),
        ]
        .into_iter()
        .enumerate()
        {
            f.messages.push(FakeMessage {
                id: id.into(),
                folder: folder.into(),
                received: received.into(),
                read,
                flagged,
                draft: false,
                raw: mail(n as u32),
            });
        }
    }
    let app = Router::new()
        .route("/v1.0/me", get(me))
        .route("/v1.0/me/mailFolders", get(folders))
        .route("/v1.0/me/mailFolders/{id}", get(folder))
        .route("/v1.0/me/mailFolders/{id}/childFolders", get(child_folders))
        .route("/v1.0/me/mailFolders/{id}/messages/delta", get(delta))
        .route("/v1.0/me/messages/{id}/$value", get(value))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    fake.lock().unwrap().port = port;
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server {
        settings: GraphSettings {
            api_url: Some(format!("http://127.0.0.1:{port}/v1.0")),
        },
        fake,
    }
}

impl Server {
    fn source(&self) -> GraphSource {
        GraphSource::new(&self.settings, Credentials::AccessToken(TOKEN.into())).unwrap()
    }

    fn requests(&self) -> usize {
        self.fake.lock().unwrap().requests.len()
    }
}

#[tokio::test]
async fn lists_folders_with_paths_roles_and_hidden_ones() {
    let s = server().await;
    let mut session = s.source().connect().await.unwrap();
    let folders = session.folders().await.unwrap();
    let paths: Vec<&str> = folders.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "Conversation History",
            "Inbox",
            "Inbox/Projects",
            "Inbox/Projects/2025",
            "Junk Email",
            "Sent Items",
        ]
    );
    let role = |path: &str| folders.iter().find(|f| f.path == path).unwrap().role;
    assert_eq!(role("Inbox"), Some(FolderRole::Inbox));
    assert_eq!(role("Sent Items"), Some(FolderRole::Sent));
    assert_eq!(role("Junk Email"), Some(FolderRole::Junk));
    assert_eq!(role("Inbox/Projects"), None);
    assert!(folders.iter().all(|f| f.selectable && !f.label));
}

#[tokio::test]
async fn reads_messages_as_mime_with_stable_uids_and_flags() {
    let s = server().await;
    let source = s.source();
    let mut session = source.connect().await.unwrap();
    let status = session.select("Inbox").await.unwrap();
    assert!(!status.uids_ascend);
    assert_eq!(status.exists, 3);
    let uids = session.uids().await.unwrap();
    assert_eq!(uids.len(), 3, "the paged listing lost a message");
    assert!(uids.windows(2).all(|w| w[0] < w[1]));

    let messages = session.fetch(&uids).await.unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].raw.as_ref(), mail(1).as_slice(), "oldest first");
    assert_eq!(messages[0].flags, ["\\Flagged"]);
    assert_eq!(messages[1].flags, ["\\Seen"]);
    assert!(messages[2].flags.is_empty());
    assert_eq!(
        messages[1].received_at,
        Some(holtmail_core::Timestamp::from_secs(1_740_821_400))
    );
    let flags = session.flags(uids[0]..=uids[1], None).await.unwrap();
    assert_eq!(flags.len(), 2);
    assert_eq!(flags[0].flags, ["\\Flagged"]);
    assert!(session.flags(1..=2, Some(1)).await.is_err());

    let mut again = source.connect().await.unwrap();
    let same = again.select("Inbox").await.unwrap();
    assert_eq!(same.uid_validity, status.uid_validity);
    assert_eq!(
        again.uids().await.unwrap(),
        uids,
        "UIDs changed between sessions"
    );
    let sent = again.select("Sent Items").await.unwrap();
    assert_ne!(sent.uid_validity, status.uid_validity);

    s.fake.lock().unwrap().messages.retain(|m| m.id != "m-3");
    let mut later = source.connect().await.unwrap();
    later.select("Inbox").await.unwrap();
    later.uids().await.unwrap();
    s.fake.lock().unwrap().messages.retain(|m| m.id != "m-2");
    assert_eq!(
        later.fetch(&uids).await.unwrap().len(),
        1,
        "a message gone since the listing was not left out"
    );
}

#[tokio::test]
async fn keeps_the_delta_link_and_asks_it_for_changes() {
    let s = server().await;
    let mut session = s.source().connect().await.unwrap();
    session.select("Inbox").await.unwrap();
    assert_eq!(session.state(), None);
    session.uids().await.unwrap();
    let state = session.state().expect("no delta link after a listing");
    assert!(state.contains("deltatoken=0"), "{state}");

    let mut next = s.source().connect().await.unwrap();
    next.select("Inbox").await.unwrap();
    let before = s.requests();
    assert_eq!(
        next.unchanged_since(&state).await.unwrap(),
        Some(state.clone())
    );
    assert_eq!(s.requests(), before + 1, "an unchanged folder was listed");

    s.fake.lock().unwrap().bump("AAMkInbox");
    assert_eq!(next.unchanged_since(&state).await.unwrap(), None);
    let expired = state.replace("deltatoken=0", "deltatoken=expired");
    assert_eq!(next.unchanged_since(&expired).await.unwrap(), None);

    let before = s.requests();
    let elsewhere = "https://graph.example.org/v1.0/me/mailFolders/AAMkInbox/messages/delta";
    assert_eq!(next.unchanged_since(elsewhere).await.unwrap(), None);
    assert_eq!(
        s.requests(),
        before,
        "a stored link to another host was followed"
    );
}

#[tokio::test]
async fn waits_out_throttling() {
    let s = server().await;
    s.fake.lock().unwrap().throttle = 2;
    let mut session = s.source().connect().await.unwrap();
    assert_eq!(session.folders().await.unwrap().len(), 6);
    assert_eq!(s.fake.lock().unwrap().throttled, 2);

    {
        let mut fake = s.fake.lock().unwrap();
        fake.throttle = 100;
        fake.throttled = 0;
    }
    let mut busy = s.source().connect().await.unwrap();
    assert!(matches!(
        busy.folders().await,
        Err(SourceError::Connection(_))
    ));
}

#[tokio::test]
async fn refuses_wrong_tokens_passwords_and_plain_http() {
    let s = server().await;
    let wrong = GraphSource::new(&s.settings, Credentials::AccessToken("expired".into())).unwrap();
    let mut session = wrong.connect().await.unwrap();
    assert!(matches!(session.folders().await, Err(SourceError::Auth(_))));
    assert!(matches!(
        GraphSource::new(&s.settings, Credentials::Password("secret".into())),
        Err(SourceError::Config(_))
    ));
    let public = GraphSettings {
        api_url: Some("http://graph.example.org/v1.0".into()),
    };
    assert!(matches!(
        GraphSource::new(&public, Credentials::AccessToken(TOKEN.into())),
        Err(SourceError::Config(_))
    ));
    assert_eq!(
        GraphSettings::from_json(&json!({ "oauth": { "provider": "microsoft" } })).unwrap(),
        GraphSettings::default()
    );
}

#[tokio::test]
async fn names_the_signed_in_address() {
    let s = server().await;
    assert_eq!(
        s.source().address().await.unwrap().as_deref(),
        Some("ada@example.com")
    );
}

#[test]
fn derives_uids_from_receive_time_and_id() {
    let early = uid("m-1", Some(holtmail_core::Timestamp::from_secs(100)));
    let late = uid("m-0", Some(holtmail_core::Timestamp::from_secs(101)));
    assert!(early < late);
    assert_eq!(
        early,
        uid("m-1", Some(holtmail_core::Timestamp::from_secs(100)))
    );
    assert!(
        uid(
            "x",
            Some(holtmail_core::Timestamp::from_secs(i64::MAX / 1000))
        ) < 1 << 62
    );
}
