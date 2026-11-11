use std::collections::HashMap;

use bytes::Bytes;
use holtmail_collection::{Partstat, WriteCaps};
use holtmail_compose::{Answer, ImipKind, Invitation, Mailbox};
use holtmail_core::{IdentityId, Timestamp};
use holtmail_pim::Method;
use holtmail_pim_edit::{
    Imip, ImipError, OrganizerChange, OrganizerRoute, ReplyRoute, ScheduleAgent, organize,
    organizer_route, reply, reply_agent, reply_route,
};
use holtmail_schedule::{
    Change, Delivery, Host, HostCaps, ItipKey, ItipMethod, Origin, RECENT, RecipientStatus, Role,
    ScheduleError, ScheduleStatus, Way,
};
use holtmail_submission::{SubmissionError, SubmissionState, Submit};

use crate::ItipScheduler;
use crate::ical::{Comp, param, parse, people, set_param, unfold};
use crate::prepare::{normalized, own_partstat, role};

fn host(host: Host) -> holtmail_pim_edit::Host {
    match host {
        Host::CalDav { auto_schedule } => holtmail_pim_edit::Host::CalDav { auto_schedule },
        Host::Graph => holtmail_pim_edit::Host::Graph,
        Host::Jmap { scheduling } => holtmail_pim_edit::Host::Jmap { scheduling },
        Host::Mail => holtmail_pim_edit::Host::Mail,
    }
}

fn caps(caps: HostCaps) -> WriteCaps {
    WriteCaps {
        create: caps.create,
        update: caps.update,
        delete: caps.delete,
        respond: caps.respond,
        ..WriteCaps::default()
    }
}

fn rank(status: ScheduleStatus) -> u8 {
    match status {
        ScheduleStatus::Sent | ScheduleStatus::Delivered => 0,
        ScheduleStatus::Pending => 1,
        ScheduleStatus::NoRoute => 2,
        ScheduleStatus::Failed => 3,
    }
}

fn method(m: Method) -> ItipMethod {
    match m {
        Method::Reply => ItipMethod::Reply,
        Method::Cancel => ItipMethod::Cancel,
        _ => ItipMethod::Request,
    }
}

fn readable(raw: &Option<Bytes>) -> Result<Option<Comp>, ScheduleError> {
    raw.as_deref()
        .map(|b| parse(b).ok_or_else(|| ScheduleError::Invalid("not a readable event".into())))
        .transpose()
}

#[derive(Clone)]
struct Sender {
    id: IdentityId,
    from: String,
    name: Option<String>,
    route: Option<holtmail_route::Route>,
}

struct Run<'a> {
    scheduler: &'a ItipScheduler,
    change: &'a Change,
    delivery: Delivery,
    senders: HashMap<String, Result<Sender, ScheduleStatus>>,
    marks: Vec<(String, ScheduleStatus)>,
}

