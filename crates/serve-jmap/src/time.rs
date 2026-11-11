use chrono::{DateTime, SecondsFormat, Utc};

/// A JMAP `UTCDate` for milliseconds since the epoch, to the second.
pub(crate) fn utc_date(millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Milliseconds since the epoch of an RFC 3339 date (`UTCDate` or `Date`).
pub(crate) fn parse_date(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(utc_date(1_793_610_000_000), "2026-11-02T09:00:00Z");
        assert_eq!(parse_date("2026-11-02T09:00:00Z"), Some(1_793_610_000_000));
        assert_eq!(
            parse_date("2026-11-02T10:00:00+01:00"),
            Some(1_793_610_000_000)
        );
        assert_eq!(parse_date("yesterday"), None);
    }
}
