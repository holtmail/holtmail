//! Reading the hand-written fixtures.

use holtmail_blob::BlobId;
use holtmail_pim::*;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn contact(raw: &[u8]) -> Contact {
    match normalize(ItemKind::Contact, raw) {
        Normalized::Contact(contact) => *contact,
        other => panic!("not a contact: {other:?}"),
    }
}

fn calendar(raw: &[u8]) -> CalendarObject {
    match normalize(ItemKind::Event, raw) {
        Normalized::Calendar(calendar) => calendar,
        other => panic!("not a calendar: {other:?}"),
    }
}

#[test]
fn reads_vcard_3_with_folded_lines_and_a_photo() {
    let file = fixture("contacts-v3.vcf");
    let items = split(&file);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].uid.as_deref(), Some("card-ada@example.com"));
    let read = read(ItemKind::Contact, &items[0].raw);
    let Normalized::Contact(ada) = &read.normalized else {
        panic!("not a contact");
    };
    assert_eq!(ada.full_name.as_deref(), Some("Ada Quill"));
    assert_eq!(ada.name.as_ref().unwrap().formatted(), "Dr. Ada Quill");
    assert_eq!(
        ada.organization.as_deref(),
        Some("Example Instruments, Research")
    );
    assert_eq!(ada.title.as_deref(), Some("Analyst"));
    assert_eq!(ada.emails[0].address, "Ada.Quill@Example.COM");
    assert_eq!(
        ada.emails[0].normalized.as_deref(),
        Some("ada.quill@example.com")
    );
    assert_eq!(ada.emails[0].kinds, ["work"]);
    assert_eq!(ada.phones[0].e164.as_deref(), Some("+15550100"));
    assert_eq!(ada.phones[0].kinds, ["cell"]);
    assert_eq!(ada.phones[1].e164, None, "no country code, no guess");
    assert_eq!(ada.addresses[0].street.as_deref(), Some("12 Harbour Lane"));
    assert_eq!(ada.addresses[0].country.as_deref(), Some("United States"));
    assert_eq!(ada.birthday.as_deref(), Some("1985-04-12"));
    assert!(
        ada.note
            .as_deref()
            .unwrap()
            .contains("folded over two lines"),
        "folded note unfolded: {:?}",
        ada.note
    );
    assert_eq!(ada.rev.as_deref(), Some("2025-01-15T10:00:00Z"));
    let photo = read.photo.expect("photo bytes");
    assert!(photo.bytes.starts_with(b"\x89PNG"));
    assert_eq!(photo.content_type.as_deref(), Some("image/png"));
    assert_eq!(
        ada.photo,
        Some(Photo::Embedded {
            blob: BlobId::of(&photo.bytes),
            content_type: Some("image/png".into()),
            size: photo.bytes.len() as u64,
        })
    );
    assert_eq!(
        read.normalized.contact_keys(),
        [
            "email:ada.quill@example.com",
            "email:ada@example.org",
            "tel:+15550100"
        ]
    );
}

#[test]
fn reads_vcard_4_with_a_data_uri_and_a_yearless_birthday() {
    let cleo = contact(&fixture("contact-v4.vcf"));
    assert_eq!(cleo.full_name.as_deref(), Some("Cleo Marsh"));
    assert_eq!(cleo.birthday.as_deref(), Some("--12-10"));
    assert_eq!(cleo.phones[0].e164.as_deref(), Some("+15550102"));
    assert!(matches!(
        cleo.photo,
        Some(Photo::Embedded { ref content_type, .. }) if content_type.as_deref() == Some("image/png")
    ));
}

#[test]
fn reads_vcard_2_1_quoted_printable() {
    let juergen = contact(&fixture("contact-v21.vcf"));
    assert_eq!(juergen.full_name.as_deref(), Some("Jürgen Müller"));
    assert_eq!(
        juergen.name.as_ref().unwrap().family.as_deref(),
        Some("Müller")
    );
    assert_eq!(juergen.emails[0].kinds, Vec::<String>::new());
}

