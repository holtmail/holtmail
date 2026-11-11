use holtmail_pim::{ItemKind, Normalized};
use holtmail_recur::{Window, expand};

use crate::xml::{CALDAV, CARDDAV, Element};

/// One unfolded content line: its name in upper case, its parameter text and its value.
struct Line {
    name: String,
    value: String,
}

fn unfold(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match line.chars().next() {
            Some(' ' | '\t') if !out.is_empty() => {
                if let Some(last) = out.last_mut() {
                    last.push_str(&line[1..]);
                }
            }
            _ => out.push(line.to_owned()),
        }
    }
    out
}

fn split(line: &str) -> Option<Line> {
    let mut quoted = false;
    let mut name_end = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted && name_end.is_none() => name_end = Some(i),
            ':' if !quoted => {
                let name = &line[..name_end.unwrap_or(i)];
                let name = name.rsplit('.').next().unwrap_or(name);
                return Some(Line {
                    name: name.to_ascii_uppercase(),
                    value: line[i + 1..].to_owned(),
                });
            }
            _ => {}
        }
    }
    None
}

/// The lines of every `component` block (directly, not of nested blocks); the whole object
/// when `component` is `None`.
fn lines_of(raw: &str, component: Option<&str>) -> Vec<Line> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    for line in unfold(raw) {
        let Some(l) = split(&line) else { continue };
        match l.name.as_str() {
            "BEGIN" => stack.push(l.value.trim().to_ascii_uppercase()),
            "END" => {
                stack.pop();
            }
            _ => {
                let inside = match component {
                    None => true,
                    Some(c) => stack.last().is_some_and(|s| s == c),
                };
                if inside {
                    out.push(l);
                }
            }
        }
    }
    out
}

fn has_component(raw: &str, component: &str) -> bool {
    unfold(raw).iter().any(|l| {
        split(l)
            .is_some_and(|l| l.name == "BEGIN" && l.value.trim().eq_ignore_ascii_case(component))
    })
}

fn casefold(text: &str, collation: &str) -> String {
    match collation {
        "i;octet" => text.to_owned(),
        _ => text.to_lowercase(),
    }
}

/// Whether a `text-match` holds for a value.
fn text_match(element: &Element, value: &str, default_match: &str) -> bool {
    let collation = element.attr("collation").unwrap_or("i;unicode-casemap");
    let needle = casefold(&element.text, collation);
    let value = casefold(value, collation);
    let hit = match element.attr("match-type").unwrap_or(default_match) {
        "equals" => value == needle,
        "starts-with" => value.starts_with(&needle),
        "ends-with" => value.ends_with(&needle),
        _ => value.contains(&needle),
    };
    hit != (element.attr("negate-condition") == Some("yes"))
}

fn prop_filter(filter: &Element, lines: &[Line], ns: &str) -> bool {
    let name = filter.attr("name").unwrap_or_default().to_ascii_uppercase();
    let values: Vec<&Line> = lines.iter().filter(|l| l.name == name).collect();
    if filter.child(ns, "is-not-defined").is_some() {
        return values.is_empty();
    }
    if values.is_empty() {
        return false;
    }
    let matches: Vec<&Element> = filter.children(ns, "text-match").collect();
    if matches.is_empty() {
        return true;
    }
    let one = |m: &Element| values.iter().any(|l| text_match(m, &l.value, "contains"));
    match filter.attr("test") {
        Some("allof") => matches.iter().all(|m| one(m)),
        _ => matches.iter().any(|m| one(m)),
    }
}

