use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `at` as an HTTP date (RFC 9110 IMF-fixdate), whole seconds.
pub(crate) fn imf_fixdate(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{}, {day:02} {} {year:04} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// An HTTP date in IMF-fixdate form; `None` for anything else.
pub(crate) fn parse_imf_fixdate(text: &str) -> Option<SystemTime> {
    let mut parts = text.split_whitespace();
    let _weekday = parts.next()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == month_name)? as i64 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts.next()?.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if parts.next()? != "GMT" || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_http_dates() {
        let at = UNIX_EPOCH + Duration::from_secs(784_111_777);
        assert_eq!(imf_fixdate(at), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 GMT"), Some(at));
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(imf_fixdate(leap), "Thu, 29 Feb 2024 00:00:00 GMT");
        assert_eq!(parse_imf_fixdate(&imf_fixdate(leap)), Some(leap));
        assert_eq!(parse_imf_fixdate("Sunday, 06-Nov-94 08:49:37 GMT"), None);
        assert_eq!(parse_imf_fixdate("Sun, 06 Nov 1994 08:49:37 UTC"), None);
    }
}
