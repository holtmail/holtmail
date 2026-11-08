use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta};
use holtmail_pim::{
    EventTime, ItemKind, NameParts, Normalized, Participant, PostalAddress, iana_zone, normalize,
    normalize_email,
};
use serde::{Deserialize, Serialize};

use crate::lines::{self, Component, Line, escape, parameter};
use crate::model::{Item, set_group};
use crate::validate::{Invalid, validate};

const PRODID: &str = "PRODID:-//Holtmail//Edit//EN";

/// Why a form cannot become an item.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormError {
    /// A new item needs a `uid`.
    #[error("a new item needs a UID")]
    MissingUid,
    /// A start or end that is not `YYYY-MM-DD` (all-day) or `YYYY-MM-DDTHH:MM[:SS]`.
    #[error("{0} is not a valid time")]
    BadTime(&'static str),
    /// The end is not after the start.
    #[error("the end is not after the start")]
    EndBeforeStart,
    /// The time zone is neither `UTC` nor a known IANA or Windows zone name.
    #[error("unknown time zone")]
    UnknownZone,
    /// The recurrence rule is not an `RRULE` value with `FREQ`.
    #[error("invalid recurrence rule")]
    BadRule,
    /// An attendee, organizer or email entry is not an email address.
    #[error("invalid email address")]
    BadAddress,
    /// A contact with no name, organization or email address.
    #[error("a contact needs a name, an organization or an email address")]
    Empty,
    /// The base cannot be read, or the result fails [`crate::validate`].
    #[error(transparent)]
    Invalid(#[from] Invalid),
}

/// A person on an event: the organizer or an attendee.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Person {
    /// The email address.
    pub email: String,
    /// The display name (`CN`).
    pub name: Option<String>,
    /// The answer (`PARTSTAT`), such as `ACCEPTED`; `None` keeps what the base had, or
    /// `NEEDS-ACTION` with `RSVP=TRUE` for a new attendee.
    pub partstat: Option<String>,
}

/// A reminder before the start of an event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alarm {
    /// Minutes before the start; 0 at the start.
    pub minutes_before: i64,
}

/// The fields the web app's event form edits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventForm {
    /// `UID`; required for a new event, ignored when a base is given.
    pub uid: Option<String>,
    /// `SUMMARY`; empty for none.
    pub title: String,
    /// `YYYY-MM-DD` for an all-day event, else `YYYY-MM-DDTHH:MM[:SS]` in `time_zone`.
    pub start: String,
    /// Like `start`; exclusive for all-day events. `None` means one day (all-day) or one hour.
    pub end: Option<String>,
    /// The event takes whole days.
    pub all_day: bool,
    /// `UTC`, an IANA or Windows zone name, or `None` for a floating time. Ignored for all-day
    /// events.
    pub time_zone: Option<String>,
    /// An `RRULE` value such as `FREQ=WEEKLY;BYDAY=MO`.
    pub recurrence: Option<String>,
    /// `LOCATION`.
    pub location: Option<String>,
    /// `DESCRIPTION`.
    pub description: Option<String>,
    /// `ORGANIZER`.
    pub organizer: Option<Person>,
    /// `ATTENDEE`s.
    pub attendees: Vec<Person>,
    /// Display alarms relative to the start.
    pub alarms: Vec<Alarm>,
}

/// An email address or phone number of a contact with its kinds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Labeled {
    /// The address or number.
    pub value: String,
    /// `TYPE` values, lowercase, such as `work` or `cell`.
    pub kinds: Vec<String>,
}

