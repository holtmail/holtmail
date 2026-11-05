use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_api::{
    AccountChange, AccountDetails, AccountStatus, ImageProxy, MailboxError, Mailboxes,
    NewImapAccount, RemoteImageJob, SyncSchedule, Task, TaskKind, TaskLevel, TaskState,
};
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_ingest::Ingest;
use holtmail_remote::{RemoteImages, Saved, Scope};
use holtmail_source::{Source, SourceError};
use holtmail_source_imap::{ImapSettings, ImapSource};
use holtmail_store::{Account, AccountKind, ArchivedWhen};
use holtmail_sync::Syncer;

use crate::accounts;
use crate::config::Services;
use crate::import;
use crate::schedule::{self, Schedule};
use crate::tasks::{NewTask, TaskHandle, Tasks};

const LOG_EVERY: u64 = 250;

/// Runs syncs, imports and remote image work as tasks, at most one per account and one remote
/// image task per archive at a time, for the API and the scheduler.
#[derive(Clone)]
pub struct Runner(Arc<Inner>);

struct Inner {
    services: Arc<Services>,
    syncer: Syncer,
    ingest: Ingest,
    uploads: PathBuf,
    status: Mutex<HashMap<AccountId, AccountStatus>>,
    remote: RemoteImages,
    tasks: Arc<Tasks>,
    again: Mutex<HashSet<ArchiveId>>,
    schedule: Arc<Schedule>,
    next: Mutex<HashMap<AccountId, Timestamp>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    New,
    Older,
    Failed,
}

impl Job {
    fn describe(self) -> &'static str {
        match self {
            Job::New => "Saving images of new mail",
            Job::Older => "Saving images of mail archived before saving was turned on",
            Job::Failed => "Fetching again images that could not be saved",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Job::Failed => "images",
            _ => "messages",
        }
    }
}

fn saved_summary(job: Job, saved: &Saved) -> String {
    format!(
        "{} of {} {} · {} saved · {} already saved · {} failed",
        saved.messages,
        saved.total,
        job.unit(),
        saved.fetched,
        saved.known,
        saved.failed
    )
}

impl Runner {
    pub fn new(services: Arc<Services>, uploads: PathBuf, schedule: Arc<Schedule>) -> Self {
        let syncer = accounts::syncer(&services);
        let ingest = Ingest::new(
            services.blobs.clone(),
            services.store.clone(),
            services.index.clone(),
        );
        let remote = RemoteImages::new(
            services.fetcher.clone(),
            services.blobs.clone(),
            services.store.clone(),
        );
        Self(Arc::new(Inner {
            services,
            syncer,
            ingest,
            uploads,
            status: Mutex::new(HashMap::new()),
            remote,
            tasks: Arc::new(Tasks::default()),
            again: Mutex::new(HashSet::new()),
            schedule,
            next: Mutex::new(HashMap::new()),
        }))
    }

    /// Saves images of new mail for every archive that has saving on and mail waiting.
    pub async fn save_new_remote_images(&self) {
        match self.0.services.store.list_archives().await {
            Ok(archives) => {
                for archive in archives.into_iter().filter(|a| a.save_remote_images) {
                    let _ = self.start_remote(archive.id, Job::New).await;
                }
            }
            Err(error) => tracing::warn!(%error, "listing archives failed"),
        }
    }

    async fn start_remote(&self, archive_id: ArchiveId, job: Job) -> Result<Task, MailboxError> {
        if let Some(running) = self.0.tasks.running(TaskKind::RemoteImages, archive_id) {
            if job == Job::New {
                self.0.again.lock().expect("again lock").insert(archive_id);
            }
            return Ok(running);
        }
        let task = self.begin_remote(archive_id, job).await?;
        let snapshot = task.snapshot().expect("just started");
        let runner = self.clone();
        tokio::spawn(async move { runner.run_remote(archive_id, job, task).await });
        Ok(snapshot)
    }

