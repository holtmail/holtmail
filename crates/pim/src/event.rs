use calcard::common::IanaString;
use calcard::common::PartialDateTime;
use calcard::common::timezone::Tz;
use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarEntry, ICalendarParameterName,
    ICalendarPeriod, ICalendarProperty, ICalendarValue, Uri,
};
use chrono::{NaiveDate, TimeDelta, TimeZone};
use serde::{Deserialize, Serialize};

use crate::ItemKind;
use crate::contact::format_partial;
use crate::keys::{EventKey, normalize_email};

/// The events, tasks or journal entries of one calendar item: a single object, or a recurring
/// one with its changed instances.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarObject {
    /// The object without `RECURRENCE-ID` first, then the changed instances by `RECURRENCE-ID`.
    pub components: Vec<Event>,
    /// The `VTIMEZONE`s the object carries, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<ZoneDefinition>,
}

/// A `VTIMEZONE` as the object defines it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneDefinition {
    /// `TZID` as written.
    pub tzid: String,
    /// The IANA zone it stands for, when its `TZID`, `X-LIC-LOCATION` or Microsoft zone number
    /// names one, such as `Europe/Berlin` for `W. Europe Standard Time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iana: Option<String>,
    /// Its `STANDARD` and `DAYLIGHT` rules, kept only when no IANA zone is known, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observances: Vec<Observance>,
}

/// One `STANDARD` or `DAYLIGHT` rule of a [`ZoneDefinition`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observance {
    /// `DAYLIGHT` rather than `STANDARD`.
    pub daylight: bool,
    /// `DTSTART`: the first onset as a local time in the offset before it, `2001-03-25T02:00:00`.
    pub start: String,
    /// `TZOFFSETFROM` in seconds east of UTC.
    pub offset_from: i32,
    /// `TZOFFSETTO` in seconds east of UTC.
    pub offset_to: i32,
    /// `RRULE` as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    /// `RDATE`s as local times like `start`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rdates: Vec<String>,
}

/// One `VEVENT`, `VTODO` or `VJOURNAL`. Tasks and journal entries fill the same fields as far as
/// they have them (`DUE` is the end of a task); nothing else of them is modeled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Event, task or other.
    pub kind: ItemKind,
    /// `UID`.
    pub uid: Option<String>,
    /// `RECURRENCE-ID` of a changed instance.
    pub recurrence_id: Option<EventTime>,
    /// `SUMMARY`.
    pub summary: Option<String>,
    /// `DESCRIPTION`.
    pub description: Option<String>,
    /// `LOCATION`.
    pub location: Option<String>,
    /// `DTSTART`.
    pub start: Option<EventTime>,
    /// `DTEND`, `DUE`, or the start plus `DURATION`.
    pub end: Option<EventTime>,
    /// Whether the start is a date without a time.
    pub all_day: bool,
    /// Whether it repeats: it has an `RRULE` or `RDATE`.
    pub recurring: bool,
    /// `ORGANIZER`.
    pub organizer: Option<Participant>,
    /// `ATTENDEE`s in file order.
    pub attendees: Vec<Participant>,
    /// `SEQUENCE`.
    pub sequence: Option<i64>,
    /// `DTSTAMP` in milliseconds since the epoch.
    pub dtstamp: Option<i64>,
    /// `STATUS`, uppercase, such as `CONFIRMED` or `CANCELLED`.
    pub status: Option<String>,
    /// `RRULE` as written, such as `FREQ=WEEKLY;BYDAY=MO,WE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    /// `RDATE`s; of a period only its start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rdates: Vec<EventTime>,
    /// `EXDATE`s.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exdates: Vec<EventTime>,
    /// Whether the `RECURRENCE-ID` has `RANGE=THISANDFUTURE`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub this_and_future: bool,
    /// `LAST-MODIFIED` in milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
}

/// A time of an event: as written, in its zone, and in UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTime {
    /// The local value: `2025-03-10` for a date, `2025-03-10T09:00:00` for a time, with `Z` or an
    /// offset when the value carries one.
    pub value: String,
    /// The `TZID` as written, `UTC` for a value ending in `Z`, or `None` for floating times and
    /// dates.
    pub time_zone: Option<String>,
    /// Milliseconds since the epoch in UTC. Dates count from midnight UTC, floating times as if
    /// they were UTC, and a `TZID` that names no known zone as UTC too. A local time skipped by a
    /// daylight saving change counts as one hour later; one repeated counts as the earlier.
    pub utc: Option<i64>,
}

