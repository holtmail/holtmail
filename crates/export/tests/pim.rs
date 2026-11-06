//! Exporting contacts and calendar items.

use std::io::Read;
use std::sync::Arc;

use bytes::Bytes;
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{ArchiveId, CollectionId, Timestamp};
use holtmail_export::{ExportError, Exporter, PimFormat, PimKind, PimSelection, PimTotals};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{IncomingItem, Ingest, Placement};
use holtmail_pim::{Format, Normalized};
use holtmail_store::{CollectionKind, NewCollection, Store};
use holtmail_store_sqlite::SqliteStore;

const ADA_1: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada Quill\r\nEMAIL:ada@example.com\r\nEND:VCARD\r\n";
const ADA_2: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada Quill\r\nEMAIL:ada@example.com\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";
const BOB: &str =
    "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:bob\r\nFN:Bob Stone\r\nEMAIL:bob@example.org\r\nEND:VCARD";
const GONE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:gone\r\nFN:Gone Away\r\nEND:VCARD\r\n";
const CLEO: &str = r#"{"id":"AAMk-cleo","displayName":"Cleo Graph","emailAddresses":[{"name":"Cleo","address":"cleo@example.org"}]}"#;
const REVIEW: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:review@example.com\r\nDTSTAMP:20250301T000000Z\r\nDTSTART:20250310T090000Z\r\nDTEND:20250310T100000Z\r\nSUMMARY:Lamp review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const LATER: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:later@example.com\r\nDTSTAMP:20250301T000000Z\r\nDTSTART:20260310T090000Z\r\nDTEND:20260310T100000Z\r\nSUMMARY:Later\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const HEAD: &str =
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Export//EN\r\nCALSCALE:GREGORIAN\r\n";

fn component(calendar: &str) -> &str {
    let start = calendar.find("BEGIN:VEVENT").unwrap();
    let end = calendar.find("END:VCALENDAR").unwrap();
    &calendar[start..end]
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<dyn Store>,
    ingest: Ingest,
    exporter: Exporter,
    archive: ArchiveId,
    work: CollectionId,
    team: CollectionId,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let blobs: Arc<dyn BlobStore> =
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
    let store: Arc<dyn Store> = Arc::new(
        SqliteStore::open(dir.path().join("holtmail.db"))
            .await
            .unwrap(),
    );
    let index = Arc::new(TantivyIndex::in_memory().unwrap());
    let ingest = Ingest::new(blobs.clone(), store.clone(), index);
    let archive = store.create_archive("Personal").await.unwrap().id;
    let collection = |kind, remote: &str, name: &str| NewCollection {
        archive_id: archive,
        account_id: None,
        kind,
        remote_id: remote.into(),
        name: name.into(),
        color: None,
    };
    let work = store
        .upsert_collection(collection(CollectionKind::Contacts, "work.vcf", "Work"))
        .await
        .unwrap()
        .id;
    let graph = store
        .upsert_collection(collection(CollectionKind::Contacts, "contacts", "work"))
        .await
        .unwrap()
        .id;
    let team = store
        .upsert_collection(collection(CollectionKind::Calendar, "team.ics", "Team"))
        .await
        .unwrap()
        .id;
    for (collection, remote, format, raw, at) in [
        (work, "ada", Format::VCard, ADA_1, 1_700_000_000),
        (work, "ada", Format::VCard, ADA_2, 1_700_086_400),
        (work, "bob", Format::VCard, BOB, 1_700_000_000),
        (work, "gone", Format::VCard, GONE, 1_700_000_000),
        (
            graph,
            "AAMk-cleo",
            Format::GraphContact,
            CLEO,
            1_700_000_000,
        ),
        (team, "review", Format::ICalendar, REVIEW, 1_740_000_000),
        (team, "later", Format::ICalendar, LATER, 1_740_000_000),
    ] {
        ingest
            .ingest_item(IncomingItem {
                collection,
                remote_id: remote.into(),
                kind: format.kind(raw.as_bytes()),
                format,
                raw: Bytes::from_static(raw.as_bytes()),
                etag: None,
                seen_at: Timestamp::from_secs(at),
            })
            .await
            .unwrap();
    }
    store
        .mark_items_removed(work, &["gone".into()], Timestamp::from_secs(1_700_100_000))
        .await
        .unwrap();
    holtmail_people::rebuild(&*store, archive).await.unwrap();
    Fixture {
        exporter: Exporter::new(blobs, store.clone()),
        _dir: dir,
        store,
        ingest,
        archive,
        work,
        team,
    }
}

