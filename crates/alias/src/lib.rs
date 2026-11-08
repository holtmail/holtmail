//! Contract for email alias services: SimpleLogin, addy.io, Firefox Relay, DuckDuckGo, Fastmail
//! Masked Email, Apple Hide My Email and anything like them.
//!
//! Two traits. An [`AliasDetector`] looks at the header fields of one message and says whether
//! an alias service forwarded it, to which alias, and who really sent it. An [`AliasDirectory`]
//! is one account at a service: it lists the account's aliases and, as far as the service
//! allows, creates, changes and deletes them.
//!
//! Detection is pure and cheap and never fails: a message that is not recognised is simply not
//! forwarded. Everything a detector returns is derived from the raw message and can be worked
//! out again.

use async_trait::async_trait;
pub use holtmail_core::{Address, Timestamp};

mod mail;

#[cfg(feature = "testing")]
pub mod testing;

pub use mail::{Header, Mail, parse_mailbox};

/// What a detector found in a forwarded message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasHit {
    /// The provider id of the detector that found it, e.g. `simplelogin`.
    pub provider: String,
    /// The alias that received the message, lowercase.
    pub alias: String,
    /// Who really sent the message, when the service rewrote `From` and said who it was. `None`
    /// when `From` already is the sender.
    pub sender: Option<Address>,
    /// The address that reaches [`AliasHit::sender`] through the service, lowercase, when the
    /// service gives one per sender (the rewritten `From` or `Reply-To`). `None` when replies go
    /// to an address shared by many senders, or the service does not hide the sender.
    pub reverse_alias: Option<String>,
    /// The receiving server recorded a passing DKIM signature of one of the provider's domains
    /// in the topmost `Authentication-Results` field. Without it the forwarding fields may have
    /// been written by anyone.
    pub signed: bool,
}

/// How a reply to mail that came through an alias leaves, so the recipient sees the alias and
/// never the real mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyStrategy {
    /// `To:` the reverse alias, `From:` a mailbox the alias forwards to; the service rewrites
    /// `From` to the alias. SimpleLogin, addy.io, DuckDuckGo, Apple Hide My Email.
    ReverseAlias,
    /// `From:` the alias itself, through a transport allowed to send as it. Fastmail Masked
    /// Email, own domains and catch-all rules, and any alias without a detector.
    Direct,
    /// `To:` the `Reply-To` the service set, `From:` the real mailbox. Firefox Relay.
    ReplyHeader,
}

impl ReplyStrategy {
    /// The name used in config, the API and route explanations: `reverse_alias`, `direct`,
    /// `reply_header`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReverseAlias => "reverse_alias",
            Self::Direct => "direct",
            Self::ReplyHeader => "reply_header",
        }
    }

    /// The strategy named `name` (as [`ReplyStrategy::as_str`] writes it), `None` for any
    /// other text.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "reverse_alias" => Some(Self::ReverseAlias),
            "direct" => Some(Self::Direct),
            "reply_header" => Some(Self::ReplyHeader),
            _ => None,
        }
    }
}

/// Recognises mail forwarded by one alias service.
///
/// Implementations must be deterministic (the same mail always gives the same answer), must
/// never panic on malformed fields, and must return `None` for mail that carries none of the
/// service's marks.
pub trait AliasDetector: Send + Sync {
    /// The provider id put in every [`AliasHit::provider`], lowercase ASCII, e.g. `simplelogin`.
    fn provider(&self) -> &str;

    /// Changes whenever the detector may give other answers for the same mail, such as when its
    /// rules change; callers detect stored mail again when it does.
    fn version(&self) -> String;

    /// The alias the message was forwarded to, or `None` when this service did not forward it.
    fn detect(&self, mail: &Mail) -> Option<AliasHit>;

