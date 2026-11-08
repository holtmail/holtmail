use holtmail_core::ArchiveId;
use holtmail_route::{
    AliasHit, Context, Leak, Original, Recipients, Relay, RouteError, Routed, Strategy, Thread,
};
use holtmail_store::{Message, User};

use crate::drafts::MailboxDto;
use crate::{ApiError, ApiResult, ApiState};

const THREAD_LIMIT: usize = 200;

/// Everything routing knows about what a draft answers or forwards.
#[derive(Debug, Clone)]
pub(crate) struct Routing {
    pub archive: ArchiveId,
    pub context: Context,
    pub relay: Option<Relay>,
    pub thread: Thread,
    /// Addresses that stand for the original's sender, lowercase: its `From`, the alias hit's
    /// sender and reverse alias, and its `Reply-To` only when the service signed it or it is
    /// the hit's reverse alias.
    pub senders: Vec<String>,
    /// The original's `From`, lowercase.
    pub from: Option<String>,
    /// Where the relay's strategy comes from: `alias`, `domain`, `service` or `default`.
    pub source: Option<&'static str>,
}

fn lower(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

/// What routing knows about an archived message: who it was delivered to, the account it was
/// synced from and the alias it came through.
pub(crate) async fn original_context(
    state: &ApiState,
    message: &Message,
    parsed: Option<&holtmail_mime::ParsedMessage>,
) -> ApiResult<Original> {
    let mut recipients: Vec<String> = message
        .to
        .iter()
        .chain(&message.cc)
        .map(|a| a.email.to_lowercase())
        .collect();
    if let Some(parsed) = parsed {
        for header in &parsed.headers {
            if header.name.eq_ignore_ascii_case("Delivered-To")
                || header.name.eq_ignore_ascii_case("X-Original-To")
            {
                let value = header
                    .value
                    .trim()
                    .trim_matches(|c| c == '<' || c == '>')
                    .to_lowercase();
                if value.contains('@') && !recipients.contains(&value) {
                    recipients.push(value);
                }
            }
        }
    }
    let mut account = None;
    for location in state.store.locations(message.id).await? {
        if let Some(folder) = state.store.get_folder(location.folder_id).await? {
            account = Some(folder.account_id.to_string());
            if location.removed_at.is_none() {
                break;
            }
        }
    }
    let alias = state
        .store
        .message_alias(message.id)
        .await?
        .map(|a| AliasHit {
            provider: a.provider,
            alias: a.alias,
            reverse_alias: a.reverse_alias,
            sender: a.sender.map(|s| s.email),
        });
    Ok(Original {
        recipients,
        account,
        alias,
    })
}

fn strategy(name: holtmail_alias::ReplyStrategy) -> Strategy {
    match name {
        holtmail_alias::ReplyStrategy::ReverseAlias => Strategy::ReverseAlias,
        holtmail_alias::ReplyStrategy::Direct => Strategy::Direct,
        holtmail_alias::ReplyStrategy::ReplyHeader => Strategy::ReplyHeader,
    }
}

/// The routing context of a reply (`forward` false) or forward of `message`. `reverse_alias`
/// is a reverse alias made earlier for the original's sender, used when the original carries
/// no trusted one.
pub(crate) async fn routing_of(
    state: &ApiState,
    message: &Message,
    parsed: Option<&holtmail_mime::ParsedMessage>,
    forward: bool,
    reverse_alias: Option<String>,
) -> ApiResult<Routing> {
    let archive = message.archive_id;
    let original = original_context(state, message, parsed).await?;
    let from = message.from.first().map(|a| lower(&a.email));
    let reply_to: Vec<String> = parsed
        .map(|p| p.reply_to.iter().map(|a| lower(&a.email)).collect())
        .unwrap_or_default();
    let mut senders: Vec<String> = from.iter().cloned().collect();
    let mut thread = Thread::default();
    let mut relay = None;
    let mut source = None;
    if let Some(hit) = state.store.message_alias(message.id).await? {
        if let Some(sender) = &hit.sender {
            senders.push(lower(&sender.email));
        }
        let reverse = hit.reverse_alias.as_deref().map(lower);
        if let Some(reverse) = &reverse {
            senders.push(reverse.clone());
        }
        senders.extend(
            reply_to
                .iter()
                .filter(|r| hit.signed || reverse.as_ref() == Some(*r))
                .cloned(),
        );
        thread.aliases.push(lower(&hit.alias));
        let overrides = state.store.alias_strategies(archive).await?;
        let effective = holtmail_aliases::effective_strategy(
            &overrides,
            &hit.alias,
            &hit.provider,
            state.aliases.default_strategy(&hit.provider),
        );
        source = Some(effective.source.as_str());
        let mailboxes = state
            .store
            .known_aliases_among(archive, std::slice::from_ref(&hit.alias))
            .await?
            .into_iter()
            .next()
            .map(|k| k.mailboxes)
            .unwrap_or_default();
        if hit.signed {
            thread.reverse_aliases.extend(reply_to.first().cloned());
            thread.reverse_aliases.extend(reverse);
        }
        thread.can_reverse = state
            .aliases
            .can_reverse(archive, &hit.alias)
            .await
            .unwrap_or(false);
        relay = Some(Relay {
            hit: original.alias.clone().unwrap_or_default(),
            signed: hit.signed,
            strategy: strategy(effective.strategy),
            mailboxes,
            reply_to: reply_to.first().cloned(),
            reverse_alias: reverse_alias.map(|r| lower(&r)),
        });
    }
    let mut members = state.store.thread_messages(message.thread_id).await?;
    if members.len() > THREAD_LIMIT {
        members.drain(..members.len() - THREAD_LIMIT);
    }
    for member in members {
        let Some(hit) = state.store.message_alias(member.id).await? else {
            continue;
        };
        let alias = lower(&hit.alias);
        if !thread.aliases.contains(&alias) {
            thread.aliases.push(alias);
        }
        if hit.signed
            && let Some(reverse) = hit.reverse_alias.as_deref().map(lower)
            && !thread.reverse_aliases.contains(&reverse)
        {
            thread.reverse_aliases.push(reverse);
        }
    }
    if relay.is_none()
        && let Some(alias) = thread.aliases.first()
    {
        thread.can_reverse = state
            .aliases
            .can_reverse(archive, alias)
            .await
            .unwrap_or(false);
    }
    let mut seen = Vec::new();
    senders.retain(|s| {
        let new = !seen.contains(s);
        seen.push(s.clone());
        new
    });
    Ok(Routing {
        archive,
        context: if forward {
            Context::Forward(original)
        } else {
            Context::Reply(original)
        },
        relay,
        thread,
        senders,
        from,
        source,
    })
}

/// A routing context for a new message.
pub(crate) fn new_message(archive: ArchiveId) -> Routing {
    Routing {
        archive,
        context: Context::New,
        relay: None,
        thread: Thread::default(),
        senders: Vec::new(),
        from: None,
        source: None,
    }
}

/// The route for `routing` from the user's identities and the archive's enabled transports.
/// Asks no alias service: without a usable reverse alias the answer is `needs_reverse_alias`.
pub(crate) async fn route(
    state: &ApiState,
    user: &User,
    routing: &Routing,
    chosen: Option<&str>,
) -> ApiResult<Result<Routed, RouteError>> {
    let archive = routing.archive;
    let identities: Vec<holtmail_route::Identity> = state
        .store
        .user_identities(user.id, archive)
        .await?
        .into_iter()
        .map(|i| holtmail_route::Identity {
            id: i.id.to_string(),
            address: i.fields.address,
            name: i.fields.name,
            transport: i.fields.transport_id.to_string(),
            account: i.fields.account_id.map(|a| a.to_string()),
            alias: i.fields.alias,
            active: i.fields.active,
            plus_addresses: i.fields.plus_addresses,
        })
        .collect();
    let transports: Vec<holtmail_route::Transport> = state
        .store
        .archive_transports(archive)
        .await?
        .into_iter()
        .filter(|t| t.enabled)
        .map(|t| holtmail_route::Transport {
            id: t.id.to_string(),
            allowed_from: t.allowed_from,
        })
        .collect();
    let default = state
        .store
        .default_identity(user.id, archive)
        .await?
        .map(|i| i.to_string());
    Ok(holtmail_route::route_relay(
        holtmail_route::Request {
            identities: &identities,
            transports: &transports,
            context: &routing.context,
            default_identity: default.as_deref(),
            chosen,
        },
        routing.relay.as_ref(),
    ))
}

/// [`route`], and when it needs a reverse alias, one made through the alias service or the
/// detector, then routed again. A service that refuses is `400`.
pub(crate) async fn route_creating(
    state: &ApiState,
    user: &User,
    routing: &mut Routing,
    chosen: Option<&str>,
) -> ApiResult<Result<Routed, RouteError>> {
    let first = route(state, user, routing, chosen).await?;
    let Err(RouteError::NeedsReverseAlias { alias, contact }) = &first else {
        return Ok(first);
    };
    let Some(contact) = contact.clone().or_else(|| routing.from.clone()) else {
        return Ok(first);
    };
    let made = state
        .aliases
        .reverse_alias(routing.archive, alias, &contact)
        .await
        .map_err(|error| ApiError::bad_request(format!("no reverse alias for {alias}: {error}")))?;
    if let Some(relay) = routing.relay.as_mut() {
        relay.reverse_alias = Some(lower(&made));
    }
    route(state, user, routing, chosen).await
}

/// Points a reply at the address the relay strategy chose: the first `To` entry that stands
/// for the original's sender becomes `routed.to`, later ones are dropped; with no `To` at all
/// and `add`, it is added. Other recipients stay.
pub(crate) fn apply_to(to: &mut Vec<MailboxDto>, routed: &Routed, routing: &Routing, add: bool) {
    let (Some(target), Context::Reply(_)) = (&routed.to, &routing.context) else {
        return;
    };
    let target = lower(target);
    let mut placed = false;
    to.retain_mut(|m| {
        let address = lower(&m.address);
        if address == target || routing.senders.contains(&address) {
            if placed {
                return false;
            }
            placed = true;
            m.address = target.clone();
        }
        true
    });
    if add && !placed && to.is_empty() {
        to.push(MailboxDto {
            name: None,
            address: target,
        });
    }
}

/// The recipients that would see the real mailbox, see [`holtmail_route::leaks`].
pub(crate) fn leaks(
    routing: &Routing,
    routed: &Routed,
    to: &[MailboxDto],
    cc: &[MailboxDto],
    bcc: &[MailboxDto],
) -> Vec<Leak> {
    let list = |l: &[MailboxDto]| -> Vec<String> {
        l.iter()
            .map(|m| lower(&m.address))
            .filter(|a| !a.is_empty())
            .collect()
    };
    let (to, cc, bcc) = (list(to), list(cc), list(bcc));
    holtmail_route::leaks(
        routed,
        &routing.context,
        Recipients {
            to: &to,
            cc: &cc,
            bcc: &bcc,
        },
        &routing.thread,
    )
}

/// The alias of the original, else the first alias of its thread.
pub(crate) fn thread_alias(routing: &Routing) -> Option<String> {
    routing
        .relay
        .as_ref()
        .map(|r| lower(&r.hit.alias))
        .or_else(|| routing.thread.aliases.first().cloned())
}

/// Counts `addresses` as reverse aliases of the thread for the leak guard.
pub(crate) fn trust<'a>(routing: &mut Routing, addresses: impl IntoIterator<Item = &'a str>) {
    for address in addresses {
        let address = lower(address);
        if !address.is_empty() && !routing.thread.reverse_aliases.contains(&address) {
            routing.thread.reverse_aliases.push(address);
        }
    }
}

/// The reverse aliases of `contacts` the alias service confirms now, so a stored pair counts
/// only when asking again gives the same address.
pub(crate) async fn confirmed(
    state: &ApiState,
    routing: &Routing,
    contacts: &[(String, String)],
) -> Vec<String> {
    let Some(alias) = thread_alias(routing) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (contact, address) in contacts {
        if let Ok(again) = state
            .aliases
            .reverse_alias(routing.archive, &alias, contact)
            .await
            && lower(&again) == lower(address)
        {
            out.push(lower(address));
        }
    }
    out
}
