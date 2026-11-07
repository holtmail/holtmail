use bytes::Bytes;
use holtmail_core::{Address, Timestamp};

use super::zip::{Entry, archive};
use super::{Case, Expect};
use crate::{
    Anchor, AttachmentMeta, HeaderField, HtmlView, InsightInput, MicrodataItem, MicrodataProperty,
    MicrodataValue, Segment,
};

const TEN_MB: usize = 10 * 1024 * 1024;
const PKPASS: &str = "application/vnd.apple.pkpass";

fn address(name: &str, email: &str) -> Address {
    Address {
        name: Some(name.into()),
        email: email.into(),
    }
}

fn personal_headers(subject: &str) -> Vec<HeaderField> {
    vec![
        HeaderField::new("Return-Path", "<ada@example.com>"),
        HeaderField::new("Message-ID", "<lunch-1@example.com>"),
        HeaderField::new("Date", "Fri, 06 Nov 2026 09:12:00 +0100"),
        HeaderField::new("From", "Ada Example <ada@example.com>"),
        HeaderField::new("To", "Bob Example <bob@example.org>"),
        HeaderField::new("Subject", subject),
        HeaderField::new("MIME-Version", "1.0"),
        HeaderField::new("Content-Type", "text/plain; charset=utf-8"),
    ]
}

fn segment(text: &str) -> Segment {
    Segment {
        text: text.into(),
        ..Segment::default()
    }
}

/// Plain personal mail, from which every extractor must return nothing.
pub fn personal_mail() -> Vec<Case> {
    let base = InsightInput {
        from: vec![address("Ada Example", "ada@example.com")],
        sent_at: Some(Timestamp::from_secs(1_793_952_720)),
        ..InsightInput::default()
    };
    let lunch = "Hi Bob,\n\nare we still on for lunch on Friday? I found a small place near \
                 the station, we could meet there at half past twelve.\n\nAda\n";
    let reply = "Sounds good, see you then.\n\nOn Thu, Ada Example wrote:\n> are we still on \
                 for lunch on Friday?\n";
    vec![
        Case::new(
            "personal-plain",
            InsightInput {
                headers: personal_headers("Lunch on Friday"),
                subject: Some("Lunch on Friday".into()),
                text: lunch.into(),
                ..base.clone()
            },
            Expect::Nothing,
        ),
        Case::new(
            "personal-html",
            InsightInput {
                headers: personal_headers("Photos from the hike"),
                subject: Some("Photos from the hike".into()),
                text: "Here are the photos from Sunday. The album is on my page.\n".into(),
                html: Some(
                    HtmlView::new()
                        .with_segments(vec![
                            segment("Here are the photos from Sunday."),
                            segment("The album is on"),
                            segment("my page"),
                            segment("."),
                        ])
                        .with_anchors(vec![Anchor {
                            href: "https://photos.example.org/ada/hike".into(),
                            text: "my page".into(),
                            ..Anchor::default()
                        }]),
                ),
                ..base.clone()
            },
            Expect::Nothing,
        ),
        Case::new(
            "personal-reply",
            InsightInput {
                headers: personal_headers("Re: Lunch on Friday"),
                from: vec![address("Bob Example", "bob@example.org")],
                subject: Some("Re: Lunch on Friday".into()),
                text: reply.into(),
                ..base
            },
            Expect::Nothing,
        ),
    ]
}

fn hostile(name: &str, input: InsightInput) -> Case {
    Case::new(name, input, Expect::Any)
}

