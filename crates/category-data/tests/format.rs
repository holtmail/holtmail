//! Format tests on invented `example.com` and `example.org` domains.

use std::io::Write;

use flate2::{Compression, write::GzEncoder};
use holtmail_category::{Topic, TopicSet};
use holtmail_category_data::{
    DataError, Header, Limits, MAX_SOURCE_BYTES, Row, parse, parse_with, read, write,
};
use proptest::prelude::*;

fn gz(text: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(text).unwrap();
    encoder.finish().unwrap()
}

fn header() -> Header {
    Header::new("wikidata", "2026.11.07")
}

fn row(domain: &str, topics: TopicSet) -> Row {
    Row {
        domain: domain.into(),
        topics,
        source: "wikidata:Q1".into(),
    }
}

fn line_error(text: &str) -> DataError {
    parse(&gz(text.as_bytes())).unwrap_err()
}

const HEAD: &str = "#holtmail-categories 1 wikidata 1\n";

#[test]
fn round_trip_and_lookup() {
    let rows = vec![
        row("bank.example.org", TopicSet::from(Topic::Finance)),
        row(
            "example.com",
            TopicSet::from(Topic::Purchases).with(Topic::Travel),
        ),
        row("shop.example.com", TopicSet::from(Topic::Jobs)),
    ];
    let bytes = write(&header(), &rows).unwrap();
    let table = parse(&bytes).unwrap();
    assert_eq!(table.header(), &header());
    assert_eq!(table.name(), "wikidata");
    assert_eq!(table.version(), "2026.11.07");
    assert_eq!(table.len(), 3);
    assert_eq!(table.skipped(), 0);
    assert_eq!(
        table.get("example.com"),
        Some(TopicSet::from(Topic::Purchases).with(Topic::Travel))
    );
    assert_eq!(table.get("Example.com"), None);
    assert_eq!(table.get("other.example.com"), None);
    assert_eq!(
        table.lookup(Some("shop.example.com"), Some("example.com")),
        Some(TopicSet::from(Topic::Jobs))
    );
    assert_eq!(
        table.lookup(Some("mail.example.com"), Some("example.com")),
        table.get("example.com")
    );
    assert_eq!(
        table.lookup(Some("SHOP.Example.COM"), None),
        Some(TopicSet::from(Topic::Jobs))
    );
    assert_eq!(
        table.lookup(None, Some("Bank.Example.Org")),
        Some(TopicSet::from(Topic::Finance))
    );
    assert_eq!(table.lookup(Some("example.net"), None), None);
    assert_eq!(table.lookup(None, None), None);

    let (read_header, read_rows) = read(&bytes, Limits::default()).unwrap();
    assert_eq!(read_header, header());
    assert_eq!(read_rows, rows);
}

#[test]
fn written_text_is_the_documented_format() {
    let rows = vec![row(
        "example.com",
        TopicSet::from(Topic::Travel).with(Topic::Purchases),
    )];
    let bytes = write(&header(), &rows).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut text)
        .unwrap();
    assert_eq!(
        text,
        "#holtmail-categories 1 wikidata 2026.11.07\nexample.com\tpurchases,travel\twikidata:Q1\n"
    );
}

#[test]
fn deterministic_bytes() {
    let rows = vec![row("example.com", TopicSet::from(Topic::Events))];
    let first = write(&header(), &rows).unwrap();
    assert_eq!(first, write(&header(), &rows).unwrap());
    assert_eq!(&first[4..8], &[0, 0, 0, 0]);
    assert_eq!(first[3] & 0x08, 0);
}

#[test]
fn hash_follows_content_not_compression() {
    let text = format!("{HEAD}example.com\tjobs\twikidata:Q2\n");
    let a = parse(&gz(text.as_bytes())).unwrap();
    let mut best = GzEncoder::new(Vec::new(), Compression::best());
    best.write_all(text.as_bytes()).unwrap();
    let b = parse(&best.finish().unwrap()).unwrap();
    assert_eq!(a.hash(), b.hash());
    assert_eq!(a.hash().len(), 64);
    let other = parse(&gz(
        format!("{HEAD}example.com\tjobs\twikidata:Q3\n").as_bytes()
    ))
    .unwrap();
    assert_ne!(a.hash(), other.hash());
}

#[test]
fn header_only_is_an_empty_table() {
    let table = parse(&gz(HEAD.as_bytes())).unwrap();
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);
    assert_eq!(table.lookup(Some("example.com"), Some("example.com")), None);
    let written = write(&Header::new("wikidata", "0"), &[]).unwrap();
    assert!(parse(&written).unwrap().is_empty());
}

