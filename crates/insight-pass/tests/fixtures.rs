//! The §15 fixtures, built in-test from invented `pass.json` files: field by field, through the
//! explanation, and as `.eml` messages through `holtmail_mime` and the runner.

mod common;

use std::sync::Arc;

use base64::Engine;
use chrono::{NaiveDate, NaiveDateTime};
use holtmail_insight::{
    BoardingLeg, BoardingPass, Confidence, Date, InsightExtractor, InsightKind, KindTag,
    LocalOrZoned, PartRef, PassDate, PassStyle, Provenance, Signal,
};
use holtmail_insight_pass::PassExtractor;
use holtmail_insights::Runner;

use common::{boarding_json, event_json, fixtures, input, lying_bomb, pkpass};

#[test]
fn every_fixture_gives_what_it_promises() {
    let extractor = PassExtractor::new();
    for (name, content, found) in fixtures() {
        let items = extractor.extract(&input(vec![content]));
        assert_eq!(items.len(), usize::from(found), "{name}");
        for item in &items {
            assert_eq!(item.provenance, Provenance::PkPass, "{name}");
            assert_eq!(item.confidence, Confidence::Certain, "{name}");
            assert_eq!(item.score, 1000, "{name}");
            assert_eq!(item.origin, PartRef::Attachment(0), "{name}");
            assert!(!item.forwarded, "{name}");
        }
    }
}

#[test]
fn boarding_pass_reads_semantics() {
    let content = pkpass(&[("pass.json", boarding_json().as_bytes())]);
    let items = PassExtractor::new().extract(&input(vec![content]));
    let [item] = items.as_slice() else {
        panic!("expected one item, got {}", items.len());
    };
    let relevant = chrono::DateTime::parse_from_rfc3339("2026-11-20T07:15:00+01:00").unwrap();
    assert_eq!(
        item.kind,
        InsightKind::Pass {
            style: PassStyle::BoardingPass,
            organization: "Example Air".into(),
            description: "Boarding pass FRA to LIS".into(),
            relevant: Some(LocalOrZoned::Zoned(relevant, None)),
            boarding: Some(BoardingPass {
                pnr: Some("EXMPL01".into()),
                passenger: Some("Ada Example".into()),
                legs: vec![BoardingLeg {
                    from: "FRA".into(),
                    to: "LIS".into(),
                    carrier: Some("XA".into()),
                    flight: Some("123".into()),
                    date: Some(PassDate::Full(Date(
                        NaiveDate::from_ymd_opt(2026, 11, 20).unwrap()
                    ))),
                    seat: Some("14C".into()),
                }],
            }),
            attachment: 0,
            signature_checked: false,
        }
    );
}

#[test]
fn flight_code_fills_in_for_a_missing_number() {
    let json = common::edit(&boarding_json(), |map| {
        let semantics = map["semantics"].as_object_mut().unwrap();
        semantics.remove("flightNumber");
        semantics.remove("airlineCode");
        semantics.insert("flightCode".into(), "xa 0456".into());
        semantics.insert("departureAirportCode".into(), "lis".into());
    });
    let items =
        PassExtractor::new().extract(&input(vec![pkpass(&[("pass.json", json.as_bytes())])]));
    let InsightKind::Pass {
        boarding: Some(boarding),
        ..
    } = &items[0].kind
    else {
        panic!("no boarding");
    };
    assert_eq!(boarding.legs[0].from, "LIS");
    assert_eq!(boarding.legs[0].carrier.as_deref(), Some("XA"));
    assert_eq!(boarding.legs[0].flight.as_deref(), Some("0456"));
}

#[test]
fn boarding_needs_air_and_airports() {
    let cases = [
        common::edit(&boarding_json(), |map| {
            map["boardingPass"]["transitType"] = "PKTransitTypeTrain".into();
        }),
        common::edit(&boarding_json(), |map| {
            map["semantics"]["destinationAirportCode"] = "Lisbon".into();
        }),
        common::without(&boarding_json(), "semantics"),
    ];
    for json in cases {
        let items =
            PassExtractor::new().extract(&input(vec![pkpass(&[("pass.json", json.as_bytes())])]));
        let [item] = items.as_slice() else {
            panic!("expected one item");
        };
        let InsightKind::Pass {
            style, boarding, ..
        } = &item.kind
        else {
            panic!("not a pass");
        };
        assert_eq!(*style, PassStyle::BoardingPass);
        assert_eq!(*boarding, None);
    }
}

#[test]
fn event_ticket_keeps_local_time() {
    let content = pkpass(&[("pass.json", event_json().as_bytes())]);
    let items = PassExtractor::new().extract(&input(vec![content]));
    let InsightKind::Pass {
        style,
        organization,
        relevant,
        boarding,
        ..
    } = &items[0].kind
    else {
        panic!("not a pass");
    };
    assert_eq!(*style, PassStyle::EventTicket);
    assert_eq!(organization, "Example Hall");
    let local = NaiveDateTime::parse_from_str("2026-12-05T19:30", "%Y-%m-%dT%H:%M").unwrap();
    assert_eq!(*relevant, Some(LocalOrZoned::Local(local)));
    assert_eq!(*boarding, None);
}