    async fn begin_remote(
        &self,
        archive_id: ArchiveId,
        job: Job,
    ) -> Result<TaskHandle, MailboxError> {
        let store = &self.0.services.store;
        let archive = store
            .get_archive(archive_id)
            .await
            .map_err(backend)?
            .ok_or_else(|| MailboxError::Invalid("There is no such archive.".into()))?;
        let Some(since) = archive.save_remote_images_since else {
            return Err(MailboxError::Invalid(
                "Saving remote images is off for this archive.".into(),
            ));
        };
        if job == Job::New
            && store
                .count_unscanned(archive_id, ArchivedWhen::Since(since))
                .await
                .map_err(backend)?
                == 0
        {
            return Err(MailboxError::Invalid("No new mail to look at.".into()));
        }
        let task = self.0.tasks.start(NewTask {
            kind: TaskKind::RemoteImages,
            archive: Some(archive_id),
            account: None,
            title: archive.name,
            cancellable: true,
        });
        task.log(TaskLevel::Info, job.describe());
        Ok(task)
    }

    async fn run_remote(&self, archive: ArchiveId, mut job: Job, mut task: TaskHandle) {
        loop {
            self.run_remote_once(archive, job, &task).await;
            if !self.0.again.lock().expect("again lock").remove(&archive) {
                return;
            }
            match self.begin_remote(archive, Job::New).await {
                Ok(next) => {
                    task = next;
                    job = Job::New;
                }
                Err(_) => return,
            }
        }
    }

    async fn run_remote_once(&self, archive: ArchiveId, job: Job, task: &TaskHandle) {
        let progress = |saved: &Saved| {
            task.progress(saved.messages, Some(saved.total), saved_summary(job, saved));
            if saved.messages > 0 && saved.messages.is_multiple_of(LOG_EVERY) {
                task.log(TaskLevel::Info, saved_summary(job, saved));
            }
        };
        let remote = &self.0.remote;
        let outcome = match job {
            Job::New => {
                remote
                    .save_archive(archive, Scope::New, &task.cancel, progress)
                    .await
            }
            Job::Older => {
                remote
                    .save_archive(archive, Scope::Older, &task.cancel, progress)
                    .await
            }
            Job::Failed => remote.retry_failed(archive, &task.cancel, progress).await,
        };
        match outcome {
            Ok(saved) => {
                tracing::info!(%archive, task = task.id(), messages = saved.messages, fetched = saved.fetched, known = saved.known, failed = saved.failed, cancelled = saved.cancelled, "remote images");
                let summary = saved_summary(job, &saved);
                if saved.failed > 0 {
                    task.log(
                        TaskLevel::Warning,
                        format!(
                            "{} images could not be fetched; Retry failed images tries them again.",
                            saved.failed
                        ),
                    );
                }
                if saved.cancelled {
                    task.log(TaskLevel::Warning, "Stopped on request.");
                    task.finish(TaskState::Cancelled, summary);
                } else {
                    task.log(TaskLevel::Info, format!("Done: {summary}."));
                    task.finish(TaskState::Done, summary);
                }
            }
            Err(error) => {
                tracing::warn!(%archive, task = task.id(), %error, "saving remote images failed");
                task.log(TaskLevel::Error, error.to_string());
                task.finish(TaskState::Failed, "Stopped by an error");
            }
        }
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
        let task = self.0.tasks.start(NewTask {
            kind: TaskKind::Sync,
            archive: Some(account.archive_id),
            account: Some(account.id),
            title: account.name.clone(),
            cancellable: false,
        });
        task.log(TaskLevel::Info, "Connecting to the server");
        let outcome = accounts::sync_one(&self.0.services, &self.0.syncer, account).await;
        match &outcome {
            Ok(report) => {
                let failed = report.folders.iter().find_map(|f| f.error.clone());
                tracing::info!(account = %account.id, new = report.new_messages(), folders = report.folders.len(), "synced");
                for folder in &report.folders {
                    let line = format!(
                        "{}: {} new, {} known, {} removed, {} failed{}",
                        folder.path,
                        folder.new,
                        folder.known,
                        folder.removed,
                        folder.failed,
                        if folder.renumbered {
                            ", renumbered by the server"
                        } else {
                            ""
                        }
                    );
                    match &folder.error {
                        Some(error) => task.log(TaskLevel::Error, format!("{line}: {error}")),
                        None if folder.failed > 0 => task.log(TaskLevel::Warning, line),
                        None => task.log(TaskLevel::Info, line),
                    }
                }
                let folders = report.folders.len() as u64;
                let summary = format!(
                    "{} new messages in {folders} folders",
                    report.new_messages()
                );
                task.progress(folders, Some(folders), summary.clone());
                task.finish(
                    if failed.is_some() {
                        TaskState::Failed
                    } else {
                        TaskState::Done
                    },
                    summary,
                );
                self.finish(
                    account.id,
                    match failed {
                        Some(error) => Err(error),
                        None => Ok(report.new_messages()),
                    },
                );
                if report.new_messages() > 0 {
                    let _ = self.start_remote(account.archive_id, Job::New).await;
                }
            }
            Err(error) => {
                tracing::warn!(account = %account.id, %error, "sync failed");
                let error = format!("{error:#}");
                task.log(TaskLevel::Error, error.clone());
                task.finish(TaskState::Failed, "The sync failed");
                self.finish(account.id, Err(error));
            }
        }
    }