    /// How replies to this service's mail leave by default; the same answer for the
    /// detector's lifetime. Without an override it is [`ReplyStrategy::Direct`], which never
    /// shows the real mailbox and is refused by any transport not allowed to send as the alias.
    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::Direct
    }

    /// The reverse alias through which `alias` reaches `contact`, for services that encode the
    /// contact in the address. Lowercase; `None` when the service does not encode addresses,
    /// `alias` is not one of its aliases or either address cannot be encoded. Pure: the same
    /// arguments always give the same answer, and a result given to
    /// [`AliasDetector::decode_reverse`] gives `contact` back.
    fn encode_reverse(&self, alias: &str, contact: &str) -> Option<String> {
        let _ = (alias, contact);
        None
    }

    /// The contact a reverse alias of this service stands for, lowercase, when the service
    /// encodes it in the address; `None` otherwise. A display hint only: anyone can write an
    /// address of this shape.
    fn decode_reverse(&self, address: &str) -> Option<String> {
        let _ = address;
        None
    }
}

/// Where a service forwards an alias's mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    /// The service's id for it.
    pub id: String,
    /// Its address.
    pub email: String,
}

/// An alias as the service knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAlias {
    /// The service's id for the alias, used for changes.
    pub id: String,
    /// The alias, lowercase.
    pub address: String,
    /// Forwarding is on.
    pub enabled: bool,
    /// The owner's note.
    pub note: Option<String>,
    /// The display name the service sends replies with.
    pub name: Option<String>,
    /// The website or domain the alias was made for.
    pub for_domain: Option<String>,
    /// Where its mail goes.
    pub mailboxes: Vec<Mailbox>,
    /// When the service made it.
    pub created_at: Option<Timestamp>,
    /// Messages forwarded, as the service counts them.
    pub forwarded: Option<u64>,
    /// Messages blocked.
    pub blocked: Option<u64>,
    /// Replies sent through it.
    pub replied: Option<u64>,
}

/// What a directory can do besides listing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Creates aliases with a name the service picks.
    pub create_random: bool,
    /// Creates aliases with a prefix the owner picks, on a domain from
    /// [`AliasOptions::domains`].
    pub create_custom: bool,
    /// Keeps a note per alias.
    pub note: bool,
    /// Keeps a display name per alias.
    pub name: bool,
    /// Keeps the website an alias is for.
    pub for_domain: bool,
    /// Switches forwarding off and on.
    pub switch: bool,
    /// Chooses the mailboxes an alias forwards to.
    pub mailboxes: bool,
    /// Deletes aliases.
    pub delete: bool,
}

/// A domain or suffix new aliases can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasDomain {
    /// What [`NewAlias::domain`] takes. Opaque; some services sign it.
    pub id: String,
    /// What people see, e.g. `@example.com` or `.a1b2@example.com`.
    pub label: String,
}

/// Choices for new aliases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AliasOptions {
    /// Domains or suffixes for aliases with a chosen prefix, the service's default first.
    pub domains: Vec<AliasDomain>,
    /// Mailboxes aliases can forward to, the service's default first.
    pub mailboxes: Vec<Mailbox>,
    /// The service's own suggestion for a prefix.
    pub prefix_hint: Option<String>,
    /// The account may create aliases now; `false` when a quota is used up.
    pub can_create: bool,
}

/// An alias to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAlias {
    /// The part before the domain or suffix; `None` lets the service pick the whole alias.
    pub prefix: Option<String>,
    /// An [`AliasDomain::id`]; `None` for the service's default.
    pub domain: Option<String>,
    /// A note.
    pub note: Option<String>,
    /// A display name.
    pub name: Option<String>,
    /// The website it is for.
    pub for_domain: Option<String>,
    /// [`Mailbox::id`]s to forward to; empty for the service's default.
    pub mailbox_ids: Vec<String>,
}

/// Changes to an alias. `None` leaves a field as it is; an empty string clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AliasChange {
    /// Forwarding on or off.
    pub enabled: Option<bool>,
    /// The note.
    pub note: Option<String>,
    /// The display name.
    pub name: Option<String>,
    /// The website it is for.
    pub for_domain: Option<String>,
    /// The mailboxes it forwards to, at least one.
    pub mailbox_ids: Option<Vec<String>>,
}

