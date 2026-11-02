use std::sync::Arc;

use std::net::SocketAddr;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use bytes::Bytes;
use holtmail_auth::{HashCost, Options, Signup};
use holtmail_blob_fs::FsBlobStore;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{Ingest, Placement};
use holtmail_store::{AccountKind, FolderRole, InstanceRole, NewAccount};
use holtmail_store_sqlite::SqliteStore;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;

const NEWSLETTER: &[u8] = b"From: News <news@example.com>\r\nTo: ada@example.com\r\nSubject: Weekly news\r\nMessage-ID: <n1@example.com>\r\nDate: Mon, 10 Mar 2025 08:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=r\r\n\r\n--r\r\nContent-Type: text/html\r\n\r\n<p onclick=\"x()\">Hello <img src=\"cid:logo@example.com\"><img src=\"https://tracker.example.com/p.gif\"></p>\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <logo@example.com>\r\nContent-Disposition: inline\r\n\r\nPNGDATA\r\n--r\r\nContent-Type: text/html; name=\"evil.html\"\r\nContent-Disposition: attachment; filename=\"evil.html\"\r\n\r\n<script>alert(1)</script>\r\n--r--\r\n";
const PLAIN: &[u8] = b"From: Bob <bob@example.org>\r\nTo: ada@example.com\r\nSubject: Re: Weekly news\r\nIn-Reply-To: <n1@example.com>\r\nDate: Tue, 11 Mar 2025 08:00:00 +0000\r\n\r\nThanks <3\r\n";

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    cookie: String,
    archive: String,
    folder: String,
    _dir: tempfile::TempDir,
}

const PASSWORD: &str = "analytical engine";

fn auth(store: Arc<dyn Store>) -> Arc<Auth> {
    Arc::new(
        Auth::new(
            store,
            Options {
                max_failures: 3,
                hash_cost: HashCost {
                    memory_kib: 64,
                    iterations: 1,
                    lanes: 1,
                },
                ..Options::default()
            },
        )
        .unwrap(),
    )
}

async fn fixture() -> Fixture {
    let mut f = empty_fixture().await;
    f.auth
        .create_user(
            Signup {
                email: "ada@example.com".into(),
                name: "Ada Lovelace".into(),
                password: PASSWORD.into(),
            },
            InstanceRole::Owner,
        )
        .await
        .unwrap();
    let (status, headers, _) = f
        .send(post_json(
            "/api/login",
            json!({ "email": "ada@example.com", "password": PASSWORD }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    f.cookie = session_cookie(&headers).unwrap();
    f
}

async fn empty_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let blobs: Arc<dyn BlobStore> =
        Arc::new(FsBlobStore::open(dir.path().join("b")).await.unwrap());
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
    let archive = store.create_archive("Personal").await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Import".into(),
            address: None,
            settings: Default::default(),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    let folder = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    let ingest = Ingest::new(blobs.clone(), store.clone(), index.clone());
    for (uid, raw, flags) in [
        (
            1,
            NEWSLETTER,
            vec!["\\Seen".to_owned(), "\\Flagged".to_owned()],
        ),
        (2, PLAIN, Vec::new()),
    ] {
        ingest
            .ingest(
                archive,
                Bytes::from_static(raw),
                &Placement {
                    folder: Some(folder),
                    uid: Some(uid),
                    flags,
                    ..Placement::default()
                },
            )
            .await
            .unwrap();
    }
    ingest.commit().await.unwrap();
    let auth = auth(store.clone());
    Fixture {
        app: router(ApiState {
            blobs,
            store,
            index,
            auth: auth.clone(),
            secure_cookies: false,
        }),
        auth,
        cookie: String::new(),
        archive: archive.to_string(),
        folder: folder.to_string(),
        _dir: dir,
    }
}

type Answer = (StatusCode, HeaderMap, Bytes);

impl Fixture {
    async fn send(&self, mut request: Request<Body>) -> Answer {
        let headers = request.headers_mut();
        headers.insert(header::HOST, "mail.example.com".parse().unwrap());
        if !self.cookie.is_empty() && !headers.contains_key(header::COOKIE) {
            headers.insert(header::COOKIE, self.cookie.parse().unwrap());
        }
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 7], 50_000))));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        (
            status,
            headers,
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        )
    }

    async fn get(&self, uri: &str) -> Answer {
        self.send(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    async fn json(&self, uri: &str) -> Value {
        let (status, _, body) = self.get(uri).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri}: {}",
            String::from_utf8_lossy(&body)
        );
        serde_json::from_slice(&body).unwrap()
    }
}

