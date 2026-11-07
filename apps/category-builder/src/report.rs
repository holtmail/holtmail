use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use holtmail_category::{Topic, TopicSet};
use holtmail_category_data::Row;

use crate::build::Skip;
use crate::gate::Unreviewed;
use crate::golden::Miss;
use crate::industries::{Pass, PassStats};

/// What a build or a check found. It holds only Wikidata- or UT1-derived domains, counts and
/// timings; nothing in it comes from mail.
#[derive(Debug, Clone, Default)]
pub(crate) struct Report {
    pub command: &'static str,
    pub version: String,
    pub previous_version: Option<String>,
    pub first_release: bool,
    pub passes: BTreeMap<Pass, PassStats>,
    pub queries: usize,
    pub requests: usize,
    pub skipped: BTreeMap<Skip, usize>,
    pub crowded: Vec<(String, usize)>,
    pub unreviewed: Vec<Unreviewed>,
    pub dropped: usize,
    pub golden: usize,
    pub misses: Vec<Miss>,
    pub rows: Vec<(Topic, usize)>,
    pub total_rows: usize,
    pub bytes: usize,
    pub elapsed: Option<Duration>,
    pub outcome: String,
    pub notes: Vec<String>,
}

fn names(topics: TopicSet) -> String {
    topics.iter().map(Topic::name).collect::<Vec<_>>().join(",")
}

/// Rows per topic, in bit order, every topic listed.
pub(crate) fn per_topic(rows: &[Row]) -> Vec<(Topic, usize)> {
    Topic::ALL
        .iter()
        .map(|t| (*t, rows.iter().filter(|r| r.topics.contains(*t)).count()))
        .collect()
}

impl Report {
    /// The report as plain text.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "holtmail-category-builder {}", self.command);
        let _ = writeln!(out, "outcome: {}", self.outcome);
        let _ = writeln!(out, "version: {}", self.version);
        if let Some(previous) = &self.previous_version {
            let _ = writeln!(out, "previous version: {previous}");
        }
        for note in &self.notes {
            let _ = writeln!(out, "{note}");
        }
        if self.first_release {
            let _ = writeln!(
                out,
                "first release: every unreviewed sensitive topic stripped"
            );
        }
        let _ = writeln!(out, "rows: {}", self.total_rows);
        for (topic, count) in &self.rows {
            let _ = writeln!(out, "  {topic}: {count}");
        }
        if self.bytes > 0 {
            let _ = writeln!(out, "compressed size: {} bytes", self.bytes);
        }
        if let Some(elapsed) = self.elapsed {
            let _ = writeln!(out, "run time: {} s", elapsed.as_secs());
        }
        if self.queries > 0 {
            let _ = writeln!(
                out,
                "queries: {}, HTTP requests: {}",
                self.queries, self.requests
            );
        }
        for (pass, stats) in &self.passes {
            let _ = writeln!(
                out,
                "pass {pass}: {} roots, {} classes, {} batches, {} results, {} organisations",
                stats.roots, stats.classes, stats.batches, stats.bindings, stats.organisations
            );
        }
        if !self.skipped.is_empty() {
            let _ = writeln!(out, "websites skipped:");
            for (why, count) in &self.skipped {
                let _ = writeln!(out, "  {why}: {count}");
            }
        }
        if !self.crowded.is_empty() {
            let _ = writeln!(
                out,
                "domains with the most organisations (check for shared platforms):"
            );
            for (domain, count) in &self.crowded {
                let _ = writeln!(out, "  {domain}: {count}");
            }
        }
        let _ = writeln!(
            out,
            "golden: {} of {} as expected",
            self.golden.saturating_sub(self.misses.len()),
            self.golden
        );
        for miss in &self.misses {
            let found = miss.found.map_or_else(|| "missing".to_owned(), names);
            let expected = miss.expected.map_or_else(|| "no row".to_owned(), names);
            let _ = writeln!(out, "  {}: expected {expected}, found {found}", miss.domain);
        }
        let _ = writeln!(
            out,
            "unreviewed sensitive rows: {} ({} rows dropped with nothing left)",
            self.unreviewed.len(),
            self.dropped
        );
        for (domain, topics) in &self.unreviewed {
            let _ = writeln!(out, "  {domain}\t{}", names(*topics));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counts_and_lists() {
        let report = Report {
            command: "wikidata",
            version: "20261107".into(),
            outcome: "written".into(),
            total_rows: 2,
            rows: per_topic(&[Row {
                domain: "example.com".into(),
                topics: TopicSet::EMPTY.with(Topic::Travel),
                source: "wikidata:Q9000001".into(),
            }]),
            skipped: BTreeMap::from([(Skip::Path, 3)]),
            unreviewed: vec![("bank.example".into(), TopicSet::EMPTY.with(Topic::Finance))],
            golden: 2,
            misses: vec![
                Miss {
                    domain: "example.org".into(),
                    expected: Some(TopicSet::EMPTY.with(Topic::Jobs)),
                    found: None,
                },
                Miss {
                    domain: "mail.example".into(),
                    expected: None,
                    found: Some(TopicSet::EMPTY.with(Topic::Social)),
                },
            ],
            ..Report::default()
        };
        let text = report.render();
        assert!(text.contains("  travel: 1\n"));
        assert!(text.contains("  a page, not a site root: 3\n"));
        assert!(text.contains("  bank.example\tfinance\n"));
        assert!(text.contains("golden: 0 of 2 as expected\n"));
        assert!(text.contains("  example.org: expected jobs, found missing\n"));
        assert!(text.contains("  mail.example: expected no row, found social\n"));
    }
}
