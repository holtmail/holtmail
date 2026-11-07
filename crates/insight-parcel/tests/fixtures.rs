//! The §15 `.eml` fixtures, read through `holtmail_mime` and the runner's input builder.

use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Confidence, InsightExtractor, InsightInput, InsightKind, PartRef, Provenance,
};
use holtmail_insight_parcel::ParcelExtractor;
use holtmail_insights::Runner;

const FIXTURES: &[(&str, Option<(&str, &str)>)] = &[
    ("parcel-s10-valid", Some(("s10", "RA123456785RU"))),
    ("parcel-s10-bad-check", None),
    ("parcel-ups-valid", Some(("ups", "1Z4E8752A038192654"))),
    ("parcel-no-context", None),
    (
        "parcel-usps-valid",
        Some(("usps", "9400111899223197428497")),
    ),
    ("parcel-fedex-12-valid", Some(("fedex", "798530184721"))),
    ("parcel-fedex-15-valid", Some(("fedex", "612900348571028"))),
    (
        "parcel-dhl-express-valid",
        Some(("dhl_express", "4738291051")),
    ),
    (
        "parcel-russian-post-valid",
        Some(("russian_post", "14102081078442")),
    ),
    ("parcel-forwarded", None),
    ("parcel-phone-number", None),
];

fn input(name: &str) -> InsightInput {
    let path = format!("{}/tests/fixtures/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing"));
    let message = holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{name} does not parse"));
    Runner::input(&message).0
}

#[test]
fn every_fixture_gives_its_number() {
    let extractor = ParcelExtractor::new();
    for (name, expected) in FIXTURES {
        let items = extractor.extract(&input(name));
        let found: Vec<(&str, &str)> = items
            .iter()
            .filter_map(|item| match &item.kind {
                InsightKind::Parcel {
                    carrier, number, ..
                } => Some((carrier.as_str(), number.as_str())),
                _ => None,
            })
            .collect();
        match expected {
            Some(parcel) => assert_eq!(found, [*parcel], "{name}"),
            None => assert!(found.is_empty(), "{name}: {found:?}"),
        }
        for item in &items {
            assert_eq!(item.provenance, Provenance::Heuristic, "{name}");
            assert_eq!(item.confidence, Confidence::Likely, "{name}");
            assert_eq!(item.score, 850, "{name}");
            assert!(!item.forwarded, "{name}");
        }
    }
}

#[test]
fn html_fixture_reads_segments_and_builds_the_template_url() {
    let items = ParcelExtractor::new().extract(&input("parcel-ups-valid"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].origin, PartRef::Html);
    let InsightKind::Parcel {
        check_digit,
        track_url,
        ..
    } = &items[0].kind
    else {
        panic!("not a parcel");
    };
    assert!(*check_digit);
    assert_eq!(
        track_url.as_ref().map(|u| u.as_str()),
        Some("https://www.ups.com/track?tracknum=1Z4E8752A038192654")
    );

    let items = ParcelExtractor::new().extract(&input("parcel-s10-valid"));
    assert_eq!(items[0].origin, PartRef::Text);
    let InsightKind::Parcel { track_url, .. } = &items[0].kind else {
        panic!("not a parcel");
    };
    assert_eq!(
        track_url.as_ref().map(|u| u.as_str()),
        Some("https://www.pochta.ru/tracking?barcode=RA123456785RU")
    );
}

#[test]
fn explanations_name_rules_and_never_values() {
    let extractor = ParcelExtractor::new();
    let steps = extractor.explain(&input("parcel-s10-valid"));
    let rules: Vec<&str> = steps.iter().map(|s| s.rule).collect();
    for rule in ["pattern", "check_digit", "keyword", "chosen"] {
        assert!(rules.contains(&rule), "{rule} missing in {rules:?}");
    }
    let printed = format!("{steps:?}");
    for value in ["RA123456785RU", "123 456", "pochta", "example.com"] {
        assert!(!printed.contains(value), "{value} in the explanation");
    }
    assert!(printed.contains("aaDDDDDDDDDaa"));
    assert!(printed.contains("tracking number"));

    let rules: Vec<&str> = extractor
        .explain(&input("parcel-s10-bad-check"))
        .iter()
        .map(|s| s.rule)
        .collect();
    assert!(rules.contains(&"check_digit") && !rules.contains(&"chosen"));
    let rules: Vec<&str> = extractor
        .explain(&input("parcel-forwarded"))
        .iter()
        .map(|s| s.rule)
        .collect();
    assert!(rules.contains(&"forwarded"), "{rules:?}");
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
    conformance::extractor(&ParcelExtractor::new(), &cases);
}
