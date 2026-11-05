use std::collections::BTreeSet;
use std::path::Path;

use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{Address, Timestamp};
use holtmail_index::{Clause, IndexDocument};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_store::{AccountKind, NewAccount, NewMessage};
use holtmail_store_sqlite::SqliteStore;

use super::*;

const MESSAGES: [(&str, &[u8]); 5] = [
    (
        "INBOX",
        b"From: ada@example.com\r\nSubject: Plain\r\n\r\nHello\r\n",
    ),
    (
        "INBOX",
        b"From: bob@example.org\nSubject: Quoted\n\nFrom the start\n>From a quote\n\n",
    ),
    (
        "Projects/2025",
        b"From: cy@example.com\nSubject: ../../escape\n\nNested\n",
    ),
    (
        "Projects/2025",
        b"From: dee@example.org\nSubject: Odd lines\n\nFrom me\n>>From you\n",
    ),
    (
        "INBOX",
        b"From: eve@example.com\nSubject: No newline\n\nEnd",
    ),
];

struct Fixture {
    _dir: tempfile::TempDir,
    exporter: Exporter,
    store: Arc<dyn Store>,
    archive: ArchiveId,
    folders: HashMap<String, FolderId>,
    blobs: Vec<BlobId>,
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
    let archive = store.create_archive("Personal").await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Import".into(),
            address: None,
            settings: serde_json::Value::Null,
            sealed_secret: None,
            enabled: true,
        })
        .await
        .unwrap();
    let mut folders = HashMap::new();
    let mut ids = Vec::new();
    let mut documents = Vec::new();
    for (n, (folder, raw)) in MESSAGES.iter().enumerate() {
        let folder_id = match folders.get(*folder) {
            Some(id) => *id,
            None => {
                let id = store
                    .upsert_folder(account.id, folder, None)
                    .await
                    .unwrap()
                    .id;
                folders.insert(folder.to_string(), id);
                id
            }
        };
        let blob = blobs.put(Bytes::from_static(raw)).await.unwrap();
        let inserted = store
            .insert_message(NewMessage {
                archive_id: archive,
                blob_id: blob,
                size: raw.len() as u64,
                rfc_message_id: None,
                parent_ids: Vec::new(),
                subject: Some(subject(raw)),
                from: vec![Address {
                    name: None,
                    email: format!("sender{n}@example.com"),
                }],
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                sent_at: Some(Timestamp::from_secs(1_741_942_800 + n as i64 * 60)),
                received_at: None,
                preview: String::new(),
                attachments: Vec::new(),
            })
            .await
            .unwrap();
        store
            .record_location(
                inserted.message_id,
                folder_id,
                Some(n as u64 + 1),
                &[],
                Timestamp::now(),
            )
            .await
            .unwrap();
        if n == 1 {
            store
                .set_labels(inserted.message_id, &["Travel".into()])
                .await
                .unwrap();
        }
        let message = store
            .get_message(inserted.message_id)
            .await
            .unwrap()
            .unwrap();
        documents.push(IndexDocument {
            message_id: message.id,
            archive_id: archive,
            thread_id: message.thread_id,
            subject: subject(raw),
            from: Vec::new(),
            recipients: Vec::new(),
            body: String::new(),
            attachment_names: Vec::new(),
            attachment_text: String::new(),
            folders: vec![folder.to_string()],
            flags: Vec::new(),
            labels: Vec::new(),
            date: message.date,
            has_attachments: false,
        });
        ids.push(blob);
    }
    index.put(documents).await.unwrap();
    index.commit().await.unwrap();
    Fixture {
        exporter: Exporter::new(blobs, store.clone()).with_index(index),
        _dir: dir,
        store,
        archive,
        folders,
        blobs: ids,
    }
}

