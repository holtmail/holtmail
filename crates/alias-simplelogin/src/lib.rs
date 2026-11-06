//! SimpleLogin (and Proton Pass aliases, which run on it): [`SimpleLoginDetector`] recognises
//! mail SimpleLogin forwarded, [`SimpleLoginDirectory`] manages an account's aliases through
//! the SimpleLogin API.

mod directory;

use holtmail_alias::{AliasDetector, AliasHit, Mail, parse_mailbox};
use holtmail_core::Address;

pub use directory::{DEFAULT_URL, SimpleLoginDirectory};

/// The provider id.
pub const PROVIDER: &str = "simplelogin";

/// Domains whose DKIM signature marks mail as really forwarded by the hosted service.
pub const SIGNING_DOMAINS: &[&str] = &["simplelogin.co", "simplelogin.com"];

/// Recognises mail SimpleLogin forwarded: `X-SimpleLogin-Type: Forward` with the alias in
/// `X-SimpleLogin-Envelope-To`. The sender comes from `X-SimpleLogin-Original-From` when the
/// account adds it, else from the name SimpleLogin gives the rewritten `From`
/// (`Name - ada at example.com`, `ada at example.com`, `Name - ada(a)example.com`), else from
/// `X-SimpleLogin-Envelope-From`. The rewritten `From` address is the reverse alias.
#[derive(Debug, Clone)]
pub struct SimpleLoginDetector {
    signing: Vec<String>,
}

impl Default for SimpleLoginDetector {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl SimpleLoginDetector {
    /// A detector that also trusts DKIM signatures of `extra_signing` domains, for a
    /// self-hosted SimpleLogin.
    pub fn new(extra_signing: &[String]) -> Self {
        let mut signing: Vec<String> = SIGNING_DOMAINS.iter().map(|d| d.to_string()).collect();
        signing.extend(extra_signing.iter().map(|d| d.trim().to_ascii_lowercase()));
        signing.retain(|d| !d.is_empty());
        signing.sort();
        signing.dedup();
        Self { signing }
    }
}

impl AliasDetector for SimpleLoginDetector {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn version(&self) -> String {
        format!("1;{}", self.signing.join(","))
    }

    fn detect(&self, mail: &Mail) -> Option<AliasHit> {
        if !mail
            .header("X-SimpleLogin-Type")
            .is_some_and(|t| t.eq_ignore_ascii_case("forward"))
        {
            return None;
        }
        let alias = parse_mailbox(mail.header("X-SimpleLogin-Envelope-To")?)?
            .email
            .to_ascii_lowercase();
        let from = mail.from.first();
        let sender = mail
            .header("X-SimpleLogin-Original-From")
            .and_then(parse_mailbox)
            .or_else(|| from.and_then(|f| f.name.as_deref()).and_then(from_name))
            .or_else(|| {
                let envelope = mail
                    .header("X-SimpleLogin-Envelope-From")
                    .and_then(parse_mailbox)?;
                Some(Address {
                    name: from
                        .and_then(|f| f.name.as_deref())
                        .map(str::trim)
                        .filter(|n| !n.is_empty() && !n.contains('@'))
                        .map(str::to_owned),
                    email: envelope.email,
                })
            });
        let reverse_alias = from
            .map(|f| f.email.trim().to_ascii_lowercase())
            .filter(|r| r.contains('@') && *r != alias);
        let signing: Vec<&str> = self.signing.iter().map(String::as_str).collect();
        Some(AliasHit {
            provider: PROVIDER.into(),
            alias,
            sender,
            reverse_alias,
            signed: mail.dkim_pass(&signing),
        })
    }
}

/// The sender in a display name SimpleLogin wrote: `Name - ada at example.com`,
/// `ada at example.com`, `Name - ada(a)example.com` or `ada(a)example.com`.
fn from_name(name: &str) -> Option<Address> {
    let name = name.trim();
    let (person, address) = match name.rsplit_once(" - ") {
        Some((person, address)) => (Some(person.trim()), address.trim()),
        None => (None, name),
    };
    let email = match address.rsplit_once(" at ") {
        Some((local, domain)) => format!("{}@{}", local.trim(), domain.trim()),
        None => address.replacen("(a)", "@", 1),
    };
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || !domain.contains('.')
        || domain.contains('@')
        || email.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(Address {
        name: person.filter(|p| !p.is_empty()).map(str::to_owned),
        email,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sender_formats() {
        let a = |name: Option<&str>, email: &str| {
            Some(Address {
                name: name.map(str::to_owned),
                email: email.into(),
            })
        };
        assert_eq!(
            from_name("Ada Lovelace - ada at example.com"),
            a(Some("Ada Lovelace"), "ada@example.com")
        );
        assert_eq!(from_name("ada at example.com"), a(None, "ada@example.com"));
        assert_eq!(
            from_name("Ada - Lovelace - ada(a)example.com"),
            a(Some("Ada - Lovelace"), "ada@example.com")
        );
        assert_eq!(from_name("Ada Lovelace"), None);
        assert_eq!(from_name("Shop - news at"), None);
        assert_eq!(from_name("x at y at z"), None);
    }
}
