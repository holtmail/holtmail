use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use holtmail_insight::{Date, LocalOrZoned};

/// An ISO 8601 date or date-time as the message wrote it.
///
/// `2027-03-14T09:30:00+01:00` (or `Z`, `+0100`, `+01`) is `Zoned` with no zone name and is never
/// converted; `2027-03-14T09:30` is `Local`; a bare date is `Local` at 00:00. Seconds and a
/// fraction are optional, `T` may be a space. Anything else is `None`.
pub(crate) fn parse_time(text: &str) -> Option<LocalOrZoned> {
    let text = text.trim();
    let date_part = text.get(..10)?;
    let date = NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()?;
    let rest = text.get(10..)?;
    if rest.is_empty() {
        return Some(LocalOrZoned::Local(date.and_time(NaiveTime::MIN)));
    }
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (clock, offset) = split_offset(rest)?;
    let time = NaiveTime::parse_from_str(clock, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(clock, "%H:%M"))
        .ok()?;
    let naive = NaiveDateTime::new(date, time);
    match offset {
        None => Some(LocalOrZoned::Local(naive)),
        Some(offset) => offset
            .from_local_datetime(&naive)
            .single()
            .map(|at| LocalOrZoned::Zoned(at, None)),
    }
}

/// The calendar date of `text` in its own offset.
pub(crate) fn parse_date(text: &str) -> Option<Date> {
    parse_time(text).map(|at| Date(local_date(&at)))
}

/// The wall-clock date of a time, in its own offset.
pub(crate) fn local_date(at: &LocalOrZoned) -> NaiveDate {
    match at {
        LocalOrZoned::Local(at) => at.date(),
        LocalOrZoned::Zoned(at, _) => at.date_naive(),
    }
}

/// Whether `end` comes before `start`. Times of different kinds are compared as wall times.
pub(crate) fn ends_before(start: &LocalOrZoned, end: &LocalOrZoned) -> bool {
    match (start, end) {
        (LocalOrZoned::Zoned(start, _), LocalOrZoned::Zoned(end, _)) => end < start,
        _ => wall(end) < wall(start),
    }
}

fn wall(at: &LocalOrZoned) -> NaiveDateTime {
    match at {
        LocalOrZoned::Local(at) => *at,
        LocalOrZoned::Zoned(at, _) => at.naive_local(),
    }
}

fn split_offset(rest: &str) -> Option<(&str, Option<FixedOffset>)> {
    if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        return Some((clock, FixedOffset::east_opt(0)));
    }
    let Some(at) = rest.rfind(['+', '-']) else {
        return Some((rest, None));
    };
    let (clock, offset) = rest.split_at_checked(at)?;
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let digits: String = offset.chars().skip(1).filter(|c| *c != ':').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !matches!(digits.len(), 2 | 4) {
        return None;
    }
    let hours: i32 = digits.get(..2)?.parse().ok()?;
    let minutes: i32 = match digits.get(2..) {
        Some("") | None => 0,
        Some(m) => m.parse().ok()?,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    let offset = FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))?;
    Some((clock, Some(offset)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zoned(text: &str) -> String {
        match parse_time(text) {
            Some(LocalOrZoned::Zoned(at, None)) => at.to_rfc3339(),
            other => panic!("{text}: {other:?}"),
        }
    }

    fn local(text: &str) -> String {
        match parse_time(text) {
            Some(LocalOrZoned::Local(at)) => at.to_string(),
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn offsets_stay_as_written() {
        assert_eq!(
            zoned("2027-03-14T09:30:00+01:00"),
            "2027-03-14T09:30:00+01:00"
        );
        assert_eq!(zoned("2027-03-14T09:30-05:00"), "2027-03-14T09:30:00-05:00");
        assert_eq!(zoned("2027-03-14 09:30:00Z"), "2027-03-14T09:30:00+00:00");
        assert_eq!(
            zoned("2027-03-14T09:30:00.250+0530"),
            "2027-03-14T09:30:00.250+05:30"
        );
        assert_eq!(zoned("2027-03-14T23:30+09"), "2027-03-14T23:30:00+09:00");
    }

    #[test]
    fn times_without_offset_stay_local() {
        assert_eq!(local("2027-03-14T09:30:00"), "2027-03-14 09:30:00");
        assert_eq!(local("2027-03-14T09:30"), "2027-03-14 09:30:00");
        assert_eq!(local("2027-03-14"), "2027-03-14 00:00:00");
        assert_eq!(local(" 2027-03-14 "), "2027-03-14 00:00:00");
    }

    #[test]
    fn junk_is_refused() {
        for bad in [
            "",
            "14 March 2027",
            "2027-02-30",
            "2027-03-14T25:00",
            "2027-03-14T09:30+24:00",
            "2027-03-14T09:30+1",
            "2027-03-14X09:30",
            "2027-03-14T",
            "2027-03-1é",
            "２０２７-03-14",
        ] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dates_and_order() {
        assert_eq!(
            parse_date("2027-03-14T23:30:00-08:00").map(|d| d.0.to_string()),
            Some("2027-03-14".into())
        );
        let start = parse_time("2027-03-14T10:00:00+01:00").unwrap();
        let end = parse_time("2027-03-14T09:30:00Z").unwrap();
        assert!(!ends_before(&start, &end));
        let end = parse_time("2027-03-14T08:30:00Z").unwrap();
        assert!(ends_before(&start, &end));
        let local_end = parse_time("2027-03-14T09:00").unwrap();
        assert!(ends_before(&start, &local_end));
    }
}
