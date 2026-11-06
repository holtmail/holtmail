use holtmail_core::Address;

/// One header field, unfolded, with encoded words decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The name as written.
    pub name: String,
    /// The value on one line, without surrounding whitespace.
    pub value: String,
}

impl Header {
    /// A field with this name and value.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// What a detector sees of a message: its header fields in message order (topmost first) and
/// its parsed address fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mail {
    /// Every header field, topmost first.
    pub headers: Vec<Header>,
    /// `From`.
    pub from: Vec<Address>,
    /// `To`.
    pub to: Vec<Address>,
    /// `Cc`.
    pub cc: Vec<Address>,
    /// `Reply-To`.
    pub reply_to: Vec<Address>,
}

impl Mail {
    /// The value of the topmost field named `name`, compared without regard to letter case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    /// The values of every field named `name`, topmost first.
    pub fn headers_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    /// The addresses the message was delivered to, lowercase, each once: `To`, `Cc`,
    /// `Delivered-To`, `X-Original-To` and `Envelope-To`.
    pub fn recipients(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .to
            .iter()
            .chain(&self.cc)
            .map(|a| a.email.trim().to_ascii_lowercase())
            .collect();
        for name in ["Delivered-To", "X-Original-To", "Envelope-To"] {
            out.extend(
                self.headers_named(name)
                    .filter_map(parse_mailbox)
                    .map(|a| a.email.to_ascii_lowercase()),
            );
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|a| !a.is_empty() && seen.insert(a.clone()));
        out
    }

    /// The first recipient (see [`Mail::recipients`]) whose domain is `domain` or below it.
    pub fn recipient_at(&self, domain: &str) -> Option<String> {
        self.recipients()
            .into_iter()
            .find(|a| domain_of(a).is_some_and(|d| within(d, domain)))
    }

    /// The topmost `Authentication-Results` field, which the receiving server adds, records a
    /// passing DKIM signature by one of `domains` or a domain below one of them. Comments in
    /// parentheses are ignored, as RFC 8601 allows them anywhere.
    pub fn dkim_pass(&self, domains: &[&str]) -> bool {
        self.dkim_pass_domains()
            .iter()
            .any(|signer| domains.iter().any(|d| within(signer, d)))
    }

    /// The signing domains the topmost `Authentication-Results` field records as `dkim=pass`:
    /// each `header.d`, and the domain of each `header.i` written as `@domain`. Lowercase, each
    /// once, in field order. Empty when the field is missing or records no pass.
    pub fn dkim_pass_domains(&self) -> Vec<String> {
        let Some(results) = self
            .header("Authentication-Results")
            .and_then(holtmail_auth_results::parse_auth_results)
        else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        let passes = results
            .results
            .iter()
            .filter(|r| r.method == "dkim" && r.result == "pass");
        for property in passes.flat_map(|r| &r.properties) {
            let signer = match (property.ptype.as_str(), property.name.as_str()) {
                ("header", "d") => property.value.as_str(),
                ("header", "i") => match property.value.strip_prefix('@') {
                    Some(domain) => domain,
                    None => continue,
                },
                _ => continue,
            };
            let signer = signer.trim_end_matches([')', ',']).to_ascii_lowercase();
            if !signer.is_empty() && !out.contains(&signer) {
                out.push(signer);
            }
        }
        out
    }
}

/// The domain of an address, after the last `@`.
pub(crate) fn domain_of(address: &str) -> Option<&str> {
    address
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| !d.is_empty())
}

fn within(domain: &str, parent: &str) -> bool {
    let domain = domain.trim_end_matches('.');
    let parent = parent.trim_end_matches('.');
    domain.eq_ignore_ascii_case(parent)
        || (domain.len() > parent.len()
            && domain[domain.len() - parent.len()..].eq_ignore_ascii_case(parent)
            && domain.as_bytes()[domain.len() - parent.len() - 1] == b'.')
}

