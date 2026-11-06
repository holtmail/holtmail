use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_api::{
    AccountChange, AccountDetails, AccountStatus, ContactsCalendarsRun, ImageProxy, MailboxError,
    Mailboxes, NewImapAccount, NewJmapAccount, OAuthFinished, OAuthProvider, OAuthStart,
    OAuthStarted, OAuthTarget, RemoteImageJob, SyncSchedule, Task, TaskKind, TaskLevel, TaskState,
};
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_ingest::Ingest;
use holtmail_oauth::{Access, Provider};
use holtmail_pim_sync::{ChangeMode, PimReport, PimSyncer};

use crate::people::PeopleRefresh;
use holtmail_remote::{RemoteImages, Saved, Scope};
use holtmail_source::Credentials;
use holtmail_source_imap::ImapSettings;
use holtmail_store::{Account, AccountKind, AccountRun, ArchivedWhen, StoreError, TaskRecord};
use holtmail_sync::Syncer;

use crate::accounts;
use crate::config::Services;
use crate::dav;
use crate::import;
use crate::oauth::{OAuthSettings, Purpose};
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
    pim: PimSyncer,
    pim_running: Mutex<HashSet<AccountId>>,
    ingest: Ingest,
    uploads: PathBuf,
    status: Mutex<HashMap<AccountId, AccountStatus>>,
    remote: RemoteImages,
    tasks: Arc<Tasks>,
    again: Mutex<HashSet<ArchiveId>>,
    schedule: Arc<Schedule>,
    next: Mutex<HashMap<AccountId, Timestamp>>,
    due: Mutex<HashSet<AccountId>>,
    interrupted: Mutex<Vec<TaskRecord>>,
    people: Arc<PeopleRefresh>,
    reviews: crate::deletion::Reviews,
}

/// An account's contacts sync marked as running until dropped, also when the sync panics.
struct PimRunning<'a> {
    set: &'a Mutex<HashSet<AccountId>>,
    account: AccountId,
}

impl<'a> PimRunning<'a> {
    fn claim(set: &'a Mutex<HashSet<AccountId>>, account: AccountId) -> Option<Self> {
        set.lock()
            .expect("pim lock")
            .insert(account)
            .then_some(Self { set, account })
    }
}

impl Drop for PimRunning<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.set.lock() {
            set.remove(&self.account);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    New,
    Older,
    Failed,
}

