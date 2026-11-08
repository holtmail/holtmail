use std::collections::{BTreeMap, BTreeSet};

use chrono::DateTime;
use holtmail_collection::Partstat;
use holtmail_pim::{ItemKind, Method, normalize_email};

use crate::form::unescape;
use crate::lines::{Component, Line, escape, parameter, split_outside_quotes};
use crate::model::{self, ANSWER_PARAMS, Category, Item, Part, address_key, category};
use crate::schedule::ScheduleAgent;

const PRODID: &str = "PRODID:-//Holtmail//iMIP//EN";
const SCHEDULE_PARAMS: &[&str] = &["SCHEDULE-AGENT", "SCHEDULE-STATUS", "SCHEDULE-FORCE-SEND"];
const SIGNIFICANT: &[&str] = &[
    "DTSTART", "DTEND", "DURATION", "DUE", "RRULE", "RDATE", "EXDATE", "EXRULE", "STATUS",
];
const RESCHEDULING: &[&str] = &[
    "DTSTART", "DTEND", "DURATION", "DUE", "RRULE", "RDATE", "EXRULE",
];

/// One iTIP message to send by mail (RFC 5546, RFC 6047): the `text/calendar` body and what the
/// mail around it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imip {
    /// `REPLY`, `REQUEST` or `CANCEL`.
    pub method: Method,
    /// The iCalendar object with `METHOD`, CRLF line endings, folded at 75 octets.
    pub calendar: Vec<u8>,
    /// The event's `UID`.
    pub uid: String,
    /// The `SEQUENCE` the message carries (of the series, or of the one instance it is about).
    pub sequence: u32,
    /// The `RECURRENCE-ID` value when the message is about one instance only.
    pub recurrence_id: Option<String>,
    /// The organizer's address, normalized (lowercase, no `mailto:`).
    pub organizer: String,
    /// Who the mail goes to, normalized, each once, in the order of the event: the organizer for
    /// a `REPLY`, attendees for a `REQUEST` or `CANCEL`. Never empty.
    pub recipients: Vec<String>,
    /// The event's title, unescaped.
    pub summary: Option<String>,
    /// The start as plain text for the readable part, such as `2026-12-01 10:00 (Europe/Berlin)`.
    pub when: Option<String>,
    /// The location, unescaped.
    pub location: Option<String>,
}

/// Why an iTIP message could not be built. Messages never carry addresses or content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImipError {
    /// The bytes are not one readable `VCALENDAR` with events.
    #[error("not a readable event")]
    Unreadable,
    /// The event has no `ORGANIZER` with an email address: it is not a scheduling object.
    #[error("the event has no organizer")]
    NoOrganizer,
    /// The answering address is not an attendee of the event or the instance.
    #[error("the address is not an attendee")]
    NotAttendee,
    /// The instance is neither a changed instance of the event nor can it be made from the
    /// series, or the `RECURRENCE-ID` value is malformed.
    #[error("the event has no such instance")]
    NoInstance,
    /// Nobody is left to send the message to.
    #[error("nobody to send to")]
    NoRecipients,
}

/// Which attendees an organizer's message goes to. Attendees whose `SCHEDULE-AGENT` is `NONE`
/// and the organizer itself never get one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience<'a> {
    /// Every attendee: Holtmail schedules ([`crate::OrganizerRoute::Imip`]).
    Everyone,
    /// Only attendees whose `SCHEDULE-AGENT` is `CLIENT`: the server sends to the others
    /// ([`crate::OrganizerRoute::Server`]).
    ClientScheduled,
    /// Only these addresses (any spelling `normalize_email` accepts), when they are attendees.
    Only(&'a [String]),
}

/// What a `CANCEL` calls off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelScope<'a> {
    /// The whole event.
    Series,
    /// One instance, by its `RECURRENCE-ID` value as iCalendar writes it (`20261208T100000Z`,
    /// `20261208T110000` in the series' zone, or `20261208` for all-day events).
    Instance(&'a str),
    /// The event for these attendees only: they were removed from it.
    Attendees(&'a [String]),
}

/// The result of [`organize`]: the organizer's copy to store and the messages to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Organized {
    /// The new copy with `SEQUENCE` raised and answers reset where the RFC asks; the bytes as
    /// given when nothing needed it; `None` for a delete.
    pub event: Option<Vec<u8>>,
    /// The messages, cancellations first, then the request.
    pub messages: Vec<Imip>,
}

