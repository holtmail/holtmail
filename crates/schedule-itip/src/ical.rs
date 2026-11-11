//! A small content-line model of iCalendar: components with their lines in order, parameters
//! edited in place, everything else kept as written.

pub(crate) type Params = Vec<(String, String)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Line(String),
    Comp(Comp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Comp {
    pub name: String,
    pub items: Vec<Node>,
}

pub(crate) fn unfold(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.push_str(rest);
            }
        } else if !line.trim().is_empty() {
            out.push(line.to_owned());
        }
    }
    out
}

/// The `VCALENDAR` of `raw`, when its `BEGIN` and `END` lines balance.
pub(crate) fn parse(raw: &[u8]) -> Option<Comp> {
    let mut stack: Vec<Comp> = Vec::new();
    let mut root = None;
    for line in unfold(raw) {
        let (name, _, value) = split(&line);
        match name.as_str() {
            "BEGIN" => stack.push(Comp {
                name: value.trim().to_ascii_uppercase(),
                items: Vec::new(),
            }),
            "END" => {
                let done = stack.pop()?;
                if done.name != value.trim().to_ascii_uppercase() {
                    return None;
                }
                match stack.last_mut() {
                    Some(parent) => parent.items.push(Node::Comp(done)),
                    None if root.is_none() => root = Some(done),
                    None => return None,
                }
            }
            _ => stack.last_mut()?.items.push(Node::Line(line)),
        }
    }
    let root = root.filter(|r| stack.is_empty() && r.name == "VCALENDAR")?;
    let has_event = root.events().next().is_some();
    has_event.then_some(root)
}

fn fold(line: &str, out: &mut String) {
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out.push_str("\r\n");
}

fn write(comp: &Comp, out: &mut String) {
    fold(&format!("BEGIN:{}", comp.name), out);
    for item in &comp.items {
        match item {
            Node::Line(line) => fold(line, out),
            Node::Comp(child) => write(child, out),
        }
    }
    fold(&format!("END:{}", comp.name), out);
}

impl Comp {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        write(self, &mut out);
        out.into_bytes()
    }

    pub fn events(&self) -> impl Iterator<Item = &Comp> {
        self.items.iter().filter_map(|i| match i {
            Node::Comp(c) if c.name == "VEVENT" => Some(c),
            _ => None,
        })
    }

    pub fn events_mut(&mut self) -> impl Iterator<Item = &mut Comp> {
        self.items.iter_mut().filter_map(|i| match i {
            Node::Comp(c) if c.name == "VEVENT" => Some(c),
            _ => None,
        })
    }

    pub fn lines(&self) -> impl Iterator<Item = &String> {
        self.items.iter().filter_map(|i| match i {
            Node::Line(l) => Some(l),
            Node::Comp(_) => None,
        })
    }

    pub fn value(&self, name: &str) -> Option<String> {
        self.lines()
            .map(|l| split(l))
            .find(|(n, _, _)| n == name)
            .map(|(_, _, v)| v)
    }

    /// The `RECURRENCE-ID` value, empty for the series.
    pub fn rid(&self) -> String {
        self.value("RECURRENCE-ID")
            .map(|v| v.trim().to_owned())
            .unwrap_or_default()
    }

    /// Changes the parameters of every `name` line, in this component and its children;
    /// true when a line changed.
    pub fn edit_params(&mut self, name: &str, f: &mut dyn FnMut(&str, &mut Params)) -> bool {
        let mut changed = false;
        for item in &mut self.items {
            match item {
                Node::Line(line) => {
                    let (n, mut params, value) = split(line);
                    if n != name {
                        continue;
                    }
                    let before = params.clone();
                    f(&value, &mut params);
                    if params != before {
                        *line = join(&n, &params, &value);
                        changed = true;
                    }
                }
                Node::Comp(child) => changed |= child.edit_params(name, f),
            }
        }
        changed
    }
}

fn split_outside_quotes(text: &str, at: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == at && !quoted {
            out.push(&text[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out
}

/// A content line's name (uppercase), parameters (names uppercase, values as written) and value.
pub(crate) fn split(line: &str) -> (String, Params, String) {
    let mut quoted = false;
    let mut colon = line.len();
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = i;
                break;
            }
            _ => {}
        }
    }
    let head = &line[..colon];
    let value = line.get(colon + 1..).unwrap_or("").to_owned();
    let mut parts = split_outside_quotes(head, ';').into_iter();
    let name = parts.next().unwrap_or("").trim().to_ascii_uppercase();
    let params = parts
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k.trim().to_ascii_uppercase(), v.to_owned())
        })
        .collect();
    (name, params, value)
}

pub(crate) fn join(name: &str, params: &[(String, String)], value: &str) -> String {
    let mut out = name.to_owned();
    for (k, v) in params {
        out.push(';');
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out.push(':');
    out.push_str(value);
    out
}

pub(crate) fn param<'p>(params: &'p [(String, String)], key: &str) -> Option<&'p str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim_matches('"'))
}

pub(crate) fn set_param(params: &mut Params, key: &str, value: Option<&str>) {
    match (params.iter().position(|(k, _)| k == key), value) {
        (Some(i), Some(v)) => params[i].1 = v.to_owned(),
        (Some(i), None) => {
            params.remove(i);
        }
        (None, Some(v)) => params.push((key.to_owned(), v.to_owned())),
        (None, None) => {}
    }
}

pub(crate) fn address(value: &str) -> Option<String> {
    holtmail_pim::normalize_email(value)
}

/// Each `name` line (`ATTENDEE` or `ORGANIZER`) of a component: its address and parameters.
pub(crate) fn people(comp: &Comp, name: &str) -> Vec<(String, Params)> {
    comp.lines()
        .map(|l| split(l))
        .filter(|(n, _, _)| n == name)
        .filter_map(|(_, p, v)| Some((address(&v)?, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_folds_at_75_octets() {
        let long = format!("DESCRIPTION:{}", "ä".repeat(60));
        let raw = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a@example.com\r\n{long}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let cal = parse(raw.as_bytes()).expect("parse");
        let bytes = cal.to_bytes();
        for line in String::from_utf8(bytes.clone())
            .expect("utf8")
            .split("\r\n")
        {
            assert!(line.len() <= 75, "{line}");
        }
        assert_eq!(parse(&bytes), Some(cal));
    }

    #[test]
    fn quoted_parameters_keep_colons_and_semicolons() {
        let (name, params, value) =
            split("ATTENDEE;CN=\"Doe; Ada: PhD\";PARTSTAT=ACCEPTED:mailto:ada@example.com");
        assert_eq!(name, "ATTENDEE");
        assert_eq!(param(&params, "CN"), Some("Doe; Ada: PhD"));
        assert_eq!(param(&params, "PARTSTAT"), Some("ACCEPTED"));
        assert_eq!(value, "mailto:ada@example.com");
    }

    #[test]
    fn unbalanced_objects_are_refused() {
        assert!(parse(b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nEND:VCALENDAR\r\n").is_none());
        assert!(parse(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_none());
        assert!(parse(b"plain text").is_none());
    }
}