/// The fields the web app's contact form edits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContactForm {
    /// `UID`; required for a new contact, ignored when a base is given.
    pub uid: Option<String>,
    /// `FN`; `None` makes it from the name, the organization or the first email address.
    pub full_name: Option<String>,
    /// `N`.
    pub name: NameParts,
    /// `EMAIL`s.
    pub emails: Vec<Labeled>,
    /// `TEL`s.
    pub phones: Vec<Labeled>,
    /// `ADR`s.
    pub addresses: Vec<PostalAddress>,
    /// `ORG`.
    pub organization: Option<String>,
    /// `TITLE`.
    pub title: Option<String>,
    /// `NOTE`.
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Zone {
    Floating,
    Utc,
    Named(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum When {
    Date(NaiveDate),
    Local(NaiveDateTime),
}

fn zone_of(name: Option<&str>) -> Result<Zone, FormError> {
    match name.map(str::trim) {
        None | Some("") => Ok(Zone::Floating),
        Some(z) if z.eq_ignore_ascii_case("utc") || z == "Etc/UTC" || z == "Z" => Ok(Zone::Utc),
        Some(z) => iana_zone(z).map(Zone::Named).ok_or(FormError::UnknownZone),
    }
}

fn when_of(text: &str, all_day: bool, field: &'static str) -> Result<When, FormError> {
    let text = text.trim();
    if all_day {
        return NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(When::Date)
            .map_err(|_| FormError::BadTime(field));
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .map(When::Local)
        .map_err(|_| FormError::BadTime(field))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Times {
    start: When,
    end: When,
    zone: Zone,
}

fn times(form: &EventForm) -> Result<Times, FormError> {
    let zone = if form.all_day {
        Zone::Floating
    } else {
        zone_of(form.time_zone.as_deref())?
    };
    let start = when_of(&form.start, form.all_day, "start")?;
    let end = match &form.end {
        Some(end) if !end.trim().is_empty() => when_of(end, form.all_day, "end")?,
        _ => match start {
            When::Date(d) => When::Date(d + TimeDelta::days(1)),
            When::Local(t) => When::Local(t + TimeDelta::hours(1)),
        },
    };
    if end <= start {
        return Err(FormError::EndBeforeStart);
    }
    Ok(Times { start, end, zone })
}

fn time_line(name: &str, when: When, zone: &Zone) -> String {
    match (when, zone) {
        (When::Date(d), _) => format!("{name};VALUE=DATE:{}", d.format("%Y%m%d")),
        (When::Local(t), Zone::Floating) => format!("{name}:{}", t.format("%Y%m%dT%H%M%S")),
        (When::Local(t), Zone::Utc) => format!("{name}:{}Z", t.format("%Y%m%dT%H%M%S")),
        (When::Local(t), Zone::Named(z)) => {
            format!("{name};TZID={}:{}", parameter(z), t.format("%Y%m%dT%H%M%S"))
        }
    }
}

fn stamp(millis: i64) -> String {
    DateTime::from_timestamp_millis(millis)
        .unwrap_or_default()
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn check_rule(rule: &str) -> Result<(), FormError> {
    let ok = rule.to_ascii_uppercase().contains("FREQ=")
        && rule
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "=;,:+-".contains(c));
    if ok { Ok(()) } else { Err(FormError::BadRule) }
}

fn address(email: &str) -> Result<String, FormError> {
    normalize_email(email).ok_or(FormError::BadAddress)
}

fn person_line(name: &str, person: &Person, base: Option<&Line>) -> Result<String, FormError> {
    let email = address(&person.email)?;
    let mut params: Vec<(String, String)> = base.map(|l| l.params.clone()).unwrap_or_else(|| {
        if name == "ATTENDEE" {
            vec![
                ("ROLE".into(), "REQ-PARTICIPANT".into()),
                ("PARTSTAT".into(), "NEEDS-ACTION".into()),
                ("RSVP".into(), "TRUE".into()),
            ]
        } else {
            Vec::new()
        }
    });
    params.retain(|(k, _)| k != "CN");
    if let Some(cn) = person.name.as_deref().filter(|n| !n.trim().is_empty()) {
        params.insert(0, ("CN".into(), cn.trim().to_owned()));
    }
    if let Some(partstat) = person.partstat.as_deref().filter(|p| !p.trim().is_empty()) {
        params.retain(|(k, _)| k != "PARTSTAT");
        params.push(("PARTSTAT".into(), partstat.trim().to_ascii_uppercase()));
    }
    let mut out = name.to_owned();
    for (k, v) in params {
        out.push_str(&format!(";{k}={}", parameter(&v)));
    }
    out.push_str(&format!(":mailto:{email}"));
    Ok(out)
}

fn same_person(a: &Person, b: &Person) -> bool {
    let up = |p: &Option<String>| p.as_deref().map(|s| s.trim().to_ascii_uppercase());
    normalize_email(&a.email) == normalize_email(&b.email)
        && a.name.as_deref().map(str::trim).filter(|s| !s.is_empty())
            == b.name.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && (a.partstat.is_none() || up(&a.partstat) == up(&b.partstat))
}

fn trigger_minutes(value: &str) -> Option<i64> {
    let value = value.trim();
    let (negative, rest) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut minutes = 0i64;
    let mut number = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => number.push(c),
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                minutes += match (unit, in_time) {
                    ('W', false) => n * 7 * 24 * 60,
                    ('D', false) => n * 24 * 60,
                    ('H', true) => n * 60,
                    ('M', true) => n,
                    ('S', true) => n / 60,
                    _ => return None,
                };
            }
        }
    }
    if !number.is_empty() {
        return None;
    }
    Some(if negative { minutes } else { -minutes })
}

fn alarm_minutes(alarm: &Component) -> Option<i64> {
    let trigger = alarm.line("TRIGGER")?;
    if trigger
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
        || trigger
            .param("RELATED")
            .is_some_and(|v| v.eq_ignore_ascii_case("END"))
    {
        return None;
    }
    trigger_minutes(&trigger.value)
}

fn duration(minutes: i64) -> String {
    let (sign, m) = if minutes >= 0 {
        ("-", minutes)
    } else {
        ("", -minutes)
    };
    if m == 0 {
        "PT0S".to_owned()
    } else if m % (24 * 60) == 0 {
        format!("{sign}P{}D", m / (24 * 60))
    } else {
        format!("{sign}PT{m}M")
    }
}

fn form_time(t: &EventTime) -> (String, Option<String>) {
    let value = t.value.as_str();
    if !value.contains('T') {
        return (value.to_owned(), None);
    }
    if let Some(local) = value.strip_suffix('Z').or_else(|| value.strip_suffix('z')) {
        return (local.to_owned(), Some("UTC".into()));
    }
    if value.len() > 19 {
        let utc = t
            .utc
            .and_then(DateTime::from_timestamp_millis)
            .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| value[..19].to_owned());
        return (utc, Some("UTC".into()));
    }
    (
        value.to_owned(),
        t.time_zone
            .as_ref()
            .map(|z| iana_zone(z).unwrap_or_else(|| z.clone())),
    )
}

