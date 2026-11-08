use async_trait::async_trait;
use holtmail_core::{AccountId, Address, AliasServiceId, ArchiveId, MessageId, Timestamp};

use crate::StoreError;

/// What alias detection found in one message. Derived from the raw message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageAlias {
    /// The provider id of the detector, e.g. `simplelogin`.
    pub provider: String,
    /// The alias that received the message, lowercase.
    pub alias: String,
    /// Who really sent it, when the service rewrote `From`.
    pub sender: Option<Address>,
    /// The address that reaches the sender through the service, lowercase.
    pub reverse_alias: Option<String>,
    /// The provider's DKIM signature passed at the receiving server.
    pub signed: bool,
}

/// An account at an alias service. Its API key is sealed by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasService {
    /// Id, chosen by the caller so the key can be sealed for it before storing.
    pub id: AliasServiceId,
    /// The archive its aliases belong to.
    pub archive_id: ArchiveId,
    /// The provider id, e.g. `simplelogin`.
    pub provider: String,
    /// A name the owner gave it.
    pub name: String,
    /// The service's address.
    pub base_url: String,
    /// The API key or token, sealed.
    pub sealed_key: Vec<u8>,
    /// When it was added.
    pub created_at: Timestamp,
    /// When its aliases were last listed.
    pub synced_at: Option<Timestamp>,
    /// Why the last listing failed, in words without secrets; `None` after a good one.
    pub sync_error: Option<String>,
}

/// An alias the owner knows of: listed by a service, or added by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownAlias {
    /// The alias, lowercase.
    pub address: String,
    /// The provider id.
    pub provider: String,
    /// The service that lists it; `None` for an alias added by hand.
    pub service_id: Option<AliasServiceId>,
    /// The service's id for it.
    pub remote_id: Option<String>,
    /// Forwarding is on, when the service says.
    pub enabled: Option<bool>,
    /// The owner's note.
    pub note: Option<String>,
    /// The display name for replies.
    pub name: Option<String>,
    /// The website it is for.
    pub for_domain: Option<String>,
    /// Addresses its mail is forwarded to.
    pub mailboxes: Vec<String>,
    /// When the service made it.
    pub created_at: Option<Timestamp>,
    /// Forwarded messages, as the service counts them.
    pub forwarded: Option<u64>,
    /// Blocked messages.
    pub blocked: Option<u64>,
    /// Replies sent through it.
    pub replied: Option<u64>,
}

/// Mail of an alias in the archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AliasMail {
    /// Messages forwarded to it or addressed to it.
    pub messages: u64,
    /// Distinct senders of those messages.
    pub senders: u64,
    /// Of the forwarded ones, those whose provider signature passed.
    pub signed: u64,
    /// The oldest message's date.
    pub first_at: Option<Timestamp>,
    /// The newest message's date.
    pub last_at: Option<Timestamp>,
    /// Accounts the messages were found in, with how many each, most first.
    pub accounts: Vec<(AccountId, u64)>,
}

/// One alias of an archive: what is known of it and what the archive's mail says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasSummary {
    /// The alias, lowercase.
    pub address: String,
    /// The provider id: the service's when known, else the detector's.
    pub provider: String,
    /// What a service or the owner said about it; `None` for an alias only seen in mail.
    pub known: Option<KnownAlias>,
    /// Its mail.
    pub mail: AliasMail,
}

/// What an alias reply strategy override applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StrategyScope {
    /// One alias address.
    Alias,
    /// Every alias on a domain.
    Domain,
    /// Every alias of a provider, by provider id (`simplelogin`).
    Service,
}

impl StrategyScope {
    /// Every scope, most specific first.
    pub const ALL: [StrategyScope; 3] = [Self::Alias, Self::Domain, Self::Service];

    /// The stable lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Domain => "domain",
            Self::Service => "service",
        }
    }

    /// The scope named by [`StrategyScope::as_str`].
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// A reply strategy an archive manager set for an alias, a domain or a service, overriding the
/// detector's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasStrategy {
    /// What it applies to.
    pub scope: StrategyScope,
    /// The alias address, domain or provider id, lowercase.
    pub key: String,
    /// The strategy name (`reverse_alias`, `direct`, `reply_header`), kept as given; callers
    /// check it.
    pub strategy: String,
}