#[test]
fn gzip_bomb_is_refused() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..40 {
        encoder.write_all(&zeros).unwrap();
    }
    let bomb = encoder.finish().unwrap();
    assert!(bomb.len() < 1024 * 1024);
    assert_eq!(parse(&bomb).unwrap_err(), DataError::TooLarge);
    let small = Limits {
        max_bytes: 16,
        max_rows: 10,
    };
    assert_eq!(
        parse_with(&gz(HEAD.as_bytes()), small).unwrap_err(),
        DataError::TooLarge
    );
}

#[test]
fn not_gzip() {
    assert_eq!(parse(b"").unwrap_err(), DataError::Gzip);
    assert_eq!(parse(HEAD.as_bytes()).unwrap_err(), DataError::Gzip);
    let whole = gz(format!("{HEAD}example.com\tjobs\tx\n").as_bytes());
    assert_eq!(
        parse(&whole[..whole.len() - 4]).unwrap_err(),
        DataError::Gzip
    );
}

#[test]
fn too_many_rows() {
    let text =
        format!("{HEAD}a.example.com\tjobs\tx\nb.example.com\tjobs\tx\nc.example.com\tjobs\tx\n");
    let limits = Limits {
        max_bytes: 1024,
        max_rows: 2,
    };
    assert_eq!(
        parse_with(&gz(text.as_bytes()), limits).unwrap_err(),
        DataError::TooManyRows
    );
    assert_eq!(
        read(&gz(text.as_bytes()), limits).unwrap_err(),
        DataError::TooManyRows
    );
    let limits = Limits {
        max_bytes: 1024,
        max_rows: 3,
    };
    assert_eq!(parse_with(&gz(text.as_bytes()), limits).unwrap().len(), 3);
}

#[test]
fn line_rules() {
    let cases: &[(&str, usize, &str)] = &[
        (
            "b.example.com\tjobs\tx\na.example.com\tjobs\tx\n",
            3,
            "order",
        ),
        (
            "a.example.com\tjobs\tx\na.example.com\tsocial\tx\n",
            3,
            "duplicate",
        ),
        ("Example.com\tjobs\tx\n", 2, "domain"),
        ("example.com.\tjobs\tx\n", 2, "domain"),
        ("com\tjobs\tx\n", 2, "domain"),
        ("example.com\tjobs\ta\tb\n", 2, "fields"),
        ("example.com\tjobs\n", 2, "fields"),
        ("example.com\t\tx\n", 2, "topics"),
        ("example.com\tjobs,\tx\n", 2, "topics"),
        ("example.com\tJobs\tx\n", 2, "topics"),
        ("example.com\tjobs\t\n", 2, "source"),
        ("example.com\tjobs\tx\r\n", 2, "source"),
        ("example.com\tjobs\tx\n\n", 3, "empty"),
        ("example.com\tjobs\tx", 2, "newline"),
    ];
    for (rows, line, rule) in cases {
        assert_eq!(
            line_error(&format!("{HEAD}{rows}")),
            DataError::Line { line: *line, rule },
            "{rows:?}"
        );
    }
    let long = "q".repeat(MAX_SOURCE_BYTES + 1);
    assert_eq!(
        line_error(&format!("{HEAD}example.com\tjobs\t{long}\n")),
        DataError::Line {
            line: 2,
            rule: "source"
        }
    );
}

#[test]
fn invalid_utf8() {
    let mut text = HEAD.as_bytes().to_vec();
    text.extend_from_slice(b"example.com\tjobs\twikidata:\xff\n");
    assert_eq!(
        parse(&gz(&text)).unwrap_err(),
        DataError::Line {
            line: 2,
            rule: "utf8"
        }
    );
    assert_eq!(
        parse(&gz(b"#holtmail-categories 1 \xff 1\n")).unwrap_err(),
        DataError::Header("utf8")
    );
}

#[test]
fn header_rules() {
    let cases: &[(&str, &str)] = &[
        ("", "missing"),
        ("#holtmail-categories 1 wikidata 1", "newline"),
        ("#other 1 wikidata 1\n", "magic"),
        ("example.com\tjobs\tx\n", "magic"),
        ("#holtmail-categories 1 wikidata\n", "fields"),
        ("#holtmail-categories 1 wikidata 1 extra\n", "fields"),
        ("#holtmail-categories 2 wikidata 1\n", "format"),
        ("#holtmail-categories 01 wikidata 1\n", "format"),
        ("#holtmail-categories 1 Wikidata 1\n", "name"),
        ("#holtmail-categories 1 wikidata 1/2\n", "version"),
    ];
    for (text, rule) in cases {
        assert_eq!(
            parse(&gz(text.as_bytes())).unwrap_err(),
            DataError::Header(rule),
            "{text:?}"
        );
    }
}

