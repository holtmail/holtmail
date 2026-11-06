//! DuckDuckGo Email Protection: [`DuckDetector`] recognises mail forwarded from a `@duck.com`
//! address.
//!
//! DuckDuckGo has no public API for listing addresses, so there is no directory.

use holtmail_alias::{AliasDetector, AliasHit, Mail};
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
}
