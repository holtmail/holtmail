//! The directory against an in-process fake of the SimpleLogin API.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use holtmail_alias::testing::conformance;
use holtmail_alias::{AliasDirectory, AliasError, NewAlias};
use holtmail_alias_simplelogin::SimpleLoginDirectory;
use holtmail_core::TlsRoots;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Default)]
struct Fake {
    aliases: Vec<Value>,
    next: i64,
}

type Shared = Arc<Mutex<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authentication").and_then(|v| v.to_str().ok()) == Some("sl-key")
}

fn denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error": "Wrong api key"})),
    )
        .into_response()
}

fn mailbox(id: i64) -> Value {
    json!({"id": id, "email": if id == 1 { "owner@example.org" } else { "other@example.org" }})
}

fn new_alias(fake: &mut Fake, email: String, note: Value, name: Value, boxes: Vec<i64>) -> Value {
    fake.next += 1;
    let alias = json!({
        "id": fake.next, "email": email, "name": name, "enabled": true, "note": note,
        "creation_timestamp": 1_741_000_000 + fake.next, "nb_forward": 0, "nb_block": 0,
        "nb_reply": 0, "pinned": false,
        "mailboxes": boxes.into_iter().map(mailbox).collect::<Vec<_>>(),
    });
    fake.aliases.push(alias.clone());
    alias
}

async fn list(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let Some(page) = query.get("page_id").and_then(|p| p.parse::<usize>().ok()) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "page_id must be provided"})),
        )
            .into_response();
    };
    let fake = fake.lock().await;
    let aliases: Vec<_> = fake
        .aliases
        .iter()
        .rev()
        .skip(page * 20)
        .take(20)
        .cloned()
        .collect();
    axum::Json(json!({ "aliases": aliases })).into_response()
}

async fn one(State(fake): State<Shared>, headers: HeaderMap, Path(id): Path<i64>) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    match fake.lock().await.aliases.iter().find(|a| a["id"] == id) {
        Some(alias) => axum::Json(alias.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "Unknown error"})),
        )
            .into_response(),
    }
}

async fn patch(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let Some(alias) = fake.aliases.iter_mut().find(|a| a["id"] == id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    for key in ["note", "name"] {
        if let Some(value) = body.get(key) {
            alias[key] = value.clone();
        }
    }
    if let Some(ids) = body.get("mailbox_ids").and_then(|v| v.as_array()) {
        alias["mailboxes"] = json!(
            ids.iter()
                .filter_map(|i| i.as_i64())
                .map(mailbox)
                .collect::<Vec<_>>()
        );
    }
    axum::Json(json!({"ok": true})).into_response()
}

async fn remove(State(fake): State<Shared>, headers: HeaderMap, Path(id): Path<i64>) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let before = fake.aliases.len();
    fake.aliases.retain(|a| a["id"] != id);
    if fake.aliases.len() == before {
        return StatusCode::NOT_FOUND.into_response();
    }
    axum::Json(json!({"deleted": true})).into_response()
}

async fn toggle(State(fake): State<Shared>, headers: HeaderMap, Path(id): Path<i64>) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let Some(alias) = fake.aliases.iter_mut().find(|a| a["id"] == id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let enabled = !alias["enabled"].as_bool().unwrap();
    alias["enabled"] = json!(enabled);
    axum::Json(json!({ "enabled": enabled })).into_response()
}

async fn options(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    axum::Json(json!({
        "can_create": true,
        "prefix_suggestion": "",
        "suffixes": [
            {"suffix": ".x1@alias.example.org", "signed_suffix": "signed:.x1@alias.example.org", "is_custom": false, "is_premium": false},
            {"suffix": "@mine.example.org", "signed_suffix": "signed:@mine.example.org", "is_custom": true, "is_premium": false}
        ]
    }))
    .into_response()
}

async fn mailboxes(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    axum::Json(json!({"mailboxes": [
        {"id": 2, "email": "other@example.org", "default": false, "verified": true, "nb_alias": 0, "creation_timestamp": 1},
        {"id": 3, "email": "pending@example.org", "default": false, "verified": false, "nb_alias": 0, "creation_timestamp": 1},
        {"id": 1, "email": "owner@example.org", "default": true, "verified": true, "nb_alias": 0, "creation_timestamp": 1}
    ]}))
    .into_response()
}

