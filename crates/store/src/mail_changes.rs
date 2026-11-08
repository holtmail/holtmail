use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, DeviceId, FolderId, MailChangeId, MessageId, OutboxId, Timestamp, UserId,
};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// Where a change to a message on its server is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailChangeState {
    /// Recorded and shown, waiting to be sent, or waiting for `next_attempt_at` after a
    /// temporary failure.
    Held,
    /// Being sent, or (with [`MailChange::sent_at`]) sent and waiting for a sync to show it.
    Sending,
    /// The server has it.
    Applied,
    /// The server shows something else after it was sent: the server wins.
    SupersededByServer,
    /// Given up: a permanent refusal, or temporary ones for too long.
    Failed,
}

impl MailChangeState {
    /// Every state, in a fixed order.
    pub const ALL: [MailChangeState; 5] = [
        Self::Held,
        Self::Sending,
        Self::Applied,
        Self::SupersededByServer,
        Self::Failed,
    ];

    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Sending => "sending",
            Self::Applied => "applied",
            Self::SupersededByServer => "superseded_by_server",
            Self::Failed => "failed",
        }
    }

    /// The state named by [`MailChangeState::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }

    /// Whether the change is still on its way: held or sending.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Held | Self::Sending)
    }
}

/// Who asked for a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailChangeOrigin {
    /// A person, in the web app, the API or a client; restoring to the mailbox included.
    User,
    /// A rule.
    Rule,
    /// The retention job deleting from the server after a period.
    Retention,
    /// The sent copy of an outbox message, by its identity's `append:<folder>` policy.
    SentCopy,
}

impl MailChangeOrigin {
    /// Every origin, in a fixed order.
    pub const ALL: [MailChangeOrigin; 4] =
        [Self::User, Self::Rule, Self::Retention, Self::SentCopy];

    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Rule => "rule",
            Self::Retention => "retention",
            Self::SentCopy => "sent_copy",
        }
    }

    /// The origin named by [`MailChangeOrigin::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// What a change does on the server. Flags are spelled as IMAP spells them (`\Seen`,
/// `$Forwarded`) and compared ignoring ASCII case; labels are readable names compared exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailOp {
    /// Adds and removes flags of the message at the change's location.
    Flags {
        /// Flags to add.
        add: Vec<String>,
        /// Flags to remove.
        remove: Vec<String>,
    },
    /// Adds and removes labels; the location is in the account's folder with the `All` role.
    Labels {
        /// Labels to add.
        add: Vec<String>,
        /// Labels to remove.
        remove: Vec<String>,
    },
    /// Moves the message to another folder of the same account.
    Move {
        /// The folder it goes to.
        target: FolderId,
    },
    /// Deletes the message; without `expunge` it is only marked deleted.
    Delete {
        /// Whether it is removed for good.
        expunge: bool,
    },
    /// Saves stored bytes as a new message in the change's folder: a sent copy or a restore.
    Append {
        /// The bytes, already in the blob store.
        blob_id: BlobId,
        /// The flags it gets, e.g. `\Seen` for a sent copy.
        flags: Vec<String>,
        /// Its received time on the server.
        date: Timestamp,
    },
}

impl MailOp {
    /// The `kind` name used in storage and the API: `flags`, `labels`, `move`, `delete` or
    /// `append`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Flags { .. } => "flags",
            Self::Labels { .. } => "labels",
            Self::Move { .. } => "move",
            Self::Delete { .. } => "delete",
            Self::Append { .. } => "append",
        }
    }

    /// Whether only a sync can tell that the server has it: flags and labels. Other changes
    /// are applied as soon as the server accepts them.
    pub fn needs_confirmation(&self) -> bool {
        matches!(self, Self::Flags { .. } | Self::Labels { .. })
    }
}

/// A change to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailChange {
    /// The account whose server it changes.
    pub account_id: AccountId,
    /// The folder of that account it works in; for an append, where the bytes go.
    pub folder_id: FolderId,
    /// The archived message it is about, when there is one.
    pub message_id: Option<MessageId>,
    /// The message's UID in the folder; `None` exactly for an append.
    pub uid: Option<u64>,
    /// What it does.
    pub op: MailOp,
    /// Who asked for it.
    pub origin: MailChangeOrigin,
    /// The person who asked, when one did.
    pub user_id: Option<UserId>,
    /// The device it came from; `None` for the web app and the server itself.
    pub device_id: Option<DeviceId>,
    /// The outbox message a sent copy belongs to.
    pub outbox_id: Option<OutboxId>,
    /// When it was asked for.
    pub at: Timestamp,
}