    async fn import_now(&self, account: Account, file: PathBuf) {
        let task = self.0.tasks.start(NewTask {
            kind: TaskKind::Import,
            archive: Some(account.archive_id),
            account: Some(account.id),
            title: account.name.clone(),
            cancellable: false,
        });
        task.log(TaskLevel::Info, "Reading the uploaded file");
        let runner = self.clone();
        let outcome = import::import_path(
            &*self.0.services.store,
            &self.0.ingest,
            account.archive_id,
            account.id,
            &file,
            |totals| {
                runner.progress(account.id, totals.new);
                let done = totals.new + totals.known + totals.failed;
                task.progress(done, None, import_summary(totals));
                if done > 0 && done.is_multiple_of(LOG_EVERY * 4) {
                    task.log(TaskLevel::Info, import_summary(totals));
                }
            },
        )
        .await;
        let _ = tokio::fs::remove_file(&file).await;
        match outcome {
            Ok(totals) => {
                tracing::info!(account = %account.id, new = totals.new, known = totals.known, failed = totals.failed, "imported");
                let summary = import_summary(&totals);
                if totals.failed > 0 {
                    task.log(
                        TaskLevel::Warning,
                        format!("{} messages could not be read", totals.failed),
                    );
                }
                task.log(TaskLevel::Info, format!("Done: {summary}."));
                task.finish(TaskState::Done, summary);
                self.finish(account.id, Ok(totals.new));
                if totals.new > 0 {
                    let _ = self.start_remote(account.archive_id, Job::New).await;
                }
            }
            Err(error) => {
                tracing::warn!(account = %account.id, %error, "import failed");
                let reason = import_error(&error);
                task.log(TaskLevel::Error, reason.clone());
                task.finish(TaskState::Failed, "The import failed");
                self.finish(account.id, Err(reason));
            }
        }
    }

