//! An HTTP client for Holtmail's API that keeps the session cookie and sends the instance's
//! `Origin` on every request, as the web app does.

use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::Holtmail;

/// The owner every [`Api::setup_owner`] creates; the password is also scanned for in logs.
pub const OWNER_EMAIL: &str = "owner@example.com";
/// The owner's password.
pub const OWNER_PASSWORD: &str = "it-owner-password-1";

/// An answer: its status and its body as JSON (`Null` when empty or not JSON).
#[derive(Debug, Clone)]
pub struct Reply {
    /// HTTP status.
    pub status: StatusCode,
    /// The body as JSON.
    pub body: Value,
}

impl Reply {
    /// The body, after checking the status is 2xx.
    #[track_caller]
    pub fn ok(self) -> Value {
        assert!(
            self.status.is_success(),
            "expected success, got {}: {}",
            self.status,
            self.body
        );
        self.body
    }
}

/// A signed-in or anonymous client of one instance.
#[derive(Clone)]
pub struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    /// A client without a session for `holtmail`.
    pub fn new(holtmail: &Holtmail) -> Self {
        Self::for_url(&holtmail.url())
    }

    /// A client without a session for the instance at `base`, sending `base` as `Origin`.
    pub fn for_url(base: &str) -> Self {
        Self::for_url_with(base, reqwest::Client::builder())
    }

    /// A client without a session for the instance at `base` behind the stack's TLS (Caddy),
    /// trusting the stack's CA.
    pub fn behind_stack_tls(base: &str) -> Self {
        let pem = std::fs::read(crate::ca_file()).expect("the stack's CA; run `mise run it`");
        let roots = reqwest::Certificate::from_pem_bundle(&pem).expect("a PEM CA");
        Self::for_url_with(base, reqwest::Client::builder().tls_certs_merge(roots))
    }

    fn for_url_with(base: &str, builder: reqwest::ClientBuilder) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_str(base).expect("valid origin"));
        let client = builder
            .cookie_store(true)
            .default_headers(headers)
            .timeout(Duration::from_secs(60))
            .build()
            .expect("client");
        Self {
            base: base.trim_end_matches('/').to_owned(),
            client,
        }
    }

    /// Sends `body` as JSON (or nothing for `None`) to `path` under the instance.
    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Reply {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        let response = request.send().await.expect("the instance answers");
        let status = response.status();
        let bytes = response.bytes().await.expect("a body");
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    /// `GET path`.
    pub async fn get(&self, path: &str) -> Reply {
        self.request(Method::GET, path, None).await
    }

    /// `POST path` with a JSON body.
    pub async fn post(&self, path: &str, body: Value) -> Reply {
        self.request(Method::POST, path, Some(&body)).await
    }

    /// `PATCH path` with a JSON body.
    pub async fn patch(&self, path: &str, body: Value) -> Reply {
        self.request(Method::PATCH, path, Some(&body)).await
    }

    /// `DELETE path` without a body, declared JSON as the web app sends it (the server refuses
    /// writes without a JSON or upload content type).
    pub async fn delete(&self, path: &str) -> Reply {
        let response = self
            .client
            .delete(format!("{}{path}", self.base))
            .header(CONTENT_TYPE, "application/json")
            .send()
            .await
            .expect("the instance answers");
        let status = response.status();
        let bytes = response.bytes().await.expect("a body");
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    /// Creates the owner ([`OWNER_EMAIL`]) with the setup token from the logs and keeps its
    /// session.
    pub async fn setup_owner(holtmail: &Holtmail) -> Self {
        let api = Self::new(holtmail);
        let token = holtmail.setup_token().expect("the logs hold a setup link");
        api.post(
            "/api/setup",
            json!({
                "token": token,
                "email": OWNER_EMAIL,
                "name": "Owner Example",
                "password": OWNER_PASSWORD,
            }),
        )
        .await
        .ok();
        api
    }

    /// Signs in with a password and keeps the session.
    pub async fn sign_in(holtmail: &Holtmail, email: &str, password: &str) -> Self {
        Self::sign_in_url(&holtmail.url(), email, password).await
    }

    /// Signs in to the instance at `base` with a password and keeps the session.
    pub async fn sign_in_url(base: &str, email: &str, password: &str) -> Self {
        let api = Self::for_url(base);
        api.post(
            "/api/login",
            json!({ "email": email, "password": password }),
        )
        .await
        .ok();
        api
    }

    /// Creates an archive and returns its id.
    pub async fn create_archive(&self, name: &str) -> String {
        let archive = self
            .post("/api/archives", json!({ "name": name }))
            .await
            .ok();
        id(&archive)
    }

    /// Adds an IMAP account to `archive` (the server signs in first) and returns it.
    pub async fn add_imap_account(&self, archive: &str, account: Value) -> Reply {
        self.post(&format!("/api/archives/{archive}/accounts"), account)
            .await
    }

    /// The accounts of `archive`.
    pub async fn accounts(&self, archive: &str) -> Vec<Value> {
        let reply = self
            .get(&format!("/api/archives/{archive}/accounts"))
            .await
            .ok();
        reply.as_array().cloned().unwrap_or_default()
    }

    /// The account `account` of `archive`.
    pub async fn account(&self, archive: &str, account: &str) -> Value {
        self.accounts(archive)
            .await
            .into_iter()
            .find(|a| a["id"] == account)
            .expect("the account is listed")
    }

    /// Starts a sync of `account` and waits until a run that ended after the request is
    /// recorded; returns the account as listed then.
    pub async fn sync(&self, archive: &str, account: &str) -> Value {
        let asked = crate::data::now().timestamp_millis();
        self.post(&format!("/api/accounts/{account}/sync"), json!({}))
            .await
            .ok();
        self.wait_for_run(archive, account, asked, Duration::from_secs(120))
            .await
    }

    /// Waits until `account` is not running and its last run ended at or after `since` (Unix
    /// milliseconds).
    pub async fn wait_for_run(
        &self,
        archive: &str,
        account: &str,
        since: i64,
        timeout: Duration,
    ) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let listed = self.account(archive, account).await;
            let status = &listed["status"];
            let finished = status["finished_at"].as_i64().unwrap_or(0);
            if status["running"] == false && finished >= since {
                return listed;
            }
            assert!(
                Instant::now() < deadline,
                "the sync did not finish in time: {listed}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Waits until `account` has finished a run since it was added (Holtmail starts one at once)
    /// and is not running.
    pub async fn settle(&self, archive: &str, account: &str) -> Value {
        self.wait_for_run(archive, account, 1, Duration::from_secs(120))
            .await
    }

    /// Adds an IMAP account and returns its id, failing the test when it is refused.
    pub async fn add_imap(&self, archive: &str, account: Value) -> String {
        id(&self.add_imap_account(archive, account).await.ok())
    }

    /// `PATCH /api/accounts/{account}` with `change`.
    pub async fn update_account(&self, account: &str, change: Value) -> Reply {
        self.patch(&format!("/api/accounts/{account}"), change)
            .await
    }

    /// The folders of `archive`.
    pub async fn folders(&self, archive: &str) -> Vec<Value> {
        let reply = self
            .get(&format!("/api/archives/{archive}/folders"))
            .await
            .ok();
        reply.as_array().cloned().unwrap_or_default()
    }

    /// One message with its locations.
    pub async fn message(&self, message: &str) -> Value {
        self.get(&format!("/api/messages/{message}")).await.ok()
    }

    /// The stored bytes of a message.
    pub async fn raw(&self, message: &str) -> Vec<u8> {
        let response = self
            .client
            .get(format!("{}/api/messages/{message}/raw", self.base))
            .send()
            .await
            .expect("the instance answers");
        assert!(response.status().is_success(), "{}", response.status());
        response.bytes().await.expect("a body").to_vec()
    }

    /// The tasks the instance lists.
    pub async fn tasks(&self) -> Vec<Value> {
        let reply = self.get("/api/tasks").await.ok();
        reply.as_array().cloned().unwrap_or_default()
    }

    /// The log lines of the latest sync task of `account`.
    pub async fn sync_log(&self, account: &str) -> Vec<String> {
        self.task_log("sync", account).await
    }

    /// The log lines of the latest task of `kind` (such as `contacts_calendars`) of `account`.
    pub async fn task_log(&self, kind: &str, account: &str) -> Vec<String> {
        let mut tasks: Vec<Value> = self
            .tasks()
            .await
            .into_iter()
            .filter(|t| t["kind"] == kind && t["account_id"] == account)
            .collect();
        tasks.sort_by_key(|t| t["started_at"].as_i64().unwrap_or(0));
        tasks
            .last()
            .and_then(|t| t["log"].as_array())
            .map(|lines| {
                lines
                    .iter()
                    .map(|l| l["text"].as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Adds a JMAP account to `archive` (the server signs in first).
    pub async fn add_jmap_account(&self, archive: &str, account: Value) -> Reply {
        self.post(&format!("/api/archives/{archive}/accounts/jmap"), account)
            .await
    }

    /// Starts a sync of `account` and waits until its contacts and calendars run that started
    /// after the request has ended; returns the account as listed then.
    pub async fn sync_contacts(&self, archive: &str, account: &str) -> Value {
        let asked = crate::data::now().timestamp_millis();
        self.post(&format!("/api/accounts/{account}/sync"), json!({}))
            .await
            .ok();
        self.wait_for_contacts(archive, account, asked).await
    }

    /// Waits until the contacts and calendars run of `account` ended at or after `since` (Unix
    /// milliseconds) and the mail sync is not running.
    pub async fn wait_for_contacts(&self, archive: &str, account: &str, since: i64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let listed = self.account(archive, account).await;
            let run = &listed["status"]["contacts_calendars"];
            if listed["status"]["running"] == false
                && run["running"] == false
                && run["finished_at"].as_i64().unwrap_or(0) >= since
            {
                return listed;
            }
            assert!(
                Instant::now() < deadline,
                "the contacts and calendars sync did not finish in time: {listed}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// The address books and calendars of `archive`, removed ones too.
    pub async fn collections(&self, archive: &str) -> Vec<Value> {
        let reply = self
            .get(&format!("/api/archives/{archive}/collections"))
            .await
            .ok();
        reply.as_array().cloned().unwrap_or_default()
    }

    /// Every contact (`list` = `contacts`) or event (`events`) of `archive`, removed ones too.
    pub async fn items(&self, archive: &str, list: &str) -> Vec<Value> {
        let mut items = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut path = format!("/api/archives/{archive}/{list}?removed=include&limit=500");
            if let Some(after) = &after {
                path.push_str(&format!("&after={}", encode(after)));
            }
            let page = self.get(&path).await.ok();
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            match page["next"].as_str() {
                Some(next) => after = Some(next.to_owned()),
                None => return items,
            }
        }
    }

    /// Every message of `archive`, following `next` through all pages.
    pub async fn messages(&self, archive: &str) -> Vec<Value> {
        let mut items = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let path = match &after {
                Some(after) => format!(
                    "/api/archives/{archive}/messages?limit=500&after={}",
                    encode(after)
                ),
                None => format!("/api/archives/{archive}/messages?limit=500"),
            };
            let page = self.get(&path).await.ok();
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            match page["next"].as_str() {
                Some(next) => after = Some(next.to_owned()),
                None => return items,
            }
        }
    }

    /// Turns contacts and calendars on for `account`, reading both from `url` (or from what
    /// the account finds itself when `None`).
    pub async fn enable_contacts(&self, account: &str, url: Option<&str>) -> Reply {
        self.update_account(
            account,
            json!({ "contacts_calendars": {
                "enabled": true,
                "contacts_url": url,
                "calendars_url": url,
            } }),
        )
        .await
    }

    /// The calendar of `archive` between `from` and `to` (Unix milliseconds), removed events
    /// too when `show_removed`.
    pub async fn calendar(&self, archive: &str, from: i64, to: i64, show_removed: bool) -> Value {
        self.get(&format!(
            "/api/archives/{archive}/calendar?from={from}&to={to}&show_removed={show_removed}"
        ))
        .await
        .ok()
    }

    /// Every person of `archive`, following `next` through all pages.
    pub async fn people(&self, archive: &str) -> Vec<Value> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut path = format!("/api/archives/{archive}/people?limit=200");
            if let Some(cursor) = &cursor {
                path.push_str(&format!("&cursor={}", encode(cursor)));
            }
            let page = self.get(&path).await.ok();
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            match page["next"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return items,
            }
        }
    }

    /// `GET path`: its status and body as bytes.
    pub async fn get_bytes(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .expect("the instance answers");
        let status = response.status();
        (status, response.bytes().await.expect("a body").to_vec())
    }

    /// `GET path` as bytes, failing the test unless it is 2xx.
    pub async fn bytes(&self, path: &str) -> Vec<u8> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .expect("the instance answers");
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response.bytes().await.expect("a body").to_vec()
    }

    /// `GET path` streamed into `file`: the status and how many bytes were written.
    pub async fn download(&self, path: &str, file: &std::path::Path) -> (StatusCode, u64) {
        use std::io::Write;
        let mut response = self
            .client
            .get(format!("{}{path}", self.base))
            .timeout(Duration::from_secs(1_800))
            .send()
            .await
            .expect("the instance answers");
        let status = response.status();
        let mut out = std::io::BufWriter::new(std::fs::File::create(file).expect("the file"));
        let mut written = 0;
        while let Some(chunk) = response.chunk().await.expect("the body") {
            out.write_all(&chunk).expect("written");
            written += chunk.len() as u64;
        }
        out.flush().expect("flushed");
        (status, written)
    }

    /// Uploads `bytes` as the file `name` into `archive`, waits until its import task has
    /// finished, and returns the import account's id and the task.
    pub async fn import(&self, archive: &str, name: &str, bytes: Vec<u8>) -> (String, Value) {
        self.import_within(archive, name, bytes, Duration::from_secs(120))
            .await
    }

    /// [`Api::import`], waiting at most `timeout` for the task.
    pub async fn import_within(
        &self,
        archive: &str,
        name: &str,
        bytes: Vec<u8>,
        timeout: Duration,
    ) -> (String, Value) {
        let asked = crate::data::now().timestamp_millis();
        let response = self
            .client
            .post(format!(
                "{}/api/archives/{archive}/imports?name={}",
                self.base,
                encode(name)
            ))
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .expect("the instance answers");
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "import {name}: {status} {body}");
        let account = id(&body);
        let deadline = Instant::now() + timeout;
        loop {
            let task = self.tasks().await.into_iter().find(|t| {
                t["kind"] == "import"
                    && t["account_id"] == account.as_str()
                    && t["started_at"].as_i64().unwrap_or(0) >= asked - 1_000
                    && t["finished_at"].is_i64()
            });
            if let Some(task) = task {
                return (account, task);
            }
            assert!(
                Instant::now() < deadline,
                "the import of {name} did not finish"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// The `id` field of an API object.
#[track_caller]
pub fn id(value: &Value) -> String {
    value["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {value}"))
        .to_owned()
}

/// Percent-encodes everything but unreserved characters, for query values.
pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
