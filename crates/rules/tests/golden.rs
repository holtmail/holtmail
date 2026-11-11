//! Golden plans: each `tests/golden/<case>.json` holds a flow, an event, facts and query
//! answers; its plan must equal `tests/golden/<case>.plan.json`. `UPDATE_GOLDEN=1` rewrites the
//! expected plans.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use holtmail_rules::{Event, FactSheet, FactsError, Flow, WithMatcher, evaluate, validate};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    flow: Flow,
    event: Event,
    facts: FactSheet,
    #[serde(default)]
    queries: BTreeMap<String, bool>,
}

#[test]
fn golden_plans() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut cases: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && !p.to_string_lossy().ends_with(".plan.json")
        })
        .collect();
    cases.sort();
    assert!(cases.len() >= 5);
    let mut failures = Vec::new();
    for path in cases {
        let case: Case = serde_json::from_str(&fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        validate(&case.flow).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        let queries = case.queries.clone();
        let facts = WithMatcher(case.facts, move |q: &str| {
            queries
                .get(q)
                .copied()
                .ok_or_else(|| FactsError::Unavailable(q.to_owned()))
        });
        let plan = evaluate(&case.flow, &case.event, &facts).unwrap();
        let again = evaluate(&case.flow, &case.event, &facts).unwrap();
        assert_eq!(plan, again, "{}: not deterministic", path.display());
        let got = serde_json::to_string_pretty(&plan).unwrap() + "\n";
        let expected_path = path.with_extension("plan.json");
        if update {
            fs::write(&expected_path, &got).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&expected_path).unwrap_or_default();
        if expected != got {
            failures.push(format!("{}:\n{got}", expected_path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "golden plans differ:\n{}",
        failures.join("\n")
    );
}
