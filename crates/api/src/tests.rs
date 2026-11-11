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

mod devices;
mod invitations;
mod mail;
mod pim_edit;
mod rules;
mod send;
mod serving;
mod write;

const NEWSLETTER: &[u8] = b"From: News <news@example.com>\r\nTo: ada@example.com\r\nSubject: Weekly news\r\nMessage-ID: <n1@example.com>\r\nDate: Mon, 10 Mar 2025 08:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=r\r\n\r\n--r\r\nContent-Type: text/html\r\n\r\n<p onclick=\"x()\">Hello <img src=\"cid:logo@example.com\"><img src=\"https://tracker.example.com/p.gif\"></p>\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <logo@example.com>\r\nContent-Disposition: inline\r\n\r\nPNGDATA\r\n--r\r\nContent-Type: text/html; name=\"evil.html\"\r\nContent-Disposition: attachment; filename=\"evil.html\"\r\n\r\n<script>alert(1)</script>\r\n--r--\r\n";
const PLAIN: &[u8] = b"From: Bob <bob@example.org>\r\nTo: ada@example.com\r\nSubject: Re: Weekly news\r\nIn-Reply-To: <n1@example.com>\r\nDate: Tue, 11 Mar 2025 08:00:00 +0000\r\n\r\nThanks <3\r\n";

