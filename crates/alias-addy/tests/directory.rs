//! The directory against an in-process fake of the addy.io API.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use holtmail_alias::testing::conformance;
use holtmail_alias::{AliasDirectory, AliasError};
use holtmail_alias_addy::AddyDirectory;
use holtmail_core::TlsRoots;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Default)]
struct Fake {
    aliases: Vec<Value>,
    next: u32,
}

type Shared = Arc<Mutex<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer addy-key")
        && headers.get("x-requested-with").is_some()
}

fn denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"message": "Unauthenticated."})),
    )
        .into_response()
}

fn recipient(id: &str) -> Value {
    let email = if id == "r-1" {
        "owner@example.org"
    } else {
        "other@example.org"
    };
    json!({"id": id, "email": email, "email_verified_at": "2025-01-01 00:00:00"})
}

fn make(fake: &mut Fake, email: String, description: Value, recipients: Vec<String>) -> Value {
    fake.next += 1;
    let alias = json!({
        "id": format!("00000000-0000-4000-8000-{:012}", fake.next),
        "email": email, "active": true, "description": description, "from_name": null,
        "emails_forwarded": 1, "emails_blocked": 0, "emails_replied": 0,
        "created_at": "2025-03-03 18:00:00", "deleted_at": null,
        "recipients": recipients.iter().map(|r| recipient(r)).collect::<Vec<_>>(),
    });
    fake.aliases.push(alias.clone());
    alias
}

async fn list(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let page: usize = query
        .get("page[number]")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let size: usize = query
        .get("page[size]")
        .and_then(|p| p.parse().ok())
        .unwrap_or(100);
    let fake = fake.lock().await;
    let live: Vec<_> = fake
        .aliases
        .iter()
        .filter(|a| a["deleted_at"].is_null())
        .cloned()
        .collect();
    let last = live.len().div_ceil(size).max(1);
    let data: Vec<_> = live
        .into_iter()
        .skip((page - 1) * size)
        .take(size)
        .collect();
    axum::Json(json!({"data": data, "meta": {"current_page": page, "last_page": last}}))
        .into_response()
}

fn find<'a>(fake: &'a mut Fake, id: &str) -> Option<&'a mut Value> {
    fake.aliases
        .iter_mut()
        .find(|a| a["id"] == id && a["deleted_at"].is_null())
}

async fn one(State(fake): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    match find(&mut *fake.lock().await, &id) {
        Some(alias) => axum::Json(json!({"data": alias})).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn patch(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let Some(alias) = find(&mut fake, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    for key in ["description", "from_name"] {
        if let Some(value) = body.get(key) {
            alias[key] = value.clone();
        }
    }
    axum::Json(json!({"data": alias})).into_response()
}

async fn remove(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    match find(&mut fake, &id) {
        Some(alias) => {
            alias["deleted_at"] = json!("2025-03-04 00:00:00");
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn activate(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    match find(&mut fake, body["id"].as_str().unwrap_or_default()) {
        Some(alias) => {
            alias["active"] = json!(true);
            axum::Json(json!({"data": alias})).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn deactivate(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    match find(&mut fake, &id) {
        Some(alias) => {
            alias["active"] = json!(false);
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn recipients_of(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let Some(alias) = find(&mut fake, body["alias_id"].as_str().unwrap_or_default()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    alias["recipients"] = json!(
        body["recipient_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| recipient(r.as_str().unwrap()))
            .collect::<Vec<_>>()
    );
    axum::Json(json!({"data": alias})).into_response()
}

async fn create(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let domain = body["domain"].as_str().unwrap_or_default().to_owned();
    if domain != "ada.addy.example.org" && domain != "addy.example.org" {
        return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({"message": "The selected domain is invalid.", "errors": {"domain": ["The selected domain is invalid."]}}))).into_response();
    }
    let mut fake = fake.lock().await;
    let local = match body["format"].as_str() {
        Some("custom") => body["local_part"].as_str().unwrap_or_default().to_owned(),
        _ => format!("r{}", fake.next + 1),
    };
    let email = format!("{local}@{domain}");
    if fake.aliases.iter().any(|a| a["email"] == email.as_str()) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({"message": "The local part has already been taken."})),
        )
            .into_response();
    }
    let recipients: Vec<String> = body
        .get("recipient_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_else(|| vec!["r-1".into()]);
    let alias = make(&mut fake, email, body["description"].clone(), recipients);
    (StatusCode::CREATED, axum::Json(json!({"data": alias}))).into_response()
}

async fn domain_options(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    axum::Json(json!({"data": ["addy.example.org", "ada.addy.example.org"], "sharedDomains": [], "defaultAliasDomain": "ada.addy.example.org", "defaultAliasFormat": "random_characters"})).into_response()
}

async fn recipients(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    axum::Json(json!({"data": [recipient("r-1"), recipient("r-2"), {"id": "r-3", "email": "pending@example.org", "email_verified_at": null}]})).into_response()
}

async fn start(fake: Shared) -> String {
    let router = Router::new()
        .route("/api/v1/aliases", get(list).post(create))
        .route("/api/v1/aliases/{id}", get(one).patch(patch).delete(remove))
        .route("/api/v1/active-aliases", post(activate))
        .route("/api/v1/active-aliases/{id}", delete(deactivate))
        .route("/api/v1/alias-recipients", post(recipients_of))
        .route("/api/v1/domain-options", get(domain_options))
        .route("/api/v1/recipients", get(recipients))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn keeps_the_contract() {
    let base = start(Shared::default()).await;
    let directory = AddyDirectory::new(&base, "addy-key", &TlsRoots::default()).unwrap();
    conformance::directory(&directory).await;
}

#[tokio::test]
async fn reads_every_page_and_puts_the_default_domain_first() {
    let fake = Shared::default();
    {
        let mut f = fake.lock().await;
        for i in 0..230 {
            make(
                &mut f,
                format!("a{i}@addy.example.org"),
                Value::Null,
                vec!["r-1".into()],
            );
        }
    }
    let base = start(fake).await;
    let directory = AddyDirectory::new(&base, "addy-key", &TlsRoots::default()).unwrap();
    let aliases = directory.aliases().await.unwrap();
    assert_eq!(aliases.len(), 230);
    assert_eq!(
        aliases[0].created_at.unwrap().as_millis(),
        1_741_024_800_000
    );
    let options = directory.options().await.unwrap();
    assert_eq!(options.domains[0].label, "@ada.addy.example.org");
    assert_eq!(options.mailboxes.len(), 2);
}

#[tokio::test]
async fn a_wrong_key_is_auth() {
    let base = start(Shared::default()).await;
    let directory = AddyDirectory::new(&base, "wrong", &TlsRoots::default()).unwrap();
    assert_eq!(directory.options().await, Err(AliasError::Auth));
}
