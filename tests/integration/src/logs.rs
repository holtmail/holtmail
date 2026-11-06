//! Checks that Holtmail's logs carry no secret, address or subject a test seeded.

/// The values a test seeded and that must never appear in logs.
#[derive(Debug, Default, Clone)]
pub struct LogScanner {
    secrets: Vec<(String, String)>,
}

impl LogScanner {
    /// A scanner that already knows the owner's email and password.
    pub fn new() -> Self {
        Self::default()
            .secret("the owner's email", crate::api::OWNER_EMAIL)
            .secret("the owner's password", crate::api::OWNER_PASSWORD)
    }

    /// Adds `value`, described as `what` in failures.
    pub fn secret(mut self, what: &str, value: &str) -> Self {
        if !value.is_empty() {
            self.secrets.push((what.to_owned(), value.to_owned()));
        }
        self
    }

    /// Adds every value of `values`, described as `what`.
    pub fn secrets<'a>(mut self, what: &str, values: impl IntoIterator<Item = &'a str>) -> Self {
        for value in values {
            self = self.secret(what, value);
        }
        self
    }

    /// What appears in `logs`, as `(what, line)` pairs; empty when the logs are clean.
    pub fn findings(&self, logs: &str) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for line in logs.lines() {
            for (what, value) in &self.secrets {
                if line.contains(value.as_str()) {
                    found.push((what.clone(), line.to_owned()));
                }
            }
        }
        found
    }

    /// Fails the test when anything seeded appears in `logs`.
    #[track_caller]
    pub fn check(&self, logs: &str) {
        let found = self.findings(logs);
        assert!(
            found.is_empty(),
            "seeded values appear in the logs:\n{}",
            found
                .iter()
                .map(|(what, line)| format!("  {what}: {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_seeded_values_by_line() {
        let scanner = LogScanner::new().secret("a subject", "Quarterly plan 7f3a");
        let logs =
            "INFO synced 2 messages\nWARN failed: Quarterly plan 7f3a\nINFO owner@example.com\n";
        let found = scanner.findings(logs);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "a subject");
        assert_eq!(found[1].0, "the owner's email");
        assert!(scanner.findings("INFO synced 2 messages\n").is_empty());
    }
}
