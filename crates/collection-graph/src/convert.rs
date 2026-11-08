use calcard::icalendar::{
    ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarParameterName,
    ICalendarParameterValue, ICalendarProperty, ICalendarRelated, ICalendarValue,
};
use calcard::vcard::{VCard, VCardProperty, VCardValue};
use chrono::{Datelike, NaiveDate};
use holtmail_collection::{CollectionError, Field, ItemPatch, media};
use holtmail_pim::{Contact, Event, EventTime, Format, ItemKind, Normalized, iana_zone};
use serde_json::{Map, Value, json};

const EVENT_FIELDS: [Field; 11] = [
    Field::Summary,
    Field::Start,
    Field::End,
    Field::Recurrence,
    Field::Location,
    Field::Description,
    Field::Attendees,
    Field::Alarms,
    Field::Categories,
    Field::Transparency,
    Field::Privacy,
];

const CONTACT_FIELDS: [Field; 9] = [
    Field::Name,
    Field::Emails,
    Field::Phones,
    Field::Addresses,
    Field::Organization,
    Field::Title,
    Field::Notes,
    Field::Dates,
    Field::Urls,
];

const MAX_EMAILS: usize = 3;

const DAYS: [(&str, &str); 7] = [
    ("MO", "monday"),
    ("TU", "tuesday"),
    ("WE", "wednesday"),
    ("TH", "thursday"),
    ("FR", "friday"),
    ("SA", "saturday"),
    ("SU", "sunday"),
];

/// The Graph properties a change sets, and for an event that writes its recurrence, the
/// excluded occurrences (UTC milliseconds) it expects, which Graph cannot set by a change.
pub(crate) struct Draft {
    pub(crate) fields: Map<String, Value>,
    pub(crate) exdates: Option<Vec<i64>>,
}

fn refused(what: impl Into<String>) -> CollectionError {
    CollectionError::Refused(what.into())
}

fn wanted(patch: &ItemPatch, all: &[Field], create: bool) -> Vec<Field> {
    match &patch.fields {
        Some(fields) if !create => fields.clone(),
        _ => all.to_vec(),
    }
}

fn text(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |s| Value::String(s.clone()))
}

fn finish(mut fields: Map<String, Value>, create: bool) -> Map<String, Value> {
    if create {
        fields.retain(|_, v| !v.is_null());
    }
    fields
}

/// The Graph event properties for `patch`, an iCalendar object: every field it lists, or every
/// field Graph can take when it lists none or for a create, which has no base to differ from. Only the main component is written; changed
/// instances stay as the server holds them.
pub(crate) fn event(patch: &ItemPatch, create: bool) -> Result<Draft, CollectionError> {
    if !patch.content_type.eq_ignore_ascii_case(media::ICALENDAR) {
        return Err(refused("Microsoft Graph events are written from iCalendar"));
    }
    let Normalized::Calendar(object) =
        holtmail_pim::normalize_as(Format::ICalendar, ItemKind::Event, &patch.raw)
    else {
        return Err(refused("the change is not a readable iCalendar event"));
    };
    let Some(main) = object
        .main()
        .filter(|e| e.kind == ItemKind::Event && e.recurrence_id.is_none())
    else {
        return Err(refused("the change has no event to write"));
    };
    let text_raw = String::from_utf8_lossy(&patch.raw);
    let calendar = ICalendar::parse(text_raw.as_ref())
        .map_err(|_| refused("the change is not a readable iCalendar event"))?;
    let component = main_component(&calendar).ok_or_else(|| refused("no VEVENT to write"))?;
    let mut fields = Map::new();
    let mut exdates = None;
    for field in wanted(patch, &EVENT_FIELDS, create) {
        match field {
            Field::Summary => {
                fields.insert(
                    "subject".into(),
                    json!(main.summary.clone().unwrap_or_default()),
                );
            }
            Field::Start | Field::End => times(main, &mut fields)?,
            Field::Recurrence => {
                if !main.rdates.is_empty() {
                    return Err(refused(
                        "Microsoft Graph cannot add single dates to a series",
                    ));
                }
                let value = match (&main.rrule, &main.start) {
                    (Some(rule), Some(start)) => recurrence(rule, start, main.all_day)?,
                    (Some(_), None) => return Err(refused("a repeating event needs a start")),
                    (None, _) => Value::Null,
                };
                fields.insert("recurrence".into(), value);
                exdates = Some(main.exdates.iter().filter_map(|t| t.utc).collect());
            }
            Field::Location => {
                fields.insert(
                    "location".into(),
                    json!({ "displayName": main.location.clone().unwrap_or_default() }),
                );
            }
            Field::Description => {
                fields.insert(
                    "body".into(),
                    json!({
                        "contentType": "text",
                        "content": main.description.clone().unwrap_or_default(),
                    }),
                );
            }
            Field::Status => {
                if main.status.as_deref() == Some("CANCELLED") {
                    return Err(refused(
                        "Microsoft Graph cancels events with a request of its own",
                    ));
                }
            }
            Field::Organizer => {
                return Err(refused("Microsoft Graph sets the organizer itself"));
            }
            Field::Attendees => {
                fields.insert("attendees".into(), attendees(main));
            }
            Field::Alarms => alarm(&calendar, component, &mut fields)?,
            Field::Categories => {
                fields.insert("categories".into(), json!(categories(component)));
            }
            Field::Transparency => {
                let free = property_text(component, ICalendarProperty::Transp)
                    .is_some_and(|t| t.eq_ignore_ascii_case("TRANSPARENT"));
                fields.insert("showAs".into(), json!(if free { "free" } else { "busy" }));
            }
            Field::Privacy => {
                let class = property_text(component, ICalendarProperty::Class)
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let sensitivity = match class.as_str() {
                    "PRIVATE" => "private",
                    "CONFIDENTIAL" => "confidential",
                    _ => "normal",
                };
                fields.insert("sensitivity".into(), json!(sensitivity));
            }
            other => {
                return Err(refused(format!(
                    "Microsoft Graph events have no place for {other:?}"
                )));
            }
        }
    }
    Ok(Draft {
        fields: finish(fields, create),
        exdates,
    })
}