/// Reads one mailbox from a header value: `Name <a@example.com>`, `"Name" <a@example.com>`,
/// `<a@example.com>` or `a@example.com`. The first one when there are several. `None` when
/// the value has no address.
pub fn parse_mailbox(value: &str) -> Option<Address> {
    let value = value.trim();
    if let Some(open) = value.rfind('<') {
        let close = value[open..].find('>')? + open;
        let email = value[open + 1..close].trim();
        if !email.contains('@') {
            return None;
        }
        let name = value[..open].trim().trim_matches('"').trim();
        return Some(Address {
            name: (!name.is_empty()).then(|| name.replace("\\\"", "\"")),
            email: email.to_owned(),
        });
    }
    let first = value.split([',', ' ']).find(|w| w.contains('@'))?;
    let email = first.trim().trim_matches(['"', '\'']);
    (email.contains('@') && !email.starts_with('@') && !email.ends_with('@')).then(|| Address {
        name: None,
        email: email.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(headers: &[(&str, &str)]) -> Mail {
        Mail {
            headers: headers.iter().map(|(n, v)| Header::new(*n, *v)).collect(),
            ..Mail::default()
        }
    }

    #[test]
    fn reads_mailboxes() {
        let ada = |name: Option<&str>| Address {
            name: name.map(str::to_owned),
            email: "ada@example.com".into(),
        };
        assert_eq!(
            parse_mailbox("Ada Lovelace <ada@example.com>"),
            Some(ada(Some("Ada Lovelace")))
        );
        assert_eq!(
            parse_mailbox("\"Ada Lovelace\" <ada@example.com>"),
            Some(ada(Some("Ada Lovelace")))
        );
        assert_eq!(parse_mailbox("<ada@example.com>"), Some(ada(None)));
        assert_eq!(parse_mailbox(" ada@example.com "), Some(ada(None)));
        assert_eq!(
            parse_mailbox("ada@example.com, bob@example.org"),
            Some(ada(None))
        );
        assert_eq!(parse_mailbox("nobody"), None);
        assert_eq!(parse_mailbox("Broken <nobody>"), None);
        assert_eq!(parse_mailbox("<ada@example.com"), None);
        assert_eq!(parse_mailbox(""), None);
    }

    #[test]
    fn dkim_pass_reads_only_the_topmost_results() {
        let signed = mail(&[
            (
                "Authentication-Results",
                "mx.example.org; spf=pass smtp.mailfrom=relay.example.net; dkim=pass header.d=mail.relay.example.net header.s=one; dmarc=pass",
            ),
            (
                "Authentication-Results",
                "mx.example.org; dkim=pass header.d=other.example",
            ),
        ]);
        assert!(signed.dkim_pass(&["relay.example.net"]));
        assert!(!signed.dkim_pass(&["other.example"]));
        assert!(signed.dkim_pass(&["example.net"]));
        let opendkim = mail(&[(
            "Authentication-Results",
            "mx.example.org;\tdkim=pass (2048-bit key; unprotected) header.d=relay.example.net header.i=@relay.example.net header.a=rsa-sha256 header.s=dkim header.b=HV6FNGOV",
        )]);
        assert!(opendkim.dkim_pass(&["relay.example.net"]));
        let commented = mail(&[(
            "Authentication-Results",
            "mx.example.org (a (nested; comment)); dkim=fail (dkim=pass header.d=relay.example.net) header.d=relay.example.net",
        )]);
        assert!(!commented.dkim_pass(&["relay.example.net"]));
        let failed = mail(&[(
            "Authentication-Results",
            "mx.example.org; dkim=fail header.d=relay.example.net",
        )]);
        assert!(!failed.dkim_pass(&["relay.example.net"]));
        let lookalike = mail(&[(
            "Authentication-Results",
            "mx.example.org; dkim=pass header.d=evilrelay.example.net",
        )]);
        assert!(!lookalike.dkim_pass(&["relay.example.net"]));
        assert!(!mail(&[]).dkim_pass(&["relay.example.net"]));
    }

    #[test]
    fn dkim_pass_domains_lists_passing_signers_of_the_topmost_field() {
        let m = mail(&[
            (
                "Authentication-Results",
                "mx.example.org; dkim=pass header.d=Relay.Example.NET header.i=@relay.example.net; dkim=pass header.i=user@other.example.org; dkim=pass header.i=@sub.example.org; dkim=fail header.d=failed.example.com",
            ),
            (
                "Authentication-Results",
                "mx.example.org; dkim=pass header.d=lower.example.com",
            ),
        ]);
        assert_eq!(
            m.dkim_pass_domains(),
            ["relay.example.net", "sub.example.org"]
        );
        assert!(mail(&[]).dkim_pass_domains().is_empty());
    }

    #[test]
    fn recipients_come_from_address_fields_and_delivery_fields() {
        let mut m = mail(&[
            ("Delivered-To", "Shop.One@Alias.Example"),
            ("X-Original-To", "<other@example.org>"),
        ]);
        m.to = vec![Address {
            name: None,
            email: "shop.one@alias.example".into(),
        }];
        assert_eq!(
            m.recipients(),
            vec!["shop.one@alias.example", "other@example.org"]
        );
        assert_eq!(
            m.recipient_at("alias.example").as_deref(),
            Some("shop.one@alias.example")
        );
        assert_eq!(m.recipient_at("ias.example"), None);
    }
}
