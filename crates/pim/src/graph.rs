use calcard::common::timezone::Tz;
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use serde_json::Value;

use crate::ItemKind;
use crate::contact::{Contact, Email, NameParts, Phone, PostalAddress};
use crate::event::{CalendarObject, Event, EventTime, Participant, sort_components};
use crate::keys::{normalize_email, normalize_phone};

fn text(value: &Value, key: &str) -> Option<String> {
    let s = value.get(key)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn object(raw: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    value.is_object().then_some(value)
}

pub(crate) fn contact(raw: &[u8]) -> Option<Contact> {
    let card = object(raw)?;
    let mut contact = Contact {
        rev: text(&card, "lastModifiedDateTime"),
        full_name: text(&card, "displayName"),
        nicknames: text(&card, "nickName").into_iter().collect(),
        title: text(&card, "jobTitle"),
        birthday: text(&card, "birthday").map(|b| b.get(..10).unwrap_or(&b).to_owned()),
        note: text(&card, "personalNotes"),
        ..Contact::default()
    };
    let name = NameParts {
        family: text(&card, "surname"),
        given: text(&card, "givenName"),
        additional: text(&card, "middleName"),
        prefix: text(&card, "title"),
        suffix: text(&card, "generation"),
    };
    contact.name = (name != NameParts::default()).then_some(name);
    let units: Vec<String> = ["companyName", "department"]
        .iter()
        .filter_map(|k| text(&card, k))
        .collect();
    contact.organization = (!units.is_empty()).then(|| units.join(", "));
    for email in card
        .get("emailAddresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(address) = text(email, "address") {
            contact.emails.push(Email {
                normalized: normalize_email(&address),
                address,
                kinds: Vec::new(),
            });
        }
    }
    let phones = strings(&card, "businessPhones")
        .into_iter()
        .map(|n| (n, "work"))
        .chain(
            strings(&card, "homePhones")
                .into_iter()
                .map(|n| (n, "home")),
        )
        .chain(text(&card, "mobilePhone").map(|n| (n, "cell")));
    for (number, kind) in phones {
        contact.phones.push(Phone {
            e164: normalize_phone(&number),
            number,
            kinds: vec![kind.to_owned()],
        });
    }
    for (key, kind) in [
        ("homeAddress", "home"),
        ("businessAddress", "work"),
        ("otherAddress", "other"),
    ] {
        let Some(address) = card.get(key).filter(|a| a.is_object()) else {
            continue;
        };
        let postal = PostalAddress {
            kinds: vec![kind.to_owned()],
            street: text(address, "street"),
            locality: text(address, "city"),
            region: text(address, "state"),
            postal_code: text(address, "postalCode"),
            country: text(address, "countryOrRegion"),
            ..PostalAddress::default()
        };
        if postal
            != (PostalAddress {
                kinds: postal.kinds.clone(),
                ..PostalAddress::default()
            })
        {
            contact.addresses.push(postal);
        }
    }
    Some(contact)
}

/// A zone named by Graph: an IANA or Windows name, `tzone://Microsoft/Utc`. `None` for custom
/// zones and names nobody knows.
fn zone(name: &str) -> Option<Tz> {
    let name = name.trim();
    if name.eq_ignore_ascii_case("tzone://Microsoft/Utc") {
        return "UTC".parse().ok();
    }
    match name.parse::<Tz>() {
        Ok(Tz::Floating) | Err(_) => None,
        Ok(tz) => Some(tz),
    }
}

fn local(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim().trim_end_matches('Z');
    let text = text.get(..19).unwrap_or(text);
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").ok()
}

fn utc_millis(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.timestamp_millis())
        .or_else(|| local(text).map(|l| l.and_utc().timestamp_millis()))
}

fn to_local(utc: i64, zone: &Tz) -> Option<NaiveDateTime> {
    let at = chrono::DateTime::from_timestamp_millis(utc)?.naive_utc();
    Some(zone.from_utc_datetime(&at).naive_local())
}

/// The zone an event's times are shown in: the one it was created in, if known.
struct Shown {
    name: Option<String>,
    tz: Option<Tz>,
}

