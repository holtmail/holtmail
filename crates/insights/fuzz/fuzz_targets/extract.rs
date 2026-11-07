#![no_main]

use std::sync::{Arc, LazyLock};

use holtmail_insight::InsightExtractor;
use holtmail_insight_code::CodeExtractor;
use holtmail_insight_links::LinksExtractor;
use holtmail_insight_parcel::ParcelExtractor;
use holtmail_insight_pass::PassExtractor;
use holtmail_insight_schemaorg::SchemaOrgExtractor;
use holtmail_insight_unsubscribe::UnsubscribeExtractor;
use holtmail_insights::{MAX_ITEMS, MAX_ITEMS_JSON_BYTES, Runner};
use libfuzzer_sys::fuzz_target;

static RUNNER: LazyLock<Runner> = LazyLock::new(|| {
    let extractors: Vec<Arc<dyn InsightExtractor>> = vec![
        Arc::new(UnsubscribeExtractor::new()),
        Arc::new(SchemaOrgExtractor::new()),
        Arc::new(PassExtractor::new()),
        Arc::new(ParcelExtractor::new()),
        Arc::new(LinksExtractor::new()),
        Arc::new(CodeExtractor::new()),
    ];
    Runner::new(extractors)
});

fuzz_target!(|data: &[u8]| {
    let Ok(message) = holtmail_mime::parse(data) else {
        return;
    };
    let first = RUNNER.extract(&message);
    let second = RUNNER.extract(&message);
    assert_eq!(first, second, "two runs differ");
    assert!(first.items.items.len() <= MAX_ITEMS);
    assert!(first.items.to_json().len() <= MAX_ITEMS_JSON_BYTES);
    let _ = RUNNER.explain(&message);
});
