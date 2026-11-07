use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use holtmail_category::{
    Categorizer, CategoryError, CategoryInput, CategorySnapshot, Topic, TopicSet, TrustState,
};
use holtmail_category_data::{DomainTable, Header, Row};

use super::*;

fn table(name: &str, version: &str, rows: &[(&str, &[Topic])]) -> DomainTable {
    let rows: Vec<Row> = rows
        .iter()
        .map(|(domain, topics)| Row {
            domain: (*domain).into(),
            topics: topics.iter().copied().collect(),
            source: "test".into(),
        })
        .collect();
    let gz = holtmail_category_data::write(&Header::new(name, version), &rows).unwrap();
    holtmail_category_data::parse(&gz).unwrap()
}

fn input(address: &str, domain: &str) -> CategoryInput {
    CategoryInput {
        signals: Default::default(),
        kinds: Default::default(),
        reservations: Default::default(),
        structured: false,
        forwarded: false,
        own: false,
        trust: TrustState::Unknown,
        sender_address: Some(address.into()),
        sender_domain: Some(domain.into()),
    }
}

fn topics(snapshot: &dyn CategorySnapshot, address: &str, domain: &str) -> Option<TopicSet> {
    snapshot
        .categorize(&input(address, domain))
        .map(|vote| vote.topics)
}

fn set(topics: &[Topic]) -> TopicSet {
    topics.iter().copied().collect()
}

#[test]
fn reload_changes_new_snapshots_only() {
    let domains = DomainsCategorizer::new(vec![table(
        "wikidata",
        "1",
        &[("example.com", &[Topic::Purchases])],
    )])
    .unwrap();
    let before = domains.snapshot();
    domains
        .reload(vec![table(
            "wikidata",
            "2",
            &[("example.com", &[Topic::Travel])],
        )])
        .unwrap();
    let after = domains.snapshot();
    assert_ne!(before.version(), after.version());
    assert_eq!(
        topics(before.as_ref(), "a@example.com", "example.com"),
        Some(set(&[Topic::Purchases]))
    );
    assert_eq!(
        topics(after.as_ref(), "a@example.com", "example.com"),
        Some(set(&[Topic::Travel]))
    );
}

#[test]
fn version_follows_content_not_only_header() {
    let a = DomainsCategorizer::new(vec![table(
        "wikidata",
        "1",
        &[("example.com", &[Topic::Purchases])],
    )])
    .unwrap();
    let b = DomainsCategorizer::new(vec![table(
        "wikidata",
        "1",
        &[("example.com", &[Topic::Social])],
    )])
    .unwrap();
    let version = a.snapshot().version();
    assert!(version.starts_with("domains-2:wikidata@1#"), "{version}");
    assert_ne!(version, b.snapshot().version());
    assert_eq!(
        DomainsCategorizer::new(Vec::new())
            .unwrap()
            .snapshot()
            .version(),
        "domains-2:none"
    );
}

#[test]
fn duplicate_names_are_refused_and_keep_the_old_set() {
    let one = || table("wikidata", "1", &[("example.com", &[Topic::Purchases])]);
    assert_eq!(
        DomainsCategorizer::new(vec![one(), one()]).unwrap_err(),
        CategoryError::Data("duplicate table name")
    );
    let domains = DomainsCategorizer::new(vec![one()]).unwrap();
    let version = domains.snapshot().version();
    assert!(domains.reload(vec![one(), one()]).is_err());
    let snapshot = domains.snapshot();
    assert_eq!(snapshot.version(), version);
    assert_eq!(
        topics(snapshot.as_ref(), "a@example.com", "example.com"),
        Some(set(&[Topic::Purchases]))
    );
}

