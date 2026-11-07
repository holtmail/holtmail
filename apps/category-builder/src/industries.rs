use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use holtmail_category::{Topic, TopicSet};
use serde::Deserialize;

use crate::Error;
use crate::sparql::{Sparql, qid};

/// Class QIDs per organisation query, within the 20–50 the Wikidata policy suggests.
pub(crate) const BATCH: usize = 30;

/// How an organisation is tied to a root class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub(crate) enum Pass {
    /// `industry` (P452): the first pass.
    P452,
    /// `instance of` (P31): the second pass, for organisations without an industry.
    P31,
}

impl Pass {
    pub(crate) const ALL: [Self; 2] = [Self::P452, Self::P31];
}

impl fmt::Display for Pass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::P452 => "P452",
            Self::P31 => "P31",
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    skip_domains: Vec<String>,
    #[serde(default)]
    mailbox_domains: Vec<String>,
    #[serde(default, rename = "root")]
    roots: Vec<RawRoot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoot {
    qid: String,
    label: String,
    property: Pass,
    topics: Vec<String>,
}

/// A root class whose subclasses map to topics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Root {
    pub qid: u64,
    pub label: String,
    pub pass: Pass,
    pub topics: TopicSet,
}

/// `data/industries.toml`: the roots, and the shared platforms and mailbox providers never
/// written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Industries {
    pub roots: Vec<Root>,
    pub skip_domains: BTreeSet<String>,
    pub mailbox_domains: BTreeSet<String>,
}

fn domain_set(what: &str, domains: Vec<String>) -> Result<BTreeSet<String>, Error> {
    let mut set = BTreeSet::new();
    for domain in domains {
        if !holtmail_category_data::is_valid_domain(&domain) {
            return Err(Error::Config(format!(
                "industries.toml: {what} {domain} is not a valid key"
            )));
        }
        if !set.insert(domain.clone()) {
            return Err(Error::Config(format!(
                "industries.toml: {what} {domain} is listed twice"
            )));
        }
    }
    Ok(set)
}

impl Industries {
    /// Parses and checks the file: QIDs `Q<n>`, a non-empty English label, known topics, no
    /// root twice for the same pass, and skip and mailbox domains that are valid keys, each
    /// listed once.
    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        let file: File = toml::from_str(text)
            .map_err(|e| Error::Config(format!("industries.toml: {}", e.message())))?;
        let mut seen = BTreeSet::new();
        let mut roots = Vec::new();
        for raw in file.roots {
            let bad =
                |what: &str| Error::Config(format!("industries.toml: root {}: {what}", raw.qid));
            let id = qid(&raw.qid)
                .filter(|_| raw.qid.starts_with('Q'))
                .ok_or_else(|| bad("not a QID"))?;
            if raw.label.trim().is_empty() {
                return Err(bad("empty label"));
            }
            let topics = topic_set(&raw.topics).map_err(|t| bad(&format!("topic {t}")))?;
            if !seen.insert((raw.property, id)) {
                return Err(bad("listed twice"));
            }
            roots.push(Root {
                qid: id,
                label: raw.label,
                pass: raw.property,
                topics,
            });
        }
        if roots.is_empty() {
            return Err(Error::Config("industries.toml: no root".into()));
        }
        Ok(Self {
            roots,
            skip_domains: domain_set("skip domain", file.skip_domains)?,
            mailbox_domains: domain_set("mailbox domain", file.mailbox_domains)?,
        })
    }
}

/// A topic set from names: unknown or no names give the offending name back.
pub(crate) fn topic_set(names: &[String]) -> Result<TopicSet, String> {
    if names.is_empty() {
        return Err("list is empty".into());
    }
    let mut set = TopicSet::EMPTY;
    for name in names {
        set.insert(Topic::from_name(name).ok_or_else(|| name.clone())?);
    }
    Ok(set)
}

