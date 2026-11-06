use std::sync::Arc;

use holtmail_blob_fs::FsBlobStore;
use holtmail_collection::testing::FakeCollections;
use holtmail_index::SearchIndex;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_store::{AccountKind, ItemKind, ItemOrder, ItemQuery, NewAccount, RemovedFilter};
use holtmail_store_sqlite::SqliteStore;

use super::*;

const BOOK: &str = "/dav/books/main/";
const CAL: &str = "/dav/cals/work/";

struct Fixture {
    syncer: PimSyncer,
    store: Arc<dyn Store>,
    archive: ArchiveId,
    account: AccountId,
    server: FakeCollections,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
    let ingest = Ingest::new(
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap()),
        store.clone(),
        index,
    );
    let archive = store.create_archive("Mail").await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "ada@example.com".into(),
            address: Some("ada@example.com".into()),
            settings: Default::default(),
            sealed_secret: None,
            enabled: true,
        })
        .await
        .unwrap()
        .id;
    let server = FakeCollections::new();
    server.add_collection(BOOK, CollectionKind::Contacts, "Contacts", None);
    server.add_collection(CAL, CollectionKind::Calendar, "Work", Some("#336699"));
    Fixture {
        syncer: PimSyncer::new(store.clone(), ingest),
        store,
        archive,
        account,
        server,
        _dir: dir,
    }
}

fn card(uid: &str, name: &str, phone: &str) -> Vec<u8> {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nTEL:{phone}\r\nEND:VCARD\r\n")
        .into_bytes()
}

fn event(uid: &str, summary: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20260101T090000Z\r\nDTSTART:20260105T090000Z\r\nSUMMARY:{summary}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
    .into_bytes()
}

fn task(uid: &str) -> Vec<u8> {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VTODO\r\nUID:{uid}\r\n\
         DTSTAMP:20260101T090000Z\r\nSUMMARY:Pack\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
    )
    .into_bytes()
}

impl Fixture {
    async fn sync(&self) -> PimReport {
        self.syncer
            .sync(self.archive, self.account, &self.server)
            .await
            .unwrap()
    }

    async fn items(&self, removed: RemovedFilter) -> Vec<(String, ItemKind, u32, bool)> {
        self.store
            .list_items(&ItemQuery {
                archive_id: self.archive,
                kind: None,
                collection_id: None,
                removed,
                order: ItemOrder::Title,
                after: None,
                limit: 100,
            })
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|s| {
                (
                    s.item.remote_id,
                    s.item.kind,
                    s.item.versions,
                    s.item.removed_at.is_some(),
                )
            })
            .collect()
    }

    async fn collection(&self, remote: &str) -> Collection {
        self.store
            .list_collections(self.archive)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.collection)
            .find(|c| c.remote_id == remote)
            .unwrap()
    }
}

#[tokio::test]
async fn archives_versions_and_removals_and_never_deletes() {
    let f = fixture().await;
    f.server
        .put_item(BOOK, "ada.vcf", &card("ada", "Ada Example", "+1 555 0100"));
    f.server
        .put_item(BOOK, "bea.vcf", &card("bea", "Bea Example", "+1 555 0101"));
    f.server
        .put_item(CAL, "standup.ics", &event("standup", "Standup"));
    f.server.put_item(CAL, "pack.ics", &task("pack"));

    let first = f.sync().await;
    assert_eq!(first.collections.len(), 2);
    assert!(
        first
            .collections
            .iter()
            .all(|c| c.created && c.mode == ChangeMode::Full)
    );
    assert_eq!(first.new_items(), 4);
    let book = f.collection(BOOK).await;
    assert_eq!(book.name, "Contacts");
    assert!(book.sync_token.is_some() && book.ctag.is_some() && book.synced_at.is_some());
    let cal = f.collection(CAL).await;
    assert_eq!(cal.color.as_deref(), Some("#336699"));
    let kinds: Vec<_> = f
        .items(RemovedFilter::Hide)
        .await
        .into_iter()
        .map(|(id, kind, _, _)| (id, kind))
        .collect();
    assert!(kinds.contains(&(format!("{CAL}pack.ics"), ItemKind::Task)));
    assert!(kinds.contains(&(format!("{CAL}standup.ics"), ItemKind::Event)));

    let quiet = f.sync().await;
    assert!(
        quiet
            .collections
            .iter()
            .all(|c| c.mode == ChangeMode::Unchanged)
    );
    assert_eq!(
        f.server.fetches().len(),
        2,
        "nothing fetched when the tags stay"
    );

    f.server
        .put_item(BOOK, "ada.vcf", &card("ada", "Ada Example", "+1 555 0177"));
    f.server.delete_item(BOOK, "bea.vcf");
    f.server
        .add_collection(CAL, CollectionKind::Calendar, "Work and life", None);
    let second = f.sync().await;
    let book_report = &second.collections[0];
    assert_eq!(book_report.mode, ChangeMode::SinceToken);
    assert_eq!((book_report.changed, book_report.removed), (1, 1));
    assert_eq!(
        f.server.fetches().last().unwrap(),
        &[format!("{BOOK}ada.vcf")]
    );
    assert_eq!(f.collection(CAL).await.name, "Work and life");
    let items = f.items(RemovedFilter::Include).await;
    let ada = items.iter().find(|i| i.0.ends_with("ada.vcf")).unwrap();
    assert_eq!((ada.2, ada.3), (2, false), "a second version");
    let bea = items.iter().find(|i| i.0.ends_with("bea.vcf")).unwrap();
    assert_eq!((bea.2, bea.3), (1, true), "marked removed, kept");

    f.server.remove_collection(CAL);
    let third = f.sync().await;
    assert_eq!(third.removed_collections, 1);
    let cal = f.collection(CAL).await;
    assert!(cal.removed_at.is_some());
    assert_eq!(
        f.items(RemovedFilter::Only).await.len(),
        3,
        "the calendar's items are marked removed with it"
    );
}

