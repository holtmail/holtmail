use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use holtmail_source::testing::{
    MailRemote, MailboxProbe, Probed, WriteFolders, watch_conformance, writer_conformance,
};
use holtmail_source::{Flag, MailboxWriter};
use serde_json::Map;

use super::*;

const PASSWORD: &str = "correct horse";
const TOKEN: &str = "fmu1-example-token";

#[derive(Clone)]
struct Email {
    id: String,
    blob: String,
    mailboxes: Vec<String>,
    keywords: Vec<String>,
    received: String,
    raw: Vec<u8>,
}

#[derive(Default)]
struct Fake {
    port: u16,
    mailboxes: Vec<(String, String, Option<String>, Option<String>)>,
    emails: Vec<Email>,
    max_objects: usize,
    max_calls: usize,
    requests: Vec<Vec<(String, usize)>>,
    downloads: usize,
    lose_blob: Option<String>,
    push: Option<tokio::sync::broadcast::Sender<String>>,
    no_events: bool,
    silent: bool,
    event_queries: Vec<String>,
    uploads: HashMap<String, Vec<u8>>,
    created: u32,
    sets: Vec<Value>,
    interfere: usize,
    instance: Option<String>,
}

impl Fake {
    fn state(&self) -> String {
        let mut text = String::new();
        for e in &self.emails {
            text.push_str(&format!("{}|{:?}|{:?};", e.id, e.mailboxes, e.keywords));
        }
        format!("s{:x}", fnv(&text))
    }
}

type Shared = Arc<Mutex<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("ada@example.com:{PASSWORD}"))
    );
    let bearer = format!("Bearer {TOKEN}");
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == basic || v == bearer)
}

