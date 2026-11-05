use std::path::{Path, PathBuf};

use async_trait::async_trait;
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_store::Account;

/// Work the API hands to the server because it needs implementation crates: talking to mail
/// servers, sealing passwords and reading mailbox files.
#[async_trait]
pub trait Mailboxes: Send + Sync {
    /// Signs in to the server and lists its folders, then stores the account in `archive` with
    /// its password sealed and starts its first sync in the background.
    ///
    /// Nothing is stored when signing in fails.
    async fn add_imap(
        &self,
        archive: ArchiveId,
        account: NewImapAccount,
    ) -> Result<Account, MailboxError>;

    /// Changes how `account` connects and how often it is synced, as `change` says. A new
    /// connection or password is checked by signing in first, and nothing is stored when that
    /// fails. Returns the stored account. `Invalid` for an import account, for a change while the
    /// account syncs, or for an interval out of range.
    async fn configure(
        &self,
        account: &Account,
        change: AccountChange,
    ) -> Result<Account, MailboxError>;

    /// How `account` connects and its own sync interval, as stored.
    fn details(&self, account: &Account) -> AccountDetails;

    /// The sync interval of accounts without their own.
    fn sync_schedule(&self) -> SyncSchedule;

    /// Sets the sync interval of accounts without their own, in minutes; 0 syncs them only on
    /// request. `Invalid` above a week, or when the server's config sets it.
    async fn set_sync_schedule(&self, minutes: u64) -> Result<SyncSchedule, MailboxError>;

    /// Starts syncing `account` in the background. Does nothing when a sync of it is already
    /// running.
    async fn sync(&self, account: &Account) -> Result<(), MailboxError>;

    /// Starts importing the mailbox file at `file` into the import account `account` in the
    /// background, and removes the file when done. The file sits in [`Mailboxes::upload_dir`] and
    /// keeps the extension of the uploaded name.
    async fn import(&self, account: &Account, file: PathBuf) -> Result<(), MailboxError>;

    /// Where the API writes uploaded files before [`Mailboxes::import`].
    fn upload_dir(&self) -> &Path;

    /// What is happening to `account` now and how its last run ended.
    fn status(&self, account: AccountId) -> AccountStatus;

    /// Background tasks: every running one and the most recent finished ones, newest first.
    fn tasks(&self) -> Vec<Task>;

    /// Asks a running task to stop between two steps. `Invalid` for an unknown or finished task,
    /// or one that cannot be cancelled.
    fn cancel_task(&self, id: &str) -> Result<(), MailboxError>;

    /// Starts a remote image task for `archive` in the background and returns it; returns the
    /// running one instead when the archive already has one. `Invalid` when saving is off.
    async fn start_remote_images(
        &self,
        archive: ArchiveId,
        job: RemoteImageJob,
    ) -> Result<Task, MailboxError>;

    /// The outbound proxy remote images are fetched through.
    fn image_proxy(&self) -> ImageProxy;

    /// Sets or, with `None`, removes the outbound proxy for remote images, after checking that it
    /// can be reached. It applies at once to the reader's proxy and to saving. `Invalid` for a bad
    /// or unreachable proxy, or when the server's config sets it.
    async fn set_image_proxy(&self, proxy: Option<String>) -> Result<ImageProxy, MailboxError>;
}

/// Remote image work started on request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteImageJob {
    /// Save the images of messages archived before saving was turned on.
    Older,
    /// Fetch again the images that could not be saved.
    Failed,
}

/// What a background task does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// Syncing an account.
    Sync,
    /// Importing an uploaded file.
    Import,
    /// Saving remote images.
    RemoteImages,
}

/// Where a task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Still working.
    Running,
    /// Finished without errors.
    Done,
    /// Stopped by an error, or finished with some.
    Failed,
    /// Stopped on request.
    Cancelled,
}

/// How serious a task log line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLevel {
    /// Progress.
    Info,
    /// Something was skipped.
    Warning,
    /// Something failed.
    Error,
}

/// One line of a task's log. Holds counts, names and reasons, never message content or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLine {
    /// When it was written.
    pub at: Timestamp,
    /// How serious it is.
    pub level: TaskLevel,
    /// The text.
    pub text: String,
}

