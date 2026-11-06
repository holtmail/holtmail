use std::sync::Arc;

use holtmail_blob::BlobId;
use holtmail_core::CollectionId;
use holtmail_pim::normalize;
use holtmail_store::{CollectionKind, Item, NewCollection, NewItemVersion, PimIndex};
use holtmail_store_sqlite::SqliteStore;

use super::*;

fn ics(events: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n{}END:VCALENDAR\r\n",
        events.replace('\n', "\r\n")
    )
    .into_bytes()
}

fn index(raw: &[u8]) -> PimIndex {
    let normalized = normalize(holtmail_pim::ItemKind::Event, raw);
    let main = match &normalized {
        Normalized::Calendar(c) => c.main().cloned(),
        _ => None,
    };
    PimIndex {
        version: 1,
        readable: main.is_some(),
        title: normalized.title(),
        starts_at: main
            .as_ref()
            .and_then(|e| e.start.as_ref())
            .and_then(|s| s.utc)
            .map(Timestamp::from_millis),
        ends_at: None,
        all_day: false,
        time_zone: None,
        recurring: main.as_ref().is_some_and(|e| e.recurring),
        sequence: main.as_ref().and_then(|e| e.sequence),
        uid: main.as_ref().and_then(|e| e.uid.clone()),
        organizer: None,
        emails: normalized.emails(),
        phones: Vec::new(),
        data: serde_json::to_value(&normalized).unwrap(),
    }
}

fn summary(archive: ArchiveId, raw: &[u8]) -> ItemSummary {
    ItemSummary {
        item: Item {
            id: ItemId::new(),
            collection_id: CollectionId::new(),
            archive_id: archive,
            remote_id: "x".into(),
            kind: ItemKind::Event,
            key: None,
            first_seen_at: Timestamp::from_secs(0),
            removed_at: None,
            versions: 1,
        },
        index: Some(index(raw)),
    }
}

fn ms(utc: &str) -> i64 {
    let n = |r: std::ops::Range<usize>| utc[r].parse::<i64>().unwrap();
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let (hh, mm) = (n(11..13), n(14..16));
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mi = (m + 9) % 12;
    let doy = (153 * mi + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + hh * 3600 + mm * 60) * 1000
}

const WEEKLY: &str = "BEGIN:VEVENT
UID:weekly@example.com
SEQUENCE:1
DTSTAMP:20250301T000000Z
DTSTART;TZID=Europe/Berlin:20250317T090000
DTEND;TZID=Europe/Berlin:20250317T093000
RRULE:FREQ=WEEKLY;BYDAY=MO
SUMMARY:Weekly
ATTENDEE;CN=Bob Stone;PARTSTAT=ACCEPTED:mailto:bob@example.org
ATTENDEE;CN=Cleo Park;PARTSTAT=NEEDS-ACTION:mailto:cleo@example.org
END:VEVENT
";

#[test]
fn groups_by_uid_within_an_archive() {
    let archive = ArchiveId::new();
    let a = group_id(archive, Some("x@example.com"), ItemId::new());
    assert_eq!(a, group_id(archive, Some(" x@example.com "), ItemId::new()));
    assert_ne!(
        a,
        group_id(ArchiveId::new(), Some("x@example.com"), ItemId::new())
    );
    let item = ItemId::new();
    assert_eq!(
        group_id(archive, None, item),
        group_id(archive, Some(""), item)
    );
    assert_ne!(
        group_id(archive, None, item),
        group_id(archive, None, ItemId::new())
    );
}