async fn session(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let fake = fake.lock().unwrap();
    let base = format!("http://127.0.0.1:{}", fake.port);
    let mut session = json!({
        "capabilities": {
            CORE: { "maxObjectsInGet": fake.max_objects, "maxCallsInRequest": fake.max_calls },
            MAIL: {},
        },
        "accounts": { "u1": { "name": "ada@example.com" } },
        "primaryAccounts": { MAIL: "u1", CORE: "u1" },
        "username": "ada@example.com",
        "apiUrl": format!("{base}/jmap/api/"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
        "uploadUrl": format!("{base}/jmap/upload/{{accountId}}/"),
        "state": "1",
    });
    if let Some(token) = &fake.instance {
        session["capabilities"]["urn:holtmail:params:jmap:instance"] = json!({ "token": token });
    }
    if !fake.no_events {
        session["eventSourceUrl"] = json!(format!(
            "{base}/jmap/events/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"
        ));
    }
    axum::Json(session).into_response()
}

async fn events(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let query = query.unwrap_or_default();
    let ping: u64 = query
        .split('&')
        .find_map(|p| p.strip_prefix("ping="))
        .and_then(|p| p.parse().ok())
        .unwrap_or(60);
    let (receiver, silent) = {
        let mut fake = fake.lock().unwrap();
        fake.event_queries.push(query);
        (fake.push.as_ref().unwrap().subscribe(), fake.silent)
    };
    let stream = futures::stream::unfold(receiver, move |mut receiver| async move {
        let text = tokio::select! {
            message = receiver.recv() => message.ok()?,
            _ = tokio::time::sleep(std::time::Duration::from_secs(ping)), if !silent => {
                format!("event: ping\ndata: {{\"interval\":{ping}}}\n\n")
            }
        };
        Some((Ok::<_, std::io::Error>(text), receiver))
    });
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

fn state_change(account: &str, kind: &str, state: &str) -> String {
    format!(
        "event: state\r\ndata: {}\r\n\r\n",
        json!({ "@type": "StateChange", "changed": { account: { kind: state } } })
    )
}

fn email_json(email: &Email, properties: &[String]) -> Value {
    let mut out = json!({ "id": email.id });
    for property in properties {
        let value = match property.as_str() {
            "blobId" => json!(email.blob),
            "receivedAt" => json!(email.received),
            "mailboxIds" => Value::Object(
                email
                    .mailboxes
                    .iter()
                    .map(|m| (m.clone(), json!(true)))
                    .collect(),
            ),
            "keywords" => Value::Object(
                email
                    .keywords
                    .iter()
                    .map(|k| (k.clone(), json!(true)))
                    .collect(),
            ),
            _ => continue,
        };
        out[property] = value;
    }
    out
}

async fn api(State(fake): State<Shared>, headers: HeaderMap, body: String) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let request: Value = serde_json::from_str(&body).unwrap();
    let calls = request["methodCalls"].as_array().unwrap().clone();
    let mut fake = fake.lock().unwrap();
    if calls.len() > fake.max_calls {
        return (StatusCode::BAD_REQUEST, "too many calls").into_response();
    }
    let mut results: Vec<Value> = Vec::new();
    let mut seen = Vec::new();
    for call in &calls {
        let name = call[0].as_str().unwrap().to_owned();
        let args = &call[1];
        let id = call[2].clone();
        assert_eq!(args["accountId"], "u1");
        let answer = match name.as_str() {
            "Mailbox/get" => {
                seen.push((name.clone(), 0));
                let list: Vec<Value> = fake
                    .mailboxes
                    .iter()
                    .map(|(id, name, parent, role)| {
                        let total = fake.emails.iter().filter(|e| e.mailboxes.contains(id)).count();
                        json!({ "id": id, "name": name, "parentId": parent, "role": role, "totalEmails": total })
                    })
                    .collect();
                json!(["Mailbox/get", { "accountId": "u1", "state": "1", "list": list, "notFound": [] }, id])
            }
            "Email/query" => {
                let mailbox = args["filter"]["inMailbox"].as_str().unwrap();
                let bound = |key: &str| {
                    args["filter"]
                        .get(key)
                        .and_then(Value::as_str)
                        .map(|t| time::parse_utc(t).unwrap())
                };
                let (after, before) = (bound("after"), bound("before"));
                let mut found: Vec<&Email> = fake
                    .emails
                    .iter()
                    .filter(|e| e.mailboxes.iter().any(|m| m == mailbox))
                    .filter(|e| {
                        let at = time::parse_utc(&e.received).unwrap();
                        after.is_none_or(|a| at >= a) && before.is_none_or(|b| at < b)
                    })
                    .collect();
                found.sort_by(|a, b| a.received.cmp(&b.received).then(a.id.cmp(&b.id)));
                let position = args["position"].as_u64().unwrap_or(0) as usize;
                let limit = (args["limit"].as_u64().unwrap() as usize).min(fake.max_objects);
                let ids: Vec<&str> = found
                    .iter()
                    .skip(position)
                    .take(limit)
                    .map(|e| e.id.as_str())
                    .collect();
                seen.push((name.clone(), ids.len()));
                json!(["Email/query", { "accountId": "u1", "ids": ids, "position": position, "total": found.len() }, id])
            }
            "Email/get" => {
                let ids: Vec<String> = if let Some(reference) = args.get("#ids") {
                    let index: usize = reference["resultOf"].as_str().unwrap().parse().unwrap();
                    results[index][1]["ids"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_owned())
                        .collect()
                } else {
                    args["ids"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_owned())
                        .collect()
                };
                seen.push((name.clone(), ids.len()));
                if ids.len() > fake.max_objects {
                    json!(["error", { "type": "requestTooLarge" }, id])
                } else {
                    let properties: Vec<String> = args["properties"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_owned())
                        .collect();
                    let list: Vec<Value> = ids
                        .iter()
                        .filter_map(|id| fake.emails.iter().find(|e| &e.id == id))
                        .map(|e| email_json(e, &properties))
                        .collect();
                    json!(["Email/get", { "accountId": "u1", "state": fake.state(), "list": list, "notFound": [] }, id])
                }
            }
            "Email/set" => {
                seen.push((name.clone(), 0));
                fake.sets.push(args.clone());
                email_set(&mut fake, args, id)
            }
            "Email/import" => {
                seen.push((name.clone(), 0));
                email_import(&mut fake, args, id)
            }
            _ => json!(["error", { "type": "unknownMethod" }, id]),
        };
        results.push(answer);
    }
    fake.requests.push(seen);
    axum::Json(json!({ "methodResponses": results, "sessionState": "1" })).into_response()
}

fn email_set(fake: &mut Fake, args: &Value, id: Value) -> Value {
    if fake.interfere > 0 && args.get("ifInState").is_some() {
        fake.interfere -= 1;
        let email = fake.emails.iter_mut().find(|e| e.id == "M0006").unwrap();
        if email.keywords.iter().any(|k| k == "$flagged") {
            email.keywords.retain(|k| k != "$flagged");
        } else {
            email.keywords.push("$flagged".into());
        }
    }
    let old = fake.state();
    if let Some(expected) = args.get("ifInState").and_then(Value::as_str)
        && expected != old
    {
        return json!(["error", { "type": "stateMismatch" }, id]);
    }
    let mut updated = Map::new();
    let mut not_updated = Map::new();
    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (email_id, patch) in update {
            let known: Vec<String> = fake.mailboxes.iter().map(|m| m.0.clone()).collect();
            let Some(email) = fake.emails.iter_mut().find(|e| &e.id == email_id) else {
                not_updated.insert(email_id.clone(), json!({ "type": "notFound" }));
                continue;
            };
            let mut next = email.clone();
            for (path, value) in patch.as_object().unwrap() {
                let (property, key) = path.split_once('/').unwrap();
                let list = match property {
                    "keywords" => {
                        assert_eq!(key, key.to_ascii_lowercase(), "keywords go lowercase");
                        &mut next.keywords
                    }
                    "mailboxIds" => {
                        assert!(known.iter().any(|k| k == key), "an unknown mailbox");
                        &mut next.mailboxes
                    }
                    other => panic!("patch of {other}"),
                };
                list.retain(|k| k != key);
                if value == &json!(true) {
                    list.push(key.to_owned());
                }
            }
            if next.mailboxes.is_empty() {
                not_updated.insert(email_id.clone(), json!({ "type": "invalidProperties" }));
                continue;
            }
            *email = next;
            updated.insert(email_id.clone(), Value::Null);
        }
    }
    let mut destroyed = Vec::new();
    let mut not_destroyed = Map::new();
    for email_id in args
        .get("destroy")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let before = fake.emails.len();
        fake.emails
            .retain(|e| Some(e.id.as_str()) != email_id.as_str());
        if fake.emails.len() < before {
            destroyed.push(email_id.clone());
        } else {
            not_destroyed.insert(
                email_id.as_str().unwrap().to_owned(),
                json!({ "type": "notFound" }),
            );
        }
    }
    json!(["Email/set", {
        "accountId": "u1",
        "oldState": old,
        "newState": fake.state(),
        "updated": updated,
        "notUpdated": not_updated,
        "destroyed": destroyed,
        "notDestroyed": not_destroyed,
    }, id])
}

