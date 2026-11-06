//! Invitations, updates, replies and cancellations in mail become one event with its history.

use std::sync::Arc;

use bytes::Bytes;
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{ArchiveId, ItemId, MessageId};
use holtmail_index::SearchIndex;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{Ingest, MAIL_COLLECTION, Placement};
use holtmail_store::{ItemKind, ItemOrder, ItemQuery, RemovedFilter, Store};
use holtmail_store_sqlite::SqliteStore;

const MESSAGES: [&str; 5] = [
    "invite-request.eml",
    "unrelated.eml",
    "invite-update.eml",
    "invite-reply.eml",
    "invite-cancel.eml",
];

fn fixture(name: &str) -> Bytes {
    Bytes::from(
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
}

struct Setup {
    ingest: Ingest,
    store: Arc<dyn Store>,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let blobs: Arc<dyn BlobStore> =
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
    Setup {
        ingest: Ingest::new(blobs, store.clone(), index),
        store,
        _dir: dir,
    }
}

async fn ingest_all(s: &Setup, archive: ArchiveId, names: &[&str]) -> Vec<MessageId> {
    let mut ids = Vec::new();
    for name in names {
        ids.push(
            s.ingest
                .ingest(archive, fixture(name), &Placement::default())
                .await
                .unwrap()
                .message_id,
        );
    }
    ids
}

async fn events(s: &Setup, archive: ArchiveId) -> Vec<ItemId> {
    s.store
        .list_items(&ItemQuery {
            archive_id: archive,
            kind: Some(ItemKind::Event),
            collection_id: None,
            removed: RemovedFilter::Include,
            order: ItemOrder::Start,
            after: None,
            limit: 50,
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| i.item.id)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_event_with_ordered_versions_in_any_import_order() {
    let s = setup().await;
    let forward = s.store.create_archive("Forward").await.unwrap().id;
    let backward = s.store.create_archive("Backward").await.unwrap().id;
    let ids = ingest_all(&s, forward, &MESSAGES).await;
    let mut reversed = MESSAGES;
    reversed.reverse();
    ingest_all(&s, backward, &reversed).await;

    let mut views = Vec::new();
    for archive in [forward, backward] {
        let items = events(&s, archive).await;
        assert_eq!(items.len(), 1, "one event per UID");
        let collections = s.store.list_collections(archive).await.unwrap();
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].collection.remote_id, MAIL_COLLECTION);
        assert_eq!(collections[0].collection.name, "From mail");
        let summary = s.store.get_item(items[0]).await.unwrap().unwrap();
        let versions: Vec<_> = s
            .store
            .item_versions(items[0])
            .await
            .unwrap()
            .into_iter()
            .map(|v| (v.number, v.blob_id, v.mail.unwrap().method))
            .collect();
        views.push((summary.index.unwrap(), versions));
    }
    assert_eq!(
        views[0].1, views[1].1,
        "the same versions in the same order"
    );
    assert_eq!(views[0].0.data, views[1].0.data, "the same merged event");

    let methods: Vec<_> = views[0].1.iter().map(|v| v.2.clone().unwrap()).collect();
    assert_eq!(methods, ["REQUEST", "REQUEST", "REPLY", "CANCEL"]);
    let index = &views[0].0;
    assert_eq!(index.version, 4);
    assert_eq!(index.title.as_deref(), Some("Harbour planning"));
    assert_eq!(index.time_zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(
        index.starts_at.map(|t| t.as_millis()),
        Some(1_762_779_600_000),
        "14:00 in Berlin is 13:00 UTC"
    );
    let main = &index.data["components"][0];
    assert_eq!(main["status"], "CANCELLED");
    assert_eq!(
        main["location"], "Room 4",
        "the cancel does not wipe the fields"
    );
    let answers: Vec<_> = main["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["email"].as_str().unwrap(), a["partstat"].clone()))
        .collect();
    assert_eq!(
        answers,
        [
            ("bob@example.org", serde_json::json!("ACCEPTED")),
            ("cleo@example.org", serde_json::json!("NEEDS-ACTION")),
        ]
    );

    let item = events(&s, forward).await[0];
    let linked: Vec<_> = s
        .store
        .version_messages(item)
        .await
        .unwrap()
        .into_iter()
        .map(|v| (v.number, v.message_id))
        .collect();
    assert_eq!(linked, [(1, ids[0]), (2, ids[2]), (3, ids[3]), (4, ids[4])]);
    assert!(s.store.message_versions(ids[1]).await.unwrap().is_empty());

    ingest_all(&s, forward, &MESSAGES).await;
    for id in &ids {
        assert_eq!(s.ingest.record_invitations(*id).await.unwrap(), 0);
    }
    assert_eq!(s.store.item_versions(item).await.unwrap().len(), 4);
    assert_eq!(s.store.version_messages(item).await.unwrap().len(), 4);
}
