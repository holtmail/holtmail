use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// How often a rule repeats: the `FREQ` of an `RRULE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Frequency {
    /// `YEARLY`.
    Yearly,
    /// `MONTHLY`.
    Monthly,
    /// `WEEKLY`.
    Weekly,
    /// `DAILY`.
    Daily,
    /// `HOURLY`.
    Hourly,
    /// `MINUTELY`.
    Minutely,
    /// `SECONDLY`.
    Secondly,
}

impl Frequency {
    /// The `FREQ` value, uppercase.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Yearly => "YEARLY",
            Self::Monthly => "MONTHLY",
            Self::Weekly => "WEEKLY",
            Self::Daily => "DAILY",
            Self::Hourly => "HOURLY",
            Self::Minutely => "MINUTELY",
            Self::Secondly => "SECONDLY",
        }
    }
}

/// A weekday of `BYDAY`, with its ordinal when it has one: `-1FR` is the last Friday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WeekdayNum {
    /// `1` for the first, `-1` for the last; `None` for every such weekday.
    pub ordinal: Option<i16>,
    /// The weekday.
    pub weekday: Weekday,
}

/// An `RRULE` read into its parts (RFC 5545, section 3.3.10). Parts Holtmail does not use, such
/// as `RSCALE`, are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// `FREQ`.
    pub frequency: Frequency,
    /// `INTERVAL`, 1 when missing.
    pub interval: u32,
    /// `COUNT`.
    pub count: Option<u32>,
    /// `UNTIL` as written: `20250428`, `20250428T090000` or `20250428T070000Z`.
    pub until: Option<String>,
    /// `BYSECOND`.
    pub by_second: Vec<u32>,
    /// `BYMINUTE`.
    pub by_minute: Vec<u32>,
    /// `BYHOUR`.
    pub by_hour: Vec<u32>,
    /// `BYDAY`.
    pub by_day: Vec<WeekdayNum>,
    /// `BYMONTHDAY`, negative from the end of the month.
    pub by_month_day: Vec<i32>,
    /// `BYYEARDAY`, negative from the end of the year.
    pub by_year_day: Vec<i32>,
    /// `BYWEEKNO`, negative from the end of the year.
    pub by_week_no: Vec<i32>,
    /// `BYMONTH`, 1 to 12.
    pub by_month: Vec<u32>,
    /// `BYSETPOS`, negative from the end of each period.
    pub by_set_pos: Vec<i32>,
    /// `WKST`, Monday when missing.
    pub week_start: Weekday,
}

/// Why an `RRULE` could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// It has no `FREQ`, or one this crate does not know.
    #[error("the rule has no known FREQ")]
    Frequency,
    /// A part has a value out of its range, such as `BYMONTH=13`.
    #[error("the rule part {0} has an invalid value")]
    Value(String),
}