    /// Checks every 30 seconds which enabled accounts are due, by their own interval or the
    /// server's, and syncs those one after another. Accounts are due at start.
    pub async fn run_schedule(self) {
        loop {
            match accounts::syncable_accounts(&*self.0.services.store).await {
                Ok(list) => {
                    let mut next = HashMap::new();
                    let mut due = Vec::new();
                    let now = Timestamp::now();
                    let every = self.0.schedule.get().minutes;
                    for account in list {
                        let minutes = accounts::sync_interval(&account).unwrap_or(every);
                        let last = self.status(account.id).finished_at;
                        if let Some(at) = schedule::next_sync(last, minutes, now) {
                            next.insert(account.id, at);
                            if at <= now {
                                due.push(account);
                            }
                        }
                    }
                    *self.0.next.lock().expect("next lock") = next;
                    for account in due {
                        self.sync_now(&account).await;
                    }
                }
                Err(error) => tracing::warn!(%error, "listing accounts failed"),
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }
}

fn import_summary(totals: &import::Totals) -> String {
    format!(
        "{} new, {} already archived, {} failed",
        totals.new, totals.known, totals.failed
    )
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

    async fn configure(
        &self,
        account: &Account,
        change: AccountChange,
    ) -> Result<Account, MailboxError> {
        let connects = change.connection.is_some() || change.password.is_some();
        if account.kind == AccountKind::Import
            && (connects || change.sync_interval_minutes.is_some())
        {
            return Err(MailboxError::Invalid(
                "An import account has no server.".into(),
            ));
        }
        if connects && self.status(account.id).running {
            return Err(MailboxError::Invalid(
                "The account is syncing; change it when the sync is done.".into(),
            ));
        }
        let mut updated = account.clone();
        if let Some(Some(minutes)) = change.sync_interval_minutes {
            schedule::check_minutes(minutes)?;
        }
        if connects {
            let secrets = &self.0.services.secrets;
            let (settings, imap) = match &change.connection {
                Some(connection) => accounts::with_connection(&account.settings, connection)
                    .map_err(|e| MailboxError::Invalid(format!("{e:#}")))?,
                None => (
                    account.settings.clone(),
                    ImapSettings::from_json(&account.settings)
                        .map_err(|e| MailboxError::Invalid(e.to_string()))?,
                ),
            };
            let password = match &change.password {
                Some(password) => password.clone(),
                None => {
                    let sealed = account.sealed_secret.as_deref().ok_or_else(|| {
                        MailboxError::Invalid("Enter the password for this account.".into())
                    })?;
                    String::from_utf8(
                        secrets
                            .open(&account.id.to_string(), sealed)
                            .map_err(backend)?,
                    )
                    .map_err(backend)?
                }
            };
            check_sign_in(&imap, &password).await?;
            if let Some(connection) = &change.connection
                && let Some(old) = accounts::connection(account)
                && connection.username.trim() != old.username
                && (account.address.is_none() || account.address.as_deref() == Some(&old.username))
            {
                updated.address = imap.username.contains('@').then(|| imap.username.clone());
            }
            updated.settings = settings;
            if change.password.is_some() {
                updated.sealed_secret = Some(
                    secrets
                        .seal(&account.id.to_string(), password.as_bytes())
                        .map_err(backend)?,
                );
            }
        }
        if let Some(minutes) = change.sync_interval_minutes {
            updated.settings = accounts::with_sync_interval(&updated.settings, minutes);
        }
        self.0
            .services
            .store
            .update_account(&updated)
            .await
            .map_err(backend)?;
        Ok(updated)
    }

    fn details(&self, account: &Account) -> AccountDetails {
        AccountDetails {
            connection: accounts::connection(account),
            sync_interval_minutes: accounts::sync_interval(account),
        }
    }

    fn sync_schedule(&self) -> SyncSchedule {
        self.0.schedule.get()
    }

    async fn set_sync_schedule(&self, minutes: u64) -> Result<SyncSchedule, MailboxError> {
        self.0.schedule.set(minutes).await
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

    fn tasks(&self) -> Vec<Task> {
        self.0.tasks.list()
    }

    fn cancel_task(&self, id: &str) -> Result<(), MailboxError> {
        self.0
            .tasks
            .cancel(id)
            .map_err(|reason| MailboxError::Invalid(reason.into()))
    }

    async fn start_remote_images(
        &self,
        archive: ArchiveId,
        job: RemoteImageJob,
    ) -> Result<Task, MailboxError> {
        self.start_remote(
            archive,
            match job {
                RemoteImageJob::Older => Job::Older,
                RemoteImageJob::Failed => Job::Failed,
            },
        )
        .await
    }

    fn image_proxy(&self) -> ImageProxy {
        self.0.services.fetcher.status()
    }

    async fn set_image_proxy(&self, proxy: Option<String>) -> Result<ImageProxy, MailboxError> {
        self.0.services.fetcher.set(proxy).await
    }

    fn upload_dir(&self) -> &Path {
        &self.0.uploads
    }

    fn status(&self, account: AccountId) -> AccountStatus {
        let mut status = self
            .0
            .status
            .lock()
            .expect("status lock")
            .get(&account)
            .cloned()
            .unwrap_or_default();
        if !status.running {
            status.next_sync_at = self
                .0
                .next
                .lock()
                .expect("next lock")
                .get(&account)
                .copied();
        }
        status
    }
}