/// Reads an iCalendar UTC time (`20260105T090000Z`) as milliseconds since the epoch.
pub(crate) fn parse_utc(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = t.strip_suffix('Z').or_else(|| t.strip_suffix('z'))?;
    let (date, time) = t.split_once(['T', 't'])?;
    if date.len() != 8 || time.len() != 6 {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, m, d) = (num(&date[..4])?, num(&date[4..6])?, num(&date[6..])?);
    let (hh, mm, ss) = (num(&time[..2])?, num(&time[2..4])?, num(&time[4..])?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    Some(((days_from_civil(y, m, d) * 24 + hh) * 60 + mm) * 60_000 + ss * 1000)
}

/// Writes milliseconds since the epoch as an iCalendar UTC time.
pub(crate) fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// The window of a `time-range` element; an open side is unbounded.
pub(crate) fn window(range: &Element) -> Option<Window> {
    let from = match range.attr("start") {
        Some(s) => parse_utc(s)?,
        None => Window::ALL.from,
    };
    let to = match range.attr("end") {
        Some(e) => parse_utc(e)?,
        None => Window::ALL.to,
    };
    (from < to).then_some(Window { from, to })
}

/// The busy periods of an object in the window: its instances that are not cancelled, unless
/// the object is marked `TRANSP:TRANSPARENT`.
pub(crate) fn busy(kind: ItemKind, raw: &[u8], window: Window) -> Vec<(i64, i64)> {
    if kind != ItemKind::Event {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(raw);
    if lines_of(&text, Some("VEVENT"))
        .iter()
        .any(|l| l.name == "TRANSP" && l.value.trim().eq_ignore_ascii_case("TRANSPARENT"))
    {
        return Vec::new();
    }
    let Normalized::Calendar(object) = holtmail_pim::normalize(kind, raw) else {
        return Vec::new();
    };
    expand(&object, window)
        .occurrences
        .into_iter()
        .filter(|o| {
            !o.cancelled
                && object.components.get(o.component).is_none_or(|c| {
                    !c.status
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"))
                })
        })
        .map(|o| (o.start.max(window.from), o.end.min(window.to)))
        .filter(|(s, e)| s < e)
        .collect()
}

fn overlaps(kind: ItemKind, raw: &[u8], window: Window) -> bool {
    let Normalized::Calendar(object) = holtmail_pim::normalize(kind, raw) else {
        return true;
    };
    let undated = object
        .components
        .iter()
        .all(|c| c.start.is_none() && c.end.is_none());
    if undated {
        return kind == ItemKind::Task;
    }
    !expand(&object, window).occurrences.is_empty()
}

fn comp_filter(filter: &Element, kind: ItemKind, raw: &[u8], text: &str, top: bool) -> bool {
    let name = filter.attr("name").unwrap_or_default().to_ascii_uppercase();
    let present = if top {
        name == "VCALENDAR"
    } else {
        has_component(text, &name)
    };
    if filter.child(CALDAV, "is-not-defined").is_some() {
        return !present;
    }
    if !present {
        return false;
    }
    if !top
        && let Some(range) = filter.child(CALDAV, "time-range")
        && let Some(w) = window(range)
        && matches!(name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL")
        && !overlaps(kind, raw, w)
    {
        return false;
    }
    let lines = lines_of(text, (!top).then_some(name.as_str()));
    filter
        .children(CALDAV, "prop-filter")
        .all(|p| prop_filter(p, &lines, CALDAV))
        && filter
            .children(CALDAV, "comp-filter")
            .all(|c| comp_filter(c, kind, raw, text, false))
}

/// Whether a calendar object passes a `calendar-query` `<filter>`; no filter passes everything.
pub(crate) fn calendar(filter: Option<&Element>, kind: ItemKind, raw: &[u8]) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let text = String::from_utf8_lossy(raw);
    filter
        .children(CALDAV, "comp-filter")
        .all(|c| comp_filter(c, kind, raw, &text, true))
}

/// Whether a vCard passes an `addressbook-query` `<filter>`: `anyof` by default (RFC 6352).
pub(crate) fn contact(filter: Option<&Element>, raw: &[u8]) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let text = String::from_utf8_lossy(raw);
    let lines = lines_of(&text, Some("VCARD"));
    let filters: Vec<&Element> = filter.children(CARDDAV, "prop-filter").collect();
    if filters.is_empty() {
        return true;
    }
    let one = |f: &Element| {
        let name = f.attr("name").unwrap_or_default().to_ascii_uppercase();
        let values: Vec<&Line> = lines.iter().filter(|l| l.name == name).collect();
        if f.child(CARDDAV, "is-not-defined").is_some() {
            return values.is_empty();
        }
        if values.is_empty() {
            return false;
        }
        let matches: Vec<&Element> = f.children(CARDDAV, "text-match").collect();
        if matches.is_empty() {
            return true;
        }
        let hit = |m: &Element| values.iter().any(|l| text_match(m, &l.value, "contains"));
        match f.attr("test") {
            Some("allof") => matches.iter().all(|m| hit(m)),
            _ => matches.iter().any(|m| hit(m)),
        }
    };
    match filter.attr("test") {
        Some("allof") => filters.iter().all(|f| one(f)),
        _ => filters.iter().any(|f| one(f)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse;

    const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:standup@example.com\r\nDTSTAMP:20260101T090000Z\r\nSUMMARY:Daily standup\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T093000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Bea Example\r\nitem1.EMAIL;TYPE=INTERNET:bea@example.org\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";

    fn query(filter: &str) -> Element {
        let doc =
            format!("<c:filter xmlns:c=\"urn:ietf:params:xml:ns:caldav\">{filter}</c:filter>");
        parse(doc.as_bytes()).unwrap()
    }

    #[test]
    fn time_ranges_follow_recurrence() {
        let raw = EVENT.as_bytes();
        let within = query(
            "<c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:time-range start=\"20260108T000000Z\" end=\"20260109T000000Z\"/></c:comp-filter></c:comp-filter>",
        );
        assert!(calendar(Some(&within), ItemKind::Event, raw));
        let after = query(
            "<c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:time-range start=\"20260110T000000Z\" end=\"20260111T000000Z\"/></c:comp-filter></c:comp-filter>",
        );
        assert!(!calendar(Some(&after), ItemKind::Event, raw));
        let todo = query(
            "<c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VTODO\"/></c:comp-filter>",
        );
        assert!(!calendar(Some(&todo), ItemKind::Event, raw));
        assert!(calendar(None, ItemKind::Event, raw));
    }

    #[test]
    fn prop_filters_match_text() {
        let raw = EVENT.as_bytes();
        let uid = query(
            "<c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:prop-filter name=\"UID\"><c:text-match>STANDUP@</c:text-match></c:prop-filter></c:comp-filter></c:comp-filter>",
        );
        assert!(calendar(Some(&uid), ItemKind::Event, raw));
        let negated = query(
            "<c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:prop-filter name=\"SUMMARY\"><c:text-match negate-condition=\"yes\">standup</c:text-match></c:prop-filter></c:comp-filter></c:comp-filter>",
        );
        assert!(!calendar(Some(&negated), ItemKind::Event, raw));
        let alarm_action = query(
            "<c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:prop-filter name=\"ACTION\"><c:is-not-defined/></c:prop-filter><c:comp-filter name=\"VALARM\"/></c:comp-filter></c:comp-filter>",
        );
        assert!(calendar(Some(&alarm_action), ItemKind::Event, raw));
    }

    #[test]
    fn contact_filters() {
        let raw = CARD.as_bytes();
        let f = |body: &str| {
            parse(
                format!("<card:filter xmlns:card=\"urn:ietf:params:xml:ns:carddav\" {body}</card:filter>")
                    .as_bytes(),
            )
            .unwrap()
        };
        assert!(contact(
            Some(&f(
                "><card:prop-filter name=\"EMAIL\"><card:text-match match-type=\"ends-with\">@EXAMPLE.ORG</card:text-match></card:prop-filter>"
            )),
            raw
        ));
        assert!(!contact(
            Some(&f(
                "test=\"allof\"><card:prop-filter name=\"FN\"><card:text-match match-type=\"equals\">bea</card:text-match></card:prop-filter><card:prop-filter name=\"TEL\"/>"
            )),
            raw
        ));
        assert!(contact(
            Some(&f(
                "><card:prop-filter name=\"FN\"><card:text-match match-type=\"starts-with\">bea</card:text-match></card:prop-filter><card:prop-filter name=\"NOTE\"/>"
            )),
            raw
        ));
        assert!(contact(
            Some(&f(
                "><card:prop-filter name=\"NOTE\"><card:is-not-defined/></card:prop-filter>"
            )),
            raw
        ));
    }

    #[test]
    fn times_round_trip() {
        let ms = parse_utc("20260105T090000Z").unwrap();
        assert_eq!(ms, 1_767_603_600_000);
        assert_eq!(format_utc(ms), "20260105T090000Z");
        assert_eq!(format_utc(0), "19700101T000000Z");
        assert_eq!(parse_utc("20260105T090000"), None);
    }

    #[test]
    fn busy_periods_skip_transparent_events() {
        let w = Window {
            from: parse_utc("20260105T000000Z").unwrap(),
            to: parse_utc("20260107T000000Z").unwrap(),
        };
        let busy_times = busy(ItemKind::Event, EVENT.as_bytes(), w);
        assert_eq!(busy_times.len(), 2);
        let free = EVENT.replace("SUMMARY", "TRANSP:TRANSPARENT\r\nSUMMARY");
        assert!(busy(ItemKind::Event, free.as_bytes(), w).is_empty());
    }
}
