//! Decides how one draft is sent: which identity it comes from, the exact `From` address,
//! the transport that carries it and the strategy.
//!
//! A pure library over its own input types: callers load identities and transports from
//! wherever they keep them and pass them in. [`route`] is deterministic: the same request
//! always gives the same answer. [`route_relay`] adds the alias strategies and [`leaks`]
//! the leak guard.

mod leak;
mod pattern;
mod relay;

pub use leak::{Field, Fix, Leak, LeakReason, Recipients, Thread, leaks};
pub use pattern::{allows, valid_pattern};
pub use relay::{Relay, Routed, attendee_address, route_relay};
use serde::{Deserialize, Serialize};

/// An address a person may send as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Stable id, unique among the identities passed to one [`route`] call.
    pub id: String,
    /// The `From` address.
    pub address: String,
    /// The display name shown with [`Identity::address`].
    pub name: Option<String>,
    /// The id of the [`Transport`] that sends for this identity.
    pub transport: String,
    /// The account the identity belongs to, used to answer mail synced from that account.
    pub account: Option<String>,
    /// The alias address the identity answers for, used to answer mail sent to that alias.
    pub alias: Option<String>,
    /// Inactive identities are never picked, not even when chosen explicitly.
    pub active: bool,
    /// A reply to `local+tag@domain` keeps that address as `From` instead of the plain
    /// address, when the transport allows it.
    pub plus_addresses: bool,
}

/// How bytes leave, as far as routing cares: which addresses it may send as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transport {
    /// Stable id, referenced by [`Identity::transport`].
    pub id: String,
    /// Patterns of the addresses this transport may send as, see [`allows`]. An empty list
    /// allows nothing.
    pub allowed_from: Vec<String>,
}

impl Transport {
    /// Whether any of [`Transport::allowed_from`] covers `address`.
    pub fn allows(&self, address: &str) -> bool {
        self.allowed_from.iter().any(|p| allows(p, address))
    }
}

/// What the draft is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Context {
    /// A message that answers nothing.
    #[default]
    New,
    /// A reply (or reply all, or reply to list) to an archived message.
    Reply(Original),
    /// A forward of an archived message. Routed like a reply.
    Forward(Original),
}

/// What routing needs to know about the message being answered or forwarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Original {
    /// The addresses it was delivered to, in order of preference: `To`, `Cc`,
    /// `Delivered-To`, `X-Original-To`. Earlier addresses win.
    pub recipients: Vec<String>,
    /// The account it was synced from.
    pub account: Option<String>,
    /// The alias service it came through, when one was detected.
    pub alias: Option<AliasHit>,
}

/// An alias service hit on the original, mirroring `holtmail_alias::AliasHit`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasHit {
    /// The provider id of the detector, e.g. `simplelogin`.
    pub provider: String,
    /// The alias that received the message.
    pub alias: String,
    /// The address that reaches the real sender through the service, when there is one.
    pub reverse_alias: Option<String>,
    /// The real sender, when the service rewrote `From`.
    pub sender: Option<String>,
}

/// One routing question.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Every identity the person may use, active or not.
    pub identities: &'a [Identity],
    /// Every transport those identities may reference.
    pub transports: &'a [Transport],
    /// What the draft is.
    pub context: &'a Context,
    /// The id of the person's default identity.
    pub default_identity: Option<&'a str>,
    /// The id of an identity the person picked by hand. It wins over every default but is
    /// still checked.
    pub chosen: Option<&'a str>,
}

/// How the message reaches its recipients through the transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Sent as is, `From` the route's address; through an alias, `From` the alias itself. The
    /// only strategy [`route`] returns.
    #[default]
    Direct,
    /// Sent to the alias service's reverse address from a mailbox the alias forwards to; the
    /// service rewrites `From` to the alias. See [`route_relay`].
    ReverseAlias,
    /// Sent to the `Reply-To` the alias service set, from the real mailbox. See
    /// [`route_relay`].
    ReplyHeader,
}