fn holtmail_api_contacts(dav: &Value) -> ContactsCalendars {
    ContactsCalendars {
        enabled: dav["enabled"] == json!(true),
        contacts_url: dav["contacts_url"].as_str().map(str::to_owned),
        calendars_url: dav["calendars_url"].as_str().map(str::to_owned),
        needs_consent: false,
        protocol: Default::default(),
        calendars_protocol: Default::default(),
        interval_minutes: dav["interval"].as_u64(),
        has_dav_password: dav["dav_password"] == json!(true),
        caldav_offer: None,
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
    changed: std::sync::Mutex<Vec<holtmail_core::ArchiveId>>,
    fresh: std::sync::OnceLock<FreshInsights>,
    freshened: std::sync::Mutex<Vec<holtmail_core::MessageId>>,
    scheduling: std::sync::Mutex<Option<crate::SchedulingProbe>>,
}

struct FreshInsights {
    ingest: Ingest,
    fingerprint: String,
    deriver: Arc<holtmail_insights::Deriver>,
}

fn backend(error: impl std::fmt::Display) -> MailboxError {
    MailboxError::Backend(error.to_string())
}

struct PlainSecrets;

impl holtmail_secret::SecretBox for PlainSecrets {
    fn seal(&self, _: &str, plaintext: &[u8]) -> Result<Vec<u8>, holtmail_secret::SecretError> {
        Ok(plaintext.to_vec())
    }

    fn open(&self, _: &str, sealed: &[u8]) -> Result<Vec<u8>, holtmail_secret::SecretError> {
        Ok(sealed.to_vec())
    }
}

/// The real manager over a fake service that takes the key `good-key`.
struct FakeAliases(holtmail_aliases::AliasManager);

impl FakeAliases {
    fn new(store: Arc<dyn Store>, directory: Arc<holtmail_alias::testing::FakeDirectory>) -> Self {
        let caps = holtmail_alias::AliasDirectory::capabilities(&*directory);
        let connect: holtmail_aliases::Connect = Arc::new(move |_, _, key| {
            if key == "good-key" {
                Ok(directory.clone() as Arc<dyn holtmail_alias::AliasDirectory>)
            } else {
                Err(holtmail_alias::AliasError::Auth)
            }
        });
        Self(
            holtmail_aliases::AliasManager::new(
                store,
                Arc::new(PlainSecrets),
                vec![holtmail_aliases::ProviderInfo {
                    id: "fake".into(),
                    name: "Fake".into(),
                    default_url: Some("https://alias.example".into()),
                    capabilities: Some(caps),
                    key_label: "API key".into(),
                    key_help: "Settings".into(),
                    detects: false,
                }],
                connect,
            )
            .with_detectors(vec![Arc::new(
                holtmail_alias_simplelogin::SimpleLoginDetector::new(&[]),
            )]),
        )
    }
}

fn manage_error(error: holtmail_aliases::ManageError) -> MailboxError {
    match error {
        holtmail_aliases::ManageError::Invalid(m) => MailboxError::Invalid(m),
        holtmail_aliases::ManageError::Store(e) => MailboxError::Backend(e.to_string()),
    }
}

#[async_trait::async_trait]
impl Aliases for FakeAliases {
    fn providers(&self) -> Vec<AliasProvider> {
        self.0
            .providers()
            .iter()
            .map(|p| AliasProvider {
                id: p.id.clone(),
                name: p.name.clone(),
                default_url: p.default_url.clone(),
                capabilities: p.capabilities,
                key_label: p.key_label.clone(),
                key_help: p.key_help.clone(),
                detects: p.detects,
            })
            .collect()
    }

    async fn add_service(
        &self,
        archive: holtmail_core::ArchiveId,
        s: NewService,
    ) -> Result<holtmail_store::AliasService, MailboxError> {
        self.0
            .add_service(archive, &s.provider, &s.name, s.base_url.as_deref(), &s.key)
            .await
            .map_err(manage_error)
    }

    async fn update_service(
        &self,
        id: holtmail_core::AliasServiceId,
        c: ServiceChange,
    ) -> Result<holtmail_store::AliasService, MailboxError> {
        self.0
            .update_service(
                id,
                c.name.as_deref(),
                c.base_url.as_deref(),
                c.key.as_deref(),
            )
            .await
            .map_err(manage_error)
    }

    async fn sync_service(
        &self,
        id: holtmail_core::AliasServiceId,
    ) -> Result<holtmail_store::AliasService, MailboxError> {
        self.0.sync_service(id).await.map_err(manage_error)
    }

    async fn alias_options(
        &self,
        id: holtmail_core::AliasServiceId,
    ) -> Result<(holtmail_alias::Capabilities, holtmail_alias::AliasOptions), MailboxError> {
        self.0.options(id).await.map_err(manage_error)
    }

    async fn create_alias(
        &self,
        id: holtmail_core::AliasServiceId,
        alias: holtmail_alias::NewAlias,
    ) -> Result<holtmail_store::KnownAlias, MailboxError> {
        self.0.create_alias(id, &alias).await.map_err(manage_error)
    }

    async fn update_alias(
        &self,
        archive: holtmail_core::ArchiveId,
        address: &str,
        change: holtmail_alias::AliasChange,
    ) -> Result<holtmail_store::KnownAlias, MailboxError> {
        self.0
            .update_alias(archive, address, &change)
            .await
            .map_err(manage_error)
    }

    async fn delete_alias(
        &self,
        archive: holtmail_core::ArchiveId,
        address: &str,
        at_service: bool,
    ) -> Result<(), MailboxError> {
        self.0
            .delete_alias(archive, address, at_service)
            .await
            .map_err(manage_error)
    }
    fn default_strategy(&self, provider: &str) -> holtmail_alias::ReplyStrategy {
        self.0.default_strategy(provider)
    }

    async fn can_reverse(
        &self,
        archive: holtmail_core::ArchiveId,
        alias: &str,
    ) -> Result<bool, MailboxError> {
        self.0
            .can_reverse(archive, alias)
            .await
            .map_err(manage_error)
    }

    async fn reverse_alias(
        &self,
        archive: holtmail_core::ArchiveId,
        alias: &str,
        contact: &str,
    ) -> Result<String, MailboxError> {
        self.0
            .reverse_alias(archive, alias, contact)
            .await
            .map_err(manage_error)
    }
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
        if selection == DeletionSelection::Query("category:travel".into()) {
            return Err(MailboxError::Conflict(
                "Some messages are still being read. Try again when search has caught up.".into(),
            ));
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

    async fn fresh_insights(&self, message: holtmail_core::MessageId) -> Result<(), MailboxError> {
        self.freshened.lock().unwrap().push(message);
        let Some(fresh) = self.fresh.get() else {
            return Ok(());
        };
        let store = self.store.get().unwrap();
        let view = store.insight_view(message).await.map_err(backend)?;
        if view
            .as_ref()
            .is_none_or(|v| v.insights.fingerprint != fresh.fingerprint)
        {
            fresh
                .ingest
                .refresh_insights(message)
                .await
                .map_err(backend)?;
        }
        let archive = store
            .get_message(message)
            .await
            .map_err(backend)?
            .ok_or_else(|| MailboxError::Invalid("no such message".into()))?
            .archive_id;
        let ctx = fresh
            .deriver
            .context(&**store, archive)
            .await
            .map_err(backend)?;
        let view = store.insight_view(message).await.map_err(backend)?;
        if view
            .and_then(|v| v.categories)
            .is_none_or(|c| c.row.write.derive_fp != ctx.derive_fp())
        {
            fresh
                .ingest
                .derive_categories(message)
                .await
                .map_err(backend)?;
        }
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
    async fn scheduling(&self, _: &holtmail_store::Collection) -> crate::SchedulingProbe {
        self.scheduling
            .lock()
            .unwrap()
            .unwrap_or(crate::SchedulingProbe::NoServer)
    }

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
                "dav_password": dav.dav_password.is_some_and(|p| !p.is_empty()),
            });
        }
        if let Some(minutes) = change.contacts_interval_minutes {
            account.settings["dav"]["interval"] = json!(minutes);
        }
        if let Some(watch) = change.watch {
            account.settings["watch"] =
                json!({ "mode": watch.mode.as_str(), "folders": watch.folders });
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
            sign_in_reason: None,
            contacts_calendars: (account.kind != holtmail_store::AccountKind::Import).then(|| {
                let dav = &account.settings["dav"];
                holtmail_api_contacts(dav)
            }),
            watch: (account.kind != holtmail_store::AccountKind::Import).then(|| WatchSettings {
                mode: account.settings["watch"]["mode"]
                    .as_str()
                    .and_then(WatchMode::parse)
                    .unwrap_or_default(),
                folders: serde_json::from_value(account.settings["watch"]["folders"].clone())
                    .unwrap_or_default(),
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

    async fn receive_push(&self, id: &str, body: &[u8]) -> bool {
        id == "0123abcd" && body == b"sealed"
    }

    fn insights_changed(&self, archive: holtmail_core::ArchiveId) {
        self.changed.lock().unwrap().push(archive);
    }
}

struct Fixture {
    app: Router,
    state: ApiState,
    mailboxes: Arc<FakeMailboxes>,
    fetcher: Arc<FakeFetcher>,
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
    auth: Arc<Auth>,
    cookie: String,
    archive: String,
    folder: String,
    progress: Arc<InsightProgress>,
    submitted: send::Submitted,
    directory: Arc<holtmail_alias::testing::FakeDirectory>,
    _dir: tempfile::TempDir,
}

const CURRENT_FP: &str = "fp-current";

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
    signed_in(empty_fixture().await).await
}

async fn signed_in(mut f: Fixture) -> Fixture {
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
    empty_fixture_with(Some(InsightsConfig::new(CURRENT_FP))).await
}

async fn empty_fixture_with(insights: Option<InsightsConfig>) -> Fixture {
    let progress = insights
        .as_ref()
        .map_or_else(Default::default, |c| c.progress.clone());
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
    let (sending, submitted) = send::fake_sending(store.clone(), blobs.clone(), ingest.clone());
    let directory = Arc::new(holtmail_alias::testing::FakeDirectory::new());
    let state = ApiState {
        blobs: blobs.clone(),
        store: store.clone(),
        index,
        auth: auth.clone(),
        mailboxes: mailboxes.clone(),
        operations: mailboxes.clone(),
        aliases: Arc::new(FakeAliases::new(store.clone(), directory.clone())),
        fetcher: fetcher.clone(),
        secure_cookies: false,
        insights,
        write: WriteSwitches::default(),
        sending: Some(sending),
        devices: Arc::new(holtmail_device_local::LocalDeviceAuth::new(
            store.clone(),
            holtmail_device_local::Options::default(),
        )),
        serve: crate::ServeOptions::default(),
        rule_hooks: Arc::new(crate::NoRuleHooks),
    };
    Fixture {
        app: router(state.clone()),
        state,
        mailboxes,
        fetcher,
        store,
        blobs,
        ingest,
        auth,
        cookie: String::new(),
        archive: archive.to_string(),
        folder: folder.to_string(),
        progress,
        submitted,
        directory,
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
        "/api/devices/{id}",
        "/api/devices/{id}/profile",
        "/api/devices/profile/{token}",
        "/api/push/dav/{id}",
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
        "/api/alias-providers",
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
        format!("/api/archives/{}/insights/status", f.archive),
        format!("/api/archives/{}/categories", f.archive),
        format!("/api/archives/{}/category-overrides", f.archive),
        format!("/api/archives/{}/messages?category=bulk", f.archive),
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
        format!("/api/archives/{}/insights/status", f.archive),
        format!("/api/archives/{}/categories", f.archive),
        format!("/api/archives/{}/categories?include=health", f.archive),
        format!("/api/archives/{}/category-overrides", f.archive),
        format!("/api/archives/{}/messages?category=health", f.archive),
        "/api/insights/data".to_owned(),
    ] {
        let (status, _, _) = f
            .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    let accounts = f
        .send_as(
            &member,
            Request::get(format!("/api/archives/{}/accounts", f.archive))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let accounts: Value = serde_json::from_slice(&accounts.2).unwrap();
    assert!(accounts[0]["trusted_authserv_ids"].is_null());
    assert!(accounts[0]["suggested_authserv_id"].is_null());
    let account = accounts[0]["id"].as_str().unwrap().to_owned();
    let (_, _, body) = f
        .send(request(
            "PUT",
            &format!("/api/archives/{}/category-overrides", f.archive),
            json!({ "scope": "domain", "key": "example.com", "kind": "bulk" }),
        ))
        .await;
    let made: Value = serde_json::from_slice(&body).unwrap();
    let made = made["id"].as_str().unwrap().to_owned();
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
        (
            "PUT",
            format!("/api/archives/{}/category-overrides", f.archive),
            json!({ "scope": "domain", "key": "example.org", "kind": "bulk" }),
        ),
        (
            "DELETE",
            format!("/api/archives/{}/category-overrides/{made}", f.archive),
            json!({}),
        ),
        (
            "PATCH",
            format!("/api/accounts/{account}"),
            json!({ "trusted_authserv_ids": ["mx.example.org"] }),
        ),
        (
            "POST",
            format!("/api/messages/{message}/unsubscribe"),
            json!({ "confirm": true }),
        ),
    ] {
        let (status, _, _) = f.send_as(&member, request(method, &uri, body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    let overrides = f
        .json(&format!("/api/archives/{}/category-overrides", f.archive))
        .await;
    assert_eq!(overrides.as_array().unwrap().len(), 1);
    assert_eq!(overrides[0]["key"], "example.com");

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
    assert_eq!(account["status"]["watch"]["state"], "off");
    assert_eq!(account["status"]["contacts_watch"]["state"], "off");
    assert_eq!(account["watch"]["mode"], "inbox", "watched by default");

    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "watch": { "mode": "folders", "folders": [" INBOX ", "Work", ""] }, "contacts_interval_minutes": 5 }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["watch"]["mode"], "folders");
    assert_eq!(account["watch"]["folders"], json!(["INBOX", "Work"]));
    assert_eq!(account["contacts_calendars"]["interval_minutes"], 5);
    for bad in [
        json!({ "watch": { "mode": "sometimes" } }),
        json!({ "watch": { "mode": "folders", "folders": [] } }),
    ] {
        let (status, _, _) = f.send(request("PATCH", &uri, bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "watch": { "mode": "off" }, "contacts_interval_minutes": null }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let account: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(account["watch"]["mode"], "off");
    assert_eq!(
        account["contacts_calendars"]["interval_minutes"],
        Value::Null
    );

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
        json!([{ "email": "ada@example.com", "person_id": person, "has_card": true }])
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
        .send(post_json(&url, json!({ "query": "category:travel" })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"],
        "Some messages are still being read. Try again when search has caught up."
    );
    assert!(f.mailboxes.deletions.lock().unwrap().is_empty());
    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    assert!(spec["paths"]["/api/archives/{id}/deletions"]["post"]["responses"]["409"].is_object());
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

#[tokio::test(flavor = "multi_thread")]
async fn receives_push_messages_without_a_session() {
    let f = fixture().await;
    let push = |uri: &str, body: &'static [u8], content_type: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::COOKIE, "holtmail_session=none")
            .header(header::ORIGIN, "https://dav.example.org")
            .body(Body::from(body))
            .unwrap()
    };
    let (status, _, _) = f
        .send(push(
            "/api/push/dav/0123abcd",
            b"sealed",
            "application/octet-stream",
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "any origin and content type");
    let (status, _, _) = f
        .send(push("/api/push/dav/0123abcd", b"other", "application/xml"))
        .await;
    assert_eq!(status, StatusCode::GONE, "a message it cannot read");
    let (status, _, _) = f
        .send(push(
            "/api/push/dav/ffff",
            b"sealed",
            "application/octet-stream",
        ))
        .await;
    assert_eq!(status, StatusCode::GONE, "an address it does not know");
    let (status, _, _) = f
        .send(push(
            "/api/push/dav/..%2Fx",
            b"sealed",
            "application/octet-stream",
        ))
        .await;
    assert_eq!(status, StatusCode::GONE);
    let large: &'static [u8] = Box::leak(vec![0u8; 20 * 1024].into_boxed_slice());
    let (status, _, _) = f
        .send(push(
            "/api/push/dav/0123abcd",
            large,
            "application/octet-stream",
        ))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

const FORWARDED: &[u8] = b"X-SimpleLogin-Type: Forward\r\nX-SimpleLogin-Envelope-To: shop@alias.example.org\r\nFrom: \"Grace Hopper - grace at example.com\" <ra_q1@sl.example.org>\r\nTo: shop@alias.example.org\r\nSubject: Parcel\r\nMessage-ID: <p1@example.com>\r\nDate: Fri, 14 Mar 2025 11:00:00 +0000\r\n\r\nOn its way.\r\n";

fn send_json(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn aliases_from_mail_services_and_by_hand() {
    let f = fixture().await;
    let ingest = f.ingest.clone().with_alias_detectors(vec![Arc::new(
        holtmail_alias_simplelogin::SimpleLoginDetector::default(),
    )]);
    let folder: holtmail_core::FolderId = f.folder.parse().unwrap();
    let forwarded = ingest
        .ingest(
            f.archive.parse().unwrap(),
            Bytes::from_static(FORWARDED),
            &Placement {
                folder: Some(folder),
                uid: Some(9),
                ..Placement::default()
            },
        )
        .await
        .unwrap();
    ingest.commit().await.unwrap();

    let message = f
        .json(&format!("/api/messages/{}", forwarded.message_id))
        .await;
    assert_eq!(message["alias"]["provider"], "simplelogin");
    assert_eq!(message["alias"]["alias"], "shop@alias.example.org");
    assert_eq!(message["alias"]["sender"]["email"], "grace@example.com");
    assert_eq!(message["alias"]["sender"]["name"], "Grace Hopper");
    assert_eq!(message["alias"]["signed"], false);
    assert_eq!(message["alias"]["known"], false);

    let providers = f.json("/api/alias-providers").await;
    assert_eq!(providers[0]["id"], "fake");
    assert_eq!(providers[0]["capabilities"]["switch"], true);

    let list = format!("/api/archives/{}/aliases", f.archive);
    let listed = f.json(&list).await;
    assert_eq!(listed["aliases"][0]["address"], "shop@alias.example.org");
    assert_eq!(listed["aliases"][0]["source"], "mail");
    assert_eq!(listed["aliases"][0]["messages"], 1);
    assert_eq!(listed["aliases"][0]["accounts"][0]["messages"], 1);

    let services = format!("/api/archives/{}/alias-services", f.archive);
    let (status, _, body) = f
        .send(post_json(
            &services,
            json!({"provider": "fake", "key": "wrong"}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("refused the key"));
    let (status, _, body) = f
        .send(post_json(
            &services,
            json!({"provider": "fake", "name": "Main", "key": "good-key"}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let service: Value = serde_json::from_slice(&body).unwrap();
    let service_id = service["id"].as_str().unwrap().to_owned();
    assert_eq!(service["name"], "Main");
    assert!(service["synced_at"].is_i64());

    let options = f
        .json(&format!("/api/alias-services/{service_id}/options"))
        .await;
    assert_eq!(options["domains"][0]["label"], "@alias.example");
    assert_eq!(options["capabilities"]["create_custom"], true);

    let (status, _, body) = f
        .send(post_json(
            &format!("/api/alias-services/{service_id}/aliases"),
            json!({"prefix": "news", "note": "letters", "mailbox_ids": ["2"]}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let made: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(made["address"], "news@alias.example");
    assert_eq!(made["source"], "service");
    assert_eq!(made["mailboxes"], json!(["other@example.org"]));

    let alias_uri = format!("/api/archives/{}/aliases/news@alias.example", f.archive);
    let (status, _, body) = f
        .send(send_json(
            "PATCH",
            &alias_uri,
            json!({"enabled": false, "note": ""}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let changed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(changed["enabled"], false);
    assert_eq!(changed["note"], Value::Null);

    let (status, _, body) = f
        .send(post_json(
            &list,
            json!({"address": "Quiet.River@icloud.example.org", "provider": "apple", "note": "shop"}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let (status, _, _) = f
        .send(post_json(
            &list,
            json!({"address": "quiet.river@icloud.example.org", "provider": "apple"}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = f
        .send(post_json(
            &list,
            json!({"address": "not an address", "provider": "apple"}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = f
        .send(
            Request::delete(format!("{alias_uri}?at_service=true"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let listed = f.json(&list).await;
    let addresses: Vec<&str> = listed["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["address"].as_str().unwrap())
        .collect();
    assert_eq!(
        addresses,
        ["quiet.river@icloud.example.org", "shop@alias.example.org"]
    );
    assert_eq!(listed["services"][0]["id"], service_id.as_str());

    let (status, _, _) = f
        .send(
            Request::delete(format!("/api/alias-services/{service_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(f.json(&list).await["services"], json!([]));
}

fn insight(
    kind: holtmail_insight::InsightKind,
    confidence: holtmail_insight::Confidence,
    score: u16,
) -> holtmail_insight::Insight {
    holtmail_insight::Insight {
        kind,
        provenance: holtmail_insight::Provenance::Heuristic,
        confidence,
        score,
        origin: holtmail_insight::PartRef::Text,
        forwarded: false,
    }
}

fn code(value: &str, confidence: holtmail_insight::Confidence) -> holtmail_insight::Insight {
    insight(
        holtmail_insight::InsightKind::Code {
            value: value.into(),
            origin_bound: None,
        },
        confidence,
        if confidence == holtmail_insight::Confidence::Possible {
            300
        } else {
            800
        },
    )
}

fn unsubscribe_item() -> holtmail_insight::Insight {
    insight(
        holtmail_insight::InsightKind::Unsubscribe {
            https: Some(holtmail_insight::Url::parse("https://lists.example.com/u/42").unwrap()),
            mailto: Some("mailto:leave@lists.example.com".into()),
            http: None,
            one_click: true,
            list_id: Some("weekly.lists.example.com".into()),
        },
        holtmail_insight::Confidence::Certain,
        1000,
    )
}

impl Fixture {
    async fn message_id(&self, subject: &str) -> holtmail_core::MessageId {
        let page = self
            .json(&format!("/api/archives/{}/messages", self.archive))
            .await;
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["subject"] == subject)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    async fn store_insights(
        &self,
        message: holtmail_core::MessageId,
        fingerprint: &str,
        items: &str,
        kinds: &[holtmail_insight::KindTag],
    ) {
        let row = holtmail_store::InsightRow {
            message,
            archive: self.archive.parse().unwrap(),
            fingerprint: fingerprint.into(),
            outcome: holtmail_store::InsightOutcome::Ok,
            truncated: false,
            from_address: None,
            signals: 0,
            kinds: kinds
                .iter()
                .copied()
                .collect::<holtmail_insight::KindTags>()
                .bits(),
            auth: "{}".into(),
            items: items.into(),
            computed_at: holtmail_core::Timestamp::now(),
        };
        self.store
            .set_message_insights(message, &row)
            .await
            .unwrap();
    }

    async fn store_newsletter_insights(&self, fingerprint: &str) -> holtmail_core::MessageId {
        let id = self.message_id("Weekly news").await;
        let items = holtmail_insight::Items {
            items: vec![
                code("482913", holtmail_insight::Confidence::Likely),
                code("7731", holtmail_insight::Confidence::Possible),
                unsubscribe_item(),
            ],
        };
        self.store_insights(
            id,
            fingerprint,
            &items.to_json(),
            &[
                holtmail_insight::KindTag::Code,
                holtmail_insight::KindTag::Unsubscribe,
            ],
        )
        .await;
        id
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_insights_in_messages_threads_and_lists() {
    let f = fixture().await;
    let news = f.store_newsletter_insights(CURRENT_FP).await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["status"], "ready");
    assert_eq!(insights["trust"]["state"], "unknown");
    assert_eq!(insights["own"], false);
    let items = insights["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["type"], "code");
    assert_eq!(items[0]["value"], "482913");
    assert_eq!(items[0]["confidence"], "likely");
    assert_eq!(items[0]["score"], 800);
    assert_eq!(items[1]["type"], "unsubscribe");
    assert_eq!(items[1]["list_id"], "weekly.lists.example.com");
    assert_eq!(
        insights["unsubscribe"],
        json!({
            "https": "https://lists.example.com/u/42",
            "mailto": "mailto:leave@lists.example.com",
            "http": null,
            "one_click": { "available": false, "reason": "no_post_header" },
        })
    );
    assert!(insights["kind"].is_null());
    assert_eq!(insights["topics"], json!([]));

    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let kinds: Vec<&Value> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["insight_kinds"])
        .collect();
    assert!(
        kinds.contains(&&json!(["code", "unsubscribe"])),
        "{kinds:?}"
    );
    assert!(kinds.contains(&&json!([])), "{kinds:?}");

    let plain = f.message_id("Re: Weekly news").await;
    let detail = f.json(&format!("/api/messages/{plain}")).await;
    assert_eq!(detail["insights"]["status"], "pending");
    assert_eq!(detail["insights"]["items"], json!([]));
    assert!(detail["insights"]["unsubscribe"].is_null());

    let thread = f
        .json(&format!(
            "/api/threads/{}",
            detail["thread_id"].as_str().unwrap()
        ))
        .await;
    let statuses: Vec<&Value> = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["insights"]["status"])
        .collect();
    assert_eq!(statuses, [&json!("ready"), &json!("pending")]);

    f.store_newsletter_insights("fp-old").await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(detail["insights"]["status"], "outdated");
    assert_eq!(detail["insights"]["items"][0]["value"], "482913");

    f.store_insights(
        news,
        CURRENT_FP,
        r#"{"v":1,"items":[{"type":"hologram","p":"header","c":"certain","score":1000,"origin":"text","forwarded":false}]}"#,
        &[],
    )
    .await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(detail["insights"]["status"], "outdated");
    assert_eq!(detail["insights"]["items"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_possible_items_above_the_configured_score() {
    let mut config = InsightsConfig::new(CURRENT_FP);
    config
        .show_possible
        .insert(holtmail_insight::KindTag::Code, 250);
    let f = signed_in(empty_fixture_with(Some(config)).await).await;
    let news = f.store_newsletter_insights(CURRENT_FP).await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    let values: Vec<&Value> = detail["insights"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "code")
        .map(|i| &i["value"])
        .collect();
    assert_eq!(values, [&json!("482913"), &json!("7731")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_insights_out_without_a_runner() {
    let f = signed_in(empty_fixture_with(None).await).await;
    let news = f.store_newsletter_insights(CURRENT_FP).await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert!(detail["insights"].is_null());
    let (status, _, _) = f
        .get(&format!("/api/archives/{}/insights/status", f.archive))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn hides_codes_on_own_mail() {
    let f = fixture().await;
    let news = f.store_newsletter_insights(CURRENT_FP).await;
    f.store
        .create_account(NewAccount {
            archive_id: f.archive.parse().unwrap(),
            kind: AccountKind::Import,
            name: "News desk".into(),
            address: Some("News@Example.com".into()),
            settings: Default::default(),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    let detail = f.json(&format!("/api/messages/{news}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["own"], true);
    let types: Vec<&Value> = insights["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["type"])
        .collect();
    assert_eq!(types, [&json!("unsubscribe")]);
    assert_eq!(
        insights["unsubscribe"]["https"],
        "https://lists.example.com/u/42"
    );
    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == news.to_string())
        .unwrap();
    assert_eq!(row["insight_kinds"], json!(["unsubscribe"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_insights_for_members() {
    let f = fixture().await;
    let news = f.store_newsletter_insights(CURRENT_FP).await;
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let grace: Value = serde_json::from_slice(&body).unwrap();
    let grace_id = grace["user"]["id"].as_str().unwrap().to_owned();
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/users/{grace_id}"),
            json!({ "archives": [f.archive] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let read = |uri: String| {
        let member = member.clone();
        let f = &f;
        async move {
            let (status, _, body) = f
                .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
                .await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            serde_json::from_slice::<Value>(&body).unwrap()
        }
    };

    let detail = read(format!("/api/messages/{news}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["status"], "ready");
    assert_eq!(insights["items"], json!([]));
    assert_eq!(
        insights["unsubscribe"],
        json!({
            "https": null,
            "mailto": null,
            "http": null,
            "one_click": { "available": false, "reason": "members" },
        })
    );
    let body = detail.to_string();
    assert!(!body.contains("482913"));
    assert!(!body.contains("lists.example.com/u/42"));

    let thread = read(format!(
        "/api/threads/{}",
        detail["thread_id"].as_str().unwrap()
    ))
    .await;
    assert!(!thread.to_string().contains("482913"));

    let page = read(format!("/api/archives/{}/messages", f.archive)).await;
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == news.to_string())
        .unwrap();
    assert_eq!(row["insight_kinds"], json!(["unsubscribe"]));

    let status = read(format!("/api/archives/{}/insights/status", f.archive)).await;
    assert_eq!(
        status,
        json!({ "pending": 1, "reindex": 0, "categorize": 0, "running": false })
    );

    let other = f.store.create_archive("Work").await.unwrap().id;
    for uri in [
        format!("/api/archives/{other}/insights/status"),
        format!(
            "/api/archives/{}/insights/status",
            holtmail_core::ArchiveId::new()
        ),
    ] {
        let (status, _, _) = f
            .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_categories_derived_under_older_settings() {
    let mut config = InsightsConfig::new(CURRENT_FP);
    config.deriver = Some(Arc::new(holtmail_insights::Deriver::new(
        Vec::new(),
        holtmail_insights::DeriveConfig::default(),
    )));
    let f = signed_in(empty_fixture_with(Some(config)).await).await;
    let uri = format!("/api/archives/{}/insights/status", f.archive);
    f.store_newsletter_insights(CURRENT_FP).await;
    assert_eq!(
        f.json(&uri).await,
        json!({ "pending": 1, "reindex": 0, "categorize": 1, "running": false })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_insights_progress() {
    let f = fixture().await;
    let uri = format!("/api/archives/{}/insights/status", f.archive);
    assert_eq!(
        f.json(&uri).await,
        json!({ "pending": 2, "reindex": 0, "categorize": 0, "running": false })
    );
    f.store_newsletter_insights(CURRENT_FP).await;
    f.progress.start();
    assert_eq!(
        f.json(&uri).await,
        json!({ "pending": 1, "reindex": 0, "categorize": 0, "running": true })
    );
    f.progress.finish();
    f.store_newsletter_insights("fp-old").await;
    assert_eq!(
        f.json(&uri).await,
        json!({ "pending": 2, "reindex": 0, "categorize": 0, "running": false })
    );
    for uri in [
        format!(
            "/api/archives/{}/insights/status",
            holtmail_core::ArchiveId::new()
        ),
        "/api/archives/not-an-id/insights/status".to_owned(),
    ] {
        let (status, _, _) = f.get(&uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    let responses = &spec["paths"]["/api/archives/{id}/insights/status"]["get"]["responses"];
    for code in ["200", "401", "403", "404", "500"] {
        assert!(responses[code].is_object(), "{code}");
    }
    for schema in ["MessageInsightsDto", "InsightDto", "InsightsStatusDto"] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "{schema}"
        );
    }
}

#[test]
fn gives_every_operation_its_own_id() {
    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    let mut seen = std::collections::HashSet::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            let id = operation["operationId"].as_str().unwrap();
            assert!(seen.insert(id.to_owned()), "{method} {path}: {id}");
        }
    }
}

fn action(
    kind: holtmail_insight::LinkAction,
    url: &str,
    destination: &str,
    via: Option<&str>,
    text_host: Option<&str>,
    confidence: holtmail_insight::Confidence,
    score: u16,
) -> holtmail_insight::Insight {
    insight(
        holtmail_insight::InsightKind::Action {
            action: kind,
            url: holtmail_insight::Url::parse(url).unwrap(),
            destination: holtmail_insight::Url::parse(destination).unwrap(),
            via: via.map(Into::into),
            tracked: destination.contains("/t/"),
            text_host: text_host.map(Into::into),
            label: Some(format!("{kind:?}")),
        },
        confidence,
        score,
    )
}

impl Fixture {
    async fn store_link_insights(&self) -> holtmail_core::MessageId {
        use holtmail_insight::{Confidence, LinkAction};
        let id = self.message_id("Weekly news").await;
        let mut parcel = insight(
            holtmail_insight::InsightKind::Parcel {
                carrier: holtmail_insight::CarrierId::new("ups").unwrap(),
                number: "1Z999AA10123456784".into(),
                check_digit: true,
                track_url: Some(
                    holtmail_insight::Url::parse(
                        "https://track.example.net/ups/1Z999AA10123456784",
                    )
                    .unwrap(),
                ),
            },
            Confidence::Likely,
            900,
        );
        parcel.forwarded = true;
        let items = holtmail_insight::Items {
            items: vec![
                code("482913", Confidence::Likely),
                action(
                    LinkAction::ViewOrder,
                    "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fshop.example.com%2Forders%2F7",
                    "https://shop.example.com/orders/7",
                    Some("outlook.com"),
                    Some("Shop.Example.com."),
                    Confidence::Likely,
                    800,
                ),
                action(
                    LinkAction::SignIn,
                    "https://xn--80ak6aa92e.com/login",
                    "https://xn--80ak6aa92e.com/login",
                    None,
                    Some("example.com"),
                    Confidence::Likely,
                    700,
                ),
                action(
                    LinkAction::Track,
                    "https://xn--bcher-kva.example.org/t/1",
                    "https://xn--bcher-kva.example.org/t/1",
                    None,
                    None,
                    Confidence::Likely,
                    650,
                ),
                action(
                    LinkAction::View,
                    "https://news.example.com/web",
                    "https://news.example.com/web",
                    None,
                    Some("archive.example.com"),
                    Confidence::Possible,
                    400,
                ),
                parcel,
                unsubscribe_item(),
            ],
        };
        self.store_insights(
            id,
            CURRENT_FP,
            &items.to_json(),
            &[
                holtmail_insight::KindTag::Code,
                holtmail_insight::KindTag::Link,
                holtmail_insight::KindTag::Parcel,
                holtmail_insight::KindTag::Unsubscribe,
            ],
        )
        .await;
        id
    }
}

fn of_type<'a>(insights: &'a Value, kind: &str) -> Vec<&'a Value> {
    insights["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == kind)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_action_links_and_parcels_with_read_time_flags() {
    let f = fixture().await;
    let news = f.store_link_insights().await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["status"], "ready");
    assert_eq!(insights["trust"]["state"], "unknown");
    let actions = of_type(insights, "action");
    assert_eq!(actions.len(), 3, "{actions:?}");
    assert_eq!(
        actions[0],
        &json!({
            "type": "action",
            "action": "view_order",
            "url": "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fshop.example.com%2Forders%2F7",
            "destination": "https://shop.example.com/orders/7",
            "display_host": "shop.example.com",
            "via": "outlook.com",
            "tracked": false,
            "text_domain_mismatch": false,
            "destination_differs_from_sender": false,
            "confusable": false,
            "label": "ViewOrder",
            "confidence": "likely",
            "score": 800,
            "forwarded": false,
        })
    );
    assert_eq!(actions[1]["action"], "sign_in");
    assert_eq!(actions[1]["display_host"], "xn--80ak6aa92e.com");
    assert_eq!(actions[1]["confusable"], true);
    assert_eq!(actions[1]["text_domain_mismatch"], true);
    assert_eq!(actions[1]["destination_differs_from_sender"], true);
    assert!(actions[1]["via"].is_null());
    assert_eq!(actions[2]["action"], "track");
    assert_eq!(actions[2]["display_host"], "bücher.example.org");
    assert_eq!(actions[2]["confusable"], false);
    assert_eq!(actions[2]["tracked"], true);
    assert_eq!(actions[2]["text_domain_mismatch"], false);
    assert_eq!(actions[2]["destination_differs_from_sender"], true);
    assert_eq!(
        of_type(insights, "parcel"),
        [&json!({
            "type": "parcel",
            "carrier": "ups",
            "number": "1Z999AA10123456784",
            "check_digit": true,
            "track_url": "https://track.example.net/ups/1Z999AA10123456784",
            "track_host": "track.example.net",
            "confidence": "likely",
            "score": 900,
            "forwarded": true,
        })]
    );

    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == news.to_string())
        .unwrap();
    assert_eq!(
        row["insight_kinds"],
        json!(["code", "link", "unsubscribe", "parcel"])
    );

    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    let action_values = &spec["components"]["schemas"]["LinkActionDto"]["enum"];
    assert_eq!(
        action_values.as_array().unwrap().len(),
        11,
        "{action_values}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_destinations_without_a_registrable_domain() {
    use holtmail_insight::{Confidence, LinkAction};
    let f = fixture().await;
    let id = f.message_id("Weekly news").await;
    let items = holtmail_insight::Items {
        items: vec![
            action(
                LinkAction::ResetPassword,
                "http://192.0.2.1/reset",
                "http://192.0.2.1/reset",
                None,
                Some("accounts.example.com"),
                Confidence::Likely,
                800,
            ),
            action(
                LinkAction::SignIn,
                "https://github.io/login",
                "https://github.io/login",
                None,
                None,
                Confidence::Likely,
                700,
            ),
        ],
    };
    f.store_insights(
        id,
        CURRENT_FP,
        &items.to_json(),
        &[holtmail_insight::KindTag::Link],
    )
    .await;
    let detail = f.json(&format!("/api/messages/{id}")).await;
    let actions = of_type(&detail["insights"], "action");
    assert_eq!(actions.len(), 2, "{actions:?}");
    for a in &actions {
        assert_eq!(a["destination_differs_from_sender"], true, "{a}");
    }
    assert_eq!(actions[0]["text_domain_mismatch"], true);
    assert_eq!(actions[1]["text_domain_mismatch"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_possible_actions_only_when_configured() {
    let mut config = InsightsConfig::new(CURRENT_FP);
    config
        .show_possible
        .insert(holtmail_insight::KindTag::Link, 300);
    let f = signed_in(empty_fixture_with(Some(config)).await).await;
    let news = f.store_link_insights().await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    let actions = of_type(&detail["insights"], "action");
    assert_eq!(actions.len(), 4, "{actions:?}");
    let view = actions.iter().find(|a| a["action"] == "view").unwrap();
    assert_eq!(view["confidence"], "possible");
    assert_eq!(view["text_domain_mismatch"], false);

    let mut config = InsightsConfig::new(CURRENT_FP);
    config
        .show_possible
        .insert(holtmail_insight::KindTag::Link, 500);
    let f = signed_in(empty_fixture_with(Some(config)).await).await;
    let news = f.store_link_insights().await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(of_type(&detail["insights"], "action").len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn hides_actions_on_own_mail() {
    let f = fixture().await;
    let news = f.store_link_insights().await;
    f.store
        .create_account(NewAccount {
            archive_id: f.archive.parse().unwrap(),
            kind: AccountKind::Import,
            name: "News desk".into(),
            address: Some("news@example.com".into()),
            settings: Default::default(),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    let detail = f.json(&format!("/api/messages/{news}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["own"], true);
    let types: Vec<&Value> = insights["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["type"])
        .collect();
    assert_eq!(types, [&json!("parcel"), &json!("unsubscribe")]);
    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == news.to_string())
        .unwrap();
    assert_eq!(row["insight_kinds"], json!(["unsubscribe", "parcel"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn members_lose_sign_in_actions() {
    let f = fixture().await;
    let news = f.store_link_insights().await;
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let grace: Value = serde_json::from_slice(&body).unwrap();
    let grace_id = grace["user"]["id"].as_str().unwrap().to_owned();
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/users/{grace_id}"),
            json!({ "archives": [f.archive] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let read = |uri: String| {
        let member = member.clone();
        let f = &f;
        async move {
            let (status, _, body) = f
                .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
                .await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            serde_json::from_slice::<Value>(&body).unwrap()
        }
    };

    let detail = read(format!("/api/messages/{news}")).await;
    let thread = read(format!(
        "/api/threads/{}",
        detail["thread_id"].as_str().unwrap()
    ))
    .await;
    let in_thread = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == news.to_string())
        .unwrap();
    for insights in [&detail["insights"], &in_thread["insights"]] {
        let types: Vec<(&Value, &Value)> = insights["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (&i["type"], &i["action"]))
            .collect();
        assert_eq!(
            types,
            [
                (&json!("action"), &json!("view_order")),
                (&json!("action"), &json!("track")),
                (&json!("parcel"), &Value::Null),
            ]
        );
        assert!(!insights.to_string().contains("xn--80ak6aa92e"));
    }

    let page = read(format!("/api/archives/{}/messages", f.archive)).await;
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == news.to_string())
        .unwrap();
    assert_eq!(
        row["insight_kinds"],
        json!(["link", "unsubscribe", "parcel"])
    );
}

fn certain(kind: holtmail_insight::InsightKind) -> holtmail_insight::Insight {
    let mut item = insight(kind, holtmail_insight::Confidence::Certain, 1000);
    item.provenance = holtmail_insight::Provenance::JsonLd;
    item.origin = holtmail_insight::PartRef::Html;
    item
}

fn wall(text: &str) -> chrono::NaiveDateTime {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").unwrap()
}

fn zoned(text: &str, zone: Option<&str>) -> holtmail_insight::LocalOrZoned {
    holtmail_insight::LocalOrZoned::Zoned(
        chrono::DateTime::parse_from_rfc3339(text).unwrap(),
        zone.map(Into::into),
    )
}

fn money(minor: i64, code: &str, exponent: u8) -> holtmail_insight::Money {
    holtmail_insight::Money {
        minor,
        currency: holtmail_insight::Currency::new(code, exponent).unwrap(),
    }
}

fn day(text: &str) -> holtmail_insight::Date {
    holtmail_insight::Date(chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

fn insight_url(url: &str) -> Option<holtmail_insight::Url> {
    Some(holtmail_insight::Url::parse(url).unwrap())
}

fn order(number: &str, total: holtmail_insight::Money) -> holtmail_insight::Insight {
    certain(holtmail_insight::InsightKind::Order {
        number: Some(number.into()),
        merchant: Some("Example Shop".into()),
        total: Some(total),
        status: Some("Shipped".into()),
        url: None,
    })
}

impl Fixture {
    async fn store_structured_insights(&self) -> holtmail_core::MessageId {
        use holtmail_insight::{
            BoardingLeg, BoardingPass, FlightReservation, InsightKind, LocalOrZoned,
            LodgingReservation, PassDate, PassStyle, Reservation,
        };
        let id = self.message_id("Weekly news").await;
        let items = holtmail_insight::Items {
            items: vec![
                code("482913", holtmail_insight::Confidence::Likely),
                certain(InsightKind::Reservation(Reservation::Flight(
                    FlightReservation {
                        reservation_number: Some("XK7Q2P".into()),
                        carrier: Some("EX".into()),
                        flight_number: Some("123".into()),
                        departure_airport: "AMS".into(),
                        arrival_airport: "JFK".into(),
                        departure: zoned("2026-12-01T10:30:00+01:00", Some("Europe/Amsterdam")),
                        arrival: Some(zoned("2026-12-01T12:45:00-05:00", None)),
                        passengers: vec!["Ada Example".into(), "Bob Example".into()],
                        seats: vec!["12A".into(), "12B".into()],
                        url: insight_url("https://xn--80ak6aa92e.com/trips/XK7Q2P"),
                    },
                ))),
                certain(InsightKind::Reservation(Reservation::Flight(
                    FlightReservation {
                        reservation_number: None,
                        carrier: None,
                        flight_number: Some("456".into()),
                        departure_airport: "JFK".into(),
                        arrival_airport: "AMS".into(),
                        departure: LocalOrZoned::Local(wall("2026-12-08T18:00:00")),
                        arrival: None,
                        passengers: Vec::new(),
                        seats: Vec::new(),
                        url: insight_url("mailto:trips@example.com"),
                    },
                ))),
                certain(InsightKind::Reservation(Reservation::Lodging(
                    LodgingReservation {
                        reservation_number: "H-55190".into(),
                        name: "Hotel Example".into(),
                        address: Some("1 Example Street".into()),
                        checkin: LocalOrZoned::Local(wall("2026-12-01T15:00:00")),
                        checkout: Some(LocalOrZoned::Local(wall("2026-12-08T11:00:00"))),
                        guests: vec!["Ada Example".into()],
                        url: insight_url("https://xn--bcher-kva.example.org/stay/55190"),
                    },
                ))),
                order("JP-1", money(1200, "JPY", 0)),
                order("US-1", money(123450, "USD", 2)),
                order("KW-1", money(-3000, "KWD", 3)),
                order("MAX-1", money(i64::MAX, "USD", 2)),
                certain(InsightKind::Invoice {
                    number: Some("INV-2026-11".into()),
                    payee: Some("Example Utilities".into()),
                    total: Some(money(5, "USD", 2)),
                    due: Some(day("2026-12-15")),
                    url: insight_url("https://billing.example.com/inv/11"),
                }),
                {
                    let mut pass = certain(InsightKind::Pass {
                        style: PassStyle::BoardingPass,
                        organization: "Example Air".into(),
                        description: "Boarding pass".into(),
                        relevant: Some(zoned("2026-12-01T09:45:00+01:00", None)),
                        boarding: Some(BoardingPass {
                            pnr: Some("XK7Q2P".into()),
                            passenger: Some("Ada Example".into()),
                            legs: vec![
                                BoardingLeg {
                                    from: "AMS".into(),
                                    to: "JFK".into(),
                                    carrier: Some("EX".into()),
                                    flight: Some("123".into()),
                                    date: Some(PassDate::Full(day("2026-12-01"))),
                                    seat: Some("12A".into()),
                                },
                                BoardingLeg {
                                    from: "JFK".into(),
                                    to: "AMS".into(),
                                    carrier: None,
                                    flight: None,
                                    date: Some(PassDate::MonthDay { month: 12, day: 8 }),
                                    seat: None,
                                },
                            ],
                        }),
                        attachment: 2,
                        signature_checked: false,
                    });
                    pass.provenance = holtmail_insight::Provenance::PkPass;
                    pass.origin = holtmail_insight::PartRef::Attachment(2);
                    pass
                },
            ],
        };
        self.store_insights(
            id,
            CURRENT_FP,
            &items.to_json(),
            &[
                holtmail_insight::KindTag::Code,
                holtmail_insight::KindTag::Reservation,
                holtmail_insight::KindTag::Order,
                holtmail_insight::KindTag::Invoice,
                holtmail_insight::KindTag::Pass,
            ],
        )
        .await;
        id
    }

    async fn member_session(&self) -> String {
        let (status, _, body) = self
            .send(post_json(
                "/api/users",
                json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let grace: Value = serde_json::from_slice(&body).unwrap();
        let grace_id = grace["user"]["id"].as_str().unwrap().to_owned();
        let (status, _, _) = self
            .send(request(
                "PATCH",
                &format!("/api/users/{grace_id}"),
                json!({ "archives": [self.archive] }),
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        self.sign_in("grace@example.org", "compiler 1952").await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_reservations_orders_invoices_and_passes() {
    let f = fixture().await;
    let id = f.store_structured_insights().await;
    let detail = f.json(&format!("/api/messages/{id}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["status"], "ready");

    let reservations = of_type(insights, "reservation");
    assert_eq!(reservations.len(), 3, "{reservations:?}");
    assert_eq!(
        reservations[0],
        &json!({
            "type": "reservation",
            "reservation": {
                "kind": "flight",
                "reservation_number": "XK7Q2P",
                "carrier": "EX",
                "flight_number": "123",
                "departure_airport": "AMS",
                "arrival_airport": "JFK",
                "departure": {
                    "local": "2026-12-01T10:30:00",
                    "offset": "+01:00",
                    "zone": "Europe/Amsterdam",
                },
                "arrival": { "local": "2026-12-01T12:45:00", "offset": "-05:00", "zone": null },
                "passengers": ["Ada Example", "Bob Example"],
                "seats": ["12A", "12B"],
                "url": "https://xn--80ak6aa92e.com/trips/XK7Q2P",
                "url_host": "xn--80ak6aa92e.com",
            },
            "confidence": "certain",
            "score": 1000,
            "forwarded": false,
        })
    );
    let local = &reservations[1]["reservation"];
    assert_eq!(
        local["departure"],
        json!({ "local": "2026-12-08T18:00:00", "offset": null, "zone": null })
    );
    assert!(local["arrival"].is_null());
    assert!(local["url"].is_null());
    assert!(local["url_host"].is_null());
    assert_eq!(
        reservations[2]["reservation"],
        json!({
            "kind": "lodging",
            "reservation_number": "H-55190",
            "name": "Hotel Example",
            "address": "1 Example Street",
            "checkin": { "local": "2026-12-01T15:00:00", "offset": null, "zone": null },
            "checkout": { "local": "2026-12-08T11:00:00", "offset": null, "zone": null },
            "guests": ["Ada Example"],
            "url": "https://xn--bcher-kva.example.org/stay/55190",
            "url_host": "bücher.example.org",
        })
    );

    let totals: Vec<&Value> = of_type(insights, "order")
        .into_iter()
        .map(|o| &o["total"])
        .collect();
    assert_eq!(
        totals,
        [
            &json!({ "amount": "1200", "currency": "JPY", "exponent": 0 }),
            &json!({ "amount": "1234.50", "currency": "USD", "exponent": 2 }),
            &json!({ "amount": "-3.000", "currency": "KWD", "exponent": 3 }),
            &json!({ "amount": "92233720368547758.07", "currency": "USD", "exponent": 2 }),
        ]
    );
    assert_eq!(
        of_type(insights, "order")[0],
        &json!({
            "type": "order",
            "number": "JP-1",
            "merchant": "Example Shop",
            "total": { "amount": "1200", "currency": "JPY", "exponent": 0 },
            "status": "Shipped",
            "url": null,
            "url_host": null,
            "confidence": "certain",
            "score": 1000,
            "forwarded": false,
        })
    );
    assert_eq!(
        of_type(insights, "invoice"),
        [&json!({
            "type": "invoice",
            "number": "INV-2026-11",
            "payee": "Example Utilities",
            "total": { "amount": "0.05", "currency": "USD", "exponent": 2 },
            "due": "2026-12-15",
            "url": "https://billing.example.com/inv/11",
            "url_host": "billing.example.com",
            "confidence": "certain",
            "score": 1000,
            "forwarded": false,
        })]
    );
    assert_eq!(
        of_type(insights, "pass"),
        [&json!({
            "type": "pass",
            "style": "boarding_pass",
            "organization": "Example Air",
            "description": "Boarding pass",
            "relevant": { "local": "2026-12-01T09:45:00", "offset": "+01:00", "zone": null },
            "boarding": {
                "pnr": "XK7Q2P",
                "passenger": "Ada Example",
                "legs": [
                    {
                        "from": "AMS",
                        "to": "JFK",
                        "carrier": "EX",
                        "flight": "123",
                        "date": "2026-12-01",
                        "seat": "12A",
                    },
                    {
                        "from": "JFK",
                        "to": "AMS",
                        "carrier": null,
                        "flight": null,
                        "date": { "month": 12, "day": 8 },
                        "seat": null,
                    },
                ],
            },
            "attachment": 2,
            "signature_checked": false,
            "confidence": "certain",
            "score": 1000,
            "forwarded": false,
        })]
    );

    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id.to_string())
        .unwrap();
    assert_eq!(
        row["insight_kinds"],
        json!(["code", "reservation", "order", "invoice", "pass"])
    );
    let listed = row.to_string();
    for value in ["XK7Q2P", "1234.50", "Hotel Example", "INV-2026-11"] {
        assert!(!listed.contains(value), "{value}");
    }

    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    for schema in [
        "ReservationDto",
        "TimeDto",
        "MoneyDto",
        "PassStyleDto",
        "BoardingPassDto",
        "BoardingLegDto",
        "PassDateDto",
    ] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "{schema}"
        );
    }
    let kinds: Vec<&str> = spec["components"]["schemas"]["ReservationDto"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["properties"]["kind"]["enum"].as_array().unwrap())
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "flight",
            "lodging",
            "train",
            "bus",
            "event",
            "food",
            "rental_car"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn members_and_own_mail_keep_structured_insights() {
    let f = fixture().await;
    let id = f.store_structured_insights().await;
    let member = f.member_session().await;
    let (status, _, body) = f
        .send_as(
            &member,
            Request::get(format!("/api/messages/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let detail: Value = serde_json::from_slice(&body).unwrap();
    let types: Vec<&Value> = detail["insights"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["type"])
        .collect();
    assert_eq!(
        types,
        [
            "reservation",
            "reservation",
            "reservation",
            "order",
            "order",
            "order",
            "order",
            "invoice",
            "pass"
        ]
    );
    assert!(!detail.to_string().contains("482913"));
    let (_, _, body) = f
        .send_as(
            &member,
            Request::get(format!("/api/archives/{}/messages", f.archive))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let page: Value = serde_json::from_slice(&body).unwrap();
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id.to_string())
        .unwrap();
    assert_eq!(
        row["insight_kinds"],
        json!(["reservation", "order", "invoice", "pass"])
    );

    f.store
        .create_account(NewAccount {
            archive_id: f.archive.parse().unwrap(),
            kind: AccountKind::Import,
            name: "News desk".into(),
            address: Some("news@example.com".into()),
            settings: Default::default(),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    let detail = f.json(&format!("/api/messages/{id}")).await;
    let insights = &detail["insights"];
    assert_eq!(insights["own"], true);
    assert!(of_type(insights, "code").is_empty());
    assert_eq!(of_type(insights, "reservation").len(), 3);
    assert_eq!(of_type(insights, "order").len(), 4);
    assert_eq!(of_type(insights, "invoice").len(), 1);
    assert_eq!(of_type(insights, "pass").len(), 1);
}

fn category_write(
    archive: &str,
    message: holtmail_core::MessageId,
    sender: &str,
    trust: holtmail_category::TrustState,
    kind: holtmail_category::MessageKind,
    topics: &[holtmail_category::Topic],
) -> holtmail_store::CategoryWrite {
    holtmail_store::CategoryWrite {
        message,
        archive: archive.parse().unwrap(),
        derive_fp: "derive-fp".into(),
        trust,
        trust_authserv: None,
        one_click_ok: false,
        own: false,
        sender_address: Some(sender.into()),
        sender_domain: sender.rsplit_once('@').map(|(_, d)| d.to_owned()),
        derived_kind: kind,
        derived_kind_source: holtmail_category::CategorySource::Headers,
        derived_topics: topics.iter().copied().collect(),
        derived_sources: topics
            .iter()
            .map(|t| (*t, holtmail_category::CategorySource::Domain))
            .collect(),
    }
}

impl Fixture {
    async fn categorize(&self, writes: &[holtmail_store::CategoryWrite]) {
        self.store
            .set_message_categories(writes, holtmail_store::CategoryPolicy { health: true })
            .await
            .unwrap();
    }

    async fn two_categorized(&self) -> (holtmail_core::MessageId, holtmail_core::MessageId) {
        use holtmail_category::{MessageKind, Topic, TrustState};
        let news = self.store_newsletter_insights(CURRENT_FP).await;
        let bob = self.message_id("Re: Weekly news").await;
        self.store_insights(bob, CURRENT_FP, r#"{"v":1,"items":[]}"#, &[])
            .await;
        self.categorize(&[
            category_write(
                &self.archive,
                news,
                "news@example.com",
                TrustState::Aligned {
                    domain: "example.com".into(),
                },
                MessageKind::Bulk,
                &[Topic::Travel],
            ),
            category_write(
                &self.archive,
                bob,
                "bob@example.org",
                TrustState::Unverified,
                MessageKind::Personal,
                &[],
            ),
        ])
        .await;
        (news, bob)
    }

    async fn ids_in(&self, query: &str) -> Vec<String> {
        let page = self
            .json(&format!("/api/archives/{}/messages?{query}", self.archive))
            .await;
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_owned())
            .collect()
    }

    fn changes(&self) -> usize {
        self.mailboxes.changed.lock().unwrap().len()
    }
}

fn counts(value: &Value) -> Vec<(String, u64)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_owned(),
                c["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn count_of(value: &Value, id: &str) -> Option<u64> {
    counts(value)
        .into_iter()
        .find(|(k, _)| k == id)
        .map(|(_, n)| n)
}

#[tokio::test(flavor = "multi_thread")]
async fn counts_lists_and_overrides_categories() {
    let f = fixture().await;
    let (news, bob) = f.two_categorized().await;
    let categories = format!("/api/archives/{}/categories", f.archive);
    let overrides = format!("/api/archives/{}/category-overrides", f.archive);

    let got = f.json(&categories).await;
    assert_eq!(
        counts(&got["kinds"]),
        [
            ("personal".to_owned(), 1),
            ("list".to_owned(), 0),
            ("bulk".to_owned(), 1),
            ("notification".to_owned(), 0)
        ]
    );
    let topics: Vec<String> = counts(&got["topics"]).into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        topics,
        [
            "purchases",
            "travel",
            "finance",
            "social",
            "jobs",
            "events",
            "government"
        ]
    );
    assert_eq!(count_of(&got["topics"], "travel"), Some(1));
    let with_health = f.json(&format!("{categories}?include=health")).await;
    assert_eq!(count_of(&with_health["topics"], "health"), Some(0));
    let (status, _, _) = f.get(&format!("{categories}?include=everything")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    assert_eq!(f.ids_in("category=bulk").await, [news.to_string()]);
    assert_eq!(f.ids_in("category=travel").await, [news.to_string()]);
    assert!(f.ids_in("category=health").await.is_empty());
    for bad in ["newsletter", "Bulk", "kind:bulk"] {
        let (status, _, _) = f
            .get(&format!(
                "/api/archives/{}/messages?category={bad}",
                f.archive
            ))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let row = |id: holtmail_core::MessageId| {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.to_string())
            .unwrap()
            .clone()
    };
    assert_eq!(row(news)["kind"], "bulk");
    assert_eq!(row(news)["topic"], "travel");
    assert_eq!(row(bob)["kind"], "personal");
    assert!(row(bob)["topic"].is_null());

    let before = f.changes();
    let (status, _, body) = f
        .send(request(
            "PUT",
            &overrides,
            json!({ "scope": "domain", "key": "Mail.Example.ORG.", "kind": "bulk" }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let first: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(first["scope"], "domain");
    assert_eq!(first["key"], "example.org");
    assert_eq!(first["kind"], "bulk");
    assert!(first["topics"].is_null());
    assert_eq!(f.changes(), before + 1);
    assert_eq!(
        *f.mailboxes.changed.lock().unwrap().last().unwrap(),
        f.archive.parse::<holtmail_core::ArchiveId>().unwrap()
    );
    assert_eq!(
        count_of(&f.json(&categories).await["kinds"], "bulk"),
        Some(2)
    );
    assert_eq!(f.ids_in("category=bulk").await.len(), 2);
    assert!(f.ids_in("category=personal").await.is_empty());
    let detail = f.json(&format!("/api/messages/{bob}")).await;
    assert_eq!(
        detail["insights"]["kind"],
        json!({ "id": "bulk", "source": "override", "overridden": true })
    );
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(
        detail["insights"]["kind"],
        json!({ "id": "bulk", "source": "headers", "overridden": false })
    );
    assert_eq!(
        detail["insights"]["topics"],
        json!([{ "id": "travel", "source": "domain", "overridden": false }])
    );

    let (status, _, body) = f
        .send(request(
            "PUT",
            &overrides,
            json!({ "scope": "domain", "key": "example.org", "kind": "notification", "topics": [] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let second: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(second["id"], first["id"]);
    assert_eq!(second["created_at"], first["created_at"]);
    assert_eq!(second["topics"], json!([]));
    let listed = f.json(&overrides).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["kind"], "notification");

    let (status, _, body) = f
        .send(request(
            "PUT",
            &overrides,
            json!({ "scope": "address", "key": " News@Example.COM ", "topics": ["purchases"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let address: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(address["key"], "news@example.com");
    assert!(address["kind"].is_null());
    let listed = f.json(&overrides).await;
    let scopes: Vec<&Value> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|o| &o["scope"])
        .collect();
    assert_eq!(scopes, ["address", "domain"]);
    assert_eq!(f.ids_in("category=purchases").await, [news.to_string()]);
    assert!(f.ids_in("category=travel").await.is_empty());

    let changes = f.changes();
    for body in [
        json!({ "scope": "domain", "key": "example.org" }),
        json!({ "scope": "domain", "key": "example.org", "kind": null, "topics": null }),
        json!({ "scope": "domain", "key": "example.org", "kind": "newsletter" }),
        json!({ "scope": "domain", "key": "example.org", "topics": ["travel", "gardening"] }),
        json!({ "scope": "sender", "key": "example.org", "kind": "bulk" }),
        json!({ "scope": "domain", "key": "", "kind": "bulk" }),
        json!({ "scope": "domain", "key": "localhost", "kind": "bulk" }),
        json!({ "scope": "address", "key": "example.org", "kind": "bulk" }),
    ] {
        let (status, _, answer) = f.send(request("PUT", &overrides, body.clone())).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{body}: {}",
            body_text(&answer)
        );
    }
    let (_, _, answer) = f
        .send(request(
            "PUT",
            &overrides,
            json!({ "scope": "domain", "key": "example.org" }),
        ))
        .await;
    assert!(body_text(&answer).contains("nothing to change"));
    assert_eq!(f.changes(), changes);

    let id = first["id"].as_str().unwrap();
    let other = f.store.create_archive("Work").await.unwrap().id;
    let (status, _, _) = f
        .send(request(
            "DELETE",
            &format!("/api/archives/{other}/category-overrides/{id}"),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = f
        .send(request("DELETE", &format!("{overrides}/{id}"), json!({})))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(f.changes(), changes + 1);
    assert_eq!(f.ids_in("category=personal").await, [bob.to_string()]);
    assert_eq!(
        count_of(&f.json(&categories).await["kinds"], "personal"),
        Some(1)
    );
    for uri in [
        format!("{overrides}/{id}"),
        format!("{overrides}/not-an-id"),
    ] {
        let (status, _, _) = f.send(request("DELETE", &uri, json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(
        f.json(&format!("/api/archives/{other}/category-overrides"))
            .await,
        json!([])
    );

    let missing = holtmail_core::ArchiveId::new();
    for (method, uri) in [
        ("GET", format!("/api/archives/{missing}/categories")),
        ("GET", format!("/api/archives/{missing}/category-overrides")),
        ("PUT", format!("/api/archives/{missing}/category-overrides")),
        (
            "DELETE",
            format!(
                "/api/archives/{missing}/category-overrides/{}",
                address["id"].as_str().unwrap()
            ),
        ),
        ("GET", "/api/archives/not-an-id/categories".to_owned()),
    ] {
        let (status, _, _) = f
            .send(request(
                method,
                &uri,
                json!({ "scope": "domain", "key": "example.org", "kind": "bulk" }),
            ))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }

    let spec: Value = serde_json::from_str(&openapi_json()).unwrap();
    for (path, method) in [
        ("/api/archives/{id}/categories", "get"),
        ("/api/archives/{id}/category-overrides", "get"),
        ("/api/archives/{id}/category-overrides", "put"),
        ("/api/archives/{id}/category-overrides/{override}", "delete"),
        ("/api/archives/{id}/messages", "get"),
    ] {
        for code in ["401", "403", "404", "500"] {
            assert!(
                spec["paths"][path][method]["responses"][code].is_object(),
                "{method} {path} {code}"
            );
        }
    }
    for schema in [
        "CategoriesDto",
        "CategoryOverrideDto",
        "CategoryOverrideRequest",
        "CategoryKindDto",
        "CategoryTopicDto",
        "HeldBackDto",
    ] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "{schema}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_messages_an_override_changes_for_reindex() {
    let f = fixture().await;
    let (news, _bob) = f.two_categorized().await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let status = format!("/api/archives/{}/insights/status", f.archive);
    assert_eq!(f.json(&status).await["reindex"], 0);

    let (code, _, body) = f
        .send(request(
            "PUT",
            &format!("/api/archives/{}/category-overrides", f.archive),
            json!({ "scope": "address", "key": "news@example.com", "topics": ["purchases"] }),
        ))
        .await;
    assert_eq!(code, StatusCode::OK, "{}", body_text(&body));
    let queued: Vec<holtmail_core::MessageId> = f
        .store
        .reindex_queue(archive, None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(queued, [news]);
    assert_eq!(
        *f.mailboxes.changed.lock().unwrap().last().unwrap(),
        archive
    );
    assert_eq!(f.json(&status).await["reindex"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn holds_back_sensitive_override_topics_from_unverified_senders() {
    let f = fixture().await;
    let (news, bob) = f.two_categorized().await;
    let overrides = format!("/api/archives/{}/category-overrides", f.archive);
    for key in ["example.org", "example.com"] {
        let (status, _, _) = f
            .send(request(
                "PUT",
                &overrides,
                json!({ "scope": "domain", "key": key, "topics": ["finance", "travel"] }),
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let detail = f.json(&format!("/api/messages/{bob}")).await;
    assert_eq!(detail["insights"]["trust"]["state"], "unverified");
    assert_eq!(
        detail["insights"]["topics"],
        json!([
            { "id": "travel", "source": "override", "overridden": true },
            { "id": "finance", "source": "override", "overridden": true, "held_back": "not_verified" },
        ])
    );
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(
        detail["insights"]["topics"],
        json!([
            { "id": "travel", "source": "override", "overridden": true },
            { "id": "finance", "source": "override", "overridden": true },
        ])
    );
    assert_eq!(f.ids_in("category=finance").await, [news.to_string()]);
    let got = f
        .json(&format!("/api/archives/{}/categories", f.archive))
        .await;
    assert_eq!(count_of(&got["topics"], "finance"), Some(1));
    assert_eq!(count_of(&got["topics"], "travel"), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_health_out_when_turned_off() {
    use holtmail_category::{MessageKind, Topic, TrustState};
    for health in [true, false] {
        let mut config = InsightsConfig::new(CURRENT_FP);
        config.health = health;
        let f = signed_in(empty_fixture_with(Some(config)).await).await;
        let news = f.store_newsletter_insights(CURRENT_FP).await;
        let aligned = TrustState::Aligned {
            domain: "example.com".into(),
        };
        f.categorize(&[category_write(
            &f.archive,
            news,
            "news@example.com",
            aligned,
            MessageKind::Bulk,
            &[Topic::Health],
        )])
        .await;
        let categories = format!("/api/archives/{}/categories", f.archive);
        let plain = f.json(&categories).await;
        assert_eq!(count_of(&plain["topics"], "health"), None, "{health}");
        let asked = f.json(&format!("{categories}?include=health")).await;
        assert_eq!(
            count_of(&asked["topics"], "health"),
            health.then_some(1),
            "{health}"
        );
        let page = f
            .json(&format!("/api/archives/{}/messages", f.archive))
            .await;
        assert!(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["topic"].is_null())
        );
        assert!(!page.to_string().contains("health"));
        let detail = f.json(&format!("/api/messages/{news}")).await;
        let topics = &detail["insights"]["topics"];
        if health {
            assert_eq!(topics[0]["id"], "health");
        } else {
            assert_eq!(*topics, json!([]));
        }
        assert_eq!(f.ids_in("category=health").await.len(), usize::from(health));
        let (status, _, _) = f
            .send(request(
                "PUT",
                &format!("/api/archives/{}/category-overrides", f.archive),
                json!({ "scope": "domain", "key": "example.com", "topics": ["health"] }),
            ))
            .await;
        let expected = if health {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        };
        assert_eq!(status, expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_trust_from_stored_rows() {
    use holtmail_category::{MessageKind, TrustState};
    let f = fixture().await;
    let news = f.store_newsletter_insights(CURRENT_FP).await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(
        detail["insights"]["trust"],
        json!({ "state": "unknown", "domain": null, "provider": null })
    );
    for (trust, expected) in [
        (
            TrustState::Aligned {
                domain: "example.com".into(),
            },
            json!({ "state": "aligned", "domain": "example.com", "provider": null }),
        ),
        (
            TrustState::Relayed {
                provider: "simplelogin".into(),
            },
            json!({ "state": "relayed", "domain": null, "provider": "simplelogin" }),
        ),
        (
            TrustState::Unverified,
            json!({ "state": "unverified", "domain": null, "provider": null }),
        ),
        (
            TrustState::Unknown,
            json!({ "state": "unknown", "domain": null, "provider": null }),
        ),
    ] {
        f.categorize(&[category_write(
            &f.archive,
            news,
            "news@example.com",
            trust,
            MessageKind::Bulk,
            &[],
        )])
        .await;
        let detail = f.json(&format!("/api/messages/{news}")).await;
        assert_eq!(detail["insights"]["trust"], expected);
    }

    let mut own = category_write(
        &f.archive,
        news,
        "news@example.com",
        TrustState::Unknown,
        MessageKind::Personal,
        &[],
    );
    own.own = true;
    f.categorize(&[own]).await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(detail["insights"]["own"], true);
    let types: Vec<&Value> = detail["insights"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["type"])
        .collect();
    assert_eq!(types, ["unsubscribe"]);
}

impl Fixture {
    async fn one_click(&self, message: holtmail_core::MessageId) -> Value {
        let detail = self.json(&format!("/api/messages/{message}")).await;
        detail["insights"]["unsubscribe"]["one_click"].clone()
    }

    async fn store_unsubscribe(
        &self,
        message: holtmail_core::MessageId,
        signals: &[holtmail_insight::Signal],
        unsubscribe_fields: u32,
        https: bool,
        one_click: bool,
    ) {
        let item = insight(
            holtmail_insight::InsightKind::Unsubscribe {
                https: https.then(|| {
                    holtmail_insight::Url::parse("https://lists.example.com/u/42").unwrap()
                }),
                mailto: Some("mailto:leave@lists.example.com".into()),
                http: None,
                one_click,
                list_id: None,
            },
            holtmail_insight::Confidence::Certain,
            1000,
        );
        let auth = json!({
            "v": 2,
            "results": [],
            "signatures": [],
            "fields": { "from": 1, "list-unsubscribe": unsubscribe_fields, "list-unsubscribe-post": 1 },
            "sig_cap_hit": false,
            "from_domain": "example.com",
            "from_addresses": 1,
        });
        let row = holtmail_store::InsightRow {
            message,
            archive: self.archive.parse().unwrap(),
            fingerprint: CURRENT_FP.into(),
            outcome: holtmail_store::InsightOutcome::Ok,
            truncated: false,
            from_address: Some("news@example.com".into()),
            signals: signals
                .iter()
                .copied()
                .collect::<holtmail_insight::Signals>()
                .bits(),
            kinds: holtmail_insight::KindTags::from(holtmail_insight::KindTag::Unsubscribe).bits(),
            auth: auth.to_string(),
            items: holtmail_insight::Items { items: vec![item] }.to_json(),
            computed_at: holtmail_core::Timestamp::now(),
        };
        self.store
            .set_message_insights(message, &row)
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn explains_why_one_click_is_not_offered() {
    use holtmail_category::{MessageKind, TrustState};
    use holtmail_insight::Signal;
    let f = fixture().await;
    let news = f.message_id("Weekly news").await;
    let post = [Signal::Unsubscribe, Signal::UnsubscribePost];
    let aligned = || TrustState::Aligned {
        domain: "example.com".into(),
    };
    let write = |trust: TrustState, ok: bool| {
        let mut write = category_write(
            &f.archive,
            news,
            "news@example.com",
            trust,
            MessageKind::Bulk,
            &[],
        );
        write.one_click_ok = ok;
        write
    };
    f.categorize(&[]).await;
    f.store_unsubscribe(news, &post, 1, true, true).await;
    f.categorize(&[write(aligned(), true)]).await;
    assert_eq!(
        f.one_click(news).await,
        json!({ "available": false, "reason": "disabled" })
    );

    for (signals, fields, https, one_click, trust, ok, expected) in [
        (&post[..1], 1, true, true, aligned(), true, "no_post_header"),
        (&post[..], 1, false, false, aligned(), true, "not_https"),
        (
            &post[..],
            2,
            true,
            false,
            aligned(),
            true,
            "multiple_fields",
        ),
        (&post[..], 1, true, false, aligned(), true, "no_post_header"),
        (
            &post[..],
            1,
            true,
            true,
            TrustState::Unknown,
            true,
            "no_trusted_authserv",
        ),
        (
            &post[..],
            1,
            true,
            true,
            TrustState::Unverified,
            true,
            "not_aligned",
        ),
        (
            &post[..],
            1,
            true,
            true,
            TrustState::Relayed {
                provider: "simplelogin".into(),
            },
            true,
            "not_aligned",
        ),
        (&post[..], 1, true, true, aligned(), false, "not_covered"),
    ] {
        f.store_unsubscribe(news, signals, fields, https, one_click)
            .await;
        f.categorize(&[write(trust, ok)]).await;
        assert_eq!(
            f.one_click(news).await,
            json!({ "available": false, "reason": expected }),
            "{expected}"
        );
    }

    f.store.invalidate_derived(&[news]).await.unwrap();
    f.store_unsubscribe(news, &post, 1, true, true).await;
    assert_eq!(
        f.one_click(news).await,
        json!({ "available": false, "reason": "no_trusted_authserv" })
    );

    f.categorize(&[write(aligned(), true)]).await;
    let member = f.member_session().await;
    let (_, _, body) = f
        .send_as(
            &member,
            Request::get(format!("/api/messages/{news}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let detail: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        detail["insights"]["unsubscribe"],
        json!({
            "https": null,
            "mailto": null,
            "http": null,
            "one_click": { "available": false, "reason": "members" },
        })
    );
    assert_eq!(detail["insights"]["kind"]["id"], "bulk");
    assert!(!detail.to_string().contains("lists.example.com/u/42"));
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_trusted_authentication_servers() {
    let f = fixture().await;
    let archive: holtmail_core::ArchiveId = f.archive.parse().unwrap();
    let imap = f
        .store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "Work".into(),
            address: Some("ada@example.com".into()),
            settings: json!({ "host": "imap.example.com" }),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    let accounts = format!("/api/archives/{}/accounts", f.archive);
    let listed = f.json(&accounts).await;
    let of = |listed: &Value, id: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()
            .clone()
    };
    let work = of(&listed, &imap.id.to_string());
    assert_eq!(work["trusted_authserv_ids"], json!([]));
    assert!(work["suggested_authserv_id"].is_null());
    let import = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "import")
        .unwrap()
        .clone();
    assert!(import["trusted_authserv_ids"].is_null());
    assert!(import["suggested_authserv_id"].is_null());

    let uri = format!("/api/accounts/{}", imap.id);
    let before = f.changes();
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "trusted_authserv_ids": [" mx.example.org", "MX.EXAMPLE.ORG", "mx2.example.org"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let dto: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        dto["trusted_authserv_ids"],
        json!(["mx.example.org", "mx2.example.org"])
    );
    assert_eq!(f.changes(), before + 1);
    assert_eq!(
        f.store.trusted_authserv_ids(archive).await.unwrap()[&imap.id],
        ["mx.example.org", "mx2.example.org"]
    );
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "trusted_authserv_ids": ["mx.example.org", "mx2.example.org"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(f.changes(), before + 1);
    let (status, _, _) = f
        .send(request("PATCH", &uri, json!({ "name": "Work mail" })))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(f.changes(), before + 1);

    let many: Vec<String> = (0..17).map(|i| format!("mx{i}.example.org")).collect();
    for body in [
        json!({ "trusted_authserv_ids": many }),
        json!({ "trusted_authserv_ids": ["mx example.org"] }),
        json!({ "trusted_authserv_ids": [""] }),
        json!({ "trusted_authserv_ids": ["a".repeat(256)] }),
    ] {
        let (status, _, _) = f.send(request("PATCH", &uri, body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, _, _) = f
        .send(request(
            "PATCH",
            &format!("/api/accounts/{}", import["id"].as_str().unwrap()),
            json!({ "trusted_authserv_ids": ["mx.example.org"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(f.changes(), before + 1);

    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "trusted_authserv_ids": [] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["trusted_authserv_ids"],
        json!([])
    );
    let folder = f
        .store
        .upsert_folder(imap.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    let auth = json!({
        "v": 2,
        "results": [{ "pos": 0, "authserv": "MX.example.org", "dkim": [] }],
        "signatures": [],
        "fields": { "from": 1 },
        "sig_cap_hit": false,
        "from_domain": "example.net",
        "from_addresses": 1,
    })
    .to_string();
    for n in 0..=50u64 {
        let raw = format!(
            "From: Shop <shop@example.net>\r\nTo: ada@example.com\r\nSubject: Receipt {n}\r\nMessage-ID: <r{n}@example.net>\r\nDate: Mon, 10 Mar 2025 08:00:00 +0000\r\n\r\nThanks.\r\n"
        );
        let id = f
            .ingest
            .ingest(
                archive,
                Bytes::from(raw),
                &Placement {
                    folder: Some(folder),
                    uid: Some(n + 1),
                    ..Placement::default()
                },
            )
            .await
            .unwrap()
            .message_id;
        let row = holtmail_store::InsightRow {
            message: id,
            archive,
            fingerprint: CURRENT_FP.into(),
            outcome: holtmail_store::InsightOutcome::Ok,
            truncated: false,
            from_address: None,
            signals: 0,
            kinds: 0,
            auth: if n == 0 {
                "{}".to_owned()
            } else {
                auth.clone()
            },
            items: r#"{"v":1,"items":[]}"#.into(),
            computed_at: holtmail_core::Timestamp::now(),
        };
        f.store.set_message_insights(id, &row).await.unwrap();
        if n == 49 {
            let work = of(&f.json(&accounts).await, &imap.id.to_string());
            assert!(
                work["suggested_authserv_id"].is_null(),
                "49 stamped messages"
            );
        }
    }
    let work = of(&f.json(&accounts).await, &imap.id.to_string());
    assert_eq!(work["suggested_authserv_id"], "mx.example.org");
    assert_eq!(work["trusted_authserv_ids"], json!([]));
    let (_, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "trusted_authserv_ids": ["mx.example.org"] }),
        ))
        .await;
    let dto: Value = serde_json::from_slice(&body).unwrap();
    assert!(dto["suggested_authserv_id"].is_null());
    assert_eq!(dto["trusted_authserv_ids"], json!(["mx.example.org"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn compares_link_destinations_with_the_derived_sender() {
    use holtmail_category::{MessageKind, TrustState};
    let f = fixture().await;
    let news = f.message_id("Weekly news").await;
    let items = holtmail_insight::Items {
        items: vec![action(
            holtmail_insight::LinkAction::ViewOrder,
            "https://shop.example.net/orders/7",
            "https://shop.example.net/orders/7",
            None,
            None,
            holtmail_insight::Confidence::Likely,
            800,
        )],
    };
    f.store_insights(
        news,
        CURRENT_FP,
        &items.to_json(),
        &[holtmail_insight::KindTag::Link],
    )
    .await;
    let differs =
        |detail: &Value| detail["insights"]["items"][0]["destination_differs_from_sender"].clone();
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(differs(&detail), true);
    f.categorize(&[category_write(
        &f.archive,
        news,
        "grace@example.net",
        TrustState::Unverified,
        MessageKind::Notification,
        &[],
    )])
    .await;
    let detail = f.json(&format!("/api/messages/{news}")).await;
    assert_eq!(differs(&detail), false);
}

#[derive(Clone, Copy)]
enum OneClickAnswer {
    Status(u16),
    Refused,
    Network,
}

#[derive(Default)]
struct FakeOneClick {
    calls: std::sync::Mutex<Vec<String>>,
    answer: std::sync::Mutex<Option<OneClickAnswer>>,
}

impl FakeOneClick {
    fn answer(&self, answer: OneClickAnswer) {
        *self.answer.lock().unwrap() = Some(answer);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl holtmail_fetch::OneClick for FakeOneClick {
    async fn post(
        &self,
        url: &str,
    ) -> Result<holtmail_fetch::OneClickResponse, holtmail_fetch::FetchError> {
        self.calls.lock().unwrap().push(url.to_owned());
        let answer = self
            .answer
            .lock()
            .unwrap()
            .unwrap_or(OneClickAnswer::Status(200));
        match answer {
            OneClickAnswer::Status(status) => Ok(holtmail_fetch::OneClickResponse {
                outcome: holtmail_fetch::OneClickOutcome::from_status(status),
                status,
            }),
            OneClickAnswer::Refused => Err(holtmail_fetch::FetchError::Refused(
                "news.example.com is not public".into(),
            )),
            OneClickAnswer::Network => Err(holtmail_fetch::FetchError::Unavailable(
                "news.example.com timed out".into(),
            )),
        }
    }
}

const ONE_CLICK_URL: &str = "https://news.example.com/unsubscribe/one/7f3a";
const PASS: &str = "dkim=pass header.d=news.example.com header.s=s1 header.b=AbCdEf12";
const SIGNED: &str = "from:to:subject:date:list-unsubscribe:list-unsubscribe-post";

fn one_click_mail(n: u32, list: &str, auth: &str, signed: &str, unsubscribe: &str) -> Vec<u8> {
    format!(
        "From: Example News <news@news.example.com>\r\n\
         To: Ada Example <ada@example.org>\r\n\
         Subject: Issue {n}\r\n\
         Date: Fri, 06 Nov 2026 09:12:00 +0000\r\n\
         Message-ID: <issue-{n}@news.example.com>\r\n\
         List-Id: Example News <{list}>\r\n\
         DKIM-Signature: v=1; a=rsa-sha256; c=relaxed/relaxed; d=news.example.com; s=s1; h={signed}; bh=Zm9vYmFyYmF6cXV4ZXhhbXBsZWJvZHloYXNoMDA=; b=AbCdEf12GhIj34KlMn56OpQr78StUv90WxYz\r\n\
         Authentication-Results: mx.example.org; spf=pass smtp.mailfrom=news.example.com; {auth}\r\n\
         List-Unsubscribe: <{unsubscribe}>, <mailto:leave@news.example.com>\r\n\
         List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         Three new features this week.\r\n"
    )
    .into_bytes()
}

async fn one_click_fixture(enabled: bool) -> (Fixture, Arc<FakeOneClick>) {
    let runner = Arc::new(holtmail_insights::Runner::new(vec![Arc::new(
        holtmail_insight_unsubscribe::UnsubscribeExtractor::new(),
    )]));
    let deriver = Arc::new(holtmail_insights::Deriver::new(
        Vec::new(),
        holtmail_insights::DeriveConfig {
            import_authserv_ids: vec!["mx.example.org".into()],
            ..Default::default()
        },
    ));
    let sender = Arc::new(FakeOneClick::default());
    let mut config = InsightsConfig::new(runner.extract_fp());
    config.deriver = Some(deriver.clone());
    config.one_click = enabled.then(|| sender.clone() as Arc<dyn holtmail_fetch::OneClick>);
    let f = signed_in(empty_fixture_with(Some(config)).await).await;
    let ingest = f
        .ingest
        .clone()
        .with_insights(runner.clone())
        .with_categories(deriver.clone(), deriver.policy());
    let _ = f.mailboxes.fresh.set(FreshInsights {
        ingest,
        fingerprint: runner.extract_fp(),
        deriver,
    });
    (f, sender)
}

impl Fixture {
    async fn ingest_mail(&self, raw: Vec<u8>, uid: u32) -> holtmail_core::MessageId {
        self.ingest
            .ingest(
                self.archive.parse().unwrap(),
                Bytes::from(raw),
                &Placement {
                    folder: Some(self.folder.parse().unwrap()),
                    uid: Some(u64::from(uid)),
                    ..Placement::default()
                },
            )
            .await
            .unwrap()
            .message_id
    }

    async fn newsletter_issue(&self, n: u32, list: &str) -> holtmail_core::MessageId {
        self.ingest_mail(
            one_click_mail(n, list, PASS, SIGNED, ONE_CLICK_URL),
            100 + n,
        )
        .await
    }

    async fn unsubscribe_with(
        &self,
        message: holtmail_core::MessageId,
        body: Value,
    ) -> (StatusCode, HeaderMap, Value) {
        let (status, headers, body) = self
            .send(post_json(
                &format!("/api/messages/{message}/unsubscribe"),
                body,
            ))
            .await;
        (
            status,
            headers,
            serde_json::from_slice(&body).unwrap_or(Value::Null),
        )
    }

    async fn unsubscribe(&self, message: holtmail_core::MessageId) -> (StatusCode, Value) {
        let (status, _, body) = self
            .unsubscribe_with(message, json!({ "confirm": true }))
            .await;
        (status, body)
    }

    async fn unsubscribe_dto(&self, message: holtmail_core::MessageId) -> Value {
        self.mailboxes.fresh_insights(message).await.unwrap();
        let detail = self.json(&format!("/api/messages/{message}")).await;
        detail["insights"]["unsubscribe"].clone()
    }

    async fn record_request(
        &self,
        list: &str,
        outcome: holtmail_store::UnsubscribeOutcome,
        age_ms: i64,
    ) {
        let user = self
            .store
            .find_user_by_email("ada@example.com")
            .await
            .unwrap()
            .unwrap();
        self.store
            .record_unsubscribe(&holtmail_store::UnsubscribeRequest {
                id: holtmail_store::UnsubscribeRequestId::new(),
                archive: self.archive.parse().unwrap(),
                message: None,
                list_key: list.into(),
                requested_at: holtmail_core::Timestamp::from_millis(
                    holtmail_core::Timestamp::now().as_millis() - age_ms,
                ),
                requested_by: user.id,
                outcome,
                status: None,
            })
            .await
            .unwrap();
    }
}

#[derive(Clone, Default)]
struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_one_click_unsubscribe_once_a_day() {
    let (f, sender) = one_click_fixture(true).await;
    let news = f.newsletter_issue(1, "weekly.news.example.com").await;
    let dto = f.unsubscribe_dto(news).await;
    assert_eq!(dto["https"], ONE_CLICK_URL);
    assert_eq!(
        dto["one_click"],
        json!({ "available": true, "reason": null })
    );
    assert!(dto.get("last_request").is_none(), "{dto}");

    for body in [
        json!({}),
        json!({ "confirm": false }),
        json!({ "confirm": "yes" }),
    ] {
        let (status, _, answer) = f.unsubscribe_with(news, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(answer["reason"].is_null(), "{answer}");
    }
    assert!(sender.calls().is_empty());

    // With a single live dispatcher, tracing-core caches callsite interest from whichever
    // thread registers first, so a parallel test could silence this one's logs.
    let _second_dispatcher = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let logs = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let (status, _, answer) = tracing::instrument::WithSubscriber::with_subscriber(
        f.unsubscribe_with(
            news,
            json!({ "confirm": true, "url": "https://elsewhere.example.org/u" }),
        ),
        subscriber,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["repeated"], false);
    assert_eq!(answer["request"]["outcome"], "accepted");
    assert_eq!(answer["request"]["status"], 200);
    assert!(answer["request"]["requested_at"].as_i64().unwrap() > 0);
    assert_eq!(sender.calls(), [ONE_CLICK_URL]);
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("accepted"), "{logs}");
    assert!(logs.contains(&news.to_string()), "{logs}");
    for secret in ["news.example.com", "7f3a", "weekly", "elsewhere"] {
        assert!(!logs.contains(secret), "{secret} in {logs}");
    }

    let dto = f.unsubscribe_dto(news).await;
    assert_eq!(
        dto["one_click"],
        json!({ "available": false, "reason": "recently_requested" })
    );
    assert_eq!(dto["last_request"]["outcome"], "accepted");
    assert_eq!(dto["last_request"]["status"], 200);

    let (status, again) = f.unsubscribe(news).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["repeated"], true);
    assert_eq!(again["request"], answer["request"]);
    let next = f.newsletter_issue(2, "weekly.news.example.com").await;
    let (status, again) = f.unsubscribe(next).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["repeated"], true);
    assert_eq!(sender.calls().len(), 1, "no second request within 24 h");

    let thread = f.json(&format!("/api/messages/{next}")).await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let thread = f.json(&format!("/api/threads/{thread}")).await;
    assert_eq!(
        thread["messages"][0]["insights"]["unsubscribe"]["last_request"]["outcome"],
        "accepted"
    );

    f.record_request(
        "old.news.example.com",
        holtmail_store::UnsubscribeOutcome::Accepted,
        25 * 60 * 60 * 1000,
    )
    .await;
    let old = f.newsletter_issue(3, "old.news.example.com").await;
    let dto = f.unsubscribe_dto(old).await;
    assert_eq!(dto["one_click"]["available"], true);
    assert_eq!(dto["last_request"]["outcome"], "accepted");
    let (status, answer) = f.unsubscribe(old).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["repeated"], false);
    assert_eq!(sender.calls().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_one_click_a_minute_after_a_failure() {
    let (f, sender) = one_click_fixture(true).await;
    sender.answer(OneClickAnswer::Network);
    let news = f.newsletter_issue(1, "a.news.example.com").await;
    let (status, answer) = f.unsubscribe(news).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["request"]["outcome"], "network");
    assert!(answer["request"]["status"].is_null());
    let (status, headers, _) = f.unsubscribe_with(news, json!({ "confirm": true })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert_eq!(sender.calls().len(), 1);
    let dto = f.unsubscribe_dto(news).await;
    assert_eq!(dto["one_click"]["available"], true);
    assert_eq!(dto["last_request"]["outcome"], "network");

    f.record_request(
        "b.news.example.com",
        holtmail_store::UnsubscribeOutcome::Network,
        61_000,
    )
    .await;
    sender.answer(OneClickAnswer::Status(302));
    let later = f.newsletter_issue(2, "b.news.example.com").await;
    let (status, answer) = f.unsubscribe(later).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["request"]["outcome"], "redirected");
    assert_eq!(answer["request"]["status"], 302);
    assert_eq!(sender.calls().len(), 2);

    for (n, answer, outcome, code) in [
        (3, OneClickAnswer::Refused, "refused", Value::Null),
        (4, OneClickAnswer::Status(500), "http_error", json!(500)),
    ] {
        sender.answer(answer);
        let id = f
            .newsletter_issue(n, &format!("{n}.news.example.com"))
            .await;
        let (status, answer) = f.unsubscribe(id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["request"]["outcome"], outcome);
        assert_eq!(answer["request"]["status"], code);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_one_click_unless_every_check_passes() {
    let (f, sender) = one_click_fixture(true).await;
    let other = "dkim=pass header.d=other.example.org header.s=s1 header.b=AbCdEf12";
    let unsigned_post = "from:to:subject:date:list-unsubscribe";
    for (n, auth, signed, url, reason) in [
        (1, PASS, unsigned_post, ONE_CLICK_URL, "not_covered"),
        (2, other, SIGNED, ONE_CLICK_URL, "not_aligned"),
        (3, PASS, SIGNED, "http://news.example.com/u", "not_https"),
        (
            4,
            PASS,
            SIGNED,
            "https://news.example.com:8443/u",
            "not_https",
        ),
        (
            5,
            PASS,
            SIGNED,
            "https://user:pw@news.example.com/u",
            "not_https",
        ),
    ] {
        let id = f
            .ingest_mail(
                one_click_mail(n, &format!("{n}.news.example.com"), auth, signed, url),
                100 + n,
            )
            .await;
        let (status, answer) = f.unsubscribe(id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{reason}: {answer}");
        assert_eq!(answer["reason"], reason);
        let dto = f.unsubscribe_dto(id).await;
        assert_eq!(
            dto["one_click"],
            json!({ "available": false, "reason": reason })
        );
    }
    assert!(sender.calls().is_empty());

    let news = f.newsletter_issue(9, "weekly.news.example.com").await;
    f.record_request(
        "weekly.news.example.com",
        holtmail_store::UnsubscribeOutcome::Accepted,
        1000,
    )
    .await;
    f.mailboxes.fresh_insights(news).await.unwrap();
    let member = f.member_session().await;
    let (status, _, _) = f
        .send_as(
            &member,
            post_json(
                &format!("/api/messages/{news}/unsubscribe"),
                json!({ "confirm": true }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, _, body) = f
        .send_as(
            &member,
            Request::get(format!("/api/messages/{news}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let detail: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        detail["insights"]["unsubscribe"],
        json!({
            "https": null,
            "mailto": null,
            "http": null,
            "one_click": { "available": false, "reason": "members" },
        })
    );

    for uri in [
        format!(
            "/api/messages/{}/unsubscribe",
            holtmail_core::MessageId::new()
        ),
        "/api/messages/nope/unsubscribe".to_owned(),
    ] {
        let (status, _, _) = f.send(post_json(&uri, json!({ "confirm": true }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert!(sender.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn turns_one_click_off_with_the_kill_switch() {
    let (f, sender) = one_click_fixture(false).await;
    let news = f.newsletter_issue(1, "weekly.news.example.com").await;
    let (status, answer) = f.unsubscribe(news).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(answer["reason"], "disabled");
    assert_eq!(
        f.unsubscribe_dto(news).await["one_click"],
        json!({ "available": false, "reason": "disabled" })
    );
    assert!(sender.calls().is_empty());

    let off = signed_in(empty_fixture_with(None).await).await;
    let id = off.message_id("Weekly news").await;
    let (status, _) = off.unsubscribe(id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn recomputes_stale_rows_before_one_click() {
    let (f, sender) = one_click_fixture(true).await;
    let news = f.newsletter_issue(1, "1.news.example.com").await;
    f.store_insights(news, "fp-old", r#"{"v":1,"items":[]}"#, &[])
        .await;
    let (status, answer) = f.unsubscribe(news).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["request"]["outcome"], "accepted");
    assert!(f.mailboxes.freshened.lock().unwrap().contains(&news));
    assert_eq!(sender.calls().len(), 1);

    let stale = f.newsletter_issue(2, "2.news.example.com").await;
    f.mailboxes.fresh_insights(stale).await.unwrap();
    let mut write = f
        .store
        .insight_view(stale)
        .await
        .unwrap()
        .unwrap()
        .categories
        .unwrap()
        .row
        .write;
    assert!(write.trust.is_aligned());
    write.derive_fp = "derive-old".into();
    write.trust = holtmail_category::TrustState::Unknown;
    write.one_click_ok = false;
    f.categorize(&[write]).await;
    let detail = f.json(&format!("/api/messages/{stale}")).await;
    assert_eq!(
        detail["insights"]["unsubscribe"]["one_click"]["reason"],
        "no_trusted_authserv"
    );
    let (status, answer) = f.unsubscribe(stale).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let row = f.store.insight_view(stale).await.unwrap().unwrap();
    assert_ne!(row.categories.unwrap().row.write.derive_fp, "derive-old");

    let missing = f.newsletter_issue(3, "3.news.example.com").await;
    f.mailboxes.fresh_insights(missing).await.unwrap();
    f.store.invalidate_derived(&[missing]).await.unwrap();
    let detail = f.json(&format!("/api/messages/{missing}")).await;
    assert_eq!(
        detail["insights"]["unsubscribe"]["one_click"]["reason"],
        "no_trusted_authserv"
    );
    let (status, answer) = f.unsubscribe(missing).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(sender.calls().len(), 3);
}

fn loaded(name: &str, origin: DataOrigin, attribution: Option<&str>) -> LoadedDataFile {
    LoadedDataFile {
        name: name.into(),
        file: format!("{name}-categories.tsv.gz"),
        version: "2026110801".into(),
        rows: 28_370,
        license: if name == "ut1" {
            "CC-BY-SA-4.0"
        } else {
            "CC0-1.0"
        }
        .into(),
        origin,
        attribution: attribution.map(Into::into),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_the_category_data_to_every_user() {
    let config = InsightsConfig::new(CURRENT_FP);
    let data = config.data.clone();
    let f = signed_in(empty_fixture_with(Some(config)).await).await;

    let (status, _, _) = f
        .send_as(
            "",
            Request::get("/api/insights/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let empty = f.json("/api/insights/data").await;
    assert_eq!(empty["holtmail_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(empty["files"], json!([]));
    assert_eq!(
        empty["updates"],
        json!({ "enabled": false, "data_sources": [], "last_checked_at": null, "last_outcome": null })
    );

    data.set(DataSnapshot {
        files: vec![
            loaded(
                "ut1",
                DataOrigin::Downloaded,
                Some("Contains data from UT1"),
            ),
            loaded("wikidata", DataOrigin::Bundled, None),
        ],
        updates: DataUpdates {
            enabled: true,
            data_sources: vec!["wikidata".into(), "ut1".into()],
            last_checked_at: Some(holtmail_core::Timestamp::from_millis(1_794_009_600_000)),
            last_outcome: Some(DataOutcome::NotModified),
        },
    });
    let expected = json!({
        "holtmail_version": env!("CARGO_PKG_VERSION"),
        "files": [
            {
                "name": "ut1",
                "file": "ut1-categories.tsv.gz",
                "version": "2026110801",
                "rows": 28370,
                "license": "CC-BY-SA-4.0",
                "origin": "downloaded",
                "attribution": "Contains data from UT1"
            },
            {
                "name": "wikidata",
                "file": "wikidata-categories.tsv.gz",
                "version": "2026110801",
                "rows": 28370,
                "license": "CC0-1.0",
                "origin": "bundled",
                "attribution": null
            }
        ],
        "updates": {
            "enabled": true,
            "data_sources": ["wikidata", "ut1"],
            "last_checked_at": 1_794_009_600_000i64,
            "last_outcome": "not_modified"
        }
    });
    assert_eq!(f.json("/api/insights/data").await, expected);

    f.send(post_json(
        "/api/users",
        json!({ "email": "grace@example.org", "name": "Grace", "password": "compiler 1952" }),
    ))
    .await;
    let member = f.sign_in("grace@example.org", "compiler 1952").await;
    let (status, _, body) = f
        .send_as(
            &member,
            Request::get("/api/insights/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
    let (status, _, _) = f
        .send_as(&member, request("POST", "/api/insights/data", json!({})))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_the_version_with_insights_off() {
    let f = signed_in(empty_fixture_with(None).await).await;
    let off = f.json("/api/insights/data").await;
    assert_eq!(off["holtmail_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(off["files"], json!([]));
    assert_eq!(off["updates"]["enabled"], false);
    assert_eq!(off["updates"]["data_sources"], json!([]));
}