/// A person taking part in an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// The address, normalized like [`crate::normalize_email`], or the calendar address as
    /// written when it is not an email address.
    pub email: Option<String>,
    /// `CN`.
    pub name: Option<String>,
    /// `PARTSTAT`, uppercase, such as `ACCEPTED`.
    pub partstat: Option<String>,
    /// `ROLE`, uppercase, such as `REQ-PARTICIPANT`.
    pub role: Option<String>,
}

pub(crate) fn parse(raw: &[u8]) -> Option<CalendarObject> {
    let text = String::from_utf8_lossy(raw);
    let calendar = ICalendar::parse(text.as_ref()).ok()?;
    from_calendar(&calendar)
}

pub(crate) fn from_calendar(calendar: &ICalendar) -> Option<CalendarObject> {
    let zones = calendar.build_tz_resolver();
    let mut components: Vec<Event> = calendar
        .components
        .iter()
        .filter_map(|component| {
            let kind = match component.component_type {
                ICalendarComponentType::VEvent => ItemKind::Event,
                ICalendarComponentType::VTodo => ItemKind::Task,
                ICalendarComponentType::VJournal => ItemKind::Other,
                _ => return None,
            };
            Some(event(component, kind, &|tzid| zones.resolve(tzid)))
        })
        .collect();
    if components.is_empty() {
        return None;
    }
    sort_components(&mut components);
    let zones = calendar
        .components
        .iter()
        .filter(|c| c.component_type == ICalendarComponentType::VTimezone)
        .filter_map(|c| zone_definition(calendar, c))
        .collect();
    Some(CalendarObject { components, zones })
}

pub(crate) fn sort_components(components: &mut [Event]) {
    components.sort_by(|a, b| {
        let key = |e: &Event| {
            e.recurrence_id
                .as_ref()
                .map(|r| (r.utc.unwrap_or(i64::MIN), r.value.clone()))
        };
        key(a).cmp(&key(b))
    });
}

fn event(
    component: &ICalendarComponent,
    kind: ItemKind,
    zones: &dyn Fn(&str) -> Option<Tz>,
) -> Event {
    let entry = |property: ICalendarProperty| component.property(&property);
    let text = |property: ICalendarProperty| {
        let parts: Vec<&str> = component
            .properties(&property)
            .flat_map(|e| e.values.iter())
            .filter_map(ICalendarValue::as_text)
            .collect();
        let joined = parts.join(", ");
        let joined = joined.trim();
        (!joined.is_empty()).then(|| joined.to_owned())
    };
    let start = entry(ICalendarProperty::Dtstart).and_then(|e| time(e, zones));
    let all_day = start.as_ref().is_some_and(|s| !s.value.contains('T'));
    let end = entry(ICalendarProperty::Dtend)
        .or_else(|| entry(ICalendarProperty::Due))
        .and_then(|e| time(e, zones))
        .or_else(|| {
            let seconds =
                entry(ICalendarProperty::Duration)?
                    .values
                    .iter()
                    .find_map(|v| match v {
                        ICalendarValue::Duration(d) => Some(d.as_seconds()),
                        _ => None,
                    })?;
            shifted(start.as_ref()?, seconds)
        });
    Event {
        kind,
        uid: text(ICalendarProperty::Uid),
        recurrence_id: entry(ICalendarProperty::RecurrenceId).and_then(|e| time(e, zones)),
        summary: text(ICalendarProperty::Summary),
        description: text(ICalendarProperty::Description),
        location: text(ICalendarProperty::Location),
        start,
        end,
        all_day,
        recurring: component.has_property(&ICalendarProperty::Rrule)
            || component.has_property(&ICalendarProperty::Rdate),
        organizer: entry(ICalendarProperty::Organizer).map(participant),
        attendees: component
            .properties(&ICalendarProperty::Attendee)
            .map(participant)
            .collect(),
        sequence: entry(ICalendarProperty::Sequence)
            .and_then(|e| e.values.first())
            .and_then(ICalendarValue::as_integer),
        dtstamp: entry(ICalendarProperty::Dtstamp)
            .and_then(|e| time(e, zones))
            .and_then(|t| t.utc),
        status: component.status().map(|s| s.as_str().to_owned()),
        rrule: entry(ICalendarProperty::Rrule)
            .and_then(|e| e.values.first())
            .and_then(|v| match v {
                ICalendarValue::RecurrenceRule(rule) => Some(rule.to_string()),
                _ => None,
            }),
        rdates: times(component, ICalendarProperty::Rdate, zones),
        exdates: times(component, ICalendarProperty::Exdate, zones),
        this_and_future: entry(ICalendarProperty::RecurrenceId).is_some_and(|e| {
            e.params
                .iter()
                .any(|p| p.name == ICalendarParameterName::Range)
        }),
        last_modified: entry(ICalendarProperty::LastModified)
            .and_then(|e| time(e, zones))
            .and_then(|t| t.utc),
    }
}

