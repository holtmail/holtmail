use bytes::Bytes;
use holtmail_pim_edit::{apply_reply, organizer};
use holtmail_schedule::{
    FromMail, Ignored, Incoming, IncomingItip, ItipMethod, ReplyOutcome, Unapplied,
};

use crate::ical::{parse, people};
use crate::prepare::normalized;

fn domain(address: &str) -> &str {
    address.rsplit('@').next().unwrap_or_default()
}

fn aligned(sender: &str, signer: &str) -> bool {
    let signer = signer.trim().trim_end_matches('.').to_ascii_lowercase();
    let domain = domain(sender);
    !signer.is_empty() && (domain == signer || domain.ends_with(&format!(".{signer}")))
}

pub(crate) fn decide(mail: &IncomingItip) -> Incoming {
    let own = normalized(&mail.own);
    let mine = |list: &[String]| normalized(list).iter().any(|a| own.contains(a));
    if !mail.arrival.live {
        return Incoming::Ignored(Ignored::NotLive);
    }
    if mail.arrival.sent_folder {
        return Incoming::Ignored(Ignored::SentFolder);
    }
    if mail.arrival.outbox_copy {
        return Incoming::Ignored(Ignored::OutboxCopy);
    }
    if mine(&mail.from) {
        return Incoming::Ignored(Ignored::OwnSender);
    }
    let Some(cal) = parse(&mail.calendar) else {
        return Incoming::Ignored(Ignored::NotItip);
    };
    let method = cal.value("METHOD").and_then(|m| ItipMethod::parse(&m));
    let uid = cal
        .events()
        .find_map(|e| e.value("UID"))
        .map(|u| u.trim().to_owned());
    let (Some(method), Some(uid)) = (method, uid.filter(|u| !u.is_empty())) else {
        return Incoming::Ignored(Ignored::NotItip);
    };
    if !mine(&mail.recipients) {
        return Incoming::Ignored(Ignored::NotAddressed);
    }
    if method != ItipMethod::Reply {
        let from_mail = if organizer(&mail.calendar).is_some_and(|o| own.contains(&o)) {
            FromMail::OwnOrganizer
        } else if mail.upstream_holds_uid {
            FromMail::UpstreamHolds
        } else {
            FromMail::Update
        };
        return Incoming::Invitation {
            method,
            uid,
            from_mail,
        };
    }
    let outcome = reply(mail, &cal, &uid, &own);
    Incoming::Reply { uid, outcome }
}

fn reply(mail: &IncomingItip, cal: &crate::ical::Comp, uid: &str, own: &[String]) -> ReplyOutcome {
    let mut named: Vec<String> = cal
        .events()
        .flat_map(|e| people(e, "ATTENDEE"))
        .map(|(a, _)| a)
        .collect();
    named.sort();
    named.dedup();
    let from = normalized(&mail.from);
    let signed = mail
        .signed_sender
        .as_deref()
        .and_then(holtmail_pim::normalize_email);
    let [attendee] = named.as_slice() else {
        return ReplyOutcome::Unapplied(Unapplied::NotFromAttendee);
    };
    let by_alias = signed.as_deref() == Some(attendee.as_str());
    if !from.contains(attendee) && !by_alias {
        return ReplyOutcome::Unapplied(Unapplied::NotFromAttendee);
    }
    let one_from = from.len() == 1;
    let authenticated = by_alias
        || (one_from
            && (mail.auth.dmarc_pass.iter().any(|d| {
                d.trim()
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(domain(attendee))
            }) || mail.auth.dkim_pass.iter().any(|d| aligned(attendee, d))));
    if !authenticated && !mail.force {
        return ReplyOutcome::Unapplied(Unapplied::Unauthenticated);
    }
    let Some(copy) = &mail.organizer_copy else {
        return ReplyOutcome::Unapplied(Unapplied::NoCopy);
    };
    let ours = parse(copy).is_some_and(|c| {
        c.events()
            .any(|e| e.value("UID").map(|u| u.trim().to_owned()).as_deref() == Some(uid))
    }) && organizer(copy).is_some_and(|o| own.contains(&o));
    if !ours {
        return ReplyOutcome::Unapplied(Unapplied::NoCopy);
    }
    let mut senders = from;
    senders.extend(signed);
    match apply_reply(copy, &mail.calendar, &senders) {
        Some(body) => ReplyOutcome::Merged {
            body: Bytes::from(body),
        },
        None => ReplyOutcome::Unapplied(Unapplied::Unchanged),
    }
}
