use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use holtmail_pim::{CalendarObject, EventTime};

use crate::zone::{Zone, parse_local};

/// An event time read back from what [`holtmail_pim`] kept of it.
#[derive(Debug, Clone)]
pub(crate) struct Moment {
    /// The wall-clock time; midnight for a date.
    pub(crate) local: NaiveDateTime,
    /// Whether it is a date without a time.
    pub(crate) date_only: bool,
    /// The zone `local` is read in.
    pub(crate) zone: Zone,
}

impl Moment {
    pub(crate) fn read(object: &CalendarObject, time: &EventTime) -> Option<Moment> {
        let value = time.value.as_str();
        if value.len() == 10 {
            let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
            return Some(Moment {
                local: date.and_time(NaiveTime::MIN),
                date_only: true,
                zone: Zone::Floating,
            });
        }
        let local = parse_local(value)?;
        let suffix = &value[19..];
        let zone = if suffix == "Z" {
            Zone::Fixed(0)
        } else if !suffix.is_empty() {
            Zone::of(object, suffix)
        } else {
            match &time.time_zone {
                Some(tzid) => Zone::of(object, tzid),
                None => Zone::Floating,
            }
        };
        Some(Moment {
            local,
            date_only: false,
            zone,
        })
    }

    pub(crate) fn utc(&self) -> i64 {
        self.zone.to_utc(self.local)
    }
}

/// An `UNTIL` value: a UTC time, or a date or local time compared with local times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    Utc(NaiveDateTime),
    Local(NaiveDateTime),
}

pub(crate) fn parse_until(text: &str) -> Option<Until> {
    let text = text.trim();
    if text.len() == 8 {
        let date = NaiveDate::parse_from_str(text, "%Y%m%d").ok()?;
        return Some(Until::Local(date.and_hms_opt(23, 59, 59)?));
    }
    let utc = text.ends_with('Z');
    let local = NaiveDateTime::parse_from_str(text.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    Some(if utc {
        Until::Utc(local)
    } else {
        Until::Local(local)
    })
}

pub(crate) fn format_local(local: NaiveDateTime, date_only: bool) -> String {
    if date_only {
        local.format("%Y-%m-%d").to_string()
    } else {
        local.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

pub(crate) fn format_utc(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
