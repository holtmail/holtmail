use std::panic::{AssertUnwindSafe, catch_unwind};

use bytes::Bytes;
use holtmail_insight::limits::{
    MAX_HTML_BYTES, MAX_ITEMS_PER_EXTRACTOR, MAX_PKPASS_ATTACHMENTS, MAX_PKPASS_BYTES,
    MAX_TEXT_BYTES,
};
use holtmail_insight::testing::{hostile_corpus, personal_mail};
use holtmail_insight::{
    FlightReservation, InsightKind, KindTag, LinkAction, LocalOrZoned, PartRef, Reservation, Url,
};
use holtmail_mime::Attachment;

use super::*;

struct Fixed {
    id: &'static str,
    version: &'static str,
    items: Vec<Insight>,
}

impl InsightExtractor for Fixed {
    fn id(&self) -> &str {
        self.id
    }
    fn version(&self) -> String {
        self.version.to_owned()
    }
    fn extract(&self, _: &InsightInput) -> Vec<Insight> {
        self.items.clone()
    }
}

struct Panics;

impl InsightExtractor for Panics {
    fn id(&self) -> &str {
        "boom"
    }
    fn version(&self) -> String {
        "7".into()
    }
    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        let text = std::hint::black_box(input.text.as_str());
        assert!(text.is_ascii(), "cannot read {text:?}");
        vec![code(text)]
    }
}

struct Digits;

impl InsightExtractor for Digits {
    fn id(&self) -> &str {
        "digits"
    }
    fn version(&self) -> String {
        "1".into()
    }
    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        let segments = input.html.iter().flat_map(|html| &html.segments);
        input
            .text
            .split(|c: char| !c.is_ascii_digit())
            .chain(segments.flat_map(|s| s.text.split(|c: char| !c.is_ascii_digit())))
            .filter(|run| run.len() >= 4)
            .take(64)
            .map(code)
            .collect()
    }
}

fn item(kind: InsightKind) -> Insight {
    Insight {
        kind,
        provenance: Provenance::Heuristic,
        confidence: Confidence::Likely,
        score: 500,
        origin: PartRef::Text,
        forwarded: false,
    }
}

fn code(value: &str) -> Insight {
    item(InsightKind::Code {
        value: value.into(),
        origin_bound: None,
    })
}

fn action(path: &str, label: String) -> Insight {
    action_of(
        LinkAction::View,
        &format!("https://example.com/{path}"),
        label,
    )
}

fn action_of(kind: LinkAction, url: &str, label: String) -> Insight {
    let url = Url::parse(url).unwrap();
    item(InsightKind::Action {
        action: kind,
        destination: url.clone(),
        url,
        via: None,
        tracked: false,
        text_host: None,
        label: Some(label),
    })
}

fn fixed(id: &'static str, items: Vec<Insight>) -> Arc<dyn InsightExtractor> {
    Arc::new(Fixed {
        id,
        version: "1",
        items,
    })
}

fn values(extracted: &Extracted) -> Vec<String> {
    extracted
        .items
        .items
        .iter()
        .map(|item| match &item.kind {
            InsightKind::Code { value, .. } => value.clone(),
            InsightKind::Action { url, .. } => url.as_str().to_owned(),
            other => other.tag().name().to_owned(),
        })
        .collect()
}

const MESSAGE: &str = "Return-Path: <bounce@news.example.com>\r\n\
Authentication-Results: mx.example.org; dkim=pass header.d=news.example.com header.s=s1 header.b=AbC12345\r\n\
DKIM-Signature: v=1; a=rsa-sha256; d=news.example.com; s=s1; h=from:list-unsubscribe:list-unsubscribe; bh=x; b=AbC12345zz\r\n\
From: Example News <No-Reply@News.Example.com>\r\n\
To: ada@example.org\r\n\
Subject: Your code\r\n\
Date: Thu, 05 Nov 2026 10:00:00 +0000\r\n\
List-Unsubscribe: <https://news.example.com/u/1>\r\n\
Message-ID: <code-1@news.example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Your code is \u{FF14}\u{FF18}\u{FF12}\u{FF19}\u{FF11}\u{FF13}.\r\n\
--b\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Your code is <strong>482913</strong>.</p><a href=\"https://news.example.com/x\">Open</a>\r\n\
--b--\r\n";

fn parsed() -> ParsedMessage {
    holtmail_mime::parse(MESSAGE.as_bytes()).unwrap()
}

#[test]
fn a_message_end_to_end() {
    let runner = Runner::new(vec![Arc::new(Digits)]);
    let extracted = runner.extract(&parsed());
    assert_eq!(extracted.outcome, InsightOutcome::Ok);
    assert!(!extracted.truncated);
    assert_eq!(
        extracted.from_address.as_deref(),
        Some("no-reply@news.example.com")
    );
    assert_eq!(values(&extracted), ["482913"]);
    let signals: Vec<_> = extracted.signals.iter().map(Signal::name).collect();
    assert_eq!(signals, ["unsubscribe", "noreply"]);
    assert_eq!(extracted.kinds.iter().collect::<Vec<_>>(), [KindTag::Code]);
    let auth: serde_json::Value = serde_json::from_str(&extracted.auth).unwrap();
    assert_eq!(auth["results"][0]["pos"], 1);
    assert_eq!(auth["signatures"][0]["h"]["list-unsubscribe"], 2);
    assert_eq!(auth["from_domain"], "news.example.com");
}