fn times(
    component: &ICalendarComponent,
    property: ICalendarProperty,
    zones: &dyn Fn(&str) -> Option<Tz>,
) -> Vec<EventTime> {
    let mut out = Vec::new();
    for entry in component.properties(&property) {
        let tzid = entry.tz_id();
        for value in &entry.values {
            let date = match value {
                ICalendarValue::PartialDateTime(date) => date.as_ref(),
                ICalendarValue::Period(
                    ICalendarPeriod::Range { start, .. } | ICalendarPeriod::Duration { start, .. },
                ) => start,
                _ => continue,
            };
            if let Some(t) = partial_time(date, tzid, zones) {
                out.push(t);
            }
        }
    }
    out.sort_by(|a, b| (a.utc, &a.value).cmp(&(b.utc, &b.value)));
    out.dedup();
    out
}

fn zone_definition(calendar: &ICalendar, component: &ICalendarComponent) -> Option<ZoneDefinition> {
    let tzid = component
        .property(&ICalendarProperty::Tzid)
        .and_then(|e| e.values.first())
        .and_then(ICalendarValue::as_text)?
        .trim()
        .to_owned();
    let iana = match component.timezone() {
        Some((_, Tz::Tz(tz))) => Some(tz.name().to_owned()),
        _ => None,
    };
    let observances = if iana.is_some() {
        Vec::new()
    } else {
        component
            .component_ids
            .iter()
            .filter_map(|id| calendar.components.get(*id as usize))
            .filter_map(observance)
            .collect()
    };
    Some(ZoneDefinition {
        tzid,
        iana,
        observances,
    })
}

fn observance(component: &ICalendarComponent) -> Option<Observance> {
    let daylight = match component.component_type {
        ICalendarComponentType::Daylight => true,
        ICalendarComponentType::Standard => false,
        _ => return None,
    };
    let local = |date: &PartialDateTime| {
        let value = format_partial(date)?;
        (value.len() >= 19).then(|| value[..19].to_owned())
    };
    let start = component
        .property(&ICalendarProperty::Dtstart)
        .and_then(|e| e.values.first())
        .and_then(ICalendarValue::as_partial_date_time)
        .and_then(local)?;
    let offset = |property: ICalendarProperty| {
        let date = component
            .property(&property)
            .and_then(|e| e.values.first())
            .and_then(ICalendarValue::as_partial_date_time)?;
        let seconds = i32::from(date.tz_hour?) * 3600 + i32::from(date.tz_minute.unwrap_or(0)) * 60;
        Some(if date.tz_minus { -seconds } else { seconds })
    };
    Some(Observance {
        daylight,
        start,
        offset_from: offset(ICalendarProperty::Tzoffsetfrom)?,
        offset_to: offset(ICalendarProperty::Tzoffsetto)?,
        rrule: component
            .property(&ICalendarProperty::Rrule)
            .and_then(|e| e.values.first())
            .and_then(|v| match v {
                ICalendarValue::RecurrenceRule(rule) => Some(rule.to_string()),
                _ => None,
            }),
        rdates: component
            .properties(&ICalendarProperty::Rdate)
            .flat_map(|e| e.values.iter())
            .filter_map(ICalendarValue::as_partial_date_time)
            .filter_map(local)
            .collect(),
    })
}