impl Shown {
    fn time(&self, utc: i64) -> Option<EventTime> {
        match (&self.name, &self.tz) {
            (Some(name), Some(tz)) => Some(EventTime {
                value: to_local(utc, tz)?.format("%Y-%m-%dT%H:%M:%S").to_string(),
                time_zone: Some(name.clone()),
                utc: Some(utc),
            }),
            _ => Some(EventTime {
                value: chrono::DateTime::from_timestamp_millis(utc)?
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string(),
                time_zone: Some("UTC".to_owned()),
                utc: Some(utc),
            }),
        }
    }
}

fn date_time(value: &Value, shown: &Shown, all_day: bool) -> Option<EventTime> {
    let at = local(value.get("dateTime")?.as_str()?)?;
    if all_day {
        let date = at.date();
        return Some(EventTime {
            value: date.format("%Y-%m-%d").to_string(),
            time_zone: None,
            utc: Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()),
        });
    }
    let written = value
        .get("timeZone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    let tz = zone(written).unwrap_or(Tz::UTC);
    let utc = tz
        .from_local_datetime(&at)
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&(at + chrono::TimeDelta::hours(1)))
                .earliest()
        })
        .map(|t| t.timestamp_millis())?;
    shown.time(utc)
}

fn shown(event: &Value) -> Shown {
    let name = [
        event.get("originalStartTimeZone"),
        event.pointer("/recurrence/range/recurrenceTimeZone"),
        event.pointer("/start/timeZone"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|n| zone(n).is_some());
    match name {
        Some(n) if n.eq_ignore_ascii_case("tzone://Microsoft/Utc") => Shown {
            name: Some("UTC".into()),
            tz: zone(n),
        },
        Some(n) => Shown {
            name: Some(n.to_owned()),
            tz: zone(n),
        },
        None => Shown {
            name: None,
            tz: None,
        },
    }
}

fn participant(value: &Value) -> Option<Participant> {
    let email = value.get("emailAddress")?;
    let address = text(email, "address");
    let name = text(email, "name");
    if address.is_none() && name.is_none() {
        return None;
    }
    let partstat = value
        .pointer("/status/response")
        .and_then(Value::as_str)
        .map(|r| match r {
            "accepted" | "organizer" => "ACCEPTED",
            "declined" => "DECLINED",
            "tentativelyAccepted" => "TENTATIVE",
            _ => "NEEDS-ACTION",
        })
        .map(str::to_owned);
    let role = value.get("type").and_then(Value::as_str).map(|t| {
        match t {
            "optional" => "OPT-PARTICIPANT",
            "resource" => "NON-PARTICIPANT",
            _ => "REQ-PARTICIPANT",
        }
        .to_owned()
    });
    Some(Participant {
        email: address.map(|a| normalize_email(&a).unwrap_or(a)),
        name,
        partstat,
        role,
    })
}

fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    for c in html.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if matches!(name.as_str(), "br" | "p" | "div" | "li" | "tr") {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let mut text = String::new();
    for line in lines {
        if line.is_empty() && (text.is_empty() || text.ends_with("\n\n")) {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim().to_owned()
}

fn description(event: &Value) -> Option<String> {
    let body = event.get("body")?;
    let content = body.get("content")?.as_str()?;
    let text = if body
        .get("contentType")
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("html"))
    {
        html_text(content)
    } else {
        content.trim().to_owned()
    };
    (!text.is_empty()).then_some(text)
}

fn location(event: &Value) -> Option<String> {
    event
        .get("location")
        .and_then(|l| text(l, "displayName"))
        .or_else(|| {
            let names: Vec<String> = event
                .get("locations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| text(l, "displayName"))
                .collect();
            (!names.is_empty()).then(|| names.join("; "))
        })
}

const DAYS: [(&str, &str); 7] = [
    ("monday", "MO"),
    ("tuesday", "TU"),
    ("wednesday", "WE"),
    ("thursday", "TH"),
    ("friday", "FR"),
    ("saturday", "SA"),
    ("sunday", "SU"),
];

fn day_code(day: &str) -> Option<&'static str> {
    DAYS.iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(day))
        .map(|(_, code)| *code)
}

/// The `RRULE` equivalent of a Graph `patternedRecurrence`; `None` for a pattern it cannot be
/// written as.
pub(crate) fn rrule(recurrence: &Value) -> Option<String> {
    let pattern = recurrence.get("pattern")?;
    let range = recurrence.get("range");
    let kind = pattern.get("type")?.as_str()?;
    let interval = pattern
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    let days: Vec<&str> = pattern
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(day_code)
        .collect();
    let month = pattern
        .get("month")
        .and_then(Value::as_u64)
        .filter(|m| (1..=12).contains(m));
    let day_of_month = pattern
        .get("dayOfMonth")
        .and_then(Value::as_u64)
        .filter(|d| (1..=31).contains(d));
    let position = match pattern.get("index").and_then(Value::as_str) {
        Some("second") => 2,
        Some("third") => 3,
        Some("fourth") => 4,
        Some("last") => -1,
        _ => 1,
    };
    let mut parts: Vec<String> = Vec::new();
    match kind {
        "daily" => parts.push("FREQ=DAILY".into()),
        "weekly" => {
            parts.push("FREQ=WEEKLY".into());
            if !days.is_empty() {
                parts.push(format!("BYDAY={}", days.join(",")));
            }
        }
        "absoluteMonthly" => {
            parts.push("FREQ=MONTHLY".into());
            parts.push(format!("BYMONTHDAY={}", day_of_month?));
        }
        "relativeMonthly" => {
            parts.push("FREQ=MONTHLY".into());
            parts.push(format!("BYDAY={}", days.join(",")));
            parts.push(format!("BYSETPOS={position}"));
            if days.is_empty() {
                return None;
            }
        }
        "absoluteYearly" => {
            parts.push("FREQ=YEARLY".into());
            parts.push(format!("BYMONTH={}", month?));
            parts.push(format!("BYMONTHDAY={}", day_of_month?));
        }
        "relativeYearly" => {
            parts.push("FREQ=YEARLY".into());
            parts.push(format!("BYMONTH={}", month?));
            if days.is_empty() {
                return None;
            }
            parts.push(format!("BYDAY={}", days.join(",")));
            parts.push(format!("BYSETPOS={position}"));
        }
        _ => return None,
    }
    if interval > 1 {
        parts.insert(1, format!("INTERVAL={interval}"));
    }
    match range.and_then(|r| r.get("type")).and_then(Value::as_str) {
        Some("endDate") => {
            let end = range
                .and_then(|r| r.get("endDate"))
                .and_then(Value::as_str)?;
            let date = NaiveDate::parse_from_str(end.get(..10)?, "%Y-%m-%d").ok()?;
            parts.push(format!("UNTIL={}", date.format("%Y%m%d")));
        }
        Some("numbered") => {
            let count = range
                .and_then(|r| r.get("numberOfOccurrences"))
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)?;
            parts.push(format!("COUNT={count}"));
        }
        _ => {}
    }
    if kind == "weekly"
        && let Some(first) = pattern
            .get("firstDayOfWeek")
            .and_then(Value::as_str)
            .and_then(day_code)
        && first != "MO"
    {
        parts.push(format!("WKST={first}"));
    }
    Some(parts.join(";"))
}

