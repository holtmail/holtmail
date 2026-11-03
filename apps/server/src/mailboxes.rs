use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_api::{AccountStatus, MailboxError, Mailboxes, NewImapAccount};
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_ingest::Ingest;
use holtmail_source::{Source, SourceError};
use holtmail_source_imap::{ImapSettings, ImapSource};
use holtmail_store::{Account, AccountKind};
use holtmail_sync::Syncer;

use crate::accounts;
use crate::config::Services;
use crate::import;

/// Runs syncs and imports, at most one per account at a time, for the API and the scheduler.
#[derive(Clone)]
pub struct Runner(Arc<Inner>);

struct Inner {
    services: Arc<Services>,
    syncer: Syncer,
    ingest: Ingest,
    uploads: PathBuf,
    status: Mutex<HashMap<AccountId, AccountStatus>>,
}

impl Runner {
    pub fn new(services: Arc<Services>, uploads: PathBuf) -> Self {
        let syncer = accounts::syncer(&services);
        let ingest = Ingest::new(
            services.blobs.clone(),
            services.store.clone(),
            services.index.clone(),
        );
        Self(Arc::new(Inner {
            services,
            syncer,
            ingest,
            uploads,
            status: Mutex::new(HashMap::new()),
        }))
    }

    fn begin(&self, account: AccountId) -> bool {
        let mut status = self.0.status.lock().expect("status lock");
        let entry = status.entry(account).or_default();
        if entry.running {
            return false;
        }
        *entry = AccountStatus {
            running: true,
            ..AccountStatus::default()
        };
        true
    }

    fn progress(&self, account: AccountId, new_messages: u64) {
        if let Some(entry) = self.0.status.lock().expect("status lock").get_mut(&account) {
            entry.new_messages = new_messages;
        }
    }

    fn finish(&self, account: AccountId, outcome: Result<u64, String>) {
        let mut status = self.0.status.lock().expect("status lock");
        let entry = status.entry(account).or_default();
        entry.running = false;
        entry.finished_at = Some(Timestamp::now());
        match outcome {
            Ok(new) => {
                entry.new_messages = new;
                entry.error = None;
            }
            Err(error) => entry.error = Some(error),
        }
    }

    /// Syncs `account` and waits for it; returns at once when a run of it is already going.
    pub async fn sync_now(&self, account: &Account) {
        if !self.begin(account.id) {
            return;
        }
        let outcome = accounts::sync_one(&self.0.services, &self.0.syncer, account).await;
        match &outcome {
            Ok(report) => {
                let failed = report.folders.iter().find_map(|f| f.error.clone());
                tracing::info!(account = %account.id, new = report.new_messages(), folders = report.folders.len(), "synced");
                self.finish(
                    account.id,
                    match failed {
                        Some(error) => Err(error),
                        None => Ok(report.new_messages()),
                    },
                );
            }
            Err(error) => {
                tracing::warn!(account = %account.id, %error, "sync failed");
                self.finish(account.id, Err(format!("{error:#}")));
            }
        }
    }

    async fn import_now(&self, account: Account, file: PathBuf) {
        let runner = self.clone();
        let outcome = import::import_path(
            &*self.0.services.store,
            &self.0.ingest,
            account.archive_id,
            account.id,
            &file,
            |totals| runner.progress(account.id, totals.new),
        )
        .await;
        let _ = tokio::fs::remove_file(&file).await;
        match outcome {
            Ok(totals) => {
                tracing::info!(account = %account.id, new = totals.new, known = totals.known, failed = totals.failed, "imported");
                self.finish(account.id, Ok(totals.new));
            }
            Err(error) => {
                tracing::warn!(account = %account.id, %error, "import failed");
                self.finish(account.id, Err(import_error(&error)));
            }
        }
    }

    /// Syncs every enabled account, one after another, then waits `every` and starts again.
    pub async fn run_schedule(self, every: Duration) {
        loop {
            match accounts::syncable_accounts(&*self.0.services.store).await {
                Ok(list) => {
                    for account in list {
                        self.sync_now(&account).await;
                    }
                }
                Err(error) => tracing::warn!(%error, "listing accounts failed"),
            }
            tokio::time::sleep(every).await;
        }
    }
}