fn person(p: &Participant) -> Person {
    Person {
        email: p.email.clone().unwrap_or_default(),
        name: p.name.clone(),
        partstat: p.partstat.clone(),
    }
}

fn main_component(item: &Item) -> Option<&Component> {
    item.components()
        .into_iter()
        .find(|c| c.line("RECURRENCE-ID").is_none())
}

/// Reads the event form of an iCalendar object of `kind` (an event or a task): its series, or
/// its first instance when there is no series. Alarms that are not relative to the start are
/// left out. `None` when the bytes cannot be read.
pub fn event_form(kind: ItemKind, raw: &[u8]) -> Option<EventForm> {
    let Normalized::Calendar(object) = normalize(kind, raw) else {
        return None;
    };
    let event = object
        .components
        .iter()
        .find(|e| e.recurrence_id.is_none())
        .or_else(|| object.components.first())?;
    let (start, zone) = event.start.as_ref().map(form_time).unwrap_or_default();
    let end = event.end.as_ref().map(|t| form_time(t).0);
    let item = Item::parse(kind, raw);
    let main = item.as_ref().and_then(main_component);
    let alarms = main
        .map(|c| {
            c.children
                .iter()
                .filter(|a| a.name == "VALARM")
                .filter_map(alarm_minutes)
                .map(|minutes_before| Alarm { minutes_before })
                .collect()
        })
        .unwrap_or_default();
    let recurrence = match main {
        Some(c) => c.line("RRULE").map(|l| l.value.trim().to_owned()),
        None => event.rrule.clone(),
    };
    Some(EventForm {
        uid: event.uid.clone(),
        title: event.summary.clone().unwrap_or_default(),
        start,
        end,
        all_day: event.all_day,
        time_zone: if event.all_day { None } else { zone },
        recurrence,
        location: event.location.clone(),
        description: event.description.clone(),
        organizer: event.organizer.as_ref().map(person),
        attendees: event.attendees.iter().map(person).collect(),
        alarms,
    })
}

