use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, SystemTime};

use holtmail_category::TopicSet;
use holtmail_category_data::{Header, Row, is_newer, is_valid_domain, parse_version, write};
use url::{Host, Url};

use crate::Error;
use crate::industries::Found;

/// The largest compressed file written: the server bundles it.
pub(crate) const MAX_GZIP_BYTES: usize = 2 * 1024 * 1024;

/// The header name of the Wikidata file.
pub(crate) const NAME: &str = "wikidata";

/// Why a website gave no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Skip {
    /// Not a URL.
    Url,
    /// Neither `https` nor `http`.
    Scheme,
    /// A page below the root, as on a shared host.
    Path,
    /// No domain name, or no registrable domain (an IP address, a bare public suffix).
    Host,
    /// The registrable domain is not a valid key.
    Domain,
    /// The domain is a shared platform listed in `skip_domains`.
    Listed,
    /// The domain is a consumer mailbox provider listed in `mailbox_domains`.
    Mailbox,
    /// A UT1 line that is not a bare host name.
    Line,
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Url => "not a URL",
            Self::Scheme => "not http or https",
            Self::Path => "a page, not a site root",
            Self::Host => "no registrable domain",
            Self::Domain => "not a valid key",
            Self::Listed => "a listed shared platform",
            Self::Mailbox => "a listed mailbox provider",
            Self::Line => "not a bare host name",
        })
    }
}

/// Whether `domain` or any parent domain of it is in `list`.
pub(crate) fn listed(domain: &str, list: &BTreeSet<String>) -> bool {
    let mut rest = domain;
    loop {
        if list.contains(rest) {
            return true;
        }
        match rest.split_once('.') {
            Some((_, parent)) => rest = parent,
            None => return false,
        }
    }
}

/// The key for an official website, as a lowercase A-label: its registrable domain when the
/// site is the domain itself or its `www`, else its full host without `www.`, so a sub-brand's
/// topics never spread to the parent domain.
pub(crate) fn site_key(
    site: &str,
    skip: &BTreeSet<String>,
    mailbox: &BTreeSet<String>,
) -> Result<String, Skip> {
    let url = Url::parse(site.trim()).map_err(|_| Skip::Url)?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(Skip::Scheme);
    }
    if !matches!(url.path(), "" | "/") {
        return Err(Skip::Path);
    }
    let host = match url.host() {
        Some(Host::Domain(host)) => host.to_ascii_lowercase(),
        _ => return Err(Skip::Host),
    };
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let registrable = holtmail_links::registrable_domain(host).ok_or(Skip::Host)?;
    let key = if host.len() > registrable.len() {
        host.to_owned()
    } else {
        registrable
    };
    if !is_valid_domain(&key) {
        return Err(Skip::Domain);
    }
    if listed(&key, skip) {
        return Err(Skip::Listed);
    }
    if listed(&key, mailbox) {
        return Err(Skip::Mailbox);
    }
    Ok(key)
}

/// One domain before the gate: the union of its organisations' topics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    pub topics: TopicSet,
    pub organisations: BTreeSet<u64>,
}

/// The rows assembled from everything fetched, with what was skipped and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Assembled {
    pub domains: BTreeMap<String, Entry>,
    pub skipped: BTreeMap<Skip, usize>,
}

impl Assembled {
    /// The rows in byte order of the domain, sourced to the smallest organisation QID.
    pub(crate) fn rows(&self) -> Vec<Row> {
        self.domains
            .iter()
            .filter(|(_, e)| !e.topics.is_empty())
            .map(|(domain, entry)| Row {
                domain: domain.clone(),
                topics: entry.topics,
                source: format!(
                    "wikidata:Q{}",
                    entry.organisations.first().copied().unwrap_or_default()
                ),
            })
            .collect()
    }

    /// The domains with the most organisations, largest first, then by name: shared
    /// platforms show up here.
    pub(crate) fn crowded(&self, limit: usize) -> Vec<(String, usize)> {
        let mut all: Vec<(String, usize)> = self
            .domains
            .iter()
            .filter(|(_, e)| e.organisations.len() > 1)
            .map(|(d, e)| (d.clone(), e.organisations.len()))
            .collect();
        all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        all.truncate(limit);
        all
    }
}

/// Keys every website, drops what [`site_key`] refuses and unions topics per domain. The
/// result does not depend on the order of `found`.
pub(crate) fn assemble(
    found: &[Found],
    skip: &BTreeSet<String>,
    mailbox: &BTreeSet<String>,
) -> Assembled {
    let mut assembled = Assembled::default();
    for item in found {
        match site_key(&item.site, skip, mailbox) {
            Ok(key) => {
                let entry = assembled.domains.entry(key).or_default();
                entry.topics =
                    TopicSet::from_bits(entry.topics.bits() | item.topics.known().bits());
                entry.organisations.insert(item.org);
            }
            Err(why) => *assembled.skipped.entry(why).or_default() += 1,
        }
    }
    assembled
}

