//! iMIP messages: golden REPLY, REQUEST and CANCEL objects and the organizer's side of a change.

use holtmail_collection::Partstat;
use holtmail_pim::{ItemKind, Method, Normalized, normalize};
use holtmail_pim_edit::{
    Audience, CancelScope, ImipError, ScheduleAgent, cancel, organize, organizer, reply,
    reply_agent, request,
};

const NOW: i64 = 1_795_253_400_000;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/imip/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn crlf(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
}

fn golden(name: &str, actual: &[u8]) {
    assert!(actual.ends_with(b"\r\n"));
    assert!(
        !String::from_utf8_lossy(actual)
            .replace("\r\n", "")
            .contains('\n')
    );
    assert_eq!(
        String::from_utf8_lossy(actual),
        crlf(&fixture(name)),
        "{name}"
    );
    assert!(matches!(
        normalize(ItemKind::Event, actual),
        Normalized::Calendar(_)
    ));
    for line in String::from_utf8_lossy(actual).split("\r\n") {
        assert!(line.len() <= 75, "{line}");
        assert!(!line.contains("SCHEDULE-"), "{line}");
    }
}

#[test]
fn now_is_the_fixture_time() {
    let at = chrono::DateTime::from_timestamp_millis(NOW).unwrap();
    assert_eq!(at.format("%Y%m%dT%H%M%SZ").to_string(), "20261121T093000Z");
}

#[test]
fn replies_to_the_series_as_the_alias() {
    let invitation = fixture("invitation.ics");
    let imip = reply(
        &invitation,
        "MAILTO:Ada.Alias@Example.com",
        Partstat::Accepted,
        None,
        Some("See you there; bring notes"),
        NOW,
    )
    .unwrap();
    golden("reply-series.ics", &imip.calendar);
    assert_eq!(imip.method, Method::Reply);
    assert_eq!(imip.uid, "weekly-sync@example.org");
    assert_eq!(imip.sequence, 2);
    assert_eq!(imip.recurrence_id, None);
    assert_eq!(imip.recipients, ["cleo@example.org"]);
    assert_eq!(imip.organizer, "cleo@example.org");
    assert_eq!(imip.summary.as_deref(), Some("Weekly sync"));
    assert_eq!(
        imip.when.as_deref(),
        Some("2026-12-01 10:00 (Europe/Berlin)")
    );
    assert_eq!(imip.location.as_deref(), Some("Room 4"));
    assert_eq!(reply_agent(&invitation), ScheduleAgent::Client);
    assert_eq!(organizer(&invitation).as_deref(), Some("cleo@example.org"));
}

#[test]
fn replies_to_a_changed_instance_with_its_sequence() {
    let imip = reply(
        &fixture("invitation.ics"),
        "ada.alias@example.com",
        Partstat::Declined,
        Some("20261215T100000"),
        None,
        NOW,
    )
    .unwrap();
    golden("reply-changed-instance.ics", &imip.calendar);
    assert_eq!(imip.sequence, 3);
    assert_eq!(imip.recurrence_id.as_deref(), Some("20261215T100000"));
    assert_eq!(
        imip.when.as_deref(),
        Some("2026-12-15 14:00 (Europe/Berlin)")
    );
}

#[test]
fn replies_to_an_instance_made_from_the_series() {
    let imip = reply(
        &fixture("invitation.ics"),
        "ada.alias@example.com",
        Partstat::Tentative,
        Some("20261208T100000"),
        None,
        NOW,
    )
    .unwrap();
    golden("reply-made-instance.ics", &imip.calendar);
    assert_eq!(imip.sequence, 2);
    assert_eq!(imip.recurrence_id.as_deref(), Some("20261208T100000"));
}

#[test]
fn reply_refuses_strangers_missing_organizers_and_bad_instances() {
    let invitation = fixture("invitation.ics");
    assert_eq!(
        reply(
            &invitation,
            "ada@example.com",
            Partstat::Accepted,
            None,
            None,
            NOW
        ),
        Err(ImipError::NotAttendee)
    );
    assert_eq!(
        reply(
            &invitation,
            "ada.alias@example.com",
            Partstat::Accepted,
            Some("2026\r\nX"),
            None,
            NOW
        ),
        Err(ImipError::NoInstance)
    );
    let single = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:one@example.com\r\nDTSTART:20261201T100000Z\r\nATTENDEE:mailto:ada@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert_eq!(
        reply(
            single,
            "ada@example.com",
            Partstat::Accepted,
            None,
            None,
            NOW
        ),
        Err(ImipError::NoOrganizer)
    );
    assert_eq!(
        reply(
            b"not ical",
            "ada@example.com",
            Partstat::Accepted,
            None,
            None,
            NOW
        ),
        Err(ImipError::Unreadable)
    );
    let with_organizer = String::from_utf8_lossy(single).replace(
        "ATTENDEE:",
        "ORGANIZER:mailto:cleo@example.org\r\nATTENDEE:",
    );
    assert_eq!(
        reply(
            with_organizer.as_bytes(),
            "ada@example.com",
            Partstat::Accepted,
            Some("20261208T100000Z"),
            None,
            NOW
        ),
        Err(ImipError::NoInstance)
    );
}

