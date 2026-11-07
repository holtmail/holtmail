//! The conformance suite with built inputs: positives per carrier, hard negatives and sender
//! host hints from an invented carrier table.

use holtmail_core::{Address, Timestamp};
use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Anchor, Confidence, HeaderField, HtmlView, InsightExtractor, InsightInput, InsightKind, Segment,
};
use holtmail_insight_parcel::{ALGORITHM, ID, ParcelExtractor};

const TABLE: &str = include_str!("../data/carriers.toml");

fn mail(from: &str, text: &str) -> InsightInput {
    InsightInput {
        headers: vec![
            HeaderField::new("From", format!("Example <{from}>")),
            HeaderField::new("To", "Ada Example <ada@example.org>"),
            HeaderField::new("Subject", "Your order"),
        ],
        from: vec![Address {
            name: Some("Example".into()),
            email: from.into(),
        }],
        subject: Some("Your order".into()),
        sent_at: Some(Timestamp::from_secs(1_793_952_720)),
        text: text.into(),
        ..InsightInput::default()
    }
}

fn shop(text: &str) -> InsightInput {
    mail("orders@shop.example.com", text)
}

fn html(segments: &[&str], footer_at: Option<usize>, anchors: Vec<Anchor>) -> InsightInput {
    let segments = segments
        .iter()
        .map(|text| Segment {
            text: (*text).into(),
            alone_on_line: true,
            ..Segment::default()
        })
        .collect();
    InsightInput {
        html: Some(
            HtmlView::new()
                .with_segments(segments)
                .with_footer_at(footer_at)
                .with_anchors(anchors),
        ),
        ..shop("")
    }
}

fn parcels(extractor: &ParcelExtractor, input: &InsightInput) -> Vec<(String, String, u16)> {
    extractor
        .extract(input)
        .into_iter()
        .filter_map(|item| match item.kind {
            InsightKind::Parcel {
                carrier, number, ..
            } => Some((carrier.as_str().to_owned(), number, item.score)),
            _ => None,
        })
        .collect()
}

fn positives() -> Vec<(&'static str, InsightInput)> {
    vec![
        ("s10-ru", shop("Трек-номер вашей посылки: RA123456785RU")),
        (
            "s10-de",
            shop("Ihre Sendungsnummer lautet CP 473 829 103 DE."),
        ),
        ("s10-cn", shop("您的运单号：EE582039171CN")),
        ("s10-us-spaced", shop("Tracking number: LX 908 172 638 US")),
        ("ups-grouped", shop("UPS tracking: 1Z 999 AA1 01 2345 6784")),
        ("usps", shop("USPS tracking number 9205590164917312751089")),
        ("fedex-12-fr", shop("Numéro de suivi : 612900348578")),
        (
            "fedex-15-es",
            shop("Número de seguimiento: 041441760228964"),
        ),
        ("dhl-ja", shop("追跡番号: 1283904753")),
        (
            "russian-post",
            shop("Почтовый идентификатор 63001204567893"),
        ),
        ("ko", shop("운송장 번호: 4738291051")),
        ("tr", shop("Kargo takip numarası: 798530184721")),
        ("pl", shop("Numer przesyłki: RA123456785RU")),
        (
            "html-strong",
            html(
                &[
                    "Your parcel is on its way",
                    "Tracking number",
                    "1Z4E8752A038192654",
                ],
                None,
                vec![],
            ),
        ),
    ]
}

fn negatives() -> Vec<(&'static str, InsightInput)> {
    vec![
        ("bad-check", shop("Tracking number: RA123456784RU")),
        ("no-keyword", shop("Your parcel left today: 798530184721")),
        (
            "order-number",
            shop("Order number 798530184721 is confirmed."),
        ),
        (
            "invoice",
            shop("Invoice 4738291051, tracking to follow by email."),
        ),
        ("phone", shop("Tracking questions? Call 312 555 0186.")),
        ("phone-plus", shop("Tracking help: +1 718 555 0120")),
        (
            "iban",
            shop("Tracking refunds go to IBAN DE89 3704 0044 0532 0130 00."),
        ),
        (
            "card",
            shop("Tracking fee charged to card 4111 1111 1111 1111."),
        ),
        ("card-grouped-12", shop("Tracking fee: 1234 7985 3018 4721")),
        (
            "date",
            shop("Tracking starts 2026-11-06 10:30, 06.11.2026."),
        ),
        ("digit-run", shop("Tracking reference 12-798530184721-7")),
        ("too-long-run", shop("Tracking 94001118992231974284971")),
        (
            "footer",
            html(
                &[
                    "Thanks for your order",
                    "© 2026 Example",
                    "Tracking number RA123456785RU",
                ],
                Some(1),
                vec![],
            ),
        ),
        (
            "forwarded",
            InsightInput {
                forward_at: Some(14),
                ..shop("See below.\n\n-- Forwarded message --\nTracking number: RA123456785RU\n")
            },
        ),
        (
            "href-only",
            html(
                &["Your parcel is on its way"],
                None,
                vec![Anchor {
                    href: "https://shop.example.com/track/RA123456785RU".into(),
                    text: "Track your parcel".into(),
                    ..Anchor::default()
                }],
            ),
        ),
    ]
}