fn stamp(millis: i64) -> String {
    DateTime::from_timestamp_millis(millis)
        .unwrap_or_default()
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn line(raw: String) -> Option<Line> {
    Line::parse(&raw)
}

pub(crate) fn edit_params(raw: &str, drop: &[&str], add: &[(&str, &str)]) -> String {
    let mut quoted = false;
    let mut colon = raw.len();
    for (at, c) in raw.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = at;
                break;
            }
            _ => {}
        }
    }
    let (head, value) = raw.split_at(colon);
    let mut parts = split_outside_quotes(head, ';').into_iter();
    let mut out = parts.next().unwrap_or_default().trim().to_owned();
    for part in parts.filter(|p| !p.trim().is_empty()) {
        let key = part
            .split('=')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase();
        if drop.contains(&key.as_str()) || add.iter().any(|(k, _)| *k == key) {
            continue;
        }
        out.push(';');
        out.push_str(part);
    }
    for (key, v) in add {
        out.push_str(&format!(";{key}={}", parameter(v)));
    }
    out.push_str(value);
    out
}

fn cleaned(l: &Line) -> Option<Line> {
    line(edit_params(&l.raw, SCHEDULE_PARAMS, &[]))
}

fn sequence(component: &Component) -> u32 {
    component
        .line("SEQUENCE")
        .and_then(|l| l.value.trim().parse().ok())
        .unwrap_or(0)
}

fn rid(component: &Component) -> Option<String> {
    component
        .line("RECURRENCE-ID")
        .map(|l| l.value.trim().to_owned())
}

fn parse(raw: &[u8]) -> Result<Item, ImipError> {
    let item = Item::parse(ItemKind::Event, raw).ok_or(ImipError::Unreadable)?;
    if item.components().is_empty() {
        return Err(ImipError::Unreadable);
    }
    Ok(item)
}

fn master(item: &Item) -> Option<&Component> {
    item.components()
        .into_iter()
        .find(|c| c.line("RECURRENCE-ID").is_none())
}

fn organizer_line(item: &Item) -> Option<&Line> {
    master(item).and_then(|c| c.line("ORGANIZER")).or_else(|| {
        item.components()
            .into_iter()
            .find_map(|c| c.line("ORGANIZER"))
    })
}

fn organizer_address(item: &Item) -> Result<String, ImipError> {
    organizer_line(item)
        .and_then(|l| normalize_email(&l.value))
        .ok_or(ImipError::NoOrganizer)
}

/// The organizer's address of an event, normalized; `None` when it has none or is unreadable.
pub fn organizer(raw: &[u8]) -> Option<String> {
    organizer_address(&parse(raw).ok()?).ok()
}

/// The `SCHEDULE-AGENT` on the event's `ORGANIZER`, which decides who sends an attendee's
/// answer (RFC 6638 section 7.1); [`ScheduleAgent::Server`] when absent or unreadable.
pub fn reply_agent(raw: &[u8]) -> ScheduleAgent {
    let Ok(item) = parse(raw) else {
        return ScheduleAgent::Server;
    };
    ScheduleAgent::parse(organizer_line(&item).and_then(|l| l.param("SCHEDULE-AGENT")))
}

pub(crate) fn valid_rid(value: &str) -> bool {
    crate::instance::well_formed(value)
}

fn instance_lines(series: &Component, value: &str) -> Vec<Line> {
    let params = crate::instance::series_params(series, value);
    let mut out: Vec<Line> = [
        format!("RECURRENCE-ID{params}:{value}"),
        format!("DTSTART{params}:{value}"),
    ]
    .into_iter()
    .filter_map(line)
    .collect();
    out.extend(series.line("DURATION").cloned());
    out
}

fn find_instance<'a>(item: &'a Item, value: &str) -> Option<&'a Component> {
    let key = crate::instance::key_of_value(item, value)?;
    let at = crate::instance::position(item, &key)?;
    item.top.children.get(at)
}

fn describe(start: &Line) -> String {
    let v = start.value.trim();
    if !v.is_ascii() {
        return v.to_owned();
    }
    let date = |d: &str| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]);
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if v.len() == 8 && digits(v) {
        return date(v);
    }
    if v.len() >= 15 && v.as_bytes()[8] == b'T' && digits(&v[..8]) && digits(&v[9..15]) {
        let mut out = format!("{} {}:{}", date(v), &v[9..11], &v[11..13]);
        if v.ends_with('Z') {
            out.push_str(" UTC");
        } else if let Some(tz) = start.param("TZID") {
            out.push_str(&format!(" ({tz})"));
        }
        return out;
    }
    v.to_owned()
}

