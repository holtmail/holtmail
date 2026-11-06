use std::path::{Path, PathBuf};

use async_trait::async_trait;
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_store::{Account, AccountKind};
pub use holtmail_store::{TaskKind, TaskLevel, TaskLine, TaskState};

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

    /// Signs in to the JMAP server and lists its mailboxes, then stores the account in `archive`
    /// with its password or token sealed and starts its first sync in the background.
    ///
    /// Nothing is stored when signing in fails.
    async fn add_jmap(
        &self,
        archive: ArchiveId,
        account: NewJmapAccount,
    ) -> Result<Account, MailboxError>;

    /// Changes how `account` connects and how often it is synced, as `change` says. A new
    /// connection or password is checked by signing in first, and nothing is stored when that
    /// fails. Returns the stored account. `Invalid` for an import account, for a change while the
    /// account syncs, or for an interval or minimum age out of range. A stored password or token
    /// is never sent to another server: a connection to another server without a new
    /// `password` is `Invalid`, and so is any change of server, port or security on an account
    /// that signs in with OAuth.
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

    /// Starts a Gmail or Microsoft sign-in for a new account or for an existing one: returns
    /// the provider's sign-in page and a `state` that stays valid for a few minutes and can be
    /// used once. `Invalid` for an unknown provider, archive or account, or an import account.
    async fn start_oauth(&self, start: OAuthStart) -> Result<OAuthStarted, MailboxError>;

    /// Finishes a sign-in from the address the provider sent the browser to, or just its query:
    /// exchanges the code, checks that the mail server accepts the token, stores the refresh
    /// token sealed and starts a sync. For a new account it is created; for an existing one its
    /// sign-in is replaced and it syncs again on schedule. Nothing is stored when any step
    /// fails. `Invalid` for an unknown, used or expired `state`, a refusal at the sign-in page,
    /// or a token the server refuses.
    async fn finish_oauth(&self, answer: &str) -> Result<OAuthFinished, MailboxError>;

    /// The page the sign-in answered by `answer` goes back to, while it is waiting; nothing is
    /// used up.
    fn oauth_return_to(&self, answer: &str) -> Option<String>;

    /// The providers this server signs in with, Google first; one turned off in the config is
    /// left out.
    fn oauth_providers(&self) -> Vec<OAuthProvider>;

    /// The outbound proxy remote images are fetched through.
    fn image_proxy(&self) -> ImageProxy;

    /// Sets or, with `None`, removes the outbound proxy for remote images, after checking that it
    /// can be reached. It applies at once to the reader's proxy and to saving. `Invalid` for a bad
    /// or unreachable proxy, or when the server's config sets it.
    async fn set_image_proxy(&self, proxy: Option<String>) -> Result<ImageProxy, MailboxError>;

    /// Rebuilds the people of `archive` and returns once a rebuild that started after this call
    /// has finished, so it sees every change stored before the call. Rebuilds of one archive never
    /// overlap, whoever asked for them. `Backend` when that rebuild failed.
    async fn rebuild_people(&self, archive: ArchiveId) -> Result<(), MailboxError>;
}

/// Remote image work started on request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteImageJob {
    /// Save the images of messages archived before saving was turned on.
    Older,
    /// Fetch again the images that could not be saved.
    Failed,
}

/// A background task with its progress and log. Finished tasks stay listed after a restart, and
/// tasks a restart stopped show as [`TaskState::Interrupted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Unique among tasks, also across restarts.
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

/// A JMAP account to add.
#[derive(Debug, Clone)]
pub struct NewJmapAccount {
    /// Shown in the sidebar; the user name, or the server's host for a token, when `None`.
    pub name: Option<String>,
    /// The session address, or the server's address where `/.well-known/jmap` answers.
    pub url: String,
    /// User name for `password`; may be empty for `token`.
    pub username: String,
    /// `password` (HTTP Basic) or `token` (a bearer token, such as a Fastmail API token).
    pub auth: String,
    /// The password, app password or API token. Never logged or returned.
    pub secret: String,
}

/// What an OAuth sign-in is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthTarget {
    /// A new account in this archive, named `name` or after the address.
    NewAccount {
        /// The archive the mail goes to.
        archive: ArchiveId,
        /// Shown in the sidebar.
        name: Option<String>,
    },
    /// Sign in again to this account, or move it from a password to OAuth.
    Account(AccountId),
}

/// A provider this server signs in with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthProvider {
    /// `google` or `microsoft`.
    pub provider: String,
    /// The client is Thunderbird's, so the consent screen names Thunderbird.
    pub thunderbird_client: bool,
}

/// A sign-in to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthStart {
    /// `google` or `microsoft`.
    pub provider: String,
    /// The address to sign in as; suggested to the provider.
    pub email: String,
    /// What it is for.
    pub target: OAuthTarget,
    /// The kind of a new account: [`AccountKind::Imap`] or, for Microsoft,
    /// [`AccountKind::Graph`]. Signing in again keeps the account's own kind.
    pub kind: AccountKind,
    /// A path in the web app the callback sends the browser back to.
    pub return_to: String,
}