/// A recorded change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailChange {
    /// Id; later changes have greater ids.
    pub id: MailChangeId,
    /// What was recorded; never changes.
    pub new: NewMailChange,
    /// Where it is.
    pub state: MailChangeState,
    /// How many times it was claimed for sending.
    pub attempts: u32,
    /// When a held change may be sent again after a temporary failure.
    pub next_attempt_at: Option<Timestamp>,
    /// A short code for why the last attempt did not succeed, such as `connection` or
    /// `unsupported`; never server text, addresses or subjects.
    pub last_error: Option<String>,
    /// When the server accepted it, for a change that waits for a sync to confirm it.
    pub sent_at: Option<Timestamp>,
    /// The UID the server gave a moved or appended message, when it said.
    pub result_uid: Option<u64>,
    /// When it last changed.
    pub updated_at: Timestamp,
}

/// Which changes to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailChangeQuery {
    /// Only this account's.
    pub account: AccountId,
    /// Only these states; empty for every state.
    pub states: Vec<MailChangeState>,
    /// At most this many; the store may cap it.
    pub limit: u32,
}

/// A message the retention job may delete from its server: archived, seen at this UID in this
/// folder, not removed there, and with no open change at that location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionCandidate {
    /// The archived message.
    pub message_id: MessageId,
    /// The folder it is in.
    pub folder_id: FolderId,
    /// Its UID there.
    pub uid: u64,
}

/// Changes to messages on their servers (flags, labels, moves, deletes, appends), held until
/// a writer sends them, sent first in first out per account with at most one in flight per
/// message, and confirmed by what sync observes.
///
/// A change's recorded payload never changes; only its state and bookkeeping do. Every state
/// change is atomic: of two at once from the same state, one wins.
#[async_trait]
pub trait MailChangeStore: Send + Sync {
    /// Records a change as [`MailChangeState::Held`] and returns it. `Invalid` when the folder
    /// (or a move's target) is not the account's, a move targets its own folder, `uid` is
    /// missing (or given for an append), a flags or labels change adds and removes nothing, or
    /// the message is in another archive. `NotFound` for an unknown account, folder or
    /// message.
    async fn enqueue_mail_change(&self, change: &NewMailChange) -> Result<MailChange, StoreError>;

    /// The change, or `None`.
    async fn get_mail_change(&self, id: MailChangeId) -> Result<Option<MailChange>, StoreError>;

    /// Accounts for which [`MailChangeStore::claim_mail_change`] at `now` would find a change,
    /// ordered by id.
    async fn due_mail_accounts(&self, now: Timestamp) -> Result<Vec<AccountId>, StoreError>;

    /// Atomically takes the account's oldest due held change and moves it to
    /// [`MailChangeState::Sending`], counting an attempt. A change waits while an older change
    /// of the same message in the same account is held or sending, so a message's changes go
    /// out in the order they were made, one at a time; changes of other messages pass it.
    /// `None` when nothing can go.
    async fn claim_mail_change(
        &self,
        account: AccountId,
        now: Timestamp,
    ) -> Result<Option<MailChange>, StoreError>;

