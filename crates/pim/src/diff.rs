use serde::{Deserialize, Serialize};

use crate::{CalendarObject, Contact, Event, EventTime, Normalized, Participant, Photo};

/// One difference between two versions of an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The field, such as `email` or `start`; fields of a changed instance are prefixed with
    /// `instance <RECURRENCE-ID> `.
    pub field: String,
    /// The value before; `None` when it was added.
    pub before: Option<String>,
    /// The value after; `None` when it was removed.
    pub after: Option<String>,
}

/// What changed from `before` to `after`, field by field.
///
/// A field with at most one value on each side gives one change from the old value to the new.
/// A field with several values, such as emails or attendees, is compared as a set: each value
/// that disappeared is a change with only `before`, each new one a change with only `after`, and
/// a reordering is no change. `DTSTAMP` is left out, since servers rewrite it on every export.
/// Equal versions give no changes. Changes come in the order of the fields in [`Normalized`].
pub fn diff(before: &Normalized, after: &Normalized) -> Vec<Change> {
    let old = before.fields();
    let new = after.fields();
    let mut names: Vec<&str> = Vec::new();
    for (name, _) in old.iter().chain(new.iter()) {
        if !names.contains(&name.as_str()) {
            names.push(name);
        }
    }
    let mut changes = Vec::new();
    for name in names {
        let a: Vec<&String> = old
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v)
            .collect();
        let b: Vec<&String> = new
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v)
            .collect();
        if a.len() <= 1 && b.len() <= 1 {
            if a.first() != b.first() {
                changes.push(Change {
                    field: name.to_owned(),
                    before: a.first().map(|v| (*v).clone()),
                    after: b.first().map(|v| (*v).clone()),
                });
            }
            continue;
        }
        let mut left = b.clone();
        for value in &a {
            match left.iter().position(|v| v == value) {
                Some(at) => {
                    left.remove(at);
                }
                None => changes.push(Change {
                    field: name.to_owned(),
                    before: Some((*value).clone()),
                    after: None,
                }),
            }
        }
        let mut right = a.clone();
        for value in &b {
            match right.iter().position(|v| v == value) {
                Some(at) => {
                    right.remove(at);
                }
                None => changes.push(Change {
                    field: name.to_owned(),
                    before: None,
                    after: Some((*value).clone()),
                }),
            }
        }
    }
    changes
}

fn push(out: &mut Vec<(String, String)>, name: &str, value: Option<impl ToString>) {
    if let Some(value) = value {
        out.push((name.to_owned(), value.to_string()));
    }
}

fn with_kinds(value: &str, kinds: &[String]) -> String {
    if kinds.is_empty() {
        value.to_owned()
    } else {
        format!("{value} ({})", kinds.join(", "))
    }
}

impl Normalized {
    /// Every field with a value as `(name, value)` pairs, a name repeated for several values.
    /// This is what [`diff`] compares.
    pub fn fields(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        match self {
            Normalized::Contact(contact) => contact_fields(contact, &mut out),
            Normalized::Calendar(calendar) => calendar_fields(calendar, &mut out),
            Normalized::Unreadable => out.push(("unreadable".into(), "yes".into())),
        }
        out
    }
}

fn contact_fields(c: &Contact, out: &mut Vec<(String, String)>) {
    push(out, "full_name", c.full_name.as_ref());
    push(out, "name", c.name.as_ref().map(|n| n.formatted()));
    for nickname in &c.nicknames {
        push(out, "nickname", Some(nickname));
    }
    for email in &c.emails {
        push(out, "email", Some(with_kinds(&email.address, &email.kinds)));
    }
    for phone in &c.phones {
        push(out, "phone", Some(with_kinds(&phone.number, &phone.kinds)));
    }
    push(out, "organization", c.organization.as_ref());
    push(out, "title", c.title.as_ref());
    for address in &c.addresses {
        push(
            out,
            "address",
            Some(with_kinds(&address.formatted(), &address.kinds)),
        );
    }
    push(out, "birthday", c.birthday.as_ref());
    push(out, "note", c.note.as_ref());
    push(
        out,
        "photo",
        c.photo.as_ref().map(|p| match p {
            Photo::Embedded { blob, .. } => format!("picture {blob}"),
            Photo::Link { url } => url.clone(),
        }),
    );
    push(out, "uid", c.uid.as_ref());
    push(out, "rev", c.rev.as_ref());
}

fn time(t: &EventTime) -> String {
    match &t.time_zone {
        Some(zone) if !t.value.ends_with('Z') && !t.value.ends_with(zone.as_str()) => {
            format!("{} {zone}", t.value)
        }
        _ => t.value.clone(),
    }
}

pub(crate) fn person(p: &Participant) -> String {
    let who = match (&p.name, &p.email) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (None, Some(email)) => email.clone(),
        (Some(name), None) => name.clone(),
        (None, None) => String::new(),
    };
    match &p.partstat {
        Some(state) => format!("{who} ({})", state.to_ascii_lowercase()),
        None => who,
    }
}

fn calendar_fields(calendar: &CalendarObject, out: &mut Vec<(String, String)>) {
    for event in &calendar.components {
        let prefix = event
            .recurrence_id
            .as_ref()
            .map(|r| format!("instance {} ", time(r)))
            .unwrap_or_default();
        event_fields(event, &prefix, out);
    }
}

fn event_fields(e: &Event, prefix: &str, out: &mut Vec<(String, String)>) {
    let name = |field: &str| format!("{prefix}{field}");
    push(out, &name("kind"), Some(e.kind.as_str()));
    push(out, &name("summary"), e.summary.as_ref());
    push(out, &name("description"), e.description.as_ref());
    push(out, &name("location"), e.location.as_ref());
    push(out, &name("start"), e.start.as_ref().map(time));
    push(out, &name("end"), e.end.as_ref().map(time));
    push(out, &name("recurring"), e.recurring.then_some("yes"));
    push(out, &name("rule"), e.rrule.as_ref());
    for rdate in &e.rdates {
        push(out, &name("rdate"), Some(time(rdate)));
    }
    for exdate in &e.exdates {
        push(out, &name("exdate"), Some(time(exdate)));
    }
    push(out, &name("organizer"), e.organizer.as_ref().map(person));
    for attendee in &e.attendees {
        push(out, &name("attendee"), Some(person(attendee)));
    }
    push(out, &name("sequence"), e.sequence);
    push(out, &name("status"), e.status.as_ref());
    push(out, &name("uid"), e.uid.as_ref());
}