#[test]
fn unknown_topics_are_skipped_and_counted() {
    let text = format!(
        "{HEAD}a.example.com\tjobs,weather\tx\nb.example.com\tweather\tx\nc.example.org\tsocial\tx\n"
    );
    let table = parse(&gz(text.as_bytes())).unwrap();
    assert_eq!(table.len(), 2);
    assert_eq!(table.skipped(), 1);
    assert_eq!(
        table.get("a.example.com"),
        Some(TopicSet::from(Topic::Jobs))
    );
    assert_eq!(table.get("b.example.com"), None);
    let (_, rows) = read(&gz(text.as_bytes()), Limits::default()).unwrap();
    assert_eq!(rows.len(), 2);
}

#[test]
fn write_refuses_what_parse_refuses() {
    let fail = |rows: &[Row]| write(&header(), rows).unwrap_err();
    let jobs = TopicSet::from(Topic::Jobs);
    assert_eq!(
        fail(&[row("example.com", TopicSet::EMPTY)]),
        DataError::Line {
            line: 2,
            rule: "topics"
        }
    );
    assert_eq!(
        fail(&[row("example.com", TopicSet::from_bits(1 << 40))]),
        DataError::Line {
            line: 2,
            rule: "topics"
        }
    );
    assert_eq!(
        fail(&[row("b.example.com", jobs), row("a.example.com", jobs)]),
        DataError::Line {
            line: 3,
            rule: "order"
        }
    );
    assert_eq!(
        fail(&[row("example.com", jobs), row("example.com", jobs)]),
        DataError::Line {
            line: 3,
            rule: "duplicate"
        }
    );
    assert_eq!(
        fail(&[row("Example.com", jobs)]),
        DataError::Line {
            line: 2,
            rule: "domain"
        }
    );
    let mut tab = row("example.com", jobs);
    tab.source = "wikidata\tQ1".into();
    assert_eq!(
        fail(&[tab]),
        DataError::Line {
            line: 2,
            rule: "source"
        }
    );
    assert_eq!(
        write(&Header::new("wiki data", "1"), &[]).unwrap_err(),
        DataError::Header("name")
    );
    assert_eq!(
        write(&Header::new("wikidata", ""), &[]).unwrap_err(),
        DataError::Header("version")
    );
}

#[test]
fn errors_never_show_row_text() {
    let err = line_error(&format!("{HEAD}secret.example.com\tjobs\t\n"));
    assert!(!err.to_string().contains("secret"));
    assert_eq!(err.to_string(), "category data line 2: source");
}

fn label() -> impl Strategy<Value = String> {
    "[a-z0-9]([a-z0-9-]{0,8}[a-z0-9])?"
}

fn topics() -> impl Strategy<Value = TopicSet> {
    (1u64..256).prop_map(TopicSet::from_bits)
}

proptest! {
    #[test]
    fn write_then_parse_round_trips(
        entries in proptest::collection::btree_map(
            (proptest::collection::vec(label(), 0..3), prop_oneof![Just("example.com"), Just("example.org")])
                .prop_map(|(labels, base)| {
                    labels.into_iter().chain([base.to_string()]).collect::<Vec<_>>().join(".")
                }),
            (topics(), "wikidata:Q[1-9][0-9]{0,8}"),
            0..40,
        ),
        version in "[A-Za-z0-9.-]{1,32}",
    ) {
        let rows: Vec<Row> = entries
            .into_iter()
            .map(|(domain, (topics, source))| Row { domain, topics, source })
            .collect();
        let head = Header::new("wikidata", version);
        let bytes = write(&head, &rows).unwrap();
        prop_assert_eq!(&bytes, &write(&head, &rows).unwrap());
        let table = parse(&bytes).unwrap();
        prop_assert_eq!(table.len(), rows.len());
        prop_assert_eq!(table.skipped(), 0);
        for row in &rows {
            prop_assert_eq!(table.get(&row.domain), Some(row.topics));
            prop_assert_eq!(table.lookup(Some(&row.domain.to_ascii_uppercase()), None), Some(row.topics));
        }
        let (read_head, read_rows) = read(&bytes, Limits::default()).unwrap();
        prop_assert_eq!(read_head, head);
        prop_assert_eq!(read_rows, rows);
    }

    #[test]
    fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = parse(&bytes);
        let _ = parse(&gz(&bytes));
    }
}
