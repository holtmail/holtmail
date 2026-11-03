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

    /// Signs in with the account's settings and `password`, then stores the password sealed.
    /// Nothing changes when signing in fails.
    async fn set_password(&self, account: &Account, password: String) -> Result<(), MailboxError>;

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

    /// Starts saving the remote images of `archive`'s messages in the background, unless that is
    /// already running. Does nothing for an archive with saving turned off.
    fn save_remote_images(&self, archive: ArchiveId);
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
