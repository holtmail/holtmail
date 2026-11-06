use crate::ItemKind;

/// One object cut out of a `.vcf` or `.ics` stream, ready to be archived as one item version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawItem {
    /// What the object holds, from its `BEGIN` line.
    pub kind: ItemKind,
    /// Its `UID`, unfolded and trimmed, when it has one.
    pub uid: Option<String>,
    /// The bytes to store.
    pub raw: Vec<u8>,
}

struct Line<'a> {
    start: usize,
    end: usize,
    text: &'a [u8],
}

fn lines(bytes: &[u8]) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let end = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |at| start + at + 1);
        let mut text = &bytes[start..end];
        if start == 0 {
            text = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
        }
        while let [rest @ .., b'\n' | b'\r' | b' ' | b'\t'] = text {
            text = rest;
        }
        out.push(Line { start, end, text });
        start = end;
    }
    out
}

fn marker<'a>(text: &'a [u8], word: &[u8]) -> Option<&'a [u8]> {
    if text.len() > word.len() && text[..word.len()].eq_ignore_ascii_case(word) {
        Some(trim(&text[word.len()..]))
    } else {
        None
    }
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

fn begin(text: &[u8]) -> Option<&[u8]> {
    marker(text, b"BEGIN:")
}

fn end(text: &[u8]) -> Option<&[u8]> {
    marker(text, b"END:")
}

fn is_continuation(line: &Line<'_>) -> bool {
    matches!(line.text.first(), Some(b' ' | b'\t'))
}