/// A started sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthStarted {
    /// The provider's sign-in page.
    pub authorize_url: String,
    /// Ties the answer to this sign-in.
    pub state: String,
    /// The provider sends the browser to an address that does not load, so its address has to
    /// be pasted back; otherwise the callback finishes on its own.
    pub needs_paste: bool,
    /// The client is Thunderbird's, so the consent screen names Thunderbird.
    pub thunderbird_client: bool,
    /// When the sign-in stops being accepted.
    pub expires_at: Timestamp,
}

/// A finished sign-in.
#[derive(Debug, Clone)]
pub struct OAuthFinished {
    /// The new or updated account.
    pub account: Account,
    /// The page to go back to.
    pub return_to: String,
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
    /// The latest contacts and calendars sync; `None` when there was none.
    pub contacts_calendars: Option<ContactsCalendarsRun>,
}

/// The latest sync of an account's contacts and calendars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactsCalendarsRun {
    /// It is running now.
    pub running: bool,
    /// When it ended.
    pub finished_at: Option<Timestamp>,
    /// One line on what it did, with counts only.
    pub summary: String,
    /// Why it failed, if it did. Safe to show: never holds names, contents or secrets.
    pub error: Option<String>,
}

/// Whether and where an account's contacts and calendars are archived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactsCalendars {
    /// They are archived after each mail sync.
    pub enabled: bool,
    /// Where address books are read; `None` when not set (detected when turned on).
    pub contacts_url: Option<String>,
    /// Where calendars are read.
    pub calendars_url: Option<String>,
    /// The account signs in with Google or Microsoft Graph and has not granted contacts and
    /// calendars yet: it has to sign in again before they are archived.
    pub needs_consent: bool,
    /// How they are read.
    pub protocol: ContactsProtocol,
}

/// How an account's contacts and calendars are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContactsProtocol {
    /// CardDAV and CalDAV.
    #[default]
    Dav,
    /// Microsoft Graph.
    Graph,
    /// JMAP for Contacts and JMAP Calendars.
    Jmap,
}

impl ContactsProtocol {
    /// `dav`, `graph` or `jmap`, as the API names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dav => "dav",
            Self::Graph => "graph",
            Self::Jmap => "jmap",
        }
    }
}

/// Turns archiving an account's contacts and calendars on or off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactsCalendarsChange {
    /// On or off. Turning off keeps everything archived.
    pub enabled: bool,
    /// Where address books are read: a server, a DAV root, a principal or one address book.
    /// When neither address is given, they are detected from the account's address (known
    /// providers, then the mail domain's well-known address); when one is given, it serves both.
    pub contacts_url: Option<String>,
    /// Where calendars are read.
    pub calendars_url: Option<String>,
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

/// How a JMAP account connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapConnection {
    /// The session address.
    pub url: String,
    /// User name; empty for a token without one.
    pub username: String,
    /// `password` or `token`.
    pub auth: String,
    /// Mailbox paths that are not archived.
    pub exclude: Vec<String>,
}

/// A new connection for a JMAP account. Moving between `password` and `token` needs a new
/// secret in [`AccountChange::password`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapConnectionChange {
    /// The session address, or the server's address.
    pub url: String,
    /// User name; may be empty for `token`.
    pub username: String,
    /// `password` or `token`.
    pub auth: String,
    /// Mailbox paths not to archive.
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
    /// Where and how a JMAP account signs in.
    pub jmap: Option<JmapConnectionChange>,
    /// A new password, or a new token for a JMAP account that signs in with one; never logged
    /// or returned.
    pub password: Option<String>,
    /// The account's own sync interval in minutes, 0 for only on request; `Some(None)` follows
    /// the server's interval again.
    pub sync_interval_minutes: Option<Option<u64>>,
    /// Archive only messages at least this many days old; `Some(None)` archives every message
    /// again.
    pub min_age_days: Option<Option<u32>>,
    /// Turn archiving contacts and calendars on or off. Turning it on signs in to the
    /// contacts and calendars server first, except for Google accounts that still have to
    /// grant access by signing in again; nothing is stored when signing in fails.
    pub contacts_calendars: Option<ContactsCalendarsChange>,
}

/// What [`Mailboxes::details`] knows about an account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDetails {
    /// For IMAP accounts.
    pub connection: Option<ImapConnection>,
    /// For JMAP accounts.
    pub jmap: Option<JmapConnection>,
    /// Its own sync interval in minutes; `None` follows the server's.
    pub sync_interval_minutes: Option<u64>,
    /// Messages younger than this many days are left for a later sync; `None` archives every
    /// message.
    pub min_age_days: Option<u32>,
    /// `google` or `microsoft` for accounts that sign in with OAuth.
    pub oauth_provider: Option<String>,
    /// The provider no longer accepts the sign-in; syncs stop until someone signs in again.
    pub needs_sign_in: bool,
    /// Contacts and calendars; `None` for accounts that cannot have them yet (import accounts,
    /// and Microsoft accounts until Graph contacts and calendars arrive).
    pub contacts_calendars: Option<ContactsCalendars>,
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
