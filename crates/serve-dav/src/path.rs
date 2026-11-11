use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holtmail_core::{ArchiveId, CollectionId, UserId};

/// Where every served path starts.
pub(crate) const BASE: &str = "/dav";

/// Which home a collection lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Home {
    Calendars,
    AddressBooks,
}

impl Home {
    fn segment(self) -> &'static str {
        match self {
            Self::Calendars => "calendars",
            Self::AddressBooks => "addressbooks",
        }
    }
}

/// A collection's name in its home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CollectionKey {
    Stored(CollectionId),
    People(ArchiveId),
}

impl CollectionKey {
    fn parse(segment: &str) -> Option<Self> {
        match segment.strip_prefix("people-") {
            Some(archive) => archive.parse().ok().map(Self::People),
            None => segment.parse().ok().map(Self::Stored),
        }
    }

    pub fn segment(&self) -> String {
        match self {
            Self::Stored(id) => id.to_string(),
            Self::People(archive) => format!("people-{archive}"),
        }
    }
}

/// What a request path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Root,
    Principals,
    Principal(UserId),
    Home(Home, UserId),
    Collection(Home, UserId, CollectionKey),
    Resource(Home, UserId, CollectionKey, String),
    Inbox(UserId),
    InboxItem(UserId, String),
    Outbox(UserId),
}

impl Target {
    /// The user a path belongs to, when it names one.
    pub fn user(&self) -> Option<UserId> {
        match self {
            Self::Root | Self::Principals => None,
            Self::Principal(u)
            | Self::Home(_, u)
            | Self::Collection(_, u, _)
            | Self::Resource(_, u, _, _)
            | Self::Inbox(u)
            | Self::InboxItem(u, _)
            | Self::Outbox(u) => Some(*u),
        }
    }
}

/// Reads a raw request path (still percent-encoded). `None` for anything outside the layout.
pub(crate) fn parse(raw: &str) -> Option<Target> {
    if raw == "/" {
        return Some(Target::Root);
    }
    let rest = raw.strip_prefix(BASE)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let segments: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(decode)
        .collect::<Option<_>>()?;
    let user = |s: &String| s.parse::<UserId>().ok();
    let home = |s: &str| match s {
        "calendars" => Some(Home::Calendars),
        "addressbooks" => Some(Home::AddressBooks),
        _ => None,
    };
    Some(match segments.as_slice() {
        [] => Target::Root,
        [p] if p == "principals" => Target::Principals,
        [p, u] if p == "principals" => Target::Principal(user(u)?),
        [h, u, b] if h == "calendars" && b == "inbox" => Target::Inbox(user(u)?),
        [h, u, b] if h == "calendars" && b == "outbox" => Target::Outbox(user(u)?),
        [h, u, b, r] if h == "calendars" && b == "inbox" => Target::InboxItem(user(u)?, r.clone()),
        [h, u] => Target::Home(home(h)?, user(u)?),
        [h, u, c] => Target::Collection(home(h)?, user(u)?, CollectionKey::parse(c)?),
        [h, u, c, r] => Target::Resource(home(h)?, user(u)?, CollectionKey::parse(c)?, href_of(r)),
        _ => return None,
    })
}

pub(crate) fn principal(user: UserId) -> String {
    format!("{BASE}/principals/{user}/")
}

pub(crate) fn inbox(user: UserId) -> String {
    format!("{BASE}/calendars/{user}/inbox/")
}

pub(crate) fn outbox(user: UserId) -> String {
    format!("{BASE}/calendars/{user}/outbox/")
}

pub(crate) fn principals() -> String {
    format!("{BASE}/principals/")
}

pub(crate) fn home(home: Home, user: UserId) -> String {
    format!("{BASE}/{}/{user}/", home.segment())
}

pub(crate) fn collection(home_kind: Home, user: UserId, key: &CollectionKey) -> String {
    format!("{}{}/", home(home_kind, user), key.segment())
}

pub(crate) fn resource(home_kind: Home, user: UserId, key: &CollectionKey, href: &str) -> String {
    format!(
        "{}{}",
        collection(home_kind, user, key),
        encode(&name_of(href))
    )
}