/// The value of property `name` on the logical line starting at `lines[at]`, unfolded.
fn property(lines: &[Line<'_>], at: usize, name: &[u8]) -> Option<String> {
    let text = lines[at].text;
    let rest = text.get(name.len()..)?;
    if !text[..name.len()].eq_ignore_ascii_case(name) || !matches!(rest.first(), Some(b':' | b';'))
    {
        return None;
    }
    let mut value = Vec::new();
    value.extend_from_slice(rest);
    for line in lines[at + 1..].iter().take_while(|l| is_continuation(l)) {
        value.extend_from_slice(&line.text[1..]);
    }
    let value = String::from_utf8_lossy(&value).into_owned();
    let value = value
        .split_once(':')
        .map(|(_, v)| v)
        .unwrap_or_default()
        .trim();
    let value = value.strip_prefix('"').unwrap_or(value);
    let value = value.strip_suffix('"').unwrap_or(value);
    (!value.is_empty()).then(|| value.to_owned())
}

fn kind_of(name: &[u8]) -> ItemKind {
    match name.to_ascii_uppercase().as_slice() {
        b"VCARD" => ItemKind::Contact,
        b"VEVENT" => ItemKind::Event,
        b"VTODO" => ItemKind::Task,
        _ => ItemKind::Other,
    }
}

/// Index of the `END` line closing the component opened at `lines[at]`, or `None` when the
/// stream ends first.
fn closing(lines: &[Line<'_>], at: usize) -> Option<usize> {
    let name = begin(lines[at].text)?;
    let mut depth = 0usize;
    for (i, line) in lines.iter().enumerate().skip(at) {
        if begin(line.text).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            depth += 1;
        } else if end(line.text).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Cuts a vCard or iCalendar stream into items.
///
/// - Every top-level `VCARD` becomes one item whose bytes are exactly its lines in the file.
/// - Inside a `VCALENDAR`, components sharing a `UID` and type (a recurring event and its changed
///   instances) become one item; a component without `UID` is an item of its own. Its bytes are
///   the calendar's own lines, the `VTIMEZONE`s the item names in a `TZID`, and its components,
///   each copied byte for byte, in file order, followed by the calendar's `END` line. Once these
///   copies of the calendar's lines and zones add up to more than 16 times the stream (at least
///   64 MiB), later items carry only the `BEGIN` line, their components and the `END` line.
/// - A component left open at the end of the stream runs to the end, so nothing is lost; it is
///   likely unreadable later.
/// - Lines outside any component are ignored. Items come in file order.
pub fn split(bytes: &[u8]) -> Vec<RawItem> {
    let lines = lines(bytes);
    let mut items = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let Some(name) = begin(lines[at].text) else {
            at += 1;
            continue;
        };
        let close = closing(&lines, at);
        let last = close.unwrap_or(lines.len() - 1);
        if name.eq_ignore_ascii_case(b"VCALENDAR") {
            calendar(bytes, &lines, at, last, close.is_some(), &mut items);
        } else {
            let uid = (at + 1..=last).find_map(|i| property(&lines, i, b"UID"));
            items.push(RawItem {
                kind: kind_of(name),
                uid,
                raw: bytes[lines[at].start..lines[last].end].to_vec(),
            });
        }
        at = last + 1;
    }
    items
}

struct Component {
    kind: ItemKind,
    name: Vec<u8>,
    uid: Option<String>,
    start: usize,
    end: usize,
}

fn calendar(
    bytes: &[u8],
    lines: &[Line<'_>],
    open: usize,
    last: usize,
    closed: bool,
    items: &mut Vec<RawItem>,
) {
    let mut head = bytes[lines[open].start..lines[open].end].to_vec();
    let mut zones: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    let mut components = Vec::new();
    let inner_end = if closed { last } else { last + 1 };
    let mut at = open + 1;
    while at < inner_end {
        let line = &lines[at];
        let Some(name) = begin(line.text) else {
            if end(line.text).is_none() {
                head.extend_from_slice(&bytes[line.start..line.end]);
            }
            at += 1;
            continue;
        };
        let stop = closing(lines, at).map_or(inner_end - 1, |c| c.min(inner_end - 1));
        let mut depth = 0usize;
        let mut uid = None;
        let mut tzid = None;
        for i in at..=stop {
            if begin(lines[i].text).is_some() {
                depth += 1;
            } else if end(lines[i].text).is_some() {
                depth = depth.saturating_sub(1);
            } else if depth == 1 && !is_continuation(&lines[i]) {
                uid = uid.or_else(|| property(lines, i, b"UID"));
                tzid = tzid.or_else(|| property(lines, i, b"TZID"));
            }
        }
        let range = lines[at].start..lines[stop].end;
        if name.eq_ignore_ascii_case(b"VTIMEZONE") {
            if let Some(tzid) = tzid {
                zones.push((tzid, range));
            }
        } else {
            components.push(Component {
                kind: kind_of(name),
                name: name.to_ascii_uppercase(),
                uid,
                start: range.start,
                end: range.end,
            });
        }
        at = stop + 1;
    }
    let tail = if closed {
        &bytes[lines[last].start..lines[last].end]
    } else {
        &[][..]
    };

    let mut groups: Vec<Vec<&Component>> = Vec::new();
    let mut by_uid: std::collections::HashMap<(&str, &[u8]), usize> =
        std::collections::HashMap::new();
    for component in &components {
        let Some(uid) = component.uid.as_deref() else {
            groups.push(vec![component]);
            continue;
        };
        match by_uid.entry((uid, component.name.as_slice())) {
            std::collections::hash_map::Entry::Occupied(at) => groups[*at.get()].push(component),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(groups.len());
                groups.push(vec![component]);
            }
        }
    }
    let mut budget = bytes.len().saturating_mul(16).max(64 << 20);
    for group in groups {
        let body: Vec<&[u8]> = group.iter().map(|c| &bytes[c.start..c.end]).collect();
        let upper: Vec<Vec<u8>> = body.iter().map(|b| b.to_ascii_uppercase()).collect();
        let mut raw = Vec::new();
        let copies = head.len()
            + zones
                .iter()
                .filter(|(tzid, _)| upper.iter().any(|b| names_zone(b, tzid)))
                .map(|(_, range)| range.len())
                .sum::<usize>();
        if copies <= budget {
            budget -= copies;
            raw.extend_from_slice(&head);
            for (tzid, range) in &zones {
                if upper.iter().any(|b| names_zone(b, tzid)) {
                    raw.extend_from_slice(&bytes[range.clone()]);
                }
            }
        } else {
            raw.extend_from_slice(&bytes[lines[open].start..lines[open].end]);
        }
        for part in body {
            raw.extend_from_slice(part);
        }
        raw.extend_from_slice(tail);
        items.push(RawItem {
            kind: group[0].kind,
            uid: group[0].uid.clone(),
            raw,
        });
    }
}

/// The parts of one stored iCalendar object to copy into another calendar: its `VTIMEZONE`s that
/// its other components name, and those components, each byte for byte and ending in a line end.
pub(crate) struct CalendarParts {
    pub(crate) zones: Vec<(String, Vec<u8>)>,
    pub(crate) components: Vec<Vec<u8>>,
}

fn chunk(bytes: &[u8], start: usize, end: usize) -> Vec<u8> {
    let mut out = bytes[start..end].to_vec();
    if start == 0 && out.starts_with(b"\xEF\xBB\xBF") {
        out.drain(..3);
    }
    if !out.ends_with(b"\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

pub(crate) fn calendar_parts(bytes: &[u8]) -> CalendarParts {
    let lines = lines(bytes);
    let mut zones: Vec<(String, Vec<u8>)> = Vec::new();
    let mut components: Vec<Vec<u8>> = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let Some(name) = begin(lines[at].text) else {
            at += 1;
            continue;
        };
        let close = closing(&lines, at);
        let last = close.unwrap_or(lines.len() - 1);
        if !name.eq_ignore_ascii_case(b"VCALENDAR") {
            components.push(chunk(bytes, lines[at].start, lines[last].end));
            at = last + 1;
            continue;
        }
        let inner_end = if close.is_some() { last } else { last + 1 };
        let mut inner = at + 1;
        while inner < inner_end {
            let Some(name) = begin(lines[inner].text) else {
                inner += 1;
                continue;
            };
            let stop = closing(&lines, inner).map_or(inner_end - 1, |c| c.min(inner_end - 1));
            let part = chunk(bytes, lines[inner].start, lines[stop].end);
            if name.eq_ignore_ascii_case(b"VTIMEZONE") {
                let mut depth = 0usize;
                let tzid = (inner..=stop).find_map(|i| {
                    if begin(lines[i].text).is_some() {
                        depth += 1;
                    } else if end(lines[i].text).is_some() {
                        depth = depth.saturating_sub(1);
                    } else if depth == 1 && !is_continuation(&lines[i]) {
                        return property(&lines, i, b"TZID");
                    }
                    None
                });
                if let Some(tzid) = tzid {
                    zones.push((tzid, part));
                }
            } else {
                components.push(part);
            }
            inner = stop + 1;
        }
        at = last + 1;
    }
    let upper: Vec<Vec<u8>> = components.iter().map(|c| c.to_ascii_uppercase()).collect();
    zones.retain(|(tzid, _)| upper.iter().any(|c| names_zone(c, tzid)));
    CalendarParts { zones, components }
}

fn names_zone(upper: &[u8], tzid: &str) -> bool {
    let plain = format!("TZID={tzid}");
    let quoted = format!("TZID=\"{tzid}\"");
    [plain, quoted].iter().any(|needle| {
        let needle = needle.to_ascii_uppercase();
        upper
            .windows(needle.len())
            .enumerate()
            .any(|(i, w)| w == needle.as_bytes() && !continues_name(upper.get(i + needle.len())))
    })
}

fn continues_name(next: Option<&u8>) -> bool {
    !matches!(next, None | Some(b':' | b';' | b'\r' | b'\n' | b','))
}

/// The iTIP `METHOD` of the first `VCALENDAR` in the stream, uppercase, as iMIP mail carries it
/// (RFC 5546, RFC 6047); `None` when the calendar has none. Only the calendar's own lines count,
/// never a property of the same name inside a component.
pub fn calendar_method(bytes: &[u8]) -> Option<String> {
    let lines = lines(bytes);
    let mut depth = 0usize;
    for i in 0..lines.len() {
        if begin(lines[i].text).is_some() {
            depth += 1;
        } else if end(lines[i].text).is_some() {
            if depth == 1 {
                return None;
            }
            depth = depth.saturating_sub(1);
        } else if depth == 1
            && !is_continuation(&lines[i])
            && let Some(method) = property(&lines, i, b"METHOD")
        {
            return Some(method.to_ascii_uppercase());
        }
    }
    None
}

/// The calendar's display name from `X-WR-CALNAME`, as Google and Apple write it, when the stream
/// has one.
pub fn calendar_name(bytes: &[u8]) -> Option<String> {
    let lines = lines(bytes);
    (0..lines.len()).find_map(|i| property(&lines, i, b"X-WR-CALNAME"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_cards_byte_for_byte() {
        let file = b"\xEF\xBB\xBFBEGIN:VCARD\r\nVERSION:3.0\r\nUID:one\r\nFN:Ada\r\nEND:VCARD\r\n\
                     junk\r\nbegin:vcard\nVERSION:4.0\nUID:\n two\nFN:Bob\nend:vcard";
        let items = split(file);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, ItemKind::Contact);
        assert_eq!(items[0].uid.as_deref(), Some("one"));
        assert_eq!(
            items[0].raw,
            &file
                [..b"\xEF\xBB\xBFBEGIN:VCARD\r\nVERSION:3.0\r\nUID:one\r\nFN:Ada\r\nEND:VCARD\r\n"
                    .len()]
        );
        assert_eq!(items[1].uid.as_deref(), Some("two"), "folded UID unfolded");
        assert!(items[1].raw.starts_with(b"begin:vcard\n"));
        assert!(items[1].raw.ends_with(b"end:vcard"));
    }

    #[test]
    fn groups_overrides_and_carries_their_zones() {
        let file = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nEND:VTIMEZONE\r\n\
BEGIN:VTIMEZONE\r\nTZID:Asia/Tokyo\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nDTSTART;TZID=Europe/Berlin:20250310T090000\r\nRRULE:FREQ=DAILY\r\nBEGIN:VALARM\r\nUID:alarm\r\nEND:VALARM\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:lunch\r\nDTSTART:20250310T120000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nRECURRENCE-ID;TZID=Europe/Berlin:20250311T090000\r\nDTSTART;TZID=Europe/Berlin:20250311T100000\r\nEND:VEVENT\r\n\
BEGIN:VTODO\r\nUID:standup\r\nEND:VTODO\r\n\
END:VCALENDAR\r\n";
        let items = split(file.as_bytes());
        let uids: Vec<_> = items
            .iter()
            .map(|i| (i.kind, i.uid.clone().unwrap()))
            .collect();
        assert_eq!(
            uids,
            [
                (ItemKind::Event, "standup".into()),
                (ItemKind::Event, "lunch".into()),
                (ItemKind::Task, "standup".into())
            ]
        );
        let standup = String::from_utf8(items[0].raw.clone()).unwrap();
        assert!(standup.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n"));
        assert!(!standup.contains("Asia/Tokyo"));
        assert_eq!(standup.matches("BEGIN:VEVENT").count(), 2);
        assert!(standup.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
        let lunch = String::from_utf8(items[1].raw.clone()).unwrap();
        assert!(!lunch.contains("VTIMEZONE"));
        assert_eq!(calendar_name(file.as_bytes()).as_deref(), Some("Team"));
    }

    #[test]
    fn keeps_an_unterminated_calendar() {
        let items = split(b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nSUMMARY:cut");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].raw,
            b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nSUMMARY:cut"
        );
    }

    #[test]
    fn reads_the_calendar_method_only() {
        let file = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nMETHOD:CANCEL\r\nEND:VEVENT\r\nmethod:\r\n request\r\nEND:VCALENDAR\r\n";
        assert_eq!(calendar_method(file).as_deref(), Some("REQUEST"));
        assert_eq!(
            calendar_method(b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nEND:VEVENT\nEND:VCALENDAR\n"),
            None
        );
    }

    #[test]
    fn a_single_object_calendar_stays_whole() {
        let file = b"BEGIN:VCALENDAR\nPRODID:-//Example//EN\nBEGIN:VEVENT\nUID:a\nEND:VEVENT\nEND:VCALENDAR\n";
        assert_eq!(split(file)[0].raw, file);
    }

    #[test]
    fn a_large_header_is_copied_only_until_the_budget_runs_out() {
        let mut file = String::from("BEGIN:VCALENDAR\n");
        for i in 0..20_000 {
            file.push_str(&format!("X-PADDING-{i}:{}\n", "x".repeat(30)));
        }
        for i in 0..20_000 {
            file.push_str(&format!("BEGIN:VEVENT\nUID:{i}\nEND:VEVENT\n"));
        }
        file.push_str("END:VCALENDAR\n");
        let items = split(file.as_bytes());
        assert_eq!(items.len(), 20_000);
        let total: usize = items.iter().map(|i| i.raw.len()).sum();
        assert!(total <= (64 << 20) + file.len() * 2);
        assert!(
            items[19_999]
                .raw
                .starts_with(b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:19999")
        );
    }
}
