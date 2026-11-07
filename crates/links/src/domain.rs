use publicsuffix::Psl;

use crate::data::PSL_LIST;

const MAX_HOST_BYTES: usize = 253;

/// The registrable domain (eTLD+1) of `host`, as a lowercase A-label.
///
/// `host` may be Unicode or an A-label in any case, with at most one trailing dot. It is converted
/// with IDNA (UTS 46) and looked up in the bundled Public Suffix List, ICANN and private sections
/// both, with the list's implicit `*` rule for unknown top-level domains.
///
/// Guarantees: `None` for an IP literal, an empty or single-label host, a bare public suffix, a host
/// with an empty label or longer than 253 bytes, and anything IDNA rejects. A returned domain is a
/// label-aligned suffix of the A-label form of `host`, and is its own registrable domain.
/// Deterministic for a given [`psl_version`](crate::psl_version); never panics.
pub fn registrable_domain(host: &str) -> Option<String> {
    let ascii = to_ascii_host(host)?;
    let domain = PSL_LIST.domain(ascii.as_bytes())?;
    std::str::from_utf8(domain.as_bytes())
        .ok()
        .map(str::to_owned)
}

pub(crate) fn to_ascii_host(host: &str) -> Option<String> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > MAX_HOST_BYTES * 4 || host.starts_with('[') {
        return None;
    }
    let ascii = match url::Host::parse(host).ok()? {
        url::Host::Domain(d) => d.to_ascii_lowercase(),
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => return None,
    };
    if ascii.is_empty()
        || ascii.len() > MAX_HOST_BYTES
        || !ascii.contains('.')
        || ascii.split('.').any(str::is_empty)
    {
        return None;
    }
    Some(ascii)
}

#[cfg(test)]
mod tests {
    use super::registrable_domain;

    fn rd(host: &str) -> Option<String> {
        registrable_domain(host)
    }

    #[test]
    fn icann_and_private_suffixes() {
        assert_eq!(rd("mail.example.com").as_deref(), Some("example.com"));
        assert_eq!(rd("Example.COM").as_deref(), Some("example.com"));
        assert_eq!(rd("example.com.").as_deref(), Some("example.com"));
        assert_eq!(rd("a.b.example.co.uk").as_deref(), Some("example.co.uk"));
        assert_eq!(
            rd("someone.github.io").as_deref(),
            Some("someone.github.io")
        );
        assert_eq!(
            rd("a.someone.github.io").as_deref(),
            Some("someone.github.io")
        );
        assert_eq!(rd("shop.evil.example").as_deref(), Some("evil.example"));
    }

    #[test]
    fn idn() {
        assert_eq!(rd("пример.рф").as_deref(), Some("xn--e1afmkfd.xn--p1ai"));
        assert_eq!(
            rd("www.XN--E1AFMKFD.xn--p1ai").as_deref(),
            Some("xn--e1afmkfd.xn--p1ai")
        );
        assert_eq!(
            rd("www.bücher.example").as_deref(),
            Some("xn--bcher-kva.example")
        );
        assert_eq!(rd("xn--zz.example"), None);
    }

    #[test]
    fn none_cases() {
        for host in [
            "",
            ".",
            "com",
            "localhost",
            "co.uk",
            "github.io",
            "192.0.2.1",
            "[2001:db8::1]",
            "2001:db8::1",
            "0x7f.1",
            "a..example.com",
            "example.com..",
            "exa mple.com",
            "рф",
        ] {
            assert_eq!(rd(host), None, "{host}");
        }
        assert_eq!(rd(&format!("{}.example.com", "a".repeat(260))), None);
    }
}
