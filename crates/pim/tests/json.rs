//! Reading JSON sources: Microsoft Graph contacts and events, JSContact and JSCalendar.

use holtmail_pim::*;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn contact(format: Format, raw: &[u8]) -> Contact {
    match normalize_as(format, ItemKind::Contact, raw) {
        Normalized::Contact(contact) => *contact,
        other => panic!("not a contact: {other:?}"),
    }
}

fn calendar(format: Format, raw: &[u8]) -> CalendarObject {
    match normalize_as(format, ItemKind::Event, raw) {
        Normalized::Calendar(calendar) => calendar,
        other => panic!("not a calendar: {other:?}"),
    }
}

#[test]
fn reads_a_graph_contact() {
    let ada = contact(Format::GraphContact, &fixture("graph-contact.json"));
    assert_eq!(ada.full_name.as_deref(), Some("Ada Quill"));
    assert_eq!(ada.name.as_ref().unwrap().formatted(), "Dr. Ada Quill");
    assert_eq!(ada.nicknames, vec!["Ada Q".to_owned()]);
    assert_eq!(
        ada.emails
            .iter()
            .map(|e| e.normalized.clone().unwrap())
            .collect::<Vec<_>>(),
        vec!["ada@example.com", "ada.quill@example.org"]
    );
    let phones: Vec<(Option<&str>, &str)> = ada
        .phones
        .iter()
        .map(|p| (p.e164.as_deref(), p.kinds[0].as_str()))
        .collect();
    assert_eq!(
        phones,
        vec![
            (None, "work"),
            (Some("+442079460000"), "home"),
            (Some("+15550100"), "cell")
        ]
    );
    assert_eq!(ada.organization.as_deref(), Some("Harbour Lights, Design"));
    assert_eq!(ada.title.as_deref(), Some("Lamp designer"));
    assert_eq!(ada.birthday.as_deref(), Some("1990-05-17"));
    assert_eq!(ada.note.as_deref(), Some("Met at the lamp fair."));
    assert_eq!(ada.addresses.len(), 1, "empty addresses are dropped");
    assert_eq!(ada.addresses[0].kinds, vec!["home".to_owned()]);
    assert_eq!(ada.addresses[0].locality.as_deref(), Some("Port Example"));
    assert_eq!(ada.rev.as_deref(), Some("2025-03-02T10:15:00Z"));
    let keys = Normalized::Contact(Box::new(ada)).contact_keys();
    assert!(keys.contains(&"email:ada@example.com".to_owned()));
}

