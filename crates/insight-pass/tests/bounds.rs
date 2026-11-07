//! Property tests: no content panics or gives more than one item per attachment, and every
//! refusal stays cheap.

mod common;

use std::time::Instant;

use holtmail_insight::InsightExtractor;
use holtmail_insight_pass::PassExtractor;
use proptest::prelude::*;

use common::{boarding_json, honest_bomb, input, lying_bomb, pkpass};

fn check(contents: Vec<Vec<u8>>) {
    let count = contents.len();
    let message = input(contents);
    let extractor = PassExtractor::new();
    let items = extractor.extract(&message);
    assert!(items.len() <= count);
    let mut ordinals: Vec<_> = items.iter().map(|item| item.origin.clone()).collect();
    ordinals.dedup();
    assert_eq!(ordinals.len(), items.len());
    let _ = extractor.explain(&message);
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        check(vec![bytes.clone()]);
        let mut prefixed = b"PK\x03\x04".to_vec();
        prefixed.extend_from_slice(&bytes);
        check(vec![prefixed]);
    }

    #[test]
    fn damaged_passes_never_panic(
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..16),
        cut in any::<prop::sample::Index>(),
    ) {
        let original = pkpass(&[("pass.json", boarding_json().as_bytes())]);
        let mut damaged = original.clone();
        for (at, byte) in flips {
            let at = at.index(damaged.len());
            damaged[at] ^= byte;
        }
        let truncated = original[..cut.index(original.len())].to_vec();
        check(vec![damaged, truncated]);
    }
}

#[test]
fn bombs_are_refused_quickly() {
    let bombs = vec![lying_bomb(), honest_bomb()];
    for bomb in &bombs {
        assert!(bomb.len() < holtmail_insight::limits::MAX_PKPASS_BYTES);
    }
    let started = Instant::now();
    let items = PassExtractor::new().extract(&input(bombs));
    assert!(items.is_empty());
    assert!(started.elapsed().as_secs() < 5);
}