fn selection(kind: PimKind) -> PimSelection {
    PimSelection {
        kind,
        collection: None,
        person: None,
        range: None,
        removed: false,
    }
}

async fn run(
    f: &Fixture,
    selection: PimSelection,
    format: PimFormat,
) -> (Vec<u8>, PimTotals, String) {
    let export = f
        .exporter
        .prepare_pim(f.archive, selection, format)
        .await
        .unwrap();
    let name = export.file_name();
    let mut out = Vec::new();
    let totals = export.write(&mut out, &|_| {}).await.unwrap();
    (out, totals, name)
}

#[tokio::test]
async fn writes_the_latest_contacts_as_one_vcf() {
    let f = fixture().await;
    let (out, totals, name) = run(&f, selection(PimKind::Contacts), PimFormat::Text).await;
    assert_eq!(name, "Personal - contacts.vcf");
    assert_eq!(totals.items, 3, "the removed card is left out");
    let items = holtmail_pim::split(&out);
    assert_eq!(items.len(), 3);
    let raws: Vec<&[u8]> = items.iter().map(|i| i.raw.as_slice()).collect();
    assert!(
        raws.contains(&ADA_2.as_bytes()),
        "the latest version, unchanged"
    );
    assert!(
        raws.contains(&format!("{BOB}\r\n").as_bytes()),
        "a line end is added"
    );
    let cleo = items
        .iter()
        .find(|i| i.uid.as_deref() == Some("AAMk-cleo"))
        .expect("the Graph card as a vCard");
    assert!(String::from_utf8_lossy(&cleo.raw).contains("EMAIL:cleo@example.org"));

    let mut all = selection(PimKind::Contacts);
    all.removed = true;
    assert_eq!(run(&f, all, PimFormat::Text).await.1.items, 4);

    let mut work = selection(PimKind::Contacts);
    work.collection = Some(f.work);
    let (_, totals, name) = run(&f, work, PimFormat::Text).await;
    assert_eq!(totals.items, 2);
    assert_eq!(name, "Personal - contacts - Work.vcf");

    let ada = f
        .store
        .people_by_email(f.archive, &["ada@example.com".into()])
        .await
        .unwrap()[0]
        .1;
    let mut person = selection(PimKind::Contacts);
    person.person = Some(ada);
    let (out, totals, name) = run(&f, person, PimFormat::Text).await;
    assert_eq!(totals.items, 1);
    assert_eq!(out, ADA_2.as_bytes());
    assert_eq!(name, "Personal - contacts - Ada Quill.vcf");
}

