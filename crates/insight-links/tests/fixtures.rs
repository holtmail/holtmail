//! The §15 `.eml` fixtures, read through `holtmail_mime` and the runner's input builder.
//!
//! Only this extractor's own fields are checked; `destination`, `via` and `tracked` are set by
//! the runner and checked there.

use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Confidence, InsightExtractor, InsightInput, InsightKind, LinkAction, PartRef, Provenance,
};
use holtmail_insight_links::LinksExtractor;
use holtmail_insights::Runner;

struct Expected {
    action: LinkAction,
    url: &'static str,
    text_host: Option<&'static str>,
    score: u16,
}

const fn found(
    action: LinkAction,
    url: &'static str,
    text_host: Option<&'static str>,
    score: u16,
) -> Option<Expected> {
    Some(Expected {
        action,
        url,
        text_host,
        score,
    })
}

const FIXTURES: &[(&str, Option<Expected>)] = &[
    (
        "link-reset-safelinks",
        found(
            LinkAction::ResetPassword,
            "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Faccounts.example.com%2Freset%3Ft%3D7d41c09e&data=05%7C02%7Cada%40example.org%7C0000%7C&sdata=AbCdEf%3D&reserved=0",
            None,
            750,
        ),
    ),
    (
        "link-urldefense-v3",
        found(
            LinkAction::Verify,
            "https://urldefense.com/v3/__https://accounts.example.com/verify?t=8f2c1a__;!!AbCdEfGh!x1Y2z3W4v5U6$",
            None,
            750,
        ),
    ),
    (
        "link-lookalike-wrapper-host",
        found(
            LinkAction::ResetPassword,
            "https://safelinks.protection.outlook.com.evil.example/?url=https%3A%2F%2Faccounts.example.com%2Freset%3Ft%3D19ab&data=1",
            None,
            750,
        ),
    ),
    (
        "link-esp-tracked",
        found(
            LinkAction::Confirm,
            "https://u4812.ct.sendgrid.net/ls/click?upn=u001.Qm9va2luZ0V4YW1wbGVDb25maXJtLTEyMzQ1Njc4OTA-2BxYz",
            None,
            600,
        ),
    ),
    (
        "link-text-domain-mismatch",
        found(
            LinkAction::SignIn,
            "https://example.net/login?session=4d2b",
            Some("accounts.example.com"),
            650,
        ),
    ),
    (
        "link-text-url-mismatch",
        found(
            LinkAction::SignIn,
            "https://example.net/login?session=4d2b",
            Some("accounts.example.com"),
            1000,
        ),
    ),
    (
        "link-destination-differs",
        found(
            LinkAction::Verify,
            "https://verify.example.org/verify?t=5c7e2a91",
            None,
            750,
        ),
    ),
    (
        "link-idn-homograph",
        found(
            LinkAction::SignIn,
            "https://xn--exmple-4nf.com/login?next=%2Fhome",
            None,
            750,
        ),
    ),
    ("link-unsubscribe-not-action", None),
    (
        "link-wallet-save",
        found(
            LinkAction::WalletSave,
            "https://pay.google.com/gp/v/save/eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJib29raW5nQGV4YW1wbGUuY29tIn0.c2lnbmF0dXJl",
            None,
            600,
        ),
    ),
    ("link-forwarded-below-marker", None),
    ("link-footer-only", None),
    ("link-newsletter-shop-now", None),
];

fn input(name: &str) -> InsightInput {
    let path = format!("{}/tests/fixtures/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing"));
    let message = holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{name} does not parse"));
    Runner::input(&message).0
}

#[test]
fn every_fixture_gives_its_action() {
    let extractor = LinksExtractor::new();
    for (name, expected) in FIXTURES {
        let items = extractor.extract(&input(name));
        let Some(expected) = expected else {
            assert!(items.is_empty(), "{name}: {items:?}");
            continue;
        };
        assert_eq!(items.len(), 1, "{name}: {items:?}");
        let item = &items[0];
        assert_eq!(item.provenance, Provenance::Heuristic, "{name}");
        assert_eq!(item.confidence, Confidence::Likely, "{name}");
        assert_eq!(item.origin, PartRef::Html, "{name}");
        assert!(!item.forwarded, "{name}");
        assert_eq!(item.score, expected.score, "{name}");
        let InsightKind::Action {
            action,
            url,
            text_host,
            ..
        } = &item.kind
        else {
            panic!("{name}: not an action");
        };
        assert_eq!(*action, expected.action, "{name}");
        assert_eq!(url.as_str(), expected.url, "{name}");
        assert_eq!(text_host.as_deref(), expected.text_host, "{name}");
    }
}

#[test]
fn explanations_name_rules_and_never_hosts_or_urls() {
    let extractor = LinksExtractor::new();
    let steps = extractor.explain(&input("link-reset-safelinks"));
    let rules: Vec<&str> = steps.iter().map(|s| s.rule).collect();
    for rule in [
        "label",
        "button_like",
        "path_token",
        "first",
        "score",
        "chosen",
    ] {
        assert!(rules.contains(&rule), "{rule} missing from {rules:?}");
    }
    let printed = format!("{steps:?}");
    for value in [
        "example.com",
        "outlook.com",
        "7d41c09e",
        "Reset password",
        "http",
    ] {
        assert!(!printed.contains(value), "{value} in the explanation");
    }

    let social = extractor.explain(&input("link-newsletter-shop-now"));
    let printed = format!("{social:?}");
    assert!(social.iter().any(|s| s.rule == "unsubscribe"));
    assert!(!printed.contains("example_store") && !printed.contains("7c1d"));

    let forwarded = extractor.explain(&input("link-forwarded-below-marker"));
    assert!(forwarded.iter().any(|s| s.rule == "forwarded"));
    let wallet = extractor.explain(&input("link-wallet-save"));
    assert!(wallet.iter().any(|s| s.rule == "wallet"));
    assert!(!format!("{wallet:?}").contains("pay.google.com"));
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
    conformance::extractor(&LinksExtractor::new(), &cases);
}
