use std::collections::BTreeSet;

use chrono::{NaiveDate, NaiveDateTime};
use holtmail_collection::Partstat;
use holtmail_pim::{ItemKind, Method, normalize_email};
use serde::{Deserialize, Serialize};

use crate::imip::{ImipError, edit_params, valid_rid};
use crate::instance;
use crate::lines::{Component, Line};
use crate::model::{Item, address_key};

const RECURRENCE: &[&str] = &["RRULE", "RDATE", "EXDATE", "EXRULE"];

/// How a `reply` change reaches the account's server: through
/// `CollectionWriter::respond` where the writer offers it, telling the organizer only when
/// `notify` is set. Stored inside the change's [`crate::Patch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Respond {
    /// The answer, as iCalendar writes it (`ACCEPTED`, `TENTATIVE`, `DECLINED`).
    pub answer: String,
    /// The occurrence answered, as iCalendar writes its `RECURRENCE-ID`; `None` for the series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<String>,
    /// The server tells the organizer (no iMIP reply was sent by Holtmail).
    pub notify: bool,
}

impl Respond {
    /// The answer as a [`Partstat`]; `None` when it is not one.
    pub fn partstat(&self) -> Option<Partstat> {
        Partstat::parse(&self.answer)
    }
}

fn parse(raw: &[u8]) -> Result<Item, ImipError> {
    let item = Item::parse(ItemKind::Event, raw).ok_or(ImipError::Unreadable)?;
    if item.components().is_empty() {
        return Err(ImipError::Unreadable);
    }
    Ok(item)
}

fn events_mut(item: &mut Item) -> impl Iterator<Item = &mut Component> {
    item.top.children.iter_mut().filter(|c| c.name == "VEVENT")
}

fn set_answer(component: &mut Component, who: &str, answer: &str) -> bool {
    let mut changed = false;
    for l in component.lines.iter_mut() {
        if l.name == "ATTENDEE" && address_key(&l.value) == who {
            let raw = edit_params(
                &l.raw,
                &["RSVP", "PARTSTAT", "SCHEDULE-STATUS"],
                &[("PARTSTAT", answer)],
            );
            if let Some(new) = Line::parse(&raw)
                && new != *l
            {
                *l = new;
            }
            changed = true;
        }
    }
    changed
}

fn moment(value: &str) -> Option<(NaiveDateTime, bool)> {
    let v = value.trim().trim_end_matches('Z');
    if v.len() == 8 {
        let date = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        return Some((date.and_hms_opt(0, 0, 0)?, true));
    }
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .ok()
        .map(|t| (t, false))
}

fn duration_of(series: &Component) -> Option<String> {
    let start = moment(&series.line("DTSTART")?.value)?;
    let end = moment(&series.line("DTEND")?.value)?;
    let seconds = (end.0 - start.0).num_seconds().max(0);
    Some(if start.1 && seconds % 86_400 == 0 {
        format!("DURATION:P{}D", seconds / 86_400)
    } else {
        format!("DURATION:PT{seconds}S")
    })
}

fn occurrence(series: &Component, value: &str) -> Component {
    let params = crate::instance::series_params(series, value);
    let mut lines: Vec<Line> = Vec::with_capacity(series.lines.len() + 2);
    for l in &series.lines {
        if RECURRENCE.contains(&l.name.as_str())
            || matches!(l.name.as_str(), "RECURRENCE-ID" | "DTSTART" | "DTEND")
        {
            continue;
        }
        lines.push(l.clone());
        if l.name == "UID" {
            lines.extend(Line::parse(&format!("RECURRENCE-ID{params}:{value}")));
            lines.extend(Line::parse(&format!("DTSTART{params}:{value}")));
            if series.line("DURATION").is_none() {
                lines.extend(duration_of(series).and_then(|d| Line::parse(&d)));
            }
        }
    }
    Component {
        name: series.name.clone(),
        lines,
        children: series.children.clone(),
    }
}

