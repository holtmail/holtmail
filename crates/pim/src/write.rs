use std::borrow::Cow;

use chrono::DateTime;

use crate::contact::Contact;
use crate::event::{CalendarObject, Event, EventTime, Participant, iana_zone};
use crate::{Format, ItemKind};

const PRODID: &str = "-//Holtmail//Export//EN";

/// One item version as a vCard (contacts) or an iCalendar object (the rest), for writing it into
/// a `.vcf` or `.ics` file.
///
/// vCard and iCalendar bytes come back unchanged. JSContact is converted by `calcard`, the way it
/// is read. Graph contacts are written from the normalized model as vCard 4.0 with the Graph `id`
/// as `UID`; Graph events and JSCalendar objects as iCalendar 2.0 from the model, so changed
/// instances carry what they inherit from the series (a JSCalendar override is a patch), with
/// times in the zone the source gave (its IANA name when one is known, else the name as written,
/// without a `VTIMEZONE`). What the model does not hold, such as pictures, categories and alarms,
/// is left out. `None` when the
/// bytes cannot be read in `format`, or `format` does not fit `kind`.
pub fn as_text(format: Format, kind: ItemKind, raw: &[u8]) -> Option<Cow<'_, [u8]>> {
    if format.is_contact() != (kind == ItemKind::Contact) {
        return None;
    }
    match format {
        Format::VCard | Format::ICalendar => Some(Cow::Borrowed(raw)),
        Format::JsContact => crate::js::vcard(raw).map(|t| Cow::Owned(t.into_bytes())),
        Format::JsCalendar => {
            crate::js::calendar(raw).map(|o| Cow::Owned(icalendar(&o).into_bytes()))
        }
        Format::GraphContact => {
            let mut contact = crate::graph::contact(raw)?;
            if contact.uid.is_none() {
                contact.uid = serde_json::from_slice::<serde_json::Value>(raw)
                    .ok()
                    .and_then(|v| v.get("id")?.as_str().map(str::to_owned));
            }
            Some(Cow::Owned(vcard(&contact).into_bytes()))
        }
        Format::GraphEvent => {
            crate::graph::event(raw).map(|o| Cow::Owned(icalendar(&o).into_bytes()))
        }
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn parameter(value: &str) -> String {
    let clean: String = value
        .chars()
        .filter(|c| *c != '"' && !c.is_control())
        .collect();
    if clean.contains([':', ';', ',']) {
        format!("\"{clean}\"")
    } else {
        clean
    }
}

/// Appends `line` folded at 75 octets, never inside a character, and a CRLF. Line breaks inside
/// `line` are dropped, so no value can start a property of its own.
fn push(out: &mut String, line: &str) {
    let mut width = 0;
    for c in line.chars().filter(|c| !matches!(c, '\r' | '\n')) {
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str("\r\n");
}

fn types(kinds: &[String]) -> String {
    let kinds: Vec<String> = kinds
        .iter()
        .filter(|k| !k.is_empty())
        .map(|k| parameter(k))
        .collect();
    if kinds.is_empty() {
        String::new()
    } else {
        format!(";TYPE={}", kinds.join(","))
    }
}

fn birthday(value: &str) -> String {
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    match value.split('-').collect::<Vec<_>>().as_slice() {
        [y, m, d] if y.len() == 4 && m.len() == 2 && d.len() == 2 && digits(y) => {
            format!("BDAY:{y}{m}{d}")
        }
        ["", "", m, d] if m.len() == 2 && d.len() == 2 && digits(m) && digits(d) => {
            format!("BDAY:--{m}{d}")
        }
        _ => format!("BDAY;VALUE=text:{}", escape(value)),
    }
}

fn opt(value: &Option<String>) -> String {
    value.as_deref().map(escape).unwrap_or_default()
}

pub(crate) fn vcard(contact: &Contact) -> String {
    let mut out = String::new();
    push(&mut out, "BEGIN:VCARD");
    push(&mut out, "VERSION:4.0");
    push(&mut out, &format!("PRODID:{PRODID}"));
    if let Some(uid) = &contact.uid {
        push(&mut out, &format!("UID:{}", escape(uid)));
    }
    push(
        &mut out,
        &format!("FN:{}", escape(&contact.display_name().unwrap_or_default())),
    );
    if let Some(name) = &contact.name {
        push(
            &mut out,
            &format!(
                "N:{};{};{};{};{}",
                opt(&name.family),
                opt(&name.given),
                opt(&name.additional),
                opt(&name.prefix),
                opt(&name.suffix)
            ),
        );
    }
    for nickname in &contact.nicknames {
        push(&mut out, &format!("NICKNAME:{}", escape(nickname)));
    }
    for email in &contact.emails {
        push(
            &mut out,
            &format!("EMAIL{}:{}", types(&email.kinds), escape(&email.address)),
        );
    }
    for phone in &contact.phones {
        push(
            &mut out,
            &format!("TEL{}:{}", types(&phone.kinds), escape(&phone.number)),
        );
    }
    if let Some(organization) = &contact.organization {
        push(&mut out, &format!("ORG:{}", escape(organization)));
    }
    if let Some(title) = &contact.title {
        push(&mut out, &format!("TITLE:{}", escape(title)));
    }
    for address in &contact.addresses {
        push(
            &mut out,
            &format!(
                "ADR{}:{};{};{};{};{};{};{}",
                types(&address.kinds),
                opt(&address.po_box),
                opt(&address.extended),
                opt(&address.street),
                opt(&address.locality),
                opt(&address.region),
                opt(&address.postal_code),
                opt(&address.country)
            ),
        );
    }
    if let Some(value) = &contact.birthday {
        push(&mut out, &birthday(value));
    }
    if let Some(note) = &contact.note {
        push(&mut out, &format!("NOTE:{}", escape(note)));
    }
    if let Some(crate::Photo::Link { url }) = &contact.photo {
        push(&mut out, &format!("PHOTO:{url}"));
    }
    if let Some(rev) = &contact.rev
        && let Ok(at) = DateTime::parse_from_rfc3339(rev)
    {
        push(
            &mut out,
            &format!("REV:{}", at.to_utc().format("%Y%m%dT%H%M%SZ")),
        );
    }
    push(&mut out, "END:VCARD");
    out
}

fn utc_text(millis: i64) -> Option<String> {
    DateTime::from_timestamp_millis(millis).map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
}

fn compact(value: &str) -> String {
    value.chars().filter(|c| *c != '-' && *c != ':').collect()
}

/// `NAME;params:value` for an event time.
fn time_line(name: &str, time: &EventTime, extra: &str) -> Option<String> {
    let value = time.value.as_str();
    if !value.contains('T') {
        return Some(format!("{name};VALUE=DATE{extra}:{}", compact(value)));
    }
    let at = value.find('T')? + 9;
    let (local, rest) = (value.get(..at)?, value.get(at..)?);
    if rest.eq_ignore_ascii_case("z") || time.time_zone.as_deref() == Some("UTC") {
        return Some(format!("{name}{extra}:{}Z", compact(local)));
    }
    if !rest.is_empty() {
        return Some(format!("{name}{extra}:{}", utc_text(time.utc?)?));
    }
    match &time.time_zone {
        Some(zone) => {
            let zone = iana_zone(zone).unwrap_or_else(|| zone.clone());
            Some(format!(
                "{name};TZID={}{extra}:{}",
                parameter(&zone),
                compact(local)
            ))
        }
        None => Some(format!("{name}{extra}:{}", compact(local))),
    }
}

fn participant(name: &str, who: &Participant) -> Option<String> {
    let address = who.email.as_deref()?;
    let address = if address.contains(':') || !address.contains('@') {
        address.to_owned()
    } else {
        format!("mailto:{address}")
    };
    let mut line = name.to_owned();
    if let Some(cn) = &who.name {
        line.push_str(&format!(";CN={}", parameter(cn)));
    }
    if let Some(role) = &who.role {
        line.push_str(&format!(";ROLE={}", parameter(role)));
    }
    if let Some(partstat) = &who.partstat {
        line.push_str(&format!(";PARTSTAT={}", parameter(partstat)));
    }
    Some(format!("{line}:{address}"))
}

fn component(out: &mut String, event: &Event) {
    let name = match event.kind {
        ItemKind::Task => "VTODO",
        ItemKind::Event | ItemKind::Contact => "VEVENT",
        ItemKind::Other => "VJOURNAL",
    };
    push(out, &format!("BEGIN:{name}"));
    if let Some(uid) = &event.uid {
        push(out, &format!("UID:{}", escape(uid)));
    }
    let stamp = event
        .dtstamp
        .or(event.last_modified)
        .and_then(utc_text)
        .unwrap_or_else(|| "19700101T000000Z".into());
    push(out, &format!("DTSTAMP:{stamp}"));
    if let Some(id) = &event.recurrence_id {
        let range = if event.this_and_future {
            ";RANGE=THISANDFUTURE"
        } else {
            ""
        };
        if let Some(line) = time_line("RECURRENCE-ID", id, range) {
            push(out, &line);
        }
    }
    if let Some(line) = event
        .start
        .as_ref()
        .and_then(|t| time_line("DTSTART", t, ""))
    {
        push(out, &line);
    }
    let end = if event.kind == ItemKind::Task {
        "DUE"
    } else {
        "DTEND"
    };
    if let Some(line) = event.end.as_ref().and_then(|t| time_line(end, t, "")) {
        push(out, &line);
    }
    for (property, value) in [
        ("SUMMARY", &event.summary),
        ("DESCRIPTION", &event.description),
        ("LOCATION", &event.location),
    ] {
        if let Some(value) = value {
            push(out, &format!("{property}:{}", escape(value)));
        }
    }
    if let Some(rule) = &event.rrule {
        push(out, &format!("RRULE:{rule}"));
    }
    for (property, times) in [("RDATE", &event.rdates), ("EXDATE", &event.exdates)] {
        for time in times {
            if let Some(line) = time_line(property, time, "") {
                push(out, &line);
            }
        }
    }
    if let Some(line) = event
        .organizer
        .as_ref()
        .and_then(|o| participant("ORGANIZER", o))
    {
        push(out, &line);
    }
    for attendee in &event.attendees {
        if let Some(line) = participant("ATTENDEE", attendee) {
            push(out, &line);
        }
    }
    if let Some(sequence) = event.sequence {
        push(out, &format!("SEQUENCE:{sequence}"));
    }
    if let Some(status) = &event.status {
        push(out, &format!("STATUS:{}", escape(status)));
    }
    if let Some(modified) = event.last_modified.and_then(utc_text) {
        push(out, &format!("LAST-MODIFIED:{modified}"));
    }
    push(out, &format!("END:{name}"));
}

pub(crate) fn icalendar(object: &CalendarObject) -> String {
    let mut out = String::new();
    push(&mut out, "BEGIN:VCALENDAR");
    push(&mut out, "VERSION:2.0");
    push(&mut out, &format!("PRODID:{PRODID}"));
    for event in &object.components {
        component(&mut out, event);
    }
    push(&mut out, "END:VCALENDAR");
    out
}

/// Writes calendar items into one iCalendar object, for an `.ics` export: [`IcsWriter::begin`],
/// then each item through [`IcsWriter::native`] or [`IcsWriter::object`], then
/// [`IcsWriter::end`].
///
/// A `VTIMEZONE` is written once per `TZID`, the first definition seen winning, right before the
/// first component that names it, so a reader going through the file once knows every zone before
/// it is used.
#[derive(Debug, Default)]
pub struct IcsWriter {
    zones: std::collections::HashSet<String>,
}

impl IcsWriter {
    /// A writer that has written no zone yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The object's opening: `BEGIN:VCALENDAR`, `VERSION`, `PRODID` and `CALSCALE`.
    pub fn begin(&self) -> Vec<u8> {
        let mut out = String::new();
        push(&mut out, "BEGIN:VCALENDAR");
        push(&mut out, "VERSION:2.0");
        push(&mut out, &format!("PRODID:{PRODID}"));
        push(&mut out, "CALSCALE:GREGORIAN");
        out.into_bytes()
    }

    /// The object's closing line.
    pub fn end(&self) -> &'static [u8] {
        b"END:VCALENDAR\r\n"
    }

    /// The components of a stored iCalendar object, byte for byte, without its `VCALENDAR` lines
    /// (`METHOD`, `PRODID` and the rest are dropped) and preceded by the `VTIMEZONE`s they name
    /// that were not written yet. A line end is added where a component lacks one. `None` when the
    /// bytes hold no component.
    pub fn native(&mut self, raw: &[u8]) -> Option<Vec<u8>> {
        let parts = crate::split::calendar_parts(raw);
        if parts.components.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (tzid, zone) in parts.zones {
            if self.zones.insert(tzid) {
                out.extend_from_slice(&zone);
            }
        }
        for component in parts.components {
            out.extend_from_slice(&component);
        }
        Some(out)
    }

    /// The components of a normalized object, written as [`as_text`] writes Graph and JSCalendar
    /// events: from the model, zones by name without a `VTIMEZONE`.
    pub fn object(&mut self, object: &CalendarObject) -> Vec<u8> {
        let mut out = String::new();
        for event in &object.components {
            component(&mut out, event);
        }
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_native_components_into_one_calendar() {
        let zone = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";
        let unused = "BEGIN:VTIMEZONE\r\nTZID:Asia/Tokyo\r\nEND:VTIMEZONE\r\n";
        let event = |uid: &str| {
            format!(
                "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART;TZID=Europe/Berlin:20250310T090000\r\nEND:VEVENT\r\n"
            )
        };
        let first = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:PUBLISH\r\n{zone}{unused}{}END:VCALENDAR\r\n",
            event("a")
        );
        let second = format!(
            "BEGIN:VCALENDAR\nVERSION:2.0\n{zone}{}END:VCALENDAR",
            event("b")
        );
        let mut writer = IcsWriter::new();
        assert_eq!(
            writer.native(first.as_bytes()).unwrap(),
            format!("{zone}{}", event("a")).into_bytes()
        );
        assert_eq!(
            writer.native(second.as_bytes()).unwrap(),
            event("b").into_bytes(),
            "a zone already written is not written again"
        );
        assert_eq!(
            writer.native(b"BEGIN:VTODO\r\nUID:t\r\nEND:VTODO").unwrap(),
            b"BEGIN:VTODO\r\nUID:t\r\nEND:VTODO\r\n"
        );
        assert_eq!(writer.native(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"), None);
    }

    #[test]
    fn folds_long_lines_between_characters() {
        let mut out = String::new();
        push(&mut out, &format!("NOTE:{}", "ä".repeat(60)));
        for line in out.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.len() <= 75, "{line}");
        }
        assert_eq!(
            out.replace("\r\n ", ""),
            format!("NOTE:{}\r\n", "ä".repeat(60))
        );
    }

    #[test]
    fn escapes_text_and_parameters() {
        assert_eq!(escape("a,b;c\\d\ne"), "a\\,b\\;c\\\\d\\ne");
        assert_eq!(parameter("Quill, Ada"), "\"Quill, Ada\"");
        assert_eq!(parameter("Ada \"Q\""), "Ada Q");
        assert_eq!(birthday("1985-04-12"), "BDAY:19850412");
        assert_eq!(birthday("--04-12"), "BDAY:--0412");
        assert_eq!(birthday("spring"), "BDAY;VALUE=text:spring");
        let mut out = String::new();
        push(&mut out, "PHOTO:https://example.com/a\r\nX-INJECTED:1");
        assert_eq!(out, "PHOTO:https://example.com/aX-INJECTED:1\r\n");
    }

    #[test]
    fn writes_times() {
        let time = |value: &str, zone: Option<&str>, utc: Option<i64>| EventTime {
            value: value.into(),
            time_zone: zone.map(Into::into),
            utc,
        };
        assert_eq!(
            time_line("DTSTART", &time("2025-03-10", None, None), "").unwrap(),
            "DTSTART;VALUE=DATE:20250310"
        );
        assert_eq!(
            time_line(
                "DTSTART",
                &time("2025-03-10T09:00:00", Some("W. Europe Standard Time"), None),
                ""
            )
            .unwrap(),
            "DTSTART;TZID=Europe/Berlin:20250310T090000"
        );
        assert_eq!(
            time_line(
                "DTEND",
                &time("2025-03-10T09:00:00Z", Some("UTC"), None),
                ""
            )
            .unwrap(),
            "DTEND:20250310T090000Z"
        );
        assert_eq!(
            time_line(
                "EXDATE",
                &time("2025-03-10T09:00:00+01:00", None, Some(1_741_593_600_000)),
                ""
            )
            .unwrap(),
            "EXDATE:20250310T080000Z"
        );
        assert_eq!(time_line("DTSTART", &time("2025T", None, None), ""), None);
        assert_eq!(
            time_line("DTSTART", &time("2025-03-10T09:00:00", None, None), "").unwrap(),
            "DTSTART:20250310T090000"
        );
    }
}