fn text_line(name: &str, value: Option<&str>) -> Vec<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| vec![format!("{name}:{}", escape(v))])
        .unwrap_or_default()
}

fn zone_blocks(zone: &str, start: When, end: When) -> Vec<Component> {
    let local = |w: When| match w {
        When::Date(d) => d.format("%Y%m%dT000000").to_string(),
        When::Local(t) => t.format("%Y%m%dT%H%M%S").to_string(),
    };
    let scratch = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{PRODID}\r\nBEGIN:VEVENT\r\nUID:zone\r\nDTSTART;TZID={z}:{}\r\nDTEND;TZID={z}:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        local(start),
        local(end),
        z = parameter(zone)
    );
    let Ok(mut calendar) = calcard::icalendar::ICalendar::parse(&scratch) else {
        return Vec::new();
    };
    calendar.add_missing_timezones();
    lines::parse(calendar.to_string().as_bytes())
        .and_then(|mut top| top.pop())
        .map(|top| {
            top.children
                .into_iter()
                .filter(|c| c.name == "VTIMEZONE")
                .collect()
        })
        .unwrap_or_default()
}

fn replace_all(component: &mut Component, name: &str, values: &[String]) {
    let at = component
        .lines
        .iter()
        .position(|l| l.name == name)
        .unwrap_or(component.lines.len());
    component.lines.retain(|l| l.name != name);
    let new: Vec<Line> = values.iter().filter_map(|v| Line::parse(v)).collect();
    component.lines.splice(at..at, new);
}

fn set_line(component: &mut Component, name: &str, value: Option<String>) {
    let values: Vec<String> = value.into_iter().collect();
    replace_all(component, name, &values);
}

