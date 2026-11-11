use async_trait::async_trait;
use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, ArchiveId, DeviceId, DraftId, IdentityId, MessageId, OutboxId, Timestamp,
    TransportId, UserId,
};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// How a transport hands bytes to a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// SMTP submission with its own host, port and credentials.
    Smtp,
    /// JMAP submission (RFC 8621) with its own session address and credentials.
    Jmap,
    /// Microsoft Graph `sendMail` with its own bearer token.
    Graph,
    /// The connection and credentials of an account in the same archive.
    Account,
}

impl TransportKind {
    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smtp => "smtp",
            Self::Jmap => "jmap",
            Self::Graph => "graph",
            Self::Account => "account",
        }
    }

    /// The kind named by [`TransportKind::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "smtp" => Self::Smtp,
            "jmap" => Self::Jmap,
            "graph" => Self::Graph,
            "account" => Self::Account,
            _ => return None,
        })
    }
}

/// What a transport may carry. `None` is no limit of Holtmail's own; the server may still have
/// one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportLimits {
    /// The largest message in bytes.
    pub max_size: Option<u64>,
    /// The most envelope recipients of one message.
    pub max_recipients: Option<u32>,
    /// The most messages handed over in any hour.
    pub per_hour: Option<u32>,
}

/// A way out for mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transport {
    /// Id, chosen by the caller so the secret can be sealed with context `transport:<id>`
    /// before storing.
    pub id: TransportId,
    /// The archive it belongs to; `None` for an instance transport, which archives use only
    /// when it is granted to them.
    pub archive_id: Option<ArchiveId>,
    /// A name for people.
    pub name: String,
    /// How it sends.
    pub kind: TransportKind,
    /// For [`TransportKind::Account`], the account whose connection it uses. Cleared when the
    /// account is removed, which leaves the transport unusable.
    pub account_id: Option<AccountId>,
    /// Settings that are not secret (host, port, security, user name), as the transport's
    /// implementation reads them.
    pub settings: serde_json::Value,
    /// The password or token, sealed by the caller.
    pub sealed_secret: Option<Vec<u8>>,
    /// Address patterns it may send as, such as `*@example.org`. Empty allows nothing.
    pub allowed_from: Vec<String>,
    /// What it may carry.
    pub limits: TransportLimits,
    /// Off stops new sends through it; messages waiting for it stay queued.
    pub enabled: bool,
    /// Whether the last check found that it takes internationalized addresses. False until a
    /// check says so. Kept apart from what people edit: [`SendStore::update_transport`] leaves
    /// it alone, only [`SendStore::set_transport_smtputf8`] changes it.
    pub smtputf8: bool,
    /// When it was added.
    pub created_at: Timestamp,
}

/// What a user's devices submitted over a period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceSends {
    /// Messages submitted.
    pub messages: u64,
    /// Envelope recipients of those messages together.
    pub recipients: u64,
    /// When the oldest of them was submitted; `None` when there are none.
    pub oldest: Option<Timestamp>,
}

/// What happens to the copy a sent message should leave in the account's Sent folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "folder", rename_all = "snake_case")]
pub enum SentCopyPolicy {
    /// The provider keeps a copy itself (Gmail SMTP, Graph, JMAP).
    Provider,
    /// Holtmail appends the bytes to this folder of the account.
    Append(String),
    /// No copy upstream; the archive has it.
    None,
}

impl SentCopyPolicy {
    /// The stored form: `provider`, `append:<folder>` or `none`.
    pub fn to_stored(&self) -> String {
        match self {
            Self::Provider => "provider".into(),
            Self::Append(folder) => format!("append:{folder}"),
            Self::None => "none".into(),
        }
    }

    /// The policy written by [`SentCopyPolicy::to_stored`], or `None`.
    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "provider" => Some(Self::Provider),
            "none" => Some(Self::None),
            other => other
                .strip_prefix("append:")
                .filter(|folder| !folder.is_empty())
                .map(|folder| Self::Append(folder.into())),
        }
    }
}