fn post_json(uri: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let set = headers.get(header::SET_COOKIE)?.to_str().ok()?;
    let pair = set.split(';').next()?;
    pair.strip_prefix("holtmail_session=")
        .filter(|token| !token.is_empty())
        .map(|_| pair.to_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_archives_folders_and_messages() {
    let f = fixture().await;
    let archives = f.json("/api/archives").await;
    assert_eq!(archives[0]["name"], "Personal");
    assert_eq!(archives[0]["stats"]["messages"], 2);

    let folders = f
        .json(&format!("/api/archives/{}/folders", f.archive))
        .await;
    assert_eq!(folders[0]["role"], "inbox");
    assert_eq!(folders[0]["messages"], 2);
    assert_eq!(folders[0]["unread"], 1);

    let page = f
        .json(&format!(
            "/api/archives/{}/messages?folder={}&limit=1",
            f.archive, f.folder
        ))
        .await;
    assert_eq!(page["items"][0]["subject"], "Re: Weekly news");
    assert_eq!(page["items"][0]["thread_count"], 2);
    assert_eq!(page["items"][0]["flags"], json!([]));
    assert_eq!(page["items"][0]["folder_ids"], json!([f.folder]));
    let next = page["next"].as_str().unwrap();
    let page = f
        .json(&format!(
            "/api/archives/{}/messages?limit=1&after={next}",
            f.archive
        ))
        .await;
    assert_eq!(page["items"][0]["subject"], "Weekly news");
    assert_eq!(page["items"][0]["flags"], json!(["\\Flagged", "\\Seen"]));
    assert!(page["next"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn searches_with_the_query_language() {
    let f = fixture().await;
    let found = f
        .json(&format!(
            "/api/archives/{}/search?q=from:bob%20thanks",
            f.archive
        ))
        .await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["subject"], "Re: Weekly news");

    let (status, _, body) = f
        .get(&format!("/api/archives/{}/search?q=before:soon", f.archive))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("not a date"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_a_thread_and_a_safe_body() {
    let f = fixture().await;
    let found = f
        .json(&format!("/api/archives/{}/search?q=weekly", f.archive))
        .await;
    let thread_id = found["items"][0]["thread_id"].as_str().unwrap().to_owned();
    let thread = f.json(&format!("/api/threads/{thread_id}")).await;
    let messages = thread["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    let newsletter = &messages[0];
    assert_eq!(newsletter["has_html"], true);
    assert_eq!(newsletter["attachments"].as_array().unwrap().len(), 2);
    let id = newsletter["id"].as_str().unwrap();

    let (status, headers, body) = f.get(&format!("/api/messages/{id}/body")).await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8_lossy(&body);
    assert!(
        !body.contains("onclick") && !body.contains("tracker.example.com"),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            "http://mail.example.com/api/messages/{id}/attachments/0"
        )),
        "{body}"
    );
    assert_eq!(headers["x-holtmail-remote-blocked"], "1");

    let (_, headers, bytes) = f.get(&format!("/api/messages/{id}/attachments/0")).await;
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(&bytes[..], b"PNGDATA");
    let (_, headers, _) = f.get(&format!("/api/messages/{id}/attachments/1")).await;
    assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], "sandbox");

    assert_eq!(messages[1]["in_reply_to"], json!(["n1@example.com"]));
    assert_eq!(messages[1]["references"], json!([]));
    assert_eq!(messages[1]["preview"], "Thanks <3");
    let plain = messages[1]["id"].as_str().unwrap();
    let (_, _, body) = f.get(&format!("/api/messages/{plain}/body")).await;
    assert!(String::from_utf8_lossy(&body).contains("Thanks &lt;3"));

    let (_, headers, raw) = f.get(&format!("/api/messages/{plain}/raw")).await;
    assert_eq!(&raw[..], PLAIN);
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("Re_ Weekly news.eml")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_missing_things_with_404() {
    let f = fixture().await;
    for uri in [
        "/api/messages/not-an-id".to_string(),
        format!("/api/messages/{}", holtmail_core::MessageId::new()),
        format!("/api/threads/{}", holtmail_core::ThreadId::new()),
    ] {
        let (status, _, _) = f.get(&uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[test]
fn describes_itself_in_openapi() {
    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    assert!(spec["paths"]["/api/archives/{id}/search"]["get"].is_object());
    assert!(spec["components"]["schemas"]["MessageDetail"].is_object());
    assert!(spec["paths"]["/api/login"]["post"].is_object());
    assert!(spec["components"]["schemas"]["SetupRequest"].is_object());
    assert_eq!(
        spec["components"]["securitySchemes"]["session"]["name"],
        "holtmail_session"
    );
    assert_eq!(spec["security"][0]["session"], json!([]));
}

#[test]
fn declares_every_path_parameter() {
    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    let paths = spec["paths"].as_object().unwrap();
    let mut checked = 0;
    for (path, item) in paths {
        let names: Vec<&str> = path
            .split('/')
            .filter_map(|part| part.strip_prefix('{')?.strip_suffix('}'))
            .collect();
        for operation in item.as_object().unwrap().values() {
            let declared: Vec<&str> = operation["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["in"] == "path")
                .map(|p| p["name"].as_str().unwrap())
                .collect();
            assert_eq!(declared, names, "{path}");
            checked += names.len();
        }
    }
    assert!(checked >= 11);
    let body = &spec["paths"]["/api/messages/{id}/body"]["get"]["responses"]["200"];
    assert!(body["headers"]["x-holtmail-remote-blocked"].is_object());
    let message = &spec["paths"]["/api/messages/{id}"]["get"]["responses"];
    for status in ["401", "404", "500"] {
        assert!(message[status].is_object(), "{status}");
    }
}

fn body_text(body: &Bytes) -> String {
    String::from_utf8_lossy(body).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn sets_up_the_owner_with_the_one_time_token() {
    let mut f = empty_fixture().await;
    assert_eq!(f.json("/api/setup").await, json!({ "needed": true }));
    let (status, _, _) = f.get("/api/archives").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let setup = |token: &str, password: &str| {
        post_json(
            "/api/setup",
            json!({
                "token": token,
                "email": "ada@example.com",
                "name": "Ada Lovelace",
                "password": password,
            }),
        )
    };
    let token = f.auth.setup_token().await.unwrap().unwrap();
    let (status, _, _) = f.send(setup("not-the-token", PASSWORD)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = f.send(setup(&token, "short")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body_text(&body).contains("10 characters"),
        "{}",
        body_text(&body)
    );

    let (status, headers, body) = f.send(setup(&token, PASSWORD)).await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let set_cookie = headers[header::SET_COOKIE].to_str().unwrap();
    for part in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(set_cookie.contains(part), "{set_cookie}");
    }
    assert!(!set_cookie.contains("Secure"), "{set_cookie}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["user"]["role"], "owner");

    f.cookie = session_cookie(&headers).unwrap();
    let session = f.json("/api/session").await;
    assert_eq!(session["user"]["email"], "ada@example.com");
    assert_eq!(session["user"]["name"], "Ada Lovelace");
    assert_eq!(f.json("/api/archives").await[0]["name"], "Personal");
    assert_eq!(f.json("/api/setup").await, json!({ "needed": false }));
    let (status, _, _) = f.send(setup(&token, PASSWORD)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the setup token worked twice"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_wrong_passwords_and_then_limits_attempts() {
    let mut f = fixture().await;
    f.cookie.clear();
    let login = |password: &str| {
        post_json(
            "/api/login",
            json!({ "email": "ada@example.com", "password": password }),
        )
    };
    for _ in 0..3 {
        let (status, headers, body) = f.send(login("wrong password")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.contains_key(header::SET_COOKIE));
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"], "wrong email or password");
    }
    let (status, headers, _) = f.send(login(PASSWORD)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(retry > 0 && retry <= 900, "{retry}");

    let (status, _, _) = f
        .send(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn signs_out() {
    let f = fixture().await;
    let (status, headers, _) = f.send(post_json("/api/logout", json!({}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set_cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(
        set_cookie.starts_with("holtmail_session=;") && set_cookie.contains("Max-Age=0"),
        "{set_cookie}"
    );
    for uri in ["/api/session", "/api/archives"] {
        let (status, _, _) = f.get(uri).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    let (status, _, _) = f.send(post_json("/api/logout", json!({}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "logout is not idempotent");
}

#[tokio::test(flavor = "multi_thread")]
async fn protects_every_other_route() {
    let mut f = fixture().await;
    let message = holtmail_core::MessageId::new();
    let uris = [
        "/api/archives".to_string(),
        format!("/api/archives/{}", f.archive),
        format!("/api/archives/{}/accounts", f.archive),
        format!("/api/archives/{}/folders", f.archive),
        format!("/api/archives/{}/messages", f.archive),
        format!("/api/archives/{}/search?q=weekly", f.archive),
        format!("/api/messages/{message}"),
        format!("/api/messages/{message}/body"),
        format!("/api/messages/{message}/raw"),
        format!("/api/messages/{message}/attachments/0"),
        format!("/api/threads/{}", holtmail_core::ThreadId::new()),
        "/api/no-such-endpoint".to_string(),
    ];
    let (status, _, _) = f.get("/api/no-such-endpoint").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    for cookie in [
        "",
        "holtmail_session=forged",
        "other=1; holtmail_session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        f.cookie = cookie.into();
        for uri in &uris {
            let (status, _, body) = f.get(uri).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} with {cookie:?}");
            let error: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(error["error"], "sign in required");
        }
    }
    let (status, _, _) = f.get("/api/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_cross_origin_writes() {
    let f = fixture().await;
    let login = || {
        post_json(
            "/api/login",
            json!({ "email": "ada@example.com", "password": PASSWORD }),
        )
    };
    let mut request = login();
    request
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example.org".parse().unwrap());
    let (status, _, _) = f.send(request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut request = login();
    request
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let (status, _, _) = f.send(request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut request = login();
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://mail.example.com".parse().unwrap());
    let (status, _, _) = f.send(request).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = f
        .send(
            Request::post("/api/login")
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(
                    json!({ "email": "ada@example.com", "password": PASSWORD }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let (status, _, _) = f
        .send(Request::post("/api/logout").body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (status, _, _) = f.get("/api/session").await;
    assert_eq!(status, StatusCode::OK, "a refused logout signed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn marks_the_cookie_secure_behind_https() {
    let f = fixture().await;
    let mut request = post_json(
        "/api/login",
        json!({ "email": "ada@example.com", "password": PASSWORD }),
    );
    request
        .headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    let (status, headers, _) = f.send(request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .ends_with("; Secure")
    );
}
