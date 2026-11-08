use crate::ComposeError;

const ATEXT_SPECIALS: &[u8] = b"!#$%&'*+-/=?^_`{|}~";

pub(crate) fn no_breaks(value: &str, field: &'static str) -> Result<(), ComposeError> {
    if value.contains(['\r', '\n', '\0']) {
        Err(ComposeError::Injection(field))
    } else {
        Ok(())
    }
}

pub(crate) fn normalize(address: &str, field: &'static str) -> Result<String, ComposeError> {
    no_breaks(address, field)?;
    let invalid = || ComposeError::Address(field);
    let (local, domain) = address.rsplit_once('@').ok_or_else(invalid)?;
    if !valid_local(local) {
        return Err(invalid());
    }
    let domain = ascii_domain(domain).ok_or_else(invalid)?;
    Ok(format!("{local}@{domain}"))
}

pub(crate) fn ascii_domain(domain: &str) -> Option<String> {
    if domain.is_empty() || domain.starts_with('[') {
        return None;
    }
    let ascii = idna::domain_to_ascii(domain).ok()?;
    let labels_ok = ascii.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    (labels_ok && ascii.len() <= 253).then_some(ascii)
}

fn valid_local(local: &str) -> bool {
    !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || ATEXT_SPECIALS.contains(&b))
}

pub(crate) fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_normalizes_addresses() {
        assert_eq!(
            normalize("ada@example.com", "to").unwrap(),
            "ada@example.com"
        );
        assert_eq!(
            normalize("first.last+tag@Example.COM", "to").unwrap(),
            "first.last+tag@example.com"
        );
        assert_eq!(
            normalize("ada@bücher.example", "to").unwrap(),
            "ada@xn--bcher-kva.example"
        );
    }

    #[test]
    fn refuses_bad_addresses() {
        for bad in [
            "",
            "ada",
            "@example.com",
            "ada@",
            "a da@example.com",
            "ada@exa mple.com",
            "\"ada\"@example.com",
            "ада@example.com",
            ".ada@example.com",
            "a..da@example.com",
            "ada@[192.0.2.1]",
            "ada@-example.com",
            "ada@example..com",
            "<ada@example.com>",
            "ada@example.com>, eve@example.org",
        ] {
            assert_eq!(
                normalize(bad, "to"),
                Err(ComposeError::Address("to")),
                "{bad}"
            );
        }
        assert_eq!(
            normalize("ada@example.com\r\nBcc: eve@example.org", "to"),
            Err(ComposeError::Injection("to"))
        );
    }
}