fn shown(source: &Component, fallback: Option<&Component>, name: &str) -> Option<String> {
    source
        .line(name)
        .or_else(|| fallback.and_then(|f| f.line(name)))
        .map(|l| unescape(l.value.trim()))
        .filter(|s| !s.is_empty())
}

fn zones_for(item: &Item, events: &[Component]) -> Vec<Component> {
    let mut wanted = Vec::new();
    for event in events {
        model::tzids(event, &mut wanted);
    }
    item.top
        .children
        .iter()
        .filter(|c| c.name == "VTIMEZONE")
        .filter(|c| {
            c.line("TZID")
                .is_some_and(|l| wanted.contains(&l.value.trim().to_owned()))
        })
        .cloned()
        .collect()
}

fn calendar(method: &str, item: &Item, events: Vec<Component>) -> Vec<u8> {
    let mut children = zones_for(item, &events);
    children.extend(events);
    let top = Component {
        name: "VCALENDAR".into(),
        lines: [
            "VERSION:2.0".to_owned(),
            PRODID.to_owned(),
            "CALSCALE:GREGORIAN".to_owned(),
            format!("METHOD:{method}"),
        ]
        .into_iter()
        .filter_map(line)
        .collect(),
        children,
    };
    top.text().into_bytes()
}

fn first_shown(
    events: &[Component],
    item: &Item,
) -> (Option<String>, Option<String>, Option<String>) {
    let series = master(item);
    let Some(first) = events.first() else {
        return (None, None, None);
    };
    (
        shown(first, series, "SUMMARY"),
        first
            .line("DTSTART")
            .or_else(|| series.and_then(|s| s.line("DTSTART")))
            .map(describe),
        shown(first, series, "LOCATION"),
    )
}

fn agent_of(attendee: &Line) -> ScheduleAgent {
    ScheduleAgent::parse(attendee.param("SCHEDULE-AGENT"))
}

fn recipients(components: &[&Component], organizer: &str, audience: Audience<'_>) -> Vec<String> {
    let only: Option<BTreeSet<String>> = match audience {
        Audience::Only(list) => Some(list.iter().map(|a| address_key(a)).collect()),
        _ => None,
    };
    let mut out: Vec<String> = Vec::new();
    for attendee in components
        .iter()
        .flat_map(|c| c.lines.iter().filter(|l| l.name == "ATTENDEE"))
    {
        let Some(address) = normalize_email(&attendee.value) else {
            continue;
        };
        let agent = agent_of(attendee);
        let wanted = address != organizer
            && agent != ScheduleAgent::None
            && match audience {
                Audience::Everyone => true,
                Audience::ClientScheduled => agent == ScheduleAgent::Client,
                Audience::Only(_) => only.as_ref().is_some_and(|o| o.contains(&address)),
            };
        if wanted && !out.contains(&address) {
            out.push(address);
        }
    }
    out
}

