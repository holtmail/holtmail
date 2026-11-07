//! The shared categorizer conformance suite.

use holtmail_category::testing::conformance;
use holtmail_category::{CategoryInput, CategorySource, CategoryVote, Topic, TopicSet, TrustState};
use holtmail_category_data::{DomainTable, Header, Row};
use holtmail_category_domains::DomainsCategorizer;

fn table(name: &str, rows: &[(&str, &[Topic])]) -> DomainTable {
    let rows: Vec<Row> = rows
        .iter()
        .map(|(domain, topics)| Row {
            domain: (*domain).into(),
            topics: topics.iter().copied().collect(),
            source: format!("{name}:test"),
        })
        .collect();
    let gz = holtmail_category_data::write(&Header::new(name, "1"), &rows).unwrap();
    holtmail_category_data::parse(&gz).unwrap()
}

fn input(address: Option<&str>, domain: Option<&str>) -> CategoryInput {
    CategoryInput {
        signals: Default::default(),
        kinds: Default::default(),
        reservations: Default::default(),
        structured: false,
        forwarded: false,
        own: false,
        trust: TrustState::Unknown,
        sender_address: address.map(Into::into),
        sender_domain: domain.map(Into::into),
    }
}

fn vote(topics: &[Topic]) -> Option<CategoryVote> {
    Some(CategoryVote {
        kind: None,
        kind_source: CategorySource::Domain,
        topics: topics.iter().copied().collect::<TopicSet>(),
        source: CategorySource::Domain,
    })
}

#[test]
fn keeps_the_category_contract_on_invented_tables() {
    let domains = DomainsCategorizer::new(vec![
        table(
            "wikidata",
            &[
                ("bank.example.org", &[Topic::Finance]),
                ("example.com", &[Topic::Purchases]),
            ],
        ),
        table("extra", &[("example.org", &[Topic::Social])]),
    ])
    .unwrap();
    let cases = vec![
        (
            input(Some("news@example.com"), Some("example.com")),
            vote(&[Topic::Purchases]),
        ),
        (
            input(Some("news@shop.example.com"), Some("example.com")),
            vote(&[Topic::Purchases]),
        ),
        (
            input(Some("alerts@bank.example.org"), Some("example.org")),
            vote(&[Topic::Finance, Topic::Social]),
        ),
        (
            input(Some("ada@mail.example.org"), Some("example.org")),
            vote(&[Topic::Social]),
        ),
        (
            input(Some("ADA@Example.COM"), Some("Example.com")),
            vote(&[Topic::Purchases]),
        ),
        (input(None, None), None),
        (input(Some("ada@example.net"), Some("example.net")), None),
        (
            CategoryInput {
                own: true,
                ..input(Some("ada@example.com"), Some("example.com"))
            },
            None,
        ),
        (
            CategoryInput {
                forwarded: true,
                ..input(Some("ada@example.com"), Some("example.com"))
            },
            None,
        ),
    ];
    conformance::categorizer(&domains, &cases);
}

#[test]
fn keeps_the_category_contract_on_the_bundled_table() {
    conformance::categorizer(&DomainsCategorizer::with_bundled().unwrap(), &[]);
}