fn email_import(fake: &mut Fake, args: &Value, id: Value) -> Value {
    let mut created = Map::new();
    for (key, spec) in args["emails"].as_object().unwrap() {
        let raw = fake.uploads[spec["blobId"].as_str().unwrap()].clone();
        fake.created += 1;
        let email = Email {
            id: format!("I{:04}", fake.created),
            blob: spec["blobId"].as_str().unwrap().to_owned(),
            mailboxes: spec["mailboxIds"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect(),
            keywords: spec["keywords"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect(),
            received: spec["receivedAt"].as_str().unwrap().to_owned(),
            raw,
        };
        created.insert(
            key.clone(),
            json!({ "id": email.id, "blobId": email.blob, "threadId": "T1", "size": email.raw.len() }),
        );
        fake.emails.push(email);
    }
    json!(["Email/import", { "accountId": "u1", "newState": fake.state(), "created": created }, id])
}

async fn upload(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(account): Path<String>,
    body: Bytes,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    assert_eq!(account, "u1");
    let mut fake = fake.lock().unwrap();
    let blob = format!("U{:04}", fake.uploads.len() + 1);
    let size = body.len();
    fake.uploads.insert(blob.clone(), body.to_vec());
    axum::Json(json!({ "accountId": "u1", "blobId": blob, "type": "message/rfc822", "size": size }))
        .into_response()
}

async fn download(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path((account, blob, name)): Path<(String, String, String)>,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    assert_eq!((account.as_str(), name.as_str()), ("u1", "message.eml"));
    let mut fake = fake.lock().unwrap();
    fake.downloads += 1;
    if fake.lose_blob.as_deref() == Some(&blob) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match fake.emails.iter().find(|e| e.blob == blob) {
        Some(email) => email.raw.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mail(n: u32) -> Vec<u8> {
    format!("From: ada@example.com\r\nSubject: Note {n}\r\nMessage-ID: <{n}@example.com>\r\n\r\nBody {n}\r\n").into_bytes()
}

fn email(n: u32, mailboxes: &[&str], keywords: &[&str], received: &str) -> Email {
    Email {
        id: format!("M{n:04}"),
        blob: format!("B{n:04}"),
        mailboxes: mailboxes.iter().map(|m| m.to_string()).collect(),
        keywords: keywords.iter().map(|k| k.to_string()).collect(),
        received: received.into(),
        raw: mail(n),
    }
}

async fn serve(max_objects: usize, max_calls: usize) -> (Shared, String) {
    let fake: Shared = Arc::new(Mutex::new(Fake {
        max_objects,
        max_calls,
        mailboxes: vec![
            ("mb-in".into(), "Inbox".into(), None, Some("inbox".into())),
            (
                "mb-sent".into(),
                "Sent Items".into(),
                None,
                Some("sent".into()),
            ),
            ("mb-work".into(), "Work".into(), None, None),
            (
                "mb-2025".into(),
                "2025".into(),
                Some("mb-work".into()),
                None,
            ),
            (
                "mb-arch".into(),
                "Archive".into(),
                None,
                Some("archive".into()),
            ),
        ],
        emails: vec![
            email(1, &["mb-in"], &["$seen"], "2025-03-01T09:00:00Z"),
            email(
                2,
                &["mb-in", "mb-2025"],
                &["$seen", "$flagged", "$forwarded"],
                "2025-03-02T09:00:00Z",
            ),
            email(3, &["mb-in"], &[], "2025-03-01T08:00:00Z"),
            email(4, &["mb-in"], &["$answered"], "2025-03-03T09:00:00Z"),
            email(5, &["mb-in"], &[], "2025-03-04T09:00:00Z"),
            email(6, &["mb-sent"], &["$seen"], "2025-03-05T09:00:00Z"),
        ],
        push: Some(tokio::sync::broadcast::channel(16).0),
        ..Fake::default()
    }));
    let app = Router::new()
        .route(
            "/.well-known/jmap",
            get(|| async { Redirect::temporary("/jmap/session") }),
        )
        .route("/jmap/session", get(session))
        .route("/jmap/api/", post(api))
        .route("/jmap/download/{account}/{blob}/{name}", get(download))
        .route("/jmap/upload/{account}/", post(upload))
        .route("/jmap/events/", get(events))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    fake.lock().unwrap().port = port;
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("http://127.0.0.1:{port}"))
}

fn password_source(url: &str) -> JmapSource {
    JmapSource::new(
        JmapSettings {
            url: url.into(),
            username: "ada@example.com".into(),
            auth: JmapAuth::Password,
        },
        Credentials::Password(PASSWORD.into()),
    )
    .unwrap()
}

#[tokio::test]
async fn refuses_its_own_instance_and_reads_another() {
    let (fake, base) = serve(2, 2).await;
    fake.lock().unwrap().instance = Some("own-token".into());
    let refused = password_source(&base)
        .refusing_instance("own-token")
        .connect()
        .await;
    assert!(matches!(refused, Err(SourceError::Config(_))));
    let watched = password_source(&base)
        .refusing_instance("own-token")
        .watch(&[])
        .await;
    assert!(matches!(watched, Err(SourceError::Config(_))));
    let mut other = password_source(&base)
        .refusing_instance("other-token")
        .connect()
        .await
        .unwrap();
    assert!(!other.folders().await.unwrap().is_empty());
}

#[tokio::test]
async fn lists_mailboxes_as_folders_with_roles() {
    let (_fake, base) = serve(2, 2).await;
    let mut session = password_source(&base).connect().await.unwrap();
    let folders = session.folders().await.unwrap();
    let paths: Vec<(&str, Option<FolderRole>)> =
        folders.iter().map(|f| (f.path.as_str(), f.role)).collect();
    assert_eq!(
        paths,
        [
            ("Archive", Some(FolderRole::Archive)),
            ("Inbox", Some(FolderRole::Inbox)),
            ("Sent Items", Some(FolderRole::Sent)),
            ("Work", None),
            ("Work/2025", None),
        ]
    );
    assert!(folders.iter().all(|f| f.selectable && !f.label));
}

#[tokio::test]
async fn fetches_original_bytes_in_batches_the_server_allows() {
    let (fake, base) = serve(2, 2).await;
    let mut session = password_source(&format!("{base}/jmap/session"))
        .connect()
        .await
        .unwrap();
    let status = session.select("Inbox").await.unwrap();
    assert!(!status.uids_ascend);
    assert_eq!((status.exists, status.highest_modseq), (5, None));
    let uids = session.uids().await.unwrap();
    assert_eq!(uids.len(), 5);
    assert!(uids.windows(2).all(|w| w[0] < w[1]));

    let messages = session.fetch(&uids).await.unwrap();
    let subjects: Vec<String> = messages
        .iter()
        .map(|m| {
            String::from_utf8_lossy(&m.raw)
                .lines()
                .nth(1)
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        subjects,
        [
            "Subject: Note 3",
            "Subject: Note 1",
            "Subject: Note 2",
            "Subject: Note 4",
            "Subject: Note 5"
        ],
        "UIDs do not follow the received time"
    );
    assert_eq!(messages[2].raw, Bytes::from(mail(2)));
    assert_eq!(messages[2].flags, ["$forwarded", "\\Flagged", "\\Seen"]);
    assert_eq!(messages[3].flags, ["\\Answered"]);
    assert_eq!(
        messages[1].received_at,
        Some(holtmail_core::Timestamp::from_secs(1_740_819_600))
    );
    assert!(messages.iter().all(|m| m.labels.is_none()));

    let fake = fake.lock().unwrap();
    assert_eq!(fake.downloads, 5);
    for request in &fake.requests {
        assert!(request.len() <= 2, "more calls than maxCallsInRequest");
        assert!(
            request.iter().all(|(_, n)| *n <= 2),
            "more objects than maxObjectsInGet"
        );
    }
}

#[tokio::test]
async fn gives_an_email_the_same_uid_in_every_mailbox_and_session() {
    let (_fake, base) = serve(500, 1).await;
    let source = password_source(&base);
    let mut first = source.connect().await.unwrap();
    let inbox = first.select("Inbox").await.unwrap();
    let inbox_uids = first.uids().await.unwrap();
    let work = first.select("Work/2025").await.unwrap();
    let work_uids = first.uids().await.unwrap();
    assert_ne!(inbox.uid_validity, work.uid_validity);
    assert_eq!(work_uids.len(), 1);
    assert!(inbox_uids.contains(&work_uids[0]));

    let mut second = source.connect().await.unwrap();
    assert_eq!(second.select("Inbox").await.unwrap(), inbox);
    assert_eq!(second.uids().await.unwrap(), inbox_uids);
}

#[tokio::test]
async fn reads_flags_and_leaves_out_mail_that_vanished() {
    let (fake, base) = serve(3, 4).await;
    let mut session = password_source(&base).connect().await.unwrap();
    session.select("Inbox").await.unwrap();
    let uids = session.uids().await.unwrap();

    let flags = session.flags(uids[1]..=uids[3], None).await.unwrap();
    assert_eq!(
        flags.iter().map(|f| f.uid).collect::<Vec<_>>(),
        uids[1..=3].to_vec()
    );
    assert_eq!(flags[0].flags, ["\\Seen"]);
    assert!(matches!(
        session.flags(uids[0]..=uids[4], Some(1)).await,
        Err(SourceError::Protocol(_))
    ));

    {
        let mut fake = fake.lock().unwrap();
        fake.lose_blob = Some("B0004".into());
        fake.emails.retain(|e| e.id != "M0005");
    }
    let messages = session.fetch(&uids).await.unwrap();
    assert_eq!(messages.len(), 3);
    assert!(session.fetch(&[7]).await.unwrap().is_empty());
}

#[tokio::test]
async fn signs_in_with_a_token_and_reports_refusals() {
    let (_fake, base) = serve(10, 4).await;
    let token = JmapSource::new(
        JmapSettings {
            url: base.clone(),
            username: String::new(),
            auth: JmapAuth::Token,
        },
        Credentials::AccessToken(TOKEN.into()),
    )
    .unwrap();
    assert_eq!(
        token
            .connect()
            .await
            .unwrap()
            .folders()
            .await
            .unwrap()
            .len(),
        5
    );

    let wrong = JmapSource::new(
        JmapSettings {
            url: base.clone(),
            username: "ada@example.com".into(),
            auth: JmapAuth::Password,
        },
        Credentials::Password("wrong".into()),
    )
    .unwrap();
    assert!(matches!(wrong.connect().await, Err(SourceError::Auth(_))));

    let missing = password_source(&format!("{base}/nothing/here"));
    assert!(matches!(
        missing.connect().await,
        Err(SourceError::Config(_))
    ));

    assert!(matches!(
        JmapSource::new(
            JmapSettings {
                url: base,
                username: " ".into(),
                auth: JmapAuth::Password,
            },
            Credentials::Password(PASSWORD.into()),
        ),
        Err(SourceError::Config(_))
    ));
}

#[test]
fn turns_what_people_type_into_a_session_address() {
    assert_eq!(
        session_url("jmap.example.com").unwrap(),
        "https://jmap.example.com/.well-known/jmap"
    );
    assert_eq!(
        session_url(" https://api.example.com/jmap/session ").unwrap(),
        "https://api.example.com/jmap/session"
    );
    assert_eq!(
        session_url("http://127.0.0.1:8080/").unwrap(),
        "http://127.0.0.1:8080/.well-known/jmap"
    );
    for refused in [
        "",
        "http://jmap.example.com",
        "ftp://jmap.example.com",
        "https://ada:pw@jmap.example.com",
    ] {
        assert!(
            matches!(session_url(refused), Err(SourceError::Config(_))),
            "{refused:?} was accepted"
        );
    }
}

#[test]
fn derives_uids_that_follow_the_received_time() {
    let early = holtmail_core::Timestamp::from_secs(1_700_000_000);
    let late = holtmail_core::Timestamp::from_secs(1_700_000_001);
    assert!(uid("Mzzzz", Some(early)) < uid("Maaaa", Some(late)));
    assert_eq!(uid("M1", Some(early)), uid("M1", Some(early)));
    assert!(
        uid(
            "M1",
            Some(holtmail_core::Timestamp::from_secs(i64::MAX / 1000))
        ) < 1 << 62
    );
    assert_eq!(
        expand(
            "/d/{accountId}/{blobId}/{name}?t={type}",
            &[
                ("accountId", "u 1"),
                ("blobId", "B/1"),
                ("name", "m.eml"),
                ("type", "message/rfc822")
            ]
        ),
        "/d/u%201/B%2F1/m.eml?t=message%2Frfc822"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trusts_extra_roots_only_when_given() {
    let ca = holtmail_core::tls::testing::TestCa::new(&["localhost"]);
    let server = ca.serve_https(401, "{}").await;
    let source = || {
        JmapSource::new(
            JmapSettings {
                url: format!("https://localhost:{}/jmap/session", server.port()),
                username: String::new(),
                auth: JmapAuth::Token,
            },
            Credentials::AccessToken("token".into()),
        )
        .unwrap()
    };
    let trusted = source().with_roots(&ca.roots()).unwrap();
    assert!(matches!(trusted.connect().await, Err(SourceError::Auth(_))));
    assert!(matches!(
        source().connect().await,
        Err(SourceError::Connection(_))
    ));
    assert!(matches!(
        source().with_roots(&holtmail_core::TlsRoots::from_der(vec![vec![1, 2, 3]])),
        Err(SourceError::Config(_))
    ));
}

async fn redirector(target: String) -> (String, Arc<Mutex<Vec<(String, bool)>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().fallback(move |uri: axum::http::Uri, headers: HeaderMap| {
        let seen = seen.clone();
        let target = target.clone();
        async move {
            seen.lock()
                .unwrap()
                .push((uri.path().to_owned(), headers.contains_key("authorization")));
            Redirect::temporary(&target)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}"), asked)
}

#[tokio::test]
async fn follows_a_well_known_redirect_to_another_host_without_credentials() {
    let (_fake, base) = serve(2, 2).await;
    let (elsewhere, asked) = redirector(format!("{base}/jmap/session")).await;
    let mut session = password_source(&elsewhere).connect().await.unwrap();
    assert_eq!(session.folders().await.unwrap().len(), 5);
    assert_eq!(
        *asked.lock().unwrap(),
        [("/.well-known/jmap".to_owned(), false)]
    );
}

#[tokio::test]
async fn does_not_follow_a_session_redirect_with_credentials() {
    let (_fake, base) = serve(2, 2).await;
    let (target, target_asked) = redirector(format!("{base}/jmap/session")).await;
    let (typed, typed_asked) = redirector(format!("{target}/jmap/session")).await;
    let refused = password_source(&format!("{typed}/jmap/session"))
        .connect()
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&refused, SourceError::Protocol(r) if r.contains("not followed with credentials")),
        "{refused:?}"
    );
    assert_eq!(
        *typed_asked.lock().unwrap(),
        [("/jmap/session".to_owned(), true)]
    );
    assert!(target_asked.lock().unwrap().is_empty());
}

struct Remote(Shared);

#[async_trait]
impl MailRemote for Remote {
    async fn deliver(&self, path: &str) {
        let mut fake = self.0.lock().unwrap();
        let mailbox = fake
            .mailboxes
            .iter()
            .find(|m| m.1 == path)
            .map(|m| m.0.clone())
            .unwrap();
        let n = 100 + fake.emails.len() as u32;
        fake.emails
            .push(email(n, &[&mailbox], &[], "2025-04-01T09:00:00Z"));
        let state = fake.state();
        let _ = fake
            .push
            .as_ref()
            .unwrap()
            .send(state_change("u1", "Email", &state));
    }
}

#[tokio::test]
async fn the_event_source_keeps_the_watch_contract() {
    let (fake, url) = serve(50, 4).await;
    let source = password_source(&url);
    watch_conformance(&source, &Remote(fake.clone()), "Inbox", "Archive").await;
    let query = fake.lock().unwrap().event_queries[0].clone();
    assert_eq!(query, "types=Email%2CMailbox&closeafter=no&ping=60");
}

#[tokio::test]
async fn the_watch_ignores_other_accounts_and_types() {
    let (fake, url) = serve(50, 4).await;
    let source = password_source(&url);
    let mut watch = source.watch(&[]).await.unwrap().unwrap();
    let push = fake.lock().unwrap().push.clone().unwrap();
    push.send(state_change("u2", "Email", "x")).unwrap();
    push.send(state_change("u1", "Thread", "x")).unwrap();
    push.send("event: state\ndata: not json\n\n".into())
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), watch.next())
            .await
            .is_err(),
        "nothing relevant changed"
    );
    push.send(state_change("u1", "Mailbox", "m2")).unwrap();
    let changed = tokio::time::timeout(std::time::Duration::from_secs(5), watch.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed, holtmail_source::Changed::all());
}

#[tokio::test]
async fn a_silent_event_source_ends_the_watch() {
    let (fake, url) = serve(50, 4).await;
    fake.lock().unwrap().silent = true;
    let source = password_source(&url).with_push_ping(std::time::Duration::from_secs(1));
    let mut watch = source.watch(&[]).await.unwrap().unwrap();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(10), watch.next())
        .await
        .expect("silence ends the watch");
    assert!(matches!(ended, Err(SourceError::Connection(_))));

    fake.lock().unwrap().silent = false;
    let mut watch = source.watch(&[]).await.unwrap().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(3500), watch.next())
            .await
            .is_err(),
        "pings keep a quiet watch alive"
    );
}