fn main_component(calendar: &ICalendar) -> Option<&ICalendarComponent> {
    calendar.components.iter().find(|c| {
        c.component_type == ICalendarComponentType::VEvent
            && c.property(&ICalendarProperty::RecurrenceId).is_none()
    })
}

fn property_text(component: &ICalendarComponent, property: ICalendarProperty) -> Option<String> {
    component
        .property(&property)
        .and_then(|e| e.values.first())
        .and_then(ICalendarValue::as_text)
        .map(str::to_owned)
}

fn categories(component: &ICalendarComponent) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in component
        .properties(&ICalendarProperty::Categories)
        .flat_map(|e| e.values.iter())
        .filter_map(ICalendarValue::as_text)
    {
        for name in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !out.iter().any(|c| c == name) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

fn alarm(
    calendar: &ICalendar,
    component: &ICalendarComponent,
    fields: &mut Map<String, Value>,
) -> Result<(), CollectionError> {
    let first = component
        .component_ids
        .iter()
        .filter_map(|id| calendar.component_by_id(*id))
        .find(|c| c.component_type == ICalendarComponentType::VAlarm);
    let Some(alarm) = first else {
        fields.insert("isReminderOn".into(), json!(false));
        return Ok(());
    };
    let trigger = alarm
        .property(&ICalendarProperty::Trigger)
        .ok_or_else(|| refused("an alarm without a trigger"))?;
    let from_end = trigger.params.iter().any(|p| {
        p.name == ICalendarParameterName::Related
            && p.value == ICalendarParameterValue::Related(ICalendarRelated::End)
    });
    let seconds = trigger
        .values
        .iter()
        .find_map(|v| match v {
            ICalendarValue::Duration(d) => Some(d.as_seconds()),
            _ => None,
        })
        .filter(|s| *s <= 0 && !from_end)
        .ok_or_else(|| {
            refused("Microsoft Graph reminders are a number of minutes before the start")
        })?;
    fields.insert("isReminderOn".into(), json!(true));
    fields.insert(
        "reminderMinutesBeforeStart".into(),
        json!((-seconds).div_euclid(60)),
    );
    Ok(())
}

fn attendees(main: &Event) -> Value {
    let organizer = main.organizer.as_ref().and_then(|o| o.email.clone());
    let list: Vec<Value> = main
        .attendees
        .iter()
        .filter(|a| a.email.is_some() && a.email != organizer)
        .map(|a| {
            let kind = match a.role.as_deref() {
                Some("OPT-PARTICIPANT") => "optional",
                Some("NON-PARTICIPANT") => "resource",
                _ => "required",
            };
            let mut address = json!({ "address": a.email });
            if let Some(name) = &a.name {
                address["name"] = json!(name);
            }
            json!({ "emailAddress": address, "type": kind })
        })
        .collect();
    Value::Array(list)
}

fn times(main: &Event, fields: &mut Map<String, Value>) -> Result<(), CollectionError> {
    let start = main
        .start
        .as_ref()
        .ok_or_else(|| refused("Microsoft Graph events need a start"))?;
    let end = match &main.end {
        Some(end) => end.clone(),
        None if main.all_day => {
            let date = date_of(start)?
                .succ_opt()
                .ok_or_else(|| refused("an unreadable start"))?;
            EventTime {
                value: date.format("%Y-%m-%d").to_string(),
                time_zone: None,
                utc: None,
            }
        }
        None => start.clone(),
    };
    fields.insert("isAllDay".into(), json!(main.all_day));
    fields.insert("start".into(), time(start, main.all_day)?);
    fields.insert("end".into(), time(&end, main.all_day)?);
    Ok(())
}

fn date_of(time: &EventTime) -> Result<NaiveDate, CollectionError> {
    time.value
        .get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| refused("an unreadable date"))
}