/// The IANA name of the zone a `TZID` stands for: an IANA name itself, a Windows zone name such
/// as `W. Europe Standard Time`, or a vendor prefix around an IANA name such as
/// `/softwarestudio.org/Olson_20011030_5/America/Chicago`. `None` when it names none of these.
pub fn iana_zone(tzid: &str) -> Option<String> {
    match tzid.trim().parse::<Tz>() {
        Ok(Tz::Tz(tz)) => Some(tz.name().to_owned()),
        _ => None,
    }
}

fn time(entry: &ICalendarEntry, zones: &dyn Fn(&str) -> Option<Tz>) -> Option<EventTime> {
    let date = entry.values.first()?.as_partial_date_time()?;
    partial_time(date, entry.tz_id(), zones)
}

fn partial_time(
    date: &PartialDateTime,
    tzid: Option<&str>,
    zones: &dyn Fn(&str) -> Option<Tz>,
) -> Option<EventTime> {
    let value = format_partial(date)?;
    let parsed = date.to_date_time()?;
    if !date.has_time() {
        return Some(EventTime {
            value,
            time_zone: None,
            utc: Some(parsed.date_time.and_utc().timestamp_millis()),
        });
    }
    if let Some(offset) = parsed.offset {
        let utc = parsed.date_time - TimeDelta::seconds(i64::from(offset.local_minus_utc()));
        return Some(EventTime {
            time_zone: Some(if offset.local_minus_utc() == 0 {
                "UTC".to_owned()
            } else {
                value[19..].to_owned()
            }),
            value,
            utc: Some(utc.and_utc().timestamp_millis()),
        });
    }
    let tzid = tzid.map(str::to_owned);
    let zone = tzid.as_deref().and_then(zones).unwrap_or(Tz::Floating);
    let local = parsed.date_time;
    let utc = zone
        .from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(local + TimeDelta::hours(1)))
                .earliest()
        })
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(|| local.and_utc().timestamp_millis());
    Some(EventTime {
        value,
        time_zone: tzid,
        utc: Some(utc),
    })
}

pub(crate) fn shifted(start: &EventTime, seconds: i64) -> Option<EventTime> {
    let utc = start.utc?.checked_add(seconds.checked_mul(1000)?)?;
    let delta = TimeDelta::try_seconds(seconds)?;
    let value = if start.value.len() == 10 {
        let date = NaiveDate::parse_from_str(&start.value, "%Y-%m-%d").ok()?;
        date.checked_add_signed(delta)?
            .format("%Y-%m-%d")
            .to_string()
    } else {
        let local =
            chrono::NaiveDateTime::parse_from_str(start.value.get(..19)?, "%Y-%m-%dT%H:%M:%S")
                .ok()?;
        let mut value = local
            .checked_add_signed(delta)?
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        value.push_str(&start.value[19..]);
        value
    };
    Some(EventTime {
        value,
        time_zone: start.time_zone.clone(),
        utc: Some(utc),
    })
}

fn participant(entry: &ICalendarEntry) -> Participant {
    let address = entry.values.first().and_then(|v| match v {
        ICalendarValue::Uri(Uri::Location(text)) => Some(text.clone()),
        other => other.as_text().map(str::to_owned),
    });
    let param = |name: ICalendarParameterName| {
        entry
            .parameter(&name)
            .and_then(|v| v.as_text())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    Participant {
        email: address.map(|a| normalize_email(&a).unwrap_or_else(|| a.trim().to_owned())),
        name: param(ICalendarParameterName::Cn),
        partstat: param(ICalendarParameterName::Partstat).map(|s| s.to_ascii_uppercase()),
        role: param(ICalendarParameterName::Role).map(|s| s.to_ascii_uppercase()),
    }
}

impl Event {
    /// The key shared by every copy of this event or instance; `None` without a `UID`.
    pub fn key(&self) -> Option<EventKey> {
        Some(EventKey {
            uid: self.uid.clone()?,
            recurrence_id: self.recurrence_id.as_ref().map(|r| match r.utc {
                Some(utc) if r.value.contains('T') => format_utc(utc),
                _ => r.value.clone(),
            }),
        })
    }
}

fn format_utc(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

impl CalendarObject {
    /// The object itself: the component without `RECURRENCE-ID`, else the first instance.
    pub fn main(&self) -> Option<&Event> {
        self.components.first()
    }
}
