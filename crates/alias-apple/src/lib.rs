//! Apple: [`AppleDetector`] recognises mail forwarded from Hide My Email addresses (iCloud+) and
//! from Sign in with Apple relay addresses.
//!
//! Apple has no public API for either, so there is no directory; the owner can add the
//! addresses by hand.

use holtmail_alias::{AliasDetector, AliasHit, Mail, ReplyStrategy, parse_mailbox};
use holtmail_core::Address;

/// The provider id.
pub const PROVIDER: &str = "apple";

/// The domain of Sign in with Apple relay addresses.
pub const RELAY_DOMAIN: &str = "privaterelay.appleid.com";

/// Domains whose DKIM signature marks mail as really forwarded by Apple.
pub const SIGNING_DOMAINS: &[&str] = &["icloud.com", "privaterelay.appleid.com", "apple.com"];

/// Recognises mail Apple forwarded. Hide My Email adds `X-Icloud-Hme: p=<alias>; ...;
/// s=<sender>`; Sign in with Apple relays to an address on `privaterelay.appleid.com`. When
/// Apple rewrote `From` to an address of its own, that address is the reverse alias.
#[derive(Debug, Clone)]
pub struct AppleDetector {
    relay: String,
    signing: Vec<String>,
}

impl Default for AppleDetector {
    fn default() -> Self {
        Self::with_domains(
            RELAY_DOMAIN,
            &SIGNING_DOMAINS
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>(),
        )
    }
}

impl AppleDetector {
    /// A detector with other relay and signing domains, for tests.
    pub fn with_domains(relay: &str, signing: &[String]) -> Self {
        Self {
            relay: relay.trim().to_ascii_lowercase(),
            signing: signing
                .iter()
                .map(|d| d.trim().to_ascii_lowercase())
                .collect(),
        }
    }
}

fn parameter<'a>(value: &'a str, key: &str) -> Option<&'a str> {
    value.split(';').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case(key)
            .then(|| v.trim())
            .filter(|v| !v.is_empty())
    })
}

impl AliasDetector for AppleDetector {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn version(&self) -> String {
        format!("1;{};{}", self.relay, self.signing.join(","))
    }

    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::ReverseAlias
    }

    fn detect(&self, mail: &Mail) -> Option<AliasHit> {
        let signing: Vec<&str> = self.signing.iter().map(String::as_str).collect();
        let signed = mail.dkim_pass(&signing);
        let from = mail.from.first();
        if let Some(hme) = mail.header("X-Icloud-Hme") {
            let alias = parse_mailbox(parameter(hme, "p")?)?
                .email
                .to_ascii_lowercase();
            let original = parameter(hme, "s").and_then(parse_mailbox);
            let from_email = from.map(|f| f.email.trim().to_ascii_lowercase());
            let reverse_alias = from_email.clone().filter(|f| {
                Some(f.as_str())
                    != original
                        .as_ref()
                        .map(|o| o.email.to_ascii_lowercase())
                        .as_deref()
                    && *f != alias
                    && f.rsplit_once('@').is_some_and(|(_, d)| {
                        d == self.relay
                            || signing
                                .iter()
                                .any(|s| d == *s || d.ends_with(&format!(".{s}")))
                    })
            });
            let sender = original.map(|o| Address {
                name: o.name.or_else(|| {
                    from.and_then(|f| f.name.clone())
                        .map(|n| n.trim().to_owned())
                        .filter(|n| !n.is_empty() && !n.contains('@'))
                }),
                email: o.email,
            });
            return Some(AliasHit {
                provider: PROVIDER.into(),
                alias,
                sender,
                reverse_alias,
                signed,
            });
        }
        let alias = mail.recipient_at(&self.relay)?;
        if from.is_some_and(|f| f.email.eq_ignore_ascii_case(&alias)) {
            return None;
        }
        Some(AliasHit {
            provider: PROVIDER.into(),
            alias,
            sender: None,
            reverse_alias: None,
            signed,
        })
    }
}