/// Writes `form` as an iCalendar object of `kind` (an event or a task). With a `base`, only
/// what the form changed is rewritten: everything else of the base (changed instances,
/// `VTIMEZONE`s, client properties, attendee parameters, alarms the form shows unchanged) stays
/// byte for byte in content, `DTSTAMP` and `LAST-MODIFIED` become `now` (milliseconds since
/// the epoch), and `SEQUENCE` goes up by one when the time or the recurrence changed. A named
/// time zone gets a `VTIMEZONE` when the object has none for it. The result passes
/// [`crate::validate`] against the base.
pub fn build_event(
    kind: ItemKind,
    form: &EventForm,
    base: Option<&[u8]>,
    now: i64,
) -> Result<Vec<u8>, FormError> {
    let new_times = times(form)?;
    if let Some(rule) = form.recurrence.as_deref().filter(|r| !r.trim().is_empty()) {
        check_rule(rule.trim())?;
    }
    if let Some(o) = &form.organizer {
        address(&o.email)?;
    }
    for a in &form.attendees {
        address(&a.email)?;
    }
    let base_item = match base {
        Some(raw) => Some(Item::parse(kind, raw).ok_or(Invalid::Unreadable)?),
        None => None,
    };
    let base_form = base.and_then(|raw| event_form(kind, raw));
    let mut item = match &base_item {
        Some(item) => item.clone(),
        None => {
            let uid = form
                .uid
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .ok_or(FormError::MissingUid)?;
            let name = crate::model::component_name(kind);
            let text = format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{PRODID}\r\nCALSCALE:GREGORIAN\r\nBEGIN:{name}\r\nUID:{}\r\nDTSTAMP:{s}\r\nCREATED:{s}\r\nEND:{name}\r\nEND:VCALENDAR\r\n",
                escape(uid),
                s = stamp(now)
            );
            Item::parse(kind, text.as_bytes()).ok_or(Invalid::Unreadable)?
        }
    };
    let name = crate::model::component_name(kind);
    let Some(main) = item
        .top
        .children
        .iter_mut()
        .find(|c| c.name == name && c.line("RECURRENCE-ID").is_none())
    else {
        return Err(Invalid::WrongComponent.into());
    };
    let old_alarms: Vec<Component> = main
        .children
        .iter()
        .filter(|c| c.name == "VALARM")
        .cloned()
        .collect();
    let old_attendees: Vec<Line> = main
        .lines
        .iter()
        .filter(|l| l.name == "ATTENDEE")
        .cloned()
        .collect();
    let old_organizer = main.line("ORGANIZER").cloned();
    let base_times = base_form.as_ref().and_then(|f| times(f).ok());
    let mut rescheduled = base_item.is_some();
    if base_times.as_ref() != Some(&new_times) {
        let end_name = if kind == ItemKind::Task {
            "DUE"
        } else {
            "DTEND"
        };
        set_line(
            main,
            "DTSTART",
            Some(time_line("DTSTART", new_times.start, &new_times.zone)),
        );
        set_line(main, "DURATION", None);
        set_line(
            main,
            end_name,
            Some(time_line(end_name, new_times.end, &new_times.zone)),
        );
    } else {
        rescheduled = false;
    }
    let clean = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let changed = |f: fn(&EventForm) -> Option<String>| match &base_form {
        Some(b) => clean(&f(b)) != clean(&f(form)),
        None => true,
    };
    if changed(|f| Some(f.title.clone())) {
        replace_all(main, "SUMMARY", &text_line("SUMMARY", Some(&form.title)));
    }
    if changed(|f| f.location.clone()) {
        replace_all(
            main,
            "LOCATION",
            &text_line("LOCATION", form.location.as_deref()),
        );
    }
    if changed(|f| f.description.clone()) {
        replace_all(
            main,
            "DESCRIPTION",
            &text_line("DESCRIPTION", form.description.as_deref()),
        );
    }
    if changed(|f| f.recurrence.clone()) {
        let rule = clean(&form.recurrence).map(|r| format!("RRULE:{r}"));
        set_line(main, "RRULE", rule);
        rescheduled |= base_item.is_some();
    }
    let organizer_same = match (
        &base_form.as_ref().and_then(|b| b.organizer.clone()),
        &form.organizer,
    ) {
        (Some(a), Some(b)) => same_person(b, a),
        (None, None) => true,
        _ => false,
    };
    if !organizer_same {
        let line = match &form.organizer {
            Some(o) => Some(person_line("ORGANIZER", o, old_organizer.as_ref())?),
            None => None,
        };
        set_line(main, "ORGANIZER", line);
    }
    let base_people: Vec<Person> = base_form
        .as_ref()
        .map(|b| b.attendees.clone())
        .unwrap_or_default();
    let mut attendees = Vec::new();
    for attendee in &form.attendees {
        let email = address(&attendee.email)?;
        let old = old_attendees
            .iter()
            .find(|l| normalize_email(&l.value).as_deref() == Some(email.as_str()));
        let old_person = base_people
            .iter()
            .find(|p| normalize_email(&p.email).as_deref() == Some(email.as_str()));
        match (old, old_person) {
            (Some(line), Some(p)) if same_person(attendee, p) => attendees.push(line.raw.clone()),
            (old, _) => attendees.push(person_line("ATTENDEE", attendee, old)?),
        }
    }
    let unchanged_attendees = attendees.len() == old_attendees.len()
        && attendees
            .iter()
            .zip(&old_attendees)
            .all(|(a, b)| *a == b.raw);
    if !unchanged_attendees {
        replace_all(main, "ATTENDEE", &attendees);
    }
    let old_minutes: Vec<Option<i64>> = old_alarms.iter().map(alarm_minutes).collect();
    let wanted: Vec<i64> = form.alarms.iter().map(|a| a.minutes_before).collect();
    let shown: Vec<i64> = old_minutes.iter().flatten().copied().collect();
    if shown != wanted {
        let mut used = vec![false; old_alarms.len()];
        let mut blocks = Vec::new();
        for minutes in &wanted {
            let reuse = old_minutes
                .iter()
                .enumerate()
                .position(|(i, m)| !used[i] && *m == Some(*minutes));
            match reuse {
                Some(i) => {
                    used[i] = true;
                    blocks.push(old_alarms[i].text());
                }
                None => blocks.push(format!(
                    "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:{}\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\n",
                    duration(*minutes)
                )),
            }
        }
        set_group(kind, main, "VALARM", &blocks);
    }
    if base_item.is_some() {
        set_line(main, "DTSTAMP", Some(format!("DTSTAMP:{}", stamp(now))));
        if main.line("LAST-MODIFIED").is_some() || rescheduled {
            set_line(
                main,
                "LAST-MODIFIED",
                Some(format!("LAST-MODIFIED:{}", stamp(now))),
            );
        }
        if rescheduled {
            let sequence = main
                .line("SEQUENCE")
                .and_then(|l| l.value.trim().parse::<i64>().ok())
                .unwrap_or(0);
            set_line(main, "SEQUENCE", Some(format!("SEQUENCE:{}", sequence + 1)));
        }
    }
    if let Zone::Named(zone) = &new_times.zone
        && !item.zones().iter().any(|(tzid, _)| tzid == zone)
    {
        let blocks: Vec<String> = zone_blocks(zone, new_times.start, new_times.end)
            .iter()
            .map(Component::text)
            .collect();
        crate::model::add_missing_zones(&mut item, &blocks);
    }
    let raw = item.write();
    validate(kind, &raw, base)?;
    Ok(raw)
}

