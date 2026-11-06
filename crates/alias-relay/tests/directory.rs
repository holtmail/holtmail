//! The directory against an in-process fake of the Firefox Relay API.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use holtmail_alias::testing::conformance;
use holtmail_alias::{AliasChange, AliasDirectory, AliasError, NewAlias};
use holtmail_alias_relay::RelayDirectory;
use holtmail_core::TlsRoots;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Default)]
struct Fake {
    random: Vec<Value>,
    domain: Vec<Value>,
    next: u64,
}

type Shared = Arc<Mutex<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Token relay-key")
}

fn denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"detail": "Invalid token."})),
    )
        .into_response()
}

fn mask(id: u64, address: &str, kind: &str) -> Value {
    json!({
        "id": id, "address": address.split('@').next().unwrap(), "full_address": address,
        "domain": 2, "mask_type": kind, "enabled": true, "description": "", "generated_for": "",
        "used_on": "", "block_list_emails": false, "created_at": "2025-03-03T18:00:00.123456Z",
        "last_modified_at": "2025-03-03T18:00:00Z", "last_used_at": null,
        "num_forwarded": 2, "num_blocked": 0, "num_replied": 0, "num_spam": 0,
    })
}

fn list_of(fake: &mut Fake, domain: bool) -> &mut Vec<Value> {
    if domain {
        &mut fake.domain
    } else {
        &mut fake.random
    }
}

async fn list(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(kind): Path<String>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    axum::Json(list_of(&mut fake, kind == "domainaddresses").clone()).into_response()
}

async fn create(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path(kind): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) || kind != "relayaddresses" {
        return denied();
    }
    let mut fake = fake.lock().await;
    fake.next += 1;
    let mut made = mask(
        fake.next,
        &format!("m{}@mozmail.example.org", fake.next),
        "random",
    );
    for key in ["description", "generated_for", "enabled"] {
        if let Some(value) = body.get(key) {
            made[key] = value.clone();
        }
    }
    fake.random.push(made.clone());
    (StatusCode::CREATED, axum::Json(made)).into_response()
}

async fn one(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, u64)>,
    method: axum::http::Method,
    body: Option<axum::Json<Value>>,
) -> Response {
    if !authorized(&headers) {
        return denied();
    }
    let mut fake = fake.lock().await;
    let list = list_of(&mut fake, kind == "domainaddresses");
    let Some(at) = list.iter().position(|m| m["id"] == id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"detail": "Not found."})),
        )
            .into_response();
    };
    match method {
        axum::http::Method::DELETE => {
            list.remove(at);
            StatusCode::NO_CONTENT.into_response()
        }
        axum::http::Method::PATCH => {
            let body = body.map(|b| b.0).unwrap_or_default();
            for (key, value) in body.as_object().unwrap() {
                if ![
                    "enabled",
                    "description",
                    "generated_for",
                    "used_on",
                    "block_list_emails",
                ]
                .contains(&key.as_str())
                {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({"detail": format!("{key} is read-only")})),
                    )
                        .into_response();
                }
                list[at][key] = value.clone();
            }
            axum::Json(list[at].clone()).into_response()
        }
        _ => axum::Json(list[at].clone()).into_response(),
    }
}

async fn start(fake: Shared) -> String {
    let router = Router::new()
        .route("/api/v1/{kind}/", get(list).post(create))
        .route("/api/v1/{kind}/{id}/", get(one).patch(one).delete(one))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn keeps_the_contract() {
    let base = start(Shared::default()).await;
    let directory = RelayDirectory::new(&base, "relay-key", &TlsRoots::default()).unwrap();
    conformance::directory(&directory).await;
}

#[tokio::test]
async fn lists_both_kinds_with_their_own_ids() {
    let fake = Shared::default();
    {
        let mut f = fake.lock().await;
        f.random.push(mask(7, "abc@mozmail.example.org", "random"));
        let mut custom = mask(7, "shop@ada.mozmail.example.org", "custom");
        custom["used_on"] = json!("shop.example.com,");
        f.domain.push(custom);
    }
    let base = start(fake).await;
    let directory = RelayDirectory::new(&base, "relay-key", &TlsRoots::default()).unwrap();
    let all = directory.aliases().await.unwrap();
    assert_eq!(
        all.iter()
            .map(|a| (a.id.as_str(), a.address.as_str()))
            .collect::<Vec<_>>(),
        [
            ("r:7", "abc@mozmail.example.org"),
            ("d:7", "shop@ada.mozmail.example.org")
        ]
    );
    assert_eq!(all[1].for_domain.as_deref(), Some("shop.example.com"));
    assert_eq!(all[0].created_at.unwrap().as_millis(), 1_741_024_800_123);
    let switched = directory
        .update(
            "d:7",
            &AliasChange {
                enabled: Some(false),
                ..AliasChange::default()
            },
        )
        .await
        .unwrap();
    assert!(!switched.enabled);
    assert_eq!(
        directory
            .create(&NewAlias {
                prefix: Some("shop".into()),
                ..NewAlias::default()
            })
            .await,
        Err(AliasError::Unsupported)
    );
    assert_eq!(directory.delete("x:1").await, Err(AliasError::NotFound));
}

#[tokio::test]
async fn a_wrong_key_is_auth() {
    let base = start(Shared::default()).await;
    let directory = RelayDirectory::new(&base, "wrong", &TlsRoots::default()).unwrap();
    assert_eq!(directory.options().await, Err(AliasError::Auth));
}
