//! The detector against invented messages in `tests/fixtures`.

use holtmail_alias::testing::conformance::{Case, detector};
use holtmail_alias::{Address, AliasHit, Header, Mail};
use holtmail_alias_addy::AddyDetector;

fn fixture(name: &str) -> Mail {
    let raw = std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let parsed = holtmail_mime::parse(&raw).unwrap();
    Mail {
        headers: parsed
            .headers
            .into_iter()
            .map(|h| Header::new(h.name, h.value))
            .collect(),
        from: parsed.from,
        to: parsed.to,
        cc: parsed.cc,
        reply_to: parsed.reply_to,
    }
}

#[test]
fn keeps_the_contract_on_the_fixtures() {
    detector(
        &AddyDetector::new(&["addy.example.org".into()]),
        &[
            Case {
                name: "reverse alias in From, signed",
                mail: fixture("forward.eml"),
                expected: Some(AliasHit {
                    provider: "addy".into(),
                    alias: "shop@ada.addy.example.org".into(),
                    sender: Some(Address {
                        name: Some("Example News".into()),
                        email: "news@example.com".into(),
                    }),
                    reverse_alias: Some("shop+news=example.com@ada.addy.example.org".into()),
                    signed: true,
                }),
            },
            Case {
                name: "reverse alias in Reply-To, unsigned",
                mail: fixture("forward-reply-to.eml"),
                expected: Some(AliasHit {
                    provider: "addy".into(),
                    alias: "shop@ada.addy.example.org".into(),
                    sender: Some(Address {
                        name: None,
                        email: "billing@example.com".into(),
                    }),
                    reverse_alias: Some("shop+billing=example.com@ada.addy.example.org".into()),
                    signed: false,
                }),
            },
            Case {
                name: "a plus address without addy.io's fields",
                mail: fixture("plain-plus.eml"),
                expected: None,
            },
        ],
    );
}

#[test]
fn replies_by_the_service_default() {
    use holtmail_alias::{AliasDetector, ReplyStrategy};
    assert_eq!(
        AddyDetector::default().reply_strategy(),
        ReplyStrategy::ReverseAlias
    );
}
