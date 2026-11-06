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

fn holtmail_api_contacts(dav: &Value) -> ContactsCalendars {
    ContactsCalendars {
        enabled: dav["enabled"] == json!(true),
        contacts_url: dav["contacts_url"].as_str().map(str::to_owned),
        calendars_url: dav["calendars_url"].as_str().map(str::to_owned),
        needs_consent: false,
        protocol: Default::default(),
    }
}

#[derive(Default)]
struct FakeMailboxes {
    store: std::sync::OnceLock<Arc<dyn Store>>,
    uploads: std::path::PathBuf,
    synced: std::sync::Mutex<Vec<holtmail_core::AccountId>>,
    imported: std::sync::Mutex<Vec<(holtmail_core::AccountId, String, Vec<u8>)>>,
    jobs: std::sync::Mutex<Vec<(holtmail_core::ArchiveId, RemoteImageJob)>>,
    proxy: std::sync::Mutex<ImageProxy>,
    schedule: std::sync::Mutex<SyncSchedule>,
    cancelled: std::sync::Mutex<Vec<String>>,
    oauth: std::sync::Mutex<std::collections::HashMap<String, OAuthStart>>,
    verifications: std::sync::Mutex<Vec<VerifyLedger>>,
    deletions: std::sync::Mutex<Vec<(holtmail_core::ArchiveId, DeletionSelection)>>,
    tested: std::sync::Mutex<Vec<holtmail_core::WebhookId>>,
}

#[async_trait::async_trait]
impl Operations for FakeMailboxes {
    async fn ledger(&self) -> Result<LedgerStatus, MailboxError> {
        let store = self.store.get().unwrap();
        Ok(LedgerStatus {
            head: store.ledger_head().await.unwrap(),
            last: None,
        })
    }

    async fn verify_ledger(&self, request: VerifyLedger) -> Result<Task, MailboxError> {
        self.verifications.lock().unwrap().push(request);
        let mut task = fake_task(holtmail_core::ArchiveId::new());
        task.kind = TaskKind::LedgerVerify;
        task.archive_id = None;
        Ok(task)
    }

    async fn review_deletion(
        &self,
        archive: holtmail_core::ArchiveId,
        selection: DeletionSelection,
        _actor: holtmail_core::UserId,
    ) -> Result<DeletionReview, MailboxError> {
        if selection == DeletionSelection::Query("nothing".into()) {
            return Err(MailboxError::Invalid("Nothing to delete.".into()));
        }
        self.deletions.lock().unwrap().push((archive, selection));
        Ok(DeletionReview {
            id: "r1".into(),
            archive_id: archive,
            query: None,
            count: 2,
            bytes: 300,
            oldest: Some(holtmail_core::Timestamp::from_secs(1)),
            newest: Some(holtmail_core::Timestamp::from_secs(2)),
            sample: Vec::new(),
            expires_at: holtmail_core::Timestamp::from_secs(3),
        })
    }

    async fn confirm_deletion(
        &self,
        review: &str,
        count: u64,
        _actor: holtmail_core::UserId,
    ) -> Result<Task, MailboxError> {
        if review != "r1" || count != 2 {
            return Err(MailboxError::Invalid("stale".into()));
        }
        let mut task = fake_task(holtmail_core::ArchiveId::new());
        task.kind = TaskKind::DeleteMessages;
        Ok(task)
    }

    async fn create_webhook(
        &self,
        url: &str,
        events: &[String],
    ) -> Result<(holtmail_store::Webhook, String), MailboxError> {
        let (url, events) = holtmail_hooks::check(url, events).map_err(MailboxError::Invalid)?;
        let hook = holtmail_store::Webhook {
            id: holtmail_core::WebhookId::new(),
            url,
            events,
            sealed_secret: b"sealed".to_vec(),
            enabled: true,
            created_at: holtmail_core::Timestamp::from_secs(5),
        };
        self.store
            .get()
            .unwrap()
            .create_webhook(&hook)
            .await
            .unwrap();
        Ok((hook, "whsec_first".into()))
    }

    async fn rotate_webhook_secret(
        &self,
        _id: holtmail_core::WebhookId,
    ) -> Result<String, MailboxError> {
        Ok("whsec_second".into())
    }

    async fn test_webhook(&self, id: holtmail_core::WebhookId) -> Result<(), MailboxError> {
        self.tested.lock().unwrap().push(id);
        Ok(())
    }

    fn smtp(&self) -> Option<SmtpStatus> {
        None
    }
}

