use unicode_security::{
    GeneralSecurityProfile, MixedScript, RestrictionLevel, RestrictionLevelDetection,
    is_potential_mixed_script_confusable_char, skeleton,
};

/// How a host should be shown to a reader.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DisplayHost {
    /// The Unicode form when it is safe to show, otherwise the lowercase A-label form (or, for a
    /// host IDNA rejects, the input unchanged).
    pub host: String,
    /// Whether a label may be mistaken for another domain; the reader should then be warned.
    pub confusable: bool,
}

/// Decides how to show `host`, an A-label host as stored in an insight URL.
///
/// A label is confusable, following UTS 39 and Chromium's IDN display policy as far as the
/// Unicode security data allows, when it:
///
/// - has a character outside the General Security Profile's allowed identifier set;
/// - is neither single-script nor highly restrictive (Latin with Han and Hiragana/Katakana, with
///   Han and Bopomofo, or with Han and Hangul);
/// - mixes Latin with another script and has a character from the other script that is a
///   potential mixed-script confusable;
/// - has no ASCII letters and its confusable skeleton is all ASCII, a whole-script confusable such
///   as Cyrillic `аррӏе`.
///
/// Guarantees: when no label is confusable, `host` is the Unicode form and `confusable` is false;
/// when any label is, `host` is the A-label form and `confusable` is true. ASCII hosts and IP
/// literals come back lowercased and not confusable. A host that IDNA rejects (or an empty host)
/// comes back unchanged with `confusable` true. Deterministic; never panics.
pub fn display_host(host: &str) -> DisplayHost {
    let flagged = || DisplayHost {
        host: host.to_owned(),
        confusable: true,
    };
    let Some(ascii) = ascii_form(host) else {
        return flagged();
    };
    if ascii.parse::<std::net::Ipv4Addr>().is_ok() || ascii.starts_with('[') {
        return DisplayHost {
            host: ascii,
            confusable: false,
        };
    }
    let (unicode, result) = idna::domain_to_unicode(&ascii);
    if result.is_err() {
        return flagged();
    }
    if unicode.split('.').any(label_is_confusable) {
        DisplayHost {
            host: ascii,
            confusable: true,
        }
    } else {
        DisplayHost {
            host: unicode,
            confusable: false,
        }
    }
}

fn ascii_form(host: &str) -> Option<String> {
    if host.is_empty() || host.len() > crate::unwrap::MAX_INPUT_BYTES {
        return None;
    }
    match url::Host::parse(host).ok()? {
        url::Host::Domain(d) => Some(d.to_ascii_lowercase()),
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(ip) => Some(format!("[{ip}]")),
    }
}

fn label_is_confusable(label: &str) -> bool {
    if label.is_ascii() {
        return false;
    }
    if !label
        .chars()
        .all(|c| c == '-' || GeneralSecurityProfile::identifier_allowed(c))
    {
        return true;
    }
    let letters: String = label.chars().filter(|&c| c != '-').collect();
    if !letters
        .as_str()
        .check_restriction_level(RestrictionLevel::HighlyRestrictive)
    {
        return true;
    }
    let has_latin = label.chars().any(|c| c.is_ascii_alphabetic());
    if has_latin && !letters.as_str().is_single_script() {
        return label
            .chars()
            .any(|c| !c.is_ascii() && is_potential_mixed_script_confusable_char(c));
    }
    !has_latin && skeleton(label).all(|c| c.is_ascii())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(host: &str) -> (String, bool) {
        let d = display_host(host);
        (d.host, d.confusable)
    }

    fn a_label(unicode: &str) -> String {
        idna::domain_to_ascii(unicode).unwrap_or_default()
    }

    #[test]
    fn plain_ascii() {
        assert_eq!(show("example.com"), ("example.com".to_owned(), false));
        assert_eq!(
            show("Mail.Example.COM"),
            ("mail.example.com".to_owned(), false)
        );
        assert_eq!(
            show("my-shop.example.org"),
            ("my-shop.example.org".to_owned(), false)
        );
        assert_eq!(show("192.0.2.1"), ("192.0.2.1".to_owned(), false));
        assert_eq!(show("[2001:db8::1]"), ("[2001:db8::1]".to_owned(), false));
    }

    #[test]
    fn legitimate_idn_shown_in_unicode() {
        for unicode in [
            "пример.рф",
            "почта.пример.рф",
            "ελληνικά.example",
            "例え.テスト",
            "中文网.example",
            "한국어.example",
            "bücher.example",
            "straße-shop.example",
            "日本語abc.example",
            "ドメイン名例.example",
        ] {
            let ascii = a_label(unicode);
            assert!(ascii.contains("xn--"), "{unicode}");
            assert_eq!(show(&ascii), (unicode.to_owned(), false), "{unicode}");
        }
    }

    #[test]
    fn confusables_shown_as_a_labels() {
        for unicode in [
            "аррӏе.example",
            "раураl.example",
            "exаmple.com",
            "gοogle.example",
            "αβc.example",
            "ѕсоре.example",
            "example.ѕсоре",
            "аррӏе.рф",
            "ехаmрlе.com",
        ] {
            let ascii = a_label(unicode);
            assert!(ascii.contains("xn--"), "{unicode}");
            assert_eq!(show(&ascii), (ascii.clone(), true), "{unicode}");
        }
    }

    #[test]
    fn invalid_idna_is_flagged_unchanged() {
        for host in [
            "xn--zz.example",
            "XN--ZZ.example",
            "",
            "exa mple.com",
            "xn--abc-.example",
        ] {
            assert_eq!(show(host), (host.to_owned(), true), "{host}");
        }
    }
}
