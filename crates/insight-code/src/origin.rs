use holtmail_insight::limits::MAX_TEXT_BYTES;
use holtmail_insight::{InsightInput, fits_identifier};

/// The code and origin of a WICG origin-bound last line (`@example.com #123456`), when the text
/// ends with one above any forward marker.
pub(crate) fn bound(input: &InsightInput) -> Option<(String, String)> {
    if input.text.len() > MAX_TEXT_BYTES {
        return None;
    }
    let trimmed = input.text.trim_end();
    let line_start = trimmed.rfind('\n').map_or(0, |at| at + 1);
    let line = trimmed.get(line_start..)?;
    if let Some(forward) = input.forward_at {
        let offset = trimmed.get(..line_start)?.chars().count();
        if offset >= forward {
            return None;
        }
    }
    let mut parts = line.split_whitespace();
    let host = parts.next()?.strip_prefix('@')?;
    let code = parts.next()?.strip_prefix('#')?;
    if let Some(embedded) = parts.next() {
        embedded.strip_prefix('@').filter(|h| valid_host(h))?;
    }
    if parts.next().is_some() || !valid_host(host) || !valid_code(code) {
        return None;
    }
    Some((code.to_owned(), host.to_ascii_lowercase()))
}

fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn valid_code(code: &str) -> bool {
    fits_identifier(code)
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> InsightInput {
        InsightInput {
            text: text.into(),
            ..InsightInput::default()
        }
    }

    #[test]
    fn last_line_is_read() {
        let found = bound(&input("Your code is 482913.\n\n@Example.com #482913\n"));
        assert_eq!(found, Some(("482913".into(), "example.com".into())));
        let embedded = bound(&input("x\n@example.com #A1-b2 @pay.example.org"));
        assert_eq!(embedded, Some(("A1-b2".into(), "example.com".into())));
    }

    #[test]
    fn other_shapes_are_not() {
        assert_eq!(bound(&input("@example.com #482913\nThanks")), None);
        assert_eq!(bound(&input("@bob #42")), None);
        assert_eq!(bound(&input("@example.com #48 29")), None);
        assert_eq!(bound(&input("mail @example.com #482913")), None);
        let forwarded = InsightInput {
            forward_at: Some(3),
            ..input("Hi\n-- Forwarded --\n@example.com #482913")
        };
        assert_eq!(bound(&forwarded), None);
    }
}