#[tokio::test]
async fn an_expired_token_lists_everything_and_diffs_by_tag() {
    let f = fixture().await;
    f.server
        .put_item(BOOK, "ada.vcf", &card("ada", "Ada Example", "+1 555 0100"));
    f.server
        .put_item(BOOK, "bea.vcf", &card("bea", "Bea Example", "+1 555 0101"));
    f.sync().await;
    let fetches = f.server.fetches().len();

    f.server.expire();
    f.server
        .put_item(BOOK, "bea.vcf", &card("bea", "Bea Example", "+1 555 0199"));
    f.server.delete_item(BOOK, "ada.vcf");
    f.server.put_item(
        BOOK,
        "cleo.vcf",
        &card("cleo", "Cleo Example", "+1 555 0102"),
    );
    let report = f.sync().await;
    let book = &report.collections[0];
    assert_eq!(book.mode, ChangeMode::TokenExpired);
    assert_eq!((book.new, book.changed, book.removed), (1, 1, 1));
    let asked: Vec<_> = f.server.fetches()[fetches..].concat();
    assert_eq!(asked.len(), 2, "only items whose tag changed: {asked:?}");
    assert!(
        f.collection(BOOK).await.sync_token.is_some(),
        "a fresh token"
    );
}

#[tokio::test]
async fn servers_without_changes_are_listed_each_time() {
    let f = fixture().await;
    f.server.without_changes();
    f.server
        .put_item(BOOK, "ada.vcf", &card("ada", "Ada Example", "+1 555 0100"));
    let first = f.sync().await;
    assert_eq!(first.collections[0].mode, ChangeMode::Listing);
    assert_eq!(first.new_items(), 1);
    assert_eq!(f.collection(BOOK).await.sync_token, None);
    f.server.delete_item(BOOK, "ada.vcf");
    let second = f.sync().await;
    assert_eq!(second.collections[0].mode, ChangeMode::Listing);
    assert_eq!(second.removed_items(), 1);
    f.server
        .put_item(BOOK, "ada.vcf", &card("ada", "Ada Example", "+1 555 0100"));
    let third = f.sync().await;
    assert_eq!(third.changed_items(), 1, "the same card back is restored");
    assert_eq!(f.items(RemovedFilter::Only).await.len(), 0);
}

struct Withholding<'a> {
    inner: &'a FakeCollections,
    withheld: std::sync::Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl CollectionSource for Withholding<'_> {
    async fn collections(
        &self,
    ) -> Result<Vec<holtmail_collection::RemoteCollection>, CollectionError> {
        self.inner.collections().await
    }

    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<holtmail_collection::Changes, CollectionError> {
        self.inner.changes(collection, token).await
    }

    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<holtmail_collection::RemoteItem>, CollectionError> {
        let withheld = self.withheld.lock().unwrap().clone();
        Ok(self
            .inner
            .fetch(collection, ids)
            .await?
            .into_iter()
            .filter(|i| Some(&i.id) != withheld.as_ref())
            .collect())
    }

    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError> {
        self.inner.list(collection).await
    }
}

