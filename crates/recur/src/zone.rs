use std::sync::Arc;

use chrono::{Datelike, Duration, NaiveDateTime, Offset, TimeZone};
use holtmail_pim::{CalendarObject, ZoneDefinition};

use crate::rule::{Rule, Times};

const LAST_ONSET_YEAR: i32 = 2200;
const MAX_ONSETS: usize = 2_000;
const MAX_OBSERVANCES: usize = 64;
const CACHED_ZONES: usize = 16;

thread_local! {
    static CUSTOM: std::cell::RefCell<Vec<(ZoneDefinition, Option<Arc<Custom>>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The zone a local time is read in.
#[derive(Debug, Clone)]
pub(crate) enum Zone {
    /// No zone: the local time is read as if it were UTC.
    Floating,
    /// A fixed offset in seconds east of UTC, `0` for UTC.
    Fixed(i32),
    /// An IANA zone.
    Iana(chrono_tz::Tz),
    /// The rules of a `VTIMEZONE` that names no known zone.
    Custom(Arc<Custom>),
}

#[derive(Debug)]
pub(crate) struct Custom {
    /// `(UTC seconds of the onset, offset from then on)`, by onset.
    onsets: Vec<(i64, i32)>,
    /// Every offset of `onsets`, once each, ascending.
    offsets: Vec<i32>,
}

impl Custom {
    fn from(definition: &ZoneDefinition) -> Option<Self> {
        let mut onsets: Vec<(i64, i32)> = Vec::new();
        for observance in definition.observances.iter().take(MAX_OBSERVANCES) {
            let Some(start) = parse_local(&observance.start) else {
                continue;
            };
            let from = i64::from(observance.offset_from);
            let mut push = |local: NaiveDateTime| {
                onsets.push((local.and_utc().timestamp() - from, observance.offset_to));
            };
            push(start);
            for rdate in &observance.rdates {
                if let Some(local) = parse_local(rdate) {
                    push(local);
                }
            }
            if let Some(rule) = observance
                .rrule
                .as_deref()
                .and_then(|r| Rule::parse(r).ok())
            {
                let until = rule.until.as_deref().and_then(crate::time::parse_until);
                let past = move |local: NaiveDateTime| {
                    local.year() > LAST_ONSET_YEAR
                        || until.is_some_and(|u| match u {
                            crate::time::Until::Utc(at) => {
                                local.and_utc().timestamp() - from > at.and_utc().timestamp()
                            }
                            crate::time::Until::Local(at) => local > at,
                        })
                };
                for local in Times::new(&rule, start, false, &past)
                    .skip(1)
                    .take(MAX_ONSETS)
                {
                    push(local);
                }
            }
        }
        if onsets.is_empty() {
            return None;
        }
        onsets.sort_unstable();
        let mut offsets: Vec<i32> = onsets.iter().map(|(_, o)| *o).collect();
        offsets.sort_unstable();
        offsets.dedup();
        Some(Self { onsets, offsets })
    }

    fn cached(definition: &ZoneDefinition) -> Option<Arc<Self>> {
        if let Some(found) = CUSTOM.with_borrow(|cache| {
            cache
                .iter()
                .find(|(d, _)| d == definition)
                .map(|(_, c)| c.clone())
        }) {
            return found;
        }
        let custom = Self::from(definition).map(Arc::new);
        CUSTOM.with_borrow_mut(|cache| {
            if cache.len() >= CACHED_ZONES {
                cache.remove(0);
            }
            cache.push((definition.clone(), custom.clone()));
        });
        custom
    }

    fn offset_at(&self, utc: i64) -> i32 {
        match self.onsets.partition_point(|(at, _)| *at <= utc) {
            0 => self.onsets[0].1,
            n => self.onsets[n - 1].1,
        }
    }
}

pub(crate) fn parse_local(text: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(text.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()
}

impl Zone {
    /// The zone a `TZID` of `object` names: the object's own `VTIMEZONE` first (its IANA zone,
    /// else its rules), then the name itself as an IANA or Windows zone name; floating when
    /// nothing is known about it.
    pub(crate) fn of(object: &CalendarObject, tzid: &str) -> Zone {
        let tzid = tzid.trim();
        if let Some(definition) = object.zones.iter().find(|z| z.tzid == tzid) {
            if let Some(zone) = definition.iana.as_deref().and_then(|n| n.parse().ok()) {
                return Zone::Iana(zone);
            }
            if let Some(custom) = Custom::cached(definition) {
                return Zone::Custom(custom);
            }
        }
        if tzid.eq_ignore_ascii_case("UTC") || tzid.eq_ignore_ascii_case("Z") {
            return Zone::Fixed(0);
        }
        if let Some(offset) = parse_offset(tzid) {
            return Zone::Fixed(offset);
        }
        match holtmail_pim::iana_zone(tzid).and_then(|n| n.parse().ok()) {
            Some(zone) => Zone::Iana(zone),
            None => Zone::Floating,
        }
    }

    /// The UTC milliseconds of a local time. A time repeated when clocks go back is the earlier
    /// one; a time skipped when they go forward is read with the offset before the change, so
    /// 02:30 on a night that jumps from 02:00 to 03:00 is 03:30 (RFC 5545, section 3.3.5).
    pub(crate) fn to_utc(&self, local: NaiveDateTime) -> i64 {
        let naive = local.and_utc().timestamp_millis();
        match self {
            Zone::Floating => naive,
            Zone::Fixed(offset) => naive - i64::from(*offset) * 1000,
            Zone::Iana(tz) => match tz.from_local_datetime(&local).earliest() {
                Some(t) => t.timestamp_millis(),
                None => {
                    let before = tz
                        .offset_from_utc_datetime(&(local - Duration::days(1)))
                        .fix()
                        .local_minus_utc();
                    naive - i64::from(before) * 1000
                }
            },
            Zone::Custom(custom) => {
                let seconds = local.and_utc().timestamp();
                let valid = custom
                    .offsets
                    .iter()
                    .map(|o| seconds - i64::from(*o))
                    .filter(|utc| i64::from(custom.offset_at(*utc)) == seconds - utc)
                    .min();
                let utc = valid
                    .unwrap_or_else(|| seconds - i64::from(custom.offset_at(seconds - 86_400)));
                utc * 1000
            }
        }
    }

    /// The local time of UTC milliseconds.
    pub(crate) fn to_local(&self, utc: i64) -> Option<NaiveDateTime> {
        let at = chrono::DateTime::from_timestamp_millis(utc)?.naive_utc();
        Some(match self {
            Zone::Floating => at,
            Zone::Fixed(offset) => at + Duration::seconds(i64::from(*offset)),
            Zone::Iana(tz) => tz.from_utc_datetime(&at).naive_local(),
            Zone::Custom(custom) => {
                at + Duration::seconds(i64::from(custom.offset_at(utc.div_euclid(1000))))
            }
        })
    }

    pub(crate) fn is_floating(&self) -> bool {
        matches!(self, Zone::Floating)
    }
}

fn parse_offset(text: &str) -> Option<i32> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_pim::Observance;

    fn local(s: &str) -> NaiveDateTime {
        parse_local(s).unwrap()
    }

    fn utc(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn object_with(zone: ZoneDefinition) -> CalendarObject {
        CalendarObject {
            components: Vec::new(),
            zones: vec![zone],
        }
    }

    fn harbour() -> ZoneDefinition {
        ZoneDefinition {
            tzid: "Harbour Time".into(),
            iana: None,
            observances: vec![
                Observance {
                    daylight: false,
                    start: "1970-10-25T03:00:00".into(),
                    offset_from: 7200,
                    offset_to: 3600,
                    rrule: Some("FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU".into()),
                    rdates: Vec::new(),
                },
                Observance {
                    daylight: true,
                    start: "1970-03-29T02:00:00".into(),
                    offset_from: 3600,
                    offset_to: 7200,
                    rrule: Some("FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU".into()),
                    rdates: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn iana_zones_handle_gaps_and_overlaps() {
        let berlin = Zone::of(&CalendarObject::default(), "Europe/Berlin");
        assert_eq!(
            utc(berlin.to_utc(local("2025-03-30T01:30:00"))),
            "2025-03-30T00:30:00Z"
        );
        assert_eq!(
            utc(berlin.to_utc(local("2025-03-30T02:30:00"))),
            "2025-03-30T01:30:00Z"
        );
        assert_eq!(
            utc(berlin.to_utc(local("2025-03-30T03:30:00"))),
            "2025-03-30T01:30:00Z"
        );
        assert_eq!(
            utc(berlin.to_utc(local("2025-10-26T02:30:00"))),
            "2025-10-26T00:30:00Z"
        );
        let new_york = Zone::of(&CalendarObject::default(), "Eastern Standard Time");
        assert_eq!(
            utc(new_york.to_utc(local("2025-03-09T02:30:00"))),
            "2025-03-09T07:30:00Z"
        );
        assert_eq!(
            utc(new_york.to_utc(local("2025-11-02T01:30:00"))),
            "2025-11-02T05:30:00Z"
        );
        assert_eq!(
            new_york.to_local(new_york.to_utc(local("2025-07-01T09:00:00"))),
            Some(local("2025-07-01T09:00:00"))
        );
    }

    #[test]
    fn custom_definitions_follow_their_rules() {
        let zone = Zone::of(&object_with(harbour()), "Harbour Time");
        assert!(matches!(zone, Zone::Custom(_)));
        assert_eq!(
            utc(zone.to_utc(local("2025-01-15T09:00:00"))),
            "2025-01-15T08:00:00Z"
        );
        assert_eq!(
            utc(zone.to_utc(local("2025-07-15T09:00:00"))),
            "2025-07-15T07:00:00Z"
        );
        assert_eq!(
            utc(zone.to_utc(local("2025-03-30T02:30:00"))),
            "2025-03-30T01:30:00Z"
        );
        assert_eq!(
            utc(zone.to_utc(local("2025-10-26T02:30:00"))),
            "2025-10-26T00:30:00Z"
        );
        assert_eq!(
            zone.to_local(zone.to_utc(local("2025-07-15T09:00:00"))),
            Some(local("2025-07-15T09:00:00"))
        );
    }

    #[test]
    fn known_names_beat_definitions_and_unknown_names_float() {
        let mut definition = harbour();
        definition.iana = Some("America/New_York".into());
        assert!(matches!(
            Zone::of(&object_with(definition), "Harbour Time"),
            Zone::Iana(chrono_tz::Tz::America__New_York)
        ));
        assert!(Zone::of(&CalendarObject::default(), "Nowhere Standard Time").is_floating());
        assert!(matches!(
            Zone::of(&CalendarObject::default(), "+05:30"),
            Zone::Fixed(19_800)
        ));
        assert!(matches!(
            Zone::of(&CalendarObject::default(), "UTC"),
            Zone::Fixed(0)
        ));
    }
}