/// Why the route looks the way it does: stable codes the interface translates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Reason {
    /// The person picked the identity.
    Chosen,
    /// The original was delivered to the identity's address.
    OriginalRecipient {
        /// The recipient of the original that matched, lowercase.
        address: String,
    },
    /// The original was delivered to a plus address of the identity.
    PlusAddress {
        /// The plus address, lowercase.
        address: String,
    },
    /// The original was delivered to the alias the identity answers for.
    Alias {
        /// The alias, lowercase.
        alias: String,
    },
    /// The original was synced from the identity's account.
    Account {
        /// The account id.
        account: String,
    },
    /// The person's default identity.
    Default,
    /// The plus address is not used as `From`: the identity does not keep plus addresses
    /// or the transport may not send as it. Comes after [`Reason::PlusAddress`].
    PlusAddressDropped {
        /// The plus address that was not used, lowercase.
        address: String,
    },
    /// The identity's address is one of the mailboxes the alias forwards to.
    AliasMailbox {
        /// The mailbox, lowercase.
        address: String,
    },
    /// No active identity is a mailbox the alias forwards to, so a real mailbox of the account
    /// or the default identity is used; the service may refuse the reply. A note.
    NoAliasMailbox {
        /// The alias, lowercase.
        alias: String,
    },
    /// The chosen identity is not a mailbox the alias forwards to; the service may refuse the
    /// reply. A note.
    NotAliasMailbox {
        /// The chosen identity's address, lowercase.
        address: String,
    },
    /// `From` is the alias instead of the identity's own address. A note.
    AliasFrom {
        /// The alias, lowercase.
        alias: String,
    },
    /// `To` is the reverse alias the service gave on the original. A note.
    ReverseAlias {
        /// The reverse alias, lowercase.
        address: String,
    },
    /// `To` is a reverse alias the caller computed or created. A note.
    ComputedReverseAlias {
        /// The reverse alias, lowercase.
        address: String,
    },
    /// The original carried a reverse alias without the service's signature, so it is not
    /// used. A note.
    UnsignedReverseAlias {
        /// The ignored address, lowercase.
        address: String,
    },
    /// `To` is the `Reply-To` the service set on the original. A note.
    ReplyHeader {
        /// The `Reply-To` address, lowercase.
        address: String,
    },
}

/// The answer to one [`Request`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// The id of the identity.
    pub identity: String,
    /// The exact `From` address: the identity's address or one of its plus addresses.
    pub from: String,
    /// The display name for `From`.
    pub name: Option<String>,
    /// The id of the transport.
    pub transport: String,
    /// How the message is sent.
    pub strategy: Strategy,
    /// The rule that picked the identity first, then any notes.
    pub reasons: Vec<Reason>,
}

/// Why no route could be given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RouteError {
    /// No rule found an active identity: no match and no usable default identity.
    #[error("no identity to send from")]
    NoIdentity,
    /// The chosen identity does not exist or is inactive.
    #[error("identity {id} cannot be used")]
    UnknownIdentity {
        /// The chosen id.
        id: String,
    },
    /// The identity names a transport that was not passed in.
    #[error("transport {transport} does not exist")]
    UnknownTransport {
        /// The missing transport id.
        transport: String,
    },
    /// The transport's `allowed_from` does not cover the `From` address.
    #[error("transport {transport} may not send as {address}")]
    NotAllowedFrom {
        /// The `From` address.
        address: String,
        /// The transport id.
        transport: String,
    },
    /// The strategy is [`Strategy::ReverseAlias`] and no trusted reverse alias is known: the
    /// caller creates or computes one for `contact` and asks again with
    /// [`Relay::reverse_alias`].
    #[error("a reverse alias of {alias} is needed")]
    NeedsReverseAlias {
        /// The alias, lowercase.
        alias: String,
        /// The real sender the reverse alias must reach, lowercase; `None` when the service
        /// did not rewrite `From`, and then the original's `From` is the contact.
        contact: Option<String>,
    },
    /// The strategy is [`Strategy::ReplyHeader`] and the original has no `Reply-To` from the
    /// service, or the service's signature did not pass.
    #[error("no trusted Reply-To for {alias}")]
    NoReplyHeader {
        /// The alias, lowercase.
        alias: String,
    },
    /// The strategy is [`Strategy::Direct`] and the person has no active identity that sends as
    /// the alias: none whose address is the alias or that is linked to it.
    #[error("no identity sends as {alias}")]
    NoAliasIdentity {
        /// The alias, lowercase.
        alias: String,
    },
}