/// The zone name Graph is given for an event time.
fn zone_name(time: &EventTime) -> String {
    match time.time_zone.as_deref() {
        Some(tz) if !time.value.ends_with('Z') => iana_zone(tz).unwrap_or_else(|| tz.to_owned()),
        _ => "UTC".to_owned(),
    }
}

fn time(time: &EventTime, all_day: bool) -> Result<Value, CollectionError> {
    if all_day {
        let date = date_of(time)?;
        return Ok(json!({
            "dateTime": format!("{}T00:00:00", date.format("%Y-%m-%d")),
            "timeZone": "UTC",
        }));
    }
    let local = time
        .value
        .get(..19)
        .ok_or_else(|| refused("an unreadable time"))?;
    let rest = &time.value[19..];
    if rest.is_empty() || rest == "Z" {
        return Ok(json!({ "dateTime": local, "timeZone": zone_name(time) }));
    }
    let utc = time
        .utc
        .and_then(chrono::DateTime::from_timestamp_millis)
        .ok_or_else(|| refused("an unreadable time"))?;
    Ok(json!({
        "dateTime": utc.format("%Y-%m-%dT%H:%M:%S").to_string(),
        "timeZone": "UTC",
    }))
}

fn day_name(code: &str) -> Option<&'static str> {
    DAYS.iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, name)| *name)
}

fn index_name(n: i64) -> Result<&'static str, CollectionError> {
    Ok(match n {
        1 => "first",
        2 => "second",
        3 => "third",
        4 => "fourth",
        -1 => "last",
        _ => return Err(unsupported_rule()),
    })
}

fn unsupported_rule() -> CollectionError {
    refused("Microsoft Graph cannot hold this repeat rule")
}

fn number<T: std::str::FromStr>(value: &str) -> Result<T, CollectionError> {
    value.trim().parse().map_err(|_| unsupported_rule())
}