/// A background task with its progress and log, kept in memory until the server restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Unique while the server runs.
    pub id: String,
    /// What it does.
    pub kind: TaskKind,
    /// Its archive.
    pub archive_id: Option<ArchiveId>,
    /// Its account, for syncs and imports.
    pub account_id: Option<AccountId>,
    /// What it works on, e.g. the account or archive name.
    pub title: String,
    /// Where it is.
    pub state: TaskState,
    /// When it started.
    pub started_at: Timestamp,
    /// When it ended.
    pub finished_at: Option<Timestamp>,
    /// Steps done.
    pub done: u64,
    /// Steps in all, when known.
    pub total: Option<u64>,
    /// One line on where it stands.
    pub summary: String,
    /// Whether [`Mailboxes::cancel_task`] can stop it.
    pub cancellable: bool,
    /// The log, oldest line first; long logs keep their start and end.
    pub log: Vec<TaskLine>,
}

/// The outbound proxy for remote images.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageProxy {
    /// `scheme://host:port`, without credentials; `None` without a proxy.
    pub address: Option<String>,
    /// Set by the server's config, so it cannot be changed from the API.
    pub from_config: bool,
}

/// An IMAP account to add.
#[derive(Debug, Clone)]
pub struct NewImapAccount {
    /// Shown in the sidebar; the user name when `None`.
    pub name: Option<String>,
    /// Server host name.
    pub host: String,
    /// The default port of `security` when `None`.
    pub port: Option<u16>,
    /// `tls`, `starttls` or `plain`.
    pub security: String,
    /// User name, often the address.
    pub username: String,
    /// The password or app password. Never logged or returned.
    pub password: String,
}

/// The state of an account's sync or import.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountStatus {
    /// A sync or import is running.
    pub running: bool,
    /// Messages added by the running or last run.
    pub new_messages: u64,
    /// When the last run ended.
    pub finished_at: Option<Timestamp>,
    /// Why the last run failed, if it did. Safe to show: never holds mail or secrets.
    pub error: Option<String>,
    /// When the scheduler will sync it next; `None` for accounts it does not sync.
    pub next_sync_at: Option<Timestamp>,
}

/// How an IMAP account connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapConnection {
    /// Server host name.
    pub host: String,
    /// The port to connect to; the default of `security` when not set.
    pub port: u16,
    /// `tls`, `starttls` or `plain`.
    pub security: String,
    /// User name, often the address.
    pub username: String,
    /// Folder paths that are not archived.
    pub exclude: Vec<String>,
}

/// A new connection for an IMAP account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionChange {
    /// Server host name.
    pub host: String,
    /// The default port of `security` when `None`.
    pub port: Option<u16>,
    /// `tls`, `starttls` or `plain`.
    pub security: String,
    /// User name, often the address.
    pub username: String,
    /// Folder paths not to archive.
    pub exclude: Vec<String>,
}

/// Changes to an account; `None` keeps what is stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountChange {
    /// Where and as whom to sign in.
    pub connection: Option<ConnectionChange>,
    /// A new password; never logged or returned.
    pub password: Option<String>,
    /// The account's own sync interval in minutes, 0 for only on request; `Some(None)` follows
    /// the server's interval again.
    pub sync_interval_minutes: Option<Option<u64>>,
}

/// What [`Mailboxes::details`] knows about an account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDetails {
    /// For IMAP accounts.
    pub connection: Option<ImapConnection>,
    /// Its own sync interval in minutes; `None` follows the server's.
    pub sync_interval_minutes: Option<u64>,
}

/// The sync interval of accounts without their own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncSchedule {
    /// Minutes between syncs; 0 means only on request.
    pub minutes: u64,
    /// Set by the server's config, so it cannot be changed from the API.
    pub from_config: bool,
}

/// Why a mailbox action was refused.
#[derive(Debug, thiserror::Error)]
pub enum MailboxError {
    /// The request cannot work as given: wrong settings, rejected sign-in, an unknown file.
    #[error("{0}")]
    Invalid(String),
    /// Something failed on the server's side.
    #[error("{0}")]
    Backend(String),
}
