//! The §15 `.eml` fixtures, read through `holtmail_mime` and the runner's input builder. The
//! extractor reports what the fields declare; the auth facts the runner records next to it are
//! what stage B uses to decide coverage, so they are checked here too.

use std::sync::Arc;

use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{InsightExtractor, InsightKind};
use holtmail_insight_unsubscribe::UnsubscribeExtractor;
use holtmail_insights::Runner;
use holtmail_mime::ParsedMessage;
use serde_json::{Value, json};

const HTTPS: &str = "https://news.example.com/unsubscribe/one/7f3a";
const MAILTO: &str = "mailto:leave@news.example.com";

struct Expected {
    https: Option<&'static str>,
    mailto: Option<&'static str>,
    one_click: bool,
}

const ONE_CLICK: Expected = Expected {
    https: Some(HTTPS),
    mailto: Some(MAILTO),
    one_click: true,
};

const FIXTURES: &[(&str, Expected)] = &[
    ("unsub-one-click-covered", ONE_CLICK),
    ("unsub-one-click-h-missing-post", ONE_CLICK),
    ("unsub-one-click-other-selector", ONE_CLICK),
    ("unsub-one-click-unaligned", ONE_CLICK),
    ("unsub-not-oversigned", ONE_CLICK),
    (
        "unsub-prepended-second-field",
        Expected {
            https: Some("https://attacker.example.net/u/1"),
            mailto: None,
            one_click: false,
        },
    ),
    (
        "unsub-bad-item-stops",
        Expected {
            https: None,
            mailto: Some(MAILTO),
            one_click: false,
        },
    ),
    (
        "unsub-file-scheme",
        Expected {
            https: None,
            mailto: Some(MAILTO),
            one_click: false,
        },
    ),
];

fn message(name: &str) -> ParsedMessage {
    let path = format!("{}/tests/fixtures/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing"));
    holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{name} does not parse"))
}

#[test]
fn every_fixture_gives_its_links() {
    let extractor = UnsubscribeExtractor::new();
    for (name, expected) in FIXTURES {
        let items = extractor.extract(&Runner::input(&message(name)).0);
        assert_eq!(items.len(), 1, "{name}");
        let InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } = &items[0].kind
        else {
            panic!("{name}: not an unsubscribe item");
        };
        assert_eq!(https.as_ref().map(|u| u.as_str()), expected.https, "{name}");
        assert_eq!(mailto.as_deref(), expected.mailto, "{name}");
        assert!(http.is_none(), "{name}");
        assert_eq!(*one_click, expected.one_click, "{name}");
        assert!(list_id.is_none(), "{name}");
    }
}

fn auth(name: &str) -> Value {
    let runner = Runner::new(vec![Arc::new(UnsubscribeExtractor::new())]);
    serde_json::from_str(&runner.extract(&message(name)).auth).unwrap()
}

#[test]
fn auth_facts_carry_what_coverage_needs() {
    let covered = auth("unsub-one-click-covered");
    assert_eq!(covered["results"][0]["dkim"][0]["d"], "news.example.com");
    assert_eq!(
        covered["signatures"][0]["h"],
        json!({"from": 1, "list-unsubscribe": 1, "list-unsubscribe-post": 1})
    );
    assert_eq!(covered["signatures"][0]["l"], false);
    assert_eq!(
        covered["fields"],
        json!({"from": 1, "list-unsubscribe": 1, "list-unsubscribe-post": 1})
    );

    let missing = auth("unsub-one-click-h-missing-post");
    assert_eq!(
        missing["signatures"][0]["h"],
        json!({"from": 1, "list-unsubscribe": 1})
    );

    let selector = auth("unsub-one-click-other-selector");
    assert_eq!(selector["results"][0]["dkim"][0]["s"], "s1");
    let signatures = selector["signatures"].as_array().unwrap();
    assert_eq!(signatures.len(), 2);
    let s1 = signatures.iter().find(|s| s["s"] == "s1").unwrap();
    assert_eq!(s1["h"], json!({"from": 1}));

    let unaligned = auth("unsub-one-click-unaligned");
    assert_eq!(unaligned["results"][0]["dkim"][0]["d"], "esp.example.net");
    assert_eq!(unaligned["from_domain"], "news.example.com");

    let prepended = auth("unsub-prepended-second-field");
    assert_eq!(prepended["fields"]["list-unsubscribe"], 2);

    let not_oversigned = auth("unsub-not-oversigned");
    assert_eq!(
        not_oversigned["signatures"][0]["h"],
        json!({"from": 1, "list-unsubscribe": 1, "list-unsubscribe-post": 1})
    );
}

#[test]
fn explanations_name_items_and_never_links() {
    let extractor = UnsubscribeExtractor::new();
    let steps = extractor.explain(&Runner::input(&message("unsub-bad-item-stops")).0);
    let rules: Vec<&str> = steps.iter().map(|s| s.rule).collect();
    assert_eq!(
        rules,
        [
            "list_unsubscribe:fields",
            "item:mailto",
            "item:unbracketed_stop",
            "post:one_click",
            "one_click:no_https",
            "outcome:links",
        ]
    );
    let printed = format!("{steps:?}");
    assert!(!printed.contains("example.com"));
    assert!(!printed.contains("leave"));
}

#[test]
fn fixtures_keep_the_contract() {
    let cases: Vec<Case> = FIXTURES
        .iter()
        .map(|(name, _)| Case::new(*name, Runner::input(&message(name)).0, Expect::Found))
        .collect();
    conformance::extractor(&UnsubscribeExtractor::new(), &cases);
}
