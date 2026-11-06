use std::panic::{AssertUnwindSafe, catch_unwind};

use calcard::icalendar::ICalendar;
use calcard::jscalendar::JSCalendar;
use calcard::jscontact::JSContact;
use calcard::vcard::VCard;
use serde_json::{Map, Value};

use crate::contact::{Contact, EmbeddedPhoto, from_card};
use crate::event::{CalendarObject, from_calendar};

fn card(raw: &[u8]) -> Option<VCard> {
    let text = String::from_utf8_lossy(raw);
    catch_unwind(AssertUnwindSafe(|| {
        JSContact::<String, String>::parse(text.as_ref())
            .ok()?
            .into_vcard()
    }))
    .ok()?
}

pub(crate) fn contact(raw: &[u8]) -> Option<(Contact, Option<EmbeddedPhoto>)> {
    Some(from_card(&card(raw)?))
}

/// The JSContact card as vCard text, as `calcard` writes it.
pub(crate) fn vcard(raw: &[u8]) -> Option<String> {
    Some(card(raw)?.to_string())
}

/// RFC 8984 names for what JSCalendar 2.0, which `calcard` reads, calls otherwise: the first of
/// `recurrenceRules` as `recurrenceRule`, a participant's `sendTo.imip` (else `email`) as
/// `calendarAddress`, and `replyTo.imip` as `organizerCalendarAddress`.
fn upgrade(object: &mut Map<String, Value>) {
    if !object.contains_key("recurrenceRule")
        && let Some(first) = object
            .get("recurrenceRules")
            .and_then(Value::as_array)
            .and_then(|rules| rules.first())
            .cloned()
    {
        object.insert("recurrenceRule".into(), first);
    }
    object.remove("recurrenceRules");
    if !object.contains_key("organizerCalendarAddress")
        && let Some(address) = object.pointer_imip("replyTo")
    {
        object.insert("organizerCalendarAddress".into(), Value::String(address));
    }
    if let Some(participants) = object
        .get_mut("participants")
        .and_then(Value::as_object_mut)
    {
        for participant in participants.values_mut().filter_map(Value::as_object_mut) {
            if participant.contains_key("calendarAddress") {
                continue;
            }
            let address = participant.pointer_imip("sendTo").or_else(|| {
                participant
                    .get("email")
                    .and_then(Value::as_str)
                    .map(|e| format!("mailto:{}", e.trim()))
            });
            if let Some(address) = address {
                participant.insert("calendarAddress".into(), Value::String(address));
            }
        }
    }
    if let Some(overrides) = object
        .get_mut("recurrenceOverrides")
        .and_then(Value::as_object_mut)
    {
        for patch in overrides.values_mut().filter_map(Value::as_object_mut) {
            upgrade(patch);
        }
    }
}

trait Imip {
    fn pointer_imip(&self, key: &str) -> Option<String>;
}

impl Imip for Map<String, Value> {
    fn pointer_imip(&self, key: &str) -> Option<String> {
        self.get(key)?
            .get("imip")?
            .as_str()
            .map(|s| s.trim().to_owned())
    }
}

fn converted(raw: &[u8]) -> Option<ICalendar> {
    let mut value: Value = serde_json::from_slice(raw).ok()?;
    upgrade(value.as_object_mut()?);
    let text = serde_json::json!({ "@type": "Group", "entries": [value] }).to_string();
    catch_unwind(AssertUnwindSafe(|| {
        JSCalendar::<String, String>::parse(&text)
            .ok()?
            .into_icalendar()
    }))
    .ok()?
}

pub(crate) fn calendar(raw: &[u8]) -> Option<CalendarObject> {
    let calendar = converted(raw)?;
    let mut object = from_calendar(&calendar)?;
    inherit(&mut object);
    Some(object)
}

/// A JSCalendar override is a patch: what it leaves out is the series' value. `calcard` writes
/// only the patched properties into the changed instance, so the rest is filled in here.
fn inherit(object: &mut CalendarObject) {
    let Some(master) = object
        .components
        .iter()
        .find(|e| e.recurrence_id.is_none())
        .cloned()
    else {
        return;
    };
    let length = match (&master.start, &master.end) {
        (Some(start), Some(end)) => start.utc.zip(end.utc).map(|(s, e)| e - s),
        _ => None,
    };
    for instance in object
        .components
        .iter_mut()
        .filter(|e| e.recurrence_id.is_some() && e.uid == master.uid)
    {
        instance.summary = instance.summary.take().or(master.summary.clone());
        instance.description = instance.description.take().or(master.description.clone());
        instance.location = instance.location.take().or(master.location.clone());
        instance.organizer = instance.organizer.take().or(master.organizer.clone());
        if instance.attendees.is_empty() {
            instance.attendees = master.attendees.clone();
        }
        instance.status = instance.status.take().or(master.status.clone());
        instance.sequence = instance.sequence.or(master.sequence);
        if instance.start.is_none() {
            instance.start = instance.recurrence_id.clone();
        }
        if instance.end.is_none()
            && let (Some(start), Some(length)) = (&instance.start, length)
        {
            instance.end = crate::event::shifted(start, length / 1000);
        }
    }
}