#[test]
fn reads_events_with_zones_all_day_overrides_and_tasks() {
    let file = fixture("calendar.ics");
    assert_eq!(calendar_name(&file).as_deref(), Some("Team"));
    let items = split(&file);
    let kinds: Vec<_> = items.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, [ItemKind::Event, ItemKind::Event, ItemKind::Task]);

    let standup = calendar(&items[0].raw);
    assert_eq!(standup.components.len(), 2, "event and its moved instance");
    let main = standup.main().unwrap();
    assert_eq!(main.summary.as_deref(), Some("Weekly standup"));
    assert!(main.recurring);
    let start = main.start.as_ref().unwrap();
    assert_eq!(start.value, "2025-03-10T09:00:00");
    assert_eq!(start.time_zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(start.utc, Some(1_741_593_600_000), "08:00 UTC in winter");
    assert_eq!(main.end.as_ref().unwrap().value, "2025-03-10T09:15:00");
    assert_eq!(
        main.organizer.as_ref().unwrap().email.as_deref(),
        Some("ada.quill@example.com")
    );
    assert_eq!(main.attendees[0].email.as_deref(), Some("bo@example.org"));
    assert_eq!(main.attendees[0].partstat.as_deref(), Some("ACCEPTED"));
    assert_eq!(main.attendees[0].name.as_deref(), Some("Bo Linden"));
    assert_eq!(main.sequence, Some(1));
    assert_eq!(main.status.as_deref(), Some("CONFIRMED"));
    assert_eq!(main.dtstamp, Some(1_735_732_800_000));

    let moved = &standup.components[1];
    assert_eq!(moved.start.as_ref().unwrap().value, "2025-03-17T10:00:00");
    assert_eq!(
        moved.end.as_ref().unwrap().value,
        "2025-03-17T10:30:00",
        "end from DURATION"
    );
    let keys = Normalized::Calendar(standup.clone()).event_keys();
    assert_eq!(
        keys,
        [
            EventKey {
                uid: "standup-1@example.com".into(),
                recurrence_id: None
            },
            EventKey {
                uid: "standup-1@example.com".into(),
                recurrence_id: Some("2025-03-17T08:00:00Z".into())
            }
        ]
    );

    let holiday = calendar(&items[1].raw);
    let day = holiday.main().unwrap();
    assert!(day.all_day);
    assert_eq!(day.start.as_ref().unwrap().value, "2025-05-01");
    assert_eq!(day.start.as_ref().unwrap().utc, Some(1_746_057_600_000));
    assert!(!String::from_utf8_lossy(&items[1].raw).contains("VTIMEZONE"));

    let todo = calendar(&items[2].raw);
    let task = todo.main().unwrap();
    assert_eq!(task.kind, ItemKind::Task);
    assert_eq!(task.end.as_ref().unwrap().time_zone.as_deref(), Some("UTC"));
}

#[test]
fn summer_time_moves_the_utc_offset() {
    let raw = b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:summer@example.com\nDTSTART;TZID=Europe/Berlin:20250707T090000\nEND:VEVENT\nEND:VCALENDAR\n";
    let event = calendar(raw);
    assert_eq!(
        event.main().unwrap().start.as_ref().unwrap().utc,
        Some(1_751_871_600_000),
        "07:00 UTC in summer"
    );
}

#[test]
fn bytes_that_do_not_parse_are_unreadable() {
    assert_eq!(
        normalize(ItemKind::Contact, b"FN:no card here"),
        Normalized::Unreadable
    );
    assert!(
        matches!(
            normalize(ItemKind::Contact, b"BEGIN:VCARD\nFN:Cut off"),
            Normalized::Contact(_)
        ),
        "a card cut off at the end is read as far as it goes"
    );
    assert_eq!(
        normalize(ItemKind::Event, b"not a calendar at all"),
        Normalized::Unreadable
    );
}

#[test]
fn diffs_contacts_field_by_field() {
    let before = normalize(
        ItemKind::Contact,
        b"BEGIN:VCARD\nVERSION:3.0\nFN:Ada Quill\nEMAIL:ada@example.com\nEMAIL:old@example.com\nTEL:+1 555 0100\nEND:VCARD\n",
    );
    let after = normalize(
        ItemKind::Contact,
        b"BEGIN:VCARD\nVERSION:3.0\nFN:Ada Quill-Linden\nEMAIL:new@example.com\nEMAIL:ada@example.com\nTEL:+1 555 0100\nTITLE:Analyst\nEND:VCARD\n",
    );
    let change = |field: &str, before: Option<&str>, after: Option<&str>| Change {
        field: field.into(),
        before: before.map(Into::into),
        after: after.map(Into::into),
    };
    assert_eq!(
        diff(&before, &after),
        [
            change("full_name", Some("Ada Quill"), Some("Ada Quill-Linden")),
            change("email", Some("old@example.com"), None),
            change("email", None, Some("new@example.com")),
            change("title", None, Some("Analyst")),
        ]
    );
    assert!(diff(&after, &after).is_empty());
}

