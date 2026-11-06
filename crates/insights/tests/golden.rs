//! The golden snapshot of docs/INSIGHTS.md §6.1: stage A rows of the whole corpus compared with
//! `tests/golden.txt`. `UPDATE_GOLDEN=1 cargo test -p holtmail-insights --test golden` rewrites it.

mod common;

use std::fmt::Write as _;
use std::path::PathBuf;

const UPDATE: &str = "UPDATE_GOLDEN";

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden.txt")
}

fn render() -> String {
    let runner = common::runner();
    let mut out = String::from(
        "# Stage A rows of tests/corpus. Rewrite with UPDATE_GOLDEN=1; see docs/README.md.\n",
    );
    let _ = writeln!(out, "extract_fp {}", runner.extract_fp());
    for file in common::files() {
        let extracted = runner.extract(&common::parse(&file));
        let _ = writeln!(out, "\n== {file}");
        let _ = writeln!(
            out,
            "outcome={:?} truncated={} from={} signals={} kinds={}",
            extracted.outcome,
            extracted.truncated,
            extracted.from_address.as_deref().unwrap_or("-"),
            extracted.signals.bits(),
            extracted.kinds.bits(),
        );
        let _ = writeln!(out, "auth {}", extracted.auth);
        let _ = writeln!(out, "items {}", extracted.items.to_json());
    }
    out
}

fn fingerprint(snapshot: &str) -> Option<&str> {
    snapshot
        .lines()
        .find_map(|line| line.strip_prefix("extract_fp "))
}

fn changed_cases(stored: &str, current: &str) -> Vec<String> {
    let blocks = |text: &str| -> Vec<(String, String)> {
        text.split("\n== ")
            .skip(1)
            .map(|block| {
                let (name, body) = block.split_once('\n').unwrap_or((block, ""));
                (name.to_owned(), body.to_owned())
            })
            .collect()
    };
    let stored = blocks(stored);
    let current = blocks(current);
    let mut out: Vec<String> = current
        .iter()
        .filter(|case| !stored.contains(case))
        .map(|(name, _)| name.clone())
        .collect();
    out.extend(
        stored
            .iter()
            .filter(|(name, _)| !current.iter().any(|(n, _)| n == name))
            .map(|(name, _)| format!("{name} (gone)")),
    );
    out
}

#[test]
fn rows_match_the_golden_snapshot() {
    let current = render();
    let path = snapshot_path();
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(&path, &current).unwrap();
        return;
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_default();
    if stored == current {
        return;
    }
    let changed = changed_cases(&stored, &current);
    let same_fingerprint =
        fingerprint(&stored).is_some() && fingerprint(&stored) == fingerprint(&current);
    if same_fingerprint && !changed.is_empty() {
        panic!(
            "stage A output changed while extract_fp stayed the same, so stored rows would not be \
             recomputed. Bump the VERSION of what changed (holtmail-insights, html-scan, \
             auth-results or the extractor), then rewrite the snapshot with {UPDATE}=1.\n\
             changed: {}",
            changed.join(", ")
        );
    }
    panic!(
        "the golden snapshot is out of date: review the change and rewrite it with {UPDATE}=1.\n\
         changed: {}",
        if changed.is_empty() {
            "fingerprint only".to_owned()
        } else {
            changed.join(", ")
        }
    );
}