#[test]
fn long_texts_are_cut_for_display() {
    let json = common::edit(&event_json(), |map| {
        map.insert("description".into(), "Ticket ".repeat(200).into());
    });
    let items =
        PassExtractor::new().extract(&input(vec![pkpass(&[("pass.json", json.as_bytes())])]));
    let InsightKind::Pass { description, .. } = &items[0].kind else {
        panic!("not a pass");
    };
    assert_eq!(
        description.chars().count(),
        holtmail_insight::limits::MAX_DISPLAY_CHARS
    );
    assert!(description.ends_with('…'));
}

#[test]
fn explanation_names_the_refusal() {
    let rules = |content: Vec<u8>| -> Vec<&'static str> {
        PassExtractor::new()
            .explain(&input(vec![content]))
            .into_iter()
            .map(|step| step.rule)
            .collect()
    };
    let bomb = rules(lying_bomb());
    assert!(
        bomb.contains(&"refused:pass_json_too_large") || bomb.contains(&"refused:read_failed"),
        "{bomb:?}"
    );
    let duplicate = rules(common::duplicate_pass_json(&boarding_json()));
    assert!(
        duplicate.contains(&"refused:duplicate_names"),
        "{duplicate:?}"
    );
    let honest = rules(common::honest_bomb());
    assert!(
        honest.contains(&"refused:uncompressed_too_large"),
        "{honest:?}"
    );
    let ok = rules(pkpass(&[("pass.json", boarding_json().as_bytes())]));
    for rule in [
        "attachment",
        "entries",
        "pass_json",
        "style:boarding_pass",
        "relevant_date",
        "semantics",
        "barcodes",
        "boarding",
        "chosen",
    ] {
        assert!(ok.contains(&rule), "{rule} missing from {ok:?}");
    }
    for (name, content, found) in fixtures() {
        if !found {
            let steps = rules(content);
            assert!(
                steps.iter().any(|rule| rule.starts_with("refused:")
                    || rule.ends_with("_missing")
                    || rule.starts_with("json_")),
                "{name}: {steps:?}"
            );
        }
    }
}

fn eml(pkpass: &[u8]) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(pkpass);
    let mut body = String::new();
    for line in encoded.as_bytes().chunks(76) {
        body.push_str(std::str::from_utf8(line).unwrap());
        body.push_str("\r\n");
    }
    format!(
        "From: Example Air <boarding@air.example.com>\r\n\
         To: Ada Example <ada@example.org>\r\n\
         Subject: Your boarding pass FRA to LIS\r\n\
         Date: Fri, 20 Nov 2026 05:00:00 +0000\r\n\
         Message-ID: <pkpass-boarding-air@example.com>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"pass-boundary\"\r\n\
         \r\n\
         --pass-boundary\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         Hi Ada, your boarding pass is attached. Have a good flight.\r\n\
         --pass-boundary\r\n\
         Content-Type: application/vnd.apple.pkpass\r\n\
         Content-Disposition: attachment; filename=\"boarding.pkpass\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {body}\
         --pass-boundary--\r\n"
    )
    .into_bytes()
}

#[test]
fn eml_runs_through_the_runner() {
    let runner = Runner::new(vec![Arc::new(PassExtractor::new())]);
    let raw = eml(&pkpass(&[("pass.json", boarding_json().as_bytes())]));
    let message = holtmail_mime::parse(&raw).unwrap();
    let (built, truncated) = Runner::input(&message);
    assert!(!truncated);
    assert_eq!(built.attachments.len(), 1);
    assert!(built.attachments[0].content.is_some());

    let extracted = runner.extract(&message);
    let [item] = extracted.items.items.as_slice() else {
        panic!("expected one item, got {}", extracted.items.items.len());
    };
    let InsightKind::Pass {
        boarding: Some(boarding),
        attachment,
        ..
    } = &item.kind
    else {
        panic!("no boarding pass");
    };
    assert_eq!(*attachment, 0);
    assert_eq!(boarding.pnr.as_deref(), Some("EXMPL01"));
    assert!(extracted.kinds.contains(KindTag::Pass));
    assert!(extracted.signals.contains(Signal::Structured));
    let report = runner.explain(&message);
    assert!(!report.contains("EXMPL01"));
    assert!(!report.contains("Ada Example"));
}

#[test]
fn eml_zip_bomb_gives_nothing() {
    let runner = Runner::new(vec![Arc::new(PassExtractor::new())]);
    let message = holtmail_mime::parse(&eml(&lying_bomb())).unwrap();
    assert!(runner.extract(&message).items.items.is_empty());
}
