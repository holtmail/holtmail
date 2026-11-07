#![no_main]

use bytes::Bytes;
use holtmail_insight::{AttachmentMeta, InsightExtractor, InsightInput};
use holtmail_insight_pass::PassExtractor;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = InsightInput {
        attachments: vec![AttachmentMeta {
            ordinal: 0,
            filename: Some("pass.pkpass".into()),
            content_type: "application/vnd.apple.pkpass".into(),
            size: data.len() as u64,
            content: Some(Bytes::copy_from_slice(data)),
        }],
        ..InsightInput::default()
    };
    let extractor = PassExtractor::new();
    let first = extractor.extract(&input);
    let second = extractor.extract(&input);
    assert_eq!(first, second, "two runs differ");
    assert!(first.len() <= 1);
    let _ = extractor.explain(&input);
});