/// An identity to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIdentity {
    /// The archive it belongs to.
    pub archive_id: ArchiveId,
    /// The `From` address.
    pub address: String,
    /// The display name.
    pub name: Option<String>,
    /// Signature as HTML.
    pub signature_html: Option<String>,
    /// Signature as plain text.
    pub signature_text: Option<String>,
    /// The `Reply-To` address.
    pub reply_to: Option<String>,
    /// The transport that sends for it: the archive's own or an instance transport granted to
    /// the archive.
    pub transport_id: TransportId,
    /// What happens to the sent copy upstream.
    pub sent_copy: SentCopyPolicy,
    /// Every message also goes to the identity's own address, out of the bytes.
    pub bcc_self: bool,
    /// The account it answers for, in the same archive.
    pub account_id: Option<AccountId>,
    /// The alias address it answers for.
    pub alias: Option<String>,
    /// Replies to a plus address keep it as `From`.
    pub plus_addresses: bool,
    /// Inactive identities cannot send.
    pub active: bool,
    /// Who made it; that user may send as it from the start.
    pub created_by: Option<UserId>,
}

/// The provider put another `From` on a message sent as an identity than the identity's
/// address, as the synced copy of the sent message showed (Gmail does this for an address that
/// is not set up as "Send mail as").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewrittenFrom {
    /// The `From` address the synced copy carried.
    pub seen: String,
    /// When it was seen.
    pub at: Timestamp,
}

/// An address people may send as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Id.
    pub id: IdentityId,
    /// Every field it was made with, as last updated.
    pub fields: NewIdentity,
    /// Set by [`SendStore::set_identity_rewritten_from`]; cleared when the address changes.
    pub rewritten_from: Option<RewrittenFrom>,
    /// When it was added.
    pub created_at: Timestamp,
}

/// One saved state of a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftVersion {
    /// From 1, growing by one with every save.
    pub version: u32,
    /// What the compose window holds, as the API defines it; `null` for a raw version.
    pub payload: serde_json::Value,
    /// For a raw version, the client's message bytes as they came (a draft saved over IMAP or
    /// JMAP); `None` for a compose version. The blob is also among `blobs`.
    pub raw: Option<BlobId>,
    /// Blobs the draft uses (attachments); kept from deletion while the version is kept.
    pub blobs: Vec<BlobId>,
    /// When it was saved.
    pub saved_at: Timestamp,
}

/// A message being written: its newest version and who writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// Id.
    pub id: DraftId,
    /// The archive it is written in.
    pub archive_id: ArchiveId,
    /// Who writes it.
    pub user_id: UserId,
    /// The newest version.
    pub current: DraftVersion,
    /// When it was started.
    pub created_at: Timestamp,
}

/// What [`SendStore::save_draft`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftSave {
    /// The new version was stored.
    Saved(Draft),
    /// Another save came first; nothing was stored. This is the draft as it is now.
    Conflict(Draft),
}

/// How many versions of a draft are kept: the newest ones.
pub const DRAFT_VERSIONS_KEPT: u32 = 50;

/// Where an outbox message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    /// Held: not sent until released (undo, or a rule that needs confirmation).
    Draft,
    /// Waiting for `send_at`.
    Queued,
    /// Being handed to the transport.
    Sending,
    /// A temporary failure; waiting for `next_attempt_at`.
    Retrying,
    /// The transport took it for at least one recipient.
    Sent,
    /// Given up: a permanent refusal, or temporary ones for too long.
    Failed,
    /// The connection was lost after the whole message went out; it may have been delivered.
    Uncertain,
    /// A delivery status notification reported it undeliverable after it was sent.
    Bounced,
    /// Cancelled before it was sent.
    Cancelled,
}