fn trigger_headers() -> Vec<HeaderField> {
    vec![
        HeaderField::new("From", "Example Service <noreply@example.com>"),
        HeaderField::new("Subject", "Your verification code"),
        HeaderField::new("Auto-Submitted", "auto-generated"),
        HeaderField::new("List-Unsubscribe", "<https://example.com/u/1>"),
        HeaderField::new("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
    ]
}

fn trigger() -> InsightInput {
    InsightInput {
        headers: trigger_headers(),
        from: vec![address("Example Service", "noreply@example.com")],
        subject: Some("Your verification code".into()),
        sent_at: Some(Timestamp::from_secs(1_793_952_720)),
        ..InsightInput::default()
    }
}

fn multibyte_text(lead: usize) -> String {
    let mut text = "x".repeat(lead);
    for _ in 0..2_000 {
        text.push_str("é验😀1234567\u{301}\u{200d}\u{200f}code: ８７６５４３ 验证码是123456 ");
    }
    text
}

fn nested_json(depth: usize) -> String {
    let mut json = String::from(
        r#"{"@context":"https://schema.org","@type":"FlightReservation","reservationFor":"#,
    );
    for _ in 0..depth {
        json.push_str(r#"{"@type":"Flight","x":"#);
    }
    json.push_str("null");
    for _ in 0..depth {
        json.push('}');
    }
    json.push('}');
    json
}

fn pass_json() -> &'static [u8] {
    br#"{"formatVersion":1,"passTypeIdentifier":"pass.example.com.boarding","serialNumber":"X1","teamIdentifier":"EXAMPLE","organizationName":"Example Air","description":"Boarding pass","boardingPass":{"transitType":"PKTransitTypeAir"}}"#
}

fn pkpass(ordinal: u16, content: Vec<u8>) -> AttachmentMeta {
    AttachmentMeta {
        ordinal,
        filename: Some("pass.pkpass".into()),
        content_type: PKPASS.into(),
        size: content.len() as u64,
        content: Some(Bytes::from(content)),
    }
}

/// The shared hostile corpus: inputs far beyond every cap and shaped to break careless code.
///
/// An extractor must not panic on any of them and must keep every output rule. The inputs exceed
/// the runner's caps on purpose: the caps protect CPU and memory, not correctness.
pub fn hostile_corpus() -> Vec<Case> {
    let mut cases = Vec::new();

    let mut digits = String::from("Your verification code is ");
    digits.push_str(&"7".repeat(TEN_MB));
    cases.push(hostile(
        "one-token-10mb-digits",
        InsightInput {
            text: digits,
            ..trigger()
        },
    ));

    let mut alnum = String::from("code: ");
    alnum.push_str(&"a1".repeat(TEN_MB / 2));
    cases.push(hostile(
        "one-token-10mb-alnum-segment",
        InsightInput {
            html: Some(HtmlView::new().with_segments(vec![Segment {
                text: alnum,
                emphasis: true,
                alone_on_line: true,
                below_forward: false,
            }])),
            ..trigger()
        },
    ));

    let mut long_header = trigger_headers();
    long_header.push(HeaderField::new(
        "List-Unsubscribe",
        format!("<https://example.com/{}>", "u".repeat(TEN_MB)),
    ));
    long_header.push(HeaderField::new(
        "List-Unsubscribe",
        "<https://example.com/1>, ".repeat(100_000),
    ));
    long_header.push(HeaderField::new("List-Id", "x".repeat(TEN_MB)));
    long_header.push(HeaderField::new("Subject", "代码".repeat(TEN_MB / 6)));
    cases.push(hostile(
        "huge-header-values",
        InsightInput {
            headers: long_header,
            ..trigger()
        },
    ));

    let mut many_headers = trigger_headers();
    for i in 0..100_000 {
        many_headers.push(HeaderField::new(
            if i % 2 == 0 {
                "List-Unsubscribe"
            } else {
                "X-Filler"
            },
            format!("<https://example.com/{i}>"),
        ));
    }
    cases.push(hostile(
        "100k-header-fields",
        InsightInput {
            headers: many_headers,
            ..trigger()
        },
    ));

    let hrefs = [
        "https://example.com/verify?token=123456".to_owned(),
        "javascript:alert(document.cookie)".to_owned(),
        "file:///etc/passwd".to_owned(),
        "data:text/html;base64,PGgxPmhpPC9oMT4=".to_owned(),
        "https://ada:secret@example.com/reset".to_owned(),
        "https://пример.example/verify".to_owned(),
        "http://[::1]/sign-in".to_owned(),
        "https://example.com:99999/".to_owned(),
        "mailto:".to_owned(),
        String::new(),
        "  https://example.com/  confirm ".to_owned(),
        format!("https://example.com/reset?t={}", "z".repeat(9_000)),
        format!("https://example.com/r?u={}", "%2F".repeat(1_000)),
        "https://example.com/\u{0}/\u{feff}".to_owned(),
    ];
    let labels = [
        "Verify",
        "Reset password",
        "Sign in",
        "Confirm",
        "Track",
        "",
    ];
    let anchors = (0..1_000u32)
        .map(|i| Anchor {
            href: hrefs[i as usize % hrefs.len()].clone(),
            text: labels[i as usize % labels.len()].to_owned(),
            aria_label: (i % 3 == 0).then(|| "Verify your account".to_owned()),
            button_like: i % 2 == 0,
            in_footer: i % 5 == 0,
            below_forward: i % 7 == 0,
            index: if i == 999 { u32::MAX } else { i },
        })
        .collect();
    let segments = (0..5_000)
        .map(|i| Segment {
            text: match i % 4 {
                0 => format!("Your code is {:06}", i * 7),
                1 => "©".to_owned(),
                2 => String::new(),
                _ => "<b>".repeat(i % 50),
            },
            emphasis: i % 2 == 0,
            alone_on_line: i % 3 == 0,
            below_forward: i > 4_000,
        })
        .collect();
    cases.push(hostile(
        "many-anchors-and-segments",
        InsightInput {
            text: "Your verification code is 482913".into(),
            html: Some(
                HtmlView::new()
                    .with_anchors(anchors)
                    .with_segments(segments)
                    .with_footer_at(Some(2_500)),
            ),
            ..trigger()
        },
    ));

    let replaced = "\u{fffd}code\u{fffd} 48\u{fffd}2913 \u{fffd}\u{fffd}\u{fffd} 482913\u{fffd} \
                    https://example.com/\u{fffd}verify \u{fffd}"
        .repeat(1_000);
    cases.push(hostile(
        "invalid-utf8-replaced",
        InsightInput {
            subject: Some("Your code \u{fffd}482913\u{fffd}".into()),
            text: replaced.clone(),
            html: Some(HtmlView::new().with_segments(vec![segment(&replaced)])),
            ..trigger()
        },
    ));

    for lead in 0..4 {
        let text = multibyte_text(lead);
        cases.push(hostile(
            &format!("multibyte-boundaries-{lead}"),
            InsightInput {
                subject: Some(format!("{}验证码 123456", "é".repeat(lead))),
                text: text.clone(),
                forward_at: Some(text.chars().count() / 2 + lead),
                html: Some(
                    HtmlView::new()
                        .with_segments(vec![segment(&text), segment(&text[lead..])])
                        .with_anchors(vec![Anchor {
                            href: format!("https://example.com/{}", "验".repeat(lead + 1)),
                            text: "确认".into(),
                            ..Anchor::default()
                        }]),
                ),
                ..trigger()
            },
        ));
    }

    cases.push(hostile(
        "positions-out-of-range",
        InsightInput {
            text: "code 482913".into(),
            forward_at: Some(usize::MAX),
            sent_at: Some(Timestamp::from_millis(i64::MIN)),
            html: Some(
                HtmlView::new()
                    .with_footer_at(Some(usize::MAX))
                    .with_anchors(vec![Anchor {
                        href: "https://example.com/verify".into(),
                        text: "Verify".into(),
                        index: u32::MAX,
                        ..Anchor::default()
                    }]),
            ),
            ..trigger()
        },
    ));

    cases.push(hostile(
        "far-future-date",
        InsightInput {
            text: "code 482913".into(),
            sent_at: Some(Timestamp::from_millis(i64::MAX)),
            ..trigger()
        },
    ));

    let mut blocks = vec![
        nested_json(500),
        "[".repeat(100_000),
        format!("{}{}", "[".repeat(500), "]".repeat(500)),
        r##"{"@context":"https://schema.org","@graph":[
            {"@id":"#a","@type":"FlightReservation","reservationNumber":"XK4P2Q","reservationFor":{"@id":"#b"}},
            {"@id":"#b","@type":"Flight","flightNumber":"123","provider":{"@id":"#a"},"departureAirport":{"@id":"#b"}}
        ]}"##
            .to_owned(),
        format!(
            r#"{{"@type":"Order","orderNumber":"{}","seller":{{"name":"{}"}}}}"#,
            "9".repeat(1_000),
            "S".repeat(10_000)
        ),
        format!(r#"{{"@type":"Order","description":"{}"}}"#, "x".repeat(TEN_MB / 4)),
        r#"{"@type":["FlightReservation","Hologram"],"price":"1e400","priceCurrency":"KWD"}"#
            .to_owned(),
        "\u{feff}{\"@type\":\u{0}}".to_owned(),
    ];
    for i in 0..20 {
        blocks.push(format!(r#"{{"@type":"Order","orderNumber":"A-{i}"}}"#));
    }
    cases.push(hostile(
        "ld-json-deep-cyclic-huge",
        InsightInput {
            html: Some(HtmlView::new().with_ld_json(blocks)),
            ..trigger()
        },
    ));

    let mut cycle = MicrodataItem::new(vec!["https://schema.org/LodgingReservation".into()]);
    cycle.top_level = true;
    cycle.properties = vec![
        MicrodataProperty {
            name: "reservationFor".into(),
            value: MicrodataValue::Item(1),
        },
        MicrodataProperty {
            name: "self".into(),
            value: MicrodataValue::Item(0),
        },
        MicrodataProperty {
            name: "missing".into(),
            value: MicrodataValue::Item(usize::MAX),
        },
    ];
    let mut back = MicrodataItem::new(vec!["https://schema.org/LodgingBusiness".into()]);
    back.properties = vec![MicrodataProperty {
        name: "parent".into(),
        value: MicrodataValue::Item(0),
    }];
    let mut fan_in = vec![cycle, back];
    for i in 2..66 {
        let mut item = MicrodataItem::new(vec!["https://schema.org/Thing".into()]);
        item.top_level = i == 65;
        item.properties = (0..i)
            .map(|j| MicrodataProperty {
                name: "part".into(),
                value: MicrodataValue::Item(j),
            })
            .collect();
        fan_in.push(item);
    }
    cases.push(hostile(
        "microdata-cycles-and-fan-in",
        InsightInput {
            html: Some(HtmlView::new().with_microdata(fan_in)),
            ..trigger()
        },
    ));

    let mut forwarded = MicrodataItem::new(vec!["https://schema.org/Order".into()]);
    forwarded.top_level = true;
    forwarded.below_forward = true;
    forwarded.properties = vec![MicrodataProperty {
        name: "orderNumber".into(),
        value: MicrodataValue::Item(7),
    }];
    cases.push(hostile(
        "structured-forward-out-of-range",
        InsightInput {
            html: Some(
                HtmlView::new()
                    .with_ld_json(vec![r#"{"@type":"Order","orderNumber":"A-1"}"#.into()])
                    .with_ld_json_forward_at(Some(usize::MAX))
                    .with_microdata(vec![forwarded])
                    .with_truncated_structured(true),
            ),
            ..trigger()
        },
    ));

    let bomb = archive(&[
        Entry::stored("pass.json", pass_json()),
        Entry::zeros_bomb("strip.png", 65_000, true),
        Entry::zeros_bomb("logo.png", 65_000, false),
    ]);
    let traversal = archive(&[
        Entry::stored("../pass.json", pass_json()),
        Entry::stored("/etc/pass.json", pass_json()),
        Entry::stored("en.lproj/../../pass.json", pass_json()),
        Entry::stored("pass.json/", b""),
    ]);
    let mut attachments = vec![
        pkpass(0, bomb),
        pkpass(1, traversal),
        pkpass(2, Vec::new()),
        pkpass(3, b"PK\x03\x04 not really a zip".to_vec()),
        pkpass(
            4,
            archive(&[Entry::stored("pass.json", b"{\"organizationName\":")]),
        ),
    ];
    for i in 5..1_000u16 {
        attachments.push(AttachmentMeta {
            ordinal: i,
            filename: Some(format!("{}.pdf", "n".repeat(usize::from(i)))),
            content_type: if i % 2 == 0 {
                PKPASS.into()
            } else {
                "application/pdf".into()
            },
            size: u64::MAX,
            content: None,
        });
    }
    cases.push(hostile(
        "pkpass-bombs-and-traversal",
        InsightInput {
            attachments,
            ..trigger()
        },
    ));

    cases.push(hostile("empty", InsightInput::default()));

    cases
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn corpus_is_large_where_promised() {
        let corpus = hostile_corpus();
        assert!(corpus.iter().any(|case| case.input.text.len() > TEN_MB));
        let anchors = corpus
            .iter()
            .filter_map(|case| case.input.html.as_ref())
            .map(|html| html.anchors.len())
            .max();
        assert_eq!(anchors, Some(1_000));
    }

    #[test]
    fn personal_mail_is_personal() {
        for case in personal_mail() {
            assert_eq!(case.expect, Expect::Nothing);
            assert!(case.input.header_values("List-Id").next().is_none());
        }
    }

    #[test]
    fn pkpass_bomb_expands_past_the_cap() {
        let corpus = hostile_corpus();
        let case = corpus
            .iter()
            .find(|case| case.name == "pkpass-bombs-and-traversal")
            .unwrap();
        let bytes = case.input.attachments[0].content.clone().unwrap();
        assert!(bytes.len() < crate::limits::MAX_PKPASS_BYTES);
        let mut zip = ::zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
        let mut pass = String::new();
        zip.by_name("pass.json")
            .unwrap()
            .read_to_string(&mut pass)
            .unwrap();
        assert!(pass.contains("Example Air"));
        let mut expanded = Vec::new();
        zip.by_name("strip.png")
            .unwrap()
            .read_to_end(&mut expanded)
            .unwrap();
        assert!(expanded.len() > TEN_MB);
        assert!(expanded.iter().all(|byte| *byte == 0));

        let traversal = case.input.attachments[1].content.clone().unwrap();
        let zip = ::zip::ZipArchive::new(std::io::Cursor::new(traversal.to_vec())).unwrap();
        assert!(
            zip.file_names()
                .any(|name| name.is_ok_and(|name| name.starts_with("../")))
        );
    }
}
