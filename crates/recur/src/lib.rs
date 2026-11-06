//! Recurrence expansion for Holtmail's calendar: the instances of a normalized
//! [`CalendarObject`] that fall into a window of time.
//!
//! A library, not a contract: RFC 5545 has one right answer. `RRULE` (every `FREQ`, `INTERVAL`,
//! `COUNT`, `UNTIL`, `BYDAY` with ordinals, `BYMONTHDAY`, `BYYEARDAY`, `BYWEEKNO`, `BYMONTH`,
//! `BYSETPOS`, `WKST`, `BYHOUR`, `BYMINUTE`, `BYSECOND`), `RDATE`, `EXDATE`, changed and
//! cancelled instances by `RECURRENCE-ID` (with `RANGE=THISANDFUTURE`), all-day and floating
//! events, IANA and Windows zone names and the object's own `VTIMEZONE` rules. Nothing here does
//! I/O.

mod rule;
mod time;
mod zone;

use chrono::{Duration, NaiveDateTime};
use holtmail_pim::{CalendarObject, Event};

pub use chrono::Weekday;
pub use rule::{Frequency, Rule, RuleError, WeekdayNum, weekday_code};

use crate::rule::Times;
use crate::time::{Moment, Until, format_local, format_utc, parse_until};

/// At most this many instances come out of one call; the rest is cut off and
/// [`Expansion::truncated`] says so.
pub const MAX_OCCURRENCES: usize = 20_000;

const MAX_STEPS: usize = 2_000_000;

/// A span of time in UTC milliseconds: from `from` (included) to `to` (excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Start, included.
    pub from: i64,
    /// End, excluded.
    pub to: i64,
}

impl Window {
    /// All of time.
    pub const ALL: Window = Window {
        from: i64::MIN / 4,
        to: i64::MAX / 4,
    };

    fn overlaps(&self, start: i64, end: i64) -> bool {
        if end <= start {
            start >= self.from && start < self.to
        } else {
            start < self.to && end > self.from
        }
    }
}

/// One instance of an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The index in [`CalendarObject::components`] of the component whose fields the instance
    /// has: the recurring event, or the changed instance that replaces it.
    pub component: usize,
    /// The original start that names the instance among the copies of the event, as
    /// [`holtmail_pim::EventKey::recurrence_id`] writes it: `2025-03-10` for a date,
    /// `2025-03-10T08:00:00Z` in UTC for a time. `None` for an event that does not repeat.
    pub recurrence_id: Option<String>,
    /// Start in UTC milliseconds. A date counts from midnight UTC and a floating time as if it
    /// were UTC.
    pub start: i64,
    /// End in UTC milliseconds, never before `start`. A date's end is the midnight after its
    /// last day.
    pub end: i64,
    /// The start as a wall-clock time in its own zone: `2025-03-10T09:00:00`, or `2025-03-10`.
    pub start_local: String,
    /// The end like `start_local`; for a date, the day after the last one.
    pub end_local: String,
    /// The `TZID` of the start as written, `UTC`, or `None` for dates and floating times.
    pub time_zone: Option<String>,
    /// Whether it lasts whole days.
    pub all_day: bool,
    /// Whether its times belong to no zone and read the same everywhere.
    pub floating: bool,
    /// Whether it is cancelled: by `STATUS:CANCELLED` on the event or on the instance.
    pub cancelled: bool,
    /// Whether a changed instance (`RECURRENCE-ID`) replaced it.
    pub changed: bool,
}

/// The instances [`expand`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion {
    /// By start, then component.
    pub occurrences: Vec<Occurrence>,
    /// Whether instances were left out: there were more than [`MAX_OCCURRENCES`], or the rule
    /// went past the work budget for one call (two million periods, or a period too large to
    /// list) before the window ended. A rule whose periods stay empty for 400 years, a whole
    /// Gregorian cycle, has ended and is not truncated.
    pub truncated: bool,
}

/// When an event's instances begin and end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The earliest start in UTC milliseconds.
    pub start: i64,
    /// The latest end in UTC milliseconds; `None` when the event repeats without end.
    pub end: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
