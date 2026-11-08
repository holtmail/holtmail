//! The event and contact forms of the web app.

use holtmail_collection::Field;
use holtmail_pim::{ItemKind, NameParts, Normalized, PostalAddress, normalize};
use holtmail_pim_edit::{
    Alarm, Class, ContactForm, EventForm, FormError, Labeled, Person, build_contact, build_event,
    classify, contact_form, event_form, validate,
};

const NOW: i64 = 1_793_520_000_000;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn new_event() -> EventForm {
    EventForm {
        uid: Some("form-event-1@example.com".into()),
        title: "Planning, part 2".into(),
        start: "2026-12-03T09:30".into(),
        end: Some("2026-12-03T10:15".into()),
        time_zone: Some("Europe/Berlin".into()),
        recurrence: Some("FREQ=WEEKLY;BYDAY=TH;COUNT=4".into()),
        location: Some("Room 7".into()),
        description: Some("Agenda:\nbudget; hiring".into()),
        organizer: Some(Person {
            email: "ada@example.com".into(),
            name: Some("Ada Example".into()),
            partstat: None,
        }),
        attendees: vec![Person {
            email: "bob@example.org".into(),
            name: Some("Bob Example".into()),
            partstat: None,
        }],
        alarms: vec![
            Alarm { minutes_before: 10 },
            Alarm {
                minutes_before: 1440,
            },
        ],
        ..EventForm::default()
    }
}

#[test]
fn writes_a_new_event_from_the_form() {
    let raw = build_event(ItemKind::Event, &new_event(), None, NOW).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap().replace("\r\n ", "");
    assert!(
        text.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin"),
        "{text}"
    );
    assert!(text.contains("DTSTART;TZID=Europe/Berlin:20261203T093000"));
    assert!(text.contains("SUMMARY:Planning\\, part 2"));
    assert!(text.contains("TRIGGER:-PT10M"));
    assert!(text.contains("TRIGGER:-P1D"));
    assert!(text.contains("PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.org"));
    assert_eq!(
        validate(ItemKind::Event, &raw, None).unwrap().uid,
        "form-event-1@example.com"
    );
    let back = event_form(ItemKind::Event, &raw).unwrap();
    assert_eq!(back.title, "Planning, part 2");
    assert_eq!(back.start, "2026-12-03T09:30:00");
    assert_eq!(back.end.as_deref(), Some("2026-12-03T10:15:00"));
    assert_eq!(back.time_zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(
        back.recurrence.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=TH;COUNT=4")
    );
    assert_eq!(back.description.as_deref(), Some("Agenda:\nbudget; hiring"));
    assert_eq!(back.alarms, new_event().alarms);
    assert_eq!(back.attendees[0].email, "bob@example.org");
    assert_eq!(back.attendees[0].partstat.as_deref(), Some("NEEDS-ACTION"));
    let again = build_event(ItemKind::Event, &back, Some(&raw), NOW + 1000).unwrap();
    assert_eq!(
        classify(ItemKind::Event, Some(&raw), &again, &[])
            .unwrap()
            .class,
        Class::Noop
    );
}

#[test]
fn writes_all_day_and_utc_times() {
    let form = EventForm {
        uid: Some("holiday@example.com".into()),
        title: "Holiday".into(),
        start: "2026-12-24".into(),
        all_day: true,
        time_zone: Some("Europe/Berlin".into()),
        ..EventForm::default()
    };
    let raw = build_event(ItemKind::Event, &form, None, NOW).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("DTSTART;VALUE=DATE:20261224"));
    assert!(text.contains("DTEND;VALUE=DATE:20261225"));
    assert!(!text.contains("VTIMEZONE"));
    let back = event_form(ItemKind::Event, &raw).unwrap();
    assert!(back.all_day);
    assert_eq!(back.time_zone, None);

    let utc = EventForm {
        uid: Some("call@example.com".into()),
        start: "2026-12-01T15:00".into(),
        time_zone: Some("UTC".into()),
        ..EventForm::default()
    };
    let text = String::from_utf8(build_event(ItemKind::Event, &utc, None, NOW).unwrap()).unwrap();
    assert!(text.contains("DTSTART:20261201T150000Z"));
    assert!(text.contains("DTEND:20261201T160000Z"));
    assert!(!text.contains("SUMMARY"));
}

#[test]
fn refuses_forms_that_cannot_be_events() {
    let mut form = new_event();
    form.end = Some("2026-12-03T09:00".into());
    assert_eq!(
        build_event(ItemKind::Event, &form, None, NOW),
        Err(FormError::EndBeforeStart)
    );
    let mut form = new_event();
    form.time_zone = Some("Mars/Olympus".into());
    assert_eq!(
        build_event(ItemKind::Event, &form, None, NOW),
        Err(FormError::UnknownZone)
    );
    let mut form = new_event();
    form.recurrence = Some("WEEKLY\r\nX-INJECTED:1".into());
    assert_eq!(
        build_event(ItemKind::Event, &form, None, NOW),
        Err(FormError::BadRule)
    );
    let mut form = new_event();
    form.start = "tomorrow".into();
    assert_eq!(
        build_event(ItemKind::Event, &form, None, NOW),
        Err(FormError::BadTime("start"))
    );
    let mut form = new_event();
    form.attendees[0].email = "not an address".into();
    assert_eq!(
        build_event(ItemKind::Event, &form, None, NOW),
        Err(FormError::BadAddress)
    );
    let mut form = new_event();
    form.uid = None;
    assert_eq!(
        build_event(ItemKind::Event, &form, None, NOW),
        Err(FormError::MissingUid)
    );
}