#[test]
fn endless_series_store_a_window_and_short_ones_everything() {
    let archive = ArchiveId::new();
    let now = Timestamp::from_millis(ms("2026-01-01T00:00"));
    let endless = summary(archive, &ics(WEEKLY));
    let (series, stored) = series(&endless, now).unwrap();
    assert!(series.recurring);
    assert_eq!(series.sequence, Some(1));
    assert_eq!(
        series.stamp,
        Some(Timestamp::from_millis(ms("2025-03-01T00:00")))
    );
    assert_eq!(
        series.starts_at,
        Some(Timestamp::from_millis(ms("2025-03-17T08:00")))
    );
    assert_eq!(series.ends_at, None);
    let (from, to) = series.window.unwrap();
    assert_eq!(to.as_millis() - now.as_millis(), WINDOW_MS);
    assert_eq!(now.as_millis() - from.as_millis(), WINDOW_MS);
    assert!(stored.first().unwrap().starts_at.as_millis() >= ms("2025-03-17T08:00"));
    assert!(stored.last().unwrap().starts_at < to);
    assert_eq!(
        series.group_id,
        group_id(archive, Some("weekly@example.com"), endless.item.id)
    );

    let short = summary(
        archive,
        &ics(&WEEKLY.replace("BYDAY=MO", "BYDAY=MO;COUNT=3")),
    );
    let (series, stored) = super::series(&short, now).unwrap();
    assert_eq!(series.window, None);
    assert_eq!(stored.len(), 3);
    assert_eq!(
        series.ends_at,
        Some(Timestamp::from_millis(ms("2025-03-31T07:30")))
    );

    let mut contact = summary(archive, b"BEGIN:VCARD\r\nFN:Ada\r\nEND:VCARD\r\n");
    contact.item.kind = ItemKind::Contact;
    assert!(super::series(&contact, now).is_none());
}

fn occurrence(item: ItemId, group: EventGroupId, rid: &str, seq: i64, removed: bool) -> Occurrence {
    Occurrence {
        item_id: item,
        collection_id: CollectionId::new(),
        group_id: group,
        removed,
        sequence: Some(seq),
        stamp: Some(Timestamp::from_secs(seq * 10)),
        occurrence: NewOccurrence {
            recurrence_id: Some(rid.into()),
            starts_at: Timestamp::from_secs(rid.len() as i64 * 100 + seq),
            ends_at: Timestamp::from_secs(10_000),
            component: 0,
            start_local: String::new(),
            end_local: String::new(),
            time_zone: None,
            all_day: false,
            floating: false,
            cancelled: false,
            changed: false,
        },
    }
}

#[test]
fn folds_copies_behind_the_leading_one() {
    let group = EventGroupId::from_hash([1; 16]);
    let (old, new, removed) = (ItemId::new(), ItemId::new(), ItemId::new());
    let folded = fold(vec![
        occurrence(old, group, "a", 1, false),
        occurrence(new, group, "a", 3, false),
        occurrence(removed, group, "a", 3, true),
        occurrence(old, group, "bb", 1, false),
        occurrence(new, EventGroupId::from_hash([2; 16]), "a", 9, false),
    ]);
    assert_eq!(folded.len(), 3);
    let first = folded
        .iter()
        .find(|f| f.leading().group_id == group && f.copies.len() == 3)
        .unwrap();
    let order: Vec<ItemId> = first.copies.iter().map(|c| c.item_id).collect();
    assert_eq!(
        order,
        [new, removed, old],
        "highest SEQUENCE, then live before removed"
    );
    assert_eq!(
        leads(
            (Some(1), Some(Timestamp::from_secs(5)), false, old),
            (Some(1), Some(Timestamp::from_secs(9)), false, new)
        ),
        std::cmp::Ordering::Greater,
        "the same SEQUENCE: the later stamp leads"
    );
    assert_eq!(
        leads((None, None, false, old), (Some(0), None, false, new)),
        std::cmp::Ordering::Greater
    );
}