#[test]
fn determinism() {
    let runner = Runner::new(vec![Arc::new(Digits)]);
    let message = parsed();
    let row = |extracted: Extracted| {
        extracted.into_row(
            MessageId::new(),
            ArchiveId::new(),
            runner.extract_fp(),
            Timestamp::from_millis(0),
        )
    };
    let first = row(runner.extract(&message));
    let second = row(runner.extract(&message));
    assert_eq!(first.items, second.items);
    assert_eq!(first.auth, second.auth);
    assert_eq!(
        (first.signals, first.kinds, first.truncated),
        (second.signals, second.kinds, second.truncated)
    );
    assert_eq!(Runner::input(&message), Runner::input(&message));
}

#[test]
fn input_is_normalised() {
    let (input, truncated) = Runner::input(&parsed());
    assert!(!truncated);
    assert!(input.text.contains("Your code is 482913."));
    assert!(input.html.is_some());
    assert_eq!(input.sent_at, Some(Timestamp::from_secs(1_793_872_800)));
    assert!(input.headers.iter().any(|h| h.name == "List-Unsubscribe"));
}

#[test]
fn input_caps() {
    let mut message = parsed();
    message.text = Some("1".repeat(MAX_TEXT_BYTES * 3));
    let (input, truncated) = Runner::input(&message);
    assert!(truncated);
    assert_eq!(input.text.len(), MAX_TEXT_BYTES);

    let mut message = parsed();
    message.html = Some(format!("<p>{}</p>", "word ".repeat(MAX_HTML_BYTES)));
    assert!(Runner::input(&message).1);

    let pkpass = |size: usize, name: &str| Attachment {
        filename: Some(name.into()),
        content_type: "application/octet-stream".into(),
        content_id: None,
        inline: false,
        content: Bytes::from(vec![0u8; size]),
    };
    let mut message = parsed();
    message.attachments = vec![
        pkpass(10, "invoice.pdf"),
        pkpass(MAX_PKPASS_BYTES + 1, "big.PKPASS"),
    ];
    message
        .attachments
        .extend((0..MAX_PKPASS_ATTACHMENTS + 1).map(|_| pkpass(10, "pass.pkpass")));
    let (input, truncated) = Runner::input(&message);
    assert!(truncated);
    let with_content: Vec<u16> = input
        .attachments
        .iter()
        .filter(|a| a.content.is_some())
        .map(|a| a.ordinal)
        .collect();
    assert_eq!(
        with_content,
        (2..2 + MAX_PKPASS_ATTACHMENTS as u16).collect::<Vec<_>>()
    );
    assert_eq!(input.attachments[1].size, (MAX_PKPASS_BYTES + 1) as u64);
    assert_eq!(input.attachments.len(), MAX_PKPASS_ATTACHMENTS + 3);
}

