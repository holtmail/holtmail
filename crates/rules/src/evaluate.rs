use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveTime, SecondsFormat, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::Serialize;

use crate::catalogue::{Spec, TriggerKind};
use crate::facts::{Event, EventFacts, FactSheet, Facts, FactsError, Mailbox, MessageFacts};
use crate::params::{CounterScope, CounterWindow, Day, HeaderOp, TimeWindowParams, Trust};
use crate::plan::{Action, AutoSubmitted, Ignored, Outcome, Plan, PlannedAction, Skip, Step};
use crate::template;
use crate::validate::{CompiledNode, Issue, compile};

/// Why a flow could not be evaluated. Nothing is planned when this is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    /// The flow does not validate.
    #[error("flow is not valid ({} issues)", .0.len())]
    Invalid(Vec<Issue>),
    /// A condition could not be decided.
    #[error("node {node}: {error}")]
    Facts {
        /// The condition's node id.
        node: String,
        /// What the facts said.
        error: FactsError,
    },
    /// A counter the flow reads is not in the fact sheet.
    #[error("node {node}: counter not loaded")]
    MissingCounter {
        /// The condition's node id.
        node: String,
    },
}

/// The values a template sees, and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TemplateContext {
    /// The message's sender.
    pub sender: Option<Mailbox>,
    /// The message's subject.
    pub subject: Option<String>,
    /// The message's date, RFC 3339.
    pub date: Option<String>,
    /// The alias the message came through.
    pub alias: Option<String>,
    /// Insight fields by name.
    pub insights: BTreeMap<String, String>,
    /// The invitation the message carries.
    pub invitation: Option<EventContext>,
    /// The upcoming event.
    pub event: Option<EventContext>,
    /// When the event happened, RFC 3339.
    pub now: String,
}

/// An event as templates see it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EventContext {
    /// Title.
    pub summary: Option<String>,
    /// Start, RFC 3339.
    pub starts: Option<String>,
    /// Location.
    pub location: Option<String>,
    /// Organizer.
    pub organizer: Option<Mailbox>,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl TemplateContext {
    /// The whitelisted context for an event and its facts; the editor's preview uses the same.
    pub fn new(event: &Event, sheet: &FactSheet) -> TemplateContext {
        let ev = |e: &EventFacts| EventContext {
            summary: e.summary.clone(),
            starts: e.starts.map(rfc3339),
            location: e.location.clone(),
            organizer: e.organizer.clone(),
        };
        let m = sheet.message.as_ref();
        TemplateContext {
            sender: m.and_then(|m| m.sender.clone()),
            subject: m.and_then(|m| m.subject.clone()),
            date: m.and_then(|m| m.date).map(rfc3339),
            alias: m.and_then(|m| m.alias.as_ref()).map(|a| a.address.clone()),
            insights: m.map(|m| m.insights.clone()).unwrap_or_default(),
            invitation: sheet.invitation.as_ref().map(ev),
            event: sheet.event.as_ref().map(ev),
            now: rfc3339(event.at),
        }
    }
}

