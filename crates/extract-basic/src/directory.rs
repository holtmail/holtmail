const KEPT: &[&str] = &[
    "SUMMARY",
    "LOCATION",
    "DESCRIPTION",
    "COMMENT",
    "CATEGORIES",
    "RESOURCES",
    "CONTACT",
    "FN",
    "NICKNAME",
    "ORG",
    "TITLE",
    "ROLE",
    "NOTE",
    "EMAIL",
    "TEL",
    "ADR",
];

const PEOPLE: &[&str] = &["ORGANIZER", "ATTENDEE"];

/// The words a person would search for in iCalendar or vCard data: summaries, places,
/// descriptions, names and addresses, unescaped, one value per line, without property names,
/// dates or ids.
pub(crate) fn text(content: &[u8]) -> String {
    let raw = String::from_utf8_lossy(content);
    let mut out = String::new();
    for line in unfold(&raw) {
        let Some((name, params, value)) = split(&line) else {
            continue;
        };
        let name = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(_, n)| n)
            .to_ascii_uppercase();
        if PEOPLE.contains(&name.as_str()) {
            if let Some(cn) = param(&params, "CN") {
                push(&mut out, &cn);
            }
            let address = value.trim();
            let address = address
                .get(..7)
                .filter(|p| p.eq_ignore_ascii_case("mailto:"))
                .map_or(address, |_| &address[7..]);
            push(&mut out, address);
        } else if KEPT.contains(&name.as_str()) {
            let value = unescape(&value);
            if matches!(name.as_str(), "ADR" | "ORG") {
                push(&mut out, &value.replace(';', " "));
            } else {
                push(&mut out, &value);
            }
        }
    }
    out
}

fn push(out: &mut String, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    out.push_str(value);
    out.push('\n');
}

fn unfold(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => lines.push(line.to_owned()),
        }
    }
    lines
}

type Params = Vec<(String, String)>;

fn split(line: &str) -> Option<(String, Params, String)> {
    let mut quoted = false;
    let mut colon = None;
    for (at, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(at);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in head.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    let mut parts = parts.into_iter();
    let name = parts.next()?;
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.to_ascii_uppercase(), v.to_owned()))
        })
        .collect();
    Some((name, params, value.to_owned()))
}

fn param(params: &[(String, String)], key: &str) -> Option<String> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}