enum Length {
    Local(Duration),
    Exact(i64),
}

struct Series<'a> {
    object: &'a CalendarObject,
    index: usize,
    event: &'a Event,
    start: Moment,
    length: Length,
    cancelled: bool,
}

fn is_cancelled(event: &Event) -> bool {
    event
        .status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED"))
}

impl<'a> Series<'a> {
    fn new(object: &'a CalendarObject, index: usize, event: &'a Event) -> Option<Self> {
        let start = Moment::read(object, event.start.as_ref()?)?;
        let end = event.end.as_ref().and_then(|e| Moment::read(object, e));
        let length = match &end {
            _ if start.date_only => {
                let days = end
                    .as_ref()
                    .map(|e| (e.local.date() - start.local.date()).num_days())
                    .unwrap_or(1)
                    .max(1);
                Length::Local(Duration::days(days))
            }
            None => Length::Local(Duration::zero()),
            Some(end) if end.date_only => Length::Exact(0),
            Some(end)
                if event.end.as_ref().map(|e| &e.time_zone)
                    == event.start.as_ref().map(|s| &s.time_zone) =>
            {
                Length::Local((end.local - start.local).max(Duration::zero()))
            }
            Some(end) => Length::Exact((end.utc() - start.utc()).max(0)),
        };
        Some(Self {
            object,
            index,
            event,
            start,
            length,
            cancelled: is_cancelled(event),
        })
    }

    fn occurrence(
        &self,
        component: usize,
        local: NaiveDateTime,
        length: Length,
        recurrence_id: Option<String>,
        changed: bool,
        cancelled: bool,
    ) -> Occurrence {
        let zone = &self.start.zone;
        let date_only = self.start.date_only;
        let start = zone.to_utc(local);
        let (end, end_local) = match length {
            Length::Local(d) => {
                let end_local = local.checked_add_signed(d).unwrap_or(local);
                (zone.to_utc(end_local).max(start), end_local)
            }
            Length::Exact(ms) => {
                let end = start.saturating_add(ms);
                (end, zone.to_local(end).unwrap_or(local))
            }
        };
        let event = &self.object.components[component];
        Occurrence {
            component,
            recurrence_id,
            start,
            end,
            start_local: format_local(local, date_only),
            end_local: format_local(end_local, date_only),
            time_zone: if date_only {
                None
            } else {
                event.start.as_ref().and_then(|s| s.time_zone.clone())
            },
            all_day: date_only,
            floating: !date_only && zone.is_floating(),
            cancelled: cancelled || self.cancelled,
            changed,
        }
    }

    fn key(&self, local: NaiveDateTime) -> String {
        if self.start.date_only {
            local.format("%Y-%m-%d").to_string()
        } else {
            format_utc(self.start.zone.to_utc(local))
        }
    }
}

/// The key a changed instance's `RECURRENCE-ID` gives, read in the object's zones.
fn instance_key(object: &CalendarObject, event: &Event) -> Option<String> {
    let rid = Moment::read(object, event.recurrence_id.as_ref()?)?;
    Some(if rid.date_only {
        rid.local.format("%Y-%m-%d").to_string()
    } else {
        format_utc(rid.utc())
    })
}

struct Change<'a> {
    index: usize,
    key: String,
    rid_utc: i64,
    series: Option<Series<'a>>,
    this_and_future: bool,
}