/// The Graph `patternedRecurrence` for an `RRULE`; `Refused` for rules Graph cannot hold.
pub(crate) fn recurrence(
    rule: &str,
    start: &EventTime,
    all_day: bool,
) -> Result<Value, CollectionError> {
    let mut freq = None;
    let mut interval: u32 = 1;
    let mut days: Vec<(Option<i64>, &'static str)> = Vec::new();
    let mut month_day: Option<u32> = None;
    let mut month: Option<u32> = None;
    let mut position: Option<i64> = None;
    let mut count: Option<u32> = None;
    let mut until: Option<NaiveDate> = None;
    let mut week_start = "monday";
    for part in rule.split(';').filter(|p| !p.trim().is_empty()) {
        let (key, value) = part.split_once('=').ok_or_else(unsupported_rule)?;
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(value.trim().to_ascii_uppercase()),
            "INTERVAL" => interval = number(value)?,
            "BYDAY" => {
                for day in value.split(',') {
                    let day = day.trim();
                    let split = day.len().saturating_sub(2);
                    let (n, code) = day.split_at(split);
                    let name = day_name(code).ok_or_else(unsupported_rule)?;
                    let n = if n.is_empty() {
                        None
                    } else {
                        Some(number::<i64>(n.trim_start_matches('+'))?)
                    };
                    days.push((n, name));
                }
            }
            "BYMONTHDAY" => month_day = Some(number(value).map_err(|_| unsupported_rule())?),
            "BYMONTH" => month = Some(number(value)?),
            "BYSETPOS" => position = Some(number(value)?),
            "COUNT" => count = Some(number(value)?),
            "UNTIL" => {
                let date = value.trim().get(..8).ok_or_else(unsupported_rule)?;
                until = Some(
                    NaiveDate::parse_from_str(date, "%Y%m%d").map_err(|_| unsupported_rule())?,
                );
            }
            "WKST" => week_start = day_name(value.trim()).ok_or_else(unsupported_rule)?,
            _ => return Err(unsupported_rule()),
        }
    }
    if interval == 0
        || (count.is_some() && until.is_some())
        || count == Some(0)
        || month_day.is_some_and(|d| !(1..=31).contains(&d))
        || month.is_some_and(|m| !(1..=12).contains(&m))
    {
        return Err(unsupported_rule());
    }
    let first = date_of(start)?;
    let plain_days = || -> Vec<&'static str> { days.iter().map(|(_, d)| *d).collect() };
    let all_plain = days.iter().all(|(n, _)| n.is_none());
    let relative = || -> Result<(Vec<&'static str>, &'static str), CollectionError> {
        let numbers: Vec<Option<i64>> = days.iter().map(|(n, _)| *n).collect();
        let n = match (position, numbers.first().copied().flatten()) {
            (Some(p), None) if all_plain => p,
            (None, Some(n)) if numbers.iter().all(|m| *m == Some(n)) => n,
            _ => return Err(unsupported_rule()),
        };
        Ok((plain_days(), index_name(n)?))
    };
    let start_day = DAYS[first.weekday().num_days_from_monday() as usize].1;
    let pattern = match freq.as_deref() {
        Some("DAILY") if days.is_empty() && month_day.is_none() && month.is_none() => {
            if position.is_some() {
                return Err(unsupported_rule());
            }
            json!({ "type": "daily", "interval": interval })
        }
        Some("DAILY")
            if all_plain
                && interval == 1
                && month_day.is_none()
                && month.is_none()
                && position.is_none() =>
        {
            json!({ "type": "weekly", "interval": 1, "daysOfWeek": plain_days(),
                "firstDayOfWeek": week_start })
        }
        Some("WEEKLY")
            if all_plain && month_day.is_none() && month.is_none() && position.is_none() =>
        {
            let list = if days.is_empty() {
                vec![start_day]
            } else {
                plain_days()
            };
            json!({ "type": "weekly", "interval": interval, "daysOfWeek": list,
                "firstDayOfWeek": week_start })
        }
        Some("MONTHLY") if month.is_none() => match (month_day, days.is_empty()) {
            (Some(d), true) if position.is_none() => {
                json!({ "type": "absoluteMonthly", "interval": interval, "dayOfMonth": d })
            }
            (None, true) if position.is_none() => {
                json!({ "type": "absoluteMonthly", "interval": interval, "dayOfMonth": first.day() })
            }
            (None, false) => {
                let (list, index) = relative()?;
                json!({ "type": "relativeMonthly", "interval": interval, "daysOfWeek": list,
                    "index": index })
            }
            _ => return Err(unsupported_rule()),
        },
        Some("YEARLY") => {
            let month = month.unwrap_or(first.month());
            match (month_day, days.is_empty()) {
                (d, true) if position.is_none() => json!({
                    "type": "absoluteYearly", "interval": interval, "month": month,
                    "dayOfMonth": d.unwrap_or(first.day()),
                }),
                (None, false) => {
                    let (list, index) = relative()?;
                    json!({ "type": "relativeYearly", "interval": interval, "month": month,
                        "daysOfWeek": list, "index": index })
                }
                _ => return Err(unsupported_rule()),
            }
        }
        _ => return Err(unsupported_rule()),
    };
    let zone = if all_day {
        "UTC".to_owned()
    } else {
        zone_name(start)
    };
    let start_date = first.format("%Y-%m-%d").to_string();
    let range = match (count, until) {
        (Some(n), _) => json!({ "type": "numbered", "startDate": start_date,
            "numberOfOccurrences": n, "recurrenceTimeZone": zone }),
        (None, Some(end)) => json!({ "type": "endDate", "startDate": start_date,
            "endDate": end.format("%Y-%m-%d").to_string(), "recurrenceTimeZone": zone }),
        (None, None) => json!({ "type": "noEnd", "startDate": start_date,
            "recurrenceTimeZone": zone }),
    };
    Ok(json!({ "pattern": pattern, "range": range }))
}

