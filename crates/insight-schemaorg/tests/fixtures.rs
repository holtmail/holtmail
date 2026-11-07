//! The §15 `.eml` fixtures, read through `holtmail_mime` and the full runner.

use std::sync::Arc;

use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Confidence, Insight, InsightExtractor, InsightInput, InsightKind, LocalOrZoned, Money,
    Provenance, Reservation,
};
use holtmail_insight_schemaorg::SchemaOrgExtractor;
use holtmail_insights::Runner;

const FIXTURES: &[(&str, &[&str])] = &[
    (
        "jsonld-flight-graph",
        &[
            "flight QX7K2P XA 482 LHR-AMS 2027-03-14T09:30:00+00:00..2027-03-14T12:05:00+01:00 [Ada Example] [14C] https://travel.example.com/trips/QX7K2P",
            "action CheckIn https://travel.example.com/checkin/QX7K2P",
        ],
    ),
    (
        "jsonld-array-uri-type",
        &[
            "train TR-5521 IC 72 Example Central-Sampleton 2027-04-02 07:12:00 [] [42]",
            "event EV-881 Example Quartet Live 2027-05-01T20:00:00+02:00 Example Hall []",
        ],
    ),
    (
        "jsonld-multi-passenger-two-legs",
        &[
            "flight RT4PAX XA 101 LHR-AMS 2027-03-14T09:30:00+00:00..2027-03-14T12:00:00+01:00 [Ada Example, Ben Example, Cleo Example, Dan Example] [20A, 21A, 22A, 23A] -",
            "flight RT4PAX XA 102 AMS-LHR 2027-03-21T18:10:00+01:00..2027-03-21T18:35:00+00:00 [Ada Example, Ben Example, Cleo Example, Dan Example] [20A, 21A, 22A, 23A] -",
        ],
    ),
    (
        "microdata-lodging",
        &[
            "lodging HX-4417 Example Harbour Hotel (1 Example Quay, Exampleton) 2026-12-01T15:00:00+01:00..2026-12-03T11:00:00+01:00 [Ada Example] https://stays.example.com/booking/HX-4417",
        ],
    ),
    (
        "jsonld-forged-unverified",
        &["flight FAKE01 XA 777 JFK-LHR 2027-02-01T21:00:00-05:00.. [Ada Example] [] -"],
    ),
    ("jsonld-end-before-start", &[]),
    ("jsonld-deep", &["order DEEP-1 Example Shop - - - -"]),
    (
        "jsonld-id-cycle",
        &["flight CYC123 - 300 CDG-FCO 2027-07-01T08:00:00+02:00.. [] [] -"],
    ),
    (
        "jsonld-price-jpy",
        &["order JP-3310 Example Shop 12800 JPY - -"],
    ),
    (
        "jsonld-price-kwd",
        &["invoice KW-0042 Example Telecom 4125 KWD 2027-01-31 -"],
    ),
    (
        "jsonld-price-overflow",
        &["order BIG-1 Example Shop - - - -"],
    ),
    (
        "fwd-inline-booking",
        &["food TB-209 Example Bistro 2027-02-14 19:30:00 party 2 (forwarded)"],
    ),
    (
        "jsonld-order",
        &[
            "order ORD-2041 Example Shop 5990 EUR OrderProcessing https://shop.example.com/orders/2041",
            "action View https://shop.example.com/orders/2041 View order",
        ],
    ),
    (
        "jsonld-invoice",
        &[
            "invoice INV-0091 Example Utilities 8420 EUR 2027-01-15 https://billing.example.com/invoices/0091",
        ],
    ),
    (
        "jsonld-parcel-delivery",
        &[
            "parcel ups 1Z4E8752A038192654",
            "action Track https://store.example.com/track/7731",
        ],
    ),
    (
        "jsonld-actions",
        &[
            "action Confirm https://news.example.com/confirm/k3 Confirm",
            "action Track https://store.example.com/track/42 Track",
        ],
    ),
    (
        "jsonld-unknown-type-mix",
        &["order MIX-7 Example Shop - - - -"],
    ),
];

fn message(name: &str) -> holtmail_mime::ParsedMessage {
    let path = format!("{}/tests/fixtures/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing"));
    holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{name} does not parse"))
}

fn input(name: &str) -> InsightInput {
    Runner::input(&message(name)).0
}

fn at(time: &LocalOrZoned) -> String {
    match time {
        LocalOrZoned::Local(at) => at.to_string(),
        LocalOrZoned::Zoned(at, _) => at.to_rfc3339(),
    }
}

