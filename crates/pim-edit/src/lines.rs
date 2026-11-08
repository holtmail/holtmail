#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub raw: String,
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Component {
    pub name: String,
    pub lines: Vec<Line>,
    pub children: Vec<Component>,
}

pub(crate) fn split_outside_quotes(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == separator && !quoted {
            parts.push(&text[start..at]);
            start = at + c.len_utf8();
        }
    }
    parts.push(&text[start..]);
    parts
}

impl Line {
    pub fn parse(raw: &str) -> Option<Line> {
        let mut quoted = false;
        let mut colon = None;
        for (at, c) in raw.char_indices() {
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
        let head = &raw[..colon];
        let value = raw[colon + 1..].to_owned();
        let mut parts = split_outside_quotes(head, ';').into_iter();
        let full_name = parts.next()?.trim();
        if full_name.is_empty() {
            return None;
        }
        let name = full_name
            .rsplit_once('.')
            .map_or(full_name, |(_, n)| n)
            .to_ascii_uppercase();
        let params = parts
            .filter(|p| !p.trim().is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_uppercase(), v.trim().replace('"', "")),
                None => ("TYPE".to_owned(), p.trim().to_owned()),
            })
            .collect();
        Some(Line {
            raw: raw.to_owned(),
            name,
            params,
            value,
        })
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn group(&self) -> Option<&str> {
        let head = self.raw.split([';', ':']).next()?;
        head.rsplit_once('.').map(|(g, _)| g)
    }

    pub fn canonical(&self, drop: &[&str]) -> String {
        let mut params: Vec<(String, String)> = self
            .params
            .iter()
            .filter(|(k, _)| !drop.contains(&k.as_str()))
            .map(|(k, v)| {
                if k == "TYPE" {
                    let mut kinds: Vec<String> = v
                        .split(',')
                        .map(|t| t.trim().to_ascii_lowercase())
                        .filter(|t| !t.is_empty())
                        .collect();
                    kinds.sort();
                    (k.clone(), kinds.join(","))
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();
        let mut types: Vec<String> = Vec::new();
        params.retain(|(k, v)| {
            if k == "TYPE" {
                types.extend(v.split(',').filter(|t| !t.is_empty()).map(str::to_owned));
                false
            } else {
                true
            }
        });
        if !types.is_empty() {
            types.sort();
            types.dedup();
            params.push(("TYPE".into(), types.join(",")));
        }
        params.sort();
        let mut out = self.name.clone();
        for (k, v) in params {
            out.push(';');
            out.push_str(&k);
            out.push('=');
            out.push_str(&v);
        }
        out.push(':');
        out.push_str(self.value.trim_end());
        out
    }
}

pub(crate) fn unfold(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut lines: Vec<String> = Vec::new();
    let mut soft_break = false;
    for physical in text.split('\n') {
        let physical = physical.strip_suffix('\r').unwrap_or(physical);
        if soft_break && let Some(last) = lines.last_mut() {
            last.pop();
            last.push_str(physical);
        } else if let (Some(rest), Some(last)) = (
            physical
                .strip_prefix(' ')
                .or_else(|| physical.strip_prefix('\t')),
            lines.last_mut(),
        ) {
            last.push_str(rest);
        } else {
            lines.push(physical.to_owned());
        }
        soft_break = lines.last().is_some_and(|l| {
            l.ends_with('=')
                && l.split(':')
                    .next()
                    .is_some_and(|h| h.to_ascii_uppercase().contains("QUOTED-PRINTABLE"))
        });
    }
    lines
}

pub(crate) fn parse(raw: &[u8]) -> Option<Vec<Component>> {
    let mut stack: Vec<Component> = Vec::new();
    let mut top = Vec::new();
    for text in unfold(raw) {
        if text.trim().is_empty() {
            continue;
        }
        let line = Line::parse(&text)?;
        match line.name.as_str() {
            "BEGIN" => stack.push(Component {
                name: line.value.trim().to_ascii_uppercase(),
                lines: Vec::new(),
                children: Vec::new(),
            }),
            "END" => {
                let done = stack.pop()?;
                if !done.name.eq_ignore_ascii_case(line.value.trim()) {
                    return None;
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => top.push(done),
                }
            }
            _ => stack.last_mut()?.lines.push(line),
        }
    }
    stack.is_empty().then_some(top)
}

pub(crate) fn fold(out: &mut String, line: &str) {
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

impl Component {
    pub fn write(&self, out: &mut String) {
        fold(out, &format!("BEGIN:{}", self.name));
        for line in &self.lines {
            fold(out, &line.raw);
        }
        for child in &self.children {
            child.write(out);
        }
        fold(out, &format!("END:{}", self.name));
    }

    pub fn line(&self, name: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.name == name)
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

pub(crate) fn write_all(components: &[Component]) -> Vec<u8> {
    let mut out = String::new();
    for component in components {
        component.write(&mut out);
    }
    out.into_bytes()
}

pub(crate) fn escape(text: &str) -> String {
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

pub(crate) fn parameter(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folded_lines_groups_and_quoted_parameters() {
        let raw = b"BEGIN:VCARD\r\nVERSION:3.0\r\nitem1.EMAIL;type=INTERNET;type=pref:ada@exa\r\n mple.com\r\nNOTE;X-A=\"a:b;c\":one\r\nEND:VCARD\r\n";
        let top = parse(raw).unwrap();
        assert_eq!(top.len(), 1);
        let email = top[0].line("EMAIL").unwrap();
        assert_eq!(email.value, "ada@example.com");
        assert_eq!(email.group(), Some("item1"));
        assert_eq!(
            email.canonical(&[]),
            "EMAIL;TYPE=internet,pref:ada@example.com"
        );
        let note = top[0].line("NOTE").unwrap();
        assert_eq!(note.param("X-A"), Some("a:b;c"));
        assert_eq!(note.value, "one");
    }

    #[test]
    fn joins_quoted_printable_soft_breaks() {
        let raw =
            b"BEGIN:VCARD\r\nNOTE;ENCODING=QUOTED-PRINTABLE:first=\r\nsecond\r\nEND:VCARD\r\n";
        let top = parse(raw).unwrap();
        assert_eq!(top[0].line("NOTE").unwrap().value, "firstsecond");
    }

    #[test]
    fn refuses_unbalanced_components() {
        assert!(parse(b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nEND:VCALENDAR\r\n").is_none());
        assert!(parse(b"BEGIN:VCARD\r\nFN:Ada\r\n").is_none());
        assert!(parse(b"BEGIN:VCARD\r\nno colon\r\nEND:VCARD\r\n").is_none());
    }
}