#[tokio::test]
async fn a_session_without_an_event_source_is_not_watched() {
    let (fake, url) = serve(50, 4).await;
    fake.lock().unwrap().no_events = true;
    assert!(password_source(&url).watch(&[]).await.unwrap().is_none());
    let wrong = JmapSource::new(
        JmapSettings {
            url: url.clone(),
            username: "ada@example.com".into(),
            auth: JmapAuth::Password,
        },
        Credentials::Password("wrong".into()),
    )
    .unwrap();
    assert!(matches!(wrong.watch(&[]).await, Err(SourceError::Auth(_))));
}

#[tokio::test]
async fn an_unchanged_account_is_told_by_its_email_state() {
    let (fake, url) = serve(50, 4).await;
    let source = password_source(&url);
    let mut session = source.connect().await.unwrap();
    session.select("Inbox").await.unwrap();
    assert_eq!(session.state(), None, "no state before a listing");
    session.uids().await.unwrap();
    let state = session.state().expect("a state after the listing");

    let mut later = source.connect().await.unwrap();
    later.select("Sent Items").await.unwrap();
    assert_eq!(
        later.unchanged_since(&state).await.unwrap(),
        Some(state.clone())
    );

    fake.lock().unwrap().emails[0]
        .keywords
        .push("$flagged".into());
    let mut after = source.connect().await.unwrap();
    after.select("Inbox").await.unwrap();
    assert_eq!(
        after.unchanged_since(&state).await.unwrap(),
        None,
        "a flag moves the state"
    );
}

