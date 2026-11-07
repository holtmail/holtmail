use std::collections::{BTreeMap, BTreeSet};

use holtmail_category::TopicSet;
use holtmail_category_data::{Row, is_valid_domain};
use serde::Deserialize;

use crate::Error;
use crate::build::listed;
use crate::gate::{RawDomain, domain_entries};

/// A golden domain the file does not carry with exactly its listed topics, or a row the file
/// must not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Miss {
    pub domain: String,
    /// The topics expected; `None` when no row may exist.
    pub expected: Option<TopicSet>,
    pub found: Option<TopicSet>,
}

/// `golden.toml`: stable organisation domains and their known topics, and domains that must
/// have no row, neither themselves nor any subdomain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Golden {
    pub present: BTreeMap<String, TopicSet>,
    pub absent: BTreeSet<String>,
}

impl Golden {
    /// The number of listed domains.
    pub(crate) fn len(&self) -> usize {
        self.present.len() + self.absent.len()
    }

    /// Adds domains that must have no row.
    pub(crate) fn forbid<'a>(&mut self, domains: impl IntoIterator<Item = &'a String>) {
        self.absent.extend(domains.into_iter().cloned());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, rename = "domain")]
    domains: Vec<RawDomain>,
    #[serde(default)]
    absent: Vec<String>,
}

/// Parses `golden.toml`: `[[domain]]` entries and an `absent` list of valid keys, none listed
/// twice or in both.
pub(crate) fn golden_list(text: &str) -> Result<Golden, Error> {
    let raw: File =
        toml::from_str(text).map_err(|e| Error::Config(format!("golden.toml: {}", e.message())))?;
    let present = domain_entries("golden.toml", raw.domains)?;
    let mut absent = BTreeSet::new();
    for name in raw.absent {
        if !is_valid_domain(&name) {
            return Err(Error::Config(format!(
                "golden.toml: {name} is not a valid key"
            )));
        }
        if present.contains_key(&name) || !absent.insert(name.clone()) {
            return Err(Error::Config(format!(
                "golden.toml: {name} is listed twice"
            )));
        }
    }
    Ok(Golden { present, absent })
}

/// Every golden domain missing from `rows` or carrying other topics, then every row under an
/// absent domain, each in domain order.
pub(crate) fn check(rows: &[Row], golden: &Golden) -> Vec<Miss> {
    let present = golden.present.iter().filter_map(|(domain, expected)| {
        let found = rows
            .binary_search_by(|r| r.domain.as_str().cmp(domain))
            .ok()
            .map(|i| rows[i].topics);
        (found != Some(*expected)).then(|| Miss {
            domain: domain.clone(),
            expected: Some(*expected),
            found,
        })
    });
    let absent = rows
        .iter()
        .filter(|r| listed(&r.domain, &golden.absent))
        .map(|r| Miss {
            domain: r.domain.clone(),
            expected: None,
            found: Some(r.topics),
        });
    present.chain(absent).collect()
}

#[cfg(test)]
mod tests {
    use holtmail_category::Topic;

    use super::*;

    fn row(domain: &str, topics: &[Topic]) -> Row {
        Row {
            domain: domain.into(),
            topics: topics.iter().copied().collect(),
            source: "wikidata:Q9000001".into(),
        }
    }

    #[test]
    fn passes_and_fails() {
        let rows = vec![
            row("air.example", &[Topic::Travel]),
            row("example.com", &[Topic::Purchases]),
            row("example.org", &[Topic::Purchases, Topic::Travel]),
        ];
        let golden = golden_list(
            "absent = [\"mail.example\"]\n\n[[domain]]\nname = \"air.example\"\ntopics = [\"travel\"]\n\n[[domain]]\nname = \"example.com\"\ntopics = [\"purchases\"]\n",
        )
        .unwrap();
        assert_eq!(golden.len(), 3);
        assert!(check(&rows, &golden).is_empty());
        assert!(check(&rows, &Golden::default()).is_empty());

        let golden = golden_list(
            "[[domain]]\nname = \"example.org\"\ntopics = [\"purchases\"]\n\n[[domain]]\nname = \"gone.example\"\ntopics = [\"jobs\"]\n",
        )
        .unwrap();
        let misses = check(&rows, &golden);
        assert_eq!(misses.len(), 2);
        assert_eq!(misses[0].domain, "example.org");
        assert_eq!(
            misses[0].found,
            Some(TopicSet::EMPTY.with(Topic::Purchases).with(Topic::Travel))
        );
        assert_eq!(misses[1].found, None);
    }

    #[test]
    fn absent_domains_and_their_subdomains() {
        let rows = vec![
            row("example.net", &[Topic::Travel]),
            row("mail.example", &[Topic::Purchases]),
            row("shop.mail.example", &[Topic::Social]),
        ];
        let mut golden = golden_list("absent = [\"mail.example\"]\n").unwrap();
        let misses = check(&rows, &golden);
        assert_eq!(
            misses
                .iter()
                .map(|m| (m.domain.as_str(), m.expected))
                .collect::<Vec<_>>(),
            [("mail.example", None), ("shop.mail.example", None)]
        );
        golden.forbid(&["example.net".to_owned()]);
        assert_eq!(check(&rows, &golden).len(), 3);
    }

    #[test]
    fn refuses_bad_lists() {
        for bad in [
            "absent = [\"Mail.example\"]\n",
            "absent = [\"mail.example\", \"mail.example\"]\n",
            "absent = [\"air.example\"]\n[[domain]]\nname = \"air.example\"\ntopics = [\"travel\"]\n",
            "absent = \"mail.example\"\n",
            "other = 1\n",
        ] {
            assert!(golden_list(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn shipped_list_parses() {
        let golden = golden_list(include_str!("../data/golden.toml")).unwrap();
        assert!(
            golden
                .present
                .values()
                .all(|t| t.iter().all(|t| !t.is_sensitive()))
        );
        for domain in [
            "yahoo.com",
            "mail.ru",
            "qq.com",
            "naver.com",
            "free.fr",
            "gmail.com",
        ] {
            assert!(golden.absent.contains(domain), "{domain}");
        }
    }
}