#[test]
fn snapshots_are_never_torn_by_a_concurrent_reload() {
    let a = || {
        vec![
            table("extra", "1", &[("example.org", &[Topic::Jobs])]),
            table("wikidata", "1", &[("example.com", &[Topic::Purchases])]),
        ]
    };
    let b = || {
        vec![
            table("extra", "2", &[("example.org", &[Topic::Events])]),
            table("wikidata", "2", &[("example.com", &[Topic::Travel])]),
        ]
    };
    let domains = Arc::new(DomainsCategorizer::new(a()).unwrap());
    let version_a = domains.snapshot().version();
    domains.reload(b()).unwrap();
    let version_b = domains.snapshot().version();
    let (sets_a, sets_b) = (a(), b());
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let domains = domains.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            let mut flip = false;
            while !stop.load(Ordering::Relaxed) {
                let tables = if flip { sets_a.clone() } else { sets_b.clone() };
                domains.reload(tables).unwrap();
                flip = !flip;
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let domains = domains.clone();
            let (version_a, version_b) = (version_a.clone(), version_b.clone());
            thread::spawn(move || {
                for _ in 0..2_000 {
                    let snapshot = domains.snapshot();
                    let version = snapshot.version();
                    let com = topics(snapshot.as_ref(), "a@example.com", "example.com");
                    let org = topics(snapshot.as_ref(), "a@example.org", "example.org");
                    if version == version_a {
                        assert_eq!(com, Some(set(&[Topic::Purchases])));
                        assert_eq!(org, Some(set(&[Topic::Jobs])));
                    } else {
                        assert_eq!(version, version_b);
                        assert_eq!(com, Some(set(&[Topic::Travel])));
                        assert_eq!(org, Some(set(&[Topic::Events])));
                    }
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
}

#[test]
fn bundled_table_loads() {
    assert!(BUNDLED.len() <= BUNDLED_MAX_BYTES);
    let table = bundled().unwrap();
    assert_eq!(table.name(), "wikidata");
    let domains = DomainsCategorizer::with_bundled().unwrap();
    let info = domains.tables();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].name, "wikidata");
    assert_eq!(info[0].rows, table.len());
    assert_eq!(info[0].hash, table.hash());
}

#[test]
fn tables_are_listed_by_name() {
    let domains = DomainsCategorizer::new(vec![
        table(
            "wikidata",
            "2026.11.07",
            &[("example.com", &[Topic::Purchases])],
        ),
        table(
            "extra",
            "1",
            &[
                ("example.org", &[Topic::Social]),
                ("shop.example.org", &[Topic::Purchases]),
            ],
        ),
    ])
    .unwrap();
    let info = domains.tables();
    let names: Vec<_> = info.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["extra", "wikidata"]);
    assert_eq!(info[0].version, "1");
    assert_eq!(info[0].rows, 2);
    assert_eq!(info[1].version, "2026.11.07");
    assert_eq!(info[1].rows, 1);
    assert_eq!(info[1].hash.len(), 64);
    let version = domains.snapshot().version();
    assert_eq!(
        version,
        format!(
            "domains-2:extra@1#{}:wikidata@2026.11.07#{}",
            info[0].hash, info[1].hash
        )
    );
    assert!(
        DomainsCategorizer::new(Vec::new())
            .unwrap()
            .tables()
            .is_empty()
    );
}

#[test]
fn host_is_taken_after_the_last_at_and_case_is_ignored() {
    let domains = DomainsCategorizer::new(vec![table(
        "wikidata",
        "1",
        &[
            ("bank.example.com", &[Topic::Finance]),
            ("example.com", &[Topic::Purchases]),
        ],
    )])
    .unwrap();
    let snapshot = domains.snapshot();
    assert_eq!(
        topics(snapshot.as_ref(), "\"a@b\"@Bank.Example.COM", "example.com"),
        Some(set(&[Topic::Finance]))
    );
    assert_eq!(
        topics(snapshot.as_ref(), "no-at-sign", "example.com"),
        Some(set(&[Topic::Purchases]))
    );
    let vote = snapshot.categorize(&input("a@example.net", "example.net"));
    assert_eq!(vote, None);
}