/// An attendee's iMIP `REPLY` (RFC 5546 section 3.2.3) to the invitation `invitation` (the
/// account's copy or the `REQUEST` from mail), answering `answer` as `attendee` (for an
/// invitation that arrived through an alias, the alias: `holtmail_route::attendee_address`).
///
/// Guarantees:
/// - `recurrence_id` `None` answers the series, or every changed instance when the invitation
///   holds only those; `Some` answers that instance, from its changed instance or made from the
///   series (`RECURRENCE-ID` and `DTSTART` with the series' `TZID` or `VALUE=DATE`);
/// - each component carries `UID`, `RECURRENCE-ID` for an instance, a fresh `DTSTAMP` (`now`,
///   milliseconds since the epoch), the invitation's `SEQUENCE` unchanged (0 when absent),
///   `ORGANIZER`, exactly one `ATTENDEE` (the user's own line with its parameters, `PARTSTAT`
///   set and `RSVP` dropped), then `SUMMARY`, `DTSTART`, `DTEND` or `DURATION` when present,
///   and `COMMENT` when given;
/// - `SCHEDULE-AGENT`, `SCHEDULE-STATUS` and `SCHEDULE-FORCE-SEND` never leave in a message;
/// - other attendees, alarms, descriptions and client properties are never sent;
/// - `VTIMEZONE`s used by the reply are copied from the invitation;
/// - `recipients` is the organizer alone.
///
/// Whether to send it at all (`SCHEDULE-AGENT=NONE`, a server that schedules) is
/// [`crate::reply_route`]'s answer.
pub fn reply(
    invitation: &[u8],
    attendee: &str,
    answer: Partstat,
    recurrence_id: Option<&str>,
    comment: Option<&str>,
    now: i64,
) -> Result<Imip, ImipError> {
    let item = parse(invitation)?;
    let organizer = organizer_address(&item)?;
    let me = address_key(attendee);
    let series = master(&item);
    let selected: Vec<(&Component, Option<Vec<Line>>)> = match recurrence_id.map(str::trim) {
        Some(value) => {
            if !valid_rid(value) {
                return Err(ImipError::NoInstance);
            }
            match (find_instance(&item, value), series) {
                (Some(found), _) => vec![(found, None)],
                (None, Some(series))
                    if crate::instance::key_of_value(&item, value)
                        .is_some_and(|key| crate::instance::occurs(&item, &key)) =>
                {
                    vec![(series, Some(instance_lines(series, value)))]
                }
                _ => return Err(ImipError::NoInstance),
            }
        }
        None => match series {
            Some(series) => vec![(series, None)],
            None => item.components().into_iter().map(|c| (c, None)).collect(),
        },
    };
    let stamp = stamp(now);
    let mut events = Vec::new();
    for (source, synthesized) in &selected {
        let mine = |c: &Component| {
            c.lines
                .iter()
                .find(|l| l.name == "ATTENDEE" && address_key(&l.value) == me)
                .cloned()
        };
        let mine = mine(source)
            .or_else(|| series.and_then(mine))
            .ok_or(ImipError::NotAttendee)?;
        let organizer_line = source
            .line("ORGANIZER")
            .or_else(|| organizer_line(&item))
            .ok_or(ImipError::NoOrganizer)?;
        let mut lines: Vec<Line> = Vec::new();
        lines.extend(source.line("UID").cloned());
        match synthesized {
            Some(instance) => lines.extend(instance.iter().take(1).cloned()),
            None => lines.extend(source.line("RECURRENCE-ID").cloned()),
        }
        lines.extend(line(format!("DTSTAMP:{stamp}")));
        lines.extend(line(format!("SEQUENCE:{}", sequence(source))));
        lines.extend(cleaned(organizer_line));
        lines.extend(line(edit_params(
            &mine.raw,
            &[SCHEDULE_PARAMS, &["RSVP", "PARTSTAT"]].concat(),
            &[("PARTSTAT", answer.as_str())],
        )));
        match synthesized {
            Some(instance) => {
                lines.extend(source.line("SUMMARY").cloned());
                lines.extend(instance.iter().skip(1).cloned());
            }
            None => {
                for name in ["SUMMARY", "DTSTART", "DTEND", "DURATION"] {
                    lines.extend(source.line(name).cloned());
                }
            }
        }
        if let Some(text) = comment.map(str::trim).filter(|t| !t.is_empty()) {
            lines.extend(line(format!("COMMENT:{}", escape(text))));
        }
        events.push(Component {
            name: "VEVENT".into(),
            lines,
            children: Vec::new(),
        });
    }
    let (summary, when, location) = first_shown(&events, &item);
    let location = location.or_else(|| {
        selected
            .first()
            .and_then(|(s, _)| shown(s, series, "LOCATION"))
    });
    Ok(Imip {
        method: Method::Reply,
        uid: uid(&item),
        sequence: events.first().map(sequence).unwrap_or(0),
        recurrence_id: events.first().and_then(rid),
        organizer: organizer.clone(),
        recipients: vec![organizer],
        summary,
        when,
        location,
        calendar: calendar("REPLY", &item, events),
    })
}

fn uid(item: &Item) -> String {
    item.uids().into_iter().flatten().next().unwrap_or_default()
}