pub(crate) fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn split_structured(text: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let (Some(next), Some(p)) = (chars.next(), parts.last_mut()) {
                    p.push('\\');
                    p.push(next);
                }
            }
            ';' => parts.push(String::new()),
            c => {
                if let Some(p) = parts.last_mut() {
                    p.push(c);
                }
            }
        }
    }
    parts.iter().map(|p| unescape(p)).collect()
}

fn opt(text: Option<&String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

const NOT_KINDS: &[&str] = &["internet", "pref", "x400"];

fn kinds(line: &Line) -> Vec<String> {
    let mut out: Vec<String> = line
        .params
        .iter()
        .filter(|(k, _)| k == "TYPE")
        .flat_map(|(_, v)| v.split(','))
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| !k.is_empty() && !NOT_KINDS.contains(&k.as_str()))
        .collect();
    out.dedup();
    out
}

fn clean_kinds(kinds: &[String]) -> Vec<String> {
    kinds
        .iter()
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .collect()
}

fn labeled(line: &Line) -> Labeled {
    let value = unescape(line.value.trim());
    let value = match value.get(..4) {
        Some(p) if p.eq_ignore_ascii_case("tel:") => value[4..].to_owned(),
        _ => value,
    };
    Labeled {
        value,
        kinds: kinds(line),
    }
}

fn postal(line: &Line) -> PostalAddress {
    let parts = split_structured(&line.value);
    let get = |i: usize| opt(parts.get(i));
    PostalAddress {
        kinds: kinds(line),
        po_box: get(0),
        extended: get(1),
        street: get(2),
        locality: get(3),
        region: get(4),
        postal_code: get(5),
        country: get(6),
    }
}

