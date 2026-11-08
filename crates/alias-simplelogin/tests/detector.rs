//! The detector against invented messages in `tests/fixtures`.

use holtmail_alias::testing::conformance::{Case, detector};
use holtmail_alias::{Address, AliasDetector, AliasHit, Header, Mail};
use holtmail_alias_simplelogin::SimpleLoginDetector;

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

fn hit(alias: &str, sender: Option<(&str, &str)>, reverse: &str, signed: bool) -> AliasHit {
    AliasHit {
        provider: "simplelogin".into(),
        alias: alias.into(),
        sender: sender.map(|(name, email)| Address {
            name: (!name.is_empty()).then(|| name.to_owned()),
            email: email.into(),
        }),
        reverse_alias: Some(reverse.into()),
        signed,
    }
}

#[test]
fn keeps_the_contract_on_the_fixtures() {
    let detector_ = SimpleLoginDetector::new(&["sl.example.org".into()]);
    detector(
        &detector_,
        &[
            Case {
                name: "with X-SimpleLogin-Original-From, signed",
                mail: fixture("forward-original-from.eml"),
                expected: Some(hit(
                    "shop.one@alias.example.org",
                    Some(("Ada Lovelace", "ada@example.com")),
                    "ra_k3j5h9x2@sl.example.org",
                    true,
                )),
            },
            Case {
                name: "sender only in the rewritten display name, unsigned",
                mail: fixture("forward-name-only.eml"),
                expected: Some(hit(
                    "news.letters@alias.example.org",
                    Some(("Renée Martin", "renee@example.com")),
                    "ra_zz81@sl.example.org",
                    false,
                )),
            },
            Case {
                name: "forged fields signed by another domain",
                mail: fixture("forged.eml"),
                expected: Some(hit(
                    "shop.one@alias.example.org",
                    Some(("Bank", "security@example.com")),
                    "anyone@example.com",
                    false,
                )),
            },
            Case {
                name: "a reply is not a forward",
                mail: fixture("reply.eml"),
                expected: None,
            },
        ],
    );
}

#[test]
fn the_hosted_service_signs_with_its_own_domain() {
    let mut mail = fixture("forward-original-from.eml");
    mail.headers[0] = Header::new(
        "Authentication-Results",
        "mx.example.org; dkim=pass header.d=simplelogin.co",
    );
    assert!(SimpleLoginDetector::default().detect(&mail).unwrap().signed);
    assert_ne!(
        SimpleLoginDetector::default().version(),
        SimpleLoginDetector::new(&["sl.example.org".into()]).version()
    );
}

#[test]
fn replies_by_the_service_default() {
    use holtmail_alias::{AliasDetector, ReplyStrategy};
    assert_eq!(
        SimpleLoginDetector::default().reply_strategy(),
        ReplyStrategy::ReverseAlias
    );
}