fn outgoing(component: &Component, stamp: &str) -> Component {
    let mut out = component.clone();
    out.children.retain(|c| c.name != "VALARM");
    let mut lines = Vec::with_capacity(out.lines.len() + 2);
    let mut has_sequence = false;
    let mut has_stamp = false;
    for l in out.lines {
        match l.name.as_str() {
            n if n.starts_with("X-MOZ-") => {}
            "DTSTAMP" => {
                has_stamp = true;
                lines.extend(line(format!("DTSTAMP:{stamp}")));
            }
            "SEQUENCE" => {
                has_sequence = true;
                lines.push(l);
            }
            "ORGANIZER" | "ATTENDEE" => lines.extend(cleaned(&l)),
            _ => lines.push(l),
        }
    }
    let after_uid = lines
        .iter()
        .position(|l| l.name == "UID")
        .map_or(0, |at| at + 1);
    if !has_sequence {
        lines.splice(after_uid..after_uid, line("SEQUENCE:0".into()));
    }
    if !has_stamp {
        lines.splice(after_uid..after_uid, line(format!("DTSTAMP:{stamp}")));
    }
    out.lines = lines;
    out
}

/// An organizer's iMIP `REQUEST` (RFC 5546 section 3.2.2) of the event `event` (the organizer's
/// copy as stored, after [`organize`] set its `SEQUENCE`), to `audience`.
///
/// Guarantees: the series and every changed instance are sent with their `SEQUENCE` as stored
/// (0 added when absent), a fresh `DTSTAMP`, all `ORGANIZER` and `ATTENDEE` lines without
/// scheduling parameters, and the `VTIMEZONE`s they use; alarms and `X-MOZ-*` properties stay
/// home. `recipients` are the attendees `audience` picks, never the organizer; `NoRecipients`
/// when none is left.
pub fn request(event: &[u8], audience: Audience<'_>, now: i64) -> Result<Imip, ImipError> {
    let item = parse(event)?;
    let organizer = organizer_address(&item)?;
    let components = item.components();
    let recipients = recipients(&components, &organizer, audience);
    if recipients.is_empty() {
        return Err(ImipError::NoRecipients);
    }
    let stamp = stamp(now);
    let events: Vec<Component> = components.iter().map(|c| outgoing(c, &stamp)).collect();
    let (summary, when, location) = first_shown(&events, &item);
    let sequence = master(&item)
        .map(sequence)
        .unwrap_or_else(|| events.iter().map(sequence).max().unwrap_or(0));
    Ok(Imip {
        method: Method::Request,
        uid: uid(&item),
        sequence,
        recurrence_id: master(&item)
            .is_none()
            .then(|| events.first().and_then(rid))
            .flatten(),
        organizer,
        recipients,
        summary,
        when,
        location,
        calendar: calendar("REQUEST", &item, events),
    })
}

fn cancelled(
    source: &Component,
    instance: Option<Vec<Line>>,
    attendees: &[Line],
    organizer: &Line,
    sequence: u32,
    stamp: &str,
) -> Component {
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(source.line("UID").cloned());
    match &instance {
        Some(made) => lines.extend(made.iter().take(1).cloned()),
        None => lines.extend(source.line("RECURRENCE-ID").cloned()),
    }
    lines.extend(line(format!("DTSTAMP:{stamp}")));
    lines.extend(line(format!("SEQUENCE:{sequence}")));
    lines.extend(cleaned(organizer));
    lines.extend(attendees.iter().filter_map(cleaned));
    lines.extend(source.line("SUMMARY").cloned());
    match &instance {
        Some(made) => lines.extend(made.iter().skip(1).cloned()),
        None => {
            for name in ["DTSTART", "DTEND", "DURATION"] {
                lines.extend(source.line(name).cloned());
            }
        }
    }
    lines.extend(line("STATUS:CANCELLED".into()));
    Component {
        name: "VEVENT".into(),
        lines,
        children: Vec::new(),
    }
}

