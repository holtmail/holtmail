//! The precision gate of docs/INSIGHTS.md §15: stage A with the code and unsubscribe extractors
//! over the invented corpus in `tests/corpus`, compared with `labels.toml`.

mod common;

use std::collections::BTreeSet;
use std::fmt::Write as _;

use common::{Label, UnsubscribeLabel};
use holtmail_insight::{Confidence, InsightKind};

const MIN_CASES: usize = 150;
const MIN_HARD_NEGATIVES: usize = 50;
const CODE_PRECISION: f64 = 0.98;
const CODE_RECALL: f64 = 0.85;
const UNSUBSCRIBE_PRECISION: f64 = 1.0;
const UNSUBSCRIBE_RECALL: f64 = 1.0;

#[derive(Default)]
struct Confusion {
    hit: Vec<String>,
    wrong: Vec<String>,
    missed: Vec<String>,
    spurious: Vec<String>,
    quiet: usize,
    possible: Vec<String>,
}

impl Confusion {
    fn precision(&self) -> f64 {
        let predicted = self.hit.len() + self.wrong.len() + self.spurious.len();
        if predicted == 0 {
            return 1.0;
        }
        self.hit.len() as f64 / predicted as f64
    }

    fn recall(&self) -> f64 {
        let expected = self.hit.len() + self.wrong.len() + self.missed.len();
        if expected == 0 {
            return 1.0;
        }
        self.hit.len() as f64 / expected as f64
    }

    fn summary(&self, kind: &str, out: &mut String) {
        let _ = writeln!(
            out,
            "{kind}: precision {:.3}, recall {:.3}; {} right, {} wrong value, {} missed, {} spurious, {} correctly empty",
            self.precision(),
            self.recall(),
            self.hit.len(),
            self.wrong.len(),
            self.missed.len(),
            self.spurious.len(),
            self.quiet,
        );
        for (name, cases) in [
            ("wrong value", &self.wrong),
            ("missed", &self.missed),
            ("spurious", &self.spurious),
            ("only Possible", &self.possible),
        ] {
            if !cases.is_empty() {
                let _ = writeln!(out, "  {name}: {}", cases.join(", "));
            }
        }
    }
}

fn record(confusion: &mut Confusion, case: &str, expected: bool, found: bool, equal: bool) {
    let case = case.to_owned();
    match (expected, found) {
        (true, true) if equal => confusion.hit.push(case),
        (true, true) => confusion.wrong.push(case),
        (true, false) => confusion.missed.push(case),
        (false, true) => confusion.spurious.push(case),
        (false, false) => confusion.quiet += 1,
    }
}

fn unsubscribe_label(kind: &InsightKind) -> Option<UnsubscribeLabel> {
    match kind {
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } => Some(UnsubscribeLabel {
            https: https.as_ref().map(|u| u.as_str().to_owned()),
            http: http.as_ref().map(|u| u.as_str().to_owned()),
            mailto: mailto.clone(),
            one_click: *one_click,
            list_id: list_id.clone(),
        }),
        _ => None,
    }
}

#[test]
fn corpus_meets_the_precision_gate() {
    let labels = common::labels();
    check_labels(&labels);
    let runner = common::runner();

    let mut codes = Confusion::default();
    let mut unsubscribe = Confusion::default();
    for label in &labels {
        let case = label.file.trim_end_matches(".eml");
        let message = common::parse(&label.file);
        let first = runner.extract(&message);
        let second = runner.extract(&message);
        assert_eq!(first, second, "{case}: two runs differ");
        assert_eq!(
            first.items.to_json(),
            second.items.to_json(),
            "{case}: two runs differ"
        );

        let mut shown = None;
        let mut found_unsubscribe = None;
        for item in &first.items.items {
            match &item.kind {
                InsightKind::Code {
                    value,
                    origin_bound,
                } => {
                    if item.confidence >= Confidence::Likely && !item.forwarded {
                        assert!(shown.is_none(), "{case}: more than one code");
                        shown = Some((value.clone(), origin_bound.clone()));
                    } else {
                        codes.possible.push(case.to_owned());
                    }
                }
                kind @ InsightKind::Unsubscribe { .. } => {
                    assert!(
                        found_unsubscribe.is_none(),
                        "{case}: more than one unsubscribe item"
                    );
                    found_unsubscribe = unsubscribe_label(kind);
                }
                _ => {}
            }
        }

        let expected_code = label.code.clone().map(|code| (code, label.origin.clone()));
        record(
            &mut codes,
            case,
            expected_code.is_some(),
            shown.is_some(),
            shown == expected_code,
        );
        record(
            &mut unsubscribe,
            case,
            label.unsubscribe.is_some(),
            found_unsubscribe.is_some(),
            found_unsubscribe == label.unsubscribe,
        );
    }

    let mut summary = format!("corpus: {} cases\n", labels.len());
    codes.summary("codes", &mut summary);
    unsubscribe.summary("unsubscribe", &mut summary);
    println!("{summary}");

    assert!(
        codes.precision() >= CODE_PRECISION && codes.recall() >= CODE_RECALL,
        "codes below the gate ({CODE_PRECISION} / {CODE_RECALL})\n{summary}"
    );
    assert!(
        unsubscribe.precision() >= UNSUBSCRIBE_PRECISION
            && unsubscribe.recall() >= UNSUBSCRIBE_RECALL,
        "unsubscribe below the gate\n{summary}"
    );
}

fn check_labels(labels: &[Label]) {
    let files: BTreeSet<String> = common::files().into_iter().collect();
    let labelled: BTreeSet<String> = labels.iter().map(|l| l.file.clone()).collect();
    assert_eq!(labelled.len(), labels.len(), "a file is labelled twice");
    assert_eq!(files, labelled, "every .eml file needs exactly one label");
    assert!(
        labels.len() >= MIN_CASES,
        "{} cases, the gate needs {MIN_CASES}",
        labels.len()
    );
    for label in labels {
        assert!(
            label.code.is_none() || label.negative.is_none(),
            "{}: a code and a negative family",
            label.file
        );
        assert!(
            label.origin.is_none() || label.code.is_some(),
            "{}: an origin without a code",
            label.file
        );
    }
    let negatives = labels.iter().filter(|l| l.negative.is_some()).count();
    assert!(
        negatives >= MIN_HARD_NEGATIVES,
        "{negatives} hard negatives, the gate needs {MIN_HARD_NEGATIVES}"
    );
}