fn import_error(error: &anyhow::Error) -> String {
    match error.downcast_ref::<holtmail_import::ImportError>() {
        Some(holtmail_import::ImportError::UnknownFormat(_)) => {
            "The file is not an mbox, .eml or zip file.".to_owned()
        }
        _ => "The file could not be read.".to_owned(),
    }
}

fn sign_in_error(error: SourceError, settings: &ImapSettings) -> MailboxError {
    let place = format!("{}:{}", settings.host, settings.port());
    MailboxError::Invalid(match error {
        SourceError::Auth(_) => {
            "The server refused this user name and password. Some providers need an app password."
                .to_owned()
        }
        SourceError::Connection(reason) => {
            let reason = reason
                .strip_prefix(&format!("{place}: "))
                .unwrap_or(&reason);
            format!("Could not connect to {place}: {reason}")
        }
        SourceError::Protocol(reason) => format!("{place} answered unexpectedly: {reason}"),
        SourceError::Config(reason) => reason,
    })
}

async fn check_sign_in(settings: &ImapSettings, password: &str) -> Result<(), MailboxError> {
    let source = ImapSource::new(settings.clone(), password.to_owned());
    let mut session = source
        .connect()
        .await
        .map_err(|e| sign_in_error(e, settings))?;
    session
        .folders()
        .await
        .map_err(|e| sign_in_error(e, settings))?;
    let _ = session.logout().await;
    Ok(())
}

fn backend(error: impl std::fmt::Display) -> MailboxError {
    MailboxError::Backend(error.to_string())
}

#[async_trait]
impl Mailboxes for Runner {
    async fn add_imap(
        &self,
        archive: ArchiveId,
        account: NewImapAccount,
    ) -> Result<Account, MailboxError> {
        let security = accounts::parse_security(&account.security)
            .map_err(|e| MailboxError::Invalid(e.to_string()))?;
        let settings = ImapSettings {
            host: account.host.clone(),
            port: account.port,
            security,
            username: account.username.clone(),
        };
        check_sign_in(&settings, &account.password).await?;
        let stored = accounts::add_imap(
            &*self.0.services.store,
            &*self.0.services.secrets,
            accounts::NewImapAccount {
                archive,
                name: account.name,
                host: account.host,
                port: account.port,
                security,
                username: account.username,
                password: account.password,
                exclude: Vec::new(),
            },
        )
        .await
        .map_err(backend)?;
        self.sync(&stored).await?;
        Ok(stored)
    }

    async fn set_password(&self, account: &Account, password: String) -> Result<(), MailboxError> {
        let settings = ImapSettings::from_json(&account.settings)
            .map_err(|e| MailboxError::Invalid(e.to_string()))?;
        check_sign_in(&settings, &password).await?;
        let mut account = account.clone();
        account.sealed_secret = Some(
            self.0
                .services
                .secrets
                .seal(&account.id.to_string(), password.as_bytes())
                .map_err(backend)?,
        );
        self.0
            .services
            .store
            .update_account(&account)
            .await
            .map_err(backend)
    }

    async fn sync(&self, account: &Account) -> Result<(), MailboxError> {
        if account.kind == AccountKind::Import {
            return Err(MailboxError::Invalid(
                "An import account has no server.".into(),
            ));
        }
        let runner = self.clone();
        let account = account.clone();
        tokio::spawn(async move { runner.sync_now(&account).await });
        Ok(())
    }

    async fn import(&self, account: &Account, file: PathBuf) -> Result<(), MailboxError> {
        if !self.begin(account.id) {
            let _ = tokio::fs::remove_file(&file).await;
            return Err(MailboxError::Invalid(
                "This file is still being imported.".into(),
            ));
        }
        let runner = self.clone();
        let account = account.clone();
        tokio::spawn(async move { runner.import_now(account, file).await });
        Ok(())
    }

    fn upload_dir(&self) -> &Path {
        &self.0.uploads
    }

    fn status(&self, account: AccountId) -> AccountStatus {
        self.0
            .status
            .lock()
            .expect("status lock")
            .get(&account)
            .cloned()
            .unwrap_or_default()
    }
}
