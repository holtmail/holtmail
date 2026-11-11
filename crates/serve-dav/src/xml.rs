use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

pub(crate) const DAV: &str = "DAV:";
pub(crate) const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub(crate) const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub(crate) const CS: &str = "http://calendarserver.org/ns/";
pub(crate) const APPLE: &str = "http://apple.com/ns/ical/";

const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 100_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct Element {
    pub ns: String,
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    pub fn child(&self, ns: &str, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.is(ns, name))
    }

    pub fn children<'a>(
        &'a self,
        ns: &'a str,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.is(ns, name))
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn trimmed(&self) -> &str {
        self.text.trim()
    }
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

/// Parses a request body into a tree; `None` for malformed XML, more than 64 levels or more
/// than 100 000 elements.
pub(crate) fn parse(bytes: &[u8]) -> Option<Element> {
    let mut reader = NsReader::from_reader(bytes);
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut elements = 0usize;
    loop {
        let (ns, event) = reader.read_resolved_event().ok()?;
        let ns = match ns {
            ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
            _ => String::new(),
        };
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(event, Event::Empty(_));
                elements += 1;
                if elements > MAX_ELEMENTS || stack.len() > MAX_DEPTH {
                    return None;
                }
                let element = Element {
                    ns,
                    name: String::from_utf8_lossy(start.local_name().as_ref()).into_owned(),
                    attrs: start
                        .attributes()
                        .flatten()
                        .map(|a| {
                            (
                                String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned(),
                                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                    .map(|v| v.into_owned())
                                    .unwrap_or_default(),
                            )
                        })
                        .collect(),
                    ..Element::default()
                };
                if empty {
                    stack.last_mut()?.children.push(element);
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let done = stack.pop()?;
                stack.last_mut()?.children.push(done);
            }
            Event::Text(text) => stack.last_mut()?.text.push_str(&text.decode().ok()?),
            Event::CData(data) => stack.last_mut()?.text.push_str(&data.decode().ok()?),
            Event::GeneralRef(reference) => {
                let c = match reference.resolve_char_ref().ok()? {
                    Some(c) => c,
                    None => entity(&reference.decode().ok()?)?,
                };
                stack.last_mut()?.text.push(c);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut root = stack.pop()?;
    if !stack.is_empty() {
        return None;
    }
    root.children.pop()
}

/// Escapes text content: `<`, `>` and `&` only, so ETags keep their quotes for clients that
/// read responses loosely.
pub(crate) fn escape(text: &str) -> String {
    quick_xml::escape::partial_escape(text).into_owned()
}

fn escape_attr(text: &str) -> String {
    quick_xml::escape::escape(text).into_owned()
}

/// The prefix written for a namespace in responses; `None` for one Holtmail never writes.
pub(crate) fn prefix(ns: &str) -> Option<&'static str> {
    Some(match ns {
        DAV => "d",
        CALDAV => "c",
        CARDDAV => "card",
        CS => "cs",
        APPLE => "ical",
        _ => return None,
    })
}

/// The opening of a response document with every namespace Holtmail writes.
pub(crate) fn open(root: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:{root} xmlns:d=\"{DAV}\" xmlns:c=\"{CALDAV}\" xmlns:card=\"{CARDDAV}\" xmlns:cs=\"{CS}\" xmlns:ical=\"{APPLE}\">"
    )
}

/// An element with its namespace written inline, for a property in a namespace without a
/// prefix of ours.
pub(crate) fn element(ns: &str, name: &str, inner: &str) -> String {
    match prefix(ns) {
        Some(p) if inner.is_empty() => format!("<{p}:{name}/>"),
        Some(p) => format!("<{p}:{name}>{inner}</{p}:{name}>"),
        None if inner.is_empty() => format!("<x:{name} xmlns:x=\"{}\"/>", escape_attr(ns)),
        None => format!(
            "<x:{name} xmlns:x=\"{}\">{inner}</x:{name}>",
            escape_attr(ns)
        ),
    }
}

/// A DAV error body naming one precondition.
pub(crate) fn error(ns: &str, condition: &str) -> String {
    format!("{}{}</d:error>", open("error"), element(ns, condition, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_any_prefix_and_keeps_text() {
        let doc = b"<?xml version=\"1.0\"?><A:propfind xmlns:A=\"DAV:\" xmlns:B=\"urn:ietf:params:xml:ns:caldav\"><A:prop><B:calendar-data>BEGIN:VCALENDAR\r\nX &amp; Y&#13;\n</B:calendar-data><A:getetag/></A:prop></A:propfind>";
        let root = parse(doc).unwrap();
        assert!(root.is(DAV, "propfind"));
        let prop = root.child(DAV, "prop").unwrap();
        assert_eq!(
            prop.child(CALDAV, "calendar-data").unwrap().text,
            "BEGIN:VCALENDAR\r\nX & Y\r\n"
        );
        assert!(prop.child(DAV, "getetag").is_some());
    }

    #[test]
    fn refuses_broken_documents() {
        assert!(parse(b"<a:x xmlns:a=\"DAV:\"><a:y></a:x>").is_none());
        let deep = "<a xmlns=\"DAV:\">".repeat(100);
        assert!(parse(deep.as_bytes()).is_none());
    }

    #[test]
    fn writes_unknown_namespaces_inline() {
        assert_eq!(
            element(DAV, "getetag", "\"x\""),
            "<d:getetag>\"x\"</d:getetag>"
        );
        assert_eq!(
            element("urn:example", "thing", ""),
            "<x:thing xmlns:x=\"urn:example\"/>"
        );
    }
}
