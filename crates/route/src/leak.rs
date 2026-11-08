use serde::{Deserialize, Serialize};

use crate::{Context, Routed, Strategy, norm};

/// What the leak guard knows about the thread a draft belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// The aliases any message of the thread arrived through. Empty when none did, and then
    /// nothing leaks.
    pub aliases: Vec<String>,
    /// Addresses known to reach a contact through an alias service: the thread's reverse
    /// aliases and service `Reply-To` addresses, and those in the alias store.
    pub reverse_aliases: Vec<String>,
    /// A reverse alias can be made for a new contact: the directory can create one or the
    /// detector encodes it in the address.
    pub can_reverse: bool,
}

/// The final recipients of a draft.
#[derive(Debug, Clone, Copy, Default)]
pub struct Recipients<'a> {
    /// `To` addresses.
    pub to: &'a [String],
    /// `Cc` addresses.
    pub cc: &'a [String],
    /// `Bcc` addresses.
    pub bcc: &'a [String],
}

/// The recipient field an address was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// `To`.
    To,
    /// `Cc`.
    Cc,
    /// `Bcc`.
    Bcc,
}

/// Why a recipient would see the real mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakReason {
    /// A reply all that mixes reverse aliases with plain addresses: the service hides the real
    /// mailbox only from the reverse aliases.
    MixedRecipients,
    /// A `Cc` that is not a reverse alias, on mail sent from the real mailbox.
    CcNotReverseAlias,
    /// A `Bcc` that is not a reverse alias, on mail sent from the real mailbox.
    BccNotReverseAlias,
    /// A forward of an alias thread from the real mailbox to a plain address.
    ForwardFromRealAddress,
    /// Mail in an alias thread sent from the real mailbox to a plain address.
    RealAddressFrom,
}

/// The suggested one-click fix for a leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fix {
    /// Send to this recipient through a reverse alias of the service instead.
    UseReverseAlias,
    /// Send from the alias instead of the real mailbox.
    UseAliasIdentity,
    /// Take the recipient off: the service cannot reach it without showing the real mailbox.
    RemoveRecipient,
}

/// One recipient that would see the real mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leak {
    /// The recipient, lowercase.
    pub recipient: String,
    /// The first field it appears in, in order `To`, `Cc`, `Bcc`.
    pub field: Field,
    /// Why it would see the real mailbox.
    pub reason: LeakReason,
    /// The suggested fix.
    pub fix: Fix,
}

/// The recipients that would see the real mailbox if the draft were sent on `routed`.
///
/// Nothing leaks when neither the thread nor the route names an alias ([`Thread::aliases`] is
/// empty and [`Routed::alias`] is `None`), or when `From` is one of those aliases, whatever the
/// strategy. Otherwise `From` is a real mailbox and every recipient that is not a reverse alias
/// (one of [`Thread::reverse_aliases`] or the route's `to`) and not one of the aliases leaks,
/// once, in the first field it appears in. The reason is [`LeakReason::ForwardFromRealAddress`] for a forward,
/// [`LeakReason::CcNotReverseAlias`] or [`LeakReason::BccNotReverseAlias`] outside `To`,
/// [`LeakReason::MixedRecipients`] in `To` next to a reverse alias, and
/// [`LeakReason::RealAddressFrom`] otherwise. The fix is [`Fix::UseAliasIdentity`] under
/// [`Strategy::Direct`], else [`Fix::UseReverseAlias`] when [`Thread::can_reverse`] and
/// [`Fix::RemoveRecipient`] when not. The result follows the recipients' order.
pub fn leaks(
    routed: &Routed,
    context: &Context,
    recipients: Recipients<'_>,
    thread: &Thread,
) -> Vec<Leak> {
    let aliases: Vec<String> = thread
        .aliases
        .iter()
        .map(|a| norm(a))
        .chain(routed.alias.as_deref().map(norm))
        .collect();
    if aliases.is_empty() || aliases.contains(&norm(&routed.route.from)) {
        return Vec::new();
    }
    let direct = routed.route.strategy == Strategy::Direct;
    let reverse: Vec<String> = thread
        .reverse_aliases
        .iter()
        .map(|a| norm(a))
        .chain(routed.to.as_deref().map(norm))
        .collect();
    let fields = [
        (Field::To, recipients.to),
        (Field::Cc, recipients.cc),
        (Field::Bcc, recipients.bcc),
    ];
    let all: Vec<(Field, String)> = fields
        .iter()
        .flat_map(|(f, list)| list.iter().map(move |a| (*f, norm(a))))
        .collect();
    let mixed = all.iter().any(|(_, a)| reverse.contains(a));
    let forward = matches!(context, Context::Forward(_));
    let fix = if direct {
        Fix::UseAliasIdentity
    } else if thread.can_reverse {
        Fix::UseReverseAlias
    } else {
        Fix::RemoveRecipient
    };
    let mut out: Vec<Leak> = Vec::new();
    for (field, recipient) in all {
        if reverse.contains(&recipient)
            || aliases.contains(&recipient)
            || out.iter().any(|l| l.recipient == recipient)
        {
            continue;
        }
        let reason = match field {
            _ if forward => LeakReason::ForwardFromRealAddress,
            Field::Cc => LeakReason::CcNotReverseAlias,
            Field::Bcc => LeakReason::BccNotReverseAlias,
            Field::To if mixed => LeakReason::MixedRecipients,
            Field::To => LeakReason::RealAddressFrom,
        };
        out.push(Leak {
            recipient,
            field,
            reason,
            fix,
        });
    }
    out
}
