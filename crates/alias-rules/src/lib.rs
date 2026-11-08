//! Alias detection from rules in the config: own domains with catch-all addresses, a
//! self-hosted forwarder, or any service without a detector of its own.
//!
//! Each [`Rule`] is one [`RuleDetector`] with the rule's provider id.

use holtmail_alias::{AliasDetector, AliasHit, Mail, ReplyStrategy, parse_mailbox};
use serde::Deserialize;

/// One rule, as `[[aliases.rules]]` in the config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The provider id put on what the rule finds: lowercase letters, digits and `-`.
    pub provider: String,
    /// A recipient on one of these domains (or below them) is the alias.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Only mail with this header field is forwarded mail.
    #[serde(default)]
    pub header: Option<String>,
    /// A header field holding the alias; tried before `domains`.
    #[serde(default)]
    pub alias_header: Option<String>,
    /// A header field holding the original sender.
    #[serde(default)]
    pub sender_header: Option<String>,
    /// A passing DKIM signature of one of these domains in the topmost
    /// `Authentication-Results` marks the mail as signed.
    #[serde(default)]
    pub signing_domains: Vec<String>,
}

/// Why a rule cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("alias rule {provider:?}: {reason}")]
pub struct RuleError {
    /// The rule's provider id as written.
    pub provider: String,
    /// What is wrong.
    pub reason: String,
}

/// Finds aliases by one [`Rule`].
#[derive(Debug, Clone)]
pub struct RuleDetector {
    rule: Rule,
}

impl RuleDetector {
    /// A detector for `rule`. Fails for a provider id that is not lowercase letters, digits and
    /// `-`, or a rule with neither `domains` nor `alias_header`.
    pub fn new(rule: Rule) -> Result<Self, RuleError> {
        let provider = rule.provider.clone();
        let refuse = |reason: &str| RuleError {
            provider: provider.clone(),
            reason: reason.into(),
        };
        if rule.provider.is_empty()
            || !rule
                .provider
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(refuse(
                "the provider id takes lowercase letters, digits and -",
            ));
        }
        let clean = |list: &[String]| -> Vec<String> {
            let mut list: Vec<String> = list
                .iter()
                .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
                .filter(|d| !d.is_empty())
                .collect();
            list.sort();
            list.dedup();
            list
        };
        let rule = Rule {
            domains: clean(&rule.domains),
            signing_domains: clean(&rule.signing_domains),
            header: rule
                .header
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty()),
            alias_header: rule
                .alias_header
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty()),
            sender_header: rule
                .sender_header
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty()),
            provider: rule.provider,
        };
        if rule.domains.is_empty() && rule.alias_header.is_none() {
            return Err(refuse("give domains or alias_header"));
        }
        Ok(Self { rule })
    }
}

impl AliasDetector for RuleDetector {
    fn provider(&self) -> &str {
        &self.rule.provider
    }