/// Evaluates a flow for one event: validates it, checks the trigger and liveness, then follows
/// the graph from the trigger, deciding each condition on `facts` and planning each action.
///
/// Pure: no I/O, the same flow, event and facts always give the same plan. Actions are only
/// planned; the server carries them out with every safety check. A condition on an event
/// without a message takes `no`. An action that cannot apply (RFC 3834 for replies, no
/// invitation for an answer, a failing template) is [`Outcome::Skipped`] and the run goes on.
/// `stop` ends the run and sets [`Plan::stopped`].
pub fn evaluate(flow: &crate::Flow, event: &Event, facts: &dyn Facts) -> Result<Plan, EvalError> {
    let compiled = compile(flow).map_err(EvalError::Invalid)?;
    let mut plan = Plan::default();
    if compiled.trigger != event.trigger {
        plan.ignored = Some(Ignored::OtherTrigger);
        return Ok(plan);
    }
    let needs_live = matches!(
        event.trigger,
        TriggerKind::MessageArchived | TriggerKind::InvitationReceived
    );
    if needs_live && !event.live {
        plan.ignored = Some(Ignored::NotLive);
        return Ok(plan);
    }
    plan.ran = true;
    let sheet = facts.sheet();
    let ctx = TemplateContext::new(event, sheet);
    let mut current = compiled.start;
    let mut budget = compiled.nodes.len() + 1;
    while let Some(id) = current {
        let Some(node) = compiled.nodes.get(id) else {
            break;
        };
        if budget == 0 {
            break;
        }
        budget -= 1;
        let (outcome, port) = match &node.spec {
            Spec::Stop => (Outcome::Stopped, None),
            spec if node.ty.ports == ["yes", "no"] => {
                let yes = decide(id, spec, event, facts)?;
                let port = if yes { "yes" } else { "no" };
                (
                    Outcome::Branch {
                        port: port.to_owned(),
                    },
                    Some(port),
                )
            }
            spec => {
                let outcome = match act(spec, event, sheet, &ctx) {
                    Ok(action) => {
                        plan.actions.push(planned(id, node, action));
                        Outcome::Planned {
                            action: plan.actions.len() - 1,
                        }
                    }
                    Err(reason) => Outcome::Skipped { reason },
                };
                (outcome, Some("next"))
            }
        };
        let stopped = outcome == Outcome::Stopped;
        plan.path.push(Step {
            node: id.to_owned(),
            kind: node.ty.id.to_owned(),
            outcome,
        });
        if stopped {
            plan.stopped = true;
            break;
        }
        current = port.and_then(|p| node.next.get(p)).map(String::as_str);
    }
    Ok(plan)
}

fn planned(id: &str, node: &CompiledNode<'_>, action: Action) -> PlannedAction {
    PlannedAction {
        node: id.to_owned(),
        capability: node.ty.capability,
        needs_confirmation: node.confirm,
        action,
    }
}

fn headers<'a>(m: &'a MessageFacts, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    m.headers
        .iter()
        .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn decide(id: &str, spec: &Spec, event: &Event, facts: &dyn Facts) -> Result<bool, EvalError> {
    let sheet = facts.sheet();
    if let Spec::TimeWindow(p) = spec {
        return Ok(in_window(p, event.at));
    }
    if let Spec::HasInvitation = spec {
        return Ok(sheet.invitation.is_some());
    }
    let Some(m) = sheet.message.as_ref() else {
        return Ok(false);
    };
    Ok(match spec {
        Spec::Query(p) => facts.matches(&p.query).map_err(|error| EvalError::Facts {
            node: id.to_owned(),
            error,
        })?,
        Spec::Header(p) => {
            let want = p.value.trim().to_lowercase();
            let mut values = headers(m, &p.name).map(|v| v.trim().to_lowercase());
            match p.op {
                HeaderOp::Exists => values.next().is_some(),
                HeaderOp::Equals => values.any(|v| v == want),
                HeaderOp::Contains => values.any(|v| v.contains(&want)),
                HeaderOp::StartsWith => values.any(|v| v.starts_with(&want)),
                HeaderOp::EndsWith => values.any(|v| v.ends_with(&want)),
            }
        }
        Spec::Alias(p) => m.alias.as_ref().is_some_and(|a| match &p.service {
            None => true,
            Some(s) => a
                .service
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(s)),
        }),
        Spec::Trust(p) => p.any_of.contains(&m.trust.unwrap_or(Trust::Unknown)),
        Spec::Insight(p) => m.insight_kinds.iter().any(|k| k == &p.kind),
        Spec::SenderIsPerson => m.sender_is_person,
        Spec::Counter(p) => {
            let count =
                counter(sheet, p.scope, p.window).ok_or_else(|| EvalError::MissingCounter {
                    node: id.to_owned(),
                })?;
            count > u64::from(p.above)
        }
        _ => false,
    })
}

fn counter(sheet: &FactSheet, scope: CounterScope, window: CounterWindow) -> Option<u64> {
    sheet
        .counters
        .iter()
        .find(|c| c.scope == scope && c.window == window)
        .map(|c| c.count)
}

fn weekday(d: Day) -> Weekday {
    match d {
        Day::Mon => Weekday::Mon,
        Day::Tue => Weekday::Tue,
        Day::Wed => Weekday::Wed,
        Day::Thu => Weekday::Thu,
        Day::Fri => Weekday::Fri,
        Day::Sat => Weekday::Sat,
        Day::Sun => Weekday::Sun,
    }
}