/// Decides the route for one draft.
///
/// The identity is the first of, among active identities only:
///
/// 1. the chosen identity ([`Reason::Chosen`]); an unknown or inactive one is
///    [`RouteError::UnknownIdentity`], never a fallback to the defaults;
/// 2. for a reply or forward, the identity whose address is a recipient of the original
///    ([`Reason::OriginalRecipient`]);
/// 3. the identity with a plus address among the recipients, `ada+shop@example.com` for
///    `ada@example.com` ([`Reason::PlusAddress`]);
/// 4. the identity linked to the alias the original came through, or to an alias among its
///    recipients ([`Reason::Alias`]);
/// 5. the identity linked to the account the original was synced from ([`Reason::Account`]);
/// 6. the default identity ([`Reason::Default`]).
///
/// Recipients are tried in their order, identities in theirs; addresses compare lowercase.
/// `From` is the identity's address, or the plus address in step 3 when the identity keeps
/// plus addresses and its transport allows it ([`Reason::PlusAddressDropped`] otherwise).
/// A `From` outside the transport's `allowed_from` is [`RouteError::NotAllowedFrom`]; the
/// route never moves on to another identity to avoid it.
///
/// The strategy is always [`Strategy::Direct`]; [`route_relay`] applies alias strategies.
pub fn route(request: Request<'_>) -> Result<Route, RouteError> {
    let (identity, mut reasons, plus) = select(request)?;
    let transport = request
        .transports
        .iter()
        .find(|t| t.id == identity.transport)
        .ok_or_else(|| RouteError::UnknownTransport {
            transport: identity.transport.clone(),
        })?;
    let from = match plus {
        Some(plus) if identity.plus_addresses && transport.allows(&plus) => plus,
        Some(plus) => {
            reasons.push(Reason::PlusAddressDropped { address: plus });
            identity.address.clone()
        }
        None => identity.address.clone(),
    };
    if !transport.allows(&from) {
        return Err(RouteError::NotAllowedFrom {
            address: from,
            transport: transport.id.clone(),
        });
    }
    Ok(Route {
        identity: identity.id.clone(),
        from,
        name: identity.name.clone(),
        transport: transport.id.clone(),
        strategy: Strategy::Direct,
        reasons,
    })
}

type Selected<'a> = (&'a Identity, Vec<Reason>, Option<String>);

fn select(request: Request<'_>) -> Result<Selected<'_>, RouteError> {
    let active = || request.identities.iter().filter(|i| i.active);
    match request.chosen {
        Some(id) => {
            let identity = active()
                .find(|i| i.id == id)
                .ok_or_else(|| RouteError::UnknownIdentity { id: id.to_owned() })?;
            Ok((identity, vec![Reason::Chosen], None))
        }
        None => pick(request, active).ok_or(RouteError::NoIdentity),
    }
}

fn pick<'a, I>(request: Request<'a>, active: impl Fn() -> I) -> Option<Selected<'a>>
where
    I: Iterator<Item = &'a Identity>,
{
    if let Context::Reply(original) | Context::Forward(original) = request.context {
        let recipients: Vec<String> = original.recipients.iter().map(|r| norm(r)).collect();
        for r in &recipients {
            if let Some(i) = active().find(|i| norm(&i.address) == *r) {
                return Some((
                    i,
                    vec![Reason::OriginalRecipient { address: r.clone() }],
                    None,
                ));
            }
        }
        for r in &recipients {
            let Some(base) = plus_base(r) else { continue };
            if let Some(i) = active().find(|i| norm(&i.address) == base) {
                let reason = Reason::PlusAddress { address: r.clone() };
                return Some((i, vec![reason], Some(r.clone())));
            }
        }
        let hit = original.alias.as_ref().map(|h| norm(&h.alias));
        for a in hit.iter().chain(&recipients) {
            if let Some(i) = active().find(|i| i.alias.as_deref().map(norm).as_ref() == Some(a)) {
                return Some((i, vec![Reason::Alias { alias: a.clone() }], None));
            }
        }
        if let Some(account) = &original.account
            && let Some(i) = active().find(|i| i.account.as_ref() == Some(account))
        {
            let reason = Reason::Account {
                account: account.clone(),
            };
            return Some((i, vec![reason], None));
        }
    }
    let id = request.default_identity?;
    active()
        .find(|i| i.id == id)
        .map(|i| (i, vec![Reason::Default], None))
}

fn norm(address: &str) -> String {
    address.trim().to_lowercase()
}

fn plus_base(address: &str) -> Option<String> {
    let (local, domain) = address.rsplit_once('@')?;
    let (base, tag) = local.split_once('+')?;
    (!base.is_empty() && !tag.is_empty() && !domain.is_empty()).then(|| format!("{base}@{domain}"))
}

#[cfg(test)]
mod tests;
