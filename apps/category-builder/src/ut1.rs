use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use holtmail_category::{Topic, TopicSet};
use holtmail_category_data::Row;
use serde::Deserialize;

use crate::Error;
use crate::build::{Skip, site_key};

/// The header name and source column of the UT1 file.
pub(crate) const NAME: &str = "ut1";

/// The largest `domains` file read from the checkout, in bytes.
pub(crate) const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    repository: String,
    commit: String,
    categories: BTreeMap<String, String>,
}

/// `data/ut1.toml`: the pinned upstream commit and the UT1 categories mapped to topics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ut1 {
    pub repository: String,
    pub commit: String,
    pub categories: BTreeMap<String, Topic>,
}

/// Whether `commit` is a full 40-character lowercase hex commit id.
pub(crate) fn is_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Ut1 {
    /// Parses and checks the file: an https repository, a full commit id, at least one
    /// category, category names of `[a-z0-9_-]`, and known topics that are never sensitive.
    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        let file: File = toml::from_str(text)
            .map_err(|e| Error::Config(format!("ut1.toml: {}", e.message())))?;
        if !file.repository.starts_with("https://") {
            return Err(Error::Config("ut1.toml: repository is not https".into()));
        }
        if !is_commit(&file.commit) {
            return Err(Error::Config(
                "ut1.toml: commit is not a 40-character hex id".into(),
            ));
        }
        if file.categories.is_empty() {
            return Err(Error::Config("ut1.toml: no category".into()));
        }
        let mut categories = BTreeMap::new();
        for (category, name) in file.categories {
            if category.is_empty()
                || !category
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                return Err(Error::Config(format!(
                    "ut1.toml: category {category:?} is not a plain name"
                )));
            }
            let topic = Topic::from_name(&name)
                .ok_or_else(|| Error::Config(format!("ut1.toml: {category}: topic {name}")))?;
            if topic.is_sensitive() {
                return Err(Error::Config(format!(
                    "ut1.toml: {category}: {name} is sensitive and never comes from UT1"
                )));
            }
            categories.insert(category, topic);
        }
        Ok(Self {
            repository: file.repository,
            commit: file.commit,
            categories,
        })
    }
}

/// The rows assembled from a UT1 checkout, with what was skipped and the hosts read per
/// category.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Assembled {
    pub domains: BTreeMap<String, TopicSet>,
    pub skipped: BTreeMap<Skip, usize>,
    pub read: BTreeMap<String, usize>,
}

impl Assembled {
    /// The rows in byte order of the domain, every source `ut1`.
    pub(crate) fn rows(&self) -> Vec<Row> {
        self.domains
            .iter()
            .map(|(domain, topics)| Row {
                domain: domain.clone(),
                topics: *topics,
                source: NAME.into(),
            })
            .collect()
    }
}

/// The `blacklists` directory of a checkout or an extracted tarball: `dir/blacklists` when
/// it exists, else `dir` itself.
fn lists_dir(dir: &Path) -> PathBuf {
    let nested = dir.join("blacklists");
    if nested.is_dir() {
        nested
    } else {
        dir.to_owned()
    }
}

/// The key for one line of a UT1 `domains` file: the site-key rule of the Wikidata build
/// applied to `https://<line>/`. A line with anything but letters, digits, `.`, `-` and `_`
/// is refused, so a URL, a port or user info never sneaks a different host in.
pub(crate) fn host_key(
    line: &str,
    skip: &BTreeSet<String>,
    mailbox: &BTreeSet<String>,
) -> Result<String, Skip> {
    if line.is_empty()
        || !line
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(Skip::Line);
    }
    site_key(&format!("https://{line}/"), skip, mailbox)
}

/// Reads `<category>/domains` for every mapped category, keys each host and unions topics per
/// key. Empty lines and `#` comments are ignored. Only mapped categories are read, so an
/// unmapped list (`bank`, `financial`, ...) never reaches the output. A missing or oversized
/// list fails, so an upstream rename never ships a silently smaller file. The result does not
/// depend on the order of lines or categories.
pub(crate) fn assemble(
    dir: &Path,
    config: &Ut1,
    skip: &BTreeSet<String>,
    mailbox: &BTreeSet<String>,
) -> Result<Assembled, Error> {
    let lists = lists_dir(dir);
    let mut assembled = Assembled::default();
    for (category, topic) in &config.categories {
        let path = lists.join(category).join("domains");
        let text = read_list(&path)?;
        let mut count = 0usize;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            count += 1;
            match host_key(line, skip, mailbox) {
                Ok(key) => assembled.domains.entry(key).or_default().insert(*topic),
                Err(why) => *assembled.skipped.entry(why).or_default() += 1,
            }
        }
        assembled.read.insert(category.clone(), count);
    }
    Ok(assembled)
}

fn read_list(path: &Path) -> Result<String, Error> {
    let io = |e: std::io::Error| Error::Io(format!("reading {}: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(Error::Gate(format!(
            "{} is larger than {MAX_INPUT_BYTES} bytes",
            path.display()
        )));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The notice published next to the UT1 file: `data/LICENSE-DATA` with the repository and