#[tokio::test]
async fn writes_every_version_into_a_zip() {
    let f = fixture().await;
    let (out, totals, name) = run(&f, selection(PimKind::Contacts), PimFormat::Zip).await;
    assert_eq!(name, "Personal - contacts.zip");
    assert_eq!(totals.items, 4);
    assert_eq!(totals.versions, 5);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(out)).unwrap();
    let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    let ada: Vec<&String> = names.iter().filter(|n| n.contains("/Ada Quill ")).collect();
    assert_eq!(ada.len(), 2, "{names:?}");
    assert!(
        ada.iter().any(|n| n.ends_with("/1 2023-11-14 221320.vcf")),
        "{names:?}"
    );
    assert!(
        ada.iter().any(|n| n.ends_with("/2 2023-11-15 221320.vcf")),
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n.ends_with(".json") && n.contains("Cleo Graph")),
        "Graph JSON as it was stored: {names:?}"
    );
    let dirs: std::collections::BTreeSet<&str> =
        names.iter().filter_map(|n| n.split('/').next()).collect();
    assert_eq!(
        dirs.len(),
        3,
        "two books named alike get two directories, and the manifest: {dirs:?}"
    );
    let second = ada.iter().find(|n| n.contains("/2 ")).unwrap().to_string();
    let mut bytes = Vec::new();
    zip.by_name(&second)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, ADA_2.as_bytes());
    let mut manifest = String::new();
    zip.by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["format"], "holtmail-pim-export");
    assert_eq!(manifest["items"].as_array().unwrap().len(), 4);
    assert!(
        manifest["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["title"] == "Gone Away" && i["removed_at"].is_number())
    );
}

#[tokio::test]
async fn writes_calendars_by_range() {
    let f = fixture().await;
    let (out, totals, name) = run(&f, selection(PimKind::Calendar), PimFormat::Text).await;
    assert_eq!(name, "Personal - calendar.ics");
    assert_eq!(totals.items, 2);
    let text = String::from_utf8(out.clone()).unwrap();
    assert_eq!(text.matches("BEGIN:VCALENDAR").count(), 1, "{text}");
    assert_eq!(holtmail_pim::split(&out).len(), 2);

    let mut march = selection(PimKind::Calendar);
    march.range = Some((
        Timestamp::from_secs(1_740_787_200),
        Timestamp::from_secs(1_743_465_600),
    ));
    march.collection = Some(f.team);
    let (out, totals, name) = run(&f, march, PimFormat::Text).await;
    assert_eq!(totals.items, 1);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("{HEAD}{}END:VCALENDAR\r\n", component(REVIEW))
    );
    assert_eq!(name, "Personal - calendar - Team.ics");

    let (out, totals, _) = run(&f, selection(PimKind::Calendar), PimFormat::Zip).await;
    assert_eq!(totals.versions, 2);
    let zip = zip::ZipArchive::new(std::io::Cursor::new(out)).unwrap();
    assert!(
        zip.file_names()
            .map(|n| n.unwrap())
            .any(|n| n.starts_with("Team/Lamp review ") && n.ends_with(".ics"))
    );
}

#[tokio::test]
async fn writes_invitations_from_mail_as_their_merged_event() {
    let f = fixture().await;
    for name in [
        "invite-request.eml",
        "invite-reply.eml",
        "invite-cancel.eml",
    ] {
        let raw = std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        f.ingest
            .ingest(f.archive, Bytes::from(raw), &Placement::default())
            .await
            .unwrap();
    }
    let (out, totals, _) = run(&f, selection(PimKind::Calendar), PimFormat::Text).await;
    assert_eq!(totals.items, 3);
    let text = String::from_utf8(out.clone()).unwrap();
    assert_eq!(text.matches("BEGIN:VCALENDAR").count(), 1, "{text}");
    assert!(!text.contains("METHOD:"), "{text}");
    let harbour = holtmail_pim::split(&out)
        .into_iter()
        .find(|i| i.uid.as_deref() == Some("harbour-planning-7f3a@example.com"))
        .expect("the invitation's event");
    let Normalized::Calendar(object) =
        holtmail_pim::normalize(holtmail_pim::ItemKind::Event, &harbour.raw)
    else {
        panic!("readable");
    };
    let event = object.main().unwrap();
    assert_eq!(event.summary.as_deref(), Some("Harbour planning"));
    assert_eq!(
        event.location.as_deref(),
        Some("Room 4"),
        "the full invitation's fields"
    );
    assert_eq!(event.status.as_deref(), Some("CANCELLED"));
    assert_eq!(
        event.start.as_ref().map(|t| t.value.as_str()),
        Some("2025-11-10T10:00:00")
    );
    let bob = event
        .attendees
        .iter()
        .find(|a| a.email.as_deref() == Some("bob@example.org"))
        .unwrap();
    assert_eq!(
        bob.partstat.as_deref(),
        Some("ACCEPTED"),
        "the reply's answer"
    );
}

