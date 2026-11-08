use std::collections::BTreeSet;

use async_trait::async_trait;
use holtmail_core::{AccountId, ArchiveId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// One thing an account may let Holtmail do on its server. Every capability is off until an
/// archive manager turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Changing seen, flagged, answered, keywords and Gmail labels.
    #[serde(rename = "mail.flags")]
    MailFlags,
    /// Moving messages between folders.
    #[serde(rename = "mail.move")]
    MailMove,
    /// Moving messages to Trash.
    #[serde(rename = "mail.delete")]
    MailDelete,
    /// Expunging messages; only together with [`Capability::MailDelete`].
    #[serde(rename = "mail.expunge")]
    MailExpunge,
    /// Saving bytes into a folder: sent copies, drafts, restoring to the mailbox.
    #[serde(rename = "mail.append")]
    MailAppend,
    /// Using the account's own transport to send.
    #[serde(rename = "mail.send")]
    MailSend,
    /// Creating, changing and deleting contacts and events upstream.
    #[serde(rename = "pim.edit")]
    PimEdit,
    /// Answering invitations upstream.
    #[serde(rename = "pim.respond")]
    PimRespond,
}

impl Capability {
    /// Every capability, in a fixed order.
    pub const ALL: [Capability; 8] = [
        Self::MailFlags,
        Self::MailMove,
        Self::MailDelete,
        Self::MailExpunge,
        Self::MailAppend,
        Self::MailSend,
        Self::PimEdit,
        Self::PimRespond,
    ];

    /// The stable dotted name used in storage and the API, e.g. `mail.flags`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MailFlags => "mail.flags",
            Self::MailMove => "mail.move",
            Self::MailDelete => "mail.delete",
            Self::MailExpunge => "mail.expunge",
            Self::MailAppend => "mail.append",
            Self::MailSend => "mail.send",
            Self::PimEdit => "pim.edit",
            Self::PimRespond => "pim.respond",
        }
    }

    /// The capability named by [`Capability::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

/// A set of capabilities. The empty set, the default, allows nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(BTreeSet<Capability>);

impl Capabilities {
    /// No capability.
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether `capability` is in the set.
    pub fn has(&self, capability: Capability) -> bool {
        self.0.contains(&capability)
    }

    /// Adds or removes `capability`.
    pub fn set(&mut self, capability: Capability, on: bool) {
        if on {
            self.0.insert(capability);
        } else {
            self.0.remove(&capability);
        }
    }

    /// The capabilities in [`Capability`] order.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.0.iter().copied()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<Capability> for Capabilities {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// What a grant lets a user do in an archive. Each role includes the ones before it:
/// `sender` includes `editor`, which includes `reader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantRole {
    /// Reading the archive.
    Reader,
    /// Also flags, folders, contacts, calendars and answering invitations, where the account
    /// allows it.
    Editor,
    /// Also sending, where the account allows it.
    Sender,
}

impl GrantRole {
    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Editor => "editor",
            Self::Sender => "sender",
        }
    }

    /// The role named by [`GrantRole::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "reader" => Self::Reader,
            "editor" => Self::Editor,
            "sender" => Self::Sender,
            _ => return None,
        })
    }
}

/// One archive granted to a user, with the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The archive.
    pub archive: ArchiveId,
    /// What the user may do in it.
    pub role: GrantRole,
}

/// What kind of write an audit entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// An account's capabilities changed.
    CapabilitiesChanged,
    /// A user's grant on an archive was given, changed or taken away.
    GrantChanged,
    /// Flags or labels changed on the server.
    MailFlags,
    /// Messages moved on the server.
    MailMove,
    /// Messages moved to Trash or expunged on the server.
    MailDelete,
    /// Bytes saved into a server folder.
    MailAppend,
    /// A message handed to a transport.
    MailSend,
    /// A contact or event changed upstream.
    PimEdit,
    /// An invitation answered upstream.
    PimRespond,
    /// A rule flow saved, turned on or off, or removed.
    RuleChanged,
    /// A transport added, changed, granted or removed.
    TransportChanged,
    /// An identity added, changed or removed, or who may send as it changed.
    IdentityChanged,
    /// A message queued, undone, cancelled, retried or sent again.
    OutboxChanged,
    /// A device credential made.
    DeviceCreated,
    /// A device's name, protocols, scopes or archives changed.
    DeviceChanged,
    /// A device credential revoked.
    DeviceRevoked,
}

impl AuditKind {
    /// Every kind, in a fixed order.
    pub const ALL: [AuditKind; 16] = [
        Self::CapabilitiesChanged,
        Self::GrantChanged,
        Self::MailFlags,
        Self::MailMove,
        Self::MailDelete,
        Self::MailAppend,
        Self::MailSend,
        Self::PimEdit,
        Self::PimRespond,
        Self::RuleChanged,
        Self::TransportChanged,
        Self::IdentityChanged,
        Self::OutboxChanged,
        Self::DeviceCreated,
        Self::DeviceChanged,
        Self::DeviceRevoked,
    ];

