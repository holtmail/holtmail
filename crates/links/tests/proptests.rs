//! Property tests for the public API.

use holtmail_links::{display_host, registrable_domain, strip_tracking, tracker, unwrap};
use proptest::prelude::*;

fn on_allow_list(host: &str) -> bool {
    host == "www.google.com"
        || host == "urldefense.proofpoint.com"
        || host == "urldefense.com"
        || host.ends_with(".safelinks.protection.outlook.com")
}

fn host() -> impl Strategy<Value = String> {
    let label = "[a-z0-9]([a-z0-9-]{0,8}[a-z0-9])?";
    let plain = proptest::collection::vec(label, 1..4).prop_map(|labels| labels.join("."));
    let decoys = prop_oneof![
        Just("www.google.com".to_owned()),
        Just("google.com".to_owned()),
        Just("safelinks.protection.outlook.com".to_owned()),
        Just("nam12.safelinks.protection.outlook.com".to_owned()),
        Just("urldefense.com".to_owned()),
        Just("urldefense.proofpoint.com".to_owned()),
        Just("evil.example".to_owned()),
    ];
    prop_oneof![
        plain.clone(),
        (decoys.clone(), plain.clone()).prop_map(|(d, p)| format!("{d}.{p}")),
        (plain.clone(), decoys.clone()).prop_map(|(p, d)| format!("{p}{d}")),
        (plain, decoys).prop_map(|(p, d)| format!("{p}.{d}")),
    ]
}

fn tail() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("/url?q=https%3A%2F%2Fexample.com%2F".to_owned()),
        Just("/?url=https%3A%2F%2Fexample.com%2F".to_owned()),
        Just("/v1/url?u=https%3A%2F%2Fexample.com%2F".to_owned()),
        Just("/v2/url?u=https-3A__example.com_".to_owned()),
        Just("/v3/__https://example.com/__;!!x".to_owned()),
        "/[a-zA-Z0-9/._~%-]{0,20}(\\?[a-z_=&%0-9A-F]{0,30})?",
    ]
}

fn destination() -> impl Strategy<Value = String> {
    (
        prop_oneof![
            Just("example.com"),
            Just("example.org"),
            Just("shop.example.com")
        ],
        "[a-z0-9/_-]{0,16}",
        proptest::option::of("[a-z]{1,4}=[a-z0-9]{0,6}"),
    )
        .prop_map(|(h, p, q)| match q {
            Some(q) => format!("https://{h}/{p}?{q}"),
            None => format!("https://{h}/{p}"),
        })
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn wrap(kind: u8, inner: &str) -> String {
    match kind % 3 {
        0 => format!("https://www.google.com/url?q={}&sa=D", enc(inner)),
        1 => format!(
            "https://eur02.safelinks.protection.outlook.com/?url={}&data=x",
            enc(inner)
        ),
        _ => format!(
            "https://urldefense.proofpoint.com/v1/url?u={}&k=x",
            enc(inner)
        ),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn identity_off_the_allow_list(scheme in prop_oneof![Just("https"), Just("http")], host in host(), tail in tail()) {
        prop_assume!(!on_allow_list(&host));
        let url = format!("{scheme}://{host}{tail}");
        let u = unwrap(&url);
        prop_assert_eq!(u.destination, url);
        prop_assert_eq!(u.via, None);
    }

    #[test]
    fn idempotent_on_its_destination(kinds in proptest::collection::vec(any::<u8>(), 0..6), dest in destination()) {
        let mut url = dest;
        for kind in &kinds {
            url = wrap(*kind, &url);
        }
        let once = unwrap(&url);
        if kinds.is_empty() {
            prop_assert_eq!(once.via.as_deref(), None);
        } else {
            prop_assert!(once.via.is_some());
        }
        let twice = unwrap(&once.destination);
        if kinds.len() <= 4 {
            prop_assert_eq!(&twice.destination, &once.destination);
            prop_assert_eq!(twice.via, None);
        }
    }

    #[test]
    fn never_panics_on_strings(s in ".{0,200}") {
        let _ = unwrap(&s);
        let _ = strip_tracking(&s);
        let _ = tracker(&s);
        let _ = registrable_domain(&s);
        let _ = display_host(&s);
    }

    #[test]
    fn never_panics_on_wrapper_shapes(
        prefix in prop_oneof![
            Just("https://www.google.com/url?q="),
            Just("https://a.safelinks.protection.outlook.com/?url="),
            Just("https://urldefense.proofpoint.com/v1/url?u="),
            Just("https://urldefense.proofpoint.com/v2/url?u="),
            Just("https://urldefense.com/v3/__"),
        ],
        rest in "[a-zA-Z0-9:/%*_;!=&?#.+-]{0,80}",
    ) {
        let url = format!("{prefix}{rest}");
        let u = unwrap(&url);
        if u.via.is_none() {
            prop_assert_eq!(u.destination, url);
        } else {
            prop_assert!(u.destination.starts_with("http"));
        }
    }

    #[test]
    fn registrable_domain_is_a_label_suffix_and_fixed_point(
        labels in proptest::collection::vec("[a-zA-Z0-9]{1,6}|bücher|пример|例え", 0..4),
        suffix in prop_oneof![
            Just("com"), Just("co.uk"), Just("github.io"), Just("example"), Just("рф"),
            Just("XN--P1AI"), Just("uk"), Just("s3.amazonaws.com"), Just("jp"),
        ],
    ) {
        let mut parts = labels.clone();
        parts.push(suffix.to_owned());
        let host = parts.join(".");
        if let Some(domain) = registrable_domain(&host) {
            let ascii = idna::domain_to_ascii(&host.to_lowercase()).unwrap_or_default();
            prop_assert!(ascii == domain || ascii.ends_with(&format!(".{domain}")), "{} {}", ascii, domain);
            prop_assert_eq!(registrable_domain(&domain), Some(domain.clone()));
            prop_assert_eq!(domain.to_ascii_lowercase(), domain.clone());
        }
    }

    #[test]
    fn display_host_round_trips(labels in proptest::collection::vec("[a-z]{1,6}|bücher|пример|例え|аррӏе", 1..4)) {
        let host = format!("{}.example", labels.join("."));
        let ascii = idna::domain_to_ascii(&host).unwrap_or_default();
        let shown = display_host(&ascii);
        if shown.confusable {
            prop_assert_eq!(shown.host, ascii);
        } else {
            prop_assert_eq!(idna::domain_to_ascii(&shown.host).unwrap_or_default(), ascii);
        }
    }
}