impl Run<'_> {
    fn mark(&mut self, address: &str, status: ScheduleStatus) {
        match self.marks.iter_mut().find(|(a, _)| a == address) {
            Some((_, held)) if rank(status) > rank(*held) => *held = status,
            Some(_) => {}
            None => self.marks.push((address.to_owned(), status)),
        }
    }

    async fn sender(
        &mut self,
        from: &str,
    ) -> Result<Result<Sender, ScheduleStatus>, ScheduleError> {
        if let Some(known) = self.senders.get(from) {
            return Ok(known.clone());
        }
        let change = self.change;
        let found = match change
            .reply_as
            .as_ref()
            .filter(|r| r.route.from.eq_ignore_ascii_case(from))
        {
            Some(r) => r
                .route
                .identity
                .parse::<IdentityId>()
                .map(|id| Sender {
                    id,
                    from: r.route.from.clone(),
                    name: r.route.name.clone(),
                    route: Some(r.route.clone()),
                })
                .map_err(|_| ScheduleStatus::NoRoute),
            None => match self
                .scheduler
                .submission
                .sender(&change.actor, change.archive, from)
                .await
            {
                Ok(identity) => Ok(Sender {
                    id: identity.id,
                    from: from.to_owned(),
                    name: identity.name,
                    route: None,
                }),
                Err(SubmissionError::Forbidden(_)) => Err(ScheduleStatus::NoRoute),
                Err(SubmissionError::Unavailable(e)) => return Err(ScheduleError::Unavailable(e)),
                Err(_) => Err(ScheduleStatus::Failed),
            },
        };
        self.senders.insert(from.to_owned(), found.clone());
        Ok(found)
    }

    async fn send(
        &mut self,
        from: &str,
        imip: &Imip,
        recipients: &[String],
        kind: ImipKind,
    ) -> Result<ScheduleStatus, ScheduleError> {
        let scheduler = self.scheduler;
        let change = self.change;
        let partstat = match &kind {
            ImipKind::Reply(answer) => Some(
                match answer {
                    Answer::Accepted => "ACCEPTED",
                    Answer::Tentative => "TENTATIVE",
                    Answer::Declined => "DECLINED",
                }
                .to_owned(),
            ),
            _ => None,
        };
        let digest = digest(imip, recipients);
        if scheduler
            .was_sent(&change.actor, change.archive, &digest)
            .await
        {
            self.delivery.suppressed += 1;
            return Ok(ScheduleStatus::Sent);
        }
        let sender = match self.sender(from).await? {
            Ok(sender) => sender,
            Err(status) => return Ok(status),
        };
        let answering = matches!(kind, ImipKind::Reply(_));
        let mut draft = holtmail_compose::imip(
            Mailbox {
                name: sender.name.clone(),
                address: sender.from.clone(),
            },
            recipients.iter().cloned().map(Mailbox::new).collect(),
            &Invitation {
                kind,
                calendar: imip.calendar.clone(),
                summary: imip.summary.clone(),
                when: imip.when.clone(),
                location: imip.location.clone(),
                comment: change.comment.clone().filter(|_| answering),
            },
            None,
        );
        if answering {
            let references = holtmail_compose::usable_references(&change.references);
            draft.in_reply_to = references.last().cloned();
            draft.references = references;
        }
        let now = scheduler.now();
        draft.date = Some(Timestamp::from_millis(now));
        let Ok(built) = holtmail_compose::build(&draft) else {
            return Ok(ScheduleStatus::Failed);
        };
        let bucket = now.div_euclid(RECENT.as_millis() as i64);
        let message = Bytes::from(built.bytes);
        let mut queued = None;
        for _ in 0..MAX_TRIES {
            let generation = scheduler.generation(&digest);
            let request = Submit {
                actor: change.actor.clone(),
                archive: change.archive,
                identity: sender.id,
                mail_from: sender.from.clone(),
                recipients: built.recipients.clone(),
                message: message.clone(),
                send_at: None,
                undo_window: scheduler.undo_window,
                idempotency_key: Some(format!("itip:{}:{bucket}:{generation}", hex(&digest))),
                allow_leaks: false,
                route: sender.route.clone(),
                hold: true,
            };
            match scheduler.submission.queue(request).await {
                Ok(submitted) if submitted.state == SubmissionState::Cancelled => {
                    scheduler.bump(&digest);
                }
                other => {
                    queued = Some(other);
                    break;
                }
            }
        }
        let Some(queued) = queued else {
            return Ok(ScheduleStatus::Failed);
        };
        match queued {
            Ok(submitted) => {
                self.delivery.queued.push(submitted.id);
                scheduler.remember(
                    change.archive,
                    digest,
                    ItipKey {
                        uid: imip.uid.clone(),
                        sequence: imip.sequence,
                        method: method(imip.method),
                        recurrence_id: imip.recurrence_id.as_deref().map(|r| r.trim().to_owned()),
                        partstat,
                    },
                    recipients.to_vec(),
                    submitted.id,
                );
                Ok(ScheduleStatus::Sent)
            }
            Err(
                SubmissionError::Forbidden(_)
                | SubmissionError::FromMismatch
                | SubmissionError::Leak,
            ) => Ok(ScheduleStatus::NoRoute),
            Err(SubmissionError::Unavailable(e)) => Err(ScheduleError::Unavailable(e)),
            Err(_) => Ok(ScheduleStatus::Failed),
        }
    }
}