fn values(qids: impl IntoIterator<Item = u64>) -> String {
    qids.into_iter()
        .map(|q| format!("wd:Q{q}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The query for the English labels of `qids`.
pub(crate) fn label_query(qids: &BTreeSet<u64>) -> String {
    format!(
        "SELECT ?item ?l WHERE {{ VALUES ?item {{ {} }} ?item rdfs:label ?l FILTER(lang(?l) = \"en\") }}",
        values(qids.iter().copied())
    )
}

/// The query for `root` and every class below it.
pub(crate) fn subclass_query(root: u64) -> String {
    format!("SELECT ?c WHERE {{ ?c wdt:P279* wd:Q{root} }}")
}

/// The query for live organisations tied to `classes` by `pass` and their official websites.
pub(crate) fn org_query(pass: Pass, classes: &[u64]) -> String {
    format!(
        "SELECT ?org ?class ?site WHERE {{ VALUES ?class {{ {} }} ?org wdt:{pass} ?class; wdt:P856 ?site . FILTER NOT EXISTS {{ ?org wdt:P576 [] }} }}",
        values(classes.iter().copied())
    )
}

/// Checks that every root has exactly the English label the file gives.
pub(crate) async fn verify_labels(sparql: &impl Sparql, roots: &[Root]) -> Result<(), Error> {
    let qids: BTreeSet<u64> = roots.iter().map(|r| r.qid).collect();
    let mut labels: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    for binding in sparql.select(&label_query(&qids)).await? {
        if let (Some(item), Some(label)) = (binding.get("item"), binding.get("l"))
            && let Some(id) = qid(&item.value)
        {
            labels.entry(id).or_default().insert(label.value.clone());
        }
    }
    for root in roots {
        match labels.get(&root.qid) {
            Some(found) if found.contains(&root.label) => {}
            Some(found) => {
                return Err(Error::Config(format!(
                    "root Q{} is labelled {:?} on Wikidata, not {:?}",
                    root.qid,
                    found.iter().next().map(String::as_str).unwrap_or_default(),
                    root.label
                )));
            }
            None => {
                return Err(Error::Config(format!(
                    "root Q{} has no English label on Wikidata",
                    root.qid
                )));
            }
        }
    }
    Ok(())
}

/// An organisation's official website and the topics of the classes that brought it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    pub pass: Pass,
    pub org: u64,
    pub site: String,
    pub topics: TopicSet,
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PassStats {
    pub roots: usize,
    pub classes: usize,
    pub batches: usize,
    pub bindings: usize,
    pub organisations: usize,
}

/// Everything fetched, with per-pass statistics and the number of queries sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Fetched {
    pub found: Vec<Found>,
    pub passes: BTreeMap<Pass, PassStats>,
    pub queries: usize,
}

/// Verifies the roots, expands each to its subclasses, then fetches the organisations of
/// both passes in batches of [`BATCH`] classes. One query at a time; any failed query fails
/// the whole fetch.
pub(crate) async fn fetch(sparql: &impl Sparql, industries: &Industries) -> Result<Fetched, Error> {
    verify_labels(sparql, &industries.roots).await?;
    let mut fetched = Fetched {
        queries: 1,
        ..Fetched::default()
    };
    let mut classes: BTreeMap<Pass, BTreeMap<u64, TopicSet>> = BTreeMap::new();
    for root in &industries.roots {
        let bindings = sparql.select(&subclass_query(root.qid)).await?;
        fetched.queries += 1;
        let pass = classes.entry(root.pass).or_default();
        let ids = bindings
            .iter()
            .filter_map(|b| b.get("c").and_then(|t| qid(&t.value)));
        for id in ids.chain([root.qid]) {
            let topics = pass.entry(id).or_default();
            *topics = TopicSet::from_bits(topics.bits() | root.topics.bits());
        }
        fetched.passes.entry(root.pass).or_default().roots += 1;
    }
    for pass in Pass::ALL {
        let Some(map) = classes.get(&pass) else {
            continue;
        };
        let ids: Vec<u64> = map.keys().copied().collect();
        let batches = ids.len().div_ceil(BATCH);
        let mut organisations = BTreeSet::new();
        let mut stats = fetched.passes.remove(&pass).unwrap_or_default();
        stats.classes = ids.len();
        for (index, chunk) in ids.chunks(BATCH).enumerate() {
            if index % 20 == 0 {
                eprintln!("{pass}: batch {}/{batches}", index + 1);
            }
            let bindings = sparql.select(&org_query(pass, chunk)).await?;
            fetched.queries += 1;
            stats.batches += 1;
            stats.bindings += bindings.len();
            for binding in bindings {
                let (Some(org), Some(class), Some(site)) = (
                    binding.get("org").and_then(|t| qid(&t.value)),
                    binding.get("class").and_then(|t| qid(&t.value)),
                    binding.get("site"),
                ) else {
                    continue;
                };
                let Some(topics) = map.get(&class) else {
                    continue;
                };
                organisations.insert(org);
                fetched.found.push(Found {
                    pass,
                    org,
                    site: site.value.clone(),
                    topics: *topics,
                });
            }
        }
        stats.organisations = organisations.len();
        fetched.passes.insert(pass, stats);
    }
    Ok(fetched)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::sparql::{Binding, parse_results};

    pub(crate) const INDUSTRIES: &str = r#"
skip_domains = ["sites.example"]
mailbox_domains = ["mail.example"]

[[root]]
qid = "Q9000001"
label = "test retail"
property = "P452"
topics = ["purchases"]

[[root]]
qid = "Q9000002"
label = "test airline"
property = "P31"
topics = ["travel"]

[[root]]
qid = "Q9000003"
label = "test bank"
property = "P31"
topics = ["finance"]
"#;

    /// Answers each query with the first fixture whose needle the query contains.
    pub(crate) struct Fake {
        pub answers: Vec<(String, &'static [u8])>,
        pub log: Mutex<Vec<String>>,
    }

    impl Fake {
        pub(crate) fn standard() -> Self {
            Self::with(&[
                (
                    "rdfs:label",
                    include_bytes!("../tests/fixtures/labels.json"),
                ),
                (
                    "wd:Q9000001 }",
                    include_bytes!("../tests/fixtures/subclasses-retail.json"),
                ),
                (
                    "wd:Q9000002 }",
                    include_bytes!("../tests/fixtures/subclasses-airline.json"),
                ),
                (
                    "wd:Q9000003 }",
                    include_bytes!("../tests/fixtures/subclasses-bank.json"),
                ),
                (
                    "wdt:P452 ?class",
                    include_bytes!("../tests/fixtures/orgs-p452.json"),
                ),
                (
                    "wdt:P31 ?class",
                    include_bytes!("../tests/fixtures/orgs-p31.json"),
                ),
            ])
        }

        pub(crate) fn with(answers: &[(&str, &'static [u8])]) -> Self {
            Self {
                answers: answers.iter().map(|(n, a)| ((*n).to_owned(), *a)).collect(),
                log: Mutex::new(Vec::new()),
            }
        }
    }

    impl Sparql for Fake {
        async fn select(&self, query: &str) -> Result<Vec<Binding>, Error> {
            self.log.lock().unwrap().push(query.to_owned());
            let (_, body) = self
                .answers
                .iter()
                .find(|(needle, _)| query.contains(needle.as_str()))
                .ok_or_else(|| Error::Sparql(format!("no fixture for {query}")))?;
            parse_results(body)
        }
    }

    #[test]
    fn parses_the_file() {
        let industries = Industries::parse(INDUSTRIES).unwrap();
        assert_eq!(industries.roots.len(), 3);
        assert_eq!(industries.roots[1].pass, Pass::P31);
        assert_eq!(
            industries.roots[0].topics,
            TopicSet::EMPTY.with(Topic::Purchases)
        );
        assert!(industries.skip_domains.contains("sites.example"));
        assert!(industries.mailbox_domains.contains("mail.example"));
    }

    #[test]
    fn refuses_bad_files() {
        let root = |qid: &str, property: &str, topics: &str| {
            format!(
                "[[root]]\nqid = \"{qid}\"\nlabel = \"x\"\nproperty = \"{property}\"\ntopics = {topics}\n"
            )
        };
        for bad in [
            String::new(),
            root("P1", "P452", "[\"travel\"]"),
            root("Q01", "P452", "[\"travel\"]"),
            root("Q1", "P279", "[\"travel\"]"),
            root("Q1", "P31", "[]"),
            root("Q1", "P31", "[\"weather\"]"),
            format!(
                "{}{}",
                root("Q1", "P31", "[\"travel\"]"),
                root("Q1", "P31", "[\"jobs\"]")
            ),
            format!(
                "skip_domains = [\"Example.com\"]\n{}",
                root("Q1", "P31", "[\"travel\"]")
            ),
            format!(
                "mailbox_domains = [\"mail.example\", \"mail.example\"]\n{}",
                root("Q1", "P31", "[\"travel\"]")
            ),
            format!(
                "mailbox_domains = [\"-mail.example\"]\n{}",
                root("Q1", "P31", "[\"travel\"]")
            ),
            format!("other = 1\n{}", root("Q1", "P31", "[\"travel\"]")),
        ] {
            assert!(Industries::parse(&bad).is_err(), "{bad}");
        }
        assert!(
            Industries::parse(&format!(
                "{}{}",
                root("Q1", "P31", "[\"travel\"]"),
                root("Q1", "P452", "[\"travel\"]")
            ))
            .is_ok()
        );
    }

    #[test]
    fn queries() {
        assert_eq!(
            subclass_query(9_000_001),
            "SELECT ?c WHERE { ?c wdt:P279* wd:Q9000001 }"
        );
        let q = org_query(Pass::P31, &[1, 2]);
        assert!(q.contains("VALUES ?class { wd:Q1 wd:Q2 }"));
        assert!(q.contains("?org wdt:P31 ?class; wdt:P856 ?site"));
        assert!(q.contains("FILTER NOT EXISTS { ?org wdt:P576 [] }"));
        assert!(label_query(&BTreeSet::from([5, 3])).contains("VALUES ?item { wd:Q3 wd:Q5 }"));
        assert!((20..=50).contains(&BATCH));
    }

    #[tokio::test]
    async fn verifies_labels() {
        let industries = Industries::parse(INDUSTRIES).unwrap();
        let fake = Fake::standard();
        verify_labels(&fake, &industries.roots).await.unwrap();

        let mut renamed = industries.roots.clone();
        renamed[2].label = "test insurer".into();
        let err = verify_labels(&fake, &renamed).await.unwrap_err();
        assert!(err.to_string().contains("Q9000003"), "{err}");

        let mut unknown = industries.roots;
        unknown[0].qid = 9_999_999;
        assert!(verify_labels(&fake, &unknown).await.is_err());
    }

    #[tokio::test]
    async fn expands_and_fetches_both_passes() {
        let industries = Industries::parse(INDUSTRIES).unwrap();
        let fake = Fake::standard();
        let fetched = fetch(&fake, &industries).await.unwrap();
        let log = fake.log.lock().unwrap().clone();
        assert!(log[0].contains("rdfs:label"));
        assert_eq!(log.iter().filter(|q| q.contains("wdt:P279*")).count(), 3);
        let p452 = log.iter().find(|q| q.contains("wdt:P452 ?class")).unwrap();
        assert!(p452.contains("VALUES ?class { wd:Q9000001 wd:Q9000011 wd:Q9000012 }"));
        let p31 = log.iter().find(|q| q.contains("wdt:P31 ?class")).unwrap();
        assert!(p31.contains("VALUES ?class { wd:Q9000002 wd:Q9000003 wd:Q9000031 }"));
        assert_eq!(fetched.queries, log.len());

        let stats = &fetched.passes[&Pass::P452];
        assert_eq!((stats.roots, stats.classes, stats.batches), (1, 3, 1));
        assert_eq!(fetched.passes[&Pass::P31].classes, 3);
        let bank = fetched.found.iter().find(|f| f.org == 9_200_002).unwrap();
        assert_eq!(bank.pass, Pass::P31);
        assert_eq!(bank.topics, TopicSet::EMPTY.with(Topic::Finance));
        assert!(fetched.found.iter().all(|f| f.org != 9_100_099));
    }

    #[tokio::test]
    async fn batches_of_thirty() {
        let mut bindings = String::new();
        for n in 0..65 {
            if n > 0 {
                bindings.push(',');
            }
            bindings.push_str(&format!(
                r#"{{"c":{{"type":"uri","value":"http://www.wikidata.org/entity/Q{}"}}}}"#,
                9_300_000 + n
            ));
        }
        let many: &'static [u8] =
            format!(r#"{{"head":{{"vars":["c"]}},"results":{{"bindings":[{bindings}]}}}}"#)
                .into_bytes()
                .leak();
        let empty: &'static [u8] =
            br#"{"head":{"vars":["org","class","site"]},"results":{"bindings":[]}}"#;
        let fake = Fake::with(&[
            (
                "rdfs:label",
                include_bytes!("../tests/fixtures/labels.json"),
            ),
            ("wdt:P279*", many),
            ("?class", empty),
        ]);
        let industries = Industries::parse(
            "[[root]]\nqid = \"Q9000001\"\nlabel = \"test retail\"\nproperty = \"P452\"\ntopics = [\"purchases\"]\n",
        )
        .unwrap();
        let fetched = fetch(&fake, &industries).await.unwrap();
        let stats = &fetched.passes[&Pass::P452];
        assert_eq!((stats.classes, stats.batches), (66, 3));
        let log = fake.log.lock().unwrap();
        let sizes: Vec<usize> = log
            .iter()
            .filter(|q| q.contains("?class"))
            .map(|q| q.matches("wd:Q").count())
            .collect();
        assert_eq!(sizes, [30, 30, 6]);
    }
}
