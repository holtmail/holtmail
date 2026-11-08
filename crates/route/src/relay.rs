use serde::{Deserialize, Serialize};

use crate::{
    AliasHit, Context, Identity, Reason, Request, Route, RouteError, Strategy, Transport, norm,
    select,
};

/// What routing needs to answer mail that came through an alias service.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relay {
    /// The alias hit of the original.
    pub hit: AliasHit,
    /// The service's DKIM signature passed on the original. Without it the hit's
    /// `reverse_alias` and [`Relay::reply_to`] are never used as recipients.
    pub signed: bool,
    /// The effective strategy: the detector's default, overridden by a per-alias, per-domain
    /// or per-service setting.
    pub strategy: Strategy,
    /// The addresses the alias forwards to, in the service's order.
    pub mailboxes: Vec<String>,
    /// The `Reply-To` the service set on the original, for [`Strategy::ReplyHeader`].
    pub reply_to: Option<String>,
    /// A reverse alias the caller computed or created for the hit's sender, used when the hit
    /// carries none (or carries one that is not signed).
    pub reverse_alias: Option<String>,
}

/// A route together with what a relay strategy changes in the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routed {
    /// Identity, `From`, transport, strategy and reasons.
    pub route: Route,
    /// The address that replaces the original's sender as the reply's `To`: the reverse alias
    /// or the service's `Reply-To`. `None` when the recipients stay as composed.
    pub to: Option<String>,
    /// The alias the original came through, lowercase, when a relay was given.
    pub alias: Option<String>,
}

/// Decides the route for a draft that may answer mail received through an alias service.
///
/// Without `relay`, or for a new message, it is [`route`](crate::route) with `to` and `alias`
/// empty. With one, the identity is the chosen identity when there is one, otherwise:
///
/// - [`Strategy::ReverseAlias`] and [`Strategy::ReplyHeader`]: the first active identity whose
///   address is one of [`Relay::mailboxes`] ([`Reason::AliasMailbox`]); else the first active
///   real mailbox of the original's account, then the default identity if it is a real
///   mailbox, each followed by [`Reason::NoAliasMailbox`]. A real mailbox is an identity with
///   no linked alias whose address is not the alias. `From` is the identity's address. A
///   chosen identity that is not one of the mailboxes gets [`Reason::NotAliasMailbox`].
/// - [`Strategy::Direct`]: the identity [`route`](crate::route)'s rules pick among the active
///   identities that send as the alias (whose address is the alias or that are linked to it);
///   with none, or a chosen identity that is not one of them, [`RouteError::NoAliasIdentity`].
///   `From` is the alias ([`Reason::AliasFrom`] when that differs from the identity's address),
///   and only the alias is checked against the identity's transport `allowed_from`.
///
/// For a reply under [`Strategy::Direct`] `to` is `None`; otherwise it is set:
///
/// - [`Strategy::ReverseAlias`]: the hit's reverse alias when the hit is signed
///   ([`Reason::ReverseAlias`]), else [`Relay::reverse_alias`]
///   ([`Reason::ComputedReverseAlias`]); with neither, [`RouteError::NeedsReverseAlias`] so
///   the caller can create one and ask again. An unsigned hit's reverse alias is never used
///   ([`Reason::UnsignedReverseAlias`]).
/// - [`Strategy::ReplyHeader`]: [`Relay::reply_to`] ([`Reason::ReplyHeader`]); missing or
///   unsigned is [`RouteError::NoReplyHeader`].
///
/// A forward keeps its recipients (`to` is `None`); the leak guard checks them.
pub fn route_relay(request: Request<'_>, relay: Option<&Relay>) -> Result<Routed, RouteError> {
    let Some(relay) = relay.filter(|_| !matches!(request.context, Context::New)) else {
        return crate::route(request).map(|route| Routed {
            route,
            to: None,
            alias: None,
        });
    };
    let alias = norm(&relay.hit.alias);
    let route = match relay.strategy {
        Strategy::Direct => direct(request, &alias)?,
        Strategy::ReverseAlias | Strategy::ReplyHeader => mailbox(request, relay, &alias)?,
    };
    let mut routed = Routed {
        route,
        to: None,
        alias: Some(alias.clone()),
    };
    if matches!(request.context, Context::Reply(_))
        && let Some((to, reasons)) = recipient(relay, &alias)?
    {
        routed.to = Some(to);
        routed.route.reasons.extend(reasons);
    }
    Ok(routed)
}

/// The attendee address for an iMIP reply sent on this route: the alias when the invitation
/// came through one, so the organizer never sees the real mailbox; otherwise `From`.
pub fn attendee_address(routed: &Routed) -> &str {
    routed.alias.as_deref().unwrap_or(&routed.route.from)
}