/// Writes the Wikidata file and refuses one larger than `cap` bytes.
pub(crate) fn encode(version: &str, rows: &[Row], cap: usize) -> Result<Vec<u8>, Error> {
    encode_table(NAME, version, rows, cap)
}

/// Writes a file with this header name and refuses one larger than `cap` bytes.
pub(crate) fn encode_table(
    name: &str,
    version: &str,
    rows: &[Row],
    cap: usize,
) -> Result<Vec<u8>, Error> {
    let bytes = write(&Header::new(name, version), rows)?;
    if bytes.len() > cap {
        return Err(Error::Gate(format!(
            "the file is {} bytes, more than the {cap} allowed",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// The UTC date of `now` as `YYYYMMDD`.
pub(crate) fn today(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    chrono::DateTime::from_timestamp(i64::try_from(secs).unwrap_or_default(), 0)
        .unwrap_or_default()
        .format("%Y%m%d")
        .to_string()
}

/// Checks a version: 1 to 16 ASCII digits, numerically greater than the previous release's.
pub(crate) fn check_version(version: &str, previous: Option<&str>) -> Result<(), Error> {
    if parse_version(version).is_none() {
        return Err(Error::Config(format!(
            "version {version:?} is not 1 to 16 digits"
        )));
    }
    if let Some(previous) = previous {
        if parse_version(previous).is_none() {
            return Err(Error::Config(format!(
                "the previous version {previous:?} is not numeric"
            )));
        }
        if !is_newer(version, previous) {
            return Err(Error::Config(format!(
                "version {version} is not greater than the previous {previous}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use holtmail_category::Topic;
    use holtmail_category_data::{Limits, read};
    use proptest::prelude::*;

    use super::*;
    use crate::industries::Pass;

    fn skip() -> BTreeSet<String> {
        BTreeSet::from(["sites.example".to_owned()])
    }

    fn mailbox() -> BTreeSet<String> {
        BTreeSet::from(["mail.example".to_owned(), "post.example.net".to_owned()])
    }

    #[test]
    fn keys_and_skip_rules() {
        let key = |s: &str| site_key(s, &skip(), &mailbox());
        assert_eq!(
            key("https://www.example.com/").as_deref(),
            Ok("example.com")
        );
        assert_eq!(
            key("http://Shop.Example.ORG").as_deref(),
            Ok("shop.example.org")
        );
        assert_eq!(
            key("https://www.shop.example.org/").as_deref(),
            Ok("shop.example.org")
        );
        assert_eq!(
            key(" https://example.com:8443/?lang=en ").as_deref(),
            Ok("example.com")
        );
        assert_eq!(key("https://www.example/").as_deref(), Err(&Skip::Host));
        assert_eq!(
            key("https://bücher.example/").as_deref(),
            Ok("xn--bcher-kva.example")
        );
        assert_eq!(
            key("https://a.b.example.co.uk/").as_deref(),
            Ok("a.b.example.co.uk")
        );
        assert_eq!(
            key("https://www.example.co.uk/").as_deref(),
            Ok("example.co.uk")
        );
        assert_eq!(key("https://example.org/about"), Err(Skip::Path));
        assert_eq!(key("https://example.org/shop/"), Err(Skip::Path));
        assert_eq!(key("ftp://files.example.com/"), Err(Skip::Scheme));
        assert_eq!(key("mailto:info@example.com"), Err(Skip::Scheme));
        assert_eq!(key("not a url"), Err(Skip::Url));
        assert_eq!(key("https://192.0.2.1/"), Err(Skip::Host));
        assert_eq!(key("https://[2001:db8::1]/"), Err(Skip::Host));
        assert_eq!(key("https://co.uk/"), Err(Skip::Host));
        assert_eq!(key("https://shop.sites.example/"), Err(Skip::Listed));
        assert_eq!(key("https://sites.example"), Err(Skip::Listed));
        assert_eq!(key("https://www.mail.example/"), Err(Skip::Mailbox));
        assert_eq!(key("https://shop.mail.example/"), Err(Skip::Mailbox));
        assert_eq!(key("https://post.example.net/"), Err(Skip::Mailbox));
        assert_eq!(key("https://example.net/").as_deref(), Ok("example.net"));
    }

    #[test]
    fn listed_matches_parents_only() {
        let list = mailbox();
        assert!(listed("mail.example", &list));
        assert!(listed("a.b.mail.example", &list));
        assert!(!listed("email.example", &list));
        assert!(!listed("example", &list));
        assert!(!listed("example.net", &list));
    }

    fn found(org: u64, site: &str, topic: Topic) -> Found {
        Found {
            pass: Pass::P452,
            org,
            site: site.into(),
            topics: TopicSet::EMPTY.with(topic),
        }
    }

    fn sample() -> Vec<Found> {
        vec![
            found(9_100_007, "https://www.example.org/", Topic::Purchases),
            found(9_100_003, "https://example.com/", Topic::Travel),
            found(9_100_002, "https://shop.example.org", Topic::Finance),
            found(9_100_001, "https://example.org/about", Topic::Jobs),
            found(9_100_005, "https://shop.sites.example/", Topic::Social),
            found(9_100_004, "https://travel.example/", Topic::Travel),
            found(9_100_006, "http://example.org", Topic::Finance),
            found(9_100_008, "https://www.mail.example/", Topic::Purchases),
        ]
    }

    #[test]
    fn union_sort_and_source() {
        let assembled = assemble(&sample(), &skip(), &mailbox());
        let rows = assembled.rows();
        let domains: Vec<&str> = rows.iter().map(|r| r.domain.as_str()).collect();
        assert_eq!(
            domains,
            [
                "example.com",
                "example.org",
                "shop.example.org",
                "travel.example"
            ]
        );
        assert_eq!(
            rows[1].topics,
            TopicSet::EMPTY.with(Topic::Purchases).with(Topic::Finance)
        );
        assert_eq!(rows[1].source, "wikidata:Q9100006");
        assert_eq!(
            rows[2].topics,
            TopicSet::EMPTY.with(Topic::Finance),
            "a subdomain site keeps its topics to itself"
        );
        assert_eq!(assembled.skipped[&Skip::Path], 1);
        assert_eq!(assembled.skipped[&Skip::Listed], 1);
        assert_eq!(assembled.skipped[&Skip::Mailbox], 1);
        assert_eq!(assembled.crowded(5), [("example.org".to_owned(), 2)]);
    }

    #[test]
    fn deterministic_bytes() {
        let mut reversed = sample();
        reversed.reverse();
        let a = encode(
            "20261107",
            &assemble(&sample(), &skip(), &mailbox()).rows(),
            MAX_GZIP_BYTES,
        )
        .unwrap();
        let b = encode(
            "20261107",
            &assemble(&reversed, &skip(), &mailbox()).rows(),
            MAX_GZIP_BYTES,
        )
        .unwrap();
        assert_eq!(a, b);
        let (header, rows) = read(&a, Limits::default()).unwrap();
        assert_eq!(
            (header.name.as_str(), header.version.as_str()),
            (NAME, "20261107")
        );
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn size_cap() {
        let rows = assemble(&sample(), &skip(), &mailbox()).rows();
        let size = encode("1", &rows, MAX_GZIP_BYTES).unwrap().len();
        assert!(encode("1", &rows, size).is_ok());
        assert!(matches!(encode("1", &rows, size - 1), Err(Error::Gate(_))));
    }

    #[test]
    fn versions() {
        assert!(check_version("20261107", None).is_ok());
        assert!(check_version("20261107", Some("0")).is_ok());
        assert!(check_version("20261107", Some("20261031")).is_ok());
        assert!(check_version("20261107", Some("20261107")).is_err());
        assert!(check_version("20261106", Some("20261107")).is_err());
        assert!(check_version("2026110701", Some("20261107")).is_ok());
        assert!(check_version("2026-11-07", None).is_err());
        assert!(check_version("", None).is_err());
        assert!(check_version("20261107", Some("v1")).is_err());
        assert_eq!(
            today(SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_009_600)),
            "20261107"
        );
        assert_eq!(today(SystemTime::UNIX_EPOCH), "19700101");
    }

    proptest! {
        #[test]
        fn site_key_never_panics_and_keys_are_valid(site in "\\PC{0,80}") {
            if let Ok(key) = site_key(&site, &BTreeSet::new(), &BTreeSet::new()) {
                prop_assert!(is_valid_domain(&key));
            }
        }

        #[test]
        fn order_does_not_matter(seed in proptest::collection::vec((0u64..6, 0usize..6), 0..20)) {
            let sites = [
                "https://example.com/", "https://www.example.org", "https://a.example/",
                "https://b.example/x", "http://c.example", "https://example.net/",
            ];
            let found: Vec<Found> = seed
                .iter()
                .map(|(org, site)| found(9_000_000 + org, sites[*site], Topic::ALL[*site]))
                .collect();
            let mut reversed = found.clone();
            reversed.reverse();
            prop_assert_eq!(
                assemble(&found, &skip(), &mailbox()).rows(),
                assemble(&reversed, &skip(), &mailbox()).rows()
            );
        }
    }
}