impl Job {
    fn as_str(self) -> &'static str {
        match self {
            Job::New => "new",
            Job::Older => "older",
            Job::Failed => "failed",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Job::New, Job::Older, Job::Failed]
            .into_iter()
            .find(|job| job.as_str() == name)
    }

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
    pub(crate) fn services(&self) -> &Arc<Services> {
        &self.0.services
    }

    pub(crate) fn task_list(&self) -> &Arc<Tasks> {
        &self.0.tasks
    }

    pub(crate) fn reviews(&self) -> &crate::deletion::Reviews {
        &self.0.reviews
    }

    pub(crate) fn ingester(&self) -> &Ingest {
        &self.0.ingest
    }

    pub(crate) fn refresh_people(&self, archive: ArchiveId) {
        self.0.people.request(archive);
    }

    /// A runner with the tasks and last runs of earlier processes: tasks they left running
    /// are marked interrupted, accounts whose sync was interrupted are due at once, and
    /// [`Runner::resume`] starts interrupted image work again.
    pub async fn open(
        services: Arc<Services>,
        uploads: PathBuf,
        schedule: Arc<Schedule>,
    ) -> anyhow::Result<Self> {
        let (tasks, interrupted) = Tasks::open(services.store.clone()).await?;
        tasks.set_hooks(services.hooks.clone());
        let status = services
            .store
            .last_runs()
            .await?
            .into_iter()
            .map(|(account, run)| {
                (
                    account,
                    AccountStatus {
                        running: false,
                        new_messages: run.new_messages,
                        finished_at: Some(run.finished_at),
                        error: run.error,
                        next_sync_at: None,
                        contacts_calendars: None,
                    },
                )
            })
            .collect();
        let due = interrupted
            .iter()
            .filter(|t| t.kind == TaskKind::Sync)
            .filter_map(|t| t.account_id)
            .collect();
        if !interrupted.is_empty() {
            tracing::info!(
                tasks = interrupted.len(),
                "tasks stopped by the last shutdown marked interrupted"
            );
        }
        let syncer = accounts::syncer(&services);
        let ingest = services.ingest();
        let remote = RemoteImages::new(
            services.fetcher.clone(),
            services.blobs.clone(),
            services.store.clone(),
        );
        let pim = PimSyncer::new(services.store.clone(), ingest.clone());
        let people = PeopleRefresh::new(services.store.clone());
        let runner = Self(Arc::new(Inner {
            services,
            syncer,
            pim,
            pim_running: Mutex::new(HashSet::new()),
            ingest,
            uploads,
            status: Mutex::new(status),
            remote,
            tasks,
            again: Mutex::new(HashSet::new()),
            schedule,
            next: Mutex::new(HashMap::new()),
            due: Mutex::new(due),
            interrupted: Mutex::new(interrupted),
            people,
            reviews: Default::default(),
        }));
        runner.clear_uploads().await;
        Ok(runner)
    }

    async fn clear_uploads(&self) {
        let Ok(mut dir) = tokio::fs::read_dir(&self.0.uploads).await else {
            return;
        };
        let mut removed = 0u64;
        while let Ok(Some(entry)) = dir.next_entry().await {
            if entry.file_type().await.is_ok_and(|t| t.is_file())
                && tokio::fs::remove_file(entry.path()).await.is_ok()
            {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(files = removed, "uploads of interrupted imports removed");
        }
    }

    /// Writes task changes to the store behind the work, until the process ends.
    pub async fn write_tasks(self) {
        self.0.tasks.clone().write_behind().await;
    }

    /// Writes every task change made so far to the store.
    pub async fn flush_tasks(&self) {
        self.0.tasks.flush().await;
    }

    /// Starts again the remote image work a restart interrupted, then saves images of new mail
    /// for every archive that has saving on and mail waiting.
    pub async fn resume(&self) {
        let interrupted =
            std::mem::take(&mut *self.0.interrupted.lock().expect("interrupted lock"));
        let mut seen = HashSet::new();
        for task in interrupted {
            let (TaskKind::RemoteImages, Some(archive), Some(job)) = (
                task.kind,
                task.archive_id,
                task.job.as_deref().and_then(Job::parse),
            ) else {
                continue;
            };
            if job == Job::New || !seen.insert(archive) {
                continue;
            }
            match self.start_remote(archive, job).await {
                Ok(_) => {
                    tracing::info!(%archive, job = job.as_str(), "interrupted image work resumed")
                }
                Err(error) => {
                    tracing::info!(%archive, %error, "interrupted image work not resumed")
                }
            }
        }
        self.save_new_remote_images().await;
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
            job: Some(job.as_str().into()),
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
        drop(status);
        self.0.due.lock().expect("due lock").remove(&account);
        true
    }

    fn progress(&self, account: AccountId, new_messages: u64) {
        if let Some(entry) = self.0.status.lock().expect("status lock").get_mut(&account) {
            entry.new_messages = new_messages;
        }
    }

    async fn finish(&self, account: AccountId, outcome: Result<u64, String>) {
        let run = {
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
            AccountRun {
                finished_at: entry.finished_at.unwrap_or_else(Timestamp::now),
                new_messages: entry.new_messages,
                error: entry.error.clone(),
            }
        };
        match self.0.services.store.set_last_run(account, &run).await {
            Ok(()) | Err(StoreError::NotFound(_)) => {}
            Err(error) => tracing::warn!(%account, %error, "last run not stored"),
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
            job: None,
        });
        task.log(TaskLevel::Info, "Connecting to the server");
        let outcome = accounts::sync_one(&self.0.services, &self.0.syncer, account).await;
        match &outcome {
            Ok(report) => {
                let failed = report.folders.iter().find_map(|f| f.error.clone());
                tracing::info!(account = %account.id, new = report.new_messages(), folders = report.folders.len(), "synced");
                for folder in &report.folders {
                    let line = format!(
                        "{}: {} new, {} known, {} removed, {} changed, {} failed{}{}",
                        folder.path,
                        folder.new,
                        folder.known,
                        folder.removed,
                        folder.flags_changed,
                        folder.failed,
                        match (folder.deferred, folder.deleted) {
                            (0, 0) => String::new(),
                            (0, deleted) => format!(", {deleted} deleted earlier left out"),
                            (deferred, 0) => {
                                format!(", {deferred} left until they are old enough")
                            }
                            (deferred, deleted) => format!(
                                ", {deferred} left until they are old enough, {deleted} deleted earlier left out"
                            ),
                        },
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
                )
                .await;
                if report.new_messages() > 0 {
                    self.0.people.request(account.archive_id);
                    let _ = self.start_remote(account.archive_id, Job::New).await;
                }
            }
            Err(error) => {
                tracing::warn!(account = %account.id, %error, "sync failed");
                let error = format!("{error:#}");
                task.log(TaskLevel::Error, error.clone());
                task.finish(TaskState::Failed, "The sync failed");
                self.finish(account.id, Err(error)).await;
            }
        }
        self.sync_contacts_now(account.id).await;
    }

    /// Syncs the contacts and calendars of `account` as a task of its own and waits for it, when
    /// they are on and the account may sign in for them; returns at once when a run is going.
    pub async fn sync_contacts_now(&self, account: AccountId) {
        let account = match self.0.services.store.get_account(account).await {
            Ok(Some(account)) => account,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%account, %error, "account not read");
                return;
            }
        };
        if !dav::DavSettings::of(&account).is_some_and(|d| d.enabled)
            || !dav::available(&account)
            || dav::needs_consent(&account)
            || OAuthSettings::of(&account).is_some_and(|o| o.needs_sign_in)
        {
            return;
        }
        let Some(_running) = PimRunning::claim(&self.0.pim_running, account.id) else {
            return;
        };
        let task = self.0.tasks.start(NewTask {
            kind: TaskKind::ContactsCalendars,
            archive: Some(account.archive_id),
            account: Some(account.id),
            title: account.name.clone(),
            cancellable: false,
            job: None,
        });
        task.log(
            TaskLevel::Info,
            "Connecting to the contacts and calendars server",
        );
        let services = &self.0.services;
        let outcome = match dav::source_for(
            &account,
            &*services.store,
            &*services.secrets,
            &services.oauth,
        )
        .await
        {
            Ok(source) => self
                .0
                .pim
                .sync(account.archive_id, account.id, &*source)
                .await
                .map_err(|error| match error {
                    holtmail_pim_sync::PimSyncError::Source(error) => {
                        if matches!(error, holtmail_collection::CollectionError::Auth(_))
                            && OAuthSettings::of(&account).is_some()
                        {
                            services.oauth.forget(account.id);
                        }
                        dav::reason(&error)
                    }
                    other => other.to_string(),
                }),
            Err(error) => Err(format!("{error:#}")),
        };
        match outcome {
            Ok(report) => {
                self.log_contacts(&task, &report).await;
                if report.removed_collections > 0
                    || report
                        .collections
                        .iter()
                        .any(|c| c.new + c.changed + c.removed > 0)
                {
                    self.0.people.request(account.archive_id);
                }
                let summary = contacts_summary(&report);
                task.progress(
                    report.collections.len() as u64,
                    Some(report.collections.len() as u64),
                    summary.clone(),
                );
                task.finish(
                    if report.error().is_some() {
                        TaskState::Failed
                    } else {
                        TaskState::Done
                    },
                    summary,
                );
            }
            Err(error) => {
                tracing::warn!(account = %account.id, "contacts and calendars sync failed");
                task.log(TaskLevel::Error, error);
                task.finish(TaskState::Failed, "The contacts and calendars sync failed");
            }
        }
    }

    async fn log_contacts(&self, task: &TaskHandle, report: &PimReport) {
        for entry in &report.collections {
            let name = match self.0.services.store.get_collection(entry.collection).await {
                Ok(Some(c)) => c.name,
                _ => entry.collection.to_string(),
            };
            let how = match entry.mode {
                ChangeMode::Unchanged => "unchanged",
                ChangeMode::SinceToken => "changes since the last sync",
                ChangeMode::Full => "read in full",
                ChangeMode::TokenExpired => "read in full, the server forgot the last sync",
                ChangeMode::Listing => "listed in full, the server cannot list changes",
            };
            let line = format!(
                "{}{}: {} new, {} changed, {} unchanged, {} removed, {} failed ({how})",
                name,
                if entry.created { " (new)" } else { "" },
                entry.new,
                entry.changed,
                entry.same,
                entry.removed,
                entry.failed,
            );
            match &entry.error {
                Some(error) => task.log(TaskLevel::Error, format!("{line}: {error}")),
                None if entry.failed > 0 => task.log(TaskLevel::Warning, line),
                None => task.log(TaskLevel::Info, line),
            }
        }
        if report.removed_collections > 0 {
            task.log(
                TaskLevel::Info,
                format!(
                    "{} address books or calendars are gone from the server and marked removed",
                    report.removed_collections
                ),
            );
        }
    }

    fn contacts_run(&self, account: AccountId) -> Option<ContactsCalendarsRun> {
        let task = self
            .0
            .tasks
            .list()
            .into_iter()
            .filter(|t| t.kind == TaskKind::ContactsCalendars && t.account_id == Some(account))
            .max_by_key(|t| t.started_at)?;
        let error = (task.state == TaskState::Failed).then(|| {
            task.log
                .iter()
                .rev()
                .find(|l| l.level == TaskLevel::Error)
                .map_or_else(|| task.summary.clone(), |l| l.text.clone())
        });
        Some(ContactsCalendarsRun {
            running: task.state == TaskState::Running,
            finished_at: task.finished_at,
            summary: task.summary,
            error,
        })
    }

    async fn import_now(&self, account: Account, file: PathBuf) {
        let task = self.0.tasks.start(NewTask {
            kind: TaskKind::Import,
            archive: Some(account.archive_id),
            account: Some(account.id),
            title: account.name.clone(),
            cancellable: false,
            job: None,
        });
        task.log(TaskLevel::Info, "Reading the uploaded file");
        let runner = self.clone();
        let outcome = import::import_path(
            &*self.0.services.store,
            &self.0.ingest,
            account.archive_id,
            account.id,
            &file,
            Some(&account.name),
            |totals| {
                runner.progress(account.id, totals.new);
                let done = totals.new + totals.known + totals.failed + totals.deleted;
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
                self.finish(account.id, Ok(totals.new)).await;
                self.0.people.request(account.archive_id);
                if totals.new > 0 {
                    let _ = self.start_remote(account.archive_id, Job::New).await;
                }
            }
            Err(error) => {
                tracing::warn!(account = %account.id, %error, "import failed");
                let reason = import_error(&error);
                task.log(TaskLevel::Error, reason.clone());
                task.finish(TaskState::Failed, "The import failed");
                self.finish(account.id, Err(reason)).await;
            }
        }
    }

    /// Checks every 30 seconds which enabled accounts are due, by their own interval or the
    /// server's, and syncs those one after another. An account is due by its last stored run; one
    /// that never ran is due at once, and so is one whose sync a restart interrupted, also when it
    /// syncs only on request.
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
                        let interrupted =
                            self.0.due.lock().expect("due lock").contains(&account.id);
                        let last = self.status(account.id).finished_at;
                        if let Some(at) = schedule::due_at(last, minutes, interrupted, now) {
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

fn contacts_summary(report: &PimReport) -> String {
    format!(
        "{} new, {} changed, {} removed in {} address books and calendars",
        report.new_items(),
        report.changed_items(),
        report.removed_items(),
        report.collections.len()
    )
}

fn import_summary(totals: &import::Totals) -> String {
    totals.summary()
}

fn import_error(error: &anyhow::Error) -> String {
    match error.downcast_ref::<holtmail_import::ImportError>() {
        Some(holtmail_import::ImportError::UnknownFormat(_)) => {
            "The file is not an mbox, .eml, .vcf, .ics or zip file.".to_owned()
        }
        _ => "The file could not be read.".to_owned(),
    }
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
        accounts::check_sign_in(&settings, Credentials::Password(account.password.clone())).await?;
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

    async fn add_jmap(
        &self,
        archive: ArchiveId,
        account: NewJmapAccount,
    ) -> Result<Account, MailboxError> {
        let auth = accounts::parse_jmap_auth(&account.auth)
            .map_err(|e| MailboxError::Invalid(e.to_string()))?;
        let settings = accounts::jmap_settings(&account.url, &account.username, auth)
            .map_err(|e| MailboxError::Invalid(e.to_string()))?;
        accounts::check_jmap_sign_in(&settings, &account.secret).await?;
        let stored = accounts::add_jmap(
            &*self.0.services.store,
            &*self.0.services.secrets,
            accounts::NewJmapAccount {
                archive,
                name: account.name,
                settings,
                secret: account.secret,
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
        if (change.connection.is_some() || change.jmap.is_some() || change.password.is_some())
            && self.status(account.id).running
        {
            return Err(MailboxError::Invalid(
                "The account is syncing; change it when the sync is done.".into(),
            ));
        }
        let services = &self.0.services;
        let turns_on = change
            .contacts_calendars
            .as_ref()
            .is_some_and(|c| c.enabled);
        let stored = accounts::configure(
            &*services.store,
            &*services.secrets,
            &services.oauth,
            account,
            change,
        )
        .await?;
        if turns_on && stored.enabled {
            let runner = self.clone();
            let id = stored.id;
            tokio::spawn(async move { runner.sync_contacts_now(id).await });
        }
        Ok(stored)
    }

    fn details(&self, account: &Account) -> AccountDetails {
        let oauth = OAuthSettings::of(account);
        AccountDetails {
            connection: accounts::connection(account),
            jmap: accounts::jmap_connection(account),
            sync_interval_minutes: accounts::sync_interval(account),
            min_age_days: accounts::min_age_days(account),
            needs_sign_in: oauth.as_ref().is_some_and(|o| o.needs_sign_in),
            oauth_provider: oauth.map(|o| o.provider),
            contacts_calendars: dav::details(account),
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
        if let Some(oauth) = OAuthSettings::of(account)
            && oauth.needs_sign_in
        {
            return Err(MailboxError::Invalid(crate::oauth::sign_in_again(
                oauth.provider().unwrap_or(Provider::Google),
            )));
        }
        let runner = self.clone();
        let account = account.clone();
        tokio::spawn(async move { runner.sync_now(&account).await });
        Ok(())
    }

    async fn start_oauth(&self, start: OAuthStart) -> Result<OAuthStarted, MailboxError> {
        let provider = Provider::parse(&start.provider)
            .ok_or_else(|| MailboxError::Invalid("Choose Google or Microsoft.".into()))?;
        let store = &self.0.services.store;
        let mut wanted = None;
        let (email, purpose, kind) = match start.target {
            OAuthTarget::NewAccount { archive, name } => {
                store
                    .get_archive(archive)
                    .await
                    .map_err(backend)?
                    .ok_or_else(|| MailboxError::Invalid("There is no such archive.".into()))?;
                (
                    start.email,
                    Purpose::NewAccount { archive, name },
                    start.kind,
                )
            }
            OAuthTarget::Account(id) => {
                let account = store
                    .get_account(id)
                    .await
                    .map_err(backend)?
                    .ok_or_else(|| MailboxError::Invalid("There is no such account.".into()))?;
                if !matches!(account.kind, AccountKind::Imap | AccountKind::Graph) {
                    return Err(MailboxError::Invalid(
                        "Only IMAP and Microsoft Graph accounts sign in with Google or Microsoft."
                            .into(),
                    ));
                }
                let email = match accounts::oauth_email(&account) {
                    Some(email) => email,
                    None if start.email.contains('@') => start.email,
                    None => account
                        .address
                        .clone()
                        .filter(|a| a.contains('@'))
                        .ok_or_else(|| MailboxError::Invalid("Enter the email address.".into()))?,
                };
                wanted = Some(dav::wanted_access(&account));
                (email, Purpose::SignInAgain(id), account.kind)
            }
        };
        let access = match kind {
            AccountKind::Imap if provider == Provider::Google => wanted.unwrap_or(Access::Imap),
            AccountKind::Imap => Access::Imap,
            AccountKind::Graph if provider == Provider::Microsoft => {
                wanted.unwrap_or(Access::Graph)
            }
            AccountKind::Graph => {
                return Err(MailboxError::Invalid(
                    "Microsoft Graph accounts sign in with Microsoft.".into(),
                ));
            }
            _ => {
                return Err(MailboxError::Invalid(
                    "Only IMAP and Microsoft Graph accounts sign in with Google or Microsoft."
                        .into(),
                ));
            }
        };
        let started = self
            .0
            .services
            .oauth
            .start(provider, access, email.trim(), purpose, start.return_to)
            .map_err(backend)?;
        Ok(OAuthStarted {
            authorize_url: started.url,
            state: started.state,
            needs_paste: started.needs_paste,
            thunderbird_client: started.thunderbird,
            expires_at: Timestamp::from_millis(
                Timestamp::now().as_millis() + started.expires_in.as_millis() as i64,
            ),
        })
    }

    async fn finish_oauth(&self, answer: &str) -> Result<OAuthFinished, MailboxError> {
        let return_to = self
            .oauth_return_to(answer)
            .unwrap_or_else(|| "/settings".to_owned());
        let account = accounts::finish_oauth(&self.0.services, answer).await?;
        self.sync(&account).await?;
        Ok(OAuthFinished { account, return_to })
    }

    fn oauth_providers(&self) -> Vec<OAuthProvider> {
        holtmail_oauth::Provider::ALL
            .into_iter()
            .filter_map(|provider| {
                let thunderbird = self.0.services.oauth.offered(provider)?;
                Some(OAuthProvider {
                    provider: provider.as_str().to_owned(),
                    thunderbird_client: thunderbird,
                })
            })
            .collect()
    }

    fn oauth_return_to(&self, answer: &str) -> Option<String> {
        self.0
            .services
            .oauth
            .return_to(&accounts::url_state(answer)?)
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

    async fn rebuild_people(&self, archive: ArchiveId) -> Result<(), MailboxError> {
        if self.0.people.rebuild_now(archive).await {
            Ok(())
        } else {
            Err(MailboxError::Backend(
                "The people could not be rebuilt".into(),
            ))
        }
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
        status.contacts_calendars = self.contacts_run(account);
        status
    }
}