#[test]
fn all_day_instances_keep_value_date() {
    let raw = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:day@example.com\r\nDTSTART;VALUE=DATE:20261201\r\nRRULE:FREQ=DAILY;COUNT=3\r\nORGANIZER:mailto:cleo@example.org\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:ada@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let imip = reply(
        raw,
        "ada@example.com",
        Partstat::Accepted,
        Some("20261202"),
        None,
        NOW,
    )
    .unwrap();
    let text = String::from_utf8(imip.calendar).unwrap();
    assert!(text.contains("RECURRENCE-ID;VALUE=DATE:20261202\r\n"));
    assert!(text.contains("DTSTART;VALUE=DATE:20261202\r\n"));
    assert!(text.contains("SEQUENCE:0\r\n"));
    assert_eq!(imip.when.as_deref(), Some("2026-12-02"));
}

#[test]
fn requests_the_whole_event_without_personal_parts() {
    let imip = request(&fixture("organizer.ics"), Audience::Everyone, NOW).unwrap();
    golden("request.ics", &imip.calendar);
    assert_eq!(imip.method, Method::Request);
    assert_eq!(imip.recipients, ["bob@example.org", "dana@example.org"]);
    assert_eq!(imip.sequence, 0);
    assert_eq!(imip.when.as_deref(), Some("2026-12-03 15:00 UTC"));
}

#[test]
fn audiences_pick_attendees() {
    let raw = crlf(&fixture("organizer.ics"))
        .replace(
            "ATTENDEE;CN=Dana Reed;",
            "ATTENDEE;CN=Dana Reed;SCHEDULE-AGENT=CLIENT;",
        )
        .replace(
            "ATTENDEE;CN=Bob Stone;",
            "ATTENDEE;CN=Bob Stone;SCHEDULE-AGENT=NONE;",
        );
    let everyone = request(raw.as_bytes(), Audience::Everyone, NOW).unwrap();
    assert_eq!(everyone.recipients, ["dana@example.org"]);
    let client = request(raw.as_bytes(), Audience::ClientScheduled, NOW).unwrap();
    assert_eq!(client.recipients, ["dana@example.org"]);
    let plain = fixture("organizer.ics");
    assert_eq!(
        request(&plain, Audience::ClientScheduled, NOW),
        Err(ImipError::NoRecipients)
    );
    let only = ["BOB@example.org".to_owned(), "zoe@example.org".to_owned()];
    assert_eq!(
        request(&plain, Audience::Only(&only), NOW)
            .unwrap()
            .recipients,
        ["bob@example.org"]
    );
}

#[test]
fn cancels_one_instance_and_the_series() {
    let copy = fixture("organizer.ics");
    let imip = cancel(
        &copy,
        CancelScope::Instance("20261210T150000Z"),
        Audience::Everyone,
        NOW,
    )
    .unwrap();
    golden("cancel-instance.ics", &imip.calendar);
    assert_eq!(imip.method, Method::Cancel);
    assert_eq!(imip.sequence, 1);
    assert_eq!(imip.recurrence_id.as_deref(), Some("20261210T150000Z"));
    assert_eq!(imip.recipients, ["bob@example.org", "dana@example.org"]);

    let series = cancel(&copy, CancelScope::Series, Audience::Everyone, NOW).unwrap();
    let text = String::from_utf8(series.calendar).unwrap();
    assert!(text.contains("METHOD:CANCEL\r\n"));
    assert!(text.contains("SEQUENCE:1\r\n"));
    assert!(text.contains("STATUS:CANCELLED\r\n"));
    assert!(!text.contains("RRULE"));
    assert!(!text.contains("RECURRENCE-ID"));
    assert_eq!(series.recurrence_id, None);
}

#[test]
fn an_update_cancels_removed_attendees_and_requests_the_rest() {
    let previous = fixture("organizer.ics");
    let next = fixture("organizer-moved.ics");
    let organized = organize(Some(&previous), Some(&next), false, NOW).unwrap();
    assert_eq!(organized.messages.len(), 2);
    let [removed, update] = &organized.messages[..] else {
        unreachable!()
    };
    golden("cancel-removed.ics", &removed.calendar);
    assert_eq!(removed.recipients, ["dana@example.org"]);
    golden("request-moved.ics", &update.calendar);
    assert_eq!(update.recipients, ["bob@example.org", "erin@example.org"]);
    assert_eq!(update.sequence, 1);

    let copy = String::from_utf8(organized.event.unwrap()).unwrap();
    assert!(copy.contains("SEQUENCE:1\r\n"));
    assert!(copy.contains("BEGIN:VALARM"));
    assert!(copy.contains("ATTENDEE;CN=Ada Quill;PARTSTAT=ACCEPTED:mailto:ada@example.com"));
    assert!(copy.contains("SCHEDULE-STATUS=1.2;PARTSTAT=NEEDS-ACTION;RSVP=TRUE"));

    let again = organize(Some(&previous), Some(copy.as_bytes()), false, NOW).unwrap();
    assert_eq!(again.event.as_deref(), Some(copy.as_bytes()));
}