#[tokio::test]
async fn items_listed_but_not_sent_fail_and_are_asked_again() {
    let f = fixture().await;
    f.server
        .put_item(BOOK, "ada.vcf", &card("ada", "Ada Example", "+1 555 0100"));
    let bea = f
        .server
        .put_item(BOOK, "bea.vcf", &card("bea", "Bea Example", "+1 555 0101"));
    let source = Withholding {
        inner: &f.server,
        withheld: std::sync::Mutex::new(Some(bea)),
    };
    let first = f.syncer.sync(f.archive, f.account, &source).await.unwrap();
    let book = &first.collections[0];
    assert_eq!((book.new, book.failed), (1, 1));
    assert_eq!(book.error, None, "the other items are stored");
    assert_eq!(
        f.collection(BOOK).await.sync_token,
        None,
        "the token is not kept past an item the server did not send"
    );

    *source.withheld.lock().unwrap() = None;
    let second = f.syncer.sync(f.archive, f.account, &source).await.unwrap();
    let book = &second.collections[0];
    assert_eq!(book.mode, ChangeMode::Full);
    assert_eq!((book.new, book.failed), (1, 0));
    assert!(f.collection(BOOK).await.sync_token.is_some());
    assert_eq!(f.items(RemovedFilter::Hide).await.len(), 2);
}

#[tokio::test]
async fn refused_sign_in_stops_before_anything_changes() {
    let f = fixture().await;
    f.server.reject_sign_in(true);
    assert!(matches!(
        f.syncer.sync(f.archive, f.account, &f.server).await,
        Err(PimSyncError::Source(CollectionError::Auth(_)))
    ));
    assert!(
        f.store
            .list_collections(f.archive)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn json_items_keep_their_media_type_and_read_by_it() {
    use holtmail_collection::media;
    let f = fixture().await;
    f.server.set_media(BOOK, media::JSCONTACT);
    f.server.set_media(CAL, media::GRAPH_EVENT);
    let card = br#"{"@type":"Card","version":"1.0","uid":"bea-1","name":{"full":"Bea Example"},"emails":{"e":{"address":"bea@example.com"}}}"#;
    let event = br#"{"id":"AAMk1=","changeKey":"1","iCalUId":"review-1","subject":"Lamp review","type":"singleInstance","start":{"dateTime":"2026-01-05T09:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-01-05T10:00:00.0000000","timeZone":"UTC"},"originalStartTimeZone":"Europe/Berlin","isAllDay":false}"#;
    let card_id = f.server.put_item(BOOK, "bea", card);
    let event_id = f.server.put_item(CAL, "review", event);
    let report = f.sync().await;
    assert_eq!(report.new_items(), 2);
    let items = f.items(RemovedFilter::Hide).await;
    assert!(items.contains(&(card_id.clone(), ItemKind::Contact, 1, false)));
    assert!(items.contains(&(event_id.clone(), ItemKind::Event, 1, false)));
    let all = f
        .store
        .list_items(&ItemQuery {
            archive_id: f.archive,
            kind: None,
            collection_id: None,
            removed: RemovedFilter::Hide,
            order: ItemOrder::Title,
            after: None,
            limit: 100,
        })
        .await
        .unwrap()
        .items;
    for summary in &all {
        let versions = f.store.item_versions(summary.item.id).await.unwrap();
        let index = summary.index.as_ref().expect("normalized");
        assert!(index.readable, "{} reads", summary.item.remote_id);
        if summary.item.remote_id == event_id {
            assert_eq!(versions[0].content_type, media::GRAPH_EVENT);
            assert_eq!(index.title.as_deref(), Some("Lamp review"));
            assert_eq!(index.uid.as_deref(), Some("review-1"));
            assert_eq!(index.time_zone.as_deref(), Some("Europe/Berlin"));
        } else {
            assert_eq!(versions[0].content_type, media::JSCONTACT);
            assert_eq!(index.title.as_deref(), Some("Bea Example"));
            assert_eq!(index.emails, vec!["bea@example.com".to_owned()]);
        }
    }
}

#[test]
fn the_contract_media_types_are_formats() {
    use holtmail_collection::media;
    for (content_type, contact) in [
        (media::VCARD, true),
        (media::ICALENDAR, false),
        (media::GRAPH_CONTACT, true),
        (media::GRAPH_EVENT, false),
        (media::JSCONTACT, true),
        (media::JSCALENDAR, false),
    ] {
        let format = holtmail_pim::Format::from_content_type(content_type).expect("a format");
        assert_eq!(format.is_contact(), contact, "{content_type}");
        assert_eq!(
            media::kind_of(content_type),
            Some(if contact {
                CollectionKind::Contacts
            } else {
                CollectionKind::Calendar
            })
        );
    }
}