/// Aliases: what detection found in each message, the alias services connected to archives and
/// the aliases they list or the owner added.
///
/// Everything about messages is derived and replaced whole. Removing a message or an archive
/// removes what is recorded about it; removing an archive also removes its services and aliases.
#[async_trait]
pub trait AliasStore: Send + Sync {
    /// Records what detection found in `message`, replacing what was recorded; `None` records
    /// that nothing was found. Idempotent. `NotFound` for an unknown message.
    async fn set_message_alias(
        &self,
        message: MessageId,
        alias: Option<&MessageAlias>,
    ) -> Result<(), StoreError>;

    /// What detection found in `message`, or `None`.
    async fn message_alias(&self, message: MessageId) -> Result<Option<MessageAlias>, StoreError>;

    /// The known aliases of `archive` among `addresses` (compared lowercase).
    async fn known_aliases_among(
        &self,
        archive: ArchiveId,
        addresses: &[String],
    ) -> Result<Vec<KnownAlias>, StoreError>;

    /// Every alias of `archive`: the known ones and those only seen in its mail, each once,
    /// sorted by address.
    async fn aliases(&self, archive: ArchiveId) -> Result<Vec<AliasSummary>, StoreError>;

    /// Adds or replaces the known alias `alias.address` of `archive`.
    async fn put_alias(&self, archive: ArchiveId, alias: &KnownAlias) -> Result<(), StoreError>;

    /// Forgets the known alias; what mail says of it stays. Returns whether there was one.
    async fn remove_alias(&self, archive: ArchiveId, address: &str) -> Result<bool, StoreError>;

    /// Makes `aliases` the aliases the service lists: adds and replaces them and forgets the
    /// service's other aliases, records the time as `synced_at` and clears `sync_error`. An
    /// alias added by hand with the same address becomes the service's. `NotFound` for an
    /// unknown service.
    async fn replace_service_aliases(
        &self,
        service: AliasServiceId,
        aliases: &[KnownAlias],
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Adds a service. `Invalid` when the id is taken, `NotFound` for an unknown archive.
    async fn create_alias_service(&self, service: &AliasService) -> Result<(), StoreError>;

    /// The services of `archive`, oldest first.
    async fn alias_services(&self, archive: ArchiveId) -> Result<Vec<AliasService>, StoreError>;

    /// Every service of every archive, oldest first.
    async fn all_alias_services(&self) -> Result<Vec<AliasService>, StoreError>;

    /// The service, or `None`.
    async fn get_alias_service(
        &self,
        id: AliasServiceId,
    ) -> Result<Option<AliasService>, StoreError>;

    /// Replaces the service's name, address, key and sync state. `NotFound` for an unknown id.
    async fn update_alias_service(&self, service: &AliasService) -> Result<(), StoreError>;

    /// Removes the service. Its aliases stay as aliases added by hand when `keep_aliases`, and
    /// are forgotten otherwise. `NotFound` for an unknown id.
    async fn delete_alias_service(
        &self,
        id: AliasServiceId,
        keep_aliases: bool,
    ) -> Result<(), StoreError>;

    /// Every alias address of `archive` the owner has: known aliases and aliases seen in mail,
    /// lowercase, sorted. People counts them as the owner's own addresses.
    async fn own_alias_addresses(&self, archive: ArchiveId) -> Result<Vec<String>, StoreError>;

    /// Every reply strategy override of `archive`, sorted by scope (alias, domain, service) and
    /// key. Empty for an unknown archive.
    async fn alias_strategies(&self, archive: ArchiveId) -> Result<Vec<AliasStrategy>, StoreError>;

    /// Sets the override for `scope` and `key` (trimmed and compared lowercase), replacing the
    /// one there; `None` removes it, and removing a missing one is fine. Removing the archive
    /// removes its overrides. `NotFound` for an unknown archive.
    async fn set_alias_strategy(
        &self,
        archive: ArchiveId,
        scope: StrategyScope,
        key: &str,
        strategy: Option<&str>,
    ) -> Result<(), StoreError>;

    /// Records that `reverse_alias` reaches `contact` through `alias` in `archive` (all trimmed
    /// and compared lowercase), replacing what was recorded for that reverse alias. People and
    /// the reader then count the reverse alias as the contact, as for one seen in mail; a
    /// reverse alias seen in mail with a sender keeps that sender. Idempotent. Removing the
    /// archive removes the pairs. `NotFound` for an unknown archive.
    async fn put_reverse_alias(
        &self,
        archive: ArchiveId,
        alias: &str,
        contact: &str,
        reverse_alias: &str,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// The contact recorded by [`AliasStore::put_reverse_alias`] for `reverse_alias`
    /// (compared lowercase), lowercase, or `None`.
    async fn reverse_alias_contact(
        &self,
        archive: ArchiveId,
        reverse_alias: &str,
    ) -> Result<Option<String>, StoreError>;
}