fn opt(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

fn money(total: &Option<Money>) -> String {
    total.as_ref().map_or("- -".into(), |m| {
        format!("{} {}", m.minor, m.currency.code())
    })
}

fn describe(item: &Insight) -> String {
    let text = match &item.kind {
        InsightKind::Reservation(Reservation::Flight(f)) => format!(
            "flight {} {} {} {}-{} {}..{} [{}] [{}] {}",
            opt(f.reservation_number.as_deref()),
            opt(f.carrier.as_deref()),
            opt(f.flight_number.as_deref()),
            f.departure_airport,
            f.arrival_airport,
            at(&f.departure),
            f.arrival.as_ref().map(at).unwrap_or_default(),
            f.passengers.join(", "),
            f.seats.join(", "),
            opt(f.url.as_ref().map(|u| u.as_str())),
        ),
        InsightKind::Reservation(Reservation::Lodging(l)) => format!(
            "lodging {} {} ({}) {}..{} [{}] {}",
            l.reservation_number,
            l.name,
            opt(l.address.as_deref()),
            at(&l.checkin),
            l.checkout.as_ref().map(at).unwrap_or_default(),
            l.guests.join(", "),
            opt(l.url.as_ref().map(|u| u.as_str())),
        ),
        InsightKind::Reservation(Reservation::Train(t)) => format!(
            "train {} {} {}-{} {} [{}] [{}]",
            opt(t.reservation_number.as_deref()),
            opt(t.train_number.as_deref()),
            t.departure_station,
            opt(t.arrival_station.as_deref()),
            at(&t.departure),
            t.passengers.join(", "),
            t.seats.join(", "),
        ),
        InsightKind::Reservation(Reservation::Event(e)) => format!(
            "event {} {} {} {} [{}]",
            opt(e.reservation_number.as_deref()),
            e.name,
            at(&e.start),
            opt(e.venue.as_deref()),
            e.tickets.join(", "),
        ),
        InsightKind::Reservation(Reservation::Food(f)) => format!(
            "food {} {} {} party {}",
            opt(f.reservation_number.as_deref()),
            f.name,
            at(&f.start),
            f.party_size.unwrap_or_default(),
        ),
        InsightKind::Order {
            number,
            merchant,
            total,
            status,
            url,
        } => format!(
            "order {} {} {} {} {}",
            opt(number.as_deref()),
            opt(merchant.as_deref()),
            money(total),
            opt(status.as_deref()),
            opt(url.as_ref().map(|u| u.as_str())),
        ),
        InsightKind::Invoice {
            number,
            payee,
            total,
            due,
            url,
        } => format!(
            "invoice {} {} {} {} {}",
            opt(number.as_deref()),
            opt(payee.as_deref()),
            money(total),
            due.map(|d| d.0.to_string()).unwrap_or_default(),
            opt(url.as_ref().map(|u| u.as_str())),
        ),
        InsightKind::Parcel {
            carrier, number, ..
        } => format!("parcel {} {number}", carrier.as_str()),
        InsightKind::Action {
            action, url, label, ..
        } => {
            let mut text = format!("action {action:?} {url}");
            if let Some(label) = label {
                text.push(' ');
                text.push_str(label);
            }
            text
        }
        other => format!("{:?}", other.tag()),
    };
    if item.forwarded {
        format!("{text} (forwarded)")
    } else {
        text
    }
}

#[test]
fn every_fixture_gives_its_items_through_the_runner() {
    let runner = Runner::new(vec![Arc::new(SchemaOrgExtractor::new())]);
    for (name, expected) in FIXTURES {
        let row = runner.extract(&message(name));
        let found: Vec<String> = row.items.items.iter().map(describe).collect();
        assert_eq!(found, *expected, "{name}");
        for item in &row.items.items {
            assert_eq!(item.confidence, Confidence::Certain, "{name}");
            assert_eq!(item.score, 1000, "{name}");
            let structured = if *name == "microdata-lodging" {
                Provenance::Microdata
            } else {
                Provenance::JsonLd
            };
            assert_eq!(item.provenance, structured, "{name}");
        }
    }
}

#[test]
fn extractor_output_matches_the_runner() {
    let extractor = SchemaOrgExtractor::new();
    for (name, expected) in FIXTURES {
        let items = extractor.extract(&input(name));
        assert_eq!(items.len(), expected.len(), "{name}");
    }
}

#[test]
fn explanations_of_fixtures_hold_no_values() {
    let extractor = SchemaOrgExtractor::new();
    for (name, _) in FIXTURES {
        let printed = format!("{:?}", extractor.explain(&input(name)));
        for value in [
            "QX7K2P",
            "Ada",
            "example.com",
            "Example Shop",
            "1Z4E8752A038192654",
            "HX-4417",
            "12800",
        ] {
            assert!(
                !printed.contains(value),
                "{name}: {value} in the explanation"
            );
        }
    }
    let rules: Vec<&str> = extractor
        .explain(&input("jsonld-end-before-start"))
        .iter()
        .map(|s| s.rule)
        .collect();
    assert!(rules.contains(&"end_before_start"), "{rules:?}");
    let rules: Vec<&str> = extractor
        .explain(&input("jsonld-deep"))
        .iter()
        .map(|s| s.rule)
        .collect();
    assert!(
        rules.contains(&"ld_json_unparsable") && rules.contains(&"depth_cut"),
        "{rules:?}"
    );
    let rules: Vec<&str> = extractor
        .explain(&input("jsonld-price-overflow"))
        .iter()
        .map(|s| s.rule)
        .collect();
    assert!(rules.contains(&"money_dropped"), "{rules:?}");
}

#[test]
fn fixtures_keep_the_contract() {
    let cases: Vec<Case> = FIXTURES
        .iter()
        .map(|(name, expected)| {
            let expect = if expected.is_empty() {
                Expect::Nothing
            } else {
                Expect::Found
            };
            Case::new(*name, input(name), expect)
        })
        .collect();
    conformance::extractor(&SchemaOrgExtractor::new(), &cases);
}
