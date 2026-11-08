//! Firefox Relay: [`RelayDetector`] recognises mail Relay forwarded, [`RelayDirectory`] manages
//! an account's masks through the Relay API.

mod directory;

use holtmail_alias::{AliasDetector, AliasHit, Mail, ReplyStrategy, parse_mailbox};
use holtmail_core::Address;

pub use directory::{DEFAULT_URL, RelayDirectory};

/// The provider id.
pub const PROVIDER: &str = "relay";

/// Domains of Relay masks; their DKIM signatures mark mail as really forwarded by Relay.
pub const DOMAINS: &[&str] = &["mozmail.com", "relay.firefox.com"];

const VIA: &str = "[via Relay]";

/// Recognises mail Firefox Relay forwarded: `From` is the mask with a name ending in
/// `[via Relay]`, and `Resent-From` is the sender. Replies go to one address for every sender, so
/// there is no reverse alias.
#[derive(Debug, Clone)]
pub struct RelayDetector {
    domains: Vec<String>,
}

impl Default for RelayDetector {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl RelayDetector {
    /// A detector that also takes masks on `extra_domains` (a self-hosted Relay).
    pub fn new(extra_domains: &[String]) -> Self {
        let mut domains: Vec<String> = DOMAINS.iter().map(|d| d.to_string()).collect();
        domains.extend(extra_domains.iter().map(|d| d.trim().to_ascii_lowercase()));
        domains.retain(|d| !d.is_empty());
        domains.sort();
        domains.dedup();
        Self { domains }
    }
}

impl AliasDetector for RelayDetector {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn version(&self) -> String {
        format!("1;{}", self.domains.join(","))
    }

    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::ReplyHeader
    }

    fn detect(&self, mail: &Mail) -> Option<AliasHit> {
        let from = mail.from.first()?;
        let name = from.name.as_deref()?.trim();
        let shown = name.strip_suffix(VIA)?.trim();
        let alias = from.email.trim().to_ascii_lowercase();
        let (_, domain) = alias.rsplit_once('@')?;
        if !self
            .domains
            .iter()
            .any(|d| domain == d || domain.ends_with(&format!(".{d}")))
        {
            return None;
        }
        let in_name = parse_mailbox(shown);
        let resent = mail.header("Resent-From").and_then(parse_mailbox);
        let sender = match (resent, in_name) {
            (Some(resent), in_name) => Some(Address {
                name: in_name
                    .filter(|n| n.email.eq_ignore_ascii_case(&resent.email))
                    .and_then(|n| n.name)
                    .or(resent.name),
                email: resent.email,
            }),
            (None, in_name) => in_name,
        };
        let domains: Vec<&str> = self.domains.iter().map(String::as_str).collect();
        Some(AliasHit {
            provider: PROVIDER.into(),
            alias,
            sender,
            reverse_alias: None,
            signed: mail.dkim_pass(&domains),
        })
    }
}