/// An organizer's iMIP `CANCEL` (RFC 5546 section 3.2.5) for `scope` of the event `event`, the
/// organizer's copy as it was before the cancellation.
///
/// Guarantees:
/// - [`CancelScope::Series`] cancels the series (or every changed instance when the copy holds
///   only those), [`CancelScope::Instance`] one instance (from its changed instance or made from
///   the series); both carry `SEQUENCE` one above the copy's (the highest of the series and the
///   instance), all attendees, `STATUS:CANCELLED` and go to `audience`;
/// - [`CancelScope::Attendees`] tells only those attendees, listing only them, with `SEQUENCE`
///   one above the copy's and `STATUS:CANCELLED`; `audience` is ignored;
/// - every component carries `UID`, a fresh `DTSTAMP`, `ORGANIZER` without scheduling
///   parameters, `SUMMARY` and the start when present; nothing else of the event is sent;
/// - `NoRecipients` when nobody is left, `NoInstance` for an instance the event cannot have.
pub fn cancel(
    event: &[u8],
    scope: CancelScope<'_>,
    audience: Audience<'_>,
    now: i64,
) -> Result<Imip, ImipError> {
    let item = parse(event)?;
    let organizer = organizer_address(&item)?;
    let organizer_line = organizer_line(&item).ok_or(ImipError::NoOrganizer)?.clone();
    let series = master(&item);
    let stamp = stamp(now);
    let attendees = |c: &Component| -> Vec<Line> {
        let own: Vec<Line> = c
            .lines
            .iter()
            .filter(|l| l.name == "ATTENDEE")
            .cloned()
            .collect();
        if own.is_empty() {
            series
                .map(|s| {
                    s.lines
                        .iter()
                        .filter(|l| l.name == "ATTENDEE")
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            own
        }
    };
    let (events, recipients) = match scope {
        CancelScope::Series => {
            let sources: Vec<&Component> = match series {
                Some(s) => vec![s],
                None => item.components(),
            };
            let events: Vec<Component> = sources
                .iter()
                .map(|s| {
                    cancelled(
                        s,
                        None,
                        &attendees(s),
                        &organizer_line,
                        sequence(s) + 1,
                        &stamp,
                    )
                })
                .collect();
            let everyone: Vec<&Component> = item.components();
            (events, recipients(&everyone, &organizer, audience))
        }
        CancelScope::Instance(value) => {
            let value = value.trim();
            if !valid_rid(value) {
                return Err(ImipError::NoInstance);
            }
            let base = series.map(sequence).unwrap_or(0);
            let event = match (find_instance(&item, value), series) {
                (Some(found), _) => cancelled(
                    found,
                    None,
                    &attendees(found),
                    &organizer_line,
                    base.max(sequence(found)) + 1,
                    &stamp,
                ),
                (None, Some(s)) if s.line("RRULE").is_some() || s.line("RDATE").is_some() => {
                    cancelled(
                        s,
                        Some(instance_lines(s, value)),
                        &attendees(s),
                        &organizer_line,
                        base + 1,
                        &stamp,
                    )
                }
                _ => return Err(ImipError::NoInstance),
            };
            let holder: Vec<&Component> = match find_instance(&item, value) {
                Some(found) if found.lines.iter().any(|l| l.name == "ATTENDEE") => vec![found],
                _ => series.into_iter().collect(),
            };
            (vec![event], recipients(&holder, &organizer, audience))
        }
        CancelScope::Attendees(removed) => {
            let removed: BTreeSet<String> = removed.iter().map(|a| address_key(a)).collect();
            let sources: Vec<&Component> = match series {
                Some(s) => vec![s],
                None => item.components(),
            };
            let events: Vec<Component> = sources
                .iter()
                .map(|s| {
                    let gone: Vec<Line> = attendees(s)
                        .into_iter()
                        .filter(|l| removed.contains(&address_key(&l.value)))
                        .collect();
                    cancelled(s, None, &gone, &organizer_line, sequence(s) + 1, &stamp)
                })
                .collect();
            let everyone = item.components();
            let list: Vec<String> = removed.into_iter().collect();
            let mut found = recipients(&everyone, &organizer, Audience::Only(&list));
            found.retain(|a| list.contains(a));
            (events, found)
        }
    };
    if recipients.is_empty() {
        return Err(ImipError::NoRecipients);
    }
    let (summary, when, location) = first_shown(&events, &item);
    let location = location.or_else(|| series.and_then(|s| shown(s, None, "LOCATION")));
    Ok(Imip {
        method: Method::Cancel,
        uid: uid(&item),
        sequence: events.first().map(sequence).unwrap_or(0),
        recurrence_id: match scope {
            CancelScope::Instance(_) => events.first().and_then(rid),
            _ => None,
        },
        organizer,
        recipients,
        summary,
        when,
        location,
        calendar: calendar("CANCEL", &item, events),
    })
}

fn answer_free(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = values
        .iter()
        .map(|raw| {
            Line::parse(raw).map_or_else(
                || raw.clone(),
                |l| l.canonical(&[ANSWER_PARAMS, SCHEDULE_PARAMS].concat()),
            )
        })
        .collect();
    out.sort();
    out
}

fn exdates(values: &[String]) -> BTreeSet<String> {
    values
        .iter()
        .filter_map(|raw| Line::parse(raw))
        .flat_map(|l| {
            l.value
                .split(',')
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[derive(Default)]
struct Diff {
    content: bool,
    added: Vec<String>,
    removed: Vec<String>,
    exdates: Vec<String>,
    bump: BTreeSet<String>,
    reset: BTreeSet<String>,
}

fn compare(previous: &Item, next: &Item, organizer: &str) -> Diff {
    let mut diff = Diff::default();
    let before: BTreeMap<String, Part<'_>> = previous
        .parts()
        .into_iter()
        .map(|p| (p.key.clone(), p))
        .collect();
    let after: BTreeMap<String, Part<'_>> = next
        .parts()
        .into_iter()
        .map(|p| (p.key.clone(), p))
        .collect();
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for key in keys {
        let (Some(a), Some(b)) = (before.get(key), after.get(key)) else {
            diff.content = true;
            diff.bump.insert(key.clone());
            continue;
        };
        let groups: BTreeSet<&String> = a.groups.keys().chain(b.groups.keys()).collect();
        let empty = Vec::new();
        for group in groups {
            if category(ItemKind::Event, group) == Category::Personal || group == "DTSTAMP" {
                continue;
            }
            let old = a.groups.get(group).unwrap_or(&empty);
            let new = b.groups.get(group).unwrap_or(&empty);
            if let Some(address) = group.strip_prefix("ATTENDEE ") {
                if address == organizer {
                    continue;
                }
                match (old.is_empty(), new.is_empty()) {
                    (true, false) if !diff.added.iter().any(|x| x == address) => {
                        diff.added.push(address.to_owned())
                    }
                    (false, true) => {
                        if !diff.removed.iter().any(|x| x == address) {
                            diff.removed.push(address.to_owned());
                        }
                        diff.bump.insert(key.clone());
                    }
                    (false, false) if answer_free(old) != answer_free(new) => diff.content = true,
                    _ => {}
                }
                continue;
            }
            if model::same(group, old, new) {
                continue;
            }
            if group == "EXDATE" {
                let (was, now) = (exdates(old), exdates(new));
                if now.is_superset(&was) {
                    for value in now.difference(&was) {
                        diff.exdates.push(value.clone());
                    }
                } else {
                    diff.content = true;
                }
            } else {
                diff.content = true;
            }
            if SIGNIFICANT.contains(&group.as_str()) {
                diff.bump.insert(key.clone());
            }
            if RESCHEDULING.contains(&group.as_str()) {
                diff.reset.insert(key.clone());
            }
        }
    }
    diff
}

fn adjust(previous: &Item, next: &mut Item, diff: &Diff, organizer: &str) -> bool {
    let before: BTreeMap<String, u32> = previous
        .components()
        .into_iter()
        .map(|c| (previous.component_key(c), sequence(c)))
        .collect();
    let floor = master(previous).map(sequence).unwrap_or(0);
    let mut changed = false;
    for component in next.top.children.iter_mut().filter(|c| c.name == "VEVENT") {
        let key = component
            .line("RECURRENCE-ID")
            .map(model::instance_key)
            .unwrap_or_default();
        if diff.bump.contains(&key) {
            let was = before.get(&key).copied().unwrap_or(floor);
            if sequence(component) <= was {
                let new = line(format!("SEQUENCE:{}", was + 1));
                match component.lines.iter().position(|l| l.name == "SEQUENCE") {
                    Some(at) => {
                        if let Some(new) = new {
                            component.lines[at] = new;
                        }
                    }
                    None => {
                        let at = component
                            .lines
                            .iter()
                            .position(|l| l.name == "UID")
                            .map_or(0, |at| at + 1);
                        component.lines.splice(at..at, new);
                    }
                }
                changed = true;
            }
        }
        if diff.reset.contains(&key) {
            for attendee in component.lines.iter_mut().filter(|l| l.name == "ATTENDEE") {
                let needs = attendee
                    .param("PARTSTAT")
                    .is_some_and(|p| !p.eq_ignore_ascii_case("NEEDS-ACTION"));
                if needs && normalize_email(&attendee.value).as_deref() != Some(organizer) {
                    let raw = edit_params(
                        &attendee.raw,
                        &["PARTSTAT", "RSVP"],
                        &[("PARTSTAT", "NEEDS-ACTION"), ("RSVP", "TRUE")],
                    );
                    if let Some(new) = line(raw) {
                        *attendee = new;
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

fn keep(result: Result<Imip, ImipError>, out: &mut Vec<Imip>) -> Result<(), ImipError> {
    match result {
        Ok(message) => {
            out.push(message);
            Ok(())
        }
        Err(ImipError::NoRecipients | ImipError::NoOrganizer) => Ok(()),
        Err(other) => Err(other),
    }
}

/// The organizer's side of a change from `previous` to `next` (`None` for a create or a
/// delete): the copy to store and the iTIP messages to send. `server_schedules` is true on
/// [`crate::OrganizerRoute::Server`] and [`crate::OrganizerRoute::Deferred`], where only
/// `SCHEDULE-AGENT=CLIENT` attendees are mailed.
///
/// Guarantees:
/// - a create is one `REQUEST` to every attendee; a delete one series `CANCEL`;
/// - in an update, removed attendees get a `CANCEL` naming only them, an `EXDATE` added to the
///   series a `CANCEL` of that instance, and a change of anything but answers, alarms, client
///   properties and `EXDATE`s a `REQUEST` to everyone; when attendees were only added, the
///   `REQUEST` goes to them alone; answers alone send nothing;
/// - `SEQUENCE` goes one above the previous copy's for every component whose start, end,
///   duration, recurrence, `EXDATE` or status changed (RFC 5545 section 3.8.7.4) or that lost
///   an attendee (so the stored copy is never older than the `CANCEL` that attendee holds),
///   unless the new copy already raised it; a changed start, end or recurrence also resets every other
///   attendee to `PARTSTAT=NEEDS-ACTION;RSVP=TRUE`;
/// - an event without organizer, or without anyone to tell, gives no messages and no error.
pub fn organize(
    previous: Option<&[u8]>,
    next: Option<&[u8]>,
    server_schedules: bool,
    now: i64,
) -> Result<Organized, ImipError> {
    let audience = if server_schedules {
        Audience::ClientScheduled
    } else {
        Audience::Everyone
    };
    let mut messages = Vec::new();
    match (previous, next) {
        (None, None) => Ok(Organized {
            event: None,
            messages,
        }),
        (None, Some(next)) => {
            parse(next)?;
            keep(request(next, audience, now), &mut messages)?;
            Ok(Organized {
                event: Some(next.to_vec()),
                messages,
            })
        }
        (Some(previous), None) => {
            parse(previous)?;
            keep(
                cancel(previous, CancelScope::Series, audience, now),
                &mut messages,
            )?;
            Ok(Organized {
                event: None,
                messages,
            })
        }
        (Some(previous_raw), Some(next_raw)) => {
            let previous = parse(previous_raw)?;
            let mut next = parse(next_raw)?;
            let Ok(organizer) = organizer_address(&next).or_else(|_| organizer_address(&previous))
            else {
                return Ok(Organized {
                    event: Some(next_raw.to_vec()),
                    messages,
                });
            };
            let diff = compare(&previous, &next, &organizer);
            let event = if adjust(&previous, &mut next, &diff, &organizer) {
                next.write()
            } else {
                next_raw.to_vec()
            };
            if !diff.removed.is_empty() {
                let removed: Vec<String> = if server_schedules {
                    let components = previous.components();
                    recipients(&components, &organizer, Audience::ClientScheduled)
                        .into_iter()
                        .filter(|a| diff.removed.contains(a))
                        .collect()
                } else {
                    diff.removed.clone()
                };
                if !removed.is_empty() {
                    keep(
                        cancel(
                            previous_raw,
                            CancelScope::Attendees(&removed),
                            audience,
                            now,
                        ),
                        &mut messages,
                    )?;
                }
            }
            for value in &diff.exdates {
                if valid_rid(value) {
                    keep(
                        cancel(previous_raw, CancelScope::Instance(value), audience, now),
                        &mut messages,
                    )?;
                }
            }
            if diff.content {
                keep(request(&event, audience, now), &mut messages)?;
            } else if !diff.added.is_empty() {
                let added: Vec<String> = if server_schedules {
                    let parsed = parse(&event)?;
                    recipients(&parsed.components(), &organizer, Audience::ClientScheduled)
                        .into_iter()
                        .filter(|a| diff.added.contains(a))
                        .collect()
                } else {
                    diff.added.clone()
                };
                if !added.is_empty() {
                    keep(request(&event, Audience::Only(&added), now), &mut messages)?;
                }
            }
            Ok(Organized {
                event: Some(event),
                messages,
            })
        }
    }
}
