use std::collections::BTreeMap;

use bytes::Bytes;
use holtmail_pim::ItemKind;

use crate::model::{Caller, DavCollection, Res};
use crate::path::{self, Home};
use crate::xml::{self, APPLE, CALDAV, CARDDAV, CS, DAV, Element, escape};

/// Largest iCalendar resource taken, as `max-resource-size` says.
pub(crate) const MAX_ICALENDAR: usize = holtmail_pim_edit::MAX_ICALENDAR;
/// Largest vCard resource taken.
pub(crate) const MAX_VCARD: usize = holtmail_pim_edit::MAX_VCARD;

/// What a `PROPFIND` or report asks for.
#[derive(Debug, Clone)]
pub(crate) enum Wanted {
    All,
    Names,
    Props(Vec<Element>),
}

impl Wanted {
    /// Whether answering needs the resources' bytes.
    pub fn needs_bytes(&self) -> bool {
        match self {
            Self::All | Self::Names => false,
            Self::Props(props) => props.iter().any(|p| {
                p.is(CALDAV, "calendar-data")
                    || p.is(CARDDAV, "address-data")
                    || p.is(DAV, "getcontentlength")
            }),
        }
    }

    /// The request of a `<prop>`, `<allprop>` or `<propname>` parent; `All` when it has none.
    pub fn of(parent: &Element) -> Self {
        if parent.child(DAV, "propname").is_some() {
            return Self::Names;
        }
        match parent.child(DAV, "prop") {
            Some(prop) => Self::Props(prop.children.clone()),
            None => Self::All,
        }
    }
}

/// What a collection says about itself in a response.
pub(crate) struct CollectionInfo {
    pub token: String,
    pub ctag: String,
    pub writable: bool,
    pub props: BTreeMap<String, String>,
    pub components: Vec<&'static str>,
}

/// A node of the served tree.
pub(crate) enum Node<'a> {
    Root,
    Principals,
    Principal,
    Home(Home),
    Collection(&'a DavCollection, &'a CollectionInfo),
    Resource(&'a Res, bool),
    Inbox,
    Outbox,
}

/// The `{ns}name` key of a property, as display properties are stored.
pub(crate) fn key(ns: &str, name: &str) -> String {
    format!("{{{ns}}}{name}")
}

/// The properties named in an `allprop` or `propname` answer.
fn all_names(node: &Node<'_>) -> Vec<(&'static str, &'static str)> {
    match node {
        Node::Resource(..) => vec![
            (DAV, "resourcetype"),
            (DAV, "getetag"),
            (DAV, "getcontenttype"),
        ],
        Node::Collection(..) => vec![
            (DAV, "resourcetype"),
            (DAV, "displayname"),
            (DAV, "sync-token"),
            (CS, "getctag"),
        ],
        _ => vec![(DAV, "resourcetype"), (DAV, "displayname")],
    }
}

fn href(path: &str) -> String {
    format!("<d:href>{}</d:href>", escape(path))
}

fn privileges(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("<d:privilege><d:{n}/></d:privilege>"))
        .collect()
}

/// The vCard as 3.0: unchanged when it already is, else converted with `calcard`. `None` when
/// it cannot be read.
pub(crate) fn vcard3(raw: &Bytes) -> Option<Bytes> {
    let text = std::str::from_utf8(raw).ok()?;
    let card = calcard::vcard::VCard::parse(text).ok()?;
    if card.version() == Some(calcard::vcard::VCardVersion::V3_0) {
        return Some(raw.clone());
    }
    let mut out = String::new();
    card.write_to(&mut out, calcard::vcard::VCardVersion::V3_0)
        .ok()?;
    Some(Bytes::from(out))
}