/// The Graph contact properties for `patch`, a vCard: every field it lists, or every field
/// Graph can take when it lists none or for a create. A value Graph has no room for (a fourth email address, a
/// second mobile number, a second address or web address of one kind) is `Refused`.
pub(crate) fn contact(patch: &ItemPatch, create: bool) -> Result<Draft, CollectionError> {
    if !patch.content_type.eq_ignore_ascii_case(media::VCARD) {
        return Err(refused("Microsoft Graph contacts are written from vCard"));
    }
    let Normalized::Contact(card) =
        holtmail_pim::normalize_as(Format::VCard, ItemKind::Contact, &patch.raw)
    else {
        return Err(refused("the change is not a readable vCard"));
    };
    let text_raw = String::from_utf8_lossy(&patch.raw);
    let vcard = VCard::parse(text_raw.as_ref()).map_err(|_| refused("not a readable vCard"))?;
    let mut fields = Map::new();
    for field in wanted(patch, &CONTACT_FIELDS, create) {
        match field {
            Field::Name => names(&card, &mut fields)?,
            Field::Emails => {
                if card.emails.len() > MAX_EMAILS {
                    return Err(refused(
                        "Microsoft Graph keeps at most three email addresses",
                    ));
                }
                let display = card.display_name();
                let list: Vec<Value> = card
                    .emails
                    .iter()
                    .map(|e| {
                        let mut v = json!({ "address": e.address });
                        if let Some(name) = &display {
                            v["name"] = json!(name);
                        }
                        v
                    })
                    .collect();
                fields.insert("emailAddresses".into(), Value::Array(list));
            }
            Field::Phones => phones(&card, &mut fields)?,
            Field::Addresses => addresses(&card, &mut fields)?,
            Field::Organization => {
                let units = organization(&vcard);
                fields.insert("companyName".into(), text(units.first()));
                let department = (units.len() > 1).then(|| units[1..].join(", "));
                fields.insert("department".into(), text(department.as_ref()));
            }
            Field::Title => {
                fields.insert("jobTitle".into(), text(card.title.as_ref()));
            }
            Field::Notes => {
                fields.insert("personalNotes".into(), text(card.note.as_ref()));
            }
            Field::Dates => {
                let birthday = match card.birthday.as_deref() {
                    None => Value::Null,
                    Some(b) => {
                        let date = NaiveDate::parse_from_str(b, "%Y-%m-%d").map_err(|_| {
                            refused("Microsoft Graph keeps birthdays only with a year")
                        })?;
                        json!(format!("{}T00:00:00Z", date.format("%Y-%m-%d")))
                    }
                };
                fields.insert("birthday".into(), birthday);
            }
            Field::Urls => {
                let urls = urls(&vcard);
                if urls.len() > 1 {
                    return Err(refused("Microsoft Graph keeps one web address"));
                }
                fields.insert("businessHomePage".into(), text(urls.first()));
            }
            Field::Photo => {
                return Err(refused("contact photos are not written to Microsoft Graph"));
            }
            other => {
                return Err(refused(format!(
                    "Microsoft Graph contacts have no place for {other:?}"
                )));
            }
        }
    }
    Ok(Draft {
        fields: finish(fields, create),
        exdates: None,
    })
}

fn names(card: &Contact, fields: &mut Map<String, Value>) -> Result<(), CollectionError> {
    if card.nicknames.len() > 1 {
        return Err(refused("Microsoft Graph keeps one nickname"));
    }
    let parts = card.name.clone().unwrap_or_default();
    fields.insert("displayName".into(), text(card.display_name().as_ref()));
    fields.insert("givenName".into(), text(parts.given.as_ref()));
    fields.insert("surname".into(), text(parts.family.as_ref()));
    fields.insert("middleName".into(), text(parts.additional.as_ref()));
    fields.insert("title".into(), text(parts.prefix.as_ref()));
    fields.insert("generation".into(), text(parts.suffix.as_ref()));
    fields.insert("nickName".into(), text(card.nicknames.first()));
    Ok(())
}

fn phones(card: &Contact, fields: &mut Map<String, Value>) -> Result<(), CollectionError> {
    let mut business = Vec::new();
    let mut home = Vec::new();
    let mut mobile: Option<String> = None;
    for phone in &card.phones {
        let has = |k: &str| phone.kinds.iter().any(|x| x.eq_ignore_ascii_case(k));
        if has("cell") || has("mobile") {
            if mobile.is_some() {
                return Err(refused("Microsoft Graph keeps one mobile number"));
            }
            mobile = Some(phone.number.clone());
        } else if has("home") {
            home.push(phone.number.clone());
        } else {
            business.push(phone.number.clone());
        }
    }
    fields.insert("businessPhones".into(), json!(business));
    fields.insert("homePhones".into(), json!(home));
    fields.insert("mobilePhone".into(), text(mobile.as_ref()));
    Ok(())
}