#[test]
fn lists_differences_and_answers_per_copy() {
    let organizer_copy = normalize(
        holtmail_pim::ItemKind::Event,
        &ics(&WEEKLY
            .replace("SEQUENCE:1", "SEQUENCE:2")
            .replace("T090000", "T100000")
            .replace("T093000", "T103000")),
    );
    let attendee_copy = normalize(
        holtmail_pim::ItemKind::Event,
        &ics(&WEEKLY
            .replace("PARTSTAT=ACCEPTED", "PARTSTAT=TENTATIVE")
            .replace("SUMMARY:Weekly", "SUMMARY:Weekly\nLOCATION:Room 4")),
    );
    let (a, b) = (ItemId::new(), ItemId::new());
    let copies = [(a, &organizer_copy), (b, &attendee_copy)];
    let found = differences(&copies);
    let names: Vec<&str> = found.iter().map(|d| d.field.as_str()).collect();
    assert_eq!(names, ["start", "end", "sequence", "location"]);
    assert_eq!(
        found[0].values[0],
        (a, vec!["2025-03-17T10:00:00 Europe/Berlin".to_owned()])
    );
    assert_eq!(found[3].values[0], (a, Vec::new()));
    assert!(differences(&copies[..1]).is_empty());

    let answered = answers(&copies);
    assert_eq!(answered.len(), 2);
    assert_eq!(answered[0].email.as_deref(), Some("bob@example.org"));
    assert_eq!(
        answered[0].answers,
        [(a, Some("accepted".into())), (b, Some("tentative".into()))]
    );
    assert_eq!(answered[1].answers[1], (b, Some("needs-action".into())));
}

#[tokio::test]
async fn rebuilds_items_and_lists_instances_inside_and_outside_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        SqliteStore::open(dir.path().join("db.sqlite"))
            .await
            .unwrap(),
    );
    let archive = store.create_archive("Calendar").await.unwrap().id;
    let calendar = |name: &str| NewCollection {
        archive_id: archive,
        account_id: None,
        kind: CollectionKind::Calendar,
        remote_id: name.into(),
        name: name.into(),
        color: None,
    };
    let work = store
        .upsert_collection(calendar("work.ics"))
        .await
        .unwrap()
        .id;
    let home = store
        .upsert_collection(calendar("home.ics"))
        .await
        .unwrap()
        .id;
    let record = |collection, raw: Vec<u8>| NewItemVersion {
        collection_id: collection,
        remote_id: "weekly".into(),
        kind: ItemKind::Event,
        key: Some("weekly@example.com".into()),
        blob_id: BlobId::of(&raw),
        content_type: "text/calendar".into(),
        etag: None,
        seen_at: Timestamp::from_secs(0),
        photo_blob_id: None,
        index: index(&raw),
    };
    let first = store
        .record_version(record(work, ics(WEEKLY)))
        .await
        .unwrap()
        .item_id;
    let second = store
        .record_version(record(
            home,
            ics(&WEEKLY.replace("SEQUENCE:1", "SEQUENCE:4")),
        ))
        .await
        .unwrap()
        .item_id;
    let now = Timestamp::from_millis(ms("2025-06-01T00:00"));
    rebuild(&*store, first, now).await.unwrap();
    rebuild(&*store, second, now).await.unwrap();

    let query = |from: &str, to: &str| OccurrenceQuery {
        archive_id: archive,
        from: Timestamp::from_millis(ms(from)),
        to: Timestamp::from_millis(ms(to)),
        collections: None,
        removed: false,
    };
    let inside = instances(&*store, &query("2025-03-30T00:00", "2025-04-06T00:00"))
        .await
        .unwrap();
    assert_eq!(inside.len(), 2, "one instance in each copy");
    let folded = fold(inside);
    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].leading().item_id, second);
    assert_eq!(folded[0].copies.len(), 2);
    assert_eq!(
        folded[0].leading().occurrence.starts_at,
        Timestamp::from_millis(ms("2025-03-31T07:00"))
    );

    let far = instances(&*store, &query("2040-01-01T00:00", "2040-01-15T00:00"))
        .await
        .unwrap();
    assert_eq!(
        far.len(),
        4,
        "worked out from the fields, outside the stored window"
    );
    assert!(
        far.iter()
            .all(|o| o.occurrence.start_local.ends_with("T09:00:00"))
    );
    let members = store
        .group_members(folded[0].leading().group_id)
        .await
        .unwrap();
    assert_eq!(members.len(), 2);

    let before = instances(&*store, &query("2024-01-01T00:00", "2025-03-01T00:00"))
        .await
        .unwrap();
    assert!(before.is_empty(), "nothing before the series starts");
}