/// The two letters `BYDAY` and `WKST` write a weekday with: `MO` to `SU`.
pub fn weekday_code(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

pub(crate) fn weekday(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn list<T: std::str::FromStr>(
    name: &str,
    value: &str,
    ok: impl Fn(&T) -> bool,
) -> Result<Vec<T>, RuleError> {
    value
        .split(',')
        .map(|v| {
            v.trim()
                .trim_start_matches('+')
                .parse::<T>()
                .ok()
                .filter(&ok)
                .ok_or_else(|| RuleError::Value(name.to_owned()))
        })
        .collect()
}

impl Rule {
    /// Reads an `RRULE` value such as `FREQ=MONTHLY;BYDAY=-1FR;COUNT=6`, in any letter case.
    pub fn parse(text: &str) -> Result<Rule, RuleError> {
        let mut rule = Rule {
            frequency: Frequency::Daily,
            interval: 1,
            count: None,
            until: None,
            by_second: Vec::new(),
            by_minute: Vec::new(),
            by_hour: Vec::new(),
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_year_day: Vec::new(),
            by_week_no: Vec::new(),
            by_month: Vec::new(),
            by_set_pos: Vec::new(),
            week_start: Weekday::Mon,
        };
        let mut frequency = None;
        for part in text.trim().trim_start_matches("RRULE:").split(';') {
            let Some((name, value)) = part.split_once('=') else {
                continue;
            };
            let name = name.trim().to_ascii_uppercase();
            let value = value.trim().to_ascii_uppercase();
            match name.as_str() {
                "FREQ" => {
                    frequency = Some(match value.as_str() {
                        "YEARLY" => Frequency::Yearly,
                        "MONTHLY" => Frequency::Monthly,
                        "WEEKLY" => Frequency::Weekly,
                        "DAILY" => Frequency::Daily,
                        "HOURLY" => Frequency::Hourly,
                        "MINUTELY" => Frequency::Minutely,
                        "SECONDLY" => Frequency::Secondly,
                        _ => return Err(RuleError::Frequency),
                    })
                }
                "INTERVAL" => {
                    rule.interval = value
                        .parse::<u32>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| RuleError::Value(name.clone()))?
                }
                "COUNT" => {
                    rule.count = Some(
                        value
                            .parse::<u32>()
                            .map_err(|_| RuleError::Value(name.clone()))?,
                    )
                }
                "UNTIL" => rule.until = Some(value),
                "BYSECOND" => rule.by_second = list(&name, &value, |n: &u32| *n <= 60)?,
                "BYMINUTE" => rule.by_minute = list(&name, &value, |n: &u32| *n <= 59)?,
                "BYHOUR" => rule.by_hour = list(&name, &value, |n: &u32| *n <= 23)?,
                "BYMONTHDAY" => {
                    rule.by_month_day =
                        list(&name, &value, |n: &i32| *n != 0 && (-31..=31).contains(n))?
                }
                "BYYEARDAY" => {
                    rule.by_year_day =
                        list(&name, &value, |n: &i32| *n != 0 && (-366..=366).contains(n))?
                }
                "BYWEEKNO" => {
                    rule.by_week_no =
                        list(&name, &value, |n: &i32| *n != 0 && (-53..=53).contains(n))?
                }
                "BYMONTH" => rule.by_month = list(&name, &value, |n: &u32| (1..=12).contains(n))?,
                "BYSETPOS" => {
                    rule.by_set_pos =
                        list(&name, &value, |n: &i32| *n != 0 && (-366..=366).contains(n))?
                }
                "WKST" => {
                    rule.week_start =
                        weekday(&value).ok_or_else(|| RuleError::Value(name.clone()))?
                }
                "BYDAY" => {
                    rule.by_day = value
                        .split(',')
                        .map(|day| {
                            let day = day.trim();
                            let split = day.len().saturating_sub(2);
                            let (Some(ordinal), Some(code)) = (day.get(..split), day.get(split..))
                            else {
                                return Err(RuleError::Value(name.clone()));
                            };
                            let weekday = weekday(code);
                            let ordinal = if ordinal.is_empty() {
                                Some(None)
                            } else {
                                ordinal
                                    .trim_start_matches('+')
                                    .parse::<i16>()
                                    .ok()
                                    .filter(|n| *n != 0 && (-53..=53).contains(n))
                                    .map(Some)
                            };
                            match (weekday, ordinal) {
                                (Some(weekday), Some(ordinal)) => {
                                    Ok(WeekdayNum { ordinal, weekday })
                                }
                                _ => Err(RuleError::Value(name.clone())),
                            }
                        })
                        .collect::<Result<_, _>>()?
                }
                _ => {}
            }
        }
        rule.frequency = frequency.ok_or(RuleError::Frequency)?;
        Ok(rule)
    }
}

/// When a rule's `UNTIL` stops it, judged on the local times it produces.
pub(crate) type Past<'a> = &'a dyn Fn(NaiveDateTime) -> bool;

/// The Gregorian calendar repeats every 400 years, weekdays included, so a rule whose periods
/// stay empty that long never produces anything again.
const MAX_EMPTY_DAYS: i64 = 146_097;

const MAX_PERIODS: u32 = 2_000_000;

const MAX_PERIOD_TIMES: usize = 100_000;

fn in_range(day: NaiveDate) -> bool {
    (1..=9999).contains(&day.year())
}

/// The local start times a rule produces from `start`, in order: `start` itself first (RFC 5545
/// counts `DTSTART` as the first instance even when the rule would not produce it), then every
/// later time the rule produces, until `COUNT` is reached or `past` says a time is after `UNTIL`.
pub(crate) struct Times<'a> {
    rule: &'a Rule,
    start: NaiveDateTime,
    date_only: bool,
    past: Past<'a>,
    period: i64,
    buffer: std::collections::VecDeque<NaiveDateTime>,
    emitted: u32,
    periods: u32,
    started: bool,
    done: bool,
    cut: bool,
    by_day: Vec<WeekdayNum>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
}