    /// Records that the server accepted a sending change that is not yet marked sent. A change
    /// that [`MailOp::needs_confirmation`] stays sending with `sent_at` until
    /// [`MailChangeStore::confirm_mail_flags`] or [`MailChangeStore::settle_mail_changes`]
    /// closes it; any other is [`MailChangeState::Applied`]. Keeps `result_uid`. `None` when the
    /// change is in another state or already sent; `NotFound` for an unknown id.
    async fn mark_mail_change_sent(
        &self,
        id: MailChangeId,
        result_uid: Option<u64>,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError>;

    /// The newest applied move of `message` from `uid` in `folder` recorded before `before`,
    /// so a later change made at that place can follow the message to where it went. `None`
    /// when there is none.
    async fn applied_move_from(
        &self,
        message: MessageId,
        folder: FolderId,
        uid: u64,
        before: MailChangeId,
    ) -> Result<Option<MailChange>, StoreError>;

    /// Points a sending change that is not yet sent at another place of its account: `uid` in
    /// `folder`, where an earlier move took its message. `Invalid` when the folder is another
    /// account's, the change is an append, or a move would target its own folder; `None` when
    /// it is in another state or already sent; `NotFound` for an unknown id or folder.
    async fn retarget_mail_change(
        &self,
        id: MailChangeId,
        folder: FolderId,
        uid: u64,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError>;

    /// Puts a sending change that is not yet sent back to held after a temporary failure,
    /// with `error` as its code and `next_attempt_at` as when it may go again. `None` when it
    /// is in another state or already sent; `NotFound` for an unknown id.
    async fn retry_mail_change(
        &self,
        id: MailChangeId,
        next_attempt_at: Timestamp,
        error: &str,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError>;

    /// Gives up a held or sending change with `error` as its code. `None` when it is in
    /// another state; `NotFound` for an unknown id.
    async fn fail_mail_change(
        &self,
        id: MailChangeId,
        error: &str,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError>;

    /// Tells the open flags changes (and, when `labels` is given, labels changes) of the
    /// message at `uid` in `folder` what sync read there at `read_at`. A change the state
    /// already satisfies (every added name present, every removed one absent) is applied,
    /// held ones too; one being sent right now (sending, not yet sent) is left alone. A sent
    /// change sent before `read_at` that the state contradicts is
    /// [`MailChangeState::SupersededByServer`]: the server wins. Changes are judged oldest
    /// first, and the first one that stays open keeps every later one open too, so a newer
    /// change is never closed ahead of an older one it may undo. Returns the changes it
    /// closed, oldest first.
    async fn confirm_mail_flags(
        &self,
        folder: FolderId,
        uid: u64,
        flags: &[String],
        labels: Option<&[String]>,
        read_at: Timestamp,
    ) -> Result<Vec<MailChange>, StoreError>;

    /// Applies the account's sent changes sent before `sent_before` that no sync confirmed,
    /// trusting the server's answer; returns how many.
    async fn settle_mail_changes(
        &self,
        account: AccountId,
        sent_before: Timestamp,
        at: Timestamp,
    ) -> Result<u64, StoreError>;

    /// Puts every sending change that is not yet sent back to held, for a start after the
    /// process stopped mid-send, and returns them oldest first. Flags, labels, moves and
    /// deletes are safe to send again; an append may then be saved twice.
    async fn release_mail_changes(&self, at: Timestamp) -> Result<Vec<MailChange>, StoreError>;

    /// The open changes of the message in every account, oldest first, for the held mark.
    async fn pending_mail_changes(&self, message: MessageId)
    -> Result<Vec<MailChange>, StoreError>;

    /// The message's failed changes in every account that nobody dismissed, oldest first, so
    /// the reader can offer to retry or dismiss them.
    async fn failed_mail_changes(&self, message: MessageId) -> Result<Vec<MailChange>, StoreError>;

    /// Hides a failed change from [`MailChangeStore::failed_mail_changes`]; its row and state
    /// stay. `None` when it is not failed or was dismissed before; `NotFound` for an unknown id.
    async fn dismiss_mail_change(
        &self,
        id: MailChangeId,
        at: Timestamp,
    ) -> Result<Option<MailChange>, StoreError>;

    /// The changes recorded for this outbox message (its sent copy), oldest first.
    async fn outbox_mail_changes(&self, outbox: OutboxId) -> Result<Vec<MailChange>, StoreError>;

    /// Locations in the account's folders that the retention job may delete from the server,
    /// oldest message first, at most `limit`: a UID, not removed, no held or sending change at
    /// that location, no retention change ever recorded there (whatever became of it, so a
    /// refused or failed one is not recorded again every pass), and the message received (or, without a received time, dated) before
    /// `before`. With `skip_trash`, folders with the trash role are left out.
    async fn retention_candidates(
        &self,
        account: AccountId,
        before: Timestamp,
        skip_trash: bool,
        limit: u32,
    ) -> Result<Vec<RetentionCandidate>, StoreError>;

    /// The account's changes matching the query, newest first.
    async fn list_mail_changes(
        &self,
        query: &MailChangeQuery,
    ) -> Result<Vec<MailChange>, StoreError>;
}