#[test]
fn editing_a_base_rewrites_only_what_the_form_changed() {
    let base = fixture("event-apple-accept.ics");
    let mut form = event_form(ItemKind::Event, &base).unwrap();
    assert_eq!(form.alarms, vec![Alarm { minutes_before: 15 }]);
    form.title = "Weekly sync (remote)".into();
    let raw = build_event(ItemKind::Event, &form, Some(&base), NOW).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("X-WR-ALARMUID:5B1E2C40"));
    assert!(text.contains("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC"));
    assert!(text.contains("SEQUENCE:0"));
    let edit = classify(ItemKind::Event, Some(&base), &raw, &[]).unwrap();
    assert_eq!(edit.class, Class::Edit);
    assert_eq!(edit.fields, vec![Field::Summary]);

    form.start = "2026-12-02T11:00:00".into();
    form.end = Some("2026-12-02T11:30:00".into());
    form.attendees[0].partstat = Some("tentative".into());
    form.alarms = vec![Alarm { minutes_before: 15 }, Alarm { minutes_before: 60 }];
    let raw = build_event(ItemKind::Event, &form, Some(&base), NOW).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("SEQUENCE:1"));
    assert!(text.contains("X-WR-ALARMUID:5B1E2C40"));
    assert!(text.contains("TRIGGER:-PT60M"));
    let Normalized::Calendar(object) = normalize(ItemKind::Event, &raw) else {
        panic!("unreadable");
    };
    let event = &object.components[0];
    assert_eq!(event.start.as_ref().unwrap().value, "2026-12-02T11:00:00");
    assert_eq!(event.attendees[0].partstat.as_deref(), Some("TENTATIVE"));
    assert_eq!(event.attendees[0].role.as_deref(), Some("REQ-PARTICIPANT"));
    let own = vec!["ada@example.com".to_owned()];
    let fields = classify(ItemKind::Event, Some(&base), &raw, &own)
        .unwrap()
        .fields;
    assert!(fields.contains(&Field::Start) && fields.contains(&Field::Alarms));
}

#[test]
fn writes_a_new_contact_from_the_form() {
    let form = ContactForm {
        uid: Some("form-contact-1".into()),
        name: NameParts {
            given: Some("Grace".into()),
            family: Some("Example".into()),
            ..NameParts::default()
        },
        emails: vec![Labeled {
            value: "grace@example.org".into(),
            kinds: vec!["Work".into()],
        }],
        phones: vec![Labeled {
            value: "+1 555 0142".into(),
            kinds: vec!["cell".into()],
        }],
        addresses: vec![PostalAddress {
            kinds: vec!["home".into()],
            street: Some("1 Example Road".into()),
            locality: Some("Springfield".into()),
            country: Some("Exampleland".into()),
            ..PostalAddress::default()
        }],
        organization: Some("Example Labs".into()),
        notes: Some("Line one\nLine two".into()),
        ..ContactForm::default()
    };
    let raw = build_contact(&form, None, NOW).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("VERSION:3.0"));
    assert!(text.contains("FN:Grace Example"));
    assert!(text.contains("EMAIL;TYPE=work:grace@example.org"));
    assert!(text.contains("ADR;TYPE=home:;;1 Example Road;Springfield;;;Exampleland"));
    assert!(validate(ItemKind::Contact, &raw, None).is_ok());
    let back = contact_form(&raw).unwrap();
    assert_eq!(back.full_name.as_deref(), Some("Grace Example"));
    assert_eq!(back.emails[0].kinds, vec!["work"]);
    assert_eq!(back.notes.as_deref(), Some("Line one\nLine two"));
    assert_eq!(back.addresses, form.addresses);
    assert_eq!(
        build_contact(&ContactForm::default(), None, NOW),
        Err(FormError::Empty)
    );
}

#[test]
fn editing_a_contact_keeps_apple_labels() {
    let base = fixture("contact-base.vcf");
    let mut form = contact_form(&base).unwrap();
    assert_eq!(
        form.organization.as_deref(),
        Some("Example Analytical Engines, Research")
    );
    form.phones.push(Labeled {
        value: "+1 555 0177".into(),
        kinds: vec!["work".into()],
    });
    let raw = build_contact(&form, Some(&base), NOW).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("item1.EMAIL;type=INTERNET;type=pref:ada@example.com"));
    assert!(text.contains("item1.X-ABLabel"));
    assert!(text.contains("ORG:Example Analytical Engines;Research"));
    let edit = classify(ItemKind::Contact, Some(&base), &raw, &[]).unwrap();
    assert_eq!(edit.class, Class::Edit);
    assert_eq!(edit.fields, vec![Field::Phones]);

    form.emails.remove(0);
    let raw = build_contact(&form, Some(&base), NOW).unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert!(!text.contains("item1."), "{text}");
    assert!(text.contains("ada.example@example.org"));
}