struct Probe(Shared);

#[async_trait]
impl MailboxProbe for Probe {
    async fn deliver(&self, path: &str, raw: &[u8]) -> u64 {
        let mut fake = self.0.lock().unwrap();
        let mailbox = fake
            .mailboxes
            .iter()
            .find(|m| path == folder_path(&fake.mailboxes, &m.0))
            .map(|m| m.0.clone())
            .unwrap();
        fake.created += 1;
        let n = fake.created;
        let received = time::format_utc(holtmail_core::Timestamp::from_secs(
            1_793_000_000 + i64::from(n) * 7,
        ));
        let email = Email {
            id: format!("D{n:04}"),
            blob: format!("DB{n:04}"),
            mailboxes: vec![mailbox],
            keywords: Vec::new(),
            received: received.clone(),
            raw: raw.to_vec(),
        };
        let uid = uid(&email.id, time::parse_utc(&received));
        fake.emails.push(email);
        uid
    }

    async fn messages(&self, path: &str) -> Vec<Probed> {
        let fake = self.0.lock().unwrap();
        let mut out: Vec<Probed> = fake
            .emails
            .iter()
            .filter(|e| {
                e.mailboxes
                    .iter()
                    .any(|m| folder_path(&fake.mailboxes, m) == path)
            })
            .map(|e| Probed {
                uid: uid(&e.id, time::parse_utc(&e.received)),
                flags: flags(&email_json(e, &["keywords".to_owned()])),
                labels: None,
                modseq: None,
                raw: Bytes::from(e.raw.clone()),
            })
            .collect();
        out.sort_by_key(|m| m.uid);
        out
    }
}