impl OutboxState {
    /// Every state, in a fixed order.
    pub const ALL: [OutboxState; 9] = [
        Self::Draft,
        Self::Queued,
        Self::Sending,
        Self::Retrying,
        Self::Sent,
        Self::Failed,
        Self::Uncertain,
        Self::Bounced,
        Self::Cancelled,
    ];

    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Retrying => "retrying",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
            Self::Bounced => "bounced",
            Self::Cancelled => "cancelled",
        }
    }

    /// The state named by [`OutboxState::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// What became of one envelope recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecipientState {
    /// Not handed over yet, or the outcome is unknown.
    Pending,
    /// The server took the message for it.
    Accepted,
    /// The server refused it.
    Refused {
        /// A permanent (`5xx`) refusal.
        permanent: bool,
        /// The SMTP code or its closest equivalent.
        code: u16,
        /// The server's words. They may name addresses: never log them.
        text: String,
    },
    /// The server took it, then a delivery status notification said it could not be
    /// delivered.
    Bounced {
        /// The enhanced status code (RFC 3463), such as `5.1.1`, when the report gave one.
        status: Option<String>,
        /// The remote server's words. They may name addresses: never log them.
        text: Option<String>,
    },
}

/// One envelope recipient of an outbox message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRecipient {
    /// The address as in the envelope.
    pub address: String,
    /// What became of it.
    #[serde(flatten)]
    pub state: RecipientState,
}

/// A message to put in the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxEntry {
    /// The archive it is sent from.
    pub archive_id: ArchiveId,
    /// The identity it is sent as.
    pub identity_id: IdentityId,
    /// The transport that carries it.
    pub transport_id: TransportId,
    /// Who queued it; `None` for the server itself.
    pub user_id: Option<UserId>,
    /// The device it was submitted from; `None` for the web app and the server itself.
    pub device_id: Option<DeviceId>,
    /// Why the route looks the way it does, as the router explained it.
    pub route: serde_json::Value,
    /// The return path; empty for `<>`.
    pub mail_from: String,
    /// Every envelope recipient, Bcc included, in order. All start [`RecipientState::Pending`].
    pub recipients: Vec<String>,
    /// The exact bytes to hand over, already stored.
    pub blob_id: BlobId,
    /// Their size in bytes.
    pub size: u64,
    /// The `Message-ID` in the bytes, without angle brackets.
    pub rfc_message_id: String,
    /// When it may be sent, at the earliest.
    pub send_at: Timestamp,
    /// [`OutboxState::Queued`] or [`OutboxState::Draft`]; any other is `Invalid`.
    pub state: OutboxState,
    /// The rule run that queued it.
    pub flow_run: Option<String>,
    /// The caller's key: a second entry with the same user and key is not added.
    pub idempotency_key: Option<String>,
    /// The outbox message this one sends again.
    pub resend_of: Option<OutboxId>,
    /// When it was queued.
    pub at: Timestamp,
}

/// A message in the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    /// Id.
    pub id: OutboxId,
    /// What it was queued with: `recipients` are the envelope order and `state` the state it
    /// started in.
    pub new: NewOutboxEntry,
    /// Where it is.
    pub state: OutboxState,
    /// Each envelope recipient with what became of it, in envelope order.
    pub recipients: Vec<OutboxRecipient>,
    /// The archived message with these bytes, once archived.
    pub message_id: Option<MessageId>,
    /// How many times it was handed to the transport.
    pub attempts: u32,
    /// When the last attempt started.
    pub last_attempt_at: Option<Timestamp>,
    /// When the next attempt is due, while [`OutboxState::Retrying`].
    pub next_attempt_at: Option<Timestamp>,
    /// Why the last attempt did not succeed, in words that may hold server text: never log it.
    pub last_error: Option<String>,
    /// When the transport took it.
    pub sent_at: Option<Timestamp>,
    /// The provider's own copy of it that sync archived with other bytes than the sent ones,
    /// such as a Sent copy the server rewrote. The two are copies of one message.
    pub synced_copy: Option<MessageId>,
    /// Something learned after sending that is not a failure, such as a delayed delivery. May
    /// name addresses: never log it.
    pub note: Option<String>,
    /// When it last changed.
    pub updated_at: Timestamp,
}

