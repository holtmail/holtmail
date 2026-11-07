use std::collections::BTreeMap;

use holtmail_category::{Topic, TopicSet};
use holtmail_category_data::{Row, is_valid_domain};
use serde::Deserialize;

use crate::Error;
use crate::industries::topic_set;

/// More unreviewed sensitive rows than this in one build fail it, outside a first release.
pub(crate) const MAX_UNREVIEWED: usize = 50;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, rename = "domain")]
    domains: Vec<RawDomain>,
}

/// One `[[domain]]` entry as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDomain {
    name: String,
    topics: Vec<String>,
}

/// A `[[domain]] name topics` list (`golden.toml`, `sensitive-reviewed.toml`): valid keys,
/// known topics, each domain once.
pub(crate) fn domain_list(file: &str, text: &str) -> Result<BTreeMap<String, TopicSet>, Error> {
    let parsed: File =
        toml::from_str(text).map_err(|e| Error::Config(format!("{file}: {}", e.message())))?;
    domain_entries(file, parsed.domains)
}

/// Checks `[[domain]]` entries: valid keys, known topics, each domain once.
pub(crate) fn domain_entries(
    file: &str,
    entries: Vec<RawDomain>,
) -> Result<BTreeMap<String, TopicSet>, Error> {
    let mut list = BTreeMap::new();
    for raw in entries {
        if !is_valid_domain(&raw.name) {
            return Err(Error::Config(format!(
                "{file}: {} is not a valid key",
                raw.name
            )));
        }
        let topics = topic_set(&raw.topics)
            .map_err(|t| Error::Config(format!("{file}: {}: topic {t}", raw.name)))?;
        if list.insert(raw.name.clone(), topics).is_some() {
            return Err(Error::Config(format!(
                "{file}: {} is listed twice",
                raw.name
            )));
        }
    }
    Ok(list)
}

/// `sensitive-reviewed.toml`: only sensitive topics may be listed.
pub(crate) fn reviewed_list(text: &str) -> Result<BTreeMap<String, TopicSet>, Error> {
    let list = domain_list("sensitive-reviewed.toml", text)?;
    for (name, topics) in &list {
        if topics.iter().any(|t| !t.is_sensitive()) {
            return Err(Error::Config(format!(
                "sensitive-reviewed.toml: {name} lists a topic that is not sensitive"
            )));
        }
    }
    Ok(list)
}

/// A row whose sensitive topics were not all reviewed: the domain and those topics.
pub(crate) type Unreviewed = (String, TopicSet);

/// The rows after the gate, and what it took out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Gated {
    pub rows: Vec<Row>,
    pub unreviewed: Vec<Unreviewed>,
    pub dropped: usize,
}

fn unreviewed_topics(
    row: &Row,
    previous: &BTreeMap<String, TopicSet>,
    reviewed: &BTreeMap<String, TopicSet>,
) -> TopicSet {
    let allowed = |topic: Topic| {
        previous.get(&row.domain).is_some_and(|t| t.contains(topic))
            || reviewed.get(&row.domain).is_some_and(|t| t.contains(topic))
    };
    row.topics
        .iter()
        .filter(|t| t.is_sensitive() && !allowed(*t))
        .collect()
}

/// Removes every sensitive topic (finance, health, government) that neither the previous
/// release nor the reviewed list gives the same domain. A row left with no topic is
/// dropped. Order is kept; the caller decides whether the count fails the build.
pub(crate) fn apply(
    rows: Vec<Row>,
    previous: &BTreeMap<String, TopicSet>,
    reviewed: &BTreeMap<String, TopicSet>,
) -> Gated {
    let mut gated = Gated::default();
    for mut row in rows {
        let strip = unreviewed_topics(&row, previous, reviewed);
        if !strip.is_empty() {
            gated.unreviewed.push((row.domain.clone(), strip));
            row.topics = TopicSet::from_bits(row.topics.bits() & !strip.bits());
        }
        if row.topics.is_empty() {
            gated.dropped += 1;
        } else {
            gated.rows.push(row);
        }
    }
    gated
}