/// Reads the contact form of a vCard. `None` when the bytes cannot be read.
pub fn contact_form(raw: &[u8]) -> Option<ContactForm> {
    let item = Item::parse(ItemKind::Contact, raw)?;
    let card = &item.top;
    let text = |name: &str| {
        let parts: Vec<String> = card
            .lines
            .iter()
            .filter(|l| l.name == name)
            .map(|l| unescape(l.value.trim()))
            .filter(|v| !v.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    };
    let name = card
        .line("N")
        .map(|l| {
            let parts = split_structured(&l.value);
            NameParts {
                family: opt(parts.first()),
                given: opt(parts.get(1)),
                additional: opt(parts.get(2)),
                prefix: opt(parts.get(3)),
                suffix: opt(parts.get(4)),
            }
        })
        .unwrap_or_default();
    let organization = card.line("ORG").map(|l| {
        split_structured(&l.value)
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    });
    Some(ContactForm {
        uid: card.line("UID").map(|l| l.value.trim().to_owned()),
        full_name: text("FN"),
        name,
        emails: card
            .lines
            .iter()
            .filter(|l| l.name == "EMAIL")
            .map(labeled)
            .collect(),
        phones: card
            .lines
            .iter()
            .filter(|l| l.name == "TEL")
            .map(labeled)
            .collect(),
        addresses: card
            .lines
            .iter()
            .filter(|l| l.name == "ADR")
            .map(postal)
            .collect(),
        organization: organization.filter(|o| !o.is_empty()),
        title: text("TITLE"),
        notes: text("NOTE"),
    })
}

fn types(kinds: &[String]) -> String {
    let kinds = clean_kinds(kinds);
    if kinds.is_empty() {
        String::new()
    } else {
        format!(";TYPE={}", kinds.join(","))
    }
}

fn o(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(|v| escape(v.trim()))
        .unwrap_or_default()
}

fn same_labeled(a: &Labeled, b: &Labeled) -> bool {
    a.value.trim() == b.value.trim() && clean_kinds(&a.kinds) == clean_kinds(&b.kinds)
}

fn same_postal(a: &PostalAddress, b: &PostalAddress) -> bool {
    let c = |p: &PostalAddress| {
        let t = |v: &Option<String>| opt(v.as_ref());
        (
            clean_kinds(&p.kinds),
            t(&p.po_box),
            t(&p.extended),
            t(&p.street),
            t(&p.locality),
            t(&p.region),
            t(&p.postal_code),
            t(&p.country),
        )
    };
    c(a) == c(b)
}

fn reuse<T>(
    old: &[Line],
    wanted: &[T],
    read: impl Fn(&Line) -> T,
    equal: impl Fn(&T, &T) -> bool,
    write: impl Fn(&T) -> String,
) -> Option<Vec<String>> {
    let mut used = vec![false; old.len()];
    let mut out = Vec::new();
    for entry in wanted {
        match old
            .iter()
            .enumerate()
            .position(|(i, l)| !used[i] && equal(&read(l), entry))
        {
            Some(i) => {
                used[i] = true;
                out.push(old[i].raw.clone());
            }
            None => out.push(write(entry)),
        }
    }
    let same = out.len() == old.len() && out.iter().zip(old).all(|(a, b)| *a == b.raw);
    (!same).then_some(out)
}

fn display_name(form: &ContactForm) -> Option<String> {
    if let Some(full) = opt(form.full_name.as_ref()) {
        return Some(full);
    }
    let n = &form.name;
    let parts: Vec<String> = [&n.prefix, &n.given, &n.additional, &n.family, &n.suffix]
        .into_iter()
        .filter_map(|p| opt(p.as_ref()))
        .collect();
    if !parts.is_empty() {
        return Some(parts.join(" "));
    }
    opt(form.organization.as_ref()).or_else(|| form.emails.iter().find_map(|e| opt(Some(&e.value))))
}

/// Writes `form` as a vCard. A new card is vCard 3.0, which every CardDAV server takes. With a
/// `base`, only the properties the form changed are rewritten and the rest stays (its version,
/// pictures, birthdays, client properties, and the lines of unchanged emails, phones and
/// addresses with their Apple `itemN.` labels); a label left without its property is dropped.
/// `REV` becomes `now` (milliseconds since the epoch). The result passes [`crate::validate`]
/// against the base.
pub fn build_contact(
    form: &ContactForm,
    base: Option<&[u8]>,
    now: i64,
) -> Result<Vec<u8>, FormError> {
    let full_name = display_name(form).ok_or(FormError::Empty)?;
    for email in &form.emails {
        address(&email.value)?;
    }
    let base_form = match base {
        Some(raw) => Some(contact_form(raw).ok_or(Invalid::Unreadable)?),
        None => None,
    };
    let mut item = match base {
        Some(raw) => Item::parse(ItemKind::Contact, raw).ok_or(Invalid::Unreadable)?,
        None => {
            let uid = form
                .uid
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .ok_or(FormError::MissingUid)?;
            let text = format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\n{PRODID}\r\nUID:{}\r\nEND:VCARD\r\n",
                escape(uid)
            );
            Item::parse(ItemKind::Contact, text.as_bytes()).ok_or(Invalid::Unreadable)?
        }
    };
    let card = &mut item.top;
    let old = |name: &str, card: &Component| -> Vec<Line> {
        card.lines
            .iter()
            .filter(|l| l.name == name)
            .cloned()
            .collect()
    };
    let base_full = base_form.as_ref().and_then(display_name);
    if base_full.as_deref() != Some(full_name.as_str()) {
        set_line(card, "FN", Some(format!("FN:{}", escape(&full_name))));
    }
    if base_form.as_ref().map(|b| &b.name) != Some(&form.name) || card.line("N").is_none() {
        let n = &form.name;
        set_line(
            card,
            "N",
            Some(format!(
                "N:{};{};{};{};{}",
                o(&n.family),
                o(&n.given),
                o(&n.additional),
                o(&n.prefix),
                o(&n.suffix)
            )),
        );
    }
    let emails = reuse(
        &old("EMAIL", card),
        &form.emails,
        labeled,
        same_labeled,
        |e| format!("EMAIL{}:{}", types(&e.kinds), escape(e.value.trim())),
    );
    if let Some(lines) = emails {
        replace_all(card, "EMAIL", &lines);
    }
    let phones = reuse(
        &old("TEL", card),
        &form.phones,
        labeled,
        same_labeled,
        |p| format!("TEL{}:{}", types(&p.kinds), escape(p.value.trim())),
    );
    if let Some(lines) = phones {
        replace_all(card, "TEL", &lines);
    }
    let addresses = reuse(
        &old("ADR", card),
        &form.addresses,
        postal,
        same_postal,
        |a| {
            format!(
                "ADR{}:{};{};{};{};{};{};{}",
                types(&a.kinds),
                o(&a.po_box),
                o(&a.extended),
                o(&a.street),
                o(&a.locality),
                o(&a.region),
                o(&a.postal_code),
                o(&a.country)
            )
        },
    );
    if let Some(lines) = addresses {
        replace_all(card, "ADR", &lines);
    }
    let changed = |f: fn(&ContactForm) -> Option<String>| match &base_form {
        Some(b) => opt(f(b).as_ref()) != opt(f(form).as_ref()),
        None => true,
    };
    if changed(|f| f.organization.clone()) {
        replace_all(card, "ORG", &text_line("ORG", form.organization.as_deref()));
    }
    if changed(|f| f.title.clone()) {
        replace_all(card, "TITLE", &text_line("TITLE", form.title.as_deref()));
    }
    if changed(|f| f.notes.clone()) {
        replace_all(card, "NOTE", &text_line("NOTE", form.notes.as_deref()));
    }
    let groups: Vec<String> = card
        .lines
        .iter()
        .filter(|l| !l.name.starts_with("X-AB"))
        .filter_map(|l| l.group().map(str::to_owned))
        .collect();
    card.lines.retain(|l| {
        !l.name.starts_with("X-AB") || l.group().is_none_or(|g| groups.iter().any(|x| x == g))
    });
    set_line(card, "REV", Some(format!("REV:{}", stamp(now))));
    let raw = item.write();
    validate(ItemKind::Contact, &raw, base)?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_alarm_offsets() {
        assert_eq!(trigger_minutes("-PT15M"), Some(15));
        assert_eq!(trigger_minutes("-P1DT2H"), Some(1560));
        assert_eq!(trigger_minutes("PT0S"), Some(0));
        assert_eq!(trigger_minutes("PT5M"), Some(-5));
        assert_eq!(trigger_minutes("-P1W"), Some(10080));
        assert_eq!(trigger_minutes("15 minutes"), None);
        assert_eq!(duration(15), "-PT15M");
        assert_eq!(duration(2880), "-P2D");
        assert_eq!(duration(0), "PT0S");
        assert_eq!(duration(-5), "PT5M");
    }

    #[test]
    fn splits_structured_values_on_unescaped_semicolons() {
        assert_eq!(
            split_structured(r"Example;Ada\;Bea;;Dr.\, PhD"),
            vec!["Example", "Ada;Bea", "", "Dr., PhD"]
        );
        assert_eq!(unescape(r"a\nb\,c"), "a\nb,c");
    }
}