fn one(value: &Value, shown: &Shown, uid: Option<String>) -> Event {
    let all_day = value.get("isAllDay").and_then(Value::as_bool) == Some(true);
    let recurrence = value.get("recurrence").filter(|r| r.is_object());
    let recurrence_id = value
        .get("originalStart")
        .and_then(Value::as_str)
        .and_then(utc_millis)
        .and_then(|utc| {
            if all_day {
                let date = chrono::DateTime::from_timestamp_millis(utc)?.date_naive();
                Some(EventTime {
                    value: date.format("%Y-%m-%d").to_string(),
                    time_zone: None,
                    utc: Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()),
                })
            } else {
                shown.time(utc)
            }
        });
    Event {
        kind: ItemKind::Event,
        uid,
        recurrence_id,
        summary: text(value, "subject"),
        description: description(value),
        location: location(value),
        start: value
            .get("start")
            .and_then(|s| date_time(s, shown, all_day)),
        end: value.get("end").and_then(|s| date_time(s, shown, all_day)),
        all_day,
        recurring: recurrence.is_some(),
        organizer: value.get("organizer").and_then(participant),
        attendees: value
            .get("attendees")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(participant)
            .collect(),
        sequence: None,
        dtstamp: None,
        status: (value.get("isCancelled").and_then(Value::as_bool) == Some(true))
            .then(|| "CANCELLED".to_owned()),
        rrule: recurrence.and_then(rrule),
        rdates: Vec::new(),
        exdates: Vec::new(),
        this_and_future: false,
        last_modified: value
            .get("lastModifiedDateTime")
            .and_then(Value::as_str)
            .and_then(utc_millis),
    }
}