/// The rows of an existing file that carry a sensitive topic the gate would not allow.
pub(crate) fn audit(
    rows: &[Row],
    previous: &BTreeMap<String, TopicSet>,
    reviewed: &BTreeMap<String, TopicSet>,
) -> Vec<Unreviewed> {
    rows.iter()
        .filter_map(|row| {
            let strip = unreviewed_topics(row, previous, reviewed);
            (!strip.is_empty()).then(|| (row.domain.clone(), strip))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(domain: &str, topics: &[Topic]) -> Row {
        Row {
            domain: domain.into(),
            topics: topics.iter().copied().collect(),
            source: "wikidata:Q9000001".into(),
        }
    }

    fn set(topics: &[Topic]) -> TopicSet {
        topics.iter().copied().collect()
    }

    #[test]
    fn previous_release_and_reviewed_list_keep_topics() {
        let rows = vec![
            row("bank.example", &[Topic::Finance]),
            row("clinic.example", &[Topic::Health, Topic::Purchases]),
            row("example.com", &[Topic::Travel]),
            row("example.org", &[Topic::Government]),
            row("tax.example", &[Topic::Government, Topic::Finance]),
        ];
        let previous = BTreeMap::from([
            ("bank.example".to_owned(), set(&[Topic::Finance])),
            ("tax.example".to_owned(), set(&[Topic::Finance])),
        ]);
        let reviewed = BTreeMap::from([("example.org".to_owned(), set(&[Topic::Government]))]);
        let gated = apply(rows, &previous, &reviewed);
        assert_eq!(
            gated.rows,
            [
                row("bank.example", &[Topic::Finance]),
                row("clinic.example", &[Topic::Purchases]),
                row("example.com", &[Topic::Travel]),
                row("example.org", &[Topic::Government]),
                row("tax.example", &[Topic::Finance]),
            ]
        );
        assert_eq!(
            gated.unreviewed,
            [
                ("clinic.example".to_owned(), set(&[Topic::Health])),
                ("tax.example".to_owned(), set(&[Topic::Government])),
            ]
        );
        assert_eq!(gated.dropped, 0);
    }

    #[test]
    fn strips_and_drops_empty_rows() {
        let gated = apply(
            vec![row("bank.example", &[Topic::Finance, Topic::Health])],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(gated.rows.is_empty());
        assert_eq!(gated.dropped, 1);
        assert_eq!(gated.unreviewed.len(), 1);
    }

    #[test]
    fn counts_every_unreviewed_row() {
        let rows: Vec<Row> = (0..=MAX_UNREVIEWED)
            .map(|n| row(&format!("bank{n:03}.example"), &[Topic::Finance]))
            .collect();
        let gated = apply(rows.clone(), &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(gated.unreviewed.len(), MAX_UNREVIEWED + 1);
        assert_eq!(
            audit(&rows, &BTreeMap::new(), &BTreeMap::new()).len(),
            MAX_UNREVIEWED + 1
        );
        let reviewed = BTreeMap::from([("bank000.example".to_owned(), set(&[Topic::Finance]))]);
        assert_eq!(
            audit(&rows, &BTreeMap::new(), &reviewed).len(),
            MAX_UNREVIEWED
        );
    }

    #[test]
    fn lists() {
        let ok = "[[domain]]\nname = \"bank.example\"\ntopics = [\"finance\"]\n";
        assert_eq!(
            reviewed_list(ok).unwrap()["bank.example"],
            set(&[Topic::Finance])
        );
        assert!(reviewed_list("").unwrap().is_empty());
        assert!(
            reviewed_list("[[domain]]\nname = \"shop.example\"\ntopics = [\"purchases\"]\n")
                .is_err()
        );
        for bad in [
            "[[domain]]\nname = \"Bank.example\"\ntopics = [\"finance\"]\n",
            "[[domain]]\nname = \"bank.example\"\ntopics = []\n",
            "[[domain]]\nname = \"bank.example\"\ntopics = [\"loans\"]\n",
            &format!("{ok}{ok}"),
            "[[domain]]\nname = \"bank.example\"\n",
        ] {
            assert!(domain_list("x", bad).is_err(), "{bad}");
        }
    }
}
