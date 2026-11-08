//! DuckDuckGo Email Protection: [`DuckDetector`] recognises mail forwarded from a `@duck.com`
//! address.
//!
//! DuckDuckGo has no public API for listing addresses, so there is no directory.

use holtmail_alias::{AliasDetector, AliasHit, Mail, ReplyStrategy};
use holtmail_core::Address;

/// The provider id.
pub const PROVIDER: &str = "duck";

/// The domain of Duck addresses.
pub const DOMAIN: &str = "duck.com";

/// Recognises mail DuckDuckGo forwarded: `From` is a reply address
/// `sender_at_example.com_user@duck.com` and the Duck address is among the recipients. The
/// sender comes from that reply address, which is also the reverse alias.
#[derive(Debug, Clone)]
pub struct DuckDetector {
    domain: String,
}

impl Default for DuckDetector {
    fn default() -> Self {
        Self::new(DOMAIN)
    }
}

impl DuckDetector {
    /// A detector for addresses on `domain` instead of `duck.com`, for tests.
    pub fn new(domain: &str) -> Self {
        Self {
            domain: domain.trim().to_ascii_lowercase(),
        }
    }
}

/// `ada_at_example.com_jane` as `ada@example.com`.
fn sender_of(local: &str) -> Option<String> {
    let (sender_local, rest) = local.split_once("_at_")?;
    let (domain, user) = rest.rsplit_once('_')?;
    if sender_local.is_empty() || user.is_empty() || !domain.contains('.') {
        return None;
    }
    Some(format!("{sender_local}@{domain}"))
}

impl DuckDetector {
    /// `ada@example.com` written to `jane@duck.com` as `ada_at_example.com_jane@duck.com`, the
    /// mirror of [`sender_of`].
    fn join(&self, alias: &str, contact: &str) -> Option<String> {
        let alias = alias.trim().to_ascii_lowercase();
        let contact = contact.trim().to_ascii_lowercase();
        let (user, domain) = alias.rsplit_once('@')?;
        if domain != self.domain || user.is_empty() || user.contains("_at_") {
            return None;
        }
        let (contact_local, contact_domain) = contact.rsplit_once('@')?;
        let local = format!("{contact_local}_at_{contact_domain}_{user}");
        (sender_of(&local)? == contact).then(|| format!("{local}@{domain}"))
    }
}

impl AliasDetector for DuckDetector {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn version(&self) -> String {
        format!("1;{}", self.domain)
    }

    fn detect(&self, mail: &Mail) -> Option<AliasHit> {
        let from = mail.from.first()?;
        let reverse = from.email.trim().to_ascii_lowercase();
        let (local, domain) = reverse.rsplit_once('@')?;
        if domain != self.domain {
            return None;
        }
        let email = sender_of(local)?;
        let alias = mail.recipients().into_iter().find(|r| {
            r.rsplit_once('@')
                .is_some_and(|(l, d)| d == self.domain && !l.contains("_at_"))
        })?;
        let name = from
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| {
                !n.is_empty()
                    && !n.contains('@')
                    && !n.to_ascii_lowercase().contains(" at ")
                    && !n.eq_ignore_ascii_case(&email)
            })
            .map(str::to_owned);
        Some(AliasHit {
            provider: PROVIDER.into(),
            alias,
            sender: Some(Address { name, email }),
            reverse_alias: Some(reverse),
            signed: mail.dkim_pass(&[self.domain.as_str()]),
        })
    }

    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::ReverseAlias
    }

    fn encode_reverse(&self, alias: &str, contact: &str) -> Option<String> {
        self.join(alias, contact)
    }

    fn decode_reverse(&self, address: &str) -> Option<String> {
        let address = address.trim().to_ascii_lowercase();
        let (local, domain) = address.rsplit_once('@')?;
        if domain != self.domain {
            return None;
        }
        sender_of(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_reply_addresses() {
        assert_eq!(
            sender_of("ada_at_example.com_jane").as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(
            sender_of("first_last_at_mail.example.org_jane").as_deref(),
            Some("first_last@mail.example.org")
        );
        assert_eq!(sender_of("jane"), None);
        assert_eq!(sender_of("ada_at_example_jane"), None);
        assert_eq!(sender_of("_at_example.com_jane"), None);
    }

    #[test]
    fn encodes_what_it_decodes() {
        let detector = DuckDetector::default();
        for contact in ["Ada@Example.com", "first_last@mail.example.org"] {
            let encoded = detector.encode_reverse("Jane@duck.com", contact).unwrap();
            assert_eq!(
                detector.decode_reverse(&encoded),
                Some(contact.to_ascii_lowercase())
            );
        }
        assert_eq!(
            detector
                .encode_reverse("jane@duck.com", "ada@example.com")
                .as_deref(),
            Some("ada_at_example.com_jane@duck.com")
        );
        assert_eq!(
            detector.encode_reverse("jane@example.org", "ada@example.com"),
            None
        );
        assert_eq!(
            detector.encode_reverse("ada_at_example.com_jane@duck.com", "bob@example.org"),
            None
        );
        assert_eq!(
            detector.encode_reverse("jane@duck.com", "ada@localhost"),
            None
        );
        assert_eq!(
            detector.decode_reverse("ada_at_example.com_jane@example.org"),
            None
        );
        assert_eq!(detector.reply_strategy(), ReplyStrategy::ReverseAlias);
    }
}