fn folder_path(mailboxes: &[(String, String, Option<String>, Option<String>)], id: &str) -> String {
    let by_id: HashMap<String, (String, Option<String>)> = mailboxes
        .iter()
        .map(|(id, name, parent, _)| (id.clone(), (name.clone(), parent.clone())))
        .collect();
    path(id, &by_id).unwrap()
}

#[tokio::test]
async fn the_writer_keeps_the_writer_contract() {
    for (max_objects, max_calls) in [(500, 16), (1, 1)] {
        let (fake, url) = serve(max_objects, max_calls).await;
        let writer = password_source(&url).writer();
        writer_conformance(
            &writer,
            &Probe(fake.clone()),
            WriteFolders {
                folder: "Work/2025",
                target: "Archive",
                all: None,
            },
        )
        .await;
        let fake = fake.lock().unwrap();
        assert!(
            fake.sets
                .iter()
                .any(|s| s.get("ifInState").is_some() && s.get("destroy").is_some()),
            "deletes are guarded by the state they were planned at"
        );
    }
}

#[tokio::test]
async fn a_moved_or_imported_message_is_found_under_its_uid_by_sync() {
    let (fake, url) = serve(50, 4).await;
    let source = password_source(&url);
    let writer = source.writer();
    let mut session = source.connect().await.unwrap();
    session.select("Inbox").await.unwrap();
    let inbox = session.uids().await.unwrap();

    let moved = writer
        .move_to("Inbox", &inbox[..2], "Archive")
        .await
        .unwrap();
    assert_eq!(
        moved.uids,
        inbox[..2]
            .iter()
            .map(|u| (*u, Some(*u)))
            .collect::<Vec<_>>()
    );
    session.select("Archive").await.unwrap();
    assert_eq!(session.uids().await.unwrap(), inbox[..2].to_vec());

    let raw = mail(77);
    let date = holtmail_core::Timestamp::from_secs(1_793_000_123);
    let appended = writer
        .append("Archive", &raw, &[Flag::Seen], date)
        .await
        .unwrap()
        .unwrap();
    session.select("Archive").await.unwrap();
    assert!(session.uids().await.unwrap().contains(&appended));
    let fetched = session.fetch(&[appended]).await.unwrap();
    assert_eq!(fetched[0].raw, Bytes::from(raw));
    assert_eq!(fetched[0].flags, ["\\Seen"]);
    assert_eq!(fetched[0].received_at, Some(date));
    assert!(fake.lock().unwrap().uploads.len() == 1);
}