fn in_window(p: &TimeWindowParams, at: DateTime<Utc>) -> bool {
    let (Ok(tz), Ok(from), Ok(to)) = (
        p.timezone.parse::<Tz>(),
        NaiveTime::parse_from_str(&p.from, "%H:%M"),
        NaiveTime::parse_from_str(&p.to, "%H:%M"),
    ) else {
        return false;
    };
    let local = tz.from_utc_datetime(&at.naive_utc());
    let time = local.time();
    let (inside, day) = if from < to {
        (time >= from && time < to, local.weekday())
    } else if time >= from {
        (true, local.weekday())
    } else {
        (time < to, local.weekday().pred())
    };
    inside && (p.days.is_empty() || p.days.iter().any(|d| weekday(*d) == day))
}

fn render(field: &str, source: &str, ctx: &TemplateContext) -> Result<String, Skip> {
    template::render(source, ctx).map_err(|e| Skip::Template {
        field: field.to_owned(),
        error: e.to_string(),
    })
}

fn render_opt(
    field: &str,
    source: &Option<String>,
    ctx: &TemplateContext,
) -> Result<Option<String>, Skip> {
    source.as_deref().map(|s| render(field, s, ctx)).transpose()
}

fn message(sheet: &FactSheet) -> Result<&MessageFacts, Skip> {
    sheet.message.as_ref().ok_or(Skip::NoMessage)
}

fn sender(m: &MessageFacts) -> Result<&Mailbox, Skip> {
    m.sender
        .as_ref()
        .filter(|s| !s.address.trim().is_empty())
        .ok_or(Skip::NoSender)
}

fn own(sheet: &FactSheet, address: &str) -> bool {
    let a = address.trim().to_lowercase();
    sheet.own_addresses.iter().any(|o| o.to_lowercase() == a)
}

/// Local parts that never get an automatic reply: robots, daemons, bounces and lists.
fn no_reply(address: &str) -> bool {
    let local = address
        .rsplit_once('@')
        .map_or(address, |(l, _)| l)
        .to_lowercase();
    let squashed: String = local
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    squashed.starts_with("noreply")
        || squashed.starts_with("donotreply")
        || squashed.starts_with("dontreply")
        || squashed.starts_with("mailerdaemon")
        || squashed.starts_with("bounce")
        || matches!(
            squashed.as_str(),
            "postmaster" | "listserv" | "majordomo" | "mailer" | "notifications"
        )
        || local.starts_with("owner-")
        || local.ends_with("-request")
        || local.ends_with("-bounces")
}

fn automatic(m: &MessageFacts) -> bool {
    headers(m, "Auto-Submitted").any(|v| {
        let v = v.split(';').next().unwrap_or("").trim();
        !v.eq_ignore_ascii_case("no")
    })
}

fn rfc3834(m: &MessageFacts, sheet: &FactSheet, address: &str) -> Result<(), Skip> {
    let auto = automatic(m);
    let suppress = headers(m, "X-Auto-Response-Suppress").any(|v| {
        let v = v.to_lowercase();
        v.contains("all") || v.contains("oof") || v.contains("autoreply")
    });
    if auto || suppress {
        return Err(Skip::AutoSubmitted);
    }
    if ["List-Id", "List-Post", "List-Unsubscribe"]
        .iter()
        .any(|h| headers(m, h).next().is_some())
    {
        return Err(Skip::MailingList);
    }
    if headers(m, "Precedence")
        .any(|v| matches!(v.trim().to_lowercase().as_str(), "bulk" | "list" | "junk"))
    {
        return Err(Skip::BulkPrecedence);
    }
    if headers(m, "Return-Path").any(|v| v.trim() == "<>") {
        return Err(Skip::NullReturnPath);
    }
    if no_reply(address) {
        return Err(Skip::NoReplySender);
    }
    if own(sheet, address) {
        return Err(Skip::OwnAddress);
    }
    Ok(())
}

