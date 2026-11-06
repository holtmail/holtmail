//! The RFC 2369 and RFC 2919 readers never panic and only yield allowed schemes.

use holtmail_insight::UrlScheme;
use holtmail_insight_unsubscribe::{parse_list_id, parse_list_unsubscribe};
use proptest::prelude::*;

fn item() -> impl Strategy<Value = String> {
    prop_oneof![
        "<(https|http|mailto|file|javascript|ftp|HTTPS):[ -~]{0,30}>",
        "<https://[a-z]{1,8}\\.example\\.com/[a-z0-9/ ]{0,12}>",
        "<mailto:[a-z]{1,8}@example\\.org>",
        "[^<>,]{0,12}",
        "\\PC{0,16}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn list_unsubscribe_never_panics_on_any_text(value in "\\PC{0,200}") {
        let links = parse_list_unsubscribe(&value);
        prop_assert!(links.https.iter().all(|u| u.scheme() == UrlScheme::Https));
        prop_assert!(links.http.iter().all(|u| u.scheme() == UrlScheme::Http));
        prop_assert!(links.mailto.iter().all(|u| u.scheme() == UrlScheme::Mailto));
    }

    #[test]
    fn list_unsubscribe_yields_only_allowed_schemes(
        items in proptest::collection::vec(item(), 0..8),
        sep in "[ ,]{1,3}",
    ) {
        let value = items.join(&sep);
        let links = parse_list_unsubscribe(&value);
        prop_assert!(links.https.iter().all(|u| u.scheme() == UrlScheme::Https));
        prop_assert!(links.http.iter().all(|u| u.scheme() == UrlScheme::Http));
        prop_assert!(links.mailto.iter().all(|u| u.scheme() == UrlScheme::Mailto));
        for url in links.https.iter().chain(&links.http).chain(&links.mailto) {
            prop_assert!(!url.as_str().contains(char::is_whitespace));
        }
    }

    #[test]
    fn list_id_never_panics(value in "\\PC{0,120}") {
        if let Some(id) = parse_list_id(&value) {
            prop_assert!(id.len() <= 255 && id.contains('.'));
            prop_assert_eq!(id.to_ascii_lowercase(), id);
        }
    }
}