#[tokio::test]
async fn reimported_calendars_keep_their_items() {
    let f = fixture().await;
    let (first, _, _) = run(&f, selection(PimKind::Calendar), PimFormat::Text).await;
    let copy = f.store.create_archive("Copy").await.unwrap().id;
    let collection = f
        .store
        .upsert_collection(NewCollection {
            archive_id: copy,
            account_id: None,
            kind: CollectionKind::Calendar,
            remote_id: "copy.ics".into(),
            name: "Team".into(),
            color: None,
        })
        .await
        .unwrap()
        .id;
    let items = holtmail_pim::split(&first);
    assert_eq!(items.len(), 2);
    for (item, original) in items.iter().zip([REVIEW, LATER]) {
        assert_eq!(
            holtmail_pim::normalize(item.kind, &item.raw),
            holtmail_pim::normalize(item.kind, original.as_bytes()),
            "the same event"
        );
        assert!(
            String::from_utf8_lossy(&item.raw).contains(component(original)),
            "its component byte for byte"
        );
        f.ingest
            .ingest_item(IncomingItem {
                collection,
                remote_id: item.uid.clone().unwrap(),
                kind: item.kind,
                format: Format::ICalendar,
                raw: Bytes::from(item.raw.clone()),
                etag: None,
                seen_at: Timestamp::from_secs(1_750_000_000),
            })
            .await
            .unwrap();
    }
    let export = f
        .exporter
        .prepare_pim(copy, selection(PimKind::Calendar), PimFormat::Text)
        .await
        .unwrap();
    let mut second = Vec::new();
    export.write(&mut second, &|_| {}).await.unwrap();
    assert_eq!(
        holtmail_pim::split(&second),
        items,
        "a second round trip gives the same bytes"
    );
}

#[tokio::test]
async fn refuses_selections_that_do_not_fit() {
    let f = fixture().await;
    let prepare = |selection: PimSelection| {
        let exporter = f.exporter.clone();
        let archive = f.archive;
        async move {
            exporter
                .prepare_pim(archive, selection, PimFormat::Text)
                .await
                .err()
        }
    };
    let mut wrong_kind = selection(PimKind::Calendar);
    wrong_kind.collection = Some(f.work);
    assert!(matches!(
        prepare(wrong_kind).await,
        Some(ExportError::NotFound("collection"))
    ));
    let mut person = selection(PimKind::Calendar);
    person.person = Some(holtmail_core::PersonId::from_hash([1; 16]));
    assert!(matches!(
        prepare(person).await,
        Some(ExportError::Invalid(_))
    ));
    let mut range = selection(PimKind::Contacts);
    range.range = Some((Timestamp::from_secs(1), Timestamp::from_secs(2)));
    assert!(matches!(
        prepare(range).await,
        Some(ExportError::Invalid(_))
    ));
    let mut backwards = selection(PimKind::Calendar);
    backwards.range = Some((Timestamp::from_secs(2), Timestamp::from_secs(1)));
    assert!(matches!(
        prepare(backwards).await,
        Some(ExportError::Invalid(_))
    ));
    let mut nobody = selection(PimKind::Contacts);
    nobody.person = Some(holtmail_core::PersonId::from_hash([1; 16]));
    assert!(matches!(
        prepare(nobody).await,
        Some(ExportError::NotFound("person"))
    ));
    assert_eq!(
        PimFormat::parse("VCF", PimKind::Contacts),
        Some(PimFormat::Text)
    );
    assert_eq!(PimFormat::parse("vcf", PimKind::Calendar), None);
    assert_eq!(
        PimFormat::parse("zip", PimKind::Calendar),
        Some(PimFormat::Zip)
    );
}
