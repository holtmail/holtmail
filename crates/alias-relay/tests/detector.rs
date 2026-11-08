//! The detector against invented messages in `tests/fixtures`.

use holtmail_alias::testing::conformance::{Case, detector};
use holtmail_alias::{Address, AliasHit, Header, Mail};
use holtmail_alias_relay::RelayDetector;

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
        &RelayDetector::new(&["relay.example.org".into()]),
        &[
            Case {
                name: "named sender, signed",
                mail: fixture("forward.eml"),
                expected: Some(AliasHit {
                    provider: "relay".into(),
                    alias: "k3j5h9x2@relay.example.org".into(),
                    sender: Some(Address {
                        name: Some("Ada Lovelace".into()),
                        email: "ada@example.com".into(),
                    }),
                    reverse_alias: None,
                    signed: true,
                }),
            },
            Case {
                name: "sender without a name, mask on a subdomain",
                mail: fixture("forward-no-name.eml"),
                expected: Some(AliasHit {
                    provider: "relay".into(),
                    alias: "p0q9r8@sub.relay.example.org".into(),
                    sender: Some(Address {
                        name: None,
                        email: "shop@example.com".into(),
                    }),
                    reverse_alias: None,
                    signed: false,
                }),
            },
            Case {
                name: "the words without a Relay mask",
                mail: fixture("lookalike.eml"),
                expected: None,
            },
        ],
    );
}

#[test]
fn replies_by_the_service_default() {
    use holtmail_alias::{AliasDetector, ReplyStrategy};
    assert_eq!(
        RelayDetector::new(&["relay.example.org".into()]).reply_strategy(),
        ReplyStrategy::ReplyHeader
    );
}