/// The instance an `occurrenceId` such as `OID.<master id>.2025-03-17` names, at the series'
/// local start time.
fn cancelled(occurrence: &str, master: &Event) -> Option<EventTime> {
    let date = occurrence.rsplit('.').next()?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let start = master.start.as_ref()?;
    if master.all_day {
        return Some(EventTime {
            value: date.format("%Y-%m-%d").to_string(),
            time_zone: None,
            utc: Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()),
        });
    }
    let time = chrono::NaiveTime::parse_from_str(start.value.get(11..19)?, "%H:%M:%S").ok()?;
    let local = date.and_time(time);
    let tz = start.time_zone.as_deref().and_then(zone).unwrap_or(Tz::UTC);
    let utc = tz
        .from_local_datetime(&local)
        .earliest()
        .map(|t| t.timestamp_millis())?;
    let value = if start.value.ends_with('Z') {
        format!("{}Z", local.format("%Y-%m-%dT%H:%M:%S"))
    } else {
        local.format("%Y-%m-%dT%H:%M:%S").to_string()
    };
    Some(EventTime {
        value,
        time_zone: start.time_zone.clone(),
        utc: Some(utc),
    })
}

pub(crate) fn event(raw: &[u8]) -> Option<CalendarObject> {
    let value = object(raw)?;
    value.get("start")?;
    let shown = shown(&value);
    let uid = text(&value, "iCalUId");
    let mut master = one(&value, &shown, uid.clone());
    master.exdates = strings(&value, "cancelledOccurrences")
        .iter()
        .filter_map(|o| cancelled(o, &master))
        .collect();
    master
        .exdates
        .sort_by(|a, b| (a.utc, &a.value).cmp(&(b.utc, &b.value)));
    master.exdates.dedup();
    let mut components = vec![master];
    for exception in value
        .get("exceptionOccurrences")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut instance = one(exception, &shown, uid.clone());
        instance.recurring = false;
        instance.rrule = None;
        if instance.recurrence_id.is_some() {
            components.push(instance);
        }
    }
    sort_components(&mut components);
    Some(CalendarObject {
        components,
        zones: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patterns_become_rules() {
        let rule = |r: Value| rrule(&r);
        assert_eq!(
            rule(
                json!({"pattern": {"type": "weekly", "interval": 2, "daysOfWeek": ["monday", "wednesday"], "firstDayOfWeek": "sunday"},
                "range": {"type": "endDate", "startDate": "2025-03-03", "endDate": "2025-04-28"}})
            ),
            Some("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20250428;WKST=SU".into())
        );
        assert_eq!(
            rule(
                json!({"pattern": {"type": "relativeMonthly", "interval": 1, "daysOfWeek": ["friday"], "index": "last"},
                "range": {"type": "numbered", "numberOfOccurrences": 6}})
            ),
            Some("FREQ=MONTHLY;BYDAY=FR;BYSETPOS=-1;COUNT=6".into())
        );
        assert_eq!(
            rule(
                json!({"pattern": {"type": "absoluteYearly", "interval": 1, "month": 5, "dayOfMonth": 17},
                "range": {"type": "noEnd"}})
            ),
            Some("FREQ=YEARLY;BYMONTH=5;BYMONTHDAY=17".into())
        );
        assert_eq!(
            rule(
                json!({"pattern": {"type": "relativeYearly", "interval": 1, "month": 11, "daysOfWeek": ["thursday"], "index": "fourth"},
                "range": {"type": "noEnd"}})
            ),
            Some("FREQ=YEARLY;BYMONTH=11;BYDAY=TH;BYSETPOS=4".into())
        );
        assert_eq!(
            rule(
                json!({"pattern": {"type": "absoluteMonthly", "interval": 3, "dayOfMonth": 31}, "range": {"type": "noEnd"}})
            ),
            Some("FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=31".into())
        );
        assert_eq!(
            rule(json!({"pattern": {"type": "daily", "interval": 1}, "range": {"type": "noEnd"}})),
            Some("FREQ=DAILY".into())
        );
        assert_eq!(rule(json!({"pattern": {"type": "hourly"}})), None);
    }

    #[test]
    fn html_bodies_read_as_text() {
        assert_eq!(
            html_text(
                "<html><body><p>Bring &amp; share</p><div>Room&nbsp;4<br>Floor 2</div></body></html>"
            ),
            "Bring & share\n\nRoom 4\nFloor 2"
        );
    }
}