impl AliasChange {
    /// Nothing would change.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Why a directory call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AliasError {
    /// The service refused the key or token; it must be replaced.
    #[error("the service refused the key")]
    Auth,
    /// The directory cannot do this; see [`AliasDirectory::capabilities`].
    #[error("the service cannot do this")]
    Unsupported,
    /// No alias with this id.
    #[error("no such alias")]
    NotFound,
    /// The service refused the request, in words that can be shown to the owner, e.g. a taken
    /// prefix or a used-up quota. Sending the same request again gives the same answer.
    #[error("{0}")]
    Invalid(String),
    /// The service could not be reached, is busy or failed; trying later may work.
    #[error("the service is unavailable: {0}")]
    Unavailable(String),
    /// The service answered something this directory does not understand.
    #[error("unexpected answer: {0}")]
    Protocol(String),
}

/// One account at an alias service.
///
/// Calls may run concurrently. A change is done at the service when the call returns `Ok`; a
/// call that fails may or may not have reached it, so callers list the aliases again to know.
#[async_trait]
pub trait AliasDirectory: Send + Sync {
    /// What this directory can do besides listing. The same answer for the directory's lifetime.
    fn capabilities(&self) -> Capabilities;

    /// The `reverse` capability: [`AliasDirectory::reverse_alias`] gives the reverse alias for
    /// a contact an alias has never heard from. `false` by default; the same answer for the
    /// directory's lifetime.
    fn reverse(&self) -> bool {
        false
    }

    /// Every alias of the account, each once, in no particular order. Deleted aliases are not
    /// listed.
    async fn aliases(&self) -> Result<Vec<RemoteAlias>, AliasError>;

    /// Choices for new aliases. Also checks that the key works: `Auth` when it does not.
    async fn options(&self) -> Result<AliasOptions, AliasError>;

    /// Creates an alias and returns it as the service stored it. `Unsupported` when the request
    /// needs a capability the directory lacks (a prefix without `create_custom`, none without
    /// `create_random`); `Invalid` when the service refuses it.
    async fn create(&self, alias: &NewAlias) -> Result<RemoteAlias, AliasError>;

    /// Applies `change` and returns the alias as it is now. Fields the directory cannot change
    /// are `Unsupported`, and nothing is changed. `NotFound` for an unknown id.
    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError>;

    /// Deletes the alias at the service; mail to it is no longer forwarded. `NotFound` for an
    /// unknown id. `Unsupported` without the `delete` capability.
    async fn delete(&self, id: &str) -> Result<(), AliasError>;

    /// The reverse alias through which the alias `id` reaches `contact` (an address), lowercase.
    /// Idempotent: asking again for the same contact gives the same address, made at the
    /// service the first time. `NotFound` for an unknown alias, `Invalid` for an address the
    /// service refuses, `Unsupported` without the [`AliasDirectory::reverse`] capability (the
    /// default).
    async fn reverse_alias(&self, id: &str, contact: &str) -> Result<String, AliasError> {
        let _ = (id, contact);
        Err(AliasError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Plain;

    impl AliasDetector for Plain {
        fn provider(&self) -> &str {
            "plain"
        }

        fn version(&self) -> String {
            "1".into()
        }

        fn detect(&self, _mail: &Mail) -> Option<AliasHit> {
            None
        }
    }

    #[test]
    fn strategies_have_stable_names() {
        for strategy in [
            ReplyStrategy::ReverseAlias,
            ReplyStrategy::Direct,
            ReplyStrategy::ReplyHeader,
        ] {
            assert_eq!(ReplyStrategy::parse(strategy.as_str()), Some(strategy));
        }
        assert_eq!(ReplyStrategy::parse("forward"), None);
    }

    #[test]
    fn defaults_are_direct_and_no_reverse_aliases() {
        assert_eq!(Plain.reply_strategy(), ReplyStrategy::Direct);
        assert_eq!(
            Plain.encode_reverse("shop@alias.example.org", "bob@example.org"),
            None
        );
        assert_eq!(
            Plain.decode_reverse("shop+bob=example.org@alias.example.org"),
            None
        );
    }
}