/// The inner XML of one property, or `None` when the node has no such property.
pub(crate) fn render(caller: &Caller, node: &Node<'_>, prop: &Element) -> Option<String> {
    let user = caller.user.id;
    let name = prop.name.as_str();
    let ns = prop.ns.as_str();
    let principal = path::principal(user);
    match (ns, name, node) {
        (DAV, "resourcetype", Node::Resource(..)) => Some(String::new()),
        (DAV, "resourcetype", Node::Principal) => {
            Some("<d:collection/><d:principal/>".to_owned())
        }
        (DAV, "resourcetype", Node::Collection(c, _)) => Some(match c.home {
            Home::Calendars => "<d:collection/><c:calendar/>".to_owned(),
            Home::AddressBooks => "<d:collection/><card:addressbook/>".to_owned(),
        }),
        (DAV, "resourcetype", Node::Inbox) => {
            Some("<d:collection/><c:schedule-inbox/>".to_owned())
        }
        (DAV, "resourcetype", Node::Outbox) => {
            Some("<d:collection/><c:schedule-outbox/>".to_owned())
        }
        (DAV, "resourcetype", _) => Some("<d:collection/>".to_owned()),
        (DAV, "displayname", Node::Inbox) => Some("Inbox".to_owned()),
        (DAV, "displayname", Node::Outbox) => Some("Outbox".to_owned()),
        (DAV, "displayname", Node::Principal) => Some(escape(&caller.user.name)),
        (DAV, "displayname", Node::Home(Home::Calendars)) => Some("Calendars".to_owned()),
        (DAV, "displayname", Node::Home(Home::AddressBooks)) => {
            Some("Address books".to_owned())
        }
        (DAV, "displayname", Node::Collection(c, info)) => Some(escape(
            info.props
                .get(&key(DAV, "displayname"))
                .unwrap_or(&c.name),
        )),
        (DAV, "current-user-principal", _) => Some(href(&principal)),
        (DAV, "principal-URL", Node::Principal) => Some(href(&principal)),
        (DAV, "owner", Node::Collection(..) | Node::Home(_) | Node::Principal) => {
            Some(href(&principal))
        }
        (DAV, "principal-collection-set", _) => Some(href(&path::principals())),
        (CALDAV, "calendar-home-set", Node::Principal | Node::Root) => {
            Some(href(&path::home(Home::Calendars, user)))
        }
        (CARDDAV, "addressbook-home-set", Node::Principal | Node::Root) => {
            Some(href(&path::home(Home::AddressBooks, user)))
        }
        (CALDAV, "calendar-user-address-set", Node::Principal) => match &caller.sched {
            Some(sched) => Some(
                sched
                    .own
                    .iter()
                    .map(|a| href(&format!("mailto:{a}")))
                    .collect(),
            ),
            None => Some(href(&format!("mailto:{}", caller.user.email))),
        },
        (CALDAV, "schedule-inbox-URL", Node::Principal) if caller.sched.is_some() => {
            Some(href(&path::inbox(user)))
        }
        (CALDAV, "schedule-outbox-URL", Node::Principal) if caller.sched.is_some() => {
            Some(href(&path::outbox(user)))
        }
        (CALDAV, "schedule-default-calendar-URL", Node::Principal | Node::Inbox) => caller
            .sched
            .as_ref()
            .and_then(|s| s.default_calendar.as_ref())
            .map(|key| href(&path::collection(Home::Calendars, user, key))),
        (CALDAV, "schedule-calendar-transp", Node::Collection(c, _))
            if c.home == Home::Calendars && caller.sched.is_some() =>
        {
            Some("<c:opaque/>".to_owned())
        }
        (CALDAV, "schedule-tag", Node::Resource(res, _)) if caller.sched.is_some() => {
            res.quoted_schedule_tag().map(|t| escape(&t))
        }
        (DAV, "current-user-privilege-set", node) => {
            let mut names = vec!["read", "read-current-user-privilege-set"];
            match node {
                Node::Collection(_, info) => {
                    names.push("write-properties");
                    if info.writable {
                        names.extend(["write", "write-content", "bind", "unbind"]);
                    }
                }
                Node::Resource(_, true) => names.extend(["write", "write-content"]),
                Node::Inbox => names.push("unbind"),
                _ => {}
            }
            let mut out = privileges(&names);
            match node {
                Node::Inbox => out.push_str(
                    "<d:privilege><c:schedule-deliver/></d:privilege><d:privilege><c:schedule-deliver-invite/></d:privilege><d:privilege><c:schedule-deliver-reply/></d:privilege><d:privilege><c:schedule-query-freebusy/></d:privilege>",
                ),
                Node::Outbox => out.push_str(
                    "<d:privilege><c:schedule-send/></d:privilege><d:privilege><c:schedule-send-invite/></d:privilege><d:privilege><c:schedule-send-reply/></d:privilege><d:privilege><c:schedule-send-freebusy/></d:privilege>",
                ),
                _ => {}
            }
            Some(out)
        }
        (DAV, "supported-report-set", node) => {
            let reports: &[(&str, &str)] = match node {
                Node::Collection(c, _) if c.home == Home::Calendars => &[
                    ("d", "sync-collection"),
                    ("c", "calendar-multiget"),
                    ("c", "calendar-query"),
                    ("c", "free-busy-query"),
                ],
                Node::Collection(..) => &[
                    ("d", "sync-collection"),
                    ("card", "addressbook-multiget"),
                    ("card", "addressbook-query"),
                ],
                Node::Resource(..) | Node::Outbox => &[],
                Node::Inbox => &[("c", "calendar-multiget")],
                _ => &[("d", "principal-property-search")],
            };
            Some(
                reports
                    .iter()
                    .map(|(p, r)| {
                        format!("<d:supported-report><d:report><{p}:{r}/></d:report></d:supported-report>")
                    })
                    .collect(),
            )
        }
        (DAV, "sync-token", Node::Collection(_, info)) => Some(escape(&info.token)),
        (CS, "getctag", Node::Collection(_, info)) => Some(escape(&info.ctag)),
        (CALDAV, "supported-calendar-component-set", Node::Collection(c, info))
            if c.home == Home::Calendars =>
        {
            Some(
                info.components
                    .iter()
                    .map(|n| format!("<c:comp name=\"{n}\"/>"))
                    .collect(),
            )
        }
        (CALDAV, "supported-calendar-data", Node::Collection(c, _)) if c.home == Home::Calendars => {
            Some("<c:calendar-data content-type=\"text/calendar\" version=\"2.0\"/>".to_owned())
        }
        (CARDDAV, "supported-address-data", Node::Collection(c, _))
            if c.home == Home::AddressBooks =>
        {
            Some(
                "<card:address-data-type content-type=\"text/vcard\" version=\"3.0\"/><card:address-data-type content-type=\"text/vcard\" version=\"4.0\"/>"
                    .to_owned(),
            )
        }
        (CALDAV, "max-resource-size", Node::Collection(c, _)) if c.home == Home::Calendars => {
            Some(MAX_ICALENDAR.to_string())
        }
        (CARDDAV, "max-resource-size", Node::Collection(c, _)) if c.home == Home::AddressBooks => {
            Some(MAX_VCARD.to_string())
        }
        (APPLE, "calendar-color", Node::Collection(c, info)) if c.home == Home::Calendars => info
            .props
            .get(&key(APPLE, "calendar-color"))
            .or(c.color.as_ref())
            .map(|v| escape(v)),
        (_, _, Node::Collection(_, info)) if info.props.contains_key(&key(ns, name)) => {
            info.props.get(&key(ns, name)).map(|v| escape(v))
        }
        (DAV, "getetag", Node::Resource(res, _)) => Some(escape(&res.quoted_etag())),
        (DAV, "getcontenttype", Node::Resource(res, _)) => {
            Some(format!("{}; charset=utf-8", res.kind.content_type()))
        }
        (DAV, "getcontentlength", Node::Resource(res, _)) => {
            res.bytes.as_ref().map(|b| b.len().to_string())
        }
        (CALDAV, "calendar-data", Node::Resource(res, _)) if res.kind != ItemKind::Contact => {
            res.bytes
                .as_ref()
                .map(|b| escape(&String::from_utf8_lossy(b)))
        }
        (CARDDAV, "address-data", Node::Resource(res, _)) if res.kind == ItemKind::Contact => {
            let bytes = res.bytes.as_ref()?;
            let bytes = match prop.attr("version") {
                Some("3.0") => vcard3(bytes)?,
                _ => bytes.clone(),
            };
            Some(escape(&String::from_utf8_lossy(&bytes)))
        }
        _ => None,
    }
}

