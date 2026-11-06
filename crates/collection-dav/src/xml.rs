use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

pub(crate) const DAV: &str = "DAV:";
pub(crate) const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub(crate) const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub(crate) const PUSH: &str = "https://bitfire.at/webdav-push";

#[derive(Debug, Default)]
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

    /// The first child named `name` in any namespace, for properties servers put in varying
    /// namespaces (`getctag`, `calendar-color`).
    pub fn child_named(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn trimmed(&self) -> Option<&str> {
        Some(self.text.trim()).filter(|t| !t.is_empty())
    }
}

fn resolve_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 1_000_000;

/// Parses a document into a tree. Text keeps its line ends exactly as sent: vCards and
/// iCalendar objects inside `address-data` and `calendar-data` must stay byte for byte. `None`
/// for a document nested deeper than 64 elements or holding more than a million.
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
                let name = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
                let attrs = start
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
                    .collect();
                let element = Element {
                    ns,
                    name,
                    attrs,
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
                    None => resolve_entity(&reference.decode().ok()?)?,
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

pub(crate) fn escape(text: &str) -> String {
    quick_xml::escape::escape(text).into_owned()
}

/// A DAV status line's code: `HTTP/1.1 404 Not Found` gives 404.
pub(crate) fn status_code(element: Option<&Element>) -> Option<u16> {
    element?.trimmed()?.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_any_prefix_and_keeps_line_ends() {
        let doc = b"<?xml version=\"1.0\"?>\n<x:multistatus xmlns:x=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:carddav\"><x:response><x:href>/a%20b.vcf</x:href><x:propstat><x:prop><C:address-data>BEGIN:VCARD\r\nFN:A &amp; B&#13;\nEND:VCARD\r\n</C:address-data><x:getetag/></x:prop></x:propstat></x:response></x:multistatus>";
        let root = parse(doc).unwrap();
        assert!(root.is(DAV, "multistatus"));
        let response = root.child(DAV, "response").unwrap();
        assert_eq!(
            response.child(DAV, "href").unwrap().trimmed(),
            Some("/a%20b.vcf")
        );
        let prop = response
            .child(DAV, "propstat")
            .unwrap()
            .child(DAV, "prop")
            .unwrap();
        assert_eq!(
            prop.child(CARDDAV, "address-data").unwrap().text,
            "BEGIN:VCARD\r\nFN:A & B\r\nEND:VCARD\r\n"
        );
        assert!(prop.child(DAV, "getetag").is_some());
    }

    #[test]
    fn default_namespaces_and_cdata() {
        let doc = b"<multistatus xmlns=\"DAV:\"><response><href>/c/</href><propstat><prop><calendar-data xmlns=\"urn:ietf:params:xml:ns:caldav\"><![CDATA[BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n]]></calendar-data></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>";
        let root = parse(doc).unwrap();
        let propstat = root
            .child(DAV, "response")
            .unwrap()
            .child(DAV, "propstat")
            .unwrap();
        assert_eq!(status_code(propstat.child(DAV, "status")), Some(200));
        assert_eq!(
            propstat
                .child(DAV, "prop")
                .unwrap()
                .child(CALDAV, "calendar-data")
                .unwrap()
                .text,
            "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"
        );
    }

    #[test]
    fn broken_documents_are_refused() {
        assert!(parse(b"<a xmlns=\"DAV:\"><b></a>").is_none());
        assert!(parse(b"not xml at all").is_none());
        let deep = format!("{}{}", "<a>".repeat(50_000), "</a>".repeat(50_000));
        assert!(parse(deep.as_bytes()).is_none());
    }
}