/// The instances of `object` that overlap `window`, by start.
///
/// - An event without `RRULE` or `RDATE` has one instance with no `recurrence_id`.
/// - A recurring event has `DTSTART` as its first instance, then what its `RRULE` produces up to
///   `COUNT` or `UNTIL` (a UTC `UNTIL` compared in UTC, a local one with local times), plus its
///   `RDATE`s, less its `EXDATE`s (matched by time in their own zones, or by day for dates).
/// - A component with a `RECURRENCE-ID` replaces the instance it names, keeping that instance's
///   `recurrence_id`, wherever it moved it; with `RANGE=THISANDFUTURE` it also moves every later
///   instance by the same shift and gives them its fields. A changed instance that names no
///   instance of the rule is still listed.
/// - Times are generated as wall-clock times in the zone of `DTSTART` and keep their length in
///   wall-clock time across daylight saving changes; see [`Occurrence`] for dates and floating
///   times. A rule that cannot be read leaves `DTSTART` and the `RDATE`s.
///
/// The same object and window always give the same instances.
pub fn expand(object: &CalendarObject, window: Window) -> Expansion {
    let mut out = Expansion::default();
    let mut changes: Vec<Change<'_>> = Vec::new();
    for (index, event) in object.components.iter().enumerate() {
        if event.recurrence_id.is_none() {
            continue;
        }
        let Some(key) = instance_key(object, event) else {
            continue;
        };
        let rid_utc = event
            .recurrence_id
            .as_ref()
            .and_then(|r| Moment::read(object, r))
            .map_or(0, |m| m.utc());
        changes.push(Change {
            index,
            key,
            rid_utc,
            series: Series::new(object, index, event),
            this_and_future: event.this_and_future,
        });
    }
    let mut masters = 0;
    for (index, event) in object.components.iter().enumerate() {
        if event.recurrence_id.is_some() {
            continue;
        }
        masters += 1;
        let Some(series) = Series::new(object, index, event) else {
            continue;
        };
        if event.rrule.is_none() && event.rdates.is_empty() {
            let occurrence =
                series.occurrence(index, series.start.local, series.length, None, false, false);
            if window.overlaps(occurrence.start, occurrence.end) {
                out.occurrences.push(occurrence);
            }
            continue;
        }
        recurring(&series, &changes, window, &mut out);
    }
    for change in &changes {
        let Some(series) = &change.series else {
            continue;
        };
        let cancelled = masters > 0
            && object
                .components
                .iter()
                .any(|e| e.recurrence_id.is_none() && is_cancelled(e));
        let occurrence = series.occurrence(
            change.index,
            series.start.local,
            series.length,
            Some(change.key.clone()),
            true,
            cancelled,
        );
        if window.overlaps(occurrence.start, occurrence.end) {
            out.occurrences.push(occurrence);
        }
    }
    out.occurrences.sort_by(|a, b| {
        (a.start, a.component, &a.recurrence_id).cmp(&(b.start, b.component, &b.recurrence_id))
    });
    if out.occurrences.len() > MAX_OCCURRENCES {
        out.occurrences.truncate(MAX_OCCURRENCES);
        out.truncated = true;
    }
    out
}

