//! The detector against invented messages in `tests/fixtures`.

use holtmail_alias::testing::conformance::{Case, detector};
use holtmail_alias::{Address, AliasHit, Header, Mail};
use holtmail_alias_apple::AppleDetector;

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

fn sender(name: &str, email: &str) -> Option<Address> {
    Some(Address {
        name: Some(name.into()),
        email: email.into(),
    })
}

#[test]
fn keeps_the_contract_on_the_fixtures() {
    detector(
        &AppleDetector::with_domains("relay.example.org", &["icloud.example.org".into()]),
        &[
            Case {
                name: "Hide My Email with a rewritten From, signed",
                mail: fixture("hide-my-email.eml"),
                expected: Some(AliasHit {
                    provider: "apple".into(),
                    alias: "quiet.river.42@icloud.example.org".into(),
                    sender: sender("Example News", "news@example.com"),
                    reverse_alias: Some("news_at_example_com_x7y8@icloud.example.org".into()),
                    signed: true,
                }),
            },
            Case {
                name: "Hide My Email with From kept",
                mail: fixture("hide-my-email-kept-from.eml"),
                expected: Some(AliasHit {
                    provider: "apple".into(),
                    alias: "quiet.river.42@icloud.example.org".into(),
                    sender: sender("Billing", "billing@example.com"),
                    reverse_alias: None,
                    signed: false,
                }),
            },
            Case {
                name: "Sign in with Apple relay",
                mail: fixture("relay.eml"),
                expected: Some(AliasHit {
                    provider: "apple".into(),
                    alias: "x7k2m9q4@relay.example.org".into(),
                    sender: None,
                    reverse_alias: None,
                    signed: false,
                }),
            },
        ],
    );
}