fn answer_pairs(answer: &str) -> std::collections::HashMap<String, String> {
    let query = answer.rsplit_once('?').map_or(answer, |(_, q)| q);
    url::form_urlencoded::parse(query.trim().as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn fake_task(archive: holtmail_core::ArchiveId) -> Task {
    Task {
        id: "t1".into(),
        kind: TaskKind::RemoteImages,
        archive_id: Some(archive),
        account_id: None,
        title: "Personal".into(),
        state: TaskState::Running,
        started_at: holtmail_core::Timestamp::from_secs(1),
        finished_at: None,
        done: 3,
        total: Some(10),
        summary: "3 of 10 messages".into(),
        cancellable: true,
        log: vec![TaskLine {
            at: holtmail_core::Timestamp::from_secs(1),
            level: TaskLevel::Info,
            text: "Started".into(),
        }],
    }
}

#[derive(Default)]
struct FakeFetcher {
    asked: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl holtmail_fetch::Fetcher for FakeFetcher {
    async fn image(
        &self,
        url: &str,
    ) -> Result<holtmail_fetch::RemoteImage, holtmail_fetch::FetchError> {
        self.asked.lock().unwrap().push(url.to_owned());
        Ok(holtmail_fetch::RemoteImage {
            content_type: "image/gif".into(),
            bytes: Bytes::from_static(b"GIF89a fetched"),
        })
    }
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

    async fn add_jmap(
        &self,
        archive: holtmail_core::ArchiveId,
        account: NewJmapAccount,
    ) -> Result<holtmail_store::Account, MailboxError> {
        if account.secret != "right" {
            return Err(MailboxError::Invalid("The server refused".into()));
        }
        let store = self.store.get().unwrap();
        let created = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Jmap,
                name: account.name.unwrap_or_else(|| account.url.clone()),
                address: None,
                settings: json!({ "url": account.url, "username": account.username, "auth": account.auth }),
                sealed_secret: None,
                enabled: true,
            })
            .await
            .map_err(|e| MailboxError::Backend(e.to_string()))?;
        self.synced.lock().unwrap().push(created.id);
        Ok(created)
    }

    async fn configure(
        &self,
        account: &holtmail_store::Account,
        change: AccountChange,
    ) -> Result<holtmail_store::Account, MailboxError> {
        if let Some(jmap) = &change.jmap {
            if change.password.as_deref() != Some("right") {
                return Err(MailboxError::Invalid("The server refused".into()));
            }
            let mut account = account.clone();
            account.settings["url"] = json!(jmap.url);
            account.settings["auth"] = json!(jmap.auth);
            account.settings["exclude"] = json!(jmap.exclude);
            self.store
                .get()
                .unwrap()
                .update_account(&account)
                .await
                .map_err(|e| MailboxError::Backend(e.to_string()))?;
            return Ok(account);
        }
        if change.password.is_none()
            && change
                .connection
                .as_ref()
                .is_some_and(|c| account.settings["host"] != json!(c.host))
        {
            return Err(MailboxError::Invalid(
                "Enter the password again when changing the server.".into(),
            ));
        }
        if change.password.as_deref().is_some_and(|p| p != "right")
            || change
                .connection
                .as_ref()
                .is_some_and(|c| c.host != "imap.example.com" && c.host != "mail.example.org")
        {
            return Err(MailboxError::Invalid("The server refused".into()));
        }
        let mut account = account.clone();
        if let Some(password) = change.password {
            account.sealed_secret = Some(password.into_bytes());
        }
        if let Some(connection) = change.connection {
            account.settings["host"] = json!(connection.host);
            account.settings["username"] = json!(connection.username);
            account.settings["exclude"] = json!(connection.exclude);
        }
        if let Some(minutes) = change.sync_interval_minutes {
            account.settings["every"] = json!(minutes);
        }
        if let Some(days) = change.min_age_days {
            account.settings["min_age"] = json!(days);
        }
        if let Some(dav) = change.contacts_calendars {
            if dav.enabled && dav.contacts_url.as_deref() == Some("https://nowhere.example.com") {
                return Err(MailboxError::Invalid("No contacts there".into()));
            }
            account.settings["dav"] = json!({
                "enabled": dav.enabled,
                "contacts_url": dav.contacts_url,
                "calendars_url": dav.calendars_url.or(dav.contacts_url.clone()),
            });
        }
        self.store
            .get()
            .unwrap()
            .update_account(&account)
            .await
            .map_err(|e| MailboxError::Backend(e.to_string()))?;
        Ok(account)
    }

    fn details(&self, account: &holtmail_store::Account) -> AccountDetails {
        AccountDetails {
            jmap: account.settings.get("url").map(|url| JmapConnection {
                url: url.as_str().unwrap_or_default().into(),
                username: account.settings["username"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                auth: account.settings["auth"].as_str().unwrap_or_default().into(),
                exclude: serde_json::from_value(account.settings["exclude"].clone())
                    .unwrap_or_default(),
            }),
            connection: account.settings.get("host").map(|host| ImapConnection {
                host: host.as_str().unwrap_or_default().into(),
                port: 993,
                security: "tls".into(),
                username: account.settings["username"]
                    .as_str()
                    .unwrap_or("ada")
                    .into(),
                exclude: serde_json::from_value(account.settings["exclude"].clone())
                    .unwrap_or_default(),
            }),
            sync_interval_minutes: account.settings.get("every").and_then(Value::as_u64),
            min_age_days: account
                .settings
                .get("min_age")
                .and_then(Value::as_u64)
                .map(|days| days as u32),
            oauth_provider: account.settings["oauth"]["provider"]
                .as_str()
                .map(str::to_owned),
            needs_sign_in: account.settings["oauth"]["needs_sign_in"] == json!(true),
            contacts_calendars: (account.kind != holtmail_store::AccountKind::Import).then(|| {
                let dav = &account.settings["dav"];
                holtmail_api_contacts(dav)
            }),
        }
    }

    fn sync_schedule(&self) -> SyncSchedule {
        *self.schedule.lock().unwrap()
    }

    async fn set_sync_schedule(&self, minutes: u64) -> Result<SyncSchedule, MailboxError> {
        if minutes > 10_080 {
            return Err(MailboxError::Invalid("too long".into()));
        }
        let mut schedule = self.schedule.lock().unwrap();
        schedule.minutes = minutes;
        Ok(*schedule)
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

    fn tasks(&self) -> Vec<Task> {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .map(|(archive, _)| fake_task(*archive))
            .collect()
    }

    fn cancel_task(&self, id: &str) -> Result<(), MailboxError> {
        if id != "t1" {
            return Err(MailboxError::Invalid("no such task".into()));
        }
        self.cancelled.lock().unwrap().push(id.to_owned());
        Ok(())
    }

    async fn start_remote_images(
        &self,
        archive: holtmail_core::ArchiveId,
        job: RemoteImageJob,
    ) -> Result<Task, MailboxError> {
        self.jobs.lock().unwrap().push((archive, job));
        Ok(fake_task(archive))
    }

    async fn start_oauth(&self, start: OAuthStart) -> Result<OAuthStarted, MailboxError> {
        let state = format!("state-{}", self.oauth.lock().unwrap().len() + 1);
        let started = OAuthStarted {
            authorize_url: format!(
                "https://accounts.example.com/auth?state={state}&login_hint={}",
                start.email
            ),
            state: state.clone(),
            needs_paste: start.provider == "google",
            thunderbird_client: true,
            expires_at: holtmail_core::Timestamp::from_secs(600),
        };
        self.oauth.lock().unwrap().insert(state, start);
        Ok(started)
    }

    async fn finish_oauth(&self, answer: &str) -> Result<OAuthFinished, MailboxError> {
        let pairs = answer_pairs(answer);
        if pairs.contains_key("error") {
            return Err(MailboxError::Invalid("Access was not granted.".into()));
        }
        let start = pairs
            .get("state")
            .and_then(|state| self.oauth.lock().unwrap().remove(state))
            .ok_or_else(|| MailboxError::Invalid("This sign-in is unknown or expired.".into()))?;
        if pairs.get("code").map(String::as_str) != Some("good") {
            return Err(MailboxError::Invalid("The code was refused.".into()));
        }
        let store = self.store.get().unwrap();
        let account = match start.target {
            OAuthTarget::NewAccount { archive, name } => store
                .create_account(NewAccount {
                    archive_id: archive,
                    kind: start.kind,
                    name: name.unwrap_or_else(|| start.email.clone()),
                    address: Some(start.email.clone()),
                    settings: json!({ "oauth": { "provider": start.provider } }),
                    sealed_secret: None,
                    enabled: true,
                })
                .await
                .map_err(|e| MailboxError::Backend(e.to_string()))?,
            OAuthTarget::Account(id) => {
                let mut account = store.get_account(id).await.unwrap().unwrap();
                account.settings["oauth"] = json!({ "provider": start.provider });
                store.update_account(&account).await.unwrap();
                account
            }
        };
        Ok(OAuthFinished {
            account,
            return_to: start.return_to,
        })
    }

    fn oauth_providers(&self) -> Vec<OAuthProvider> {
        vec![OAuthProvider {
            provider: "google".into(),
            thunderbird_client: true,
        }]
    }

    fn oauth_return_to(&self, answer: &str) -> Option<String> {
        let state = answer_pairs(answer).remove("state")?;
        self.oauth
            .lock()
            .unwrap()
            .get(&state)
            .map(|s| s.return_to.clone())
    }

    fn image_proxy(&self) -> ImageProxy {
        self.proxy.lock().unwrap().clone()
    }

    async fn set_image_proxy(&self, proxy: Option<String>) -> Result<ImageProxy, MailboxError> {
        if proxy
            .as_deref()
            .is_some_and(|p| !p.starts_with("socks5h://"))
        {
            return Err(MailboxError::Invalid("unreachable".into()));
        }
        let mut current = self.proxy.lock().unwrap();
        current.address = proxy;
        Ok(current.clone())
    }

    async fn rebuild_people(&self, archive: holtmail_core::ArchiveId) -> Result<(), MailboxError> {
        let store = self.store.get().unwrap();
        holtmail_people::rebuild(&**store, archive)
            .await
            .map(|_| ())
            .map_err(|e| MailboxError::Backend(e.to_string()))
    }
}

struct Fixture {
    app: Router,
    mailboxes: Arc<FakeMailboxes>,
    fetcher: Arc<FakeFetcher>,
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
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
    let ingest = Ingest::new(blobs.clone(), store.clone(), index.clone())
        .with_extractor(Arc::new(holtmail_extract_basic::BasicExtractor::new()));
    for (uid, raw, flags, labels) in [
        (
            1,
            NEWSLETTER,
            vec!["\\Seen".to_owned(), "\\Flagged".to_owned()],
            Some(vec!["Inbox".to_owned(), "News & updates".to_owned()]),
        ),
        (2, PLAIN, Vec::new(), Some(vec!["Inbox".to_owned()])),
    ] {
        ingest
            .ingest(
                archive,
                Bytes::from_static(raw),
                &Placement {
                    folder: Some(folder),
                    uid: Some(uid),
                    flags,
                    labels,
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
    let fetcher = Arc::new(FakeFetcher::default());
    Fixture {
        app: router(ApiState {
            blobs: blobs.clone(),
            store: store.clone(),
            index,
            auth: auth.clone(),
            mailboxes: mailboxes.clone(),
            operations: mailboxes.clone(),
            fetcher: fetcher.clone(),
            secure_cookies: false,
        }),
        mailboxes,
        fetcher,
        store,
        blobs,
        ingest,
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
async fn lists_labels_and_filters_by_them() {
    let f = fixture().await;
    let labels = f.json(&format!("/api/archives/{}/labels", f.archive)).await;
    assert_eq!(
        labels,
        json!([
            { "name": "Inbox", "messages": 2, "unread": 1 },
            { "name": "News & updates", "messages": 1, "unread": 0 },
        ])
    );

    let page = f
        .json(&format!(
            "/api/archives/{}/messages?label=News%20%26%20updates",
            f.archive
        ))
        .await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["subject"], "Weekly news");
    assert_eq!(items[0]["labels"], json!(["Inbox", "News & updates"]));
    let id = items[0]["id"].as_str().unwrap();
    let detail = f.json(&format!("/api/messages/{id}")).await;
    assert_eq!(detail["labels"], json!(["Inbox", "News & updates"]));

    let unknown = f
        .json(&format!("/api/archives/{}/messages?label=Nope", f.archive))
        .await;
    assert!(unknown["items"].as_array().unwrap().is_empty());

    let found = f
        .json(&format!(
            "/api/archives/{}/search?q=label:%22news%20%26%20updates%22",
            f.archive
        ))
        .await;
    assert_eq!(found["total"], 1);
    assert_eq!(
        found["items"][0]["labels"],
        json!(["Inbox", "News & updates"])
    );

    let (status, _, _) = f
        .get(&format!(
            "/api/archives/{}/labels",
            holtmail_core::ArchiveId::new()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "labels of an unknown archive");
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

const PHOTOS: &[u8] = b"From: Carol <carol@example.org>\r\nTo: ada@example.com\r\nSubject: Holiday pictures\r\nMessage-ID: <p1@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain\r\n\r\nHoliday pictures attached.\r\n--m\r\nContent-Type: image/png; name=dot.png\r\nContent-Disposition: attachment; filename=dot.png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==\r\n--m\r\nContent-Type: image/png; name=broken.png\r\nContent-Disposition: attachment; filename=broken.png\r\n\r\nnot a png\r\n--m\r\nContent-Type: text/plain; name=notes.txt\r\nContent-Disposition: attachment; filename=notes.txt\r\n\r\nLighthouse keeper rota\r\n--m--\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn previews_images_and_marks_attachment_matches() {
    let f = fixture().await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let ingested = f
        .ingest
        .ingest(archive, Bytes::from_static(PHOTOS), &Placement::default())
        .await
        .unwrap();
    f.ingest.commit().await.unwrap();
    let id = ingested.message_id;

    let detail = f.json(&format!("/api/messages/{id}")).await;
    let attachments = detail["attachments"].as_array().unwrap();
    assert_eq!(
        attachments[0]["thumbnail_url"],
        format!("/api/messages/{id}/attachments/0/thumbnail")
    );
    assert_eq!(attachments[2]["thumbnail_url"], Value::Null);

    let uri = format!("/api/messages/{id}/attachments/0/thumbnail");
    let (status, headers, first) = f.get(&uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], "sandbox");
    assert!(first.starts_with(b"\x89PNG"));
    let source = f.store.attachments(id).await.unwrap()[0].blob_id;
    let Some(holtmail_store::Derived::Blob(cached)) = f
        .store
        .derived(&source, holtmail_store::DerivedKind::Thumbnail)
        .await
        .unwrap()
    else {
        panic!("the thumbnail was not kept");
    };
    assert_eq!(f.blobs.get(&cached).await.unwrap().unwrap(), first);
    let (_, _, again) = f.get(&uri).await;
    assert_eq!(again, first);

    for ordinal in [1, 2, 9] {
        let (status, _, _) = f
            .get(&format!(
                "/api/messages/{id}/attachments/{ordinal}/thumbnail"
            ))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "attachment {ordinal}");
    }
    let broken = f.store.attachments(id).await.unwrap()[1].blob_id;
    assert_eq!(
        f.store
            .derived(&broken, holtmail_store::DerivedKind::Thumbnail)
            .await
            .unwrap(),
        Some(holtmail_store::Derived::Nothing)
    );

    let found = f
        .json(&format!("/api/archives/{}/search?q=lighthouse", f.archive))
        .await;
    assert_eq!(found["items"][0]["id"], id.to_string());
    assert_eq!(found["items"][0]["attachment_match"], true);
    let found = f
        .json(&format!("/api/archives/{}/search?q=holiday", f.archive))
        .await;
    assert_eq!(found["items"][0]["attachment_match"], false);
    let listed = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    assert_eq!(listed["items"][0]["attachment_match"], false);
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

#[test]
fn declares_the_refusal_of_members_on_every_write() {
    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    let open = [
        "/api/setup",
        "/api/login",
        "/api/logout",
        "/api/me",
        "/api/me/password",
    ];
    let mut checked = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if method == "get" || open.contains(&path.as_str()) {
                continue;
            }
            assert!(
                operation["responses"]["403"].is_object(),
                "{method} {path} does not declare 403"
            );
            checked += 1;
        }
    }
    assert!(checked >= 20, "{checked}");
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
        format!("/api/messages/{message}/attachments/0/thumbnail"),
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

    let gone = format!("/api/accounts/{}", holtmail_core::AccountId::new());
    let (status, _, _) = f
        .send(Request::delete(&gone).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a delete needs no body type");
    let mut request = Request::delete(&gone).body(Body::empty()).unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example.org".parse().unwrap());
    let (status, _, _) = f.send(request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
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

    for uri in [
        "/api/webhooks",
        "/api/smtp",
        "/api/ledger",
        "/api/oauth/providers",
        "/api/tasks",
    ] {
        let (status, _, _) = f
            .send_as(&member, Request::get(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }

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
    for uri in ["/api/tasks", "/api/image-proxy", "/api/sync-schedule"] {
        let (status, _, _) = f
            .send_as(&member, Request::get(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
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

#[tokio::test(flavor = "multi_thread")]
async fn proxies_and_shows_saved_remote_images() {
    let f = fixture().await;
    let found = f
        .json(&format!("/api/archives/{}/search?q=from:news", f.archive))
        .await;
    let id = found["items"][0]["id"].as_str().unwrap().to_owned();
    let tracker = "https://tracker.example.com/p.gif";
    let key = holtmail_sanitize::remote_key(tracker);
    let proxied = format!("http://mail.example.com/api/messages/{id}/remote/{key}");

    let (_, headers, body) = f
        .get(&format!("/api/messages/{id}/body?remote=proxy"))
        .await;
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains(&proxied), "{body}");
    assert!(!body.contains("tracker.example.com"), "{body}");
    assert_eq!(headers["x-holtmail-remote-blocked"], "0");

    let (_, _, body) = f
        .get(&format!("/api/messages/{id}/body?remote=direct"))
        .await;
    assert!(String::from_utf8_lossy(&body).contains(tracker));

    let (status, headers, bytes) = f.get(&format!("/api/messages/{id}/remote/{key}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/gif");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(&bytes[..], b"GIF89a fetched");
    assert_eq!(*f.fetcher.asked.lock().unwrap(), [tracker]);

    let other = holtmail_sanitize::remote_key("http://10.0.0.1/admin");
    let (status, _, _) = f.get(&format!("/api/messages/{id}/remote/{other}")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the proxy fetched a URL the message does not link"
    );
    assert_eq!(f.fetcher.asked.lock().unwrap().len(), 1);

    let message: holtmail_core::MessageId = id.parse().unwrap();
    let blob = f
        .blobs
        .put(Bytes::from_static(b"GIF89a saved"))
        .await
        .unwrap();
    f.store
        .save_remote_copy(&holtmail_store::RemoteCopy {
            key: key.clone(),
            url: tracker.into(),
            blob_id: Some(blob),
            content_type: Some("image/gif".into()),
            size: 12,
            fetched_at: holtmail_core::Timestamp::from_secs(1),
            error: None,
        })
        .await
        .unwrap();
    f.store
        .record_remote_scan(message, std::slice::from_ref(&key))
        .await
        .unwrap();

    let (_, headers, body) = f.get(&format!("/api/messages/{id}/body")).await;
    assert!(
        String::from_utf8_lossy(&body).contains(&proxied),
        "a saved copy is hidden"
    );
    assert_eq!(headers["x-holtmail-remote-blocked"], "0");
    assert_eq!(headers["x-holtmail-remote-saved"], "1");
    let (_, _, bytes) = f.get(&format!("/api/messages/{id}/remote/{key}")).await;
    assert_eq!(&bytes[..], b"GIF89a saved");
    assert_eq!(
        f.fetcher.asked.lock().unwrap().len(),
        1,
        "a saved copy was fetched again"
    );

    let (_, headers, body) = f
        .get(&format!("/api/messages/{id}/body?remote=proxy&saved=false"))
        .await;
    assert!(
        String::from_utf8_lossy(&body).contains(&format!(
            "http://mail.example.com/api/messages/{id}/live/{key}"
        )),
        "originals were not asked for"
    );
    assert_eq!(headers["x-holtmail-remote-saved"], "0");
    let (_, _, bytes) = f.get(&format!("/api/messages/{id}/live/{key}")).await;
    assert_eq!(
        &bytes[..],
        b"GIF89a fetched",
        "the live route served the saved copy"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn turns_on_saving_and_starts_tasks() {
    let f = fixture().await;
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &format!("/api/archives/{}", f.archive),
            json!({ "save_remote_images": true }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let archive: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(archive["save_remote_images"], true);
    assert_eq!(archive["name"], "Personal");
    assert!(archive["remote_images"]["since"].is_i64());
    assert_eq!(
        archive["remote_images"]["older_pending"], 2,
        "mail archived before turning saving on is not offered"
    );
    assert!(
        f.mailboxes.jobs.lock().unwrap().is_empty(),
        "older mail was processed without being asked"
    );

    let uri = format!("/api/archives/{}/remote-images", f.archive);
    let (status, _, body) = f.send(post_json(&uri, json!({ "job": "older" }))).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let task: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(task["kind"], "remote_images");
    assert_eq!(task["state"], "running");
    assert_eq!(task["log"][0]["level"], "info");
    let (status, _, _) = f.send(post_json(&uri, json!({ "job": "failed" }))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _, _) = f.send(post_json(&uri, json!({ "job": "all" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        f.mailboxes
            .jobs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, job)| *job)
            .collect::<Vec<_>>(),
        [RemoteImageJob::Older, RemoteImageJob::Failed]
    );

    let tasks = f.json("/api/tasks").await;
    assert_eq!(tasks.as_array().unwrap().len(), 2);
    let (status, _, _) = f.send(post_json("/api/tasks/t1/cancel", json!({}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = f.send(post_json("/api/tasks/t9/cancel", json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread")]
async fn sets_the_image_proxy() {
    let f = fixture().await;
    let proxy = f.json("/api/image-proxy").await;
    assert_eq!(proxy["address"], Value::Null);
    let (status, _, body) = f
        .send(request(
            "PUT",
            "/api/image-proxy",
            json!({ "proxy": " socks5h://127.0.0.1:9050 " }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["address"],
        "socks5h://127.0.0.1:9050"
    );
    let (status, _, _) = f
        .send(request(
            "PUT",
            "/api/image-proxy",
            json!({ "proxy": "http://proxy.example.com" }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = f
        .send(request("PUT", "/api/image-proxy", json!({ "proxy": "" })))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["address"],
        Value::Null
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_the_connection_and_the_schedule() {
    let f = fixture().await;
    let accounts = format!("/api/archives/{}/accounts", f.archive);
    let (_, _, body) = f
        .send(post_json(
            &accounts,
            json!({ "host": "imap.example.com", "username": "ada", "password": "right" }),
        ))
        .await;
    let id = serde_json::from_slice::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let uri = format!("/api/accounts/{id}");

    let (status, _, _) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "connection": { "host": "imap.example.net", "username": "ada" } }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a refused connection was stored"
    );
    let moved = json!({
        "connection": {
            "host": "mail.example.org",
            "username": "ada@example.org",
            "exclude": ["Junk"]
        },
        "sync_interval_minutes": 60
    });
    let (status, _, body) = f.send(request("PATCH", &uri, moved.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"],
        "Enter the password again when changing the server."
    );
    let mut with_password = moved;
    with_password["password"] = json!("right");
    let (status, _, body) = f.send(request("PATCH", &uri, with_password)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["connection"]["host"], "mail.example.org");
    assert_eq!(account["connection"]["exclude"], json!(["Junk"]));
    assert_eq!(account["sync_interval_minutes"], 60);
    assert!(account["connection"].get("password").is_none());

    let (_, _, body) = f
        .send(request("PATCH", &uri, json!({ "name": "Work" })))
        .await;
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        account["sync_interval_minutes"], 60,
        "a missing interval was treated as a change"
    );
    let (_, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "sync_interval_minutes": null }),
        ))
        .await;
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["sync_interval_minutes"], Value::Null);
    assert_eq!(account["min_age_days"], Value::Null);

    let (_, _, body) = f
        .send(request("PATCH", &uri, json!({ "min_age_days": 30 })))
        .await;
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["min_age_days"], 30);
    let (_, _, body) = f
        .send(request("PATCH", &uri, json!({ "name": "Work" })))
        .await;
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        account["min_age_days"], 30,
        "a missing minimum age was a change"
    );
    let (_, _, body) = f
        .send(request("PATCH", &uri, json!({ "min_age_days": 0 })))
        .await;
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        account["min_age_days"],
        Value::Null,
        "0 did not archive all mail"
    );

    assert_eq!(account["contacts_calendars"]["enabled"], false);
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "contacts_calendars": { "enabled": true, "contacts_url": "https://nowhere.example.com" } }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a refused check is refused"
    );
    assert!(String::from_utf8_lossy(&body).contains("No contacts there"));
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "contacts_calendars": { "enabled": true, "contacts_url": "https://dav.example.com/remote.php/dav" } }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["contacts_calendars"]["enabled"], true);
    assert_eq!(
        account["contacts_calendars"]["calendars_url"],
        "https://dav.example.com/remote.php/dav"
    );
    assert_eq!(account["contacts_calendars"]["needs_consent"], false);
    assert_eq!(account["status"]["contacts_calendars"], Value::Null);

    let schedule = f.json("/api/sync-schedule").await;
    assert_eq!(schedule["minutes"], 0);
    let (status, _, body) = f
        .send(request(
            "PUT",
            "/api/sync-schedule",
            json!({ "minutes": 30 }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["minutes"],
        30
    );
    let (status, _, _) = f
        .send(request(
            "PUT",
            "/api/sync-schedule",
            json!({ "minutes": 20_000 }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn signs_in_with_oauth_from_a_pasted_address() {
    let f = fixture().await;
    let start = |body: Value| f.send(post_json("/api/oauth/start", body));
    let (status, _, body) = start(json!({
        "provider": "google",
        "email": "ada@example.com",
        "archive_id": f.archive,
        "return_to": "/welcome"
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let started: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(started["needs_paste"], true);
    assert_eq!(started["thunderbird_client"], true);
    assert_eq!(started["expires_at"], 600_000);
    let state = started["state"].as_str().unwrap().to_owned();

    for (body, expected) in [
        (
            json!({ "provider": "yahoo", "email": "ada@example.com", "archive_id": f.archive }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "provider": "google", "email": "ada", "archive_id": f.archive }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "provider": "google", "email": "ada@example.com" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "provider": "google", "email": "ada@example.com", "archive_id": f.archive, "return_to": "https://example.org/" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "provider": "google", "email": "ada@example.com", "archive_id": holtmail_core::ArchiveId::new().to_string() }),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(start(body.clone()).await.0, expected, "{body}");
    }

    let finish = |pasted: String| {
        f.send(post_json(
            "/api/oauth/finish",
            json!({ "pasted_url": pasted }),
        ))
    };
    let (status, _, body) = finish("http://127.0.0.1/?code=good&state=forged".into()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("unknown or expired"));
    let (status, _, body) = finish(format!("http://127.0.0.1/?code=good&state={state}")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["oauth_provider"], "google");
    assert_eq!(account["needs_sign_in"], false);
    assert_eq!(account["address"], "ada@example.com");
    let (status, _, _) = finish(format!("http://127.0.0.1/?code=good&state={state}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a sign-in was used twice");

    let id = account["id"].as_str().unwrap();
    let (status, _, body) = start(json!({ "provider": "google", "account_id": id })).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let again = serde_json::from_slice::<Value>(&body).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, _) = finish(format!("?code=good&state={again}")).await;
    assert_eq!(status, StatusCode::OK);

    for body in [
        json!({ "provider": "google", "kind": "graph", "email": "ada@example.com", "archive_id": f.archive }),
        json!({ "provider": "microsoft", "kind": "pop3", "email": "ada@example.com", "archive_id": f.archive }),
    ] {
        assert_eq!(
            start(body.clone()).await.0,
            StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    let (status, _, body) = start(json!({
        "provider": "microsoft",
        "kind": "graph",
        "email": "ada@example.org",
        "archive_id": f.archive
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let graph = serde_json::from_slice::<Value>(&body).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, body) = finish(format!("?code=good&state={graph}")).await;
    assert_eq!(status, StatusCode::OK);
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["kind"], "graph");
    assert_eq!(account["oauth_provider"], "microsoft");
    let (status, _, body) =
        start(json!({ "provider": "microsoft", "account_id": account["id"] })).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let again = serde_json::from_slice::<Value>(&body).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        f.mailboxes.oauth.lock().unwrap()[&again].kind,
        AccountKind::Graph,
        "signing in again did not keep the account's kind"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn finishes_own_client_sign_ins_through_the_callback() {
    let f = fixture().await;
    let (_, _, body) = f
        .send(post_json(
            "/api/oauth/start",
            json!({ "provider": "microsoft", "email": "ada@example.com", "archive_id": f.archive, "return_to": "/settings?tab=archives" }),
        ))
        .await;
    let started: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(started["needs_paste"], false);
    let state = started["state"].as_str().unwrap();

    let callback = |query: String| {
        f.send(
            Request::get(format!("/api/oauth/callback?{query}"))
                .header(header::COOKIE, "x=y")
                .body(Body::empty())
                .unwrap(),
        )
    };
    let (status, headers, _) = callback(format!("code=good&state={state}")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers[header::LOCATION].to_str().unwrap();
    assert!(
        location.starts_with("/settings?tab=archives&oauth=done&account="),
        "{location}"
    );
    let (status, headers, _) = callback(format!("code=good&state={state}")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers[header::LOCATION].to_str().unwrap();
    assert!(location.starts_with("/settings?oauth_error="), "{location}");
    let (_, headers, _) = callback("error=access_denied&state=x".into()).await;
    assert!(
        headers[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("oauth_error=Access+was+not+granted")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_managers_sign_in_accounts() {
    let f = fixture().await;
    f.send(post_json(
        "/api/users",
        json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
    ))
    .await;
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    for (uri, body) in [
        (
            "/api/oauth/start",
            json!({ "provider": "google", "email": "grace@example.org", "archive_id": f.archive }),
        ),
        (
            "/api/oauth/finish",
            json!({ "pasted_url": "?code=good&state=state-1" }),
        ),
    ] {
        let (status, _, _) = f.send_as(&member, post_json(uri, body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (status, _, _) = f
        .send(
            Request::post("/api/oauth/start")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, "x=y")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn exports_mbox_and_zip() {
    let f = fixture().await;
    let base = format!("/api/archives/{}/export", f.archive);

    let (status, headers, body) = f.get(&format!("{base}?format=mbox")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/mbox");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"Personal.mbox\"; filename*=UTF-8''Personal.mbox"
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let text = body_text(&body);
    assert_eq!(text.matches("\nFrom ").count() + 1, 2);
    assert!(text.starts_with("From "));
    assert!(text.contains(&String::from_utf8_lossy(PLAIN).into_owned()));

    let (status, headers, body) = f
        .get(&format!("{base}?format=zip&folder={}", f.folder))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(body.to_vec())).unwrap();
    assert_eq!(zip.len(), 2);
    assert!(
        zip.by_index(0)
            .unwrap()
            .name()
            .unwrap()
            .starts_with("INBOX/")
    );

    let (status, _, body) = f
        .get(&format!("{base}?format=mbox&label=News%20%26%20updates"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_text(&body).matches("Subject: ").count(), 1);
    let (status, _, body) = f.get(&format!("{base}?format=mbox&q=from:bob")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_text(&body).contains("Re: Weekly news"));

    for (uri, expected) in [
        (format!("{base}?format=rar"), StatusCode::BAD_REQUEST),
        (
            format!("{base}?format=zip&label=A&q=b"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("{base}?format=zip&q=before:soon"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!(
                "{base}?format=zip&folder={}",
                holtmail_core::FolderId::new()
            ),
            StatusCode::NOT_FOUND,
        ),
        (
            format!(
                "/api/archives/{}/export?format=zip",
                holtmail_core::ArchiveId::new()
            ),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, _, _) = f.get(&uri).await;
        assert_eq!(status, expected, "{uri}");
    }

    let (status, _, _) = f
        .send(
            Request::get(format!("{base}?format=zip"))
                .header(header::COOKIE, "holtmail_session=nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    f.send(post_json(
        "/api/users",
        json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
    ))
    .await;
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let export = format!("{base}?format=mbox");
    let (status, _, _) = f
        .send_as(&member, Request::get(&export).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not granted");
    let grace = f
        .store
        .find_user_by_email("grace@example.org")
        .await
        .unwrap()
        .unwrap();
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    f.store.set_grants(grace.id, &[archive]).await.unwrap();
    let (status, _, _) = f
        .send_as(&member, Request::get(&export).body(Body::empty()).unwrap())
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "granted members export what they read"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adds_and_edits_jmap_accounts_for_managers_only() {
    let f = fixture().await;
    let uri = format!("/api/archives/{}/accounts/jmap", f.archive);
    for (body, why) in [
        (
            json!({ "url": "", "auth": "token", "password": "right" }),
            "an empty address",
        ),
        (
            json!({ "url": "https://api.example.com", "auth": "oauth", "password": "right" }),
            "an unknown sign-in",
        ),
        (
            json!({ "url": "https://api.example.com", "password": "right" }),
            "a password without a user name",
        ),
        (
            json!({ "url": "https://api.example.com", "auth": "token", "password": "" }),
            "an empty token",
        ),
        (
            json!({ "url": "https://api.example.com", "auth": "token", "password": "wrong" }),
            "a refused token",
        ),
    ] {
        let (status, _, _) = f.send(post_json(&uri, body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why} was accepted");
    }
    let (status, _, body) = f
        .send(post_json(
            &uri,
            json!({ "url": "https://api.example.com/jmap/session", "auth": "token", "password": "right", "name": "Fastmail" }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["kind"], "jmap");
    assert_eq!(account["name"], "Fastmail");
    assert_eq!(account["connection"], Value::Null);
    assert_eq!(
        account["jmap"]["url"],
        "https://api.example.com/jmap/session"
    );
    assert_eq!(account["jmap"]["auth"], "token");
    assert!(account["jmap"].get("password").is_none());
    let id = account["id"].as_str().unwrap().to_owned();

    let (status, _, body) = f
        .send(request(
            "PATCH",
            &format!("/api/accounts/{id}"),
            json!({ "jmap": { "url": "https://jmap.example.org", "username": "ada", "auth": "password", "exclude": ["Spam"] }, "password": "right" }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["jmap"]["auth"], "password");
    assert_eq!(account["jmap"]["exclude"], json!(["Spam"]));

    f.send(post_json(
        "/api/users",
        json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
    ))
    .await;
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let (status, _, _) = f
        .send_as(
            &member,
            post_json(
                &uri,
                json!({ "url": "https://api.example.com", "auth": "token", "password": "right" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

const CARD_V1: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada Quill\r\nEMAIL:Ada@Example.com\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";
const CARD_V2: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada Quill\r\nEMAIL:Ada@Example.com\r\nTEL:+1 555 0199\r\nEND:VCARD\r\n";
const MEETING: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:review@example.com\r\nSUMMARY:Review\r\nDTSTART;TZID=Europe/Berlin:20250310T090000\r\nDTEND;TZID=Europe/Berlin:20250310T100000\r\nORGANIZER:mailto:ada@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

async fn pim_fixture(f: &Fixture) -> (String, String, String) {
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let collection = |kind, remote: &str| holtmail_store::NewCollection {
        archive_id: archive,
        account_id: None,
        kind,
        remote_id: remote.into(),
        name: remote.into(),
        color: None,
    };
    let book = f
        .store
        .upsert_collection(collection(
            holtmail_store::CollectionKind::Contacts,
            "people.vcf",
        ))
        .await
        .unwrap();
    let team = f
        .store
        .upsert_collection(collection(
            holtmail_store::CollectionKind::Calendar,
            "team.ics",
        ))
        .await
        .unwrap();
    let item = |collection, remote: &str, kind, raw: &'static str| holtmail_ingest::IncomingItem {
        collection,
        remote_id: remote.into(),
        kind,
        format: holtmail_pim::Format::of(kind),
        raw: Bytes::from_static(raw.as_bytes()),
        etag: None,
        seen_at: holtmail_core::Timestamp::from_secs(1_700_000_000),
    };
    let ada = f
        .ingest
        .ingest_item(item(
            book.id,
            "ada",
            holtmail_pim::ItemKind::Contact,
            CARD_V1,
        ))
        .await
        .unwrap();
    f.ingest
        .ingest_item(item(
            book.id,
            "ada",
            holtmail_pim::ItemKind::Contact,
            CARD_V2,
        ))
        .await
        .unwrap();
    let review = f
        .ingest
        .ingest_item(item(
            team.id,
            "review",
            holtmail_pim::ItemKind::Event,
            MEETING,
        ))
        .await
        .unwrap();
    (
        book.id.to_string(),
        ada.item_id.to_string(),
        review.item_id.to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_contacts_and_events_with_their_versions() {
    let f = fixture().await;
    let (book, ada, review) = pim_fixture(&f).await;

    let collections = f
        .json(&format!("/api/archives/{}/collections", f.archive))
        .await;
    assert_eq!(collections.as_array().unwrap().len(), 2);
    assert_eq!(collections[0]["kind"], "calendar");
    assert_eq!(collections[1]["kind"], "contacts");
    assert_eq!(collections[1]["items"], 1);

    let contacts = f
        .json(&format!(
            "/api/archives/{}/contacts?collection={book}",
            f.archive
        ))
        .await;
    assert_eq!(contacts["items"][0]["title"], "Ada Quill");
    assert_eq!(contacts["items"][0]["versions"], 2);
    assert_eq!(contacts["items"][0]["emails"], json!(["ada@example.com"]));
    assert_eq!(contacts["items"][0]["phones"], json!(["+15550199"]));
    assert!(contacts["next"].is_null());

    let events = f.json(&format!("/api/archives/{}/events", f.archive)).await;
    assert_eq!(events["items"][0]["id"], review);
    assert_eq!(events["items"][0]["starts_at"], 1_741_593_600_000_i64);
    assert_eq!(events["items"][0]["time_zone"], "Europe/Berlin");
    assert_eq!(events["items"][0]["organizer"], "ada@example.com");

    let detail = f.json(&format!("/api/items/{ada}")).await;
    assert_eq!(detail["data"]["type"], "contact");
    assert_eq!(detail["data"]["full_name"], "Ada Quill");
    let versions = detail["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["changes"], json!([]));
    assert_eq!(
        versions[1]["changes"],
        json!([{ "field": "phone", "before": "+1 555 0100", "after": "+1 555 0199" }])
    );
    assert_eq!(versions[1]["readable"], true);

    let raw_url = versions[0]["raw_url"].as_str().unwrap();
    let (status, headers, body) = f.get(raw_url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        Bytes::from_static(CARD_V1.as_bytes()),
        "raw bytes unchanged"
    );
    assert_eq!(headers[header::CONTENT_TYPE], "text/vcard");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    for missing in [
        format!("/api/items/{ada}/versions/3/raw"),
        format!("/api/items/{}", holtmail_core::ItemId::new()),
        "/api/items/not-an-id".to_owned(),
    ] {
        let (status, _, _) = f.get(&missing).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    }
    for bad in [
        format!("/api/archives/{}/contacts?removed=sometimes", f.archive),
        format!("/api/archives/{}/contacts?after=garbage", f.archive),
    ] {
        let (status, _, _) = f.get(&bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let gone = f
        .json(&format!(
            "/api/archives/{}/contacts?removed=only",
            f.archive
        ))
        .await;
    assert_eq!(gone["items"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn members_read_contacts_only_in_granted_archives() {
    let f = fixture().await;
    let (_, ada, _) = pim_fixture(&f).await;
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let grace = serde_json::from_slice::<Value>(&body).unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let uris = [
        format!("/api/archives/{}/collections", f.archive),
        format!("/api/archives/{}/contacts", f.archive),
        format!("/api/archives/{}/events", f.archive),
        format!("/api/items/{ada}"),
        format!("/api/items/{ada}/versions/1/raw"),
        format!(
            "/api/archives/{}/calendar?from=1741564800000&to=1741651200000",
            f.archive
        ),
        format!("/api/archives/{}/search/items?q=review", f.archive),
        format!(
            "/api/events/{}",
            holtmail_calendar::group_id(
                f.archive.parse().unwrap(),
                Some("review@example.com"),
                holtmail_core::ItemId::new()
            )
        ),
    ];
    for uri in &uris {
        let (status, _, _) = f
            .send_as(&member, Request::get(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/users/{grace}"),
            json!({ "archives": [f.archive] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    for uri in &uris {
        let (status, _, _) = f
            .send_as(&member, Request::get(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
}

const MEETING_COPY: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:review@example.com\r\nSEQUENCE:2\r\nSUMMARY:Review\r\nLOCATION:Room 4\r\nDTSTART;TZID=Europe/Berlin:20250310T090000\r\nDTEND;TZID=Europe/Berlin:20250310T100000\r\nORGANIZER:mailto:ada@example.com\r\nATTENDEE;CN=Bob Stone;PARTSTAT=TENTATIVE:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn shows_the_calendar_events_and_their_copies() {
    let f = fixture().await;
    let (_, _, review) = pim_fixture(&f).await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let home = f
        .store
        .upsert_collection(holtmail_store::NewCollection {
            archive_id: archive,
            account_id: None,
            kind: holtmail_store::CollectionKind::Calendar,
            remote_id: "home.ics".into(),
            name: "Home".into(),
            color: Some("#2a9d8f".into()),
        })
        .await
        .unwrap();
    let copy = f
        .ingest
        .ingest_item(holtmail_ingest::IncomingItem {
            collection: home.id,
            remote_id: "review".into(),
            kind: holtmail_pim::ItemKind::Event,
            format: holtmail_pim::Format::of(holtmail_pim::ItemKind::Event),
            raw: Bytes::from_static(MEETING_COPY.as_bytes()),
            etag: None,
            seen_at: holtmail_core::Timestamp::from_secs(1_700_000_100),
        })
        .await
        .unwrap()
        .item_id
        .to_string();
    f.ingest.commit().await.unwrap();

    let day = format!(
        "/api/archives/{}/calendar?from=1741564800000&to=1741651200000",
        f.archive
    );
    let range = f.json(&day).await;
    let found = range["occurrences"].as_array().unwrap();
    assert_eq!(found.len(), 1, "two copies, one instance");
    assert_eq!(found[0]["copies"], 2);
    assert_eq!(found[0]["item_id"], copy, "the higher SEQUENCE leads");
    assert_eq!(found[0]["location"], "Room 4");
    assert_eq!(found[0]["starts_at"], 1_741_593_600_000_i64);
    assert_eq!(found[0]["start_local"], "2025-03-10T09:00:00");
    assert_eq!(found[0]["time_zone"], "Europe/Berlin");
    assert_eq!(found[0]["recurrence_id"], Value::Null);
    let calendars = range["calendars"].as_array().unwrap();
    assert_eq!(calendars.len(), 2, "calendars only, not address books");
    assert!(
        calendars
            .iter()
            .any(|c| c["name"] == "Home" && c["color"] == "#2a9d8f")
    );
    let group = found[0]["group_id"].as_str().unwrap().to_owned();

    let team_id = calendars.iter().find(|c| c["name"] == "team.ics").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let only_team = f.json(&format!("{day}&collections={team_id}")).await;
    assert_eq!(only_team["occurrences"][0]["copies"], 1);
    assert_eq!(only_team["occurrences"][0]["item_id"], review);
    let none = f.json(&format!("{day}&collections=")).await;
    assert_eq!(none["occurrences"], json!([]));
    for bad in [
        format!("/api/archives/{}/calendar?from=5&to=5", f.archive),
        format!("/api/archives/{}/calendar?from=0&to=99999999999", f.archive),
    ] {
        let (status, _, _) = f.get(&bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }

    let event = f
        .json(&format!("/api/events/{group}?tz=America/New_York"))
        .await;
    assert_eq!(event["item_id"], copy);
    assert_eq!(event["title"], "Review");
    assert_eq!(event["start"]["value"], "2025-03-10T09:00:00");
    assert_eq!(event["start"]["in_zone"], "2025-03-10T04:00:00");
    assert_eq!(event["viewer_zone"], "America/New_York");
    let copies = event["copies"].as_array().unwrap();
    assert_eq!(copies.len(), 2);
    assert_eq!(copies[0]["leading"], true);
    assert_eq!(copies[0]["collection_name"], "Home");
    assert_eq!(copies[1]["item_id"], review);
    let fields: Vec<&str> = event["differences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["location", "attendee", "sequence"]);
    assert_eq!(event["attendees"][0]["email"], "bob@example.org");
    assert_eq!(event["attendees"][0]["status"], "tentative");
    assert_eq!(
        event["attendees"][0]["answers"],
        json!([{ "item_id": copy, "status": "tentative" }, { "item_id": review, "status": null }])
    );
    assert_eq!(event["organizer"]["email"], "ada@example.com");
    assert_eq!(event["timeline"].as_array().unwrap().len(), 2);
    assert_eq!(event["recurrence"], Value::Null);
    let (status, _, _) = f
        .get(&format!(
            "/api/events/{}",
            holtmail_core::EventGroupId::from_hash([1; 16])
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let search = |q: &str| format!("/api/archives/{}/search/items?q={q}", f.archive);
    let events = f.json(&search("is:event")).await;
    assert_eq!(events["total"], 1, "two copies, one event");
    assert_eq!(events["items"].as_array().unwrap().len(), 1);
    assert_eq!(events["items"][0]["group_id"], group);
    assert_eq!(
        f.json(&search("is:contact")).await["items"][0]["kind"],
        "contact"
    );
    assert_eq!(f.json(&search("with:bob")).await["total"], 1);
    assert_eq!(f.json(&search("room")).await["items"][0]["group_id"], group);
    let mail = f
        .json(&format!("/api/archives/{}/search?q=is:event", f.archive))
        .await;
    assert_eq!(mail["total"], 0, "mail search finds no events");
}

const INVITE: &[u8] = include_bytes!("../tests/fixtures/invite.eml");
const INVITE_REPLY: &[u8] = include_bytes!("../tests/fixtures/invite-reply.eml");

#[tokio::test(flavor = "multi_thread")]
async fn shows_invitations_in_messages_and_messages_in_events() {
    let f = fixture().await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let mut account = f.store.list_accounts(archive).await.unwrap().remove(0);
    account.address = Some("Bob@Example.org".into());
    f.store.update_account(&account).await.unwrap();
    let mut ids = Vec::new();
    for raw in [INVITE_REPLY, INVITE] {
        ids.push(
            f.ingest
                .ingest(archive, Bytes::from_static(raw), &Placement::default())
                .await
                .unwrap()
                .message_id,
        );
    }
    let (reply, invite) = (ids[0], ids[1]);

    let detail = f.json(&format!("/api/messages/{invite}")).await;
    let invitations = detail["invitations"].as_array().unwrap();
    assert_eq!(invitations.len(), 1);
    let invitation = &invitations[0];
    assert_eq!(invitation["uid"], "rota-2025@example.com");
    assert_eq!(invitation["method"], "request");
    assert_eq!(invitation["version"], 1);
    assert_eq!(invitation["version_count"], 2);
    assert_eq!(invitation["updates_after"], 0, "a reply is not an update");
    assert_eq!(invitation["summary"], "Lighthouse rota");
    assert_eq!(invitation["start"]["time_zone"], "America/New_York");
    assert_eq!(invitation["start"]["value"], "2025-11-07T09:00:00");
    assert_eq!(invitation["start"]["utc"], 1_762_524_000_000_i64);
    assert_eq!(invitation["organizer"]["name"], "Ada Quill");
    assert_eq!(invitation["my_status"], "accepted");
    assert_eq!(invitation["cancelled"], false);
    let item = invitation["item_id"].as_str().unwrap().to_owned();
    assert_eq!(
        f.json(&format!("/api/messages/{reply}")).await["invitations"][0]["method"],
        "reply"
    );
    let plain = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == "Re: Weekly news")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        f.json(&format!("/api/messages/{plain}")).await["invitations"],
        json!([])
    );

    let detail = f.json(&format!("/api/items/{item}")).await;
    let versions = detail["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["method"], "request");
    assert_eq!(versions[0]["messages"][0]["id"], invite.to_string());
    assert_eq!(
        versions[0]["messages"][0]["subject"],
        "Invitation: Lighthouse rota"
    );
    assert_eq!(versions[1]["method"], "reply");
    assert_eq!(versions[1]["messages"][0]["id"], reply.to_string());
    assert_eq!(versions[1]["changes"][0]["field"], "answer");
    assert_eq!(
        versions[1]["changes"][0]["after"],
        "Bob Stone <bob@example.org> (accepted)"
    );
    let main = &detail["data"]["components"][0];
    assert_eq!(main["summary"], "Lighthouse rota");
    assert_eq!(main["attendees"][0]["partstat"], "ACCEPTED");
    assert_eq!(detail["item"]["title"], "Lighthouse rota");
}

const CARD_WORK: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada-work\r\nFN:Ada Quill\r\nEMAIL:ada@example.com\r\nTEL:+1 555 0142\r\nORG:Lighthouse Co\r\nEND:VCARD\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn groups_people_with_sources_mail_and_meetings() {
    let f = fixture().await;
    let (_, ada_card, review) = pim_fixture(&f).await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let work = f
        .store
        .upsert_collection(holtmail_store::NewCollection {
            archive_id: archive,
            account_id: None,
            kind: holtmail_store::CollectionKind::Contacts,
            remote_id: "work.vcf".into(),
            name: "Work".into(),
            color: None,
        })
        .await
        .unwrap();
    let work_card = f
        .ingest
        .ingest_item(holtmail_ingest::IncomingItem {
            collection: work.id,
            remote_id: "ada-work".into(),
            kind: holtmail_pim::ItemKind::Contact,
            format: holtmail_pim::Format::of(holtmail_pim::ItemKind::Contact),
            raw: Bytes::from_static(CARD_WORK.as_bytes()),
            etag: None,
            seen_at: holtmail_core::Timestamp::from_secs(1_700_000_000),
        })
        .await
        .unwrap();
    let rebuilt = holtmail_people::rebuild(&*f.store, archive).await.unwrap();
    assert_eq!(
        rebuilt.people, 1,
        "one message each is not enough for mail alone"
    );

    let list = f.json(&format!("/api/archives/{}/people", f.archive)).await;
    assert_eq!(list["total"], 1);
    let ada = &list["items"][0];
    assert_eq!(ada["name"], "Ada Quill");
    assert_eq!(ada["cards"], 2);
    assert_eq!(ada["messages"], 2);
    assert_eq!(ada["differences"], true);
    assert_eq!(ada["phones"], json!(["+15550142", "+15550199"]));
    let person = ada["id"].as_str().unwrap().to_owned();
    for (query, total) in [
        ("has_differences=true", 1),
        ("source=mail", 0),
        ("source=contacts", 1),
        ("q=QUILL", 1),
        ("q=%2B1555", 1),
        ("q=nobody", 0),
    ] {
        let page = f
            .json(&format!("/api/archives/{}/people?{query}", f.archive))
            .await;
        assert_eq!(page["total"], total, "{query}");
        assert_eq!(page["items"].as_array().unwrap().len(), total, "{query}");
    }
    let page = f
        .json(&format!(
            "/api/archives/{}/people?source={}",
            f.archive, work.id
        ))
        .await;
    assert_eq!(page["total"], 1);
    for bad in ["source=somewhere", "cursor=garbage"] {
        let (status, _, _) = f
            .get(&format!("/api/archives/{}/people?{bad}", f.archive))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }

    let detail = f.json(&format!("/api/people/{person}")).await;
    let field = |name: &str| {
        detail["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .cloned()
            .unwrap()
    };
    let phone = field("phone");
    assert_eq!(phone["differs"], true);
    assert_eq!(phone["values"].as_array().unwrap().len(), 2);
    let email = field("email");
    assert_eq!(email["differs"], false);
    assert_eq!(email["values"].as_array().unwrap().len(), 1);
    let sources: Vec<&str> = email["values"][0]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["collection_name"].as_str().unwrap())
        .collect();
    assert_eq!(sources.len(), 2);
    assert!(sources.contains(&"Work") && sources.contains(&"people.vcf"));
    assert_eq!(
        field("organization")["differs"],
        false,
        "one card is silent"
    );
    assert_eq!(detail["cards"].as_array().unwrap().len(), 2);
    let card_ids: Vec<&str> = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["item_id"].as_str().unwrap())
        .collect();
    assert!(card_ids.contains(&ada_card.as_str()));
    assert!(card_ids.contains(&work_card.item_id.to_string().as_str()));
    assert_eq!(
        detail["links"],
        json!([{
            "from_item_id": ada_card.min(work_card.item_id.to_string()),
            "to_item_id": detail["links"][0]["to_item_id"],
            "kind": "email",
            "value": "ada@example.com"
        }])
    );
    let mail = &detail["correspondence"];
    assert_eq!(mail["messages"], 2);
    assert_eq!(mail["to_them"], 2);
    assert_eq!(mail["from_them"], 0);
    assert_eq!(mail["years"], json!([{ "year": 2025, "messages": 2 }]));
    assert_eq!(mail["threads"].as_array().unwrap().len(), 1);
    assert_eq!(mail["threads"][0]["messages"], 2);
    assert_eq!(mail["threads"][0]["subject"], "Re: Weekly news");
    assert_eq!(
        mail["attachments"][0]["attachment"]["filename"],
        "evil.html"
    );
    assert_eq!(detail["meetings"][0]["item_id"], review);
    assert_eq!(detail["meetings"][0]["role"], "organizer");

    let plain = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == "Re: Weekly news")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        f.json(&format!("/api/messages/{plain}")).await["people"],
        json!([{ "email": "ada@example.com", "person_id": person }])
    );

    for missing in [
        format!(
            "/api/people/{}",
            holtmail_core::PersonId::from_hash([1; 16])
        ),
        "/api/people/not-an-id".to_owned(),
    ] {
        let (status, _, _) = f.get(&missing).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    }

    let (status, _, _) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    for uri in [
        format!("/api/archives/{}/people", f.archive),
        format!("/api/people/{person}"),
    ] {
        let (status, _, _) = f
            .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn splits_and_joins_people_by_hand() {
    let f = fixture().await;
    let (_, ada_card, _) = pim_fixture(&f).await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let work = f
        .store
        .upsert_collection(holtmail_store::NewCollection {
            archive_id: archive,
            account_id: None,
            kind: holtmail_store::CollectionKind::Contacts,
            remote_id: "work.vcf".into(),
            name: "Work".into(),
            color: None,
        })
        .await
        .unwrap();
    let card = |remote: &str, raw: &'static str| holtmail_ingest::IncomingItem {
        collection: work.id,
        remote_id: remote.into(),
        kind: holtmail_pim::ItemKind::Contact,
        format: holtmail_pim::Format::of(holtmail_pim::ItemKind::Contact),
        raw: Bytes::from_static(raw.as_bytes()),
        etag: None,
        seen_at: holtmail_core::Timestamp::from_secs(1_700_000_000),
    };
    let work_card = f
        .ingest
        .ingest_item(card("ada-work", CARD_WORK))
        .await
        .unwrap()
        .item_id
        .to_string();
    let quill = f
        .ingest
        .ingest_item(card(
            "quill",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:quill\r\nFN:A. Quill\r\nEMAIL:quill@example.org\r\nEND:VCARD\r\n",
        ))
        .await
        .unwrap()
        .item_id
        .to_string();
    holtmail_people::rebuild(&*f.store, archive).await.unwrap();
    let list = f.json(&format!("/api/archives/{}/people", f.archive)).await;
    assert_eq!(list["total"], 2);
    let id_of = |list: &Value, cards: u64| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["cards"] == cards)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let ada = id_of(&list, 2);
    let alone = id_of(&list, 1);

    let (status, _, body) = f
        .send(post_json(
            &format!("/api/people/{ada}/split"),
            json!({ "cards": [work_card] }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let split: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(split["kind"], "split");
    assert_eq!(split["keys"], json!([format!("item:{work_card}")]));
    assert_eq!(split["from"], json!([format!("item:{ada_card}")]));
    assert_eq!(split["people"][0], ada.as_str(), "the rest keeps the id");
    let moved = split["people"][1].as_str().unwrap().to_owned();
    assert_ne!(moved, ada);
    assert_eq!(
        f.json(&format!("/api/archives/{}/people", f.archive)).await["total"],
        3
    );
    let detail = f.json(&format!("/api/people/{moved}")).await;
    assert_eq!(detail["overrides"][0]["id"], split["id"]);
    assert_eq!(detail["cards"].as_array().unwrap().len(), 1);

    for (body, expected) in [
        (json!({ "cards": [] }), StatusCode::BAD_REQUEST),
        (json!({ "cards": [ada_card] }), StatusCode::BAD_REQUEST),
        (json!({ "cards": [quill] }), StatusCode::BAD_REQUEST),
        (json!({ "cards": ["nope"] }), StatusCode::BAD_REQUEST),
    ] {
        let (status, _, _) = f
            .send(post_json(&format!("/api/people/{ada}/split"), body.clone()))
            .await;
        assert_eq!(status, expected, "{body}");
    }

    let (status, _, body) = f
        .send(post_json(
            "/api/people/join",
            json!({ "people": [ada, alone] }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let join: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(join["people"], json!([ada]));
    let detail = f.json(&format!("/api/people/{ada}")).await;
    assert_eq!(detail["cards"].as_array().unwrap().len(), 2);
    assert!(
        detail["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["kind"] == "hand" && l["value"] == join["id"])
    );

    let (status, _, _) = f
        .send(post_json(
            "/api/people/join",
            json!({ "people": [ada, moved] }),
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "the split keeps them apart");
    for body in [json!({ "people": [ada] }), json!({ "people": [ada, ada] })] {
        let (status, _, _) = f.send(post_json("/api/people/join", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let overrides = f
        .json(&format!("/api/archives/{}/people/overrides", f.archive))
        .await;
    assert_eq!(overrides.as_array().unwrap().len(), 2);
    holtmail_people::rebuild(&*f.store, archive).await.unwrap();
    assert_eq!(
        f.json(&format!("/api/archives/{}/people", f.archive)).await["total"],
        2,
        "a rebuild keeps both"
    );

    let (status, _, _) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let (status, _, _) = f
        .send_as(
            &member,
            post_json("/api/people/join", json!({ "people": [ada, moved] })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .send_as(
            &member,
            request(
                "DELETE",
                &format!("/api/people/overrides/{}", split["id"].as_str().unwrap()),
                json!({}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, body) = f
        .send(request(
            "DELETE",
            &format!("/api/people/overrides/{}", split["id"].as_str().unwrap()),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let undone: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(undone["people"], json!([ada]));
    assert_eq!(
        f.json(&format!("/api/archives/{}/people", f.archive)).await["total"],
        1
    );
    let (status, _, _) = f
        .send(request(
            "DELETE",
            &format!("/api/people/overrides/{}", split["id"].as_str().unwrap()),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn exports_contacts_and_calendars() {
    let f = fixture().await;
    pim_fixture(&f).await;
    let (status, headers, body) = f
        .get(&format!(
            "/api/archives/{}/contacts/export?format=vcf",
            f.archive
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/vcard")
    );
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("contacts.vcf")
    );
    let cards = holtmail_pim::split(&body);
    assert!(!cards.is_empty());
    assert!(
        cards
            .iter()
            .all(|c| c.kind == holtmail_pim::ItemKind::Contact)
    );

    let (status, headers, body) = f
        .get(&format!(
            "/api/archives/{}/calendar/export?format=zip",
            f.archive
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
    let zip = zip::ZipArchive::new(std::io::Cursor::new(body.to_vec())).unwrap();
    assert!(zip.file_names().any(|n| n.unwrap() == "manifest.json"));

    let (status, headers, body) = f
        .get(&format!(
            "/api/archives/{}/calendar/export?format=ics&from=0&to=4102444800000",
            f.archive
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/calendar")
    );
    assert!(body.starts_with(b"BEGIN:VCALENDAR"));

    for (uri, expected) in [
        ("contacts/export?format=pdf", StatusCode::BAD_REQUEST),
        ("contacts/export?format=ics", StatusCode::BAD_REQUEST),
        ("calendar/export?format=ics&from=1", StatusCode::BAD_REQUEST),
        (
            "calendar/export?format=ics&from=2&to=1",
            StatusCode::BAD_REQUEST,
        ),
        (
            "contacts/export?format=vcf&collection=nope",
            StatusCode::NOT_FOUND,
        ),
        (
            "contacts/export?format=vcf&person=0190d6e4-0000-7000-8000-000000000000",
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, _, _) = f.get(&format!("/api/archives/{}/{uri}", f.archive)).await;
        assert_eq!(status, expected, "{uri}");
    }

    let (status, _, _) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let (status, _, _) = f
        .send_as(
            &member,
            Request::get(format!(
                "/api/archives/{}/contacts/export?format=vcf",
                f.archive
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "members read granted archives only"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_and_verifies_the_ledger() {
    let f = fixture().await;
    let (status, _, body) = f.get("/api/ledger").await;
    assert_eq!(status, StatusCode::OK);
    let ledger: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(ledger["head"]["seq"], 2);
    assert_eq!(ledger["head"]["event"], "archived");
    assert_eq!(ledger["head"]["hash"].as_str().unwrap().len(), 64);
    assert!(ledger["last_verification"].is_null());

    let hash = ledger["head"]["hash"].as_str().unwrap().to_owned();
    for bad in ["2", "0:abc", &format!("x:{hash}"), &format!("0:{hash}")] {
        let (status, _, _) = f
            .send(post_json("/api/ledger/verify", json!({ "anchor": bad })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _, body) = f
        .send(post_json(
            "/api/ledger/verify",
            json!({ "blobs": true, "anchor": format!("2:{hash}") }),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(task["kind"], "ledger_verify");
    let asked = f.mailboxes.verifications.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].blobs);
    assert_eq!(
        asked[0].anchor.map(|(seq, h)| (seq, h.to_string())),
        Some((2, hash))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reviews_and_confirms_deletions() {
    let f = fixture().await;
    let url = format!("/api/archives/{}/deletions", f.archive);
    for bad in [
        json!({}),
        json!({ "query": "a", "message_ids": [] }),
        json!({ "message_ids": ["not-an-id"] }),
        json!({ "query": "nothing" }),
    ] {
        let (status, _, _) = f.send(post_json(&url, bad.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _, body) = f
        .send(post_json(&url, json!({ "query": "from:news@example.com" })))
        .await;
    assert_eq!(status, StatusCode::OK);
    let review: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (review["id"].as_str(), review["count"].as_u64()),
        (Some("r1"), Some(2))
    );
    assert_eq!(
        f.mailboxes.deletions.lock().unwrap()[0].1,
        DeletionSelection::Query("from:news@example.com".into())
    );
    let (status, _, _) = f
        .send(post_json(
            "/api/deletions/r1/confirm",
            json!({ "count": 3 }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = f
        .send(post_json(
            "/api/deletions/r1/confirm",
            json!({ "count": 2 }),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(task["kind"], "delete_messages");
}

#[tokio::test(flavor = "multi_thread")]
async fn manages_webhooks() {
    let f = fixture().await;
    let (status, _, _) = f
        .send(post_json(
            "/api/webhooks",
            json!({ "url": "ftp://hooks.example.com", "events": [] }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = f
        .send(post_json(
            "/api/webhooks",
            json!({ "url": "https://hooks.example.com/in", "events": ["task.finished"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["secret"], "whsec_first");
    let id = created["webhook"]["id"].as_str().unwrap().to_owned();
    assert!(created["webhook"].get("sealed_secret").is_none());

    let (status, _, body) = f.get("/api/webhooks").await;
    assert_eq!(status, StatusCode::OK);
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        list["events"]
            .as_array()
            .unwrap()
            .contains(&json!("ledger.verified"))
    );
    assert_eq!(list["webhooks"][0]["url"], "https://hooks.example.com/in");
    assert!(list["webhooks"][0]["last_delivery"].is_null());
    assert!(
        !String::from_utf8_lossy(&body).contains("whsec"),
        "the secret was listed"
    );

    let (status, _, body) = f
        .send(request(
            "PATCH",
            &format!("/api/webhooks/{id}"),
            json!({ "enabled": false, "events": ["messages.deleted", "task.finished"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let changed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(changed["enabled"], false);
    assert_eq!(
        changed["events"],
        json!(["messages.deleted", "task.finished"])
    );
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/webhooks/{id}"),
            json!({ "events": ["mail.read"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) = f
        .send(post_json(&format!("/api/webhooks/{id}/secret"), json!({})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["secret"],
        "whsec_second"
    );
    let (status, _, _) = f
        .send(post_json(&format!("/api/webhooks/{id}/test"), json!({})))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(f.mailboxes.tested.lock().unwrap().len(), 1);
    let (status, _, body) = f.get(&format!("/api/webhooks/{id}/deliveries")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!([]));

    for path in [
        format!("/api/webhooks/{}", holtmail_core::WebhookId::new()),
        "/api/webhooks/not-an-id".to_owned(),
    ] {
        let (status, _, _) = f.send(request("DELETE", &path, json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let (status, _, _) = f
        .send(request("DELETE", &format!("/api/webhooks/{id}"), json!({})))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn says_when_smtp_is_off() {
    let f = fixture().await;
    let (status, _, body) = f.get("/api/smtp").await;
    assert_eq!(status, StatusCode::OK);
    let smtp: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(smtp["enabled"], false);
    assert_eq!(smtp["routes"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_a_message_as_pdf() {
    let f = fixture().await;
    let (_, _, body) = f
        .get(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let page: Value = serde_json::from_slice(&body).unwrap();
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, headers, body) = f.get(&format!("/api/messages/{id}/pdf")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    assert!(body.starts_with(b"%PDF-"));
    let (status, _, _) = f
        .get(&format!(
            "/api/messages/{}/pdf",
            holtmail_core::MessageId::new()
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