impl<'a> Times<'a> {
    pub(crate) fn new(
        rule: &'a Rule,
        start: NaiveDateTime,
        date_only: bool,
        past: Past<'a>,
    ) -> Self {
        let mut by_day = rule.by_day.clone();
        let mut by_month_day = rule.by_month_day.clone();
        let mut by_month = rule.by_month.clone();
        let unspecified = rule.by_week_no.is_empty()
            && rule.by_year_day.is_empty()
            && rule.by_month_day.is_empty()
            && rule.by_day.is_empty();
        match rule.frequency {
            Frequency::Yearly if unspecified => {
                if by_month.is_empty() {
                    by_month.push(start.month());
                }
                by_month_day.push(start.day() as i32);
            }
            Frequency::Monthly if unspecified => by_month_day.push(start.day() as i32),
            Frequency::Weekly if unspecified => by_day.push(WeekdayNum {
                ordinal: None,
                weekday: start.weekday(),
            }),
            _ => {}
        }
        Self {
            rule,
            start,
            date_only,
            past,
            period: 0,
            buffer: Default::default(),
            emitted: 0,
            periods: 0,
            started: false,
            done: false,
            cut: false,
            by_day,
            by_month_day,
            by_month,
        }
    }

    /// Skips whole periods that end before `at`, so a far window does not walk every period
    /// from the start. Only without `COUNT`, which needs every earlier instance.
    pub(crate) fn skip_to(&mut self, at: NaiveDateTime) {
        if self.rule.count.is_some() || at <= self.start {
            return;
        }
        let interval = i64::from(self.rule.interval);
        let start = self.start;
        let periods = match self.rule.frequency {
            Frequency::Yearly => i64::from(at.year() - start.year()),
            Frequency::Monthly => {
                i64::from(at.year() - start.year()) * 12 + i64::from(at.month())
                    - i64::from(start.month())
            }
            Frequency::Weekly => (at.date() - start.date()).num_days() / 7,
            Frequency::Daily => (at.date() - start.date()).num_days(),
            Frequency::Hourly => (at - start).num_hours(),
            Frequency::Minutely => (at - start).num_minutes(),
            Frequency::Secondly => (at - start).num_seconds(),
        };
        let skip = (periods / interval - 1).max(0);
        if skip > self.period {
            self.period = skip;
            self.started = true;
            self.buffer.clear();
        }
    }

    /// Whether the rule stopped because it went past the work budget, with instances possibly
    /// left after the last one given.
    pub(crate) fn cut_short(&self) -> bool {
        self.cut
    }

    fn unit(&self) -> Option<Duration> {
        match self.rule.frequency {
            Frequency::Hourly => Some(Duration::hours(1)),
            Frequency::Minutely => Some(Duration::minutes(1)),
            Frequency::Secondly => Some(Duration::seconds(1)),
            _ => None,
        }
    }

    /// The first day period `k` covers.
    fn period_day(&self, k: i64) -> Option<NaiveDate> {
        let step = k.checked_mul(i64::from(self.rule.interval))?;
        let start = self.start.date();
        match self.rule.frequency {
            Frequency::Yearly => {
                NaiveDate::from_ymd_opt(i32::try_from(i64::from(start.year()) + step).ok()?, 1, 1)
            }
            Frequency::Monthly => {
                let index = i64::from(start.year()) * 12 + i64::from(start.month0()) + step;
                NaiveDate::from_ymd_opt(
                    i32::try_from(index.div_euclid(12)).ok()?,
                    index.rem_euclid(12) as u32 + 1,
                    1,
                )
            }
            Frequency::Weekly => {
                start.checked_add_signed(Duration::try_days(step.checked_mul(7)?)?)
            }
            Frequency::Daily => start.checked_add_signed(Duration::try_days(step)?),
            _ => Some(
                self.start
                    .checked_add_signed(self.unit()? * i32::try_from(step).ok()?)?
                    .date(),
            ),
        }
    }

