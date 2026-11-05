use holtmail_core::Timestamp;

/// Reads a Graph `DateTimeOffset` (RFC 3339, e.g. `2025-03-01T09:30:00Z` or with an offset
/// and fractional seconds).
pub(crate) fn parse_utc(raw: &str) -> Option<Timestamp> {
    let raw = raw.trim();
    let (date, rest) = raw.split_once(['T', 't'])?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let zone_at = rest.find(['Z', 'z', '+', '-'])?;
    let (clock, zone) = rest.split_at(zone_at);
    let mut clock_parts = clock.splitn(3, ':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let seconds = clock_parts.next()?;
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let second: i64 = whole.parse().ok()?;
    let millis: i64 = format!("{fraction:0<3}")
        .get(..3)
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    let offset = match zone {
        "Z" | "z" => 0,
        _ => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let (h, m) = zone[1..].split_once(':')?;
            sign * (h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?) * 60
        }
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let secs =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some(Timestamp::from_millis(secs * 1000 + millis))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utc_dates() {
        assert_eq!(
            parse_utc("1970-01-01T00:00:00Z"),
            Some(Timestamp::from_millis(0))
        );
        assert_eq!(
            parse_utc("2025-03-01T09:30:05Z"),
            Some(Timestamp::from_secs(1_740_821_405))
        );
        assert_eq!(
            parse_utc("2025-03-01T10:30:05.25+01:00"),
            Some(Timestamp::from_millis(1_740_821_405_250))
        );
        assert_eq!(parse_utc("yesterday"), None);
        assert_eq!(parse_utc("2025-13-01T00:00:00Z"), None);
    }
}
