//! The directory against an in-process fake of a JMAP server with Masked Email.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use holtmail_alias::testing::conformance;
use holtmail_alias::{AliasDirectory, AliasError, NewAlias};
use holtmail_alias_fastmail::FastmailDirectory;
use holtmail_core::TlsRoots;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Default)]
struct Fake {
    masked: Vec<Value>,
    next: u32,
    scope: bool,
}

type Shared = Arc<Mutex<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer fm-token")
}

async fn session(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut primary = json!({"urn:ietf:params:jmap:mail": "u1"});
    if fake.lock().await.scope {
        primary["https://www.fastmail.com/dev/maskedemail"] = json!("u1");
    }
    axum::Json(json!({"apiUrl": "/jmap/api/", "primaryAccounts": primary})).into_response()
}

async fn api(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let call = &request["methodCalls"][0];
    let (method, args) = (call[0].as_str().unwrap(), &call[1]);
    assert_eq!(args["accountId"], "u1");
    let mut fake = fake.lock().await;
    let result = match method {
        "MaskedEmail/get" => {
            let list: Vec<Value> = match args["ids"].as_array() {
                None => fake.masked.clone(),
                Some(ids) => fake
                    .masked
                    .iter()
                    .filter(|m| ids.contains(&m["id"]))
                    .cloned()
                    .collect(),
            };
            json!(["MaskedEmail/get", {"accountId": "u1", "list": list, "notFound": []}, "0"])
        }
        "MaskedEmail/set" => {
            let mut answer = json!({"accountId": "u1"});
            if let Some(create) = args["create"].as_object() {
                for (key, object) in create {
                    fake.next += 1;
                    let local = object["emailPrefix"]
                        .as_str()
                        .map(|p| format!("{p}.{}", fake.next))
                        .unwrap_or_else(|| format!("word{}", fake.next));
                    let id = format!("masked-{}", fake.next);
                    fake.masked.push(json!({
                        "id": id, "email": format!("{local}@masked.example.org"),
                        "state": object["state"].as_str().unwrap_or("pending"),
                        "forDomain": object.get("forDomain").cloned().unwrap_or(json!("")),
                        "description": object["description"], "createdAt": "2025-03-03T18:00:00Z",
                        "lastMessageAt": null, "url": null, "createdBy": "holtmail",
                    }));
                    answer["created"] = json!({ key.clone(): {"id": id} });
                }
            }
            if let Some(update) = args["update"].as_object() {
                for (id, patch) in update {
                    match fake.masked.iter_mut().find(|m| m["id"] == id.as_str()) {
                        Some(masked) => {
                            for (k, v) in patch.as_object().unwrap() {
                                masked[k] = v.clone();
                            }
                            answer["updated"] = json!({ id.clone(): null });
                        }
                        None => answer["notUpdated"] = json!({ id.clone(): {"type": "notFound"} }),
                    }
                }
            }
            json!(["MaskedEmail/set", answer, "0"])
        }
        _ => json!(["error", {"type": "unknownMethod"}, "0"]),
    };
    axum::Json(json!({"methodResponses": [result], "sessionState": "s1"})).into_response()
}

async fn start(fake: Shared) -> String {
    let router = Router::new()
        .route("/jmap/session", get(session))
        .route("/jmap/api/", post(api))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}/jmap/session")
}

fn with_scope() -> Shared {
    Arc::new(Mutex::new(Fake {
        scope: true,
        ..Fake::default()
    }))
}

#[tokio::test]
async fn keeps_the_contract() {
    let url = start(with_scope()).await;
    let directory = FastmailDirectory::new(&url, "fm-token", &TlsRoots::default()).unwrap();
    conformance::directory(&directory).await;
}

#[tokio::test]
async fn deleted_masks_are_not_listed_and_prefixes_are_checked() {
    let fake = with_scope();
    let url = start(fake.clone()).await;
    let directory = FastmailDirectory::new(&url, "fm-token", &TlsRoots::default()).unwrap();
    let made = directory
        .create(&NewAlias {
            prefix: Some("shop".into()),
            for_domain: Some("https://shop.example.com".into()),
            ..NewAlias::default()
        })
        .await
        .unwrap();
    assert_eq!(made.address, "shop.1@masked.example.org");
    assert_eq!(made.for_domain.as_deref(), Some("https://shop.example.com"));
    fake.lock().await.masked[0]["state"] = json!("deleted");
    assert!(directory.aliases().await.unwrap().is_empty());
    assert!(matches!(
        directory
            .create(&NewAlias {
                prefix: Some("Not Allowed!".into()),
                ..NewAlias::default()
            })
            .await,
        Err(AliasError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_token_without_the_scope_says_so() {
    let url = start(Shared::default()).await;
    let directory = FastmailDirectory::new(&url, "fm-token", &TlsRoots::default()).unwrap();
    assert!(
        matches!(directory.options().await, Err(AliasError::Invalid(m)) if m.contains("Masked Email"))
    );
    let wrong = FastmailDirectory::new(&url, "wrong", &TlsRoots::default()).unwrap();
    assert_eq!(wrong.aliases().await, Err(AliasError::Auth));
}
