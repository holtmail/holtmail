//! Cron expressions for the `schedule` trigger.

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// A five-field cron expression: minute, hour, day of month, month, day of week.
///
/// Each field takes `*`, a number, a range `a-b`, a step `*/n` or `a-b/n`, and lists of those
/// separated by commas. Day of week is 0 to 7, both 0 and 7 meaning Sunday. When both day
/// fields are restricted, a day matches if either does, as in classic cron.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    days_any: bool,
    weekdays_any: bool,
}

/// Why a cron expression was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cron field {field}: {reason}")]
pub struct CronError {
    /// The field, 1 to 5; 0 for the expression as a whole.
    pub field: usize,
    /// What is wrong.
    pub reason: String,
}

fn bad(field: usize, reason: impl Into<String>) -> CronError {
    CronError {
        field,
        reason: reason.into(),
    }
}

fn parse_field(text: &str, field: usize, min: u32, max: u32) -> Result<(u64, bool), CronError> {
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s.parse().map_err(|_| bad(field, "step is not a number"))?;
                if step == 0 {
                    return Err(bad(field, "step is zero"));
                }
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let a: u32 = a.parse().map_err(|_| bad(field, "not a number"))?;
            let b: u32 = b.parse().map_err(|_| bad(field, "not a number"))?;
            (a, b)
        } else {
            let v: u32 = range.parse().map_err(|_| bad(field, "not a number"))?;
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            return Err(bad(field, format!("out of range {min}-{max}")));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, text == "*"))
}

impl Cron {
    /// Parses an expression.
    pub fn parse(text: &str) -> Result<Cron, CronError> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(bad(0, "needs five fields"));
        }
        let (minutes, _) = parse_field(fields[0], 1, 0, 59)?;
        let (hours, _) = parse_field(fields[1], 2, 0, 23)?;
        let (days, days_any) = parse_field(fields[2], 3, 1, 31)?;
        let (months, _) = parse_field(fields[3], 4, 1, 12)?;
        let (mut weekdays, weekdays_any) = parse_field(fields[4], 5, 0, 7)?;
        if weekdays & (1 << 7) != 0 {
            weekdays |= 1;
        }
        Ok(Cron {
            minutes,
            hours,
            days,
            months,
            weekdays,
            days_any,
            weekdays_any,
        })
    }

    fn day_matches(&self, day: u32, weekday: u32) -> bool {
        let d = self.days & (1 << day) != 0;
        let w = self.weekdays & (1 << weekday) != 0;
        match (self.days_any, self.weekdays_any) {
            (true, true) => true,
            (true, false) => w,
            (false, true) => d,
            (false, false) => d || w,
        }
    }

    /// Whether the minute containing `at`, read in `tz`, is one the expression names.
    pub fn matches(&self, at: DateTime<Utc>, tz: Tz) -> bool {
        let local = tz.from_utc_datetime(&at.naive_utc());
        self.minutes & (1 << local.minute()) != 0
            && self.hours & (1 << local.hour()) != 0
            && self.months & (1 << local.month()) != 0
            && self.day_matches(local.day(), local.weekday().num_days_from_sunday())
    }

    /// The first whole minute strictly after `after` that the expression names, read in `tz`;
    /// `None` when there is none within about four years (such as `0 0 30 2 *`). Minutes a
    /// daylight-saving change skips never fire; minutes it repeats fire twice.
    pub fn next_after(&self, after: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        let start = after.timestamp().div_euclid(60) * 60 + 60;
        let mut t = DateTime::<Utc>::from_timestamp(start, 0)?;
        let end = t + Duration::days(4 * 366 + 1);
        while t < end {
            let local = tz.from_utc_datetime(&t.naive_utc());
            if self.months & (1 << local.month()) == 0
                || !self.day_matches(local.day(), local.weekday().num_days_from_sunday())
            {
                let to_midnight = (24 * 60 - (local.hour() * 60 + local.minute())) as i64;
                t += Duration::minutes(if to_midnight > 120 {
                    to_midnight - 60
                } else {
                    to_midnight
                });
                continue;
            }
            if self.hours & (1 << local.hour()) == 0 {
                t += Duration::minutes((60 - local.minute()) as i64);
                continue;
            }
            if self.minutes & (1 << local.minute()) != 0 {
                return Some(t);
            }
            t += Duration::minutes(1);
        }
        None
    }
}
