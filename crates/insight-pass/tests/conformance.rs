//! The conformance suite with built pkpass attachments: boarding and event passes, every cap and
//! refusal, and several passes in one message.

mod common;

use holtmail_insight::InsightExtractor;
use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight_pass::{ALGORITHM, ID, PassExtractor};

use common::{boarding_json, event_json, fixtures, input, pkpass};

#[test]
fn conformance_suite_passes() {
    let mut cases: Vec<Case> = fixtures()
        .into_iter()
        .map(|(name, content, found)| {
            let expect = if found {
                Expect::Found
            } else {
                Expect::Nothing
            };
            Case::new(name, input(vec![content]), expect)
        })
        .collect();
    let boarding = pkpass(&[("pass.json", boarding_json().as_bytes())]);
    let event = pkpass(&[("pass.json", event_json().as_bytes())]);
    cases.push(Case::new(
        "pkpass-nine-attachments",
        input(vec![boarding.clone(); 9]),
        Expect::Found,
    ));
    cases.push(Case::new(
        "pkpass-mixed",
        input(vec![common::lying_bomb(), event, Vec::new(), boarding]),
        Expect::Found,
    ));
    conformance::extractor(&PassExtractor::new(), &cases);
}

#[test]
fn id_and_version_are_stable() {
    let extractor = PassExtractor::new();
    assert_eq!(extractor.id(), ID);
    assert_eq!(ID, "pass");
    let version = extractor.version();
    assert!(version.starts_with(&format!("{ALGORITHM}+")));
    assert_eq!(version.len(), 2 + 12);
    assert_eq!(PassExtractor.version(), version);
}

#[test]
fn at_most_eight_attachments_are_read() {
    let boarding = pkpass(&[("pass.json", boarding_json().as_bytes())]);
    let items = PassExtractor::new().extract(&input(vec![boarding; 12]));
    assert_eq!(
        items.len(),
        holtmail_insight::limits::MAX_PKPASS_ATTACHMENTS
    );
}

#[test]
fn a_repeated_ordinal_gives_one_item() {
    let boarding = pkpass(&[("pass.json", boarding_json().as_bytes())]);
    let mut message = input(vec![boarding.clone(), boarding]);
    message.attachments[1].ordinal = 0;
    assert_eq!(PassExtractor::new().extract(&message).len(), 1);
}
