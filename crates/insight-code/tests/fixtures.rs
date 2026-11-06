//! The §15 `.eml` fixtures, read through `holtmail_mime` and the runner's input builder.

use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{Confidence, InsightExtractor, InsightInput, InsightKind, Provenance};
use holtmail_insight_code::CodeExtractor;
use holtmail_insights::Runner;

const FIXTURES: &[(&str, Option<&str>)] = &[
    ("otp-plain-en", Some("482913")),
    ("otp-ru-html-strong", Some("738204")),
    ("otp-split-dash", Some("482913")),
    ("otp-wicg-origin-bound", Some("482913")),
    ("otp-near-order-number", Some("604281")),
    ("otp-price-date-phone", Some("715309")),
    ("otp-in-href-only", None),
    ("otp-in-footer", None),
    ("otp-hex-color", Some("902817")),
    ("otp-newsletter-year", None),
    ("otp-tie-two-codes", None),
    ("otp-zh-no-space", Some("592817")),
    ("otp-ja-fullwidth", Some("876543")),
    ("otp-ko", Some("304918")),
    ("otp-subject-and-body-same", Some("482913")),
    ("otp-backup-codes", None),
    ("otp-promo-code-newsletter", None),
    ("otp-barcode-word", None),
    ("otp-text-html-disagree", Some("638291")),
    ("otp-css-in-style-block", Some("739120")),
    ("fwd-inline-otp", None),
];

fn input(name: &str) -> InsightInput {
    let path = format!("{}/tests/fixtures/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing"));
    let message = holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{name} does not parse"));
    Runner::input(&message).0
}

#[test]
fn every_fixture_gives_its_code() {
    let extractor = CodeExtractor::new();
    for (name, expected) in FIXTURES {
        let items = extractor.extract(&input(name));
        let codes: Vec<(&str, Confidence)> = items
            .iter()
            .filter_map(|item| match &item.kind {
                InsightKind::Code { value, .. } => Some((value.as_str(), item.confidence)),
                _ => None,
            })
            .collect();
        match expected {
            Some(value) => {
                assert_eq!(codes.len(), 1, "{name}: {codes:?}");
                assert_eq!(codes[0].0, *value, "{name}");
                assert!(codes[0].1 >= Confidence::Likely, "{name}: {:?}", codes[0].1);
            }
            None => assert!(codes.is_empty(), "{name}: {codes:?}"),
        }
    }
}

#[test]
fn origin_bound_fixture_is_certain() {
    let items = CodeExtractor::new().extract(&input("otp-wicg-origin-bound"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].provenance, Provenance::OriginBound);
    assert_eq!(items[0].confidence, Confidence::Certain);
    assert_eq!(
        items[0].kind,
        InsightKind::Code {
            value: "482913".into(),
            origin_bound: Some("example.com".into()),
        }
    );
}

#[test]
fn explanations_name_rules_and_never_values() {
    let extractor = CodeExtractor::new();
    let steps = extractor.explain(&input("otp-near-order-number"));
    let rules: Vec<&str> = steps.iter().map(|s| s.rule).collect();
    assert!(rules.contains(&"reference_prefix"), "{rules:?}");
    assert!(rules.contains(&"outcome:likely"), "{rules:?}");
    let printed = format!("{steps:?}");
    for value in ["604281", "12345678", "example.com"] {
        assert!(!printed.contains(value), "{value} in the explanation");
    }
    assert!(printed.contains("DDDDDD"));
}

#[test]
fn fixtures_keep_the_contract() {
    let cases: Vec<Case> = FIXTURES
        .iter()
        .map(|(name, expected)| {
            let expect = if expected.is_some() {
                Expect::Found
            } else {
                Expect::Nothing
            };
            Case::new(*name, input(name), expect)
        })
        .collect();
    conformance::extractor(&CodeExtractor::new(), &cases);
}