/// One `<d:response>` for a node: the properties it has with 200, the rest with 404.
pub(crate) fn response(caller: &Caller, target: &str, node: &Node<'_>, wanted: &Wanted) -> String {
    let mut found = String::new();
    let mut missing = String::new();
    match wanted {
        Wanted::Names => {
            for (ns, name) in all_names(node) {
                found.push_str(&xml::element(ns, name, ""));
            }
        }
        Wanted::All => {
            for (ns, name) in all_names(node) {
                let prop = Element {
                    ns: ns.to_owned(),
                    name: name.to_owned(),
                    ..Element::default()
                };
                if let Some(inner) = render(caller, node, &prop) {
                    found.push_str(&xml::element(ns, name, &inner));
                }
            }
        }
        Wanted::Props(props) => {
            for prop in props {
                match render(caller, node, prop) {
                    Some(inner) => found.push_str(&xml::element(&prop.ns, &prop.name, &inner)),
                    None => missing.push_str(&xml::element(&prop.ns, &prop.name, "")),
                }
            }
        }
    }
    let mut out = format!("<d:response>{}", href(target));
    if !found.is_empty() || missing.is_empty() {
        out.push_str(&format!(
            "<d:propstat><d:prop>{found}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>"
        ));
    }
    if !missing.is_empty() {
        out.push_str(&format!(
            "<d:propstat><d:prop>{missing}</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>"
        ));
    }
    out.push_str("</d:response>");
    out
}

/// A `<d:response>` with only a status, for a resource that is gone or cut off.
pub(crate) fn status_response(target: &str, status: &str) -> String {
    format!(
        "<d:response>{}<d:status>HTTP/1.1 {status}</d:status></d:response>",
        href(target)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_vcard4_to_3() {
        let v4 = Bytes::from_static(
            b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:1\r\nFN:Bea Example\r\nEMAIL;TYPE=work:bea@example.org\r\nEND:VCARD\r\n",
        );
        let v3 = vcard3(&v4).unwrap();
        let text = String::from_utf8(v3.to_vec()).unwrap();
        assert!(text.contains("VERSION:3.0"), "{text}");
        assert!(text.contains("bea@example.org"));
        let already = Bytes::from_static(b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:A\r\nEND:VCARD\r\n");
        assert_eq!(vcard3(&already).unwrap(), already);
    }
}
