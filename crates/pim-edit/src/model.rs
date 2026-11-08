use std::collections::BTreeMap;

use holtmail_collection::Field;
use holtmail_pim::{ItemKind, normalize_email};

use crate::lines::{self, Component, Line};

pub(crate) const DROP_PARAMS: &[&str] = &["SCHEDULE-STATUS"];
pub(crate) const ANSWER_PARAMS: &[&str] = &["SCHEDULE-STATUS", "PARTSTAT", "RSVP"];
pub(crate) const INSTANCE_GROUPS: &[&str] = &[
    "RECURRENCE-ID",
    "DTSTART",
    "DTEND",
    "DURATION",
    "DUE",
    "RRULE",
    "RDATE",
    "EXDATE",
    "EXRULE",
];
const UPSTREAM_WINS: &[&str] = &[
    "DTSTART", "DTEND", "DURATION", "DUE", "RRULE", "RDATE", "EXDATE", "EXRULE",
];

pub(crate) fn component_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Contact => "VCARD",
        ItemKind::Event => "VEVENT",
        ItemKind::Task => "VTODO",
        ItemKind::Other => "VJOURNAL",
    }
}

pub(crate) fn is_calendar_component(name: &str) -> bool {
    matches!(name, "VEVENT" | "VTODO" | "VJOURNAL" | "VFREEBUSY")
}

/// One parsed item: the `VCALENDAR` or the `VCARD`.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub kind: ItemKind,
    pub top: Component,
}

pub(crate) type Groups = BTreeMap<String, Vec<String>>;

pub(crate) struct Part<'a> {
    pub key: String,
    pub component: &'a Component,
    pub groups: Groups,
}

pub(crate) fn address_key(value: &str) -> String {
    normalize_email(value).unwrap_or_else(|| value.trim().to_lowercase())
}

pub(crate) fn instance_key(line: &Line) -> String {
    format!(
        "{}:{}",
        line.param("TZID").unwrap_or_default(),
        line.value.trim()
    )
}

impl Item {
    pub fn parse(kind: ItemKind, raw: &[u8]) -> Option<Item> {
        let mut top = lines::parse(raw)?;
        let want = if kind == ItemKind::Contact {
            "VCARD"
        } else {
            "VCALENDAR"
        };
        if top.len() != 1 || top[0].name != want {
            return None;
        }
        Some(Item {
            kind,
            top: top.remove(0),
        })
    }

    pub fn is_contact(&self) -> bool {
        self.kind == ItemKind::Contact
    }

    pub fn components(&self) -> Vec<&Component> {
        if self.is_contact() {
            vec![&self.top]
        } else {
            self.top
                .children
                .iter()
                .filter(|c| c.name == component_name(self.kind))
                .collect()
        }
    }

    pub fn component_key(&self, component: &Component) -> String {
        if self.is_contact() {
            return String::new();
        }
        component
            .line("RECURRENCE-ID")
            .map(instance_key)
            .unwrap_or_default()
    }

    pub fn parts(&self) -> Vec<Part<'_>> {
        self.components()
            .into_iter()
            .map(|component| Part {
                key: self.component_key(component),
                component,
                groups: groups(self.kind, component),
            })
            .collect()
    }

    pub fn uids(&self) -> Vec<Option<String>> {
        self.components()
            .into_iter()
            .map(|c| c.line("UID").map(|l| l.value.trim().to_owned()))
            .collect()
    }

    pub fn zones(&self) -> Vec<(String, String)> {
        self.top
            .children
            .iter()
            .filter(|c| c.name == "VTIMEZONE")
            .filter_map(|c| Some((c.line("TZID")?.value.trim().to_owned(), c.text())))
            .collect()
    }

    pub fn write(&self) -> Vec<u8> {
        lines::write_all(std::slice::from_ref(&self.top))
    }
}

pub(crate) fn ignored(kind: ItemKind, name: &str) -> bool {
    if kind == ItemKind::Contact {
        matches!(name, "VERSION" | "PRODID" | "REV" | "UID")
    } else {
        matches!(
            name,
            "DTSTAMP" | "LAST-MODIFIED" | "CREATED" | "SEQUENCE" | "UID" | "X-MOZ-GENERATION"
        )
    }
}

pub(crate) fn line_group(kind: ItemKind, line: &Line) -> Option<String> {
    if ignored(kind, &line.name) {
        return None;
    }
    if kind != ItemKind::Contact && line.name == "ATTENDEE" {
        return Some(format!("ATTENDEE {}", address_key(&line.value)));
    }
    Some(line.name.clone())
}

pub(crate) fn child_group(child: &Component) -> String {
    if child.name == "VALARM" {
        "VALARM".to_owned()
    } else {
        format!("BEGIN:{}", child.name)
    }
}

pub(crate) fn is_block(group: &str) -> bool {
    group == "VALARM" || group.starts_with("BEGIN:")
}

pub(crate) fn groups(kind: ItemKind, component: &Component) -> Groups {
    let mut out = Groups::new();
    for line in &component.lines {
        if let Some(group) = line_group(kind, line) {
            out.entry(group).or_default().push(line.raw.clone());
        }
    }
    for child in &component.children {
        out.entry(child_group(child))
            .or_default()
            .push(child.text());
    }
    out
}