fn act(
    spec: &Spec,
    event: &Event,
    sheet: &FactSheet,
    ctx: &TemplateContext,
) -> Result<Action, Skip> {
    Ok(match spec {
        Spec::Label(p) => Action::Label {
            message: message(sheet)?.id.clone(),
            name: p.name.trim().to_owned(),
        },
        Spec::SetFlags(p) => Action::SetFlags {
            message: message(sheet)?.id.clone(),
            set: p.set.clone(),
            clear: p.clear.clone(),
        },
        Spec::Move(p) => Action::Move {
            message: message(sheet)?.id.clone(),
            folder: p.folder.clone(),
        },
        Spec::Delete => Action::Delete {
            message: message(sheet)?.id.clone(),
        },
        Spec::Reply(p) => {
            let m = message(sheet)?;
            let to = sender(m)?.address.trim().to_lowercase();
            rfc3834(m, sheet, &to)?;
            if let Some(last) = sheet.last_reply {
                let until = last + Duration::days(i64::from(p.days));
                if event.at < until {
                    return Err(Skip::RepliedRecently { until });
                }
            }
            Action::Reply {
                message: m.id.clone(),
                identity: p.identity.clone(),
                to,
                subject: render("subject", &p.subject, ctx)?,
                body: render("body", &p.body, ctx)?,
                days: p.days,
                auto_submitted: AutoSubmitted::AutoReplied,
            }
        }
        Spec::Forward(p) => {
            let m = message(sheet)?;
            if m.sender.as_ref().is_some_and(|s| own(sheet, &s.address)) {
                return Err(Skip::OwnAddress);
            }
            if automatic(m) {
                return Err(Skip::AutoSubmitted);
            }
            Action::Forward {
                message: m.id.clone(),
                identity: p.identity.clone(),
                to: p.to.iter().map(|a| a.trim().to_owned()).collect(),
                note: render_opt("note", &p.note, ctx)?,
                as_attachment: p.as_attachment,
                auto_submitted: AutoSubmitted::AutoGenerated,
            }
        }
        Spec::Send(p) => Action::Send {
            identity: p.identity.clone(),
            to: p.to.iter().map(|a| a.trim().to_owned()).collect(),
            subject: render("subject", &p.subject, ctx)?,
            body: render("body", &p.body, ctx)?,
            auto_submitted: AutoSubmitted::AutoGenerated,
        },
        Spec::Rsvp(p) => {
            let invitation = sheet.invitation.as_ref().ok_or(Skip::NoInvitation)?;
            Action::Rsvp {
                message: sheet.message.as_ref().map(|m| m.id.clone()),
                uid: invitation.uid.clone(),
                answer: p.answer,
                comment: render_opt("comment", &p.comment, ctx)?,
            }
        }
        Spec::CreateEvent(p) => {
            let title = render("title", &p.title, ctx)?;
            let starts = render("starts", &p.starts, ctx)?;
            let starts = DateTime::parse_from_rfc3339(starts.trim())
                .map_err(|_| Skip::BadDate)?
                .with_timezone(&Utc);
            Action::CreateEvent {
                calendar: p.calendar.clone(),
                title,
                starts,
                duration_minutes: p.duration_minutes,
            }
        }
        Spec::CreateContact(p) => {
            let m = message(sheet)?;
            let s = sender(m)?;
            if own(sheet, &s.address) {
                return Err(Skip::OwnAddress);
            }
            if m.sender_is_person {
                return Err(Skip::AlreadyKnown);
            }
            Action::CreateContact {
                address_book: p.address_book.clone(),
                name: s.name.clone(),
                address: s.address.trim().to_lowercase(),
            }
        }
        Spec::Notify(p) => Action::Notify {
            title: render("title", &p.title, ctx)?,
            body: render("body", &p.body, ctx)?,
        },
        Spec::Webhook(p) => Action::Webhook {
            hook: p.hook.clone(),
        },
        Spec::SetIdentity(p) => Action::SetIdentity {
            identity: p.identity.clone(),
        },
        Spec::SetTransport(p) => Action::SetTransport {
            transport: p.transport.clone(),
        },
        Spec::SetAliasStrategy(p) => Action::SetAliasStrategy {
            strategy: p.strategy,
        },
        Spec::AddBcc(p) => Action::AddBcc {
            address: p.address.trim().to_owned(),
        },
        Spec::RequireConfirmation(p) => Action::RequireConfirmation {
            reason: p.reason.clone(),
        },
        Spec::Block(p) => Action::Block {
            reason: p.reason.clone(),
        },
        _ => return Err(Skip::NoMessage),
    })
}