/// The account's copy `copy` of an invitation with `attendee`'s own answer set to `answer`
/// (`PARTSTAT` set, `RSVP` and `SCHEDULE-STATUS` dropped): the bytes of a `reply` change.
///
/// Guarantees:
/// - `recurrence_id` `None` answers the series and every changed instance that lists the
///   attendee; `Some` answers that instance only, adding it as a changed instance made from the
///   series (its `RECURRENCE-ID` and `DTSTART`, a `DURATION` for the series' length, nothing
///   else changed) when the copy has none, so [`crate::classify`] sees a `reply`; an instance is
///   the same instant however `recurrence_id` writes it;
/// - nothing but the attendee's own lines changes; the address matches in any spelling
///   `normalize_email` accepts;
/// - `NotAttendee` when no answered component lists the attendee, `NoInstance` for a value that
///   is no date or date-time or an instance the series does not have (off its rule, or removed
///   by an `EXDATE`), `Unreadable` for bytes that are not one event.
pub fn answered(
    copy: &[u8],
    attendee: &str,
    answer: Partstat,
    recurrence_id: Option<&str>,
) -> Result<Vec<u8>, ImipError> {
    let mut item = parse(copy)?;
    let who = address_key(attendee);
    let answer = answer.as_str();
    let mut changed = false;
    match recurrence_id.map(str::trim) {
        None => {
            for event in events_mut(&mut item) {
                changed |= set_answer(event, &who, answer);
            }
        }
        Some(value) => {
            if !valid_rid(value) {
                return Err(ImipError::NoInstance);
            }
            let key = instance::key_of_value(&item, value).ok_or(ImipError::NoInstance)?;
            if let Some(at) = instance::position(&item, &key) {
                changed = set_answer(&mut item.top.children[at], &who, answer);
            } else {
                if !instance::occurs(&item, &key) {
                    return Err(ImipError::NoInstance);
                }
                let series = item
                    .components()
                    .into_iter()
                    .find(|c| c.line("RECURRENCE-ID").is_none())
                    .ok_or(ImipError::NoInstance)?;
                let mut instance = occurrence(series, value);
                changed = set_answer(&mut instance, &who, answer);
                if changed {
                    item.top.children.push(instance);
                }
            }
        }
    }
    if !changed {
        return Err(ImipError::NotAttendee);
    }
    Ok(item.write())
}

fn sequence(component: &Component) -> u32 {
    component
        .line("SEQUENCE")
        .and_then(|l| l.value.trim().parse().ok())
        .unwrap_or(0)
}