fn hinted() -> ParcelExtractor {
    let table = TABLE.replace("hosts = [\"ups.com\"]", "hosts = [\"parcels.example.com\"]");
    assert_ne!(table, TABLE);
    ParcelExtractor::with_data(&table).expect("invented table parses")
}

#[test]
fn every_positive_is_found_and_every_negative_is_not() {
    let extractor = ParcelExtractor::new();
    for (name, input) in positives() {
        let found = parcels(&extractor, &input);
        assert_eq!(found.len(), 1, "{name}: {found:?}");
        assert_eq!(found[0].2, 850, "{name}");
    }
    for (name, input) in negatives() {
        assert!(parcels(&extractor, &input).is_empty(), "{name}");
    }
}

#[test]
fn host_hints_match_on_a_label_boundary() {
    let extractor = hinted();
    let number = "Your shipment 1Z4E8752A038192654 left the depot.";
    let found = parcels(&extractor, &mail("notify@parcels.example.com", number));
    assert_eq!(found, [("ups".into(), "1Z4E8752A038192654".into(), 900)]);
    let found = parcels(&extractor, &mail("notify@eu.parcels.example.com", number));
    assert_eq!(found.len(), 1);
    for from in [
        "notify@otherparcels.example.com",
        "notify@parcels.example.com.example.org",
        "notify@example.com",
    ] {
        assert!(
            parcels(&extractor, &mail(from, number)).is_empty(),
            "{from}"
        );
    }
    let both = "UPS tracking number 1Z4E8752A038192654.";
    let found = parcels(&extractor, &mail("notify@parcels.example.com", both));
    assert_eq!(found[0].2, 950);
    let items = extractor.extract(&mail("notify@parcels.example.com", both));
    assert_eq!(items[0].confidence, Confidence::Likely);
}

#[test]
fn ambiguous_numbers_need_carrier_evidence() {
    let table = TABLE.replace(
        "[carriers.dhl_express]",
        "[carriers.other_express]\ncheck = \"dhl_express\"\npatterns = [\"DDDDDDDDDD\"]\nkeywords = [\"other express\"]\nhosts = [\"other.example.org\"]\n\n[carriers.dhl_express]",
    );
    let extractor = ParcelExtractor::with_data(&table).unwrap();
    assert!(parcels(&extractor, &shop("Tracking number 4738291051")).is_empty());
    let found = parcels(
        &extractor,
        &shop("Other Express tracking number 4738291051"),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "other_express");
    let found = parcels(
        &extractor,
        &mail("t@other.example.org", "Tracking 4738291051"),
    );
    assert_eq!(found[0].0, "other_express");
}

#[test]
fn version_names_algorithm_and_table() {
    let embedded = ParcelExtractor::new();
    assert_eq!(embedded.id(), ID);
    assert!(embedded.version().starts_with(&format!("{ALGORITHM}+")));
    assert_ne!(hinted().version(), embedded.version());
    assert!(ParcelExtractor::with_data("[scores]").is_err());
    assert!(ParcelExtractor::default().version() == embedded.version());
}

#[test]
fn built_cases_keep_the_contract() {
    let mut cases: Vec<Case> = positives()
        .into_iter()
        .map(|(name, input)| Case::new(name, input, Expect::Found))
        .collect();
    cases.extend(
        negatives()
            .into_iter()
            .map(|(name, input)| Case::new(name, input, Expect::Nothing)),
    );
    conformance::extractor(&ParcelExtractor::new(), &cases);
    let hinted_cases = vec![Case::new(
        "host-hint",
        mail("notify@parcels.example.com", "Shipment 1Z4E8752A038192654"),
        Expect::Found,
    )];
    conformance::extractor(&hinted(), &hinted_cases);
}