/// A change to an outbox message, applied only when it is in one of the expected states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxChange {
    /// The new state.
    pub state: OutboxState,
    /// When the change happens.
    pub at: Timestamp,
    /// Replaces `next_attempt_at`; `None` clears it.
    pub next_attempt_at: Option<Timestamp>,
    /// Replaces `send_at` when given.
    pub send_at: Option<Timestamp>,
    /// Replaces the recipients' states when given; the addresses must be the envelope's, in
    /// order, or the change is `Invalid`.
    pub recipients: Option<Vec<OutboxRecipient>>,
    /// Replaces `last_error`; `None` clears it.
    pub error: Option<String>,
    /// Sets `sent_at` when given.
    pub sent_at: Option<Timestamp>,
    /// Sets `message_id` when given.
    pub message_id: Option<MessageId>,
}

impl OutboxChange {
    /// A change to `state` at `at` that clears the error and the next attempt and touches
    /// nothing else.
    pub fn to(state: OutboxState, at: Timestamp) -> Self {
        Self {
            state,
            at,
            next_attempt_at: None,
            send_at: None,
            recipients: None,
            error: None,
            sent_at: None,
            message_id: None,
        }
    }
}

/// Which outbox messages to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxQuery {
    /// Only this archive's.
    pub archive: ArchiveId,
    /// Only these states; empty for every state.
    pub states: Vec<OutboxState>,
    /// Only ids smaller than this, for paging back.
    pub before: Option<OutboxId>,
    /// Only messages this user queued; every message when `None`. Applied before `limit`.
    pub user: Option<UserId>,
    /// At most this many; the store may cap it.
    pub limit: u32,
}

/// Transports, identities with the users who may send as them, drafts with their versions, and
/// the outbox.
///
/// Starting, saving and deleting a draft writes a `draft` change log row of the draft's user;
/// adding an outbox message, and every change of one (state, claim, linked copy, note), writes
/// a `submission` row, each in the same transaction and only when something changed.
#[async_trait]
pub trait SendStore: Send + Sync {
    /// Adds a transport. `Invalid` when the id is taken, when an account transport names no
    /// account or an account of another archive, or when an instance transport is an account
    /// transport. `NotFound` for an unknown archive or account.
    async fn create_transport(&self, transport: &Transport) -> Result<(), StoreError>;

    /// The transport, or `None`.
    async fn get_transport(&self, id: TransportId) -> Result<Option<Transport>, StoreError>;

    /// The transports an archive may use: its own and the instance transports granted to it,
    /// oldest first.
    async fn archive_transports(&self, archive: ArchiveId) -> Result<Vec<Transport>, StoreError>;

    /// Every instance transport, oldest first.
    async fn instance_transports(&self) -> Result<Vec<Transport>, StoreError>;

    /// Replaces every field except `id`, `archive_id`, `kind` and `created_at`, with the checks
    /// of [`SendStore::create_transport`]. `NotFound` for an unknown id.
    async fn update_transport(&self, transport: &Transport) -> Result<(), StoreError>;

    /// Records whether the last check found that the transport takes internationalized
    /// addresses. `NotFound` for an unknown id.
    async fn set_transport_smtputf8(
        &self,
        id: TransportId,
        smtputf8: bool,
    ) -> Result<(), StoreError>;

    /// Removes a transport with its grants. `Invalid` while an identity uses it; outbox messages
    /// that name it stay. `NotFound` for an unknown id.
    async fn delete_transport(&self, id: TransportId) -> Result<(), StoreError>;

    /// The archives an instance transport is granted to, ordered by id. Empty for an archive
    /// transport or an unknown id.
    async fn transport_grants(&self, id: TransportId) -> Result<Vec<ArchiveId>, StoreError>;

    /// Replaces the archives an instance transport is granted to. `Invalid` for an archive
    /// transport, or when a removed archive still has an identity using the transport.
    /// `NotFound` for an unknown transport or archive.
    async fn set_transport_grants(
        &self,
        id: TransportId,
        archives: &[ArchiveId],
    ) -> Result<(), StoreError>;

    /// Adds an identity; its creator may send as it. `Invalid` when the archive may not use the
    /// transport or the account is in another archive. `NotFound` for an unknown archive,
    /// transport or account.
    async fn create_identity(&self, identity: NewIdentity) -> Result<Identity, StoreError>;