async fn custom(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let prefix = body["alias_prefix"].as_str().unwrap_or_default();
    let Some(suffix) = body["signed_suffix"]
        .as_str()
        .and_then(|s| s.strip_prefix("signed:"))
    else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "Tampered suffix"})),
        )
            .into_response();
    };
    let boxes: Vec<i64> = body["mailbox_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    if boxes.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "mailbox_ids required"})),
        )
            .into_response();
    }
    let email = format!("{prefix}{suffix}");
    let mut fake = fake.lock().await;
    if fake.aliases.iter().any(|a| a["email"] == email.as_str()) {
        return (
            StatusCode::CONFLICT,
            axum::Json(json!({"error": format!("{email} already exists")})),
        )
            .into_response();
    }
    let alias = new_alias(
        &mut fake,
        email,
        body["note"].clone(),
        body.get("name").cloned().unwrap_or(Value::Null),
        boxes,
    );
    (StatusCode::CREATED, axum::Json(alias)).into_response()
}

async fn random(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let email = format!("random_{}@alias.example.org", fake.next + 1);
    let alias = new_alias(&mut fake, email, body["note"].clone(), Value::Null, vec![1]);
    (StatusCode::CREATED, axum::Json(alias)).into_response()
}

async fn start(fake: Shared) -> String {
    let router = Router::new()
        .route("/api/v2/aliases", get(list))
        .route("/api/aliases/{id}", get(one).patch(patch).delete(remove))
        .route("/api/aliases/{id}/toggle", post(toggle))
        .route("/api/v5/alias/options", get(options))
        .route("/api/v2/mailboxes", get(mailboxes))
        .route("/api/v3/alias/custom/new", post(custom))
        .route("/api/alias/random/new", post(random))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn keeps_the_contract() {
    let fake = Shared::default();
    let base = start(fake.clone()).await;
    let directory = SimpleLoginDirectory::new(&base, "sl-key", &TlsRoots::default()).unwrap();
    conformance::directory(&directory).await;
}

#[tokio::test]
async fn reads_every_page_and_the_default_mailbox_first() {
    let fake = Shared::default();
    {
        let mut f = fake.lock().await;
        for i in 0..45 {
            new_alias(
                &mut f,
                format!("a{i}@alias.example.org"),
                Value::Null,
                Value::Null,
                vec![1],
            );
        }
    }
    let base = start(fake.clone()).await;
    let directory = SimpleLoginDirectory::new(&base, "sl-key", &TlsRoots::default()).unwrap();
    let aliases = directory.aliases().await.unwrap();
    assert_eq!(aliases.len(), 45);
    let options = directory.options().await.unwrap();
    assert_eq!(
        options
            .mailboxes
            .iter()
            .map(|m| m.email.as_str())
            .collect::<Vec<_>>(),
        ["owner@example.org", "other@example.org"]
    );
    assert_eq!(options.domains[0].label, ".x1@alias.example.org");

    let made = directory
        .create(&NewAlias {
            prefix: Some("shop".into()),
            domain: Some(options.domains[1].id.clone()),
            name: Some("Ada".into()),
            mailbox_ids: vec!["2".into()],
            ..NewAlias::default()
        })
        .await
        .unwrap();
    assert_eq!(made.address, "shop@mine.example.org");
    assert_eq!(made.name.as_deref(), Some("Ada"));
    assert_eq!(made.mailboxes[0].email, "other@example.org");
    let taken = directory
        .create(&NewAlias {
            prefix: Some("shop".into()),
            domain: Some(options.domains[1].id.clone()),
            ..NewAlias::default()
        })
        .await;
    assert_eq!(
        taken,
        Err(AliasError::Invalid(
            "shop@mine.example.org already exists".into()
        ))
    );
}

#[tokio::test]
async fn a_wrong_key_is_auth() {
    let base = start(Shared::default()).await;
    let directory = SimpleLoginDirectory::new(&base, "wrong", &TlsRoots::default()).unwrap();
    assert_eq!(directory.options().await, Err(AliasError::Auth));
    assert_eq!(directory.aliases().await, Err(AliasError::Auth));
}
