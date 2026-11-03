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

#[derive(Default)]
struct FakeMailboxes {
    store: std::sync::OnceLock<Arc<dyn Store>>,
    uploads: std::path::PathBuf,
    synced: std::sync::Mutex<Vec<holtmail_core::AccountId>>,
    imported: std::sync::Mutex<Vec<(holtmail_core::AccountId, String, Vec<u8>)>>,
}

#[async_trait::async_trait]
impl Mailboxes for FakeMailboxes {
    async fn add_imap(
        &self,
        archive: holtmail_core::ArchiveId,
        account: NewImapAccount,
    ) -> Result<holtmail_store::Account, MailboxError> {
        if account.password != "right" {
            return Err(MailboxError::Invalid("The server refused".into()));
        }
        let store = self.store.get().unwrap();
        let created = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Imap,
                name: account.name.unwrap_or(account.username),
                address: None,
                settings: json!({ "host": account.host }),
                sealed_secret: None,
                enabled: true,
            })
            .await
            .map_err(|e| MailboxError::Backend(e.to_string()))?;
        self.synced.lock().unwrap().push(created.id);
        Ok(created)
    }

    async fn set_password(
        &self,
        account: &holtmail_store::Account,
        password: String,
    ) -> Result<(), MailboxError> {
        if password != "right" {
            return Err(MailboxError::Invalid("The server refused".into()));
        }
        let store = self.store.get().unwrap();
        let mut account = account.clone();
        account.sealed_secret = Some(password.into_bytes());
        store
            .update_account(&account)
            .await
            .map_err(|e| MailboxError::Backend(e.to_string()))
    }

    async fn sync(&self, account: &holtmail_store::Account) -> Result<(), MailboxError> {
        self.synced.lock().unwrap().push(account.id);
        Ok(())
    }

    async fn import(
        &self,
        account: &holtmail_store::Account,
        file: std::path::PathBuf,
    ) -> Result<(), MailboxError> {
        assert!(file.starts_with(&self.uploads));
        let bytes = std::fs::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        let extension = file
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.imported
            .lock()
            .unwrap()
            .push((account.id, extension, bytes));
        Ok(())
    }

    fn upload_dir(&self) -> &std::path::Path {
        &self.uploads
    }

    fn status(&self, account: holtmail_core::AccountId) -> AccountStatus {
        AccountStatus {
            running: self.synced.lock().unwrap().contains(&account),
            ..AccountStatus::default()
        }
    }
}