fn recurring(series: &Series<'_>, changes: &[Change<'_>], window: Window, out: &mut Expansion) {
    let object = series.object;
    let event = series.event;
    let zone = &series.start.zone;
    let date_only = series.start.date_only;
    let rule = event.rrule.as_deref().and_then(|r| Rule::parse(r).ok());
    let until = rule
        .as_ref()
        .and_then(|r| r.until.as_deref())
        .and_then(parse_until);
    let past = |local: NaiveDateTime| match until {
        Some(Until::Utc(at)) if !date_only => zone.to_utc(local) > at.and_utc().timestamp_millis(),
        Some(Until::Utc(at)) => local.date() > at.date(),
        Some(Until::Local(at)) => local > at,
        None => false,
    };
    let excluded: Vec<(bool, String, i64)> = event
        .exdates
        .iter()
        .filter_map(|t| Moment::read(object, t))
        .map(|m| (m.date_only, m.local.format("%Y-%m-%d").to_string(), m.utc()))
        .collect();
    let is_excluded = |local: NaiveDateTime| {
        let day = local.format("%Y-%m-%d").to_string();
        let utc = zone.to_utc(local);
        excluded
            .iter()
            .any(|(date, d, u)| if *date { *d == day } else { *u == utc })
    };
    let reach = match series.length {
        Length::Local(d) => d.num_milliseconds(),
        Length::Exact(ms) => ms,
    };
    let following: Vec<&Change<'_>> = changes.iter().filter(|c| c.this_and_future).collect();
    let shifts: Vec<i64> = following
        .iter()
        .filter_map(|c| c.series.as_ref().map(|s| s.start.utc() - c.rid_utc))
        .collect();
    let early = shifts
        .iter()
        .copied()
        .filter(|s| *s < 0)
        .map(|s| -s)
        .max()
        .unwrap_or(0);
    let late = shifts.iter().copied().filter(|s| *s > 0).max().unwrap_or(0);
    let stop = window.to.saturating_add(early);

    let mut starts: Vec<NaiveDateTime> = Vec::new();
    match &rule {
        Some(rule) => {
            let mut times = Times::new(rule, series.start.local, date_only, &past);
            let from = window
                .from
                .saturating_sub(reach)
                .saturating_sub(late)
                .saturating_sub(86_400_000 * 2);
            if let Some(local) = zone.to_local(from) {
                times.skip_to(local);
            }
            let mut steps = 0;
            for local in times.by_ref() {
                steps += 1;
                if steps > MAX_STEPS {
                    out.truncated = true;
                    break;
                }
                if zone.to_utc(local) >= stop {
                    break;
                }
                starts.push(local);
                if starts.len() > MAX_OCCURRENCES * 4 {
                    out.truncated = true;
                    break;
                }
            }
            if times.cut_short() {
                out.truncated = true;
            }
        }
        None => starts.push(series.start.local),
    }
    for rdate in &event.rdates {
        if let Some(m) = Moment::read(object, rdate) {
            let local = if m.date_only && !date_only {
                m.local.date().and_time(series.start.local.time())
            } else if date_only {
                m.local
            } else {
                zone.to_local(m.utc()).unwrap_or(m.local)
            };
            starts.push(local);
        }
    }
    starts.sort_unstable();
    starts.dedup();
    for local in starts {
        if is_excluded(local) {
            continue;
        }
        let key = series.key(local);
        if changes.iter().any(|c| c.key == key) {
            continue;
        }
        let utc = zone.to_utc(local);
        let from_change = following
            .iter()
            .filter(|c| c.rid_utc <= utc)
            .max_by_key(|c| c.rid_utc)
            .and_then(|c| c.series.as_ref().map(|s| (c, s)));
        let occurrence = match from_change {
            Some((change, moved)) => {
                let shift = moved.start.utc() - change.rid_utc;
                let target = zone.to_local(utc + shift).unwrap_or(local);
                let mut occurrence = series.occurrence(
                    change.index,
                    target,
                    moved.length,
                    Some(key),
                    true,
                    is_cancelled(moved.event),
                );
                occurrence.component = change.index;
                occurrence
            }
            None => series.occurrence(series.index, local, series.length, Some(key), false, false),
        };
        if window.overlaps(occurrence.start, occurrence.end) {
            out.occurrences.push(occurrence);
        }
    }
}

/// When the instances of `object` begin and end; `None` when it has no instance with a start.
pub fn span(object: &CalendarObject) -> Option<Span> {
    let endless = object.components.iter().any(|e| {
        e.recurrence_id.is_none()
            && e.rrule
                .as_deref()
                .and_then(|r| Rule::parse(r).ok())
                .is_some_and(|r| r.count.is_none() && r.until.is_none())
    });
    if endless {
        let start = object
            .components
            .iter()
            .filter_map(|e| e.start.as_ref())
            .filter_map(|t| Moment::read(object, t))
            .map(|m| m.utc())
            .min()?;
        return Some(Span { start, end: None });
    }
    let all = expand(object, Window::ALL);
    let start = all.occurrences.iter().map(|o| o.start).min()?;
    let end = all.occurrences.iter().map(|o| o.end).max();
    Some(Span {
        start,
        end: if all.truncated { None } else { end },
    })
}

/// The wall-clock time, `2025-03-10T09:00:00`, of UTC milliseconds in the zone `zone` names: an
/// IANA or Windows zone name, `UTC`, or an offset such as `+05:30`. `None` for a name that names
/// no zone.
pub fn wall_clock(utc: i64, zone: &str) -> Option<String> {
    let zone = zone::Zone::of(&CalendarObject::default(), zone);
    if zone.is_floating() {
        return None;
    }
    zone.to_local(utc).map(|t| format_local(t, false))
}
