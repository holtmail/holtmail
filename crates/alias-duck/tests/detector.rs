//! The detector against invented messages in `tests/fixtures`.

use holtmail_alias::testing::conformance::{Case, detector};
use holtmail_alias::{Address, AliasHit, Header, Mail};
use holtmail_alias_duck::DuckDetector;

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
        &DuckDetector::new("duck.example.org"),
        &[
            Case {
                name: "a forwarded message",
                mail: fixture("forward.eml"),
                expected: Some(AliasHit {
                    provider: "duck".into(),
                    alias: "shop.one@duck.example.org".into(),
                    sender: Some(Address {
                        name: Some("Ada Lovelace".into()),
                        email: "ada@example.com".into(),
                    }),
                    reverse_alias: Some("ada_at_example.com_jane@duck.example.org".into()),
                    signed: true,
                }),
            },
            Case {
                name: "the owner's own mail from a Duck address",
                mail: fixture("own-reply.eml"),
                expected: None,
            },
        ],
    );
}