    /// For a sub-daily rule at period `k` on a day the day-level parts rule out, the first period
    /// on a later day; `None` otherwise.
    fn next_day_period(&self, k: i64) -> Option<i64> {
        let unit = self.unit()?;
        let day = self.period_day(k)?;
        if self.matches(day, Scope::Month) {
            return None;
        }
        let midnight = day.succ_opt()?.and_time(NaiveTime::MIN);
        let length = (unit * i32::try_from(self.rule.interval).ok()?).num_seconds();
        let ahead = (midnight - self.start).num_seconds();
        Some((ahead + length - 1).div_euclid(length).max(k + 1))
    }

    fn times_of_day(&self) -> Vec<NaiveTime> {
        if self.date_only {
            return vec![NaiveTime::MIN];
        }
        let or = |list: &[u32], default: u32| {
            if list.is_empty() {
                vec![default]
            } else {
                let mut list = list.to_vec();
                list.sort_unstable();
                list.dedup();
                list
            }
        };
        let hours = or(&self.rule.by_hour, self.start.hour());
        let minutes = or(&self.rule.by_minute, self.start.minute());
        let seconds = or(&self.rule.by_second, self.start.second());
        let mut out = Vec::new();
        for h in &hours {
            for m in &minutes {
                for s in &seconds {
                    if let Some(t) = NaiveTime::from_hms_opt(*h, *m, (*s).min(59)) {
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    fn period(&self, k: i64) -> Option<Vec<NaiveDateTime>> {
        let step = k.checked_mul(i64::from(self.rule.interval))?;
        let start = self.start;
        let days: Vec<NaiveDate> = match self.rule.frequency {
            Frequency::Yearly => {
                let year = i32::try_from(i64::from(start.year()) + step).ok()?;
                if !(1..=9999).contains(&year) {
                    return None;
                }
                self.year_days(year)
            }
            Frequency::Monthly => {
                let index = i64::from(start.year()) * 12 + i64::from(start.month0()) + step;
                let year = i32::try_from(index.div_euclid(12)).ok()?;
                if !(1..=9999).contains(&year) {
                    return None;
                }
                let month = index.rem_euclid(12) as u32 + 1;
                let first = NaiveDate::from_ymd_opt(year, month, 1)?;
                let last = last_of_month(year, month)?;
                let mut out = Vec::new();
                let mut day = first;
                while day <= last {
                    if self.matches(day, Scope::Month) {
                        out.push(day);
                    }
                    day = day.succ_opt()?;
                }
                out
            }
            Frequency::Weekly => {
                let offset = days_from(self.rule.week_start, start.weekday());
                let first = start
                    .date()
                    .checked_sub_signed(Duration::days(offset))?
                    .checked_add_signed(Duration::try_days(step.checked_mul(7)?)?)?;
                if !in_range(first) {
                    return None;
                }
                (0..7)
                    .map(|i| first + Duration::days(i))
                    .filter(|d| self.matches(*d, Scope::Week))
                    .collect()
            }
            Frequency::Daily => {
                let day = start.date().checked_add_signed(Duration::try_days(step)?)?;
                if !in_range(day) {
                    return None;
                }
                if self.matches(day, Scope::Month) {
                    vec![day]
                } else {
                    Vec::new()
                }
            }
            Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
                let unit = match self.rule.frequency {
                    Frequency::Hourly => Duration::hours(1),
                    Frequency::Minutely => Duration::minutes(1),
                    _ => Duration::seconds(1),
                };
                let at = start.checked_add_signed(unit * i32::try_from(step).ok()?)?;
                if !in_range(at.date()) {
                    return None;
                }
                let ok = self.matches(at.date(), Scope::Month)
                    && (self.rule.by_hour.is_empty() || self.rule.by_hour.contains(&at.hour()))
                    && (self.rule.by_minute.is_empty()
                        || self.rule.by_minute.contains(&at.minute()))
                    && (self.rule.by_second.is_empty()
                        || self.rule.by_second.contains(&at.second()));
                return Some(if ok { vec![at] } else { Vec::new() });
            }
        };
        let times = self.times_of_day();
        if days.len().saturating_mul(times.len()) > MAX_PERIOD_TIMES {
            return None;
        }
        let mut out: Vec<NaiveDateTime> = days
            .iter()
            .flat_map(|d| times.iter().map(move |t| d.and_time(*t)))
            .collect();
        if !self.rule.by_set_pos.is_empty() {
            let len = out.len() as i32;
            let mut picked: Vec<NaiveDateTime> = self
                .rule
                .by_set_pos
                .iter()
                .filter_map(|p| {
                    let index = if *p > 0 { p - 1 } else { len + p };
                    (0..len).contains(&index).then(|| out[index as usize])
                })
                .collect();
            picked.sort_unstable();
            picked.dedup();
            out = picked;
        }
        Some(out)
    }

    fn year_days(&self, year: i32) -> Vec<NaiveDate> {
        let (first, last) = if self.rule.by_week_no.is_empty() {
            (
                NaiveDate::from_ymd_opt(year, 1, 1),
                NaiveDate::from_ymd_opt(year, 12, 31),
            )
        } else {
            (
                week_one(year, self.rule.week_start),
                week_one(year + 1, self.rule.week_start).and_then(|d| d.pred_opt()),
            )
        };
        let (Some(first), Some(last)) = (first, last) else {
            return Vec::new();
        };
        let scope = if !self.rule.by_week_no.is_empty() {
            Scope::Week
        } else if !self.by_month.is_empty() {
            Scope::Month
        } else {
            Scope::Year
        };
        let mut out = Vec::new();
        let mut day = first;
        while day <= last {
            if self.matches(day, scope) && self.week_no_matches(day, year) {
                out.push(day);
            }
            match day.succ_opt() {
                Some(next) => day = next,
                None => break,
            }
        }
        out
    }

    fn week_no_matches(&self, day: NaiveDate, year: i32) -> bool {
        if self.rule.by_week_no.is_empty() {
            return true;
        }
        let (Some(first), Some(next)) = (
            week_one(year, self.rule.week_start),
            week_one(year + 1, self.rule.week_start),
        ) else {
            return false;
        };
        let week = (day - first).num_days() / 7 + 1;
        let weeks = (next - first).num_days() / 7;
        self.rule
            .by_week_no
            .iter()
            .any(|n| i64::from(*n) == week || i64::from(*n) == week - weeks - 1)
    }

    fn matches(&self, day: NaiveDate, scope: Scope) -> bool {
        if !self.by_month.is_empty() && !self.by_month.contains(&day.month()) {
            return false;
        }
        if !self.by_month_day.is_empty() {
            let last = last_of_month(day.year(), day.month()).map_or(31, |d| d.day() as i32);
            let d = day.day() as i32;
            if !self
                .by_month_day
                .iter()
                .any(|n| *n == d || *n == d - last - 1)
            {
                return false;
            }
        }
        if !self.rule.by_year_day.is_empty() {
            let length = if is_leap(day.year()) { 366 } else { 365 };
            let d = day.ordinal() as i32;
            if !self
                .rule
                .by_year_day
                .iter()
                .any(|n| *n == d || *n == d - length - 1)
            {
                return false;
            }
        }
        if !self.by_day.is_empty() {
            let ordinals = matches!(self.rule.frequency, Frequency::Monthly | Frequency::Yearly)
                && scope != Scope::Week;
            let (first, last) = match scope {
                Scope::Year => (
                    NaiveDate::from_ymd_opt(day.year(), 1, 1),
                    NaiveDate::from_ymd_opt(day.year(), 12, 31),
                ),
                _ => (
                    NaiveDate::from_ymd_opt(day.year(), day.month(), 1),
                    last_of_month(day.year(), day.month()),
                ),
            };
            let ok = self.by_day.iter().any(|w| {
                if w.weekday != day.weekday() {
                    return false;
                }
                match (w.ordinal, ordinals, first, last) {
                    (Some(n), true, Some(first), Some(last)) => {
                        let from_start = (day - first).num_days() / 7 + 1;
                        let from_end = -((last - day).num_days() / 7 + 1);
                        i64::from(n) == from_start || i64::from(n) == from_end
                    }
                    _ => true,
                }
            });
            if !ok {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Year,
    Month,
    Week,
}

fn days_from(week_start: Weekday, day: Weekday) -> i64 {
    i64::from((7 + day.num_days_from_monday() - week_start.num_days_from_monday()) % 7)
}

fn week_one(year: i32, week_start: Weekday) -> Option<NaiveDate> {
    let fourth = NaiveDate::from_ymd_opt(year, 1, 4)?;
    Some(fourth - Duration::days(days_from(week_start, fourth.weekday())))
}

fn is_leap(year: i32) -> bool {
    NaiveDate::from_ymd_opt(year, 2, 29).is_some()
}

fn last_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1)?.pred_opt()
}

impl Iterator for Times<'_> {
    type Item = NaiveDateTime;

    fn next(&mut self) -> Option<NaiveDateTime> {
        if self.done {
            return None;
        }
        if !self.started {
            self.started = true;
            self.emitted = 1;
            return Some(self.start);
        }
        let mut empty_since: Option<NaiveDate> = None;
        loop {
            if let Some(next) = self.buffer.pop_front() {
                if next <= self.start {
                    continue;
                }
                if (self.past)(next) || self.rule.count.is_some_and(|c| self.emitted >= c) {
                    self.done = true;
                    return None;
                }
                self.emitted += 1;
                return Some(next);
            }
            self.periods += 1;
            if self.periods > MAX_PERIODS {
                self.cut = true;
                self.done = true;
                return None;
            }
            let skip = self.next_day_period(self.period);
            let times = match skip {
                Some(_) => Some(Vec::new()),
                None => self.period(self.period),
            };
            let Some(times) = times else {
                self.cut = self.period_day(self.period).is_some_and(in_range);
                self.done = true;
                return None;
            };
            if times.is_empty() {
                let day = self.period_day(self.period);
                let since = *empty_since.get_or_insert(day.unwrap_or(NaiveDate::MIN));
                if day.is_none_or(|d| (d - since).num_days() > MAX_EMPTY_DAYS) {
                    self.done = true;
                    return None;
                }
            } else {
                empty_since = None;
            }
            self.period = skip.unwrap_or(self.period + 1);
            self.buffer.extend(times);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap()
    }

    fn run(rule: &str, start: &str, n: usize) -> Vec<String> {
        let rule = Rule::parse(rule).unwrap();
        let never = |_: NaiveDateTime| false;
        Times::new(&rule, at(start), false, &never)
            .take(n)
            .map(|t| t.format("%Y-%m-%d %a %H:%M").to_string())
            .collect()
    }

    #[test]
    fn sparse_rules_keep_going_and_empty_ones_stop() {
        assert_eq!(
            run("FREQ=HOURLY;BYMONTH=1", "2025-01-31T22:00", 4),
            [
                "2025-01-31 Fri 22:00",
                "2025-01-31 Fri 23:00",
                "2026-01-01 Thu 00:00",
                "2026-01-01 Thu 01:00"
            ]
        );
        assert_eq!(
            run("FREQ=DAILY;BYMONTH=2;BYMONTHDAY=29", "2096-02-29T09:00", 2),
            ["2096-02-29 Wed 09:00", "2104-02-29 Fri 09:00"]
        );
        let started = std::time::Instant::now();
        let rule = Rule::parse("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30").unwrap();
        let never = |_: NaiveDateTime| false;
        let mut times = Times::new(&rule, at("2025-03-01T09:00"), false, &never);
        assert_eq!(times.by_ref().count(), 1, "only DTSTART");
        assert!(!times.cut_short(), "the rule ended, nothing was cut");
        let rule = Rule::parse("FREQ=HOURLY;BYMONTH=2;BYMONTHDAY=30").unwrap();
        let mut times = Times::new(&rule, at("2025-03-01T09:00"), false, &never);
        assert_eq!(times.by_ref().count(), 1);
        assert!(!times.cut_short());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn the_period_budget_marks_the_rule_cut_short() {
        let rule = Rule::parse("FREQ=MINUTELY;INTERVAL=2;BYMINUTE=1").unwrap();
        let never = |_: NaiveDateTime| false;
        let mut times = Times::new(&rule, at("2025-03-01T09:00"), false, &never);
        assert_eq!(times.by_ref().count(), 1);
        assert!(times.cut_short());
    }

    #[test]
    fn parses_every_part() {
        let rule = Rule::parse(
            "freq=monthly;interval=2;count=10;byday=-1fr,+2MO,TU;bymonthday=1,-1;bymonth=3;bysetpos=-1;wkst=SU;byhour=9;byminute=30;bysecond=0;byyearday=-1;byweekno=20;until=20251231T000000Z;x-name=y",
        )
        .unwrap();
        assert_eq!(rule.frequency, Frequency::Monthly);
        assert_eq!(rule.interval, 2);
        assert_eq!(rule.count, Some(10));
        assert_eq!(
            rule.by_day,
            [
                WeekdayNum {
                    ordinal: Some(-1),
                    weekday: Weekday::Fri
                },
                WeekdayNum {
                    ordinal: Some(2),
                    weekday: Weekday::Mon
                },
                WeekdayNum {
                    ordinal: None,
                    weekday: Weekday::Tue
                },
            ]
        );
        assert_eq!(rule.by_month_day, [1, -1]);
        assert_eq!(rule.week_start, Weekday::Sun);
        assert_eq!(rule.until.as_deref(), Some("20251231T000000Z"));
        assert_eq!(Rule::parse("COUNT=2"), Err(RuleError::Frequency));
        assert_eq!(
            Rule::parse("FREQ=DAILY;BYMONTH=13"),
            Err(RuleError::Value("BYMONTH".into()))
        );
        assert_eq!(
            Rule::parse("FREQ=DAILY;BYDAY=XX"),
            Err(RuleError::Value("BYDAY".into()))
        );
    }

    #[test]
    fn last_friday_of_the_month() {
        assert_eq!(
            run("FREQ=MONTHLY;BYDAY=-1FR", "2025-01-31T16:00", 4),
            [
                "2025-01-31 Fri 16:00",
                "2025-02-28 Fri 16:00",
                "2025-03-28 Fri 16:00",
                "2025-04-25 Fri 16:00",
            ]
        );
    }

    #[test]
    fn monthly_on_the_31st_skips_short_months() {
        assert_eq!(
            run("FREQ=MONTHLY;COUNT=4", "2025-01-31T09:00", 10),
            [
                "2025-01-31 Fri 09:00",
                "2025-03-31 Mon 09:00",
                "2025-05-31 Sat 09:00",
                "2025-07-31 Thu 09:00",
            ]
        );
    }

    #[test]
    fn last_weekday_of_the_month_with_setpos() {
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
                "2025-05-30T12:00",
                3
            ),
            [
                "2025-05-30 Fri 12:00",
                "2025-06-30 Mon 12:00",
                "2025-07-31 Thu 12:00",
            ]
        );
    }

    #[test]
    fn weekly_on_several_days_with_an_interval_and_week_start() {
        assert_eq!(
            run(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;COUNT=5",
                "2025-09-02T10:00",
                10
            ),
            [
                "2025-09-02 Tue 10:00",
                "2025-09-04 Thu 10:00",
                "2025-09-16 Tue 10:00",
                "2025-09-18 Thu 10:00",
                "2025-09-30 Tue 10:00",
            ]
        );
        // RFC 5545's WKST example: the week start changes which Sundays pair with Tuesdays.
        assert_eq!(
            run(
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
                "1997-08-05T09:00",
                9
            ),
            [
                "1997-08-05 Tue 09:00",
                "1997-08-10 Sun 09:00",
                "1997-08-19 Tue 09:00",
                "1997-08-24 Sun 09:00",
            ]
        );
        assert_eq!(
            run(
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
                "1997-08-05T09:00",
                9
            ),
            [
                "1997-08-05 Tue 09:00",
                "1997-08-17 Sun 09:00",
                "1997-08-19 Tue 09:00",
                "1997-08-31 Sun 09:00",
            ]
        );
    }

    #[test]
    fn yearly_forms() {
        assert_eq!(
            run("FREQ=YEARLY", "2024-02-29T08:00", 3),
            [
                "2024-02-29 Thu 08:00",
                "2028-02-29 Tue 08:00",
                "2032-02-29 Sun 08:00",
            ]
        );
        assert_eq!(
            run("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH", "2025-11-27T12:00", 2),
            ["2025-11-27 Thu 12:00", "2026-11-26 Thu 12:00"]
        );
        assert_eq!(
            run("FREQ=YEARLY;BYDAY=20MO", "1997-05-19T09:00", 2),
            ["1997-05-19 Mon 09:00", "1998-05-18 Mon 09:00"]
        );
        assert_eq!(
            run("FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO", "1997-05-12T09:00", 3),
            [
                "1997-05-12 Mon 09:00",
                "1998-05-11 Mon 09:00",
                "1999-05-17 Mon 09:00",
            ]
        );
        assert_eq!(
            run("FREQ=YEARLY;BYYEARDAY=1,-1", "2025-01-01T00:00", 3),
            [
                "2025-01-01 Wed 00:00",
                "2025-12-31 Wed 00:00",
                "2026-01-01 Thu 00:00",
            ]
        );
    }

    #[test]
    fn dtstart_counts_even_when_the_rule_skips_it() {
        assert_eq!(
            run("FREQ=WEEKLY;BYDAY=MO;COUNT=3", "2025-03-05T09:00", 5),
            [
                "2025-03-05 Wed 09:00",
                "2025-03-10 Mon 09:00",
                "2025-03-17 Mon 09:00",
            ]
        );
    }

    #[test]
    fn daily_with_hours_and_filters() {
        assert_eq!(
            run(
                "FREQ=DAILY;BYHOUR=9,17;BYMINUTE=0;COUNT=4",
                "2025-06-02T09:00",
                9
            ),
            [
                "2025-06-02 Mon 09:00",
                "2025-06-02 Mon 17:00",
                "2025-06-03 Tue 09:00",
                "2025-06-03 Tue 17:00",
            ]
        );
        assert_eq!(
            run("FREQ=DAILY;BYDAY=SA,SU;BYMONTH=6", "2025-06-01T10:00", 3),
            [
                "2025-06-01 Sun 10:00",
                "2025-06-07 Sat 10:00",
                "2025-06-08 Sun 10:00",
            ]
        );
        assert_eq!(
            run("FREQ=HOURLY;INTERVAL=6;COUNT=3", "2025-06-01T22:00", 9),
            [
                "2025-06-01 Sun 22:00",
                "2025-06-02 Mon 04:00",
                "2025-06-02 Mon 10:00",
            ]
        );
    }

    #[test]
    fn impossible_rules_end_instead_of_spinning() {
        assert_eq!(
            run("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30", "2025-01-01T09:00", 5),
            ["2025-01-01 Wed 09:00"]
        );
    }

    #[test]
    fn skipping_ahead_keeps_the_same_instances() {
        let rule = Rule::parse("FREQ=WEEKLY;INTERVAL=3;BYDAY=MO,FR").unwrap();
        let never = |_: NaiveDateTime| false;
        let from = at("2031-07-01T00:00");
        let walked: Vec<_> = Times::new(&rule, at("2020-01-06T08:00"), false, &never)
            .skip_while(|t| *t < from)
            .take(6)
            .collect();
        let mut skipping = Times::new(&rule, at("2020-01-06T08:00"), false, &never);
        skipping.skip_to(from);
        let jumped: Vec<_> = skipping.skip_while(|t| *t < from).take(6).collect();
        assert_eq!(walked, jumped);
    }
}