#[test]
fn each_extractor_gives_at_most_sixteen() {
    let many = (0..40)
        .map(|n| code(&format!("{:06}", 100_000 + n)))
        .collect();
    let runner = Runner::new(vec![fixed("many", many)]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert_eq!(extracted.items.items.len(), MAX_ITEMS_PER_EXTRACTOR);
    assert!(!extracted.truncated);
}

#[test]
fn row_caps_and_drop_order() {
    let tier = |id: &'static str, confidence: Confidence, provenance: Provenance, forwarded| {
        let items = (0..16)
            .map(|n| Insight {
                confidence,
                provenance,
                forwarded,
                score: 100 + n,
                ..code(&format!("{id}{n:03}"))
            })
            .collect();
        fixed(id, items)
    };
    let runner = Runner::new(vec![
        tier("pos", Confidence::Possible, Provenance::Heuristic, false),
        tier("fwd", Confidence::Likely, Provenance::Heuristic, true),
        tier("heu", Confidence::Likely, Provenance::Heuristic, false),
        tier("str", Confidence::Likely, Provenance::JsonLd, true),
    ]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert!(extracted.truncated);
    let kept = values(&extracted);
    assert_eq!(kept.len(), MAX_ITEMS);
    assert!(
        kept.iter()
            .all(|v| v.starts_with("str") || v.starts_with("heu"))
    );
    assert!(
        kept.first().unwrap().starts_with("heu"),
        "original order kept"
    );

    let runner = Runner::new(vec![
        tier("low", Confidence::Likely, Provenance::Heuristic, false),
        fixed(
            "big",
            (0..16)
                .map(|n| Insight {
                    score: n,
                    ..action(&format!("{n}"), "ж".repeat(500))
                })
                .collect(),
        ),
    ]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert!(extracted.truncated);
    assert!(extracted.items.to_json().len() <= MAX_ITEMS_JSON_BYTES);
    let kept = values(&extracted);
    assert_eq!(kept.iter().filter(|v| v.starts_with("low")).count(), 16);
    let actions: Vec<_> = kept.iter().filter(|v| v.starts_with("https")).collect();
    assert!(!actions.is_empty() && actions.len() < 16);
    assert!(
        actions.contains(&&"https://example.com/15".to_owned()),
        "higher scores survive"
    );
    assert!(!actions.contains(&&"https://example.com/0".to_owned()));
}

#[test]
fn later_reservation_legs_go_first() {
    let leg = |n: u32| {
        let departure = chrono_like(n);
        Insight {
            provenance: Provenance::JsonLd,
            confidence: Confidence::Certain,
            score: 1000,
            origin: PartRef::Html,
            ..item(InsightKind::Reservation(Reservation::Flight(
                FlightReservation {
                    reservation_number: Some(format!("RES{n:03}")),
                    carrier: Some("XA".into()),
                    flight_number: Some(format!("{n}")),
                    departure_airport: "AAA".into(),
                    arrival_airport: "BBB".into(),
                    departure,
                    arrival: None,
                    passengers: Vec::new(),
                    seats: Vec::new(),
                    url: None,
                },
            )))
        }
    };
    let runner = Runner::new(
        (0..3)
            .map(|e| {
                fixed(
                    ["a", "b", "c"][e],
                    (0..12).map(|n| leg(e as u32 * 12 + n)).collect(),
                )
            })
            .collect(),
    );
    let extracted = runner.extract_input(&InsightInput::default(), false);
    let numbers: Vec<String> = extracted
        .items
        .items
        .iter()
        .map(|item| match &item.kind {
            InsightKind::Reservation(Reservation::Flight(f)) => f.flight_number.clone().unwrap(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(numbers, (0..32).map(|n| n.to_string()).collect::<Vec<_>>());
    assert!(extracted.signals.contains(Signal::Structured));
}

fn chrono_like(n: u32) -> LocalOrZoned {
    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 1 + (n % 28)).unwrap();
    LocalOrZoned::Local(day.and_hms_opt(9, 0, 0).unwrap())
}

#[test]
fn merge_dedups_and_drops_cross_duplicates() {
    let order = item(InsightKind::Order {
        number: Some("A-48291".into()),
        merchant: None,
        total: None,
        status: None,
        url: None,
    });
    let structured = Insight {
        provenance: Provenance::JsonLd,
        confidence: Confidence::Certain,
        ..action("open", "Open".into())
    };
    let runner = Runner::new(vec![
        fixed("orders", vec![order, structured]),
        fixed(
            "codes",
            vec![code("a48291"), code("771100"), code("771100")],
        ),
        fixed(
            "links",
            vec![
                action("open", "Open".into()),
                action_of(
                    LinkAction::Track,
                    "https://example.com/other",
                    "Other".into(),
                ),
            ],
        ),
    ]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert_eq!(
        values(&extracted),
        [
            "order",
            "https://example.com/open",
            "771100",
            "https://example.com/other"
        ]
    );
}

fn structured(kind: LinkAction, url: &str) -> Insight {
    Insight {
        provenance: Provenance::JsonLd,
        confidence: Confidence::Certain,
        ..action_of(kind, url, "Structured".into())
    }
}

#[test]
fn a_heuristic_action_yields_to_a_structured_one() {
    let heuristic = || {
        fixed(
            "links",
            vec![
                action_of(
                    LinkAction::ViewOrder,
                    "https://shop.example.com/orders/1",
                    "View order".into(),
                ),
                action_of(
                    LinkAction::Track,
                    "https://www.google.com/url?q=https%3A%2F%2Fparcels.example.org%2Ft%2F9",
                    "Track".into(),
                ),
                action_of(
                    LinkAction::Verify,
                    "https://accounts.example.com/verify",
                    "Verify".into(),
                ),
            ],
        )
    };
    let runner = |structured_items: Vec<Insight>| {
        Runner::new(vec![fixed("schemaorg", structured_items), heuristic()])
            .extract_input(&InsightInput::default(), false)
    };
    let both = runner(vec![
        structured(LinkAction::ViewOrder, "https://shop.example.com/o?id=1"),
        structured(LinkAction::CheckIn, "https://parcels.example.org/t/9"),
    ]);
    assert_eq!(
        values(&both),
        [
            "https://shop.example.com/o?id=1",
            "https://parcels.example.org/t/9",
            "https://accounts.example.com/verify",
        ]
    );
    let none = runner(Vec::new());
    assert_eq!(none.items.items.len(), 3);
    let another = Insight {
        provenance: Provenance::Heuristic,
        ..structured(LinkAction::Verify, "https://accounts.example.com/other")
    };
    assert_eq!(runner(vec![another]).items.items.len(), 4);
}

#[test]
fn forwarded_structured_items_drop_only_forwarded_ones() {
    let quoted = Insight {
        forwarded: true,
        ..structured(LinkAction::Confirm, "https://news.example.org/confirm/q7v3")
    };
    let order = Insight {
        forwarded: true,
        ..item(InsightKind::Order {
            number: Some("A-48291".into()),
            merchant: None,
            total: None,
            status: None,
            url: None,
        })
    };
    let own_confirm = action_of(
        LinkAction::Confirm,
        "https://accounts.example.com/confirm",
        "Confirm".into(),
    );
    let quoted_confirm = Insight {
        forwarded: true,
        ..action_of(
            LinkAction::Confirm,
            "https://news.example.org/confirm/old",
            "Confirm".into(),
        )
    };
    let quoted_code = Insight {
        forwarded: true,
        ..code("A48291")
    };
    let runner = Runner::new(vec![
        fixed("schemaorg", vec![quoted, order]),
        fixed("links", vec![own_confirm, quoted_confirm]),
        fixed("codes", vec![code("a48291"), quoted_code]),
    ]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert_eq!(
        values(&extracted),
        [
            "https://news.example.org/confirm/q7v3",
            "order",
            "https://accounts.example.com/confirm",
            "a48291",
        ]
    );
}

#[test]
fn codes_equal_to_a_parcel_number_are_dropped() {
    let parcel = item(InsightKind::Parcel {
        carrier: holtmail_insight::CarrierId::new("ups").unwrap(),
        number: "1ZV77Y21K219503089".into(),
        check_digit: true,
        track_url: None,
    });
    let runner = Runner::new(vec![
        fixed("parcel", vec![parcel]),
        fixed("code", vec![code("1zv77y21k219503089"), code("482913")]),
    ]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert_eq!(values(&extracted), ["parcel", "482913"]);
}

fn action_fields(extracted: &Extracted) -> Vec<(String, Option<String>, bool)> {
    extracted
        .items
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            InsightKind::Action {
                destination,
                via,
                tracked,
                ..
            } => Some((destination.as_str().to_owned(), via.clone(), *tracked)),
            _ => None,
        })
        .collect()
}

#[test]
fn actions_are_unwrapped_and_tracked() {
    let long = format!(
        "https://www.google.com/url?q=https%3A%2F%2Fexample.com%2F{}",
        "a".repeat(2100)
    );
    let runner = Runner::new(vec![fixed(
        "links",
        vec![
            action_of(
                LinkAction::ResetPassword,
                "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Faccounts.example.com%2Freset%3Ft%3D1&data=x",
                "Reset".into(),
            ),
            action_of(
                LinkAction::Verify,
                "https://u1.ct.sendgrid.net/ls/click?upn=abc",
                "Verify".into(),
            ),
            action_of(
                LinkAction::SignIn,
                "https://safelinks.protection.outlook.com.evil.example/?url=https%3A%2F%2Faccounts.example.com",
                "Sign in".into(),
            ),
            action_of(LinkAction::Track, &long, "Track".into()),
        ],
    )]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert_eq!(
        action_fields(&extracted),
        [
            (
                "https://accounts.example.com/reset?t=1".to_owned(),
                Some("outlook.com".to_owned()),
                false
            ),
            (
                "https://u1.ct.sendgrid.net/ls/click?upn=abc".to_owned(),
                None,
                true
            ),
            (
                "https://safelinks.protection.outlook.com.evil.example/?url=https%3A%2F%2Faccounts.example.com".to_owned(),
                None,
                false
            ),
        ]
    );
    match &extracted.items.items[0].kind {
        InsightKind::Action { url, .. } => assert!(url.as_str().contains("safelinks")),
        _ => unreachable!(),
    }
}

#[test]
fn validation_drops_and_trims() {
    let mut unsub_mailto = item(InsightKind::Unsubscribe {
        https: None,
        mailto: Some("https://example.com/u".into()),
        http: None,
        one_click: false,
        list_id: None,
    });
    unsub_mailto.provenance = Provenance::Header;
    let one_click_http = item(InsightKind::Unsubscribe {
        https: None,
        mailto: None,
        http: Some(Url::parse("http://example.com/u").unwrap()),
        one_click: true,
        list_id: None,
    });
    let long = code(&"1".repeat(65));
    let mut certain = code("123456");
    certain.confidence = Confidence::Certain;
    certain.score = 3;
    let mut over = code("654321");
    over.score = 5000;
    let pass_without_attachment = item(InsightKind::Pass {
        style: holtmail_insight::PassStyle::Generic,
        organization: "Example".into(),
        description: "Card".into(),
        relevant: None,
        boarding: None,
        attachment: 3,
        signature_checked: false,
    });
    let backwards = item(InsightKind::Reservation(Reservation::Flight(
        FlightReservation {
            reservation_number: Some("XK4P2Q".into()),
            carrier: None,
            flight_number: None,
            departure_airport: "AAA".into(),
            arrival_airport: "BBB".into(),
            departure: chrono_like(5),
            arrival: Some(chrono_like(2)),
            passengers: Vec::new(),
            seats: Vec::new(),
            url: None,
        },
    )));
    let runner = Runner::new(vec![fixed(
        "mixed",
        vec![
            unsub_mailto,
            one_click_http,
            long,
            certain,
            over,
            pass_without_attachment,
            backwards,
            action("label", "y".repeat(2000)),
        ],
    )]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    let items = &extracted.items.items;
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].score, 1000);
    assert_eq!(items[1].score, 1000);
    match &items[2].kind {
        InsightKind::Action { label, .. } => {
            assert_eq!(label.as_ref().unwrap().chars().count(), 512)
        }
        _ => unreachable!(),
    }
}

#[test]
fn kinds_skip_possible_and_forwarded() {
    let possible = Insight {
        confidence: Confidence::Possible,
        ..code("111111")
    };
    let forwarded = Insight {
        forwarded: true,
        ..action("fwd", "Open".into())
    };
    let runner = Runner::new(vec![fixed("k", vec![possible, forwarded])]);
    let extracted = runner.extract_input(&InsightInput::default(), false);
    assert_eq!(extracted.items.items.len(), 2);
    assert!(extracted.kinds.is_empty());
    assert!(!extracted.signals.contains(Signal::Structured));
}

#[test]
fn a_panicking_extractor_costs_only_its_items() {
    let input = InsightInput {
        text: "é".into(),
        ..InsightInput::default()
    };
    let runner = Runner::new(vec![
        fixed("before", vec![code("111222")]),
        Arc::new(Panics),
        fixed("after", vec![code("333444")]),
    ]);
    let (extracted, _) = scope::capture(|| runner.extract_input(&input, false));
    assert_eq!(extracted.outcome, InsightOutcome::Partial);
    assert_eq!(values(&extracted), ["111222", "333444"]);
}

#[test]
fn panic_reports_never_carry_content() {
    install_panic_hook();
    install_panic_hook();
    let secret = "é secret-token-48213";
    let input = InsightInput {
        text: secret.into(),
        ..InsightInput::default()
    };

    let payload = catch_unwind(AssertUnwindSafe(|| Panics.extract(&input))).unwrap_err();
    let payload = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(payload.contains("secret-token-48213"), "the probe leaks");

    let runner = Runner::new(vec![Arc::new(Panics)]);
    let message = MessageId::new();
    let (extracted, lines) =
        scope::capture(|| with_message(message, || runner.extract_input(&input, false)));
    assert_eq!(extracted.outcome, InsightOutcome::Partial);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(!line.contains("secret"), "{line}");
    assert!(!line.contains('é'), "{line}");
    assert!(line.contains("extractor=boom version=7"), "{line}");
    assert!(line.contains(&format!("message={message}")), "{line}");
    assert!(line.contains("src/tests.rs:"), "{line}");

    let (_, outside) = scope::capture(|| {
        let _ = catch_unwind(|| panic!("outside any extraction"));
    });
    assert!(outside.is_empty());
}

#[test]
fn panics_unwind() {
    assert!(catch_unwind(|| panic!("unwinds")).is_err());
    let runner = Runner::new(vec![Arc::new(Panics)]);
    let input = InsightInput {
        text: "é".into(),
        ..InsightInput::default()
    };
    let (extracted, _) = scope::capture(|| runner.extract_input(&input, false));
    assert_eq!(extracted.outcome, InsightOutcome::Partial);
}

#[test]
fn no_profile_aborts_on_panic() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    let manifest: toml::Table = std::fs::read_to_string(root).unwrap().parse().unwrap();
    fn walk(path: &str, value: &toml::Value) {
        if let toml::Value::Table(table) = value {
            for (key, value) in table {
                let path = format!("{path}.{key}");
                assert!(
                    !(key == "panic" && value.as_str() == Some("abort")),
                    "{path} sets panic = \"abort\"; extraction relies on unwinding"
                );
                walk(&path, value);
            }
        }
    }
    if let Some(profile) = manifest.get("profile") {
        walk("profile", profile);
    }
}

#[test]
fn fingerprint_follows_versions_and_order() {
    let a = || fixed("a", Vec::new());
    let b = || {
        Arc::new(Fixed {
            id: "b",
            version: "2",
            items: Vec::new(),
        }) as Arc<dyn InsightExtractor>
    };
    let fp = Runner::new(vec![a(), b()]).extract_fp();
    assert_eq!(fp.len(), 64);
    assert_eq!(fp, Runner::new(vec![a(), b()]).extract_fp());
    assert_ne!(fp, Runner::new(vec![b(), a()]).extract_fp());
    assert_ne!(fp, Runner::new(vec![a()]).extract_fp());
    let bumped = Arc::new(Fixed {
        id: "b",
        version: "3",
        items: Vec::new(),
    });
    assert_ne!(fp, Runner::new(vec![a(), bumped]).extract_fp());
}

#[test]
fn hostile_corpus_stays_bounded() {
    let runner = Runner::new(vec![Arc::new(Digits), fixed("fixed", vec![code("123456")])]);
    for case in hostile_corpus().into_iter().chain(personal_mail()) {
        let first = runner.extract_input(&case.input, false);
        let second = runner.extract_input(&case.input, false);
        assert_eq!(first, second, "{}", case.name);
        assert_eq!(first.outcome, InsightOutcome::Ok, "{}", case.name);
        assert!(first.items.items.len() <= MAX_ITEMS, "{}", case.name);
        let json = first.items.to_json();
        assert!(json.len() <= MAX_ITEMS_JSON_BYTES, "{}", case.name);
        assert_eq!(
            Items::from_json(&json).unwrap().items,
            first.items,
            "{}",
            case.name
        );
        assert!(serde_json::from_str::<serde_json::Value>(&first.auth).is_ok());
    }
}

#[test]
fn hostile_messages_stay_bounded() {
    let mut message = parsed();
    message.text = Some(format!(
        "{}{}",
        "\u{301}".repeat(3_000_000),
        "9".repeat(10_000_000)
    ));
    message.html = Some("<div>".repeat(600_000));
    message
        .headers
        .extend((0..2_000).map(|n| holtmail_mime::HeaderField {
            name: "DKIM-Signature".into(),
            value: format!("v=1; d=junk{n}.example; s=s; h=from; b=x"),
        }));
    let runner = Runner::new(vec![Arc::new(Digits)]);
    let extracted = runner.extract(&message);
    assert!(extracted.truncated);
    let auth: serde_json::Value = serde_json::from_str(&extracted.auth).unwrap();
    assert_eq!(auth["signatures"].as_array().unwrap().len(), 8);
    assert!(extracted.auth.len() < 4096);
}

fn at(day: u32, hour: u32) -> LocalOrZoned {
    LocalOrZoned::Local(
        chrono::NaiveDate::from_ymd_opt(2027, 3, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap(),
    )
}

fn zoned(text: &str) -> LocalOrZoned {
    LocalOrZoned::Zoned(chrono::DateTime::parse_from_rfc3339(text).unwrap(), None)
}

fn of(provenance: Provenance, kind: InsightKind) -> Insight {
    Insight {
        provenance,
        confidence: if provenance == Provenance::Heuristic {
            Confidence::Likely
        } else {
            Confidence::Certain
        },
        origin: PartRef::Html,
        ..item(kind)
    }
}

fn flight(number: Option<&str>, reservation: Option<&str>, departure: LocalOrZoned) -> InsightKind {
    InsightKind::Reservation(Reservation::Flight(FlightReservation {
        reservation_number: reservation.map(str::to_owned),
        carrier: Some("XA".into()),
        flight_number: number.map(str::to_owned),
        departure_airport: "FRA".into(),
        arrival_airport: "LIS".into(),
        departure,
        arrival: None,
        passengers: Vec::new(),
        seats: Vec::new(),
        url: None,
    }))
}

fn parcel_kind(carrier: &str, number: &str, check_digit: bool, track: Option<&str>) -> InsightKind {
    InsightKind::Parcel {
        carrier: holtmail_insight::CarrierId::new(carrier).unwrap(),
        number: number.into(),
        check_digit,
        track_url: track.map(|u| Url::parse(u).unwrap()),
    }
}

fn run(items: Vec<Vec<Insight>>) -> Vec<Insight> {
    run_with(&InsightInput::default(), items)
}

fn run_with(input: &InsightInput, items: Vec<Vec<Insight>>) -> Vec<Insight> {
    let ids = ["a", "b", "c", "d", "e", "f"];
    let extractors = items
        .into_iter()
        .enumerate()
        .map(|(n, items)| fixed(ids[n], items))
        .collect();
    Runner::new(extractors)
        .extract_input(input, false)
        .items
        .items
}

#[test]
fn merge_keys_join_items_of_one_fact() {
    use holtmail_insight::{
        BusReservation, EventReservation, FoodReservation, LodgingReservation,
        RentalCarReservation, TrainReservation,
    };
    let lodging = |name: &str, checkin| {
        InsightKind::Reservation(Reservation::Lodging(LodgingReservation {
            reservation_number: "HX-4821".into(),
            name: name.into(),
            address: None,
            checkin,
            checkout: None,
            guests: Vec::new(),
            url: None,
        }))
    };
    let train = |number: Option<&str>, station: &str, departure| {
        InsightKind::Reservation(Reservation::Train(TrainReservation {
            reservation_number: Some("TR9921".into()),
            train_number: number.map(str::to_owned),
            departure_station: station.into(),
            arrival_station: None,
            departure,
            arrival: None,
            passengers: Vec::new(),
            seats: Vec::new(),
            url: None,
        }))
    };
    let bus = |number: &str, departure| {
        InsightKind::Reservation(Reservation::Bus(BusReservation {
            reservation_number: None,
            bus_number: Some(number.into()),
            departure_station: "Example Central".into(),
            arrival_station: None,
            departure,
            arrival: None,
            passengers: Vec::new(),
            seats: Vec::new(),
            url: None,
        }))
    };
    let event = |name: &str, start| {
        InsightKind::Reservation(Reservation::Event(EventReservation {
            reservation_number: None,
            name: name.into(),
            start,
            end: None,
            venue: None,
            tickets: Vec::new(),
            url: None,
        }))
    };
    let food = |name: &str, start| {
        InsightKind::Reservation(Reservation::Food(FoodReservation {
            reservation_number: None,
            name: name.into(),
            start,
            party_size: None,
            url: None,
        }))
    };
    let car = |number: &str, pickup| {
        InsightKind::Reservation(Reservation::RentalCar(RentalCarReservation {
            reservation_number: number.into(),
            company: None,
            pickup,
            pickup_location: None,
            dropoff: None,
            dropoff_location: None,
            url: None,
        }))
    };
    let order = |number: Option<&str>, merchant: Option<&str>, status: &str| InsightKind::Order {
        number: number.map(str::to_owned),
        merchant: merchant.map(str::to_owned),
        total: None,
        status: Some(status.into()),
        url: None,
    };
    let invoice = |number: Option<&str>, payee: Option<&str>, day: u32| InsightKind::Invoice {
        number: number.map(str::to_owned),
        payee: payee.map(str::to_owned),
        total: None,
        due: Some(holtmail_insight::Date(
            chrono::NaiveDate::from_ymd_opt(2027, 3, day).unwrap(),
        )),
        url: None,
    };
    let pass = |attachment: u16, description: &str| InsightKind::Pass {
        style: holtmail_insight::PassStyle::Generic,
        organization: "Example Air".into(),
        description: description.into(),
        relevant: None,
        boarding: None,
        attachment,
        signature_checked: false,
    };

    let same: Vec<(&str, InsightKind, InsightKind)> = vec![
        (
            "flight",
            flight(Some("482"), Some("ABC123"), at(14, 9)),
            flight(Some("482"), Some("XYZ999"), at(14, 18)),
        ),
        (
            "flight in its own offset",
            flight(Some("482"), None, zoned("2027-03-14T23:30:00+01:00")),
            flight(Some("482"), None, zoned("2027-03-14T09:30:00+01:00")),
        ),
        (
            "flight without a number",
            flight(None, Some("ABC123"), at(14, 9)),
            flight(None, Some("abc-123"), at(14, 10)),
        ),
        (
            "lodging",
            lodging("Example Inn", at(14, 15)),
            lodging("Example Inn Lisbon", at(14, 16)),
        ),
        (
            "train",
            train(Some("ICE 512"), "Example Hbf", at(14, 8)),
            train(Some("ice512"), "example  hbf", at(14, 11)),
        ),
        (
            "train by reservation",
            train(None, "Example Hbf", at(14, 8)),
            train(None, "Example Hbf", at(14, 9)),
        ),
        ("bus", bus("77", at(14, 8)), bus("77", at(14, 20))),
        (
            "event",
            event("Example Quartet", zoned("2027-03-14T20:00:00+01:00")),
            event("example quartet", zoned("2027-03-14T19:00:00Z")),
        ),
        (
            "food",
            food("Example Bistro", at(14, 19)),
            food("Example  Bistro", at(14, 19)),
        ),
        ("rental car", car("RC-1", at(14, 9)), car("rc1", at(14, 12))),
        (
            "order",
            order(Some("A-48291"), Some("Example Shop"), "OrderProcessing"),
            order(Some("a48291"), None, "OrderShipped"),
        ),
        (
            "order by seller",
            order(None, Some("Example Shop"), "OrderProcessing"),
            order(None, Some("example shop"), "OrderShipped"),
        ),
        (
            "invoice",
            invoice(Some("INV-7"), Some("Example Power"), 20),
            invoice(Some("inv7"), None, 21),
        ),
        (
            "invoice by payee",
            invoice(None, Some("Example Power"), 20),
            invoice(None, Some("Example Power"), 21),
        ),
        (
            "parcel",
            parcel_kind("ups", "1ZV77Y21K219503089", false, None),
            parcel_kind("ups", "1zv77y21k219503089", false, None),
        ),
        ("pass", pass(0, "Boarding pass"), pass(0, "Ticket")),
    ];
    let input = InsightInput {
        attachments: vec![holtmail_insight::AttachmentMeta {
            ordinal: 0,
            filename: Some("pass.pkpass".into()),
            content_type: "application/vnd.apple.pkpass".into(),
            size: 10,
            content: None,
        }],
        ..InsightInput::default()
    };
    for (name, first, second) in same {
        let kept = run_with(
            &input,
            vec![
                vec![of(Provenance::Microdata, second.clone())],
                vec![of(Provenance::JsonLd, first.clone())],
            ],
        );
        assert_eq!(kept.len(), 1, "{name}: {kept:?}");
        assert_eq!(kept[0].kind, first, "{name}: the JSON-LD item wins");
        assert_eq!(kept[0].provenance, Provenance::JsonLd, "{name}");
    }

    let apart: Vec<(&str, InsightKind, InsightKind)> = vec![
        (
            "another flight number",
            flight(Some("482"), None, at(14, 9)),
            flight(Some("483"), None, at(14, 9)),
        ),
        (
            "another day",
            flight(Some("482"), None, at(14, 9)),
            flight(Some("482"), None, at(15, 9)),
        ),
        (
            "another check-in",
            lodging("Example Inn", at(14, 15)),
            lodging("Example Inn", at(16, 15)),
        ),
        (
            "another station",
            train(Some("512"), "Example Hbf", at(14, 8)),
            train(Some("512"), "Example Süd", at(14, 8)),
        ),
        (
            "another start",
            event("Example Quartet", at(14, 20)),
            event("Example Quartet", at(14, 21)),
        ),
        (
            "a local and a zoned start",
            food("Example Bistro", at(14, 19)),
            food("Example Bistro", zoned("2027-03-14T19:00:00Z")),
        ),
        (
            "another order",
            order(Some("A-1"), Some("Example Shop"), "OrderProcessing"),
            order(Some("A-2"), Some("Example Shop"), "OrderProcessing"),
        ),
        (
            "another carrier",
            parcel_kind("ups", "1ZV77Y21K219503089", false, None),
            parcel_kind("fedex", "1ZV77Y21K219503089", false, None),
        ),
        ("another attachment", pass(0, "One"), pass(1, "One")),
    ];
    let two = InsightInput {
        attachments: vec![
            input.attachments[0].clone(),
            holtmail_insight::AttachmentMeta {
                ordinal: 1,
                ..input.attachments[0].clone()
            },
        ],
        ..InsightInput::default()
    };
    for (name, first, second) in apart {
        let kept = run_with(
            &two,
            vec![vec![
                of(Provenance::JsonLd, first),
                of(Provenance::JsonLd, second),
            ]],
        );
        assert_eq!(kept.len(), 2, "{name}: {kept:?}");
    }
}

#[test]
fn structured_provenance_wins_a_merge() {
    let number = "1ZV77Y21K219503089";
    let heuristic = of(
        Provenance::Heuristic,
        parcel_kind("ups", number, true, Some("https://www.ups.example.com/t/1")),
    );
    let structured = |provenance| of(provenance, parcel_kind("ups", number, false, None));
    let order = [
        Provenance::JsonLd,
        Provenance::Microdata,
        Provenance::PkPass,
        Provenance::Heuristic,
    ];
    for (n, &winner) in order.iter().enumerate() {
        for &loser in &order[n + 1..] {
            for flipped in [false, true] {
                let a = if winner == Provenance::Heuristic {
                    heuristic.clone()
                } else {
                    structured(winner)
                };
                let b = if loser == Provenance::Heuristic {
                    heuristic.clone()
                } else {
                    structured(loser)
                };
                let lists = if flipped {
                    vec![vec![b], vec![a]]
                } else {
                    vec![vec![a], vec![b]]
                };
                let kept = run(lists);
                assert_eq!(kept.len(), 1, "{winner:?} over {loser:?}");
                assert_eq!(kept[0].provenance, winner, "{winner:?} over {loser:?}");
            }
        }
    }

    let kept = run(vec![
        vec![structured(Provenance::JsonLd)],
        vec![heuristic.clone()],
    ]);
    let InsightKind::Parcel {
        check_digit,
        track_url,
        ..
    } = &kept[0].kind
    else {
        unreachable!()
    };
    assert!(*check_digit, "the heuristic check digit is kept");
    assert_eq!(
        track_url.as_ref().map(Url::as_str),
        Some("https://www.ups.example.com/t/1")
    );

    let forwarded = Insight {
        forwarded: true,
        ..structured(Provenance::JsonLd)
    };
    let kept = run(vec![vec![forwarded.clone()], vec![heuristic.clone()]]);
    assert!(!kept[0].forwarded, "forwarded only when every part was");
    let kept = run(vec![
        vec![forwarded.clone()],
        vec![Insight {
            forwarded: true,
            ..heuristic.clone()
        }],
    ]);
    assert!(kept[0].forwarded);

    let weak = Insight {
        confidence: Confidence::Likely,
        score: 400,
        ..heuristic.clone()
    };
    let strong = Insight {
        score: 900,
        ..heuristic.clone()
    };
    let kept = run(vec![vec![weak.clone()], vec![strong.clone()]]);
    assert_eq!(kept, std::slice::from_ref(&strong), "then the higher score");
    let tie = Insight {
        origin: PartRef::Text,
        ..strong.clone()
    };
    let other = Insight {
        origin: PartRef::Subject,
        ..strong
    };
    let kept = run(vec![vec![tie.clone()], vec![other]]);
    assert_eq!(kept, [tie], "then the earlier one");

    let codes = run(vec![
        vec![structured(Provenance::JsonLd)],
        vec![heuristic],
        vec![code("1ZV77Y21K219503089"), code("482913")],
    ]);
    assert_eq!(
        codes.len(),
        2,
        "the code equal to the parcel number is dropped"
    );
}

#[test]
fn iata_codes_are_normalised() {
    let lower = |departure: &str, arrival: &str, carrier: Option<&str>| {
        let mut kind = flight(Some("482"), Some("ABC123"), at(14, 9));
        if let InsightKind::Reservation(Reservation::Flight(f)) = &mut kind {
            f.departure_airport = departure.into();
            f.arrival_airport = arrival.into();
            f.carrier = carrier.map(str::to_owned);
        }
        of(Provenance::JsonLd, kind)
    };
    let kept = run(vec![vec![
        lower("fra", " lis ", Some("xa")),
        lower("FRAN", "LIS", Some("XA")),
        lower("FR", "LIS", Some("XA")),
        lower("F1A", "LIS", Some("XA")),
        lower("ФРА", "LIS", Some("XA")),
    ]]);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let InsightKind::Reservation(Reservation::Flight(f)) = &kept[0].kind else {
        unreachable!()
    };
    assert_eq!(
        (f.departure_airport.as_str(), f.arrival_airport.as_str()),
        ("FRA", "LIS")
    );
    assert_eq!(f.carrier.as_deref(), Some("XA"));

    let kept = run(vec![vec![lower("fra", "lis", Some("XAB"))]]);
    let InsightKind::Reservation(Reservation::Flight(f)) = &kept[0].kind else {
        unreachable!()
    };
    assert_eq!(
        f.carrier, None,
        "a carrier that is no IATA code is left out"
    );

    let leg = |from: &str, to: &str| holtmail_insight::BoardingLeg {
        from: from.into(),
        to: to.into(),
        carrier: Some("xa".into()),
        flight: Some("482".into()),
        date: None,
        seat: None,
    };
    let input = InsightInput {
        attachments: vec![holtmail_insight::AttachmentMeta {
            ordinal: 0,
            filename: Some("pass.pkpass".into()),
            content_type: "application/vnd.apple.pkpass".into(),
            size: 10,
            content: None,
        }],
        ..InsightInput::default()
    };
    let pass = of(
        Provenance::PkPass,
        InsightKind::Pass {
            style: holtmail_insight::PassStyle::BoardingPass,
            organization: "Example Air".into(),
            description: "Boarding pass".into(),
            relevant: None,
            boarding: Some(holtmail_insight::BoardingPass {
                pnr: None,
                passenger: None,
                legs: vec![leg("fra", "lis"), leg("FRA", "LISBON")],
            }),
            attachment: 0,
            signature_checked: false,
        },
    );
    let kept = run_with(&input, vec![vec![pass]]);
    let InsightKind::Pass {
        boarding: Some(boarding),
        ..
    } = &kept[0].kind
    else {
        unreachable!()
    };
    assert_eq!(
        boarding.legs,
        [holtmail_insight::BoardingLeg {
            from: "FRA".into(),
            to: "LIS".into(),
            carrier: Some("XA".into()),
            ..leg("", "")
        }]
    );
}

#[test]
fn rows_of_the_previous_version_still_read() {
    let previous = r#"{"v":1,"items":[{"type":"code","value":"482913","origin_bound":null,"p":"heuristic","c":"likely","score":800,"origin":"text","forwarded":false},{"type":"parcel","carrier":"ups","number":"1ZV77Y21K219503089","check_digit":true,"track_url":null,"p":"heuristic","c":"likely","score":900,"origin":"html","forwarded":false},{"type":"action","action":"track","url":"https://parcels.example.org/t/9","destination":"https://parcels.example.org/t/9","via":null,"tracked":false,"text_host":null,"label":"Track","p":"heuristic","c":"likely","score":750,"origin":"html","forwarded":false}]}"#;
    let read = Items::from_json(previous).unwrap();
    assert!(!read.outdated());
    assert_eq!(read.items.items.len(), 3);
    assert_eq!(read.items.to_json(), previous);

    let structured = Items {
        items: run(vec![vec![of(
            Provenance::JsonLd,
            flight(
                Some("482"),
                Some("ABC123"),
                zoned("2027-03-14T09:30:00+01:00"),
            ),
        )]]),
    };
    let read = Items::from_json(&structured.to_json()).unwrap();
    assert!(!read.outdated());
    assert_eq!(read.items, structured);
}