#[tokio::test]
async fn deleting_from_one_mailbox_keeps_the_email_in_the_others() {
    let (fake, url) = serve(50, 4).await;
    let source = password_source(&url);
    let mut session = source.connect().await.unwrap();
    session.select("Work/2025").await.unwrap();
    let shared = session.uids().await.unwrap();
    assert_eq!(shared.len(), 1);

    source
        .writer()
        .delete("Work/2025", &shared, true)
        .await
        .unwrap();
    let fake = fake.lock().unwrap();
    let email = fake.emails.iter().find(|e| e.id == "M0002").unwrap();
    assert_eq!(email.mailboxes, ["mb-in"], "still in the Inbox");
    assert!(fake.sets.iter().all(|s| s.get("destroy").is_none()));
}

#[tokio::test]
async fn a_change_planned_on_an_old_state_is_planned_again() {
    let (fake, url) = serve(50, 4).await;
    let source = password_source(&url);
    let mut session = source.connect().await.unwrap();
    session.select("Inbox").await.unwrap();
    let target = session.uids().await.unwrap()[0];

    fake.lock().unwrap().interfere = 2;
    let moved = source
        .writer()
        .move_to("Inbox", &[target], "Archive")
        .await
        .unwrap();
    assert_eq!(moved.uids, [(target, Some(target))]);
    assert_eq!(
        fake.lock().unwrap().sets.len(),
        3,
        "two refused, one applied"
    );
    session.select("Archive").await.unwrap();
    assert_eq!(session.uids().await.unwrap(), vec![target]);

    fake.lock().unwrap().interfere = usize::MAX;
    assert!(matches!(
        source.writer().delete("Archive", &[target], true).await,
        Err(SourceError::Connection(_))
    ));
    assert!(
        fake.lock()
            .unwrap()
            .emails
            .iter()
            .any(|e| e.mailboxes == ["mb-arch"])
    );
}

#[tokio::test]
async fn refuses_what_jmap_cannot_do_before_asking_the_server() {
    let (fake, url) = serve(50, 4).await;
    let writer = password_source(&url).writer();
    let before = fake.lock().unwrap().requests.len();
    assert!(matches!(
        writer
            .set_flags("Inbox", &[1], &[Flag::Seen], &[], Some(3))
            .await,
        Err(SourceError::Unsupported(_))
    ));
    assert!(matches!(
        writer
            .set_flags(
                "Inbox",
                &[1],
                &[Flag::Keyword("\\Deleted".into())],
                &[],
                None
            )
            .await,
        Err(SourceError::Unsupported(_))
    ));
    assert!(matches!(
        writer.set_labels(&[1], &["Work".into()], &[]).await,
        Err(SourceError::Unsupported(_))
    ));
    assert!(matches!(
        writer.delete("Inbox", &[1], false).await,
        Err(SourceError::Unsupported(_))
    ));
    assert_eq!(fake.lock().unwrap().requests.len(), before);
}
