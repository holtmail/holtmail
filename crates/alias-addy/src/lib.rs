//! addy.io (formerly AnonAddy): [`AddyDetector`] recognises mail addy.io forwarded,
//! [`AddyDirectory`] manages an account's aliases through the addy.io API.

mod directory;

use holtmail_alias::{AliasDetector, AliasHit, Mail, ReplyStrategy, parse_mailbox};
use holtmail_core::Address;

pub use directory::{AddyDirectory, DEFAULT_URL};

/// The provider id.
pub const PROVIDER: &str = "addy";

/// Domains whose DKIM signature marks mail as really forwarded by the hosted service.
pub const SIGNING_DOMAINS: &[&str] = &["addy.io", "anonaddy.com", "anonaddy.me", "addymail.com"];

/// Recognises mail addy.io forwarded: an `X-AnonAddy-Original-Sender` field or a `Feedback-ID`
/// ending in `:anonaddy`. addy.io writes the reverse alias as
/// `alias+sender=example.com@alias-domain` into `From` (or `Reply-To`, when replies go there),
/// which gives both the alias and the sender. The sender's name comes from
/// `X-AnonAddy-Original-From-Header` when present.
#[derive(Debug, Clone)]
pub struct AddyDetector {
    signing: Vec<String>,
}

impl Default for AddyDetector {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl AddyDetector {
    /// A detector that also trusts DKIM signatures of `extra_signing` domains, for a
    /// self-hosted addy.io or custom domains.
    pub fn new(extra_signing: &[String]) -> Self {
        let mut signing: Vec<String> = SIGNING_DOMAINS.iter().map(|d| d.to_string()).collect();
        signing.extend(extra_signing.iter().map(|d| d.trim().to_ascii_lowercase()));
        signing.retain(|d| !d.is_empty());
        signing.sort();
        signing.dedup();
        Self { signing }
    }
}

/// `alias+sender=example.com@domain` as (alias, sender address).
fn split_reverse(address: &str) -> Option<(String, String)> {
    let address = address.trim().to_ascii_lowercase();
    let (local, domain) = address.rsplit_once('@')?;
    let (alias_local, extension) = local.split_once('+')?;
    let (sender_local, sender_domain) = extension.rsplit_once('=')?;
    if alias_local.is_empty()
        || domain.is_empty()
        || sender_local.is_empty()
        || !sender_domain.contains('.')
    {
        return None;
    }
    Some((
        format!("{alias_local}@{domain}"),
        format!("{sender_local}@{sender_domain}"),
    ))
}

/// `shop@domain` and `news@example.com` as `shop+news=example.com@domain`, the mirror of
/// [`split_reverse`].
fn join_reverse(alias: &str, contact: &str) -> Option<String> {
    let alias = alias.trim().to_ascii_lowercase();
    let contact = contact.trim().to_ascii_lowercase();
    let (alias_local, domain) = alias.rsplit_once('@')?;
    let (contact_local, contact_domain) = contact.rsplit_once('@')?;
    if alias_local.contains('+') || contact_local.contains('@') {
        return None;
    }
    let joined = format!("{alias_local}+{contact_local}={contact_domain}@{domain}");
    (split_reverse(&joined)? == (alias, contact)).then_some(joined)
}

impl AliasDetector for AddyDetector {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn version(&self) -> String {
        format!("1;{}", self.signing.join(","))
    }

    fn detect(&self, mail: &Mail) -> Option<AliasHit> {
        let marked = mail.header("X-AnonAddy-Original-Sender").is_some()
            || mail
                .headers_named("Feedback-ID")
                .any(|f| f.trim().to_ascii_lowercase().ends_with(":anonaddy"));
        if !marked {
            return None;
        }
        let (reverse, (alias, decoded)) = mail.from.iter().chain(&mail.reply_to).find_map(|a| {
            split_reverse(&a.email).map(|split| (a.email.to_ascii_lowercase(), split))
        })?;
        let original = mail
            .header("X-AnonAddy-Original-From-Header")
            .and_then(parse_mailbox);
        let sender = match original {
            Some(original) => original,
            None => Address {
                name: None,
                email: mail
                    .header("X-AnonAddy-Original-Sender")
                    .and_then(parse_mailbox)
                    .map(|a| a.email)
                    .unwrap_or(decoded),
            },
        };
        let signing: Vec<&str> = self.signing.iter().map(String::as_str).collect();
        Some(AliasHit {
            provider: PROVIDER.into(),
            alias,
            sender: Some(sender),
            reverse_alias: Some(reverse),
            signed: mail.dkim_pass(&signing),
        })
    }

    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::ReverseAlias
    }

    fn encode_reverse(&self, alias: &str, contact: &str) -> Option<String> {
        join_reverse(alias, contact)
    }

    fn decode_reverse(&self, address: &str) -> Option<String> {
        split_reverse(address).map(|(_, contact)| contact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_reverse_aliases() {
        assert_eq!(
            split_reverse("Shop+News=Example.com@ada.anonaddy.example"),
            Some((
                "shop@ada.anonaddy.example".into(),
                "news@example.com".into()
            ))
        );
        assert_eq!(
            split_reverse("shop+first.last=mail.example.org@alias.example.com"),
            Some((
                "shop@alias.example.com".into(),
                "first.last@mail.example.org".into()
            ))
        );
        assert_eq!(split_reverse("shop@alias.example.com"), None);
        assert_eq!(split_reverse("shop+tag@alias.example.com"), None);
        assert_eq!(split_reverse("+a=b.c@alias.example.com"), None);
    }

    #[test]
    fn encodes_what_it_decodes() {
        let detector = AddyDetector::default();
        for (alias, contact) in [
            ("Shop@ada.anonaddy.example", "News@Example.com"),
            ("shop@alias.example.com", "first.last@mail.example.org"),
            ("shop@alias.example.com", "a=b@example.org"),
        ] {
            let encoded = detector.encode_reverse(alias, contact).unwrap();
            assert_eq!(
                split_reverse(&encoded),
                Some((alias.to_ascii_lowercase(), contact.to_ascii_lowercase()))
            );
            assert_eq!(
                detector.decode_reverse(&encoded),
                Some(contact.to_ascii_lowercase())
            );
        }
        assert_eq!(
            detector
                .encode_reverse("shop@ada.anonaddy.example", "news@example.com")
                .as_deref(),
            Some("shop+news=example.com@ada.anonaddy.example")
        );
        assert_eq!(
            detector.encode_reverse("shop+tag@alias.example.com", "news@example.com"),
            None
        );
        assert_eq!(
            detector.encode_reverse("shop@alias.example.com", "news@localhost"),
            None
        );
        assert_eq!(detector.encode_reverse("shop", "news@example.com"), None);
        assert_eq!(detector.decode_reverse("shop@alias.example.com"), None);
        assert_eq!(detector.reply_strategy(), ReplyStrategy::ReverseAlias);
    }
}