fn addresses(card: &Contact, fields: &mut Map<String, Value>) -> Result<(), CollectionError> {
    let mut slots: [(&str, Option<Value>); 3] = [
        ("homeAddress", None),
        ("businessAddress", None),
        ("otherAddress", None),
    ];
    for address in &card.addresses {
        let has = |k: &str| address.kinds.iter().any(|x| x.eq_ignore_ascii_case(k));
        let slot = if has("home") {
            0
        } else if has("work") {
            1
        } else {
            2
        };
        if slots[slot].1.is_some() {
            return Err(refused(
                "Microsoft Graph keeps one home, one work and one other address",
            ));
        }
        let street: Vec<&str> = [&address.po_box, &address.extended, &address.street]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        slots[slot].1 = Some(json!({
            "street": street.join("\n"),
            "city": address.locality.clone().unwrap_or_default(),
            "state": address.region.clone().unwrap_or_default(),
            "postalCode": address.postal_code.clone().unwrap_or_default(),
            "countryOrRegion": address.country.clone().unwrap_or_default(),
        }));
    }
    for (key, value) in slots {
        fields.insert(key.into(), value.unwrap_or(Value::Null));
    }
    Ok(())
}

fn vcard_texts(value: &VCardValue) -> Vec<String> {
    match value {
        VCardValue::Component(parts) => parts.clone(),
        other => other.as_text().map(str::to_owned).into_iter().collect(),
    }
}

