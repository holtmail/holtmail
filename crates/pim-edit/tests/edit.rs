//! Classification, validation, rebase and diffs over client-shaped fixtures.

use holtmail_collection::Field;
use holtmail_pim::{ItemKind, Normalized, normalize};
use holtmail_pim_edit::{
    Class, Invalid, Patch, Rebased, classify, field_diff, item_patch, rebase, serve, validate,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn ada() -> Vec<String> {
    vec!["MAILTO:Ada@Example.com".to_owned()]
}

fn main_event(raw: &[u8]) -> holtmail_pim::Event {
    let Normalized::Calendar(object) = normalize(ItemKind::Event, raw) else {
        panic!("unreadable: {}", String::from_utf8_lossy(raw));
    };
    object
        .components
        .into_iter()
        .find(|e| e.recurrence_id.is_none())
        .unwrap()
}

fn partstat(event: &holtmail_pim::Event, email: &str) -> Option<String> {
    event
        .attendees
        .iter()
        .find(|a| a.email.as_deref() == Some(email))
        .and_then(|a| a.partstat.clone())
}

fn classified(kind: ItemKind, base: &str, mine: &str) -> holtmail_pim_edit::Classified {
    let base = fixture(base);
    classify(kind, Some(&base), &fixture(mine), &ada()).unwrap()
}

#[test]
fn classifies_what_clients_save() {
    let cases = [
        (ItemKind::Event, "event-thunderbird-resave.ics", Class::Noop),
        (
            ItemKind::Event,
            "event-thunderbird-snooze.ics",
            Class::Personal,
        ),
        (ItemKind::Event, "event-apple-accept.ics", Class::Reply),
        (
            ItemKind::Event,
            "event-apple-decline-instance.ics",
            Class::Reply,
        ),
        (ItemKind::Event, "event-davx5-edit.ics", Class::Edit),
        (ItemKind::Contact, "contact-apple-resave.vcf", Class::Noop),
        (ItemKind::Contact, "contact-apple-edit.vcf", Class::Edit),
        (ItemKind::Contact, "contact-davx5-edit.vcf", Class::Edit),
    ];
    for (kind, mine, class) in cases {
        let base = if kind == ItemKind::Contact {
            "contact-base.vcf"
        } else {
            "event-base.ics"
        };
        assert_eq!(classified(kind, base, mine).class, class, "{mine}");
    }
    let base = fixture("event-base.ics");
    assert_eq!(
        classify(ItemKind::Event, None, &base, &ada())
            .unwrap()
            .class,
        Class::Create
    );
}

#[test]
fn an_answer_for_someone_else_is_an_edit() {
    let base = fixture("event-base.ics");
    let mine = fixture("event-apple-accept.ics");
    let carol = vec!["carol@example.org".to_owned()];
    assert_eq!(
        classify(ItemKind::Event, Some(&base), &mine, &carol)
            .unwrap()
            .class,
        Class::Edit
    );
    assert_eq!(
        classify(ItemKind::Event, Some(&base), &mine, &[])
            .unwrap()
            .class,
        Class::Edit
    );
}

#[test]
fn lists_the_fields_a_change_touches() {
    let edit = classified(ItemKind::Event, "event-base.ics", "event-davx5-edit.ics");
    assert_eq!(edit.fields, vec![Field::End, Field::Start, Field::Summary]);
    let reply = classified(ItemKind::Event, "event-base.ics", "event-apple-accept.ics");
    assert!(reply.fields.contains(&Field::Attendees));
    assert!(reply.fields.contains(&Field::Alarms));
    let instance = classified(
        ItemKind::Event,
        "event-base.ics",
        "event-apple-decline-instance.ics",
    );
    assert!(instance.fields.contains(&Field::Recurrence));
    let noop = classified(
        ItemKind::Event,
        "event-base.ics",
        "event-thunderbird-resave.ics",
    );
    assert!(noop.fields.is_empty());
    assert!(noop.patch.is_empty());
    let phone = classified(
        ItemKind::Contact,
        "contact-base.vcf",
        "contact-apple-edit.vcf",
    );
    assert!(phone.fields.contains(&Field::Phones));
    assert!(!phone.fields.contains(&Field::Emails));
}

#[test]
fn validates_what_a_client_sends() {
    let event = fixture("event-base.ics");
    assert_eq!(
        validate(ItemKind::Event, &event, None).unwrap().uid,
        "weekly-sync-7f3a@example.org"
    );
    assert_eq!(
        validate(ItemKind::Task, &event, None),
        Err(Invalid::WrongComponent)
    );
    assert_eq!(
        validate(ItemKind::Contact, &event, None),
        Err(Invalid::Unreadable)
    );
    let text = String::from_utf8(event.clone()).unwrap();
    let renamed = text.replace("weekly-sync-7f3a", "other");
    assert_eq!(
        validate(ItemKind::Event, renamed.as_bytes(), Some(&event)),
        Err(Invalid::UidChanged)
    );
    let no_uid = text.replace("UID:weekly-sync-7f3a@example.org\r\n", "");
    assert_eq!(
        validate(ItemKind::Event, no_uid.as_bytes(), None),
        Err(Invalid::MissingUid)
    );
    let two = fixture("event-apple-decline-instance.ics");
    let two = String::from_utf8(two).unwrap();
    let at = two.rfind("UID:weekly-sync-7f3a").unwrap();
    let mut several = two.clone();
    several.replace_range(at..at + 20, "UID:other-uid-000000");
    assert_eq!(
        validate(ItemKind::Event, several.as_bytes(), None),
        Err(Invalid::SeveralUids)
    );
    let duplicate = two.replace("RECURRENCE-ID;TZID=Europe/Berlin:20261209T100000\r\n", "");
    assert_eq!(
        validate(ItemKind::Event, duplicate.as_bytes(), None),
        Err(Invalid::DuplicateInstance)
    );
    let broken = text.replace("END:VEVENT\r\n", "");
    assert_eq!(
        validate(ItemKind::Event, broken.as_bytes(), None),
        Err(Invalid::Unreadable)
    );
    let big = vec![b'x'; holtmail_pim_edit::MAX_ICALENDAR + 1];
    let err = validate(ItemKind::Event, &big, None).unwrap_err();
    assert_eq!(err.precondition(ItemKind::Event), "max-resource-size");
    assert_eq!(
        Invalid::Unreadable.precondition(ItemKind::Contact),
        "valid-address-data"
    );
    assert_eq!(
        Invalid::WrongComponent.precondition(ItemKind::Event),
        "supported-calendar-component"
    );
    let card = fixture("contact-base.vcf");
    assert!(validate(ItemKind::Contact, &card, Some(&card)).is_ok());
    let other = String::from_utf8(card.clone())
        .unwrap()
        .replace("4e1b7a52-ada-example", "someone-else");
    assert_eq!(
        validate(ItemKind::Contact, other.as_bytes(), Some(&card)),
        Err(Invalid::UidChanged)
    );
}

#[test]
fn a_reply_survives_the_organizer_moving_the_meeting() {
    let reply = classified(ItemKind::Event, "event-base.ics", "event-apple-accept.ics");
    let theirs = fixture("event-upstream-moved.ics");
    let Rebased::Clean(served) = rebase(&reply.patch, &theirs, &ada()) else {
        panic!("not clean");
    };
    let event = main_event(&served);
    assert_eq!(event.start.as_ref().unwrap().value, "2026-12-02T14:00:00");
    assert_eq!(
        partstat(&event, "ada@example.com").as_deref(),
        Some("ACCEPTED")
    );
    assert_eq!(
        partstat(&event, "carol@example.org").as_deref(),
        Some("NEEDS-ACTION")
    );
    assert_eq!(event.sequence, Some(1));
    let text = String::from_utf8(served.clone()).unwrap();
    assert!(text.contains("BEGIN:VALARM"));
    assert!(text.contains("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR"));
    assert_eq!(
        rebase(&reply.patch, &theirs, &ada()).served(),
        Some(&served[..])
    );
}

#[test]
fn a_reply_is_obsolete_when_cancelled_or_uninvited() {
    let reply = classified(ItemKind::Event, "event-base.ics", "event-apple-accept.ics");
    for theirs in [
        "event-upstream-cancelled.ics",
        "event-upstream-removed-ada.ics",
    ] {
        let rebased = rebase(&reply.patch, &fixture(theirs), &ada());
        assert!(
            matches!(rebased, Rebased::ReplyObsolete { .. }),
            "{theirs}: {rebased:?}"
        );
        assert_eq!(rebased.reason(), Some("reply_obsolete"));
    }
}

#[test]
fn upstream_wins_the_time_and_the_change_keeps_the_rest() {
    let edit = classified(ItemKind::Event, "event-base.ics", "event-davx5-edit.ics");
    let rebased = rebase(&edit.patch, &fixture("event-upstream-moved.ics"), &ada());
    let Rebased::Conflict { served, fields } = &rebased else {
        panic!("{rebased:?}");
    };
    assert_eq!(fields, &vec![Field::End, Field::Start]);
    assert_eq!(rebased.reason(), Some("overlap"));
    let event = main_event(served);
    assert_eq!(event.start.as_ref().unwrap().value, "2026-12-02T14:00:00");
    assert_eq!(event.summary.as_deref(), Some("Weekly sync (moved)"));
}

#[test]
fn edits_that_do_not_touch_upstream_wins_fields_rebase_cleanly() {
    let edit = classified(ItemKind::Event, "event-base.ics", "event-davx5-edit.ics");
    let Rebased::Clean(served) =
        rebase(&edit.patch, &fixture("event-upstream-renamed.ics"), &ada())
    else {
        panic!("not clean");
    };
    let event = main_event(&served);
    assert_eq!(event.summary.as_deref(), Some("Weekly sync (moved)"));
    assert_eq!(event.start.as_ref().unwrap().value, "2026-12-02T11:00:00");
    assert_eq!(
        partstat(&event, "dave@example.org").as_deref(),
        Some("NEEDS-ACTION")
    );

    let personal = classified(
        ItemKind::Event,
        "event-base.ics",
        "event-thunderbird-snooze.ics",
    );
    let Rebased::Clean(served) = rebase(
        &personal.patch,
        &fixture("event-upstream-renamed.ics"),
        &ada(),
    ) else {
        panic!("not clean");
    };
    let text = String::from_utf8(served.clone()).unwrap();
    assert!(text.contains("X-MOZ-LASTACK:20261104T090112Z"));
    assert_eq!(
        main_event(&served).summary.as_deref(),
        Some("Weekly sync and planning")
    );
}

#[test]
fn a_change_upstream_already_has_is_applied() {
    let edit = classified(ItemKind::Event, "event-base.ics", "event-davx5-edit.ics");
    assert_eq!(
        rebase(&edit.patch, &fixture("event-davx5-edit.ics"), &ada()),
        Rebased::Applied
    );
    let noop = classified(
        ItemKind::Event,
        "event-base.ics",
        "event-thunderbird-resave.ics",
    );
    assert_eq!(
        rebase(&noop.patch, &fixture("event-upstream-moved.ics"), &ada()),
        Rebased::Applied
    );
    assert_eq!(
        rebase(&edit.patch, b"not a calendar", &ada()).reason(),
        Some("upstream_unreadable")
    );
}

#[test]
fn an_added_instance_rebases_onto_a_newer_series() {
    let decline = classified(
        ItemKind::Event,
        "event-base.ics",
        "event-apple-decline-instance.ics",
    );
    let Rebased::Clean(served) = rebase(
        &decline.patch,
        &fixture("event-upstream-renamed.ics"),
        &ada(),
    ) else {
        panic!("not clean");
    };
    let Normalized::Calendar(object) = normalize(ItemKind::Event, &served) else {
        panic!("unreadable");
    };
    assert_eq!(object.components.len(), 2);
    let instance = &object.components[1];
    assert_eq!(
        partstat(instance, "ada@example.com").as_deref(),
        Some("DECLINED")
    );
    assert!(validate(ItemKind::Event, &served, None).is_ok());
}

#[test]
fn contact_edits_merge_field_by_field() {
    let phone = classified(
        ItemKind::Contact,
        "contact-base.vcf",
        "contact-apple-edit.vcf",
    );
    let Rebased::Clean(served) = rebase(&phone.patch, &fixture("contact-davx5-edit.vcf"), &[])
    else {
        panic!("not clean");
    };
    let Normalized::Contact(card) = normalize(ItemKind::Contact, &served) else {
        panic!("unreadable");
    };
    assert_eq!(card.phones.len(), 2);
    assert!(card.note.unwrap().contains("Prefers mornings"));
}

#[test]
fn patches_round_trip_through_json() {
    let reply = classified(
        ItemKind::Event,
        "event-base.ics",
        "event-apple-decline-instance.ics",
    );
    let json = serde_json::to_value(&reply.patch).unwrap();
    let back: Patch = serde_json::from_value(json).unwrap();
    assert_eq!(back, reply.patch);
    assert_eq!(back.class(), Class::Reply);
    assert_eq!(back.kind(), ItemKind::Event);
    assert_eq!(Class::Reply.as_str(), "reply");
}

#[test]
fn diffs_three_ways_for_the_conflict_view() {
    let base = fixture("event-base.ics");
    let mine = fixture("event-davx5-edit.ics");
    let theirs = fixture("event-upstream-moved.ics");
    let diff = field_diff(ItemKind::Event, Some(&base), Some(&mine), Some(&theirs));
    let start = diff.iter().find(|d| d.field == "start").unwrap();
    assert!(start.conflict && start.mine_changed && start.theirs_changed);
    assert_eq!(start.theirs, vec!["2026-12-02T14:00:00 Europe/Berlin"]);
    let summary = diff.iter().find(|d| d.field == "summary").unwrap();
    assert!(summary.mine_changed && !summary.theirs_changed && !summary.conflict);
    let attendee = diff.iter().find(|d| d.field == "attendee").unwrap();
    assert!(attendee.theirs_changed && !attendee.mine_changed);
    assert!(diff.iter().all(|d| d.field != "sequence"));
    let gone = field_diff(ItemKind::Event, Some(&base), Some(&mine), None);
    assert!(
        gone.iter()
            .any(|d| d.field == "summary" && d.theirs.is_empty())
    );
}

#[test]
fn builds_the_write_through_patch() {
    let reply = classified(ItemKind::Event, "event-base.ics", "event-apple-accept.ics");
    let served = fixture("event-apple-accept.ics");
    let patch = item_patch(&reply.patch, &served);
    assert_eq!(patch.content_type, "text/calendar");
    assert_eq!(patch.uid.as_deref(), Some("weekly-sync-7f3a@example.org"));
    assert_eq!(patch.raw.as_ref(), &served[..]);
    assert!(patch.fields.unwrap().contains(&Field::Attendees));
    let card = fixture("contact-base.vcf");
    let create = classify(ItemKind::Contact, None, &card, &[]).unwrap();
    let patch = item_patch(&create.patch, &card);
    assert_eq!(patch.fields, None);
    assert_eq!(patch.content_type, "text/vcard");
}

#[test]
fn busy_or_free_alone_is_an_edit() {
    let base = String::from_utf8(fixture("event-base.ics")).unwrap();
    let mine = base.replace(
        "STATUS:CONFIRMED\r\n",
        "STATUS:CONFIRMED\r\nTRANSP:TRANSPARENT\r\n",
    );
    let class = classify(
        ItemKind::Event,
        Some(base.as_bytes()),
        mine.as_bytes(),
        &ada(),
    )
    .unwrap()
    .class;
    assert_eq!(class, Class::Edit);
}

#[test]
fn serves_the_change_or_its_rebase_on_newer_upstream() {
    let base = fixture("event-base.ics");
    let mine = fixture("event-apple-accept.ics");
    let c = classify(ItemKind::Event, Some(&base), &mine, &ada()).unwrap();
    assert_eq!(c.patch.attendees(), vec!["ada@example.com".to_owned()]);
    assert_eq!(
        serve(Some(&c.patch), &mine, Some(1), Some((1, &base)), &ada()),
        mine
    );
    assert_eq!(serve(None, &mine, Some(1), Some((2, &base)), &ada()), mine);
    let renamed = fixture("event-upstream-renamed.ics");
    let served = serve(Some(&c.patch), &mine, Some(1), Some((2, &renamed)), &[]);
    assert_eq!(
        Rebased::Clean(served),
        rebase(&c.patch, &renamed, &c.patch.attendees())
    );
    let resave = classify(
        ItemKind::Event,
        Some(&base),
        &fixture("event-thunderbird-resave.ics"),
        &ada(),
    )
    .unwrap();
    assert_eq!(
        serve(
            Some(&resave.patch),
            &mine,
            Some(1),
            Some((2, &renamed)),
            &ada()
        ),
        renamed
    );
}