/// How many idempotency keys a message tries when the outbox already holds the earlier ones as
/// cancelled.
const MAX_TRIES: usize = 4;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn digest(imip: &Imip, recipients: &[String]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    let mut sorted = recipients.to_vec();
    sorted.sort();
    for part in [
        method(imip.method).as_str(),
        &imip.uid,
        &imip.sequence.to_string(),
        imip.recurrence_id.as_deref().unwrap_or(""),
        &sorted.join(","),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    for line in unfold(&imip.calendar)
        .iter()
        .filter(|l| !l.to_ascii_uppercase().starts_with("DTSTAMP"))
    {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    *hasher.finalize().as_bytes()
}

fn agents(copies: &[&Comp]) -> HashMap<String, ScheduleAgent> {
    let mut out = HashMap::new();
    for copy in copies {
        for event in copy.events() {
            for (address, params) in people(event, "ATTENDEE") {
                out.insert(
                    address,
                    ScheduleAgent::parse(param(&params, "SCHEDULE-AGENT")),
                );
            }
        }
    }
    out
}

/// `raw` with `SCHEDULE-STATUS` on every `name` line set from `statuses`, else kept from
/// `previous`, else removed; `None` when nothing changes.
fn write_statuses(
    raw: &[u8],
    previous: Option<&Comp>,
    name: &str,
    statuses: &[(String, ScheduleStatus)],
) -> Option<Vec<u8>> {
    let mut cal = parse(raw)?;
    let mut before: HashMap<String, String> = HashMap::new();
    for event in previous.iter().flat_map(|p| p.events()) {
        for (address, params) in people(event, name) {
            if let Some(code) = param(&params, "SCHEDULE-STATUS") {
                before.entry(address).or_insert_with(|| code.to_owned());
            }
        }
    }
    let changed = cal.edit_params(name, &mut |value, params| {
        let Some(address) = holtmail_pim::normalize_email(value) else {
            return;
        };
        let code = statuses
            .iter()
            .find(|(a, _)| *a == address)
            .map(|(_, s)| s.as_str().to_owned())
            .or_else(|| before.get(&address).cloned());
        set_param(params, "SCHEDULE-STATUS", code.as_deref());
    });
    changed.then(|| cal.to_bytes())
}

fn order(copies: &[&Comp], marks: &mut [(String, ScheduleStatus)]) {
    let mut seen: Vec<String> = Vec::new();
    for copy in copies {
        for event in copy.events() {
            for (address, _) in people(event, "ATTENDEE") {
                if !seen.contains(&address) {
                    seen.push(address);
                }
            }
        }
    }
    marks.sort_by_key(|(a, _)| seen.iter().position(|s| s == a).unwrap_or(usize::MAX));
}

pub(crate) async fn deliver(
    scheduler: &ItipScheduler,
    change: &Change,
) -> Result<Delivery, ScheduleError> {
    let previous = readable(&change.previous)?;
    let next = readable(&change.next)?;
    let own = normalized(&change.own);
    let role = next
        .as_ref()
        .or(previous.as_ref())
        .map_or(Role::None, |c| role(c, &own));
    let mut run = Run {
        scheduler,
        change,
        delivery: Delivery {
            body: change.next.clone(),
            rewritten: false,
            role,
            way: Way::Nothing,
            statuses: Vec::new(),
            queued: Vec::new(),
            suppressed: 0,
        },
        senders: HashMap::new(),
        marks: Vec::new(),
    };
    if change.origin == Origin::Itip || role == Role::None {
        return Ok(run.delivery);
    }
    let outcome = match role {
        Role::Organizer => organizer(&mut run, previous.as_ref(), next.as_ref()).await,
        _ => attendee(&mut run, previous.as_ref(), next.as_ref(), &own).await,
    };
    if let Err(error) = outcome {
        let queued = std::mem::take(&mut run.delivery.queued);
        let _ = scheduler.undo_all(&change.actor, &queued).await;
        return Err(error);
    }
    let mut delivery = run.delivery;
    delivery.statuses = run
        .marks
        .into_iter()
        .map(|(address, status)| RecipientStatus { address, status })
        .collect();
    Ok(delivery)
}

async fn organizer(
    run: &mut Run<'_>,
    previous: Option<&Comp>,
    next: Option<&Comp>,
) -> Result<(), ScheduleError> {
    let change = run.change;
    let kind = match (previous, next) {
        (None, _) => OrganizerChange::Create,
        (Some(_), None) => OrganizerChange::Delete,
        (Some(_), Some(_)) => OrganizerChange::Update,
    };
    let route = organizer_route(host(change.host), &caps(change.caps), kind);
    let organized = organize(
        change.previous.as_deref(),
        change.next.as_deref(),
        false,
        run.scheduler.now(),
    )
    .map_err(|e| ScheduleError::Invalid(e.to_string()))?;
    let copies: Vec<&Comp> = next.into_iter().chain(previous).collect();
    let agents = agents(&copies.iter().rev().copied().collect::<Vec<_>>());
    let organizer = organized
        .messages
        .first()
        .map(|m| m.organizer.clone())
        .unwrap_or_default();
    for imip in &organized.messages {
        let mut mail = Vec::new();
        for r in &imip.recipients {
            let client = agents.get(r) == Some(&ScheduleAgent::Client);
            match (route, change.origin, client) {
                (_, Origin::Device, true) => {}
                (OrganizerRoute::Imip, _, _) | (_, _, true) => mail.push(r.clone()),
                (OrganizerRoute::Server, _, _) => run.mark(r, ScheduleStatus::Sent),
                (OrganizerRoute::Deferred, _, _) => run.mark(r, ScheduleStatus::Pending),
            }
        }
        if mail.is_empty() {
            continue;
        }
        let kind = match (imip.method, kind) {
            (Method::Cancel, _) => ImipKind::Cancel,
            (_, OrganizerChange::Create) => ImipKind::Invitation,
            _ => ImipKind::Update,
        };
        let status = run.send(&organizer, imip, &mail, kind).await?;
        for r in &mail {
            run.mark(r, status);
        }
    }
    if !run.marks.is_empty() {
        run.delivery.way = match route {
            OrganizerRoute::Server => Way::Server,
            OrganizerRoute::Deferred => Way::Deferred,
            OrganizerRoute::Imip => Way::Mail,
        };
    }
    order(&copies, &mut run.marks);
    if let Some(next_raw) = &change.next {
        let base = organized.event.unwrap_or_else(|| next_raw.to_vec());
        let body = write_statuses(&base, previous, "ATTENDEE", &run.marks).unwrap_or(base);
        run.delivery.rewritten = body.as_slice() != next_raw.as_ref();
        run.delivery.body = Some(if run.delivery.rewritten {
            Bytes::from(body)
        } else {
            next_raw.clone()
        });
    }
    Ok(())
}

fn answers(
    previous: Option<&Comp>,
    next: Option<&Comp>,
    me: &str,
) -> Vec<(Option<String>, String)> {
    let series = |c: &Comp| c.events().find(|e| e.rid().is_empty()).cloned();
    let Some(next) = next else {
        let held = previous.and_then(series).and_then(|s| own_partstat(&s, me));
        return if held.as_deref() == Some("DECLINED") {
            Vec::new()
        } else {
            vec![(None, "DECLINED".to_owned())]
        };
    };
    let mut out = Vec::new();
    for event in next.events() {
        let rid = event.rid();
        let Some(now) = own_partstat(event, me) else {
            continue;
        };
        let before = previous.and_then(|p| {
            p.events()
                .find(|e| e.rid() == rid)
                .or_else(|| {
                    p.events()
                        .find(|e| e.rid().is_empty())
                        .filter(|_| !rid.is_empty())
                })
                .and_then(|e| own_partstat(e, me))
        });
        if before.as_deref() != Some(now.as_str()) && now != "NEEDS-ACTION" {
            out.push(((!rid.is_empty()).then_some(rid), now));
        }
    }
    out
}

async fn attendee(
    run: &mut Run<'_>,
    previous: Option<&Comp>,
    next: Option<&Comp>,
    own: &[String],
) -> Result<(), ScheduleError> {
    let change = run.change;
    let Some(event) = next.or(previous) else {
        return Ok(());
    };
    let me = match &change.reply_as {
        Some(r) => holtmail_pim::normalize_email(&r.attendee),
        None => event
            .events()
            .flat_map(|e| people(e, "ATTENDEE"))
            .map(|(a, _)| a)
            .find(|a| own.contains(a)),
    };
    let Some(me) = me else {
        return Ok(());
    };
    let changes = answers(previous, next, &me);
    if changes.is_empty() {
        return Ok(());
    }
    let invitation = change
        .next
        .as_ref()
        .or(change.previous.as_ref())
        .cloned()
        .unwrap_or_default();
    let organizer = event
        .events()
        .find_map(|e| people(e, "ORGANIZER").into_iter().next())
        .map(|(a, _)| a)
        .unwrap_or_default();
    match reply_route(
        host(change.host),
        &caps(change.caps),
        reply_agent(&invitation),
    ) {
        ReplyRoute::Server => {
            run.delivery.way = Way::Server;
            run.mark(&organizer, ScheduleStatus::Sent);
        }
        ReplyRoute::Quiet { .. } => run.delivery.way = Way::Quiet,
        ReplyRoute::Imip { .. } => {
            run.delivery.way = Way::Mail;
            let from = change
                .reply_as
                .as_ref()
                .map(|r| r.route.from.clone())
                .unwrap_or_else(|| me.clone());
            for (rid, answer) in changes {
                let (partstat, answer) = match Partstat::parse(&answer) {
                    Some(p @ Partstat::Accepted) => (p, Answer::Accepted),
                    Some(p @ Partstat::Tentative) => (p, Answer::Tentative),
                    Some(p @ Partstat::Declined) => (p, Answer::Declined),
                    _ => continue,
                };
                let imip = match reply(
                    &invitation,
                    &me,
                    partstat,
                    rid.as_deref(),
                    change.comment.as_deref(),
                    run.scheduler.now(),
                ) {
                    Ok(imip) => imip,
                    Err(ImipError::NotAttendee) => {
                        run.mark(&organizer, ScheduleStatus::NoRoute);
                        continue;
                    }
                    Err(_) => {
                        run.mark(&organizer, ScheduleStatus::Failed);
                        continue;
                    }
                };
                let recipients = match change.reply_as.as_ref().and_then(|r| r.to.as_deref()) {
                    Some(to) => {
                        vec![holtmail_pim::normalize_email(to).unwrap_or_else(|| to.to_owned())]
                    }
                    None => imip.recipients.clone(),
                };
                let status = run
                    .send(&from, &imip, &recipients, ImipKind::Reply(answer))
                    .await?;
                run.mark(&organizer, status);
            }
        }
    }
    if let Some(next_raw) = &change.next
        && let Some(body) = write_statuses(next_raw, previous, "ORGANIZER", &run.marks)
    {
        run.delivery.rewritten = body.as_slice() != next_raw.as_ref();
        run.delivery.body = Some(Bytes::from(body));
    }
    Ok(())
}