struct Fixture {
    app: Router,
    mailboxes: Arc<FakeMailboxes>,
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
    let mailboxes = Arc::new(FakeMailboxes {
        uploads: dir.path().join("uploads"),
        ..FakeMailboxes::default()
    });
    let _ = mailboxes.store.set(store.clone());
    Fixture {
        app: router(ApiState {
            blobs,
            store,
            index,
            auth: auth.clone(),
            mailboxes: mailboxes.clone(),
            secure_cookies: false,
        }),
        mailboxes,
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

#[tokio::test(flavor = "multi_thread")]
async fn creates_archives_and_adds_accounts() {
    let f = fixture().await;
    let (status, _, body) = f
        .send(post_json("/api/archives", json!({ "name": "  Work  " })))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let archive: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(archive["name"], "Work");
    let id = archive["id"].as_str().unwrap().to_owned();
    let (status, _, _) = f
        .send(post_json("/api/archives", json!({ "name": " " })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let accounts = format!("/api/archives/{id}/accounts");
    let imap = |password: &str| json!({ "host": "imap.example.com", "username": "ada@example.com", "password": password });
    let (status, _, body) = f.send(post_json(&accounts, imap("wrong"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"],
        "The server refused"
    );
    let (status, _, _) = f
        .send(post_json(
            &accounts,
            json!({ "host": "imap.example.com", "username": "ada", "password": "right", "security": "ssl" }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = f.send(post_json(&accounts, imap("right"))).await;
    assert_eq!(status, StatusCode::CREATED);
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["kind"], "imap");
    assert_eq!(account["status"]["running"], true);
    assert!(!String::from_utf8_lossy(&body).contains("right"));

    let list = f.json(&accounts).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let account_id = account["id"].as_str().unwrap();
    let (status, _, _) = f
        .send(post_json(
            &format!("/api/accounts/{account_id}/sync"),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(f.mailboxes.synced.lock().unwrap().len(), 2);

    let (status, _, _) = f
        .send(post_json(
            "/api/archives/0190d6e4-0000-7000-8000-000000000000/accounts",
            imap("right"),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_uploaded_files() {
    let f = fixture().await;
    let upload = |name: &str, body: &'static [u8]| {
        Request::post(format!("/api/archives/{}/imports?name={name}", f.archive))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(body))
            .unwrap()
    };
    let (status, _, body) = f
        .send(upload("Old%20mail.MBOX", b"From x\nSubject: a\n\nb\n"))
        .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["kind"], "import");
    assert_eq!(account["name"], "Old mail.MBOX");
    let (status, _, again) = f.send(upload("Old%20mail.MBOX", b"more")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        serde_json::from_slice::<Value>(&again).unwrap()["id"],
        account["id"]
    );
    {
        let imported = f.mailboxes.imported.lock().unwrap();
        assert_eq!(imported.len(), 2);
        assert_eq!(imported[0].1, "mbox");
        assert_eq!(imported[0].2, b"From x\nSubject: a\n\nb\n");
    }

    let (status, _, _) = f.send(upload("empty.eml", b"")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(f.mailboxes.imported.lock().unwrap().len(), 2);
    assert_eq!(
        std::fs::read_dir(&f.mailboxes.uploads).unwrap().count(),
        0,
        "a refused upload left its file"
    );

    let (status, _, _) = f
        .send(
            Request::post(format!("/api/archives/{}/imports?name=a.eml", f.archive))
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("x"))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

fn request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

impl Fixture {
    async fn sign_in(&self, email: &str, password: &str) -> String {
        let (status, headers, body) = self
            .send(
                Request::post("/api/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, "x=y")
                    .body(Body::from(
                        json!({ "email": email, "password": password }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        session_cookie(&headers).unwrap()
    }

    async fn send_as(&self, cookie: &str, mut request: Request<Body>) -> Answer {
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        self.send(request).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn members_read_only_granted_archives() {
    let f = fixture().await;
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "Grace@example.org ", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let grace: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(grace["user"]["role"], "member");
    assert_eq!(grace["user"]["email"], "Grace@example.org");
    let grace_id = grace["user"]["id"].as_str().unwrap().to_owned();
    let member = f.sign_in("grace@example.org", "compiler 1952").await;

    let (_, _, body) = f
        .send_as(
            &member,
            Request::get("/api/archives").body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!([]));
    let messages = format!("/api/archives/{}/messages", f.archive);
    let (status, _, _) = f
        .send_as(
            &member,
            Request::get(&messages).body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let page = f.json(&messages).await;
    let message = page["items"][0]["id"].as_str().unwrap().to_owned();
    let thread = page["items"][0]["thread_id"].as_str().unwrap().to_owned();
    for uri in [
        format!("/api/messages/{message}"),
        format!("/api/messages/{message}/raw"),
        format!("/api/threads/{thread}"),
    ] {
        let (status, _, _) = f
            .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }

    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/users/{grace_id}"),
            json!({ "archives": [f.archive] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = f
        .send_as(
            &member,
            Request::get("/api/archives").body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&body)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for uri in [
        messages.clone(),
        format!("/api/messages/{message}"),
        format!("/api/threads/{thread}"),
    ] {
        let (status, _, _) = f
            .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }

    for (method, uri, body) in [
        (
            "POST",
            "/api/archives".to_owned(),
            json!({ "name": "Mine" }),
        ),
        (
            "PATCH",
            format!("/api/archives/{}", f.archive),
            json!({ "name": "Mine" }),
        ),
        ("DELETE", format!("/api/archives/{}", f.archive), json!({})),
        ("GET", "/api/users".to_owned(), json!({})),
        (
            "POST",
            format!("/api/archives/{}/accounts", f.archive),
            json!({}),
        ),
    ] {
        let (status, _, _) = f.send_as(&member, request(method, &uri, body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }

    let (status, _, body) = f
        .send_as(
            &member,
            request("PATCH", "/api/me", json!({ "name": "Grace Hopper" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["user"]["name"],
        "Grace Hopper"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn manages_people_by_role() {
    let f = fixture().await;
    let me = f.json("/api/session").await["user"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "alan@example.org", "name": "Alan", "password": "short", "role": "admin" }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "alan@example.org", "name": "Alan", "password": "enigma machine", "role": "admin" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let alan = serde_json::from_slice::<Value>(&body).unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, _) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "ALAN@example.org", "name": "Alan", "password": "enigma machine" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let admin = f.sign_in("alan@example.org", "enigma machine").await;
    let (status, _, _) = f
        .send_as(
            &admin,
            request(
                "PATCH",
                &format!("/api/users/{me}"),
                json!({ "name": "Not Ada" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .send_as(
            &admin,
            post_json(
                "/api/users",
                json!({ "email": "kurt@example.org", "name": "Kurt", "password": "incompleteness", "role": "owner" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .send_as(
            &admin,
            request("DELETE", &format!("/api/users/{me}"), json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/users/{me}"),
            json!({ "role": "member" }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = f
        .send(request("DELETE", &format!("/api/users/{me}"), json!({})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/users/{alan}"),
            json!({ "password": "a new enigma" }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = f
        .send_as(
            &admin,
            Request::get("/api/session").body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a reset password kept the old session"
    );

    let (status, _, _) = f
        .send(request("DELETE", &format!("/api/users/{alan}"), json!({})))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let users = f.json("/api/users").await;
    assert_eq!(users.as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_your_own_password() {
    let f = fixture().await;
    let (status, _, _) = f
        .send(post_json(
            "/api/me/password",
            json!({ "current": "wrong one!!", "new": "babbage engine" }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, headers, _) = f
        .send(post_json(
            "/api/me/password",
            json!({ "current": PASSWORD, "new": "babbage engine" }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let fresh = session_cookie(&headers).unwrap();
    let (status, _, _) = f.get("/api/session").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old session survived");
    let (status, _, _) = f
        .send_as(
            &fresh,
            Request::get("/api/session").body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    f.sign_in("ada@example.com", "babbage engine").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_and_removes_archives_and_accounts() {
    let f = fixture().await;
    let accounts = format!("/api/archives/{}/accounts", f.archive);
    let (_, _, body) = f
        .send(post_json(
            &accounts,
            json!({ "host": "imap.example.com", "username": "ada", "password": "right" }),
        ))
        .await;
    let imap = serde_json::from_slice::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/accounts/{imap}"),
            json!({ "password": "wrong" }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &format!("/api/accounts/{imap}"),
            json!({ "password": "right", "name": "Work mail", "enabled": false }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["name"], "Work mail");
    assert_eq!(account["enabled"], false);

    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/archives/{}", f.archive),
            json!({ "name": "Home" }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        f.json(&format!("/api/archives/{}", f.archive)).await["name"],
        "Home"
    );

    let (status, _, _) = f
        .send(request(
            "DELETE",
            &format!("/api/archives/{}", f.archive),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "deleted while syncing");
    f.mailboxes.synced.lock().unwrap().clear();

    let (status, _, _) = f
        .send(request(
            "DELETE",
            &format!("/api/accounts/{imap}"),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let search = format!("/api/archives/{}/search?q=weekly", f.archive);
    assert_eq!(f.json(&search).await["total"], 2);
    let raw = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, _, _) = f
        .send(request(
            "DELETE",
            &format!("/api/archives/{}", f.archive),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(f.json("/api/archives").await, json!([]));
    let (status, _, _) = f.get(&format!("/api/messages/{raw}/raw")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = f.get(&format!("/api/archives/{}", f.archive)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