fn organization(card: &VCard) -> Vec<String> {
    card.property(&VCardProperty::Org)
        .map(|e| {
            e.values
                .iter()
                .flat_map(vcard_texts)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn urls(card: &VCard) -> Vec<String> {
    card.properties(&VCardProperty::Url)
        .flat_map(|e| e.values.iter().flat_map(vcard_texts))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn ics(body: &str) -> ItemPatch {
        ItemPatch {
            content_type: media::ICALENDAR.into(),
            raw: Bytes::from(format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//test//EN\r\n\
                 BEGIN:VEVENT\r\nUID:e1@example.com\r\nDTSTAMP:20261101T090000Z\r\n\
                 {body}END:VEVENT\r\nEND:VCALENDAR\r\n"
            )),
            uid: Some("e1@example.com".into()),
            fields: None,
        }
    }

    fn vcf(body: &str) -> ItemPatch {
        ItemPatch {
            content_type: media::VCARD.into(),
            raw: Bytes::from(format!(
                "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1@example.com\r\n{body}END:VCARD\r\n"
            )),
            uid: Some("c1@example.com".into()),
            fields: None,
        }
    }

    fn start(value: &str, zone: Option<&str>) -> EventTime {
        EventTime {
            value: value.into(),
            time_zone: zone.map(Into::into),
            utc: None,
        }
    }

    #[test]
    fn only_listed_fields_are_sent() {
        let mut patch = ics(
            "DTSTART;TZID=Europe/Berlin:20261201T100000\r\nDTEND;TZID=Europe/Berlin:20261201T110000\r\n\
             SUMMARY:Planning\r\nLOCATION:Room 4\r\nCATEGORIES:Work,Plans\r\nTRANSP:TRANSPARENT\r\n\
             CLASS:PRIVATE\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n",
        );
        patch.fields = Some(vec![Field::Summary, Field::Start]);
        let draft = event(&patch, false).unwrap();
        assert_eq!(
            Value::Object(draft.fields),
            json!({
                "subject": "Planning",
                "isAllDay": false,
                "start": {"dateTime": "2026-12-01T10:00:00", "timeZone": "Europe/Berlin"},
                "end": {"dateTime": "2026-12-01T11:00:00", "timeZone": "Europe/Berlin"},
            })
        );
        patch.fields = None;
        let all = event(&patch, true).unwrap().fields;
        assert_eq!(all["location"], json!({"displayName": "Room 4"}));
        assert_eq!(all["categories"], json!(["Work", "Plans"]));
        assert_eq!(all["showAs"], json!("free"));
        assert_eq!(all["sensitivity"], json!("private"));
        assert_eq!(all["isReminderOn"], json!(true));
        assert_eq!(all["reminderMinutesBeforeStart"], json!(15));
        assert!(!all.contains_key("recurrence"), "a create leaves out nulls");
    }

    #[test]
    fn fields_graph_cannot_set_are_refused() {
        let mut patch = ics("DTSTART:20261201T100000Z\r\nSUMMARY:Planning\r\n");
        for field in [
            Field::Organizer,
            Field::Photo,
            Field::Emails,
            Field::Property("X-ODD".into()),
        ] {
            patch.fields = Some(vec![field.clone()]);
            assert!(
                matches!(event(&patch, false), Err(CollectionError::Refused(_))),
                "{field:?}"
            );
        }
        let alarm_after = ics(
            "DTSTART:20261201T100000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT5M\r\nEND:VALARM\r\n",
        );
        assert!(matches!(
            event(&alarm_after, false),
            Err(CollectionError::Refused(_))
        ));
        let mut cancelled = ics("DTSTART:20261201T100000Z\r\nSTATUS:CANCELLED\r\n");
        cancelled.fields = Some(vec![Field::Status]);
        assert!(matches!(
            event(&cancelled, false),
            Err(CollectionError::Refused(_))
        ));
        let card = vcf("FN:Ada Quill\r\n");
        assert!(matches!(
            event(&card, false),
            Err(CollectionError::Refused(_))
        ));
    }

    #[test]
    fn rules_become_patterns() {
        let berlin = start("2026-12-01T10:00:00", Some("Europe/Berlin"));
        assert_eq!(
            recurrence(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20270301T000000Z;WKST=SU",
                &berlin,
                false
            )
            .unwrap(),
            json!({
                "pattern": {"type": "weekly", "interval": 2, "daysOfWeek": ["monday", "wednesday"],
                    "firstDayOfWeek": "sunday"},
                "range": {"type": "endDate", "startDate": "2026-12-01", "endDate": "2027-03-01",
                    "recurrenceTimeZone": "Europe/Berlin"},
            })
        );
        let pattern = |rule: &str| recurrence(rule, &berlin, false).map(|v| v["pattern"].clone());
        assert_eq!(
            pattern("FREQ=WEEKLY;COUNT=4").unwrap(),
            json!({"type": "weekly", "interval": 1, "daysOfWeek": ["tuesday"], "firstDayOfWeek": "monday"})
        );
        assert_eq!(
            pattern("FREQ=MONTHLY;BYDAY=-1FR").unwrap(),
            json!({"type": "relativeMonthly", "interval": 1, "daysOfWeek": ["friday"], "index": "last"})
        );
        assert_eq!(
            pattern("FREQ=MONTHLY;BYDAY=TU;BYSETPOS=2").unwrap(),
            json!({"type": "relativeMonthly", "interval": 1, "daysOfWeek": ["tuesday"], "index": "second"})
        );
        assert_eq!(
            pattern("FREQ=MONTHLY;INTERVAL=3").unwrap(),
            json!({"type": "absoluteMonthly", "interval": 3, "dayOfMonth": 1})
        );
        assert_eq!(
            pattern("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH").unwrap(),
            json!({"type": "relativeYearly", "interval": 1, "month": 11, "daysOfWeek": ["thursday"],
                "index": "fourth"})
        );
        assert_eq!(
            pattern("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR").unwrap()["type"],
            json!("weekly")
        );
        for rule in [
            "FREQ=HOURLY",
            "FREQ=DAILY;BYHOUR=9",
            "FREQ=MONTHLY;BYDAY=5MO",
            "FREQ=WEEKLY;COUNT=2;UNTIL=20270101",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
        ] {
            assert!(
                matches!(
                    recurrence(rule, &berlin, false),
                    Err(CollectionError::Refused(_))
                ),
                "{rule}"
            );
        }
    }

    #[test]
    fn all_day_and_utc_times() {
        let draft = event(
            &ics("DTSTART;VALUE=DATE:20261224\r\nSUMMARY:Holiday\r\n"),
            false,
        )
        .unwrap()
        .fields;
        assert_eq!(draft["isAllDay"], json!(true));
        assert_eq!(
            draft["start"],
            json!({"dateTime": "2026-12-24T00:00:00", "timeZone": "UTC"})
        );
        assert_eq!(
            draft["end"],
            json!({"dateTime": "2026-12-25T00:00:00", "timeZone": "UTC"})
        );
        assert_eq!(draft["recurrence"], Value::Null, "an update clears it");
        assert_eq!(draft["isReminderOn"], json!(false));
        let utc = event(
            &ics("DTSTART:20261201T100000Z\r\nDTEND:20261201T103000Z\r\n"),
            false,
        )
        .unwrap()
        .fields;
        assert_eq!(
            utc["end"],
            json!({"dateTime": "2026-12-01T10:30:00", "timeZone": "UTC"})
        );
    }

    #[test]
    fn invitees_leave_out_the_organizer() {
        let mut patch = ics(
            "DTSTART:20261201T100000Z\r\nORGANIZER;CN=Olga:mailto:olga@example.org\r\n\
             ATTENDEE;CN=Olga;PARTSTAT=ACCEPTED:mailto:olga@example.org\r\n\
             ATTENDEE;ROLE=OPT-PARTICIPANT:mailto:ada@example.com\r\n",
        );
        patch.fields = Some(vec![Field::Attendees]);
        assert_eq!(
            event(&patch, false).unwrap().fields["attendees"],
            json!([{"emailAddress": {"address": "ada@example.com"}, "type": "optional"}])
        );
    }

    #[test]
    fn contacts_map_field_by_field() {
        let patch = vcf(
            "FN:Ada Quill\r\nN:Quill;Ada;M.;Dr.;\r\nNICKNAME:Addie\r\nEMAIL;TYPE=work:ada@example.com\r\n\
             TEL;TYPE=cell:+1 555 0100\r\nTEL;TYPE=home:+1 555 0101\r\nTEL;TYPE=work:+1 555 0102\r\n\
             ADR;TYPE=work:;Suite 4;1 Example Road;Springfield;IL;62701;USA\r\n\
             ORG:Example Co;Research;Lab\r\nTITLE:Engineer\r\nNOTE:Met at a fair\r\n\
             BDAY:19900115\r\nURL:https://example.com/ada\r\n",
        );
        let fields = contact(&patch, false).unwrap().fields;
        assert_eq!(fields["displayName"], json!("Ada Quill"));
        assert_eq!(fields["givenName"], json!("Ada"));
        assert_eq!(fields["surname"], json!("Quill"));
        assert_eq!(fields["middleName"], json!("M."));
        assert_eq!(fields["title"], json!("Dr."));
        assert_eq!(fields["generation"], Value::Null);
        assert_eq!(fields["nickName"], json!("Addie"));
        assert_eq!(
            fields["emailAddresses"],
            json!([{"address": "ada@example.com", "name": "Ada Quill"}])
        );
        assert_eq!(fields["mobilePhone"], json!("+1 555 0100"));
        assert_eq!(fields["homePhones"], json!(["+1 555 0101"]));
        assert_eq!(fields["businessPhones"], json!(["+1 555 0102"]));
        assert_eq!(
            fields["businessAddress"]["street"],
            json!("Suite 4\n1 Example Road")
        );
        assert_eq!(fields["homeAddress"], Value::Null);
        assert_eq!(fields["companyName"], json!("Example Co"));
        assert_eq!(fields["department"], json!("Research, Lab"));
        assert_eq!(fields["jobTitle"], json!("Engineer"));
        assert_eq!(fields["personalNotes"], json!("Met at a fair"));
        assert_eq!(fields["birthday"], json!("1990-01-15T00:00:00Z"));
        assert_eq!(fields["businessHomePage"], json!("https://example.com/ada"));
        let created = contact(&patch, true).unwrap().fields;
        assert!(!created.contains_key("homeAddress"));
    }

    #[test]
    fn contact_values_without_room_are_refused() {
        let too_many = vcf(
            "FN:Ada Quill\r\nEMAIL:a@example.com\r\nEMAIL:b@example.com\r\nEMAIL:c@example.com\r\nEMAIL:d@example.com\r\n",
        );
        assert!(matches!(
            contact(&too_many, false),
            Err(CollectionError::Refused(_))
        ));
        let two_mobiles = vcf("FN:Ada Quill\r\nTEL;TYPE=cell:1\r\nTEL;TYPE=cell:2\r\n");
        assert!(matches!(
            contact(&two_mobiles, false),
            Err(CollectionError::Refused(_))
        ));
        let mut photo = vcf("FN:Ada Quill\r\n");
        photo.fields = Some(vec![Field::Photo]);
        assert!(matches!(
            contact(&photo, false),
            Err(CollectionError::Refused(_))
        ));
        let mut yearless = vcf("FN:Ada Quill\r\nBDAY:--0115\r\n");
        yearless.fields = Some(vec![Field::Dates]);
        assert!(matches!(
            contact(&yearless, false),
            Err(CollectionError::Refused(_))
        ));
    }
}
