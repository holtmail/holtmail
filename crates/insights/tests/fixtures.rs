//! Stage A over hand-written messages.

use holtmail_insights::Runner;
use serde_json::{Value, json};

fn extract(name: &str) -> holtmail_insights::Extracted {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(path).unwrap();
    let message = holtmail_mime::parse(&raw).unwrap();
    Runner::new(Vec::new()).extract(&message)
}

#[test]
fn a_forged_result_is_recorded_as_written() {
    let extracted = extract("smtp-forged-ar-no-trust.eml");
    let auth: Value = serde_json::from_str(&extracted.auth).unwrap();
    assert_eq!(
        auth["results"],
        json!([{"pos": 0, "authserv": "mx.example.org", "dkim": [
            {"d": "bank.example.com", "i": null, "s": "s1", "b": "Forged12"}
        ]}])
    );
    assert_eq!(auth["signatures"][0]["d"], "relay.example.net");
    assert_eq!(auth["sig_cap_hit"], false);
    assert_eq!(auth["from_domain"], "bank.example.com");
    assert_eq!(
        extracted.from_address.as_deref(),
        Some("alerts@bank.example.com")
    );
    assert!(extracted.items.items.is_empty());
    assert!(extracted.signals.is_empty());
}