#[test]
fn small_updates_send_only_what_they_must() {
    let previous = crlf(&fixture("organizer.ics"));

    let answered = previous.replace(
        "Dana Reed;PARTSTAT=NEEDS-ACTION;RSVP=TRUE",
        "Dana Reed;PARTSTAT=ACCEPTED",
    );
    let organized = organize(
        Some(previous.as_bytes()),
        Some(answered.as_bytes()),
        false,
        NOW,
    )
    .unwrap();
    assert!(organized.messages.is_empty());
    assert_eq!(organized.event.as_deref(), Some(answered.as_bytes()));

    let alarm = previous.replace("TRIGGER:-PT10M", "TRIGGER:-PT30M");
    assert!(
        organize(
            Some(previous.as_bytes()),
            Some(alarm.as_bytes()),
            false,
            NOW
        )
        .unwrap()
        .messages
        .is_empty()
    );

    let added = previous.replace(
        "X-MOZ-LASTACK",
        "ATTENDEE;CN=Erin Moss;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:erin@example.org\r\nX-MOZ-LASTACK",
    );
    let organized = organize(
        Some(previous.as_bytes()),
        Some(added.as_bytes()),
        false,
        NOW,
    )
    .unwrap();
    assert_eq!(organized.messages.len(), 1);
    assert_eq!(organized.messages[0].method, Method::Request);
    assert_eq!(organized.messages[0].recipients, ["erin@example.org"]);
    assert_eq!(organized.messages[0].sequence, 0);

    let skipped = previous.replace(
        "RRULE:FREQ=WEEKLY;COUNT=4\r\n",
        "RRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE:20261217T150000Z\r\n",
    );
    let organized = organize(
        Some(previous.as_bytes()),
        Some(skipped.as_bytes()),
        false,
        NOW,
    )
    .unwrap();
    assert_eq!(organized.messages.len(), 1);
    let instance = &organized.messages[0];
    assert_eq!(instance.method, Method::Cancel);
    assert_eq!(instance.recurrence_id.as_deref(), Some("20261217T150000Z"));
    assert_eq!(instance.sequence, 1);
    let copy = String::from_utf8(organized.event.unwrap()).unwrap();
    assert!(copy.contains("SEQUENCE:1\r\n"));
    assert!(copy.contains("Bob Stone;PARTSTAT=ACCEPTED"));

    let retitled = previous.replace("SUMMARY:Design review", "SUMMARY:Design review, final");
    let organized = organize(
        Some(previous.as_bytes()),
        Some(retitled.as_bytes()),
        false,
        NOW,
    )
    .unwrap();
    assert_eq!(organized.messages.len(), 1);
    assert_eq!(organized.messages[0].sequence, 0);
    assert_eq!(organized.event.as_deref(), Some(retitled.as_bytes()));
}

#[test]
fn creates_deletes_and_server_scheduled_changes() {
    let copy = fixture("organizer.ics");
    let created = organize(None, Some(&copy), false, NOW).unwrap();
    assert_eq!(created.messages.len(), 1);
    assert_eq!(created.messages[0].method, Method::Request);
    assert_eq!(created.event.as_deref(), Some(&copy[..]));

    let deleted = organize(Some(&copy), None, false, NOW).unwrap();
    assert_eq!(deleted.event, None);
    assert_eq!(deleted.messages.len(), 1);
    assert_eq!(deleted.messages[0].method, Method::Cancel);
    assert_eq!(
        deleted.messages[0].recipients,
        ["bob@example.org", "dana@example.org"]
    );

    assert!(
        organize(None, Some(&copy), true, NOW)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        organize(Some(&copy), None, true, NOW)
            .unwrap()
            .messages
            .is_empty()
    );
    let client = crlf(&copy).replace(
        "ATTENDEE;CN=Dana Reed;",
        "ATTENDEE;CN=Dana Reed;SCHEDULE-AGENT=CLIENT;",
    );
    let created = organize(None, Some(client.as_bytes()), true, NOW).unwrap();
    assert_eq!(created.messages[0].recipients, ["dana@example.org"]);

    let solo = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:solo@example.com\r\nDTSTART:20261201T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let alone = organize(None, Some(solo), false, NOW).unwrap();
    assert!(alone.messages.is_empty());
    assert_eq!(
        organize(None, Some(b"nope"), false, NOW),
        Err(ImipError::Unreadable)
    );
}

#[test]
fn removing_an_attendee_raises_the_stored_copy_with_the_cancel() {
    let previous = crlf(&fixture("organizer.ics"));
    let removed = previous.replace(
        "ATTENDEE;CN=Dana Reed;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:dana@example.org\r\n",
        "",
    );
    let organized = organize(
        Some(previous.as_bytes()),
        Some(removed.as_bytes()),
        false,
        NOW,
    )
    .unwrap();
    assert_eq!(organized.messages.len(), 1);
    assert_eq!(organized.messages[0].method, Method::Cancel);
    assert_eq!(organized.messages[0].sequence, 1);
    let stored = String::from_utf8(organized.event.unwrap()).unwrap();
    assert!(stored.contains("SEQUENCE:1\r\n"), "{stored}");
}