#[test]
fn reads_a_graph_series_with_exceptions_and_cancellations() {
    let object = calendar(Format::GraphEvent, &fixture("graph-event.json"));
    assert_eq!(object.components.len(), 2);
    let master = &object.components[0];
    assert_eq!(
        master.uid.as_deref(),
        Some("040000008200E00074C5B7101A82E00800000000standup")
    );
    assert_eq!(master.summary.as_deref(), Some("Standup"));
    assert_eq!(master.description.as_deref(), Some("Daily notes & plans"));
    assert_eq!(master.location.as_deref(), Some("Room 4"));
    let start = master.start.as_ref().unwrap();
    assert_eq!(start.value, "2025-03-03T09:00:00", "Berlin wall clock");
    assert_eq!(start.time_zone.as_deref(), Some("W. Europe Standard Time"));
    assert_eq!(start.utc, Some(1_740_988_800_000));
    assert_eq!(
        master.rrule.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO;UNTIL=20250428")
    );
    assert!(master.recurring);
    assert_eq!(master.exdates.len(), 1);
    assert_eq!(master.exdates[0].value, "2025-04-07T09:00:00");
    assert_eq!(
        master.exdates[0].utc,
        Some(1_744_009_200_000),
        "07:00Z after the change to summer time"
    );
    assert_eq!(
        master.organizer.as_ref().unwrap().email.as_deref(),
        Some("ada@example.com")
    );
    let answers: Vec<(&str, &str, &str)> = master
        .attendees
        .iter()
        .map(|a| {
            (
                a.email.as_deref().unwrap(),
                a.partstat.as_deref().unwrap(),
                a.role.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        answers,
        vec![
            ("bob@example.org", "ACCEPTED", "REQ-PARTICIPANT"),
            ("cleo@example.net", "TENTATIVE", "OPT-PARTICIPANT"),
            ("room4@example.com", "NEEDS-ACTION", "NON-PARTICIPANT")
        ]
    );
    assert_eq!(master.last_modified, Some(1_742_473_800_123));
    let moved = &object.components[1];
    assert_eq!(moved.uid, master.uid, "exceptions keep the series' UID");
    assert_eq!(
        moved.recurrence_id.as_ref().unwrap().value,
        "2025-03-24T09:00:00"
    );
    assert_eq!(moved.start.as_ref().unwrap().value, "2025-03-25T14:00:00");
    assert_eq!(moved.location.as_deref(), Some("Room 5"));
    assert!(moved.rrule.is_none());
}

#[test]
fn reads_a_cancelled_all_day_graph_event_in_a_custom_zone() {
    let object = calendar(Format::GraphEvent, &fixture("graph-allday.json"));
    let event = &object.components[0];
    assert!(event.all_day);
    assert_eq!(event.start.as_ref().unwrap().value, "2025-05-10");
    assert_eq!(event.end.as_ref().unwrap().value, "2025-05-12");
    assert_eq!(event.status.as_deref(), Some("CANCELLED"));
    assert!(event.description.is_none());
}

#[test]
fn reads_a_jscontact_card() {
    let read = read_as(
        Format::JsContact,
        ItemKind::Contact,
        &fixture("jscontact.json"),
    );
    let Normalized::Contact(bea) = read.normalized else {
        panic!("not a contact");
    };
    assert_eq!(
        bea.uid.as_deref(),
        Some("urn:uuid:7e2d4f7a-0000-4000-8000-000000000001")
    );
    assert_eq!(bea.full_name.as_deref(), Some("Bea Example"));
    let name = bea.name.as_ref().unwrap();
    assert_eq!(
        (name.given.as_deref(), name.family.as_deref()),
        (Some("Bea"), Some("Example"))
    );
    assert_eq!(bea.nicknames, vec!["Bee".to_owned()]);
    assert_eq!(bea.emails[0].normalized.as_deref(), Some("bea@example.com"));
    assert_eq!(bea.phones[0].e164.as_deref(), Some("+15550123"));
    assert_eq!(bea.organization.as_deref(), Some("Harbour Lights"));
    assert_eq!(bea.title.as_deref(), Some("Keeper"));
    assert_eq!(bea.birthday.as_deref(), Some("1988-07-04"));
    assert_eq!(bea.note.as_deref(), Some("Likes lamps."));
    assert_eq!(bea.addresses.len(), 1);
    assert_eq!(bea.addresses[0].locality.as_deref(), Some("Port Example"));
}

#[test]
fn reads_a_jscalendar_event_with_overrides() {
    let object = calendar(Format::JsCalendar, &fixture("jscalendar.json"));
    let master = &object.components[0];
    assert_eq!(master.uid.as_deref(), Some("review-1@example.com"));
    assert_eq!(master.summary.as_deref(), Some("Lamp review"));
    assert_eq!(master.location.as_deref(), Some("Room 4"));
    let start = master.start.as_ref().unwrap();
    assert_eq!(start.value, "2025-03-10T09:00:00");
    assert_eq!(start.time_zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(start.utc, Some(1_741_593_600_000));
    assert_eq!(
        master.end.as_ref().unwrap().value,
        "2025-03-10T10:00:00",
        "start plus duration"
    );
    assert_eq!(master.sequence, Some(2));
    assert!(master.rrule.as_deref().unwrap().contains("FREQ=WEEKLY"));
    assert!(master.rrule.as_deref().unwrap().contains("COUNT=4"));
    assert_eq!(master.exdates.len(), 1);
    assert_eq!(master.exdates[0].value, "2025-03-24T09:00:00");
    assert_eq!(
        master.organizer.as_ref().unwrap().email.as_deref(),
        Some("ada@example.com")
    );
    let bob = master
        .attendees
        .iter()
        .find(|a| a.email.as_deref() == Some("bob@example.org"))
        .expect("Bob attends");
    assert_eq!(bob.partstat.as_deref(), Some("TENTATIVE"));
    assert_eq!(object.components.len(), 2);
    let moved = &object.components[1];
    assert_eq!(
        moved.recurrence_id.as_ref().unwrap().value,
        "2025-03-17T09:00:00"
    );
    assert_eq!(moved.start.as_ref().unwrap().value, "2025-03-18T14:00:00");
    assert_eq!(moved.summary.as_deref(), Some("Lamp review (moved)"));
}

#[test]
fn wrong_formats_are_unreadable() {
    assert_eq!(
        normalize_as(Format::GraphContact, ItemKind::Event, b"{}"),
        Normalized::Unreadable
    );
    assert_eq!(
        normalize_as(Format::GraphEvent, ItemKind::Event, b"not json"),
        Normalized::Unreadable
    );
    assert_eq!(
        normalize_as(Format::JsCalendar, ItemKind::Event, b"[1, 2]"),
        Normalized::Unreadable
    );
}

#[test]
fn reads_a_jscalendar_2_all_day_series() {
    let object = calendar(Format::JsCalendar, &fixture("jscalendar-bis.json"));
    let event = &object.components[0];
    assert!(event.all_day);
    assert_eq!(event.start.as_ref().unwrap().value, "2025-06-02");
    assert_eq!(event.end.as_ref().unwrap().value, "2025-06-03");
    assert_eq!(event.rrule.as_deref(), Some("FREQ=MONTHLY;BYDAY=1MO"));
    assert_eq!(
        event.organizer.as_ref().unwrap().email.as_deref(),
        Some("ada@example.com")
    );
    assert_eq!(
        event
            .attendees
            .iter()
            .find(|a| a.email.as_deref() == Some("dan@example.org"))
            .and_then(|a| a.partstat.as_deref()),
        Some("DECLINED")
    );
}

#[test]
fn jscalendar_overrides_inherit_the_series() {
    let object = calendar(Format::JsCalendar, &fixture("jscalendar.json"));
    let moved = &object.components[1];
    assert_eq!(moved.location.as_deref(), Some("Room 4"));
    assert_eq!(moved.description.as_deref(), Some("Bring the samples."));
    assert_eq!(moved.attendees.len(), object.components[0].attendees.len());
    assert_eq!(moved.end.as_ref().unwrap().value, "2025-03-18T15:00:00");
}

type Instant = (Option<i64>, Option<i64>, Option<i64>, Vec<i64>);

fn instants(object: &CalendarObject) -> Vec<Instant> {
    object
        .components
        .iter()
        .map(|e| {
            (
                e.recurrence_id.as_ref().and_then(|t| t.utc),
                e.start.as_ref().and_then(|t| t.utc),
                e.end.as_ref().and_then(|t| t.utc),
                e.exdates.iter().filter_map(|t| t.utc).collect(),
            )
        })
        .collect()
}

#[test]
fn json_items_write_as_text_that_reads_the_same() {
    for (format, name) in [
        (Format::GraphContact, "graph-contact.json"),
        (Format::JsContact, "jscontact.json"),
    ] {
        let raw = fixture(name);
        let text = as_text(format, ItemKind::Contact, &raw).unwrap();
        assert!(text.starts_with(b"BEGIN:VCARD\r\n"), "{name}");
        let (before, after) = (contact(format, &raw), contact(Format::VCard, &text));
        assert_eq!(after.full_name, before.full_name, "{name}");
        assert_eq!(after.emails, before.emails, "{name}");
        assert_eq!(after.phones, before.phones, "{name}");
        assert_eq!(after.organization, before.organization, "{name}");
        assert_eq!(after.birthday, before.birthday, "{name}");
        assert_eq!(after.addresses, before.addresses, "{name}");
    }
    let graph = String::from_utf8(
        as_text(
            Format::GraphContact,
            ItemKind::Contact,
            &fixture("graph-contact.json"),
        )
        .unwrap()
        .into_owned(),
    )
    .unwrap();
    assert!(graph.contains("\r\nUID:"), "{graph}");

    for (format, name) in [
        (Format::GraphEvent, "graph-event.json"),
        (Format::GraphEvent, "graph-allday.json"),
        (Format::JsCalendar, "jscalendar.json"),
        (Format::JsCalendar, "jscalendar-bis.json"),
    ] {
        let raw = fixture(name);
        let text = as_text(format, ItemKind::Event, &raw).unwrap();
        assert!(text.starts_with(b"BEGIN:VCALENDAR\r\n"), "{name}");
        let (before, after) = (calendar(format, &raw), calendar(Format::ICalendar, &text));
        assert_eq!(instants(&after), instants(&before), "{name}");
        let facts = |o: &CalendarObject| {
            o.components
                .iter()
                .map(|e| {
                    (
                        e.uid.clone(),
                        e.summary.clone(),
                        e.rrule.as_ref().map(|r| {
                            let mut parts: Vec<&str> = r.split(';').collect();
                            parts.sort();
                            parts.join(";")
                        }),
                        e.all_day,
                        e.status.clone(),
                        e.attendees.len(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(facts(&after), facts(&before), "{name}");
    }

    let card = b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\nEND:VCARD\r\n";
    assert_eq!(
        as_text(Format::VCard, ItemKind::Contact, card)
            .unwrap()
            .as_ref(),
        card
    );
    assert!(as_text(Format::VCard, ItemKind::Event, card).is_none());
    assert!(as_text(Format::GraphEvent, ItemKind::Event, b"not json").is_none());
}