/// The one address a `REPLY` answers for, normalized; `None` when it names none or several.
fn sole_attendee(reply: &Item) -> Option<String> {
    let found: BTreeSet<String> = reply
        .components()
        .into_iter()
        .flat_map(|c| c.lines.iter())
        .filter(|l| l.name == "ATTENDEE")
        .map(|l| address_key(&l.value))
        .collect();
    let mut found = found.into_iter();
    match (found.next(), found.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// The organizer's copy `copy` with the answer of an iMIP `REPLY` (`reply`, the whole
/// `VCALENDAR`) applied: the replying attendee's `PARTSTAT` set on the series or on the instance
/// the reply names. `senders` are the addresses the mail is known to come from.
///
/// `None` when the reply changes nothing: not a `REPLY`, another event (`UID`), not exactly one
/// `ATTENDEE`, an attendee who is none of `senders` (RFC 5546 section 3.2.3 has the attendee
/// answer for themselves only), a `SEQUENCE` older than the copy's, an attendee the copy does
/// not list, or an answer it already has.
///
/// Guarantees: only the replying attendee's `ATTENDEE` lines change, never the organizer's; an
/// instance is matched by its instant however each side writes it; an answer to an instance the
/// copy has no changed instance for is added as one made from the series, as [`answered`] does,
/// and only for an instance the series really has.
pub fn apply_reply(copy: &[u8], reply: &[u8], senders: &[String]) -> Option<Vec<u8>> {
    let method = holtmail_pim::calendar_method(reply);
    if Method::of(method.as_deref()) != Method::Reply {
        return None;
    }
    let reply = parse(reply).ok()?;
    let original = parse(copy).ok()?;
    let uid = |item: &Item| item.uids().into_iter().flatten().next();
    if uid(&reply)? != uid(&original)? {
        return None;
    }
    let who = sole_attendee(&reply)?;
    if !senders.iter().any(|s| address_key(s) == who) {
        return None;
    }
    let organizer = original
        .components()
        .into_iter()
        .find_map(|c| c.line("ORGANIZER"))
        .and_then(|l| normalize_email(&l.value));
    if organizer.as_deref() == Some(who.as_str()) {
        return None;
    }
    let mut out = original.clone();
    let before = out.write();
    for answer in reply.components() {
        let seq = sequence(answer);
        let Some(line) = answer.lines.iter().find(|l| l.name == "ATTENDEE") else {
            continue;
        };
        let Some(partstat) = line.param("PARTSTAT").and_then(Partstat::parse) else {
            continue;
        };
        let Some(rid) = answer.line("RECURRENCE-ID") else {
            if let Some(series) = out
                .top
                .children
                .iter_mut()
                .find(|c| c.name == "VEVENT" && c.line("RECURRENCE-ID").is_none())
                && seq >= sequence(series)
            {
                set_answer(series, &who, partstat.as_str());
            }
            continue;
        };
        let value = rid.value.trim();
        if !valid_rid(value) {
            continue;
        }
        let Some(key) = instance::key_of(&reply, answer) else {
            continue;
        };
        if let Some(at) = instance::position(&out, &key) {
            let target = &mut out.top.children[at];
            if seq >= sequence(target) {
                set_answer(target, &who, partstat.as_str());
            }
            continue;
        }
        if !instance::occurs(&out, &key) {
            continue;
        }
        let Some(series) = out
            .components()
            .into_iter()
            .find(|c| c.line("RECURRENCE-ID").is_none())
            .filter(|s| seq >= sequence(s))
        else {
            continue;
        };
        let mut instance = occurrence(series, value);
        if set_answer(&mut instance, &who, partstat.as_str()) {
            out.top.children.push(instance);
        }
    }
    let after = out.write();
    (after != before).then_some(after)
}

/// Whether the event in `raw` holds its series or whole event, not only changed instances.
pub fn holds_series(raw: &[u8]) -> bool {
    parse(raw).is_ok_and(|item| {
        item.components()
            .iter()
            .any(|c| c.line("RECURRENCE-ID").is_none())
    })
}

/// The invitation `base` with the changed instances of the later instance-only updates
/// `updates` (oldest first) laid over it: an instance replaces the one with the same instant, or
/// is added. `base` unchanged when it or an update cannot be read.
pub fn with_instances(base: &[u8], updates: &[&[u8]]) -> Vec<u8> {
    let Ok(mut item) = parse(base) else {
        return base.to_vec();
    };
    for update in updates {
        let Ok(update) = parse(update) else {
            return base.to_vec();
        };
        for component in update.components() {
            let Some(key) = instance::key_of(&update, component) else {
                continue;
            };
            match instance::position(&item, &key) {
                Some(at) => item.top.children[at] = component.clone(),
                None => item.top.children.push(component.clone()),
            }
        }
    }
    item.write()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Class, classify};

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:rota@example.com\r\nDTSTAMP:20261101T090000Z\r\nSEQUENCE:2\r\nDTSTART;TZID=Europe/Berlin:20261201T100000\r\nDTEND;TZID=Europe/Berlin:20261201T103000\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Rota\r\nORGANIZER;CN=Bob:mailto:bob@example.org\r\nATTENDEE;CN=Ada;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:cy@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn text(raw: &[u8]) -> String {
        String::from_utf8(raw.to_vec()).unwrap()
    }

    #[test]
    fn answers_the_series_as_a_reply_change() {
        let out = answered(
            SERIES.as_bytes(),
            "MAILTO:Ada@Example.com",
            Partstat::Accepted,
            None,
        )
        .unwrap();
        let out_text = text(&out);
        assert!(out_text.contains("ATTENDEE;CN=Ada;PARTSTAT=ACCEPTED:mailto:ada@example.com"));
        assert!(out_text.contains("ATTENDEE;PARTSTAT=ACCEPTED:mailto:cy@example.org"));
        let own = vec!["ada@example.com".to_owned()];
        let classified = classify(ItemKind::Event, Some(SERIES.as_bytes()), &out, &own).unwrap();
        assert_eq!(classified.class, Class::Reply);
    }

    #[test]
    fn answers_one_instance_by_adding_it() {
        let out = answered(
            SERIES.as_bytes(),
            "ada@example.com",
            Partstat::Declined,
            Some("20261208T100000"),
        )
        .unwrap();
        let out_text = text(&out);
        assert!(out_text.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261208T100000"));
        assert!(out_text.contains("DURATION:PT1800S"));
        assert!(out_text.contains("PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com"));
        assert!(out_text.contains("PARTSTAT=DECLINED:mailto:ada@example.com"));
        let own = vec!["ada@example.com".to_owned()];
        let classified = classify(ItemKind::Event, Some(SERIES.as_bytes()), &out, &own).unwrap();
        assert_eq!(classified.class, Class::Reply);
    }

    #[test]
    fn refuses_strangers_and_impossible_instances() {
        assert_eq!(
            answered(
                SERIES.as_bytes(),
                "eve@example.net",
                Partstat::Accepted,
                None
            ),
            Err(ImipError::NotAttendee)
        );
        assert_eq!(
            answered(
                SERIES.as_bytes(),
                "ada@example.com",
                Partstat::Accepted,
                Some("not a date")
            ),
            Err(ImipError::NoInstance)
        );
    }

    #[test]
    fn applies_an_attendee_reply_to_the_organizer_copy() {
        let copy = SERIES
            .replace(
                "CN=Bob:mailto:bob@example.org",
                "CN=Ada:mailto:ada@example.com",
            )
            .replace(
                "ATTENDEE;CN=Ada;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com",
                "ATTENDEE;CN=Bob;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org",
            );
        let reply = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:rota@example.com\r\nDTSTAMP:20261102T090000Z\r\nSEQUENCE:2\r\nORGANIZER:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=TENTATIVE:mailto:BOB@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let bob = ["bob@example.org".to_owned()];
        let out = apply_reply(copy.as_bytes(), reply.as_bytes(), &bob).unwrap();
        assert!(text(&out).contains("ATTENDEE;CN=Bob;PARTSTAT=TENTATIVE:mailto:bob@example.org"));
        assert_eq!(apply_reply(&out, reply.as_bytes(), &bob), None);
        let older = reply.replace("SEQUENCE:2", "SEQUENCE:1");
        assert_eq!(apply_reply(copy.as_bytes(), older.as_bytes(), &bob), None);
        let request = reply.replace("METHOD:REPLY", "METHOD:REQUEST");
        assert_eq!(apply_reply(copy.as_bytes(), request.as_bytes(), &bob), None);
        let other = reply.replace("UID:rota@example.com", "UID:other@example.com");
        assert_eq!(apply_reply(copy.as_bytes(), other.as_bytes(), &bob), None);
    }

    fn organized() -> String {
        SERIES
            .replace(
                "CN=Bob:mailto:bob@example.org",
                "CN=Ada:mailto:ada@example.com",
            )
            .replace(
                "ATTENDEE;CN=Ada;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com",
                "ATTENDEE;CN=Bob;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org",
            )
    }

    fn reply_of(attendees: &str, extra: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:rota@example.com\r\nDTSTAMP:20261102T090000Z\r\nSEQUENCE:2\r\n{extra}ORGANIZER:mailto:ada@example.com\r\n{attendees}END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn a_reply_speaks_only_for_its_sender() {
        let copy = organized();
        let one = reply_of("ATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\n", "");
        let stranger = ["eve@example.net".to_owned()];
        assert_eq!(
            apply_reply(copy.as_bytes(), one.as_bytes(), &stranger),
            None
        );
        let two = reply_of(
            "ATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\nATTENDEE;PARTSTAT=DECLINED:mailto:cy@example.org\r\n",
            "",
        );
        let both = ["bob@example.org".to_owned(), "cy@example.org".to_owned()];
        assert_eq!(apply_reply(copy.as_bytes(), two.as_bytes(), &both), None);
    }

    #[test]
    fn a_reply_finds_the_changed_instance_by_its_instant() {
        let moved = organized().replace(
            "END:VEVENT\r\nEND:VCALENDAR",
            "END:VEVENT\r\nBEGIN:VEVENT\r\nUID:rota@example.com\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261208T100000\r\nSEQUENCE:2\r\nDTSTART;TZID=Europe/Berlin:20261208T140000\r\nDTEND;TZID=Europe/Berlin:20261208T143000\r\nORGANIZER:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR",
        );
        let reply = reply_of(
            "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\n",
            "RECURRENCE-ID:20261208T090000Z\r\n",
        );
        let out = apply_reply(
            moved.as_bytes(),
            reply.as_bytes(),
            &["bob@example.org".to_owned()],
        )
        .unwrap();
        let out = text(&out);
        assert_eq!(out.matches("RECURRENCE-ID").count(), 1, "{out}");
        assert!(out.contains("DTSTART;TZID=Europe/Berlin:20261208T140000"));
        assert!(out.contains("ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org"));
    }

    #[test]
    fn a_reply_cannot_invent_instances() {
        let copy = organized();
        for rid in ["20261209T100000", "TTT", "20270601T100000Z"] {
            let reply = reply_of(
                "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\n",
                &format!("RECURRENCE-ID:{rid}\r\n"),
            );
            assert_eq!(
                apply_reply(
                    copy.as_bytes(),
                    reply.as_bytes(),
                    &["bob@example.org".to_owned()]
                ),
                None,
                "{rid}"
            );
        }
        assert_eq!(
            answered(
                SERIES.as_bytes(),
                "ada@example.com",
                Partstat::Accepted,
                Some("20261209T100000")
            ),
            Err(ImipError::NoInstance)
        );
    }

    #[test]
    fn later_instance_updates_are_laid_over_the_series() {
        let update = SERIES
            .replace("RRULE:FREQ=WEEKLY;COUNT=4\r\n", "")
            .replace(
                "DTSTART;TZID=Europe/Berlin:20261201T100000",
                "RECURRENCE-ID;TZID=Europe/Berlin:20261208T100000\r\nDTSTART;TZID=Europe/Berlin:20261208T150000",
            )
            .replace(
                "DTEND;TZID=Europe/Berlin:20261201T103000",
                "DTEND;TZID=Europe/Berlin:20261208T153000",
            );
        assert!(!holds_series(update.as_bytes()));
        assert!(holds_series(SERIES.as_bytes()));
        let merged = with_instances(SERIES.as_bytes(), &[update.as_bytes()]);
        let merged = text(&merged);
        assert!(merged.contains("RRULE:FREQ=WEEKLY"));
        assert!(merged.contains("DTSTART;TZID=Europe/Berlin:20261208T150000"));
    }
}