    /// The stable lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CapabilitiesChanged => "capabilities_changed",
            Self::GrantChanged => "grant_changed",
            Self::MailFlags => "mail_flags",
            Self::MailMove => "mail_move",
            Self::MailDelete => "mail_delete",
            Self::MailAppend => "mail_append",
            Self::MailSend => "mail_send",
            Self::PimEdit => "pim_edit",
            Self::PimRespond => "pim_respond",
            Self::RuleChanged => "rule_changed",
            Self::TransportChanged => "transport_changed",
            Self::IdentityChanged => "identity_changed",
            Self::OutboxChanged => "outbox_changed",
            Self::DeviceCreated => "device_created",
            Self::DeviceChanged => "device_changed",
            Self::DeviceRevoked => "device_revoked",
        }
    }

    /// The kind named by [`AuditKind::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// A write to record in the audit log. Holds ids and counts only, never message content,
/// subjects or addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEntry {
    /// When it happened.
    pub at: Timestamp,
    /// Who did it; `None` for the server itself, such as a rule run.
    pub user: Option<UserId>,
    /// What kind of write.
    pub kind: AuditKind,
    /// The archive it touched, when it touched one.
    pub archive: Option<ArchiveId>,
    /// The account it touched, when it touched one.
    pub account: Option<AccountId>,
    /// Ids of what it touched (messages, items, users, capability names), as text.
    pub targets: Vec<String>,
    /// How many things it touched; may exceed `targets.len()` when the list was cut short.
    pub count: u64,
}

/// One recorded write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Position in the log, from 1, growing with every entry.
    pub seq: u64,
    /// What was recorded.
    pub entry: NewAuditEntry,
}

/// Which audit entries to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditQuery {
    /// Only entries of this archive.
    pub archive: Option<ArchiveId>,
    /// Only entries with a smaller `seq`, for paging back.
    pub before: Option<u64>,
    /// At most this many; the store may cap it.
    pub limit: u32,
}

/// A response kept for an `Idempotency-Key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResponse {
    /// The HTTP status.
    pub status: u16,
    /// The `Content-Type`, when there was one.
    pub content_type: Option<String>,
    /// The body, byte for byte.
    pub body: Vec<u8>,
}

/// Who holds an `Idempotency-Key`: the key is scoped by user and route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey {
    /// The user who sent it.
    pub user: UserId,
    /// The key as sent.
    pub key: String,
    /// The method and path of the request, e.g. `POST /api/archives/…/outbox`.
    pub route: String,
}

/// What [`WriteStore::claim_idempotency_key`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyClaim {
    /// The key is now held for this request, which should run and then be completed or
    /// released.
    Claimed,
    /// A request with this key is still running.
    InFlight,
    /// A request with this key and another body was made.
    Mismatch,
    /// A request with this key finished; this is its response.
    Done(StoredResponse),
}

/// Account capabilities, archive grants with roles, the audit log of writes and the
/// `Idempotency-Key` records of mutating requests.
#[async_trait]
pub trait WriteStore: Send + Sync {
    /// The account's capabilities; empty for an unknown account.
    async fn account_capabilities(&self, account: AccountId) -> Result<Capabilities, StoreError>;

    /// Replaces the account's capabilities. Removed with the account. `NotFound` for an unknown
    /// account.
    async fn set_account_capabilities(
        &self,
        account: AccountId,
        capabilities: &Capabilities,
    ) -> Result<(), StoreError>;

    /// The user's grants with their roles, by archive id. Empty for an unknown user.
    async fn grant_roles(&self, user: UserId) -> Result<Vec<Grant>, StoreError>;

    /// Gives the user `role` on the archive, replacing an earlier role, or takes the grant away
    /// with `None`. Idempotent. `NotFound` for an unknown user or archive.
    async fn set_grant_role(
        &self,
        user: UserId,
        archive: ArchiveId,
        role: Option<GrantRole>,
    ) -> Result<(), StoreError>;

    /// Adds an entry to the end of the audit log and returns it with its `seq`. Entries are never
    /// changed or removed, not with their user, archive or account either.
    async fn append_audit(&self, entry: &NewAuditEntry) -> Result<AuditEntry, StoreError>;

    /// Entries matching the query, newest (largest `seq`) first.
    async fn list_audit(&self, query: &AuditQuery) -> Result<Vec<AuditEntry>, StoreError>;

    /// Holds the key for a request whose body hashes to `fingerprint`, unless a record of it
    /// newer than `since` exists. An older record is replaced, and so is an unfinished claim made
    /// before `abandoned_before` (its request is taken to have died without completing or
    /// releasing it). Atomic: of two claims at once, one gets [`IdempotencyClaim::Claimed`].
    async fn claim_idempotency_key(
        &self,
        key: &IdempotencyKey,
        fingerprint: &str,
        at: Timestamp,
        since: Timestamp,
        abandoned_before: Timestamp,
    ) -> Result<IdempotencyClaim, StoreError>;

    /// Stores the response of a claimed key, so later claims get [`IdempotencyClaim::Done`].
    /// `NotFound` when the key is not held.
    async fn complete_idempotency_key(
        &self,
        key: &IdempotencyKey,
        response: &StoredResponse,
    ) -> Result<(), StoreError>;

    /// Forgets a key, so the request may be made again. Idempotent.
    async fn release_idempotency_key(&self, key: &IdempotencyKey) -> Result<(), StoreError>;

    /// Forgets every key claimed before `before`, finished or not; returns how many.
    async fn prune_idempotency_keys(&self, before: Timestamp) -> Result<u64, StoreError>;
}
