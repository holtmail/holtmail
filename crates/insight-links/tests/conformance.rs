//! The conformance suite with hand-built inputs, and properties over arbitrary anchor sets.

use holtmail_core::{Address, Timestamp};
use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{Anchor, HeaderField, HtmlView, InsightExtractor, InsightInput, Items};
use holtmail_insight_links::{ALGORITHM, ID, LinksExtractor};
use proptest::prelude::*;

fn mail(anchors: Vec<Anchor>) -> InsightInput {
    InsightInput {
        headers: vec![
            HeaderField::new("From", "Example Accounts <no-reply@example.com>"),
            HeaderField::new("To", "Ada Example <ada@example.org>"),
            HeaderField::new("Subject", "Your account"),
        ],
        from: vec![Address {
            name: Some("Example Accounts".into()),
            email: "no-reply@example.com".into(),
        }],
        subject: Some("Your account".into()),
        sent_at: Some(Timestamp::from_secs(1_793_952_720)),
        html: Some(HtmlView::new().with_anchors(anchors)),
        ..InsightInput::default()
    }
}

fn anchor(index: u32, href: &str, text: &str, button_like: bool) -> Anchor {
    Anchor {
        href: href.into(),
        text: text.into(),
        button_like,
        index,
        ..Anchor::default()
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case::new(
            "reset-button",
            mail(vec![anchor(
                0,
                "https://example.com/reset?t=1",
                "Reset password",
                true,
            )]),
            Expect::Found,
        ),
        Case::new(
            "verify-and-sign-in",
            mail(vec![
                anchor(0, "https://example.com/verify?t=2", "Verify email", true),
                anchor(1, "https://example.com/login", "Sign in", true),
                anchor(2, "https://example.com/verify?t=2", "Verify email", true),
            ]),
            Expect::Found,
        ),
        Case::new(
            "aria-only",
            mail(vec![Anchor {
                aria_label: Some("Track your parcel".into()),
                ..anchor(0, "https://example.com/track/1", "", true)
            }]),
            Expect::Found,
        ),
        Case::new(
            "text-host",
            mail(vec![Anchor {
                aria_label: Some("Sign in".into()),
                ..anchor(0, "https://example.net/login", "accounts.example.com", true)
            }]),
            Expect::Found,
        ),
        Case::new(
            "label-equals-keyword",
            mail(vec![anchor(
                0,
                "https://example.com/verify",
                "verify",
                true,
            )]),
            Expect::Found,
        ),
        Case::new(
            "plain-link-not-enough",
            mail(vec![anchor(
                0,
                "https://example.com/account",
                "Verify",
                false,
            )]),
            Expect::Nothing,
        ),
        Case::new(
            "footer",
            mail(vec![Anchor {
                in_footer: true,
                ..anchor(0, "https://example.com/verify", "Verify", true)
            }]),
            Expect::Nothing,
        ),
        Case::new(
            "social",
            mail(vec![anchor(
                0,
                "https://www.linkedin.com/company/example/verify",
                "Verify",
                true,
            )]),
            Expect::Nothing,
        ),
        Case::new(
            "mailto",
            mail(vec![anchor(
                0,
                "mailto:confirm@example.com",
                "Confirm",
                true,
            )]),
            Expect::Nothing,
        ),
        Case::new(
            "unsubscribe",
            mail(vec![anchor(
                0,
                "https://example.com/unsubscribe?u=1",
                "Confirm",
                true,
            )]),
            Expect::Nothing,
        ),
        Case::new(
            "forwarded",
            mail(vec![Anchor {
                below_forward: true,
                ..anchor(0, "https://example.com/reset", "Reset password", true)
            }]),
            Expect::Nothing,
        ),
        Case::new(
            "text-only",
            InsightInput {
                text: "Reset your password: https://example.com/reset".into(),
                html: None,
                ..mail(Vec::new())
            },
            Expect::Nothing,
        ),
    ]
}

#[test]
fn keeps_the_contract() {
    conformance::extractor(&LinksExtractor::new(), &cases());
}

#[test]
fn id_and_version() {
    let extractor = LinksExtractor::new();
    assert_eq!(extractor.id(), "links");
    assert_eq!(ID, "links");
    let version = extractor.version();
    let (algorithm, hash) = version.split_once('+').unwrap();
    assert_eq!(algorithm, ALGORITHM.to_string());
    assert_eq!(hash.len(), 12);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn at_most_two_items() {
    let anchors = (0..40)
        .map(|i| {
            anchor(
                i,
                &format!("https://example.com/verify?t={i}"),
                "Verify",
                true,
            )
        })
        .collect();
    let items = LinksExtractor::new().extract(&mail(anchors));
    assert_eq!(items.len(), 2);
}

fn arb_anchor() -> impl Strategy<Value = Anchor> {
    let href = prop_oneof![
        Just("https://example.com/verify?token=1".to_owned()),
        Just("https://example.com/reset".to_owned()),
        Just("mailto:a@example.com".to_owned()),
        Just("https://pay.google.com/gp/v/save/abc".to_owned()),
        Just("https://www.facebook.com/example".to_owned()),
        Just("https://example.com/unsubscribe".to_owned()),
        "https://example\\.com/[a-z%0-9/?=&]{0,40}",
        ".{0,60}",
    ];
    let text = prop_oneof![
        Just("Verify".to_owned()),
        Just("Reset password".to_owned()),
        Just("登录".to_owned()),
        Just("example.com".to_owned()),
        ".{0,40}",
    ];
    (
        href,
        text,
        proptest::option::of(".{0,30}"),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<u32>(),
    )
        .prop_map(
            |(href, text, aria_label, button_like, in_footer, below_forward, index)| Anchor {
                href,
                text,
                aria_label,
                button_like,
                in_footer,
                below_forward,
                index,
            },
        )
}

proptest! {
    #[test]
    fn never_panics_and_is_deterministic(anchors in proptest::collection::vec(arb_anchor(), 0..24)) {
        let extractor = LinksExtractor::new();
        let input = mail(anchors);
        let first = extractor.extract(&input);
        let second = extractor.extract(&input);
        prop_assert!(first.len() <= 2);
        prop_assert_eq!(
            Items { items: first.clone() }.to_json(),
            Items { items: second }.to_json()
        );
        for item in &first {
            prop_assert!((600..=1000).contains(&item.score));
        }
        let _ = extractor.explain(&input);
    }
}