fn direct(request: Request<'_>, alias: &str) -> Result<Route, RouteError> {
    let owners: Vec<Identity> = request
        .identities
        .iter()
        .filter(|i| {
            i.active
                && (norm(&i.address) == alias
                    || i.alias.as_deref().map(norm).as_deref() == Some(alias))
        })
        .cloned()
        .collect();
    let missing = || RouteError::NoAliasIdentity {
        alias: alias.to_owned(),
    };
    if owners.is_empty()
        || request
            .chosen
            .is_some_and(|id| !owners.iter().any(|i| i.id == id))
    {
        return Err(missing());
    }
    let default_identity = request
        .default_identity
        .filter(|id| owners.iter().any(|i| i.id == *id));
    let (identity, mut reasons, _) = select(Request {
        identities: &owners,
        default_identity,
        ..request
    })
    .or_else(|_| {
        Ok::<_, RouteError>((
            &owners[0],
            vec![Reason::Alias {
                alias: alias.to_owned(),
            }],
            None,
        ))
    })?;
    let transport = transport(request.transports, &identity.transport)?;
    if norm(&identity.address) != alias {
        reasons.push(Reason::AliasFrom {
            alias: alias.to_owned(),
        });
    }
    check(transport, alias)?;
    Ok(Route {
        identity: identity.id.clone(),
        from: alias.to_owned(),
        name: identity.name.clone(),
        transport: transport.id.clone(),
        strategy: Strategy::Direct,
        reasons,
    })
}

fn mailbox(request: Request<'_>, relay: &Relay, alias: &str) -> Result<Route, RouteError> {
    let mut active = request.identities.iter().filter(|i| i.active);
    let mailboxes: Vec<String> = relay.mailboxes.iter().map(|m| norm(m)).collect();
    let (identity, reasons) = match request.chosen {
        Some(id) => {
            let identity = active
                .find(|i| i.id == id)
                .ok_or_else(|| RouteError::UnknownIdentity { id: id.to_owned() })?;
            let mut reasons = vec![Reason::Chosen];
            let address = norm(&identity.address);
            if !mailboxes.contains(&address) {
                reasons.push(Reason::NotAliasMailbox { address });
            }
            (identity, reasons)
        }
        None => pick_mailbox(request, &mailboxes, alias).ok_or(RouteError::NoIdentity)?,
    };
    let transport = transport(request.transports, &identity.transport)?;
    check(transport, &identity.address)?;
    Ok(Route {
        identity: identity.id.clone(),
        from: identity.address.clone(),
        name: identity.name.clone(),
        transport: transport.id.clone(),
        strategy: relay.strategy,
        reasons,
    })
}

fn pick_mailbox<'a>(
    request: Request<'a>,
    mailboxes: &[String],
    alias: &str,
) -> Option<(&'a Identity, Vec<Reason>)> {
    let active = || request.identities.iter().filter(|i| i.active);
    for m in mailboxes {
        if let Some(i) = active().find(|i| norm(&i.address) == *m) {
            return Some((i, vec![Reason::AliasMailbox { address: m.clone() }]));
        }
    }
    let real = |i: &&Identity| i.alias.is_none() && norm(&i.address) != alias;
    let missing = Reason::NoAliasMailbox {
        alias: alias.to_owned(),
    };
    let account = match request.context {
        Context::Reply(o) | Context::Forward(o) => o.account.as_ref(),
        Context::New => None,
    };
    if let Some(account) = account
        && let Some(i) = active()
            .filter(real)
            .find(|i| i.account.as_ref() == Some(account))
    {
        let reason = Reason::Account {
            account: account.clone(),
        };
        return Some((i, vec![reason, missing]));
    }
    let id = request.default_identity?;
    active()
        .filter(real)
        .find(|i| i.id == id)
        .map(|i| (i, vec![Reason::Default, missing]))
}

type Recipient = Option<(String, Vec<Reason>)>;

fn recipient(relay: &Relay, alias: &str) -> Result<Recipient, RouteError> {
    match relay.strategy {
        Strategy::Direct => Ok(None),
        Strategy::ReverseAlias => {
            let mut reasons = Vec::new();
            let provided = relay.hit.reverse_alias.as_deref().map(norm);
            if let Some(address) = provided.clone().filter(|_| relay.signed) {
                return Ok(Some((
                    address.clone(),
                    vec![Reason::ReverseAlias { address }],
                )));
            }
            if let Some(address) = provided {
                reasons.push(Reason::UnsignedReverseAlias { address });
            }
            match relay.reverse_alias.as_deref().map(norm) {
                Some(address) => {
                    reasons.push(Reason::ComputedReverseAlias {
                        address: address.clone(),
                    });
                    Ok(Some((address, reasons)))
                }
                None => Err(RouteError::NeedsReverseAlias {
                    alias: alias.to_owned(),
                    contact: relay.hit.sender.as_deref().map(norm),
                }),
            }
        }
        Strategy::ReplyHeader => match relay.reply_to.as_deref().map(norm) {
            Some(address) if relay.signed => Ok(Some((
                address.clone(),
                vec![Reason::ReplyHeader { address }],
            ))),
            _ => Err(RouteError::NoReplyHeader {
                alias: alias.to_owned(),
            }),
        },
    }
}

fn transport<'a>(transports: &'a [Transport], id: &str) -> Result<&'a Transport, RouteError> {
    transports
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| RouteError::UnknownTransport {
            transport: id.to_owned(),
        })
}

fn check(transport: &Transport, from: &str) -> Result<(), RouteError> {
    if transport.allows(from) {
        Ok(())
    } else {
        Err(RouteError::NotAllowedFrom {
            address: from.to_owned(),
            transport: transport.id.clone(),
        })
    }
}
