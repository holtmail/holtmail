//! Stage A over hand-written messages.

mod common;

use holtmail_insight::{InsightKind, Reservation, Signal};
use holtmail_insights::Runner;
use serde_json::{Value, json};

fn extract(name: &str) -> holtmail_insights::Extracted {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(path).unwrap();
    let message = holtmail_mime::parse(&raw).unwrap();
    Runner::new(Vec::new()).extract(&message)
}

#[test]
fn a_forged_result_is_recorded_as_written() {
    let extracted = extract("smtp-forged-ar-no-trust.eml");
    let auth: Value = serde_json::from_str(&extracted.auth).unwrap();
    assert_eq!(
        auth["results"],
        json!([{"pos": 0, "authserv": "mx.example.org", "dkim": [
            {"d": "bank.example.com", "i": null, "s": "s1", "b": "Forged12"}
        ]}])
    );
    assert_eq!(auth["signatures"][0]["d"], "relay.example.net");
    assert_eq!(auth["sig_cap_hit"], false);
    assert_eq!(auth["from_domain"], "bank.example.com");
    assert_eq!(
        extracted.from_address.as_deref(),
        Some("alerts@bank.example.com")
    );
    assert!(extracted.items.items.is_empty());
    assert!(extracted.signals.is_empty());
}

struct Link {
    host: &'static str,
    via: Option<&'static str>,
    tracked: bool,
}

fn link(host: &'static str, via: Option<&'static str>, tracked: bool) -> Option<Link> {
    Some(Link { host, via, tracked })
}

#[test]
fn actions_are_unwrapped_at_the_runner() {
    let links_fixtures = format!(
        "{}/../insight-links/tests/fixtures",
        env!("CARGO_MANIFEST_DIR")
    );
    let own_fixtures = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let cases = [
        (
            &links_fixtures,
            "link-reset-safelinks.eml",
            link("accounts.example.com", Some("outlook.com"), false),
        ),
        (
            &links_fixtures,
            "link-urldefense-v3.eml",
            link("accounts.example.com", Some("proofpoint.com"), false),
        ),
        (
            &own_fixtures,
            "link-google-url.eml",
            link("shop.example.com", Some("google.com"), false),
        ),
        (
            &links_fixtures,
            "link-lookalike-wrapper-host.eml",
            link("safelinks.protection.outlook.com.evil.example", None, false),
        ),
        (
            &links_fixtures,
            "link-esp-tracked.eml",
            link("u4812.ct.sendgrid.net", None, true),
        ),
        (
            &links_fixtures,
            "link-text-domain-mismatch.eml",
            link("example.net", None, false),
        ),
        (
            &links_fixtures,
            "link-text-url-mismatch.eml",
            link("example.net", None, false),
        ),
        (
            &links_fixtures,
            "link-destination-differs.eml",
            link("verify.example.org", None, false),
        ),
        (
            &links_fixtures,
            "link-idn-homograph.eml",
            link("xn--exmple-4nf.com", None, false),
        ),
        (
            &links_fixtures,
            "link-wallet-save.eml",
            link("pay.google.com", None, false),
        ),
        (&links_fixtures, "link-unsubscribe-not-action.eml", None),
        (&links_fixtures, "link-forwarded-below-marker.eml", None),
        (&links_fixtures, "link-footer-only.eml", None),
        (&links_fixtures, "link-newsletter-shop-now.eml", None),
    ];
    let runner = common::runner();
    for (dir, file, expected) in cases {
        let raw = std::fs::read(format!("{dir}/{file}")).unwrap();
        let extracted = runner.extract(&holtmail_mime::parse(&raw).unwrap());
        let actions: Vec<_> = extracted
            .items
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                InsightKind::Action {
                    url,
                    destination,
                    via,
                    tracked,
                    ..
                } => Some((url, destination, via, *tracked)),
                _ => None,
            })
            .collect();
        let Some(expected) = expected else {
            assert!(actions.is_empty(), "{file}: {actions:?}");
            continue;
        };
        let (url, destination, via, tracked) = actions
            .first()
            .unwrap_or_else(|| panic!("{file}: no action"));
        assert_eq!(destination.host(), Some(expected.host), "{file}");
        assert_eq!(via.as_deref(), expected.via, "{file}");
        assert_eq!(*tracked, expected.tracked, "{file}");
        assert_eq!(expected.via.is_none(), url == destination, "{file}");
    }
}

#[test]
fn an_over_cap_row_drops_later_legs_first() {
    let path = format!(
        "{}/tests/fixtures/items-over-cap.eml",
        env!("CARGO_MANIFEST_DIR")
    );
    let message = holtmail_mime::parse(&std::fs::read(path).unwrap()).unwrap();
    let runner = common::runner();
    let extracted = runner.extract(&message);
    assert!(extracted.truncated);
    assert!(extracted.items.to_json().len() <= holtmail_insights::MAX_ITEMS_JSON_BYTES);
    let flights: Vec<String> = extracted
        .items
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            InsightKind::Reservation(Reservation::Flight(flight)) => {
                assert_eq!(flight.passengers.len(), 3);
                flight.flight_number.clone()
            }
            InsightKind::Parcel { .. } => None,
            other => panic!("unexpected {:?}", other.tag()),
        })
        .collect();
    assert!(
        (4..16).contains(&flights.len()),
        "{} flights kept",
        flights.len()
    );
    let first: Vec<String> = (100..100 + flights.len()).map(|n| n.to_string()).collect();
    assert_eq!(flights, first, "the earliest legs are kept, in order");
    assert!(extracted.signals.contains(Signal::Structured));
    assert_eq!(runner.extract(&message), extracted);

    let report = runner.explain(&message);
    for secret in [
        "OVRCAP",
        "Example-Longname",
        "1Z4E7092F038824511",
        "FRA",
        "LIS",
    ] {
        assert!(!report.contains(secret), "{secret} leaked:\n{report}");
    }
}