#[test]
fn diffs_events_and_their_instances() {
    let before = calendar(&split(&fixture("calendar.ics"))[0].raw);
    let mut after = before.clone();
    after.components[0].start.as_mut().unwrap().value = "2025-03-10T10:00:00".into();
    after.components[0].attendees[1].partstat = Some("ACCEPTED".into());
    after.components[1].summary = Some("Standup, later".into());
    after.components[0].dtstamp = Some(0);
    let changes = diff(&Normalized::Calendar(before), &Normalized::Calendar(after));
    let fields: Vec<_> = changes.iter().map(|c| c.field.as_str()).collect();
    assert_eq!(
        fields,
        [
            "start",
            "attendee",
            "attendee",
            "instance 2025-03-17T09:00:00 Europe/Berlin summary"
        ]
    );
    assert_eq!(
        changes[0].before.as_deref(),
        Some("2025-03-10T09:00:00 Europe/Berlin")
    );
    assert_eq!(
        changes[2].after.as_deref(),
        Some("cleo@example.com (accepted)")
    );
}

#[test]
fn unreadable_versions_diff_against_readable_ones() {
    let readable = normalize(
        ItemKind::Contact,
        b"BEGIN:VCARD\nVERSION:3.0\nFN:Ada\nEND:VCARD\n",
    );
    let changes = diff(&Normalized::Unreadable, &readable);
    assert_eq!(changes[0].field, "unreadable");
    assert_eq!(changes[1].field, "full_name");
}

#[test]
fn normalized_values_round_trip_through_json() {
    let value = normalize(ItemKind::Event, &fixture("calendar.ics"));
    let json = serde_json::to_string(&value).unwrap();
    let back: Normalized = serde_json::from_str(&json).unwrap();
    assert_eq!(back, value);
}

#[test]
fn keeps_recurrence_rules_exceptions_and_custom_zones() {
    let raw = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
BEGIN:VTIMEZONE\r\nTZID:Harbour Time\r\n\
BEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
END:VTIMEZONE\r\n\
BEGIN:VTIMEZONE\r\nTZID:W. Europe Standard Time\r\n\
BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:standup@example.com\r\nDTSTAMP:20250101T000000Z\r\nLAST-MODIFIED:20250102T080000Z\r\n\
DTSTART;TZID=Harbour Time:20250303T090000\r\nDTEND;TZID=Harbour Time:20250303T091500\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20250428T070000Z\r\n\
EXDATE;TZID=W. Europe Standard Time:20250317T090000,20250324T090000\r\n\
RDATE;VALUE=PERIOD:20250305T080000Z/PT1H\r\n\
SUMMARY:Standup\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup@example.com\r\nRECURRENCE-ID;RANGE=THISANDFUTURE;TZID=Harbour Time:20250407T090000\r\n\
DTSTART;TZID=Harbour Time:20250407T100000\r\nDTEND;TZID=Harbour Time:20250407T101500\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
    let calendar = calendar(raw);
    let main = calendar.main().unwrap();
    assert_eq!(
        main.rrule.as_deref(),
        Some("FREQ=WEEKLY;UNTIL=20250428T070000Z;BYDAY=MO")
    );
    assert_eq!(main.last_modified, Some(1_735_804_800_000));
    let exdates: Vec<&str> = main.exdates.iter().map(|t| t.value.as_str()).collect();
    assert_eq!(exdates, ["2025-03-17T09:00:00", "2025-03-24T09:00:00"]);
    assert_eq!(
        main.exdates[0].time_zone.as_deref(),
        Some("W. Europe Standard Time")
    );
    assert_eq!(main.rdates[0].value, "2025-03-05T08:00:00Z");
    assert!(calendar.components[1].this_and_future);
    assert!(!main.this_and_future);
    assert_eq!(calendar.zones.len(), 2);
    let harbour = &calendar.zones[0];
    assert_eq!(harbour.tzid, "Harbour Time");
    assert_eq!(harbour.iana, None);
    assert_eq!(harbour.observances.len(), 2);
    assert_eq!(harbour.observances[1].start, "1970-03-29T02:00:00");
    assert_eq!(harbour.observances[1].offset_to, 7200);
    assert!(harbour.observances[1].daylight);
    assert_eq!(
        harbour.observances[0].rrule.as_deref(),
        Some("FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10")
    );
    assert_eq!(calendar.zones[1].iana.as_deref(), Some("Europe/Berlin"));
    assert!(calendar.zones[1].observances.is_empty());
    assert_eq!(
        iana_zone("Eastern Standard Time").as_deref(),
        Some("America/New_York")
    );
    assert_eq!(iana_zone("Europe/Berlin").as_deref(), Some("Europe/Berlin"));
    assert_eq!(iana_zone("Harbour Time"), None);
}

#[test]
fn a_duration_past_the_end_of_time_leaves_the_end_out() {
    let raw = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:long@example.com\r\nDTSTART:20250101T000000Z\r\nDURATION:P99999999W\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let object = calendar(raw.as_bytes());
    assert!(object.main().unwrap().end.is_none());
}
