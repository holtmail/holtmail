//! The invented fixtures of docs/INSIGHTS.md §15, parsed field by field.

use holtmail_auth_results::{AuthResults, DkimSignature, parse_auth_results, parse_dkim_signature};

fn fields(name: &str) -> Vec<(String, String)> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(path).expect("fixture");
    let header = raw.split("\r\n\r\n").next().unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in header.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push_str("\r\n");
                value.push_str(line);
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.to_owned(), value.to_owned()));
        }
    }
    out
}

fn results(name: &str) -> Vec<(usize, AuthResults)> {
    fields(name)
        .into_iter()
        .enumerate()
        .filter(|(_, (n, _))| n.eq_ignore_ascii_case("Authentication-Results"))
        .map(|(pos, (_, v))| (pos, parse_auth_results(&v).expect("authserv-id")))
        .collect()
}

fn signatures(name: &str) -> Vec<DkimSignature> {
    fields(name)
        .into_iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("DKIM-Signature"))
        .map(|(_, v)| parse_dkim_signature(&v).expect("tag-list"))
        .collect()
}

#[test]
fn trusted_top() {
    let ar = results("ar-trusted-top.eml");
    assert_eq!(ar.len(), 1);
    let (pos, top) = &ar[0];
    assert_eq!(*pos, 0);
    assert_eq!(top.authserv_id, "mx.example.org");
    let methods: Vec<_> = top.results.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["dkim", "spf", "dmarc"]);
    let dkim = &top.dkim()[0];
    assert!(dkim.is_pass());
    assert_eq!(dkim.d.as_deref(), Some("news.example.com"));
    assert_eq!(dkim.i.as_deref(), Some("@news.example.com"));
    assert_eq!(dkim.s.as_deref(), Some("s1"));
    assert_eq!(dkim.b_prefix(), Some("AbC12345"));

    let sigs = signatures("ar-trusted-top.eml");
    assert_eq!(sigs.len(), 1);
    let sig = &sigs[0];
    assert_eq!(sig.d.as_deref(), Some("news.example.com"));
    assert_eq!(sig.b_prefix(), dkim.b_prefix());
    assert!(sig.b.as_deref().unwrap().ends_with("wxyzABCDEFGHIJ=="));
    assert!(!sig.l);
    let counts = sig.h_counts();
    assert_eq!(counts["from"], 2);
    assert_eq!(counts["list-unsubscribe"], 2);
    assert_eq!(counts["list-unsubscribe-post"], 2);
}

#[test]
fn forged_top_untrusted() {
    let ar = results("ar-forged-top-untrusted.eml");
    assert_eq!(ar.len(), 1);
    assert_eq!(ar[0].1.authserv_id, "attacker.example.com");
    assert!(ar[0].1.dkim()[0].is_pass());
    assert!(!ar.iter().any(|(_, r)| r.authserv_id == "mx.example.org"));
    assert!(signatures("ar-forged-top-untrusted.eml").is_empty());
}

#[test]
fn forged_trusted_below() {
    let ar = results("ar-forged-trusted-below.eml");
    let positions: Vec<_> = ar
        .iter()
        .map(|(p, r)| (*p, r.authserv_id.as_str()))
        .collect();
    assert_eq!(positions, [(0, "mx.example.org"), (2, "mx.example.org")]);
    let top = &ar[0].1.dkim()[0];
    assert_eq!(top.result, "none");
    assert_eq!(top.d, None);
    assert!(ar[1].1.dkim()[0].is_pass());
}

#[test]
fn comments_everywhere() {
    let ar = results("ar-comments.eml");
    let top = &ar[0].1;
    assert_eq!(top.authserv_id, "mx.example.org");
    assert_eq!(top.version, Some(1));
    let dkim = top.dkim();
    assert_eq!(dkim.len(), 2);
    assert!(dkim[0].is_pass());
    assert_eq!(dkim[0].d.as_deref(), Some("shop.example.com"));
    assert_eq!(dkim[0].s.as_deref(), Some("mail"));
    assert_eq!(dkim[0].b.as_deref(), Some("Zz9+/aBc"));
    assert_eq!(dkim[1].result, "fail");
    assert_eq!(dkim[1].d.as_deref(), Some("lookalike.example.org"));
    assert_eq!(
        top.results[2].property("smtp", "mailfrom"),
        Some("orders@shop.example.com")
    );
    let sig = &signatures("ar-comments.eml")[0];
    assert!(
        sig.b
            .as_deref()
            .unwrap()
            .starts_with(dkim[0].b.as_deref().unwrap())
    );
}

#[test]
fn l_tag_is_recorded() {
    let sig = &signatures("dkim-l-tag-appended-link.eml")[0];
    assert!(sig.l);
    assert_eq!(sig.tag("l"), Some("64"));
    assert_eq!(sig.h, ["from", "to", "subject", "date"]);
    let ar = results("dkim-l-tag-appended-link.eml");
    assert_eq!(ar[0].1.dkim()[0].b_prefix(), sig.b_prefix());
}
