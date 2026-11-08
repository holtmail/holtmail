/// Whether `pattern` is one of the forms [`allows`] understands: an exact address,
/// `*@domain` or `*@*.domain`.
pub fn valid_pattern(pattern: &str) -> bool {
    let pattern = pattern.trim();
    let Some((local, domain)) = pattern.rsplit_once('@') else {
        return false;
    };
    let domain = domain.strip_prefix("*.").unwrap_or(domain);
    if !valid_domain(domain) {
        return false;
    }
    local == "*" || (!local.is_empty() && !local.contains(['*', '@', ' ']))
}

/// Whether an `allowed_from` pattern covers `address`.
///
/// - `ada@example.com` covers exactly that address;
/// - `*@example.com` covers every address on `example.com`, not on its subdomains;
/// - `*@*.example.com` covers every address on a subdomain of `example.com` at any depth,
///   not on `example.com` itself.
///
/// The comparison ignores case and surrounding whitespace. A pattern that is not
/// [`valid_pattern`] covers nothing, and an exact pattern does not cover plus addresses of
/// its address.
pub fn allows(pattern: &str, address: &str) -> bool {
    if !valid_pattern(pattern) {
        return false;
    }
    let pattern = pattern.trim().to_lowercase();
    let address = address.trim().to_lowercase();
    let (Some((p_local, p_domain)), Some((a_local, a_domain))) =
        (pattern.rsplit_once('@'), address.rsplit_once('@'))
    else {
        return false;
    };
    if a_local.is_empty() || !valid_domain(a_domain) {
        return false;
    }
    let domain_ok = match p_domain.strip_prefix("*.") {
        Some(parent) => a_domain
            .strip_suffix(parent)
            .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
        None => a_domain == p_domain,
    };
    domain_ok && (p_local == "*" || p_local == a_local)
}

fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let cases = [
            ("ada@example.com", "ada@example.com", true),
            ("ada@example.com", "ADA@Example.COM", true),
            (" ada@example.com ", "ada@example.com", true),
            ("ada@example.com", "bob@example.com", false),
            ("ada@example.com", "ada+shop@example.com", false),
            ("ada@example.com", "ada@mail.example.com", false),
            ("*@example.org", "bob@example.org", true),
            ("*@example.org", "bob+x@EXAMPLE.org", true),
            ("*@example.org", "bob@mail.example.org", false),
            ("*@example.org", "bob@badexample.org", false),
            ("*@example.org", "bob@example.org.example.com", false),
            ("*@*.example.org", "bob@mail.example.org", true),
            ("*@*.example.org", "bob@a.b.example.org", true),
            ("*@*.example.org", "bob@example.org", false),
            ("*@*.example.org", "bob@badexample.org", false),
            ("*@*.example.org", "bob@.example.org", false),
            ("*@example.org", "@example.org", false),
            ("*@example.org", "example.org", false),
            ("*", "bob@example.org", false),
            ("*@*", "bob@example.org", false),
            ("a*@example.org", "ab@example.org", false),
            ("", "bob@example.org", false),
            ("*@", "bob@example.org", false),
        ];
        for (pattern, address, expected) in cases {
            assert_eq!(allows(pattern, address), expected, "{pattern} / {address}");
        }
    }

    #[test]
    fn validation() {
        for good in [
            "ada@example.com",
            "*@example.com",
            "*@*.example.com",
            "a.b+c@x-y.example",
        ] {
            assert!(valid_pattern(good), "{good}");
        }
        for bad in [
            "",
            "*",
            "*@*",
            "*@",
            "@example.com",
            "a*@example.com",
            "*@exa mple.com",
            "*@example..com",
            "*@*.",
            "example.com",
        ] {
            assert!(!valid_pattern(bad), "{bad}");
        }
    }
}