/// commit filled in.
pub(crate) fn license_notice(template: &str, repository: &str, commit: &str) -> String {
    template
        .replace("@REPOSITORY@", repository)
        .replace("@COMMIT@", commit)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const CONFIG: &str = "repository = \"https://github.com/olbat/ut1-blacklists\"\ncommit = \"0123456789abcdef0123456789abcdef01234567\"\n\n[categories]\nshopping = \"purchases\"\njobsearch = \"jobs\"\nsocial_networks = \"social\"\n";

    pub(crate) fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ut1")
    }

    fn skip() -> BTreeSet<String> {
        BTreeSet::from(["sites.example".to_owned()])
    }

    fn mailbox() -> BTreeSet<String> {
        BTreeSet::from(["mail.example".to_owned()])
    }

    #[test]
    fn config() {
        let ut1 = Ut1::parse(CONFIG).unwrap();
        assert_eq!(ut1.categories["shopping"], Topic::Purchases);
        assert_eq!(ut1.categories.len(), 3);
        let shipped = Ut1::parse(include_str!("../data/ut1.toml")).unwrap();
        assert_eq!(shipped.categories, ut1.categories);
        assert!(is_commit(&shipped.commit));
        for bad in [
            CONFIG.replace("social\"", "finance\""),
            CONFIG.replace("jobs\"", "health\""),
            CONFIG.replace("purchases\"", "government\""),
            CONFIG.replace("purchases\"", "shopping\""),
            CONFIG.replace("0123456789abcdef01234567\"", "HEAD\""),
            CONFIG.replace("https://", "http://"),
            CONFIG.replace("shopping =", "\"Shop ping\" ="),
            format!("{CONFIG}extra = 1\n"),
            "repository = \"https://example.com\"\ncommit = \"0123456789abcdef0123456789abcdef01234567\"\n[categories]\n".into(),
        ] {
            assert!(Ut1::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn host_lines() {
        let key = |s: &str| host_key(s, &skip(), &mailbox());
        assert_eq!(key("www.example.com").as_deref(), Ok("example.com"));
        assert_eq!(key("Shop.Example.ORG").as_deref(), Ok("shop.example.org"));
        assert_eq!(
            key("bücher.example").as_deref(),
            Ok("xn--bcher-kva.example")
        );
        assert_eq!(key("192.0.2.1"), Err(Skip::Host));
        assert_eq!(key("10.1.2"), Err(Skip::Host));
        assert_eq!(key("shop_1.example.com"), Err(Skip::Domain));
        assert_eq!(key("example.com/shop"), Err(Skip::Line));
        assert_eq!(key("example.com:8080"), Err(Skip::Line));
        assert_eq!(key("user@example.com"), Err(Skip::Line));
        assert_eq!(key("a b.example"), Err(Skip::Line));
        assert_eq!(key("shop.sites.example"), Err(Skip::Listed));
        assert_eq!(key("news.mail.example"), Err(Skip::Mailbox));
        assert_eq!(key("co.uk"), Err(Skip::Host));
    }

    #[test]
    fn maps_only_listed_categories() {
        let config = Ut1::parse(CONFIG).unwrap();
        let assembled = assemble(&fixture(), &config, &skip(), &mailbox()).unwrap();
        let rows = assembled.rows();
        let got: Vec<(&str, TopicSet)> =
            rows.iter().map(|r| (r.domain.as_str(), r.topics)).collect();
        let set = |t: &[Topic]| t.iter().copied().collect::<TopicSet>();
        assert_eq!(
            got,
            [
                ("careers.example.org", set(&[Topic::Jobs])),
                ("example.com", set(&[Topic::Purchases])),
                ("friends.example", set(&[Topic::Social])),
                ("jobs.example", set(&[Topic::Jobs])),
                ("market.example.net", set(&[Topic::Purchases, Topic::Jobs])),
                ("shop.example.org", set(&[Topic::Purchases])),
            ]
        );
        assert!(rows.iter().all(|r| r.source == "ut1"));
        assert!(
            rows.iter()
                .all(|r| r.topics.iter().all(|t| !t.is_sensitive()))
        );
        assert!(!rows.iter().any(|r| r.domain.contains("bank")));
        assert_eq!(assembled.skipped[&Skip::Host], 2);
        assert_eq!(assembled.skipped[&Skip::Listed], 1);
        assert_eq!(assembled.skipped[&Skip::Mailbox], 1);
        assert_eq!(assembled.skipped[&Skip::Line], 1);
        assert_eq!(assembled.read["shopping"], 8);

        let nested = tempfile::tempdir().unwrap();
        let lists = nested.path().join("blacklists");
        for category in ["shopping", "jobsearch", "social_networks"] {
            std::fs::create_dir_all(lists.join(category)).unwrap();
            let mut lines: Vec<String> =
                std::fs::read_to_string(fixture().join(category).join("domains"))
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect();
            lines.reverse();
            std::fs::write(lists.join(category).join("domains"), lines.join("\n")).unwrap();
        }
        assert_eq!(
            assemble(nested.path(), &config, &skip(), &mailbox())
                .unwrap()
                .rows(),
            rows
        );
    }

    #[test]
    fn missing_category_fails() {
        let mut config = Ut1::parse(CONFIG).unwrap();
        config.categories.insert("dating".into(), Topic::Social);
        assert!(matches!(
            assemble(&fixture(), &config, &skip(), &mailbox()),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn notice() {
        let text = license_notice(
            include_str!("../data/LICENSE-DATA"),
            "https://github.com/olbat/ut1-blacklists",
            "0123456789abcdef0123456789abcdef01234567",
        );
        assert!(
            text.contains("Contains data from UT1 (Université Toulouse Capitole), CC BY-SA 4.0")
        );
        assert!(text.contains("at commit 0123456789abcdef0123456789abcdef01234567"));
        assert!(!text.contains('@'));
    }
}