pub(crate) fn canonical(group: &str, values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = if is_block(group) {
        values
            .iter()
            .map(|block| match lines::parse(block.as_bytes()) {
                Some(parsed) => parsed
                    .iter()
                    .map(|c| {
                        c.lines
                            .iter()
                            .map(|l| l.canonical(DROP_PARAMS))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => block.clone(),
            })
            .collect()
    } else {
        values
            .iter()
            .map(|raw| Line::parse(raw).map_or_else(|| raw.clone(), |l| l.canonical(DROP_PARAMS)))
            .collect()
    };
    out.sort();
    out
}

pub(crate) fn same(group: &str, a: &[String], b: &[String]) -> bool {
    canonical(group, a) == canonical(group, b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    Personal,
    AttendeeAllowed,
    Other,
}

pub(crate) fn category(kind: ItemKind, group: &str) -> Category {
    let personal = group == "VALARM"
        || ["X-MOZ-", "X-APPLE-", "X-EVOLUTION-"]
            .iter()
            .any(|p| group.starts_with(p))
        || (kind == ItemKind::Contact && group.starts_with("X-AB"));
    if personal {
        Category::Personal
    } else if kind != ItemKind::Contact
        && matches!(group, "TRANSP" | "PERCENT-COMPLETE" | "COMPLETED")
    {
        Category::AttendeeAllowed
    } else {
        Category::Other
    }
}

pub(crate) fn upstream_wins(group: &str, theirs: &[String]) -> bool {
    if UPSTREAM_WINS.contains(&group) || group.starts_with("ATTENDEE ") {
        return true;
    }
    group == "STATUS"
        && theirs.iter().any(|raw| {
            Line::parse(raw).is_some_and(|l| l.value.trim().eq_ignore_ascii_case("CANCELLED"))
        })
}

pub(crate) fn field(kind: ItemKind, group: &str) -> Field {
    let name = group.split(' ').next().unwrap_or(group);
    if kind == ItemKind::Contact {
        return match name {
            "FN" | "N" | "NICKNAME" => Field::Name,
            "EMAIL" => Field::Emails,
            "TEL" => Field::Phones,
            "ADR" => Field::Addresses,
            "ORG" => Field::Organization,
            "TITLE" | "ROLE" => Field::Title,
            "NOTE" => Field::Notes,
            "BDAY" | "ANNIVERSARY" => Field::Dates,
            "PHOTO" => Field::Photo,
            "URL" => Field::Urls,
            "CATEGORIES" => Field::Categories,
            other => Field::Property(other.to_owned()),
        };
    }
    match name {
        "SUMMARY" => Field::Summary,
        "DTSTART" => Field::Start,
        "DTEND" | "DURATION" | "DUE" => Field::End,
        "RRULE" | "RDATE" | "EXDATE" | "EXRULE" | "RECURRENCE-ID" => Field::Recurrence,
        "LOCATION" => Field::Location,
        "DESCRIPTION" => Field::Description,
        "STATUS" => Field::Status,
        "ORGANIZER" => Field::Organizer,
        "ATTENDEE" => Field::Attendees,
        "VALARM" => Field::Alarms,
        "CATEGORIES" => Field::Categories,
        "TRANSP" => Field::Transparency,
        "CLASS" => Field::Privacy,
        other => Field::Property(other.trim_start_matches("BEGIN:").to_owned()),
    }
}

/// Replaces the lines or children of `group` in `component` with `values`, keeping the place of
/// the first line it removes.
pub(crate) fn set_group(kind: ItemKind, component: &mut Component, group: &str, values: &[String]) {
    if is_block(group) {
        component.children.retain(|c| child_group(c) != group);
        for block in values {
            if let Some(parsed) = lines::parse(block.as_bytes()) {
                component.children.extend(parsed);
            }
        }
        return;
    }
    let mut at = None;
    let mut index = 0;
    component.lines.retain(|line| {
        let keep = line_group(kind, line).as_deref() != Some(group);
        if !keep && at.is_none() {
            at = Some(index);
        }
        if keep {
            index += 1;
        }
        keep
    });
    let at = at.unwrap_or(component.lines.len());
    let new: Vec<Line> = values.iter().filter_map(|raw| Line::parse(raw)).collect();
    component.lines.splice(at..at, new);
}

pub(crate) fn tzids(component: &Component, out: &mut Vec<String>) {
    for line in &component.lines {
        if let Some(tzid) = line.param("TZID")
            && !out.iter().any(|t| t == tzid)
        {
            out.push(tzid.to_owned());
        }
    }
    for child in &component.children {
        tzids(child, out);
    }
}

pub(crate) fn add_missing_zones(item: &mut Item, extra: &[String]) {
    if item.is_contact() {
        return;
    }
    let mut wanted = Vec::new();
    for child in item.top.children.iter().filter(|c| c.name != "VTIMEZONE") {
        tzids(child, &mut wanted);
    }
    let have: Vec<String> = item.zones().into_iter().map(|(t, _)| t).collect();
    for tzid in wanted.iter().filter(|t| !have.contains(t)) {
        let found = extra.iter().find_map(|block| {
            let parsed = lines::parse(block.as_bytes())?;
            parsed
                .into_iter()
                .find(|c| c.line("TZID").is_some_and(|l| l.value.trim() == tzid))
        });
        if let Some(zone) = found {
            let at = item
                .top
                .children
                .iter()
                .position(|c| c.name != "VTIMEZONE")
                .unwrap_or(item.top.children.len());
            item.top.children.insert(at, zone);
        }
    }
}
