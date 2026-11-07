#![no_main]

use holtmail_html_scan::{forward_at, scan};
use holtmail_insight::MicrodataValue;
use holtmail_insight::limits::{MAX_LD_JSON_BLOCKS, MAX_MICRODATA_PROPERTIES};
use holtmail_sanitize::{Options, sanitize};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let html = String::from_utf8_lossy(data);
    let _ = forward_at(&html);
    let view = scan(&html);
    assert!(view.ld_json.len() <= MAX_LD_JSON_BLOCKS);
    assert!(view.ld_json_forward_at.is_none_or(|at| at < view.ld_json.len()));
    assert!(view.microdata.len() <= MAX_MICRODATA_PROPERTIES);
    let properties: usize = view.microdata.iter().map(|i| i.properties.len()).sum();
    assert!(properties <= MAX_MICRODATA_PROPERTIES);
    for item in &view.microdata {
        for property in &item.properties {
            if let MicrodataValue::Item(i) = property.value {
                assert!(i < view.microdata.len(), "an item index points outside the list");
            }
        }
    }
    let clean = sanitize(&html, &Options::default()).html;
    let again = scan(&clean);
    for anchor in &view.anchors {
        assert!(
            again.anchors.iter().any(|a| a.href == anchor.href),
            "an href the sanitizer removes was reported"
        );
    }
    let text = again
        .segments
        .iter()
        .flat_map(|s| s.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    for segment in &view.segments {
        for word in segment.text.split_whitespace() {
            assert!(
                text.contains(word),
                "segment text the sanitizer removes was reported"
            );
        }
    }
});