/// The path segment a stored href is served under: the href itself, or for one holding `/`
/// (an upstream path), `~` and its base64url form, so clients that decode `%2F` still see one
/// segment.
pub(crate) fn name_of(href: &str) -> String {
    match href.contains('/') || href.starts_with('~') {
        true => format!("~{}", URL_SAFE_NO_PAD.encode(href)),
        false => href.to_owned(),
    }
}

/// The stored href a served segment names; the inverse of [`name_of`].
pub(crate) fn href_of(name: &str) -> String {
    name.strip_prefix('~')
        .and_then(|b| URL_SAFE_NO_PAD.decode(b).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| name.to_owned())
}

/// Percent-encodes one path segment: everything but unreserved characters and a few safe
/// marks, so a stored href holding `/` stays one segment.
pub(crate) fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' | b'+' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decodes one path segment; `None` for a malformed escape or bytes that are not UTF-8.
pub(crate) fn decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = segment.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The path of an href a client sent in a report: an absolute URL loses its origin.
pub(crate) fn href_path(href: &str) -> &str {
    let href = href.trim();
    match href.find("://") {
        Some(scheme) => {
            let after = &href[scheme + 3..];
            after.find('/').map_or("/", |slash| &after[slash..])
        }
        None => href,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_layout() {
        let user = UserId::new();
        let collection = CollectionId::new();
        let archive = ArchiveId::new();
        assert_eq!(parse("/"), Some(Target::Root));
        assert_eq!(parse("/dav"), Some(Target::Root));
        assert_eq!(parse("/dav/"), Some(Target::Root));
        assert_eq!(parse("/dav/principals/"), Some(Target::Principals));
        assert_eq!(parse(&principal(user)), Some(Target::Principal(user)));
        assert_eq!(
            parse(&format!("/dav/calendars/{user}/{collection}/a%20b.ics")),
            Some(Target::Resource(
                Home::Calendars,
                user,
                CollectionKey::Stored(collection),
                "a b.ics".into()
            ))
        );
        assert_eq!(
            parse(&collection_path(user, archive)),
            Some(Target::Collection(
                Home::AddressBooks,
                user,
                CollectionKey::People(archive)
            ))
        );
        assert_eq!(
            parse(&format!("/dav/calendars/{user}/inbox/")),
            Some(Target::Inbox(user))
        );
        assert_eq!(parse(&outbox(user)), Some(Target::Outbox(user)));
        assert_eq!(
            parse(&format!("{}a.ics", inbox(user))),
            Some(Target::InboxItem(user, "a.ics".into()))
        );
        assert_eq!(parse("/davx"), None);
        assert_eq!(parse("/dav/elsewhere/x"), None);
        assert_eq!(parse("/dav/calendars/not-a-user/"), None);
        assert_eq!(parse("/dav/calendars/x/%zz"), None);
    }

    fn collection_path(user: UserId, archive: ArchiveId) -> String {
        collection(Home::AddressBooks, user, &CollectionKey::People(archive))
    }

    #[test]
    fn hrefs_with_slashes_become_one_segment() {
        let user = UserId::new();
        let key = CollectionKey::Stored(CollectionId::new());
        for href in ["/cals/work/standup.ics", "lunch.ics", "~odd", "a b.vcf"] {
            let served = resource(Home::Calendars, user, &key, href);
            assert_eq!(served.matches('/').count(), 5, "{served}");
            match parse(&served) {
                Some(Target::Resource(_, _, _, back)) => assert_eq!(back, href),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(name_of("lunch.ics"), "lunch.ics");
    }

    #[test]
    fn encodes_round_trip() {
        let odd = "/remote.php/dav/x y/ä.ics";
        assert_eq!(decode(&encode(odd)).as_deref(), Some(odd));
        assert_eq!(encode("abc-1.vcf"), "abc-1.vcf");
        assert_eq!(href_path("https://dav.example.com/dav/x/"), "/dav/x/");
        assert_eq!(href_path("/dav/x/"), "/dav/x/");
    }
}