    /// The identity, or `None`.
    async fn get_identity(&self, id: IdentityId) -> Result<Option<Identity>, StoreError>;

    /// The archive's identities, oldest first.
    async fn list_identities(&self, archive: ArchiveId) -> Result<Vec<Identity>, StoreError>;

    /// Replaces every field except `id`, `archive_id`, `created_by` and `created_at`, with the
    /// checks of [`SendStore::create_identity`]. `NotFound` for an unknown id.
    async fn update_identity(&self, identity: &Identity) -> Result<(), StoreError>;

    /// Removes an identity with its users and defaults. Outbox messages that name it stay.
    /// `NotFound` for an unknown id.
    async fn delete_identity(&self, id: IdentityId) -> Result<(), StoreError>;

    /// The users who may send as the identity, ordered by id.
    async fn identity_users(&self, id: IdentityId) -> Result<Vec<UserId>, StoreError>;

    /// Replaces the users who may send as the identity. `NotFound` for an unknown identity or
    /// user.
    async fn set_identity_users(&self, id: IdentityId, users: &[UserId]) -> Result<(), StoreError>;

    /// The identities of the archive the user may send as, oldest first.
    async fn user_identities(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Vec<Identity>, StoreError>;

    /// The user's default identity in the archive, or `None`.
    async fn default_identity(
        &self,
        user: UserId,
        archive: ArchiveId,
    ) -> Result<Option<IdentityId>, StoreError>;

    /// Sets or clears the user's default identity in the archive. `Invalid` when the identity is
    /// in another archive. `NotFound` for an unknown user, archive or identity.
    async fn set_default_identity(
        &self,
        user: UserId,
        archive: ArchiveId,
        identity: Option<IdentityId>,
    ) -> Result<(), StoreError>;

    /// Starts a draft at version 1. `NotFound` for an unknown archive or user.
    async fn create_draft(
        &self,
        archive: ArchiveId,
        user: UserId,
        payload: &serde_json::Value,
        blobs: &[BlobId],
        at: Timestamp,
    ) -> Result<Draft, StoreError>;

    /// Starts a raw draft at version 1: `raw` holds the client's bytes, already in the blob
    /// store, and the payload is `null`. A later [`SendStore::save_draft`] stores compose
    /// versions on top. `NotFound` for an unknown archive or user.
    async fn create_raw_draft(
        &self,
        archive: ArchiveId,
        user: UserId,
        raw: &BlobId,
        at: Timestamp,
    ) -> Result<Draft, StoreError>;

    /// The draft at its newest version, or `None`.
    async fn get_draft(&self, id: DraftId) -> Result<Option<Draft>, StoreError>;

    /// Stores a new version if the draft is still at version `expected`, otherwise
    /// [`DraftSave::Conflict`]. Keeps the newest [`DRAFT_VERSIONS_KEPT`] versions. `NotFound`
    /// for an unknown draft.
    async fn save_draft(
        &self,
        id: DraftId,
        expected: u32,
        payload: &serde_json::Value,
        blobs: &[BlobId],
        at: Timestamp,
    ) -> Result<DraftSave, StoreError>;

    /// The kept versions of the draft, newest first; empty for an unknown draft.
    async fn draft_versions(&self, id: DraftId) -> Result<Vec<DraftVersion>, StoreError>;

    /// The user's drafts in the archive, most recently saved first.
    async fn list_drafts(&self, archive: ArchiveId, user: UserId)
    -> Result<Vec<Draft>, StoreError>;

    /// Removes the draft with every version. Idempotent.
    async fn delete_draft(&self, id: DraftId) -> Result<(), StoreError>;

    /// Puts a message in the outbox, or finds the one with the same user and idempotency key;
    /// returns it and whether it was added. `Invalid` for a state other than queued or draft or
    /// for no recipients. `NotFound` for an unknown archive.
    async fn enqueue_outbox(
        &self,
        entry: &NewOutboxEntry,
    ) -> Result<(OutboxEntry, bool), StoreError>;

    /// The message with this user and idempotency key, whatever became of it, or `None`.
    async fn outbox_by_idempotency_key(
        &self,
        user: UserId,
        key: &str,
    ) -> Result<Option<OutboxEntry>, StoreError>;

    /// What the user's devices submitted at or after `since`, across every archive. Messages
    /// the user took back ([`OutboxState::Cancelled`]) do not count; messages from the web app
    /// never do.
    async fn device_sends_since(
        &self,
        user: UserId,
        since: Timestamp,
    ) -> Result<DeviceSends, StoreError>;

    /// The outbox message, or `None`.
    async fn get_outbox(&self, id: OutboxId) -> Result<Option<OutboxEntry>, StoreError>;

    /// Outbox messages matching the query, newest first.
    async fn list_outbox(&self, query: &OutboxQuery) -> Result<Vec<OutboxEntry>, StoreError>;

    /// Applies `change` if the message is in one of the states in `from`, atomically, and
    /// returns it changed; `None` when it is in another state. Of two changes at once from the
    /// same state, one wins. `NotFound` for an unknown id.
    async fn change_outbox(
        &self,
        id: OutboxId,
        from: &[OutboxState],
        change: &OutboxChange,
    ) -> Result<Option<OutboxEntry>, StoreError>;

    /// Transports with a message due at `now`: queued with `send_at` at or before it, or
    /// retrying with `next_attempt_at` at or before it. Ordered by id.
    async fn due_transports(&self, now: Timestamp) -> Result<Vec<TransportId>, StoreError>;

    /// Atomically takes the transport's earliest due message (by when it became due, then id)
    /// and moves it to [`OutboxState::Sending`], counting an attempt at `now`. `None` when
    /// nothing is due or one of the transport's messages is already sending, so at most one
    /// message per transport is ever sending.
    async fn claim_outbox(
        &self,
        transport: TransportId,
        now: Timestamp,
    ) -> Result<Option<OutboxEntry>, StoreError>;

    /// How many of the transport's messages had an attempt start at or after `since`.
    async fn outbox_attempts_since(
        &self,
        transport: TransportId,
        since: Timestamp,
    ) -> Result<u64, StoreError>;

    /// Moves every sending message to [`OutboxState::Uncertain`] with `error`, for a start
    /// after the process stopped mid-send. Returns them.
    async fn interrupt_sending(
        &self,
        at: Timestamp,
        error: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError>;

    /// The archive's outbox messages whose `Message-ID` is `rfc_message_id` (without angle
    /// brackets, compared exactly), oldest first.
    async fn outbox_by_rfc_message_id(
        &self,
        archive: ArchiveId,
        rfc_message_id: &str,
    ) -> Result<Vec<OutboxEntry>, StoreError>;

    /// The outbox messages whose sent bytes are `message` or whose
    /// [`OutboxEntry::synced_copy`] is `message`, oldest first.
    async fn outbox_for_message(&self, message: MessageId) -> Result<Vec<OutboxEntry>, StoreError>;

    /// Records `message` as the outbox message's [`OutboxEntry::synced_copy`], replacing an
    /// earlier one, and returns the message changed. Leaves the state alone. `Invalid` when
    /// `message` is in another archive. `NotFound` for an unknown outbox message or message.
    async fn link_outbox_copy(
        &self,
        id: OutboxId,
        message: MessageId,
        at: Timestamp,
    ) -> Result<OutboxEntry, StoreError>;

    /// Replaces the outbox message's [`OutboxEntry::note`] (`None` clears it) and returns the
    /// message changed. Leaves the state alone. `NotFound` for an unknown id.
    async fn set_outbox_note(
        &self,
        id: OutboxId,
        note: Option<&str>,
        at: Timestamp,
    ) -> Result<OutboxEntry, StoreError>;

    /// Records (or with `None` clears) that the provider rewrote the identity's `From`.
    /// Idempotent. `NotFound` for an unknown identity.
    async fn set_identity_rewritten_from(
        &self,
        id: IdentityId,
        rewritten: Option<&RewrittenFrom>,
    ) -> Result<(), StoreError>;
}