fn subject(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .lines()
        .find_map(|l| l.strip_prefix("Subject: "))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

async fn export_to(fixture: &Fixture, selection: Selection, format: Format, path: &Path) -> Totals {
    let export = fixture
        .exporter
        .prepare(fixture.archive, selection, format)
        .await
        .unwrap();
    let mut file = tokio::fs::File::create(path).await.unwrap();
    export.write(&mut file, &|_| {}).await.unwrap()
}

fn read_back(path: &Path) -> Vec<holtmail_import::ImportedMessage> {
    holtmail_import::open(path)
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn mbox_reads_back_to_the_same_blobs() {
    let fixture = fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("all.mbox");
    let totals = export_to(&fixture, Selection::All, Format::Mbox, &path).await;
    assert_eq!(totals.messages, 5);
    assert_eq!(totals.skipped, 0);

    let back: BTreeSet<BlobId> = read_back(&path)
        .iter()
        .map(|m| BlobId::of(&m.raw))
        .collect();
    let expected: BTreeSet<BlobId> = fixture.blobs[..4].iter().copied().collect();
    assert!(
        expected.is_subset(&back),
        "every message ending in a line break reads back unchanged"
    );
    assert_eq!(back.len(), 5);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\n>From the start\n>>From a quote\n"));
    assert!(text.starts_with("From sender4@example.com Fri Mar 14 09:04:00 2025\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_reads_back_with_folders_and_the_same_blobs() {
    let fixture = fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("all.zip");
    let totals = export_to(&fixture, Selection::All, Format::Zip, &path).await;
    assert_eq!(totals.messages, 5);

    let back = read_back(&path);
    let blobs: BTreeSet<BlobId> = back.iter().map(|m| BlobId::of(&m.raw)).collect();
    assert_eq!(blobs, fixture.blobs.iter().copied().collect());
    let folders: BTreeSet<Option<String>> = back.iter().map(|m| m.folder.clone()).collect();
    assert_eq!(
        folders,
        BTreeSet::from([Some("INBOX".into()), Some("Projects/2025".into())])
    );

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| !n.starts_with('/') && n.split('/').all(|s| s != ".." && s != "."))
    );
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("Projects/2025/2025-03-14 0902 _.._escape "))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn narrows_to_a_folder_a_label_or_a_search() {
    let fixture = fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.zip");

    let folder = fixture.folders["Projects/2025"];
    let totals = export_to(&fixture, Selection::Folder(folder), Format::Zip, &path).await;
    assert_eq!(totals.messages, 2);
    assert!(
        read_back(&path)
            .iter()
            .all(|m| m.folder.as_deref() == Some("Projects/2025"))
    );

    let totals = export_to(
        &fixture,
        Selection::Label("Travel".into()),
        Format::Mbox,
        &path,
    )
    .await;
    assert_eq!(totals.messages, 1);

    let query = SearchQuery {
        clauses: vec![Clause::In("INBOX".into())],
    };
    let totals = export_to(&fixture, Selection::Search(query), Format::Mbox, &path).await;
    assert_eq!(totals.messages, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_what_does_not_belong_and_names_the_file() {
    let fixture = fixture().await;
    let other = fixture.store.create_archive("Other").await.unwrap().id;
    let folder = fixture.folders["INBOX"];
    assert!(matches!(
        fixture
            .exporter
            .prepare(other, Selection::Folder(folder), Format::Zip)
            .await,
        Err(ExportError::NotFound("folder"))
    ));
    assert!(matches!(
        fixture
            .exporter
            .prepare(ArchiveId::new(), Selection::All, Format::Zip)
            .await,
        Err(ExportError::NotFound("archive"))
    ));
    let without_index = Exporter::new(fixture.exporter.blobs.clone(), fixture.store.clone());
    assert!(matches!(
        without_index
            .prepare(
                fixture.archive,
                Selection::Search(SearchQuery::default()),
                Format::Mbox
            )
            .await,
        Err(ExportError::NoIndex)
    ));
    let export = fixture
        .exporter
        .prepare(fixture.archive, Selection::Folder(folder), Format::Mbox)
        .await
        .unwrap();
    assert_eq!(export.file_name(), "Personal - INBOX.mbox");
}

#[tokio::test(flavor = "multi_thread")]
async fn skips_missing_bytes() {
    let fixture = fixture().await;
    fixture
        .exporter
        .blobs
        .delete(&fixture.blobs[0])
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let totals = export_to(
        &fixture,
        Selection::All,
        Format::Zip,
        &dir.path().join("out.zip"),
    )
    .await;
    assert_eq!((totals.messages, totals.skipped), (4, 1));
}
