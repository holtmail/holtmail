use serde::{Deserialize, Serialize};

/// The key that names one event, or one changed instance of a recurring event, in every calendar
/// it is copied into.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventKey {
    /// iCalendar `UID`, as written.
    pub uid: String,
    /// `RECURRENCE-ID` of a changed instance as a UTC time (`2025-03-11T08:00:00Z`) when its time
    /// zone is known, else as written; `None` for the event itself.
    pub recurrence_id: Option<String>,
}

/// An email address in the form used to match contacts with each other and with mail: trimmed,
/// without a `mailto:` prefix, lowercased as a whole.
///
/// Lowercasing the local part is not promised by RFC 5321, but every common provider treats it
/// that way. `None` when the text is not one address (no `@`, or whitespace inside).
pub fn normalize_email(text: &str) -> Option<String> {
    let text = text.trim();
    let text = match text.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("mailto:") => &text[7..],
        _ => text,
    };
    let text = text.trim().trim_start_matches('<').trim_end_matches('>');
    let (local, domain) = text.rsplit_once('@')?;
    if local.is_empty() || domain.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    Some(text.to_lowercase())
}

/// A phone number in E.164 form (`+15550100`), when the text says its country.
///
/// Accepts `+`, `00` and `tel:` prefixes, and drops spaces, dots, dashes, slashes, brackets and a
/// trailing extension (`ext`, `x`, `;ext=`). Gives `None` for numbers written without a country
/// code, because the country cannot be known without guessing, and for results that are not 8 to
/// 15 digits long. No per-country rules are applied, so a well-formed but unassigned number
/// passes.
pub fn normalize_phone(text: &str) -> Option<String> {
    let mut text = text.trim();
    if text
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("tel:"))
    {
        text = &text[4..];
    }
    let lower = text.to_ascii_lowercase();
    let cut = [";ext=", ";", "ext", "x", "#", ","]
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .unwrap_or(text.len());
    let text = text[..cut].trim();
    let (international, rest) = if let Some(rest) = text.strip_prefix('+') {
        (true, rest)
    } else if let Some(rest) = text.strip_prefix("00") {
        (true, rest)
    } else {
        (false, text)
    };
    if !international {
        return None;
    }
    let mut digits = String::new();
    for c in rest.chars() {
        match c {
            '0'..='9' => digits.push(c),
            ' ' | '.' | '-' | '/' | '(' | ')' | '\u{a0}' => {}
            _ => return None,
        }
    }
    if !(8..=15).contains(&digits.len()) || digits.starts_with('0') {
        return None;
    }
    Some(format!("+{digits}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_emails() {
        assert_eq!(
            normalize_email(" MAILTO:Ada.Lovelace@Example.COM ").as_deref(),
            Some("ada.lovelace@example.com")
        );
        assert_eq!(
            normalize_email("<bob@example.org>").as_deref(),
            Some("bob@example.org")
        );
        assert_eq!(normalize_email("not an address"), None);
        assert_eq!(normalize_email("a b@example.com"), None);
        assert_eq!(normalize_email("@example.com"), None);
        assert_eq!(
            normalize_email("abcdef\u{20ac}@example.com").as_deref(),
            Some("abcdef\u{20ac}@example.com")
        );
        assert_eq!(normalize_phone("abc\u{20ac}1"), None);
    }

    #[test]
    fn normalizes_international_phones() {
        for (written, e164) in [
            ("+1 555 0100", Some("+15550100")),
            ("+1 (555) 010-0199", Some("+15550100199")),
            ("0044 20 7946 0000", Some("+442079460000")),
            ("tel:+1-555-0100;ext=12", Some("+15550100")),
            ("+1 555 0100 ext 7", Some("+15550100")),
            ("+49 30 1234567 x9", Some("+49301234567")),
            ("555 0100", None),
            ("(555) 0100", None),
            ("+1 555 CALL", None),
            ("+12", None),
            ("+1234567890123456", None),
        ] {
            assert_eq!(normalize_phone(written).as_deref(), e164, "{written}");
        }
    }
}