    fn version(&self) -> String {
        format!("rule;{:?}", self.rule)
    }

    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::Direct
    }

    fn detect(&self, mail: &Mail) -> Option<AliasHit> {
        if let Some(header) = &self.rule.header
            && mail.header(header).is_none()
        {
            return None;
        }
        let alias = self
            .rule
            .alias_header
            .as_deref()
            .and_then(|h| mail.header(h))
            .and_then(parse_mailbox)
            .map(|a| a.email.to_ascii_lowercase())
            .or_else(|| {
                self.rule
                    .domains
                    .iter()
                    .find_map(|domain| mail.recipient_at(domain))
            })?;
        if mail
            .from
            .iter()
            .any(|f| f.email.eq_ignore_ascii_case(&alias))
        {
            return None;
        }
        let sender = self
            .rule
            .sender_header
            .as_deref()
            .and_then(|h| mail.header(h))
            .and_then(parse_mailbox);
        let signing: Vec<&str> = self
            .rule
            .signing_domains
            .iter()
            .map(String::as_str)
            .collect();
        Some(AliasHit {
            provider: self.rule.provider.clone(),
            alias,
            sender,
            reverse_alias: None,
            signed: !signing.is_empty() && mail.dkim_pass(&signing),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_alias::Address;
    use holtmail_alias::testing::conformance::{Case, detector};
    use holtmail_alias::testing::mail;

    #[test]
    fn replies_directly_from_the_alias() {
        let rule = RuleDetector::new(Rule {
            provider: "mine".into(),
            domains: vec!["alias.example.org".into()],
            ..Rule::default()
        })
        .unwrap();
        assert_eq!(rule.reply_strategy(), ReplyStrategy::Direct);
        assert_eq!(
            rule.encode_reverse("shop@alias.example.org", "bob@example.org"),
            None
        );
    }

    #[test]
    fn refuses_unusable_rules() {
        assert!(
            RuleDetector::new(Rule {
                provider: "Mine".into(),
                domains: vec!["alias.example.org".into()],
                ..Rule::default()
            })
            .is_err()
        );
        assert!(
            RuleDetector::new(Rule {
                provider: "mine".into(),
                ..Rule::default()
            })
            .is_err()
        );
    }

    #[test]
    fn a_catch_all_domain() {
        let rule = RuleDetector::new(Rule {
            provider: "own-domain".into(),
            domains: vec!["@Alias.Example.org".into()],
            ..Rule::default()
        })
        .unwrap();
        detector(
            &rule,
            &[
                Case {
                    name: "to an address on the domain",
                    mail: mail(&[
                        ("From", "Ada <ada@example.com>"),
                        ("To", "Shop <Shop@alias.example.org>"),
                    ]),
                    expected: Some(AliasHit {
                        provider: "own-domain".into(),
                        alias: "shop@alias.example.org".into(),
                        sender: None,
                        reverse_alias: None,
                        signed: false,
                    }),
                },
                Case {
                    name: "delivered to a subdomain",
                    mail: mail(&[
                        ("Delivered-To", "news@lists.alias.example.org"),
                        ("From", "ada@example.com"),
                        ("To", "list@example.com"),
                    ]),
                    expected: Some(AliasHit {
                        provider: "own-domain".into(),
                        alias: "news@lists.alias.example.org".into(),
                        sender: None,
                        reverse_alias: None,
                        signed: false,
                    }),
                },
                Case {
                    name: "sent from the alias itself",
                    mail: mail(&[
                        ("From", "shop@alias.example.org"),
                        ("To", "shop@alias.example.org"),
                    ]),
                    expected: None,
                },
            ],
        );
    }

    #[test]
    fn a_forwarder_with_its_own_fields() {
        let rule = RuleDetector::new(Rule {
            provider: "forwarder".into(),
            header: Some("X-Forwarder".into()),
            alias_header: Some("X-Forwarder-Alias".into()),
            sender_header: Some("X-Forwarder-From".into()),
            signing_domains: vec!["fwd.example.org".into()],
            ..Rule::default()
        })
        .unwrap();
        detector(
            &rule,
            &[
                Case {
                    name: "marked and signed",
                    mail: mail(&[
                        (
                            "Authentication-Results",
                            "mx.example.org; dkim=pass header.d=fwd.example.org",
                        ),
                        ("X-Forwarder", "yes"),
                        ("X-Forwarder-Alias", "Shop@fwd.example.org"),
                        ("X-Forwarder-From", "Ada <ada@example.com>"),
                        ("From", "relay@fwd.example.org"),
                        ("To", "owner@example.org"),
                    ]),
                    expected: Some(AliasHit {
                        provider: "forwarder".into(),
                        alias: "shop@fwd.example.org".into(),
                        sender: Some(Address {
                            name: Some("Ada".into()),
                            email: "ada@example.com".into(),
                        }),
                        reverse_alias: None,
                        signed: true,
                    }),
                },
                Case {
                    name: "without the marking field",
                    mail: mail(&[
                        ("X-Forwarder-Alias", "shop@fwd.example.org"),
                        ("From", "relay@fwd.example.org"),
                    ]),
                    expected: None,
                },
            ],
        );
    }
}
