//! Look-alike fixtures from the insights design: wrapper hosts that only resemble the allow-list
//! and homograph hosts.

use holtmail_links::{display_host, registrable_domain, tracker, unwrap};

#[test]
fn wrapper_look_alikes_are_not_unwrapped() {
    for url in [
        "https://evil.example/url?q=https%3A%2F%2Fexample.com%2Freset",
        "https://safelinks.protection.outlook.com.evil.example/?url=https%3A%2F%2Fexample.com%2F",
        "https://www.google.com.evil.example/url?q=https%3A%2F%2Fexample.com%2F",
        "https://urldefense.com@evil.example/v3/__https://example.com/__;!!x",
        "https://evil.example/?redirect=https://www.google.com/url?q=https://example.com/",
    ] {
        let u = unwrap(url);
        assert_eq!(u.destination, url);
        assert_eq!(u.via, None);
    }
}

#[test]
fn wrapper_to_a_tracker_keeps_the_tracker() {
    let url = "https://nam12.safelinks.protection.outlook.com/?url=https%3A%2F%2Fu1.ct.sendgrid.net%2Fls%2Fclick%3Fupn%3Dabc";
    let u = unwrap(url);
    assert_eq!(u.via.as_deref(), Some("outlook.com"));
    assert_eq!(u.destination, "https://u1.ct.sendgrid.net/ls/click?upn=abc");
    assert_eq!(tracker(&u.destination), Some("SendGrid"));
}

#[test]
fn homograph_destination() {
    let url = "https://www.google.com/url?q=https%3A%2F%2F%D0%B0%D1%80%D1%80%D3%8F%D0%B5.example%2Fsign-in";
    let u = unwrap(url);
    assert_eq!(u.via.as_deref(), Some("google.com"));
    let parsed = url::Url::parse(&u.destination).expect("destination parses");
    let host = parsed.host_str().unwrap_or_default();
    assert!(host.starts_with("xn--"));
    let shown = display_host(host);
    assert!(shown.confusable);
    assert_eq!(shown.host, host);
    assert_eq!(registrable_domain(host).as_deref(), Some(host));
}
