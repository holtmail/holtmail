use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_collection::testing::FakeCollections;
use holtmail_collection::{
    CollectionError, CollectionKind as RemoteKind, CollectionSource, RemoteItem,
};
use holtmail_core::{CollectionId, ItemChangeId, ItemId, Timestamp};
use holtmail_index::SearchIndex;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{IncomingItem, Ingest};
use holtmail_pim::Format;
use holtmail_store::*;
use holtmail_store_sqlite::SqliteStore;

use crate::{NoRebase, Rebaser, Upstream, Writeback, WritebackConfig, Writers};

const BOOK: &str = "/dav/books/main/";
const START: i64 = 1_800_000_000_000;

#[derive(Default)]
struct Gate {
    closed: AtomicBool,
    no_reply: AtomicBool,
    error: Mutex<Option<CollectionError>>,
    always: AtomicBool,
}

struct FakeWriters {
    server: FakeCollections,
    gate: Arc<Gate>,
}

#[async_trait]
impl Writers for FakeWriters {
    async fn upstream(&self, _: &Collection) -> Result<Option<Upstream>, CollectionError> {
        if self.gate.always.load(Ordering::SeqCst) {
            return Err(CollectionError::Connection("down".into()));
        }
        if let Some(error) = self.gate.error.lock().unwrap().take() {
            return Err(error);
        }
        if self.gate.closed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(Upstream {
            writer: Arc::new(self.server.clone()),
            source: Arc::new(self.server.clone()),
        }))
    }

    async fn may_reply(&self, _: &Collection) -> Result<bool, CollectionError> {
        Ok(!self.gate.no_reply.load(Ordering::SeqCst))
    }
}

struct Marking;

impl Rebaser for Marking {
    fn rebase_reply(&self, _: &ItemChange, _: &[u8], upstream: &RemoteItem) -> Option<Bytes> {
        let text = String::from_utf8_lossy(&upstream.raw).replace("FN:", "FN:Rebased ");
        Some(Bytes::from(text))
    }
}

struct World {
    _dir: tempfile::TempDir,
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
    server: FakeCollections,
    gate: Arc<Gate>,
    clock: Arc<AtomicI64>,
    book: CollectionId,
}

fn card(uid: &str, name: &str) -> Vec<u8> {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nEND:VCARD\r\n").into_bytes()
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let blobs: Arc<dyn BlobStore> =
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
        let ingest = Ingest::new(blobs.clone(), store.clone(), index);
        let archive = store.create_archive("People").await.unwrap().id;
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
        let book = store
            .upsert_collection(NewCollection {
                archive_id: archive,
                account_id: Some(account),
                kind: CollectionKind::Contacts,
                remote_id: BOOK.into(),
                name: "Contacts".into(),
                color: None,
            })
            .await
            .unwrap()
            .id;
        store
            .set_write_mode(book, WriteMode::WriteThrough)
            .await
            .unwrap();
        let server = FakeCollections::new();
        server.add_collection(BOOK, RemoteKind::Contacts, "Contacts", None);
        server.enable_writes();
        World {
            _dir: dir,
            store,
            blobs,
            ingest,
            server,
            gate: Arc::new(Gate::default()),
            clock: Arc::new(AtomicI64::new(START)),
            book,
        }
    }

    fn writeback(&self, rebaser: Arc<dyn Rebaser>) -> Writeback {
        let clock = self.clock.clone();
        Writeback::new(
            self.store.clone(),
            self.blobs.clone(),
            self.ingest.clone(),
            Arc::new(FakeWriters {
                server: self.server.clone(),
                gate: self.gate.clone(),
            }),
            rebaser,
            WritebackConfig::default(),
        )
        .with_clock(Arc::new(move || {
            Timestamp::from_millis(clock.load(Ordering::SeqCst))
        }))
    }

    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.clock.load(Ordering::SeqCst))
    }

    fn advance(&self, by: Duration) {
        self.clock
            .fetch_add(i64::try_from(by.as_millis()).unwrap(), Ordering::SeqCst);
    }

    async fn synced(&self, name: &str, raw: &[u8]) -> ItemId {
        let id = self.server.put_item(BOOK, name, raw);
        let etag = self
            .server
            .list(BOOK)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap()
            .etag;
        self.ingest
            .ingest_item(IncomingItem {
                collection: self.book,
                remote_id: id,
                kind: holtmail_pim::ItemKind::Contact,
                format: Format::VCard,
                raw: Bytes::copy_from_slice(raw),
                etag,
                seen_at: self.now(),
            })
            .await
            .unwrap()
            .item_id
    }

    async fn record(
        &self,
        target: ChangeTarget,
        op: ChangeOp,
        class: ChangeClass,
        raw: Option<&[u8]>,
        base: Option<u32>,
    ) -> ItemChange {
        let content = match raw {
            Some(raw) => Some(ChangeContent {
                blob_id: self.blobs.put(Bytes::copy_from_slice(raw)).await.unwrap(),
                content_type: "text/vcard".into(),
            }),
            None => None,
        };
        let change = NewItemChange {
            target,
            op,
            class,
            content,
            patch: None,
            base_version: base,
            origin: ChangeOrigin::Api,
            user_id: None,
            device: None,
            at: self.now(),
            not_before: None,
            messages: Vec::new(),
        };
        match self
            .store
            .record_change(&change, Expect::Any)
            .await
            .unwrap()
        {
            RecordedChange::Recorded(change) => *change,
            other => panic!("not recorded: {other:?}"),
        }
    }

    async fn edit(&self, item: ItemId, raw: &[u8], class: ChangeClass) -> ItemChange {
        self.record(
            ChangeTarget::Item { item },
            ChangeOp::Update,
            class,
            Some(raw),
            Some(1),
        )
        .await
    }

    async fn change(&self, id: ItemChangeId) -> ItemChange {
        self.store.get_change(id).await.unwrap().unwrap()
    }

    async fn upstream(&self, name: &str) -> Option<Bytes> {
        let id = format!("{BOOK}{name}");
        self.server
            .fetch(BOOK, &[id])
            .await
            .unwrap()
            .pop()
            .map(|i| i.raw)
    }
}

#[tokio::test]
async fn an_edit_reaches_upstream_and_is_recorded_as_a_writeback_version() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w
        .edit(item, &card("u1", "Ada Lovelace"), ChangeClass::Edit)
        .await;
    let writeback = w.writeback(Arc::new(NoRebase));
    assert_eq!(writeback.drain_due().await.unwrap(), 1);

    assert_eq!(w.change(change.id).await.state, ItemChangeState::Applied);
    assert_eq!(
        w.upstream("ada.vcf").await.unwrap(),
        Bytes::from(card("u1", "Ada Lovelace"))
    );
    let versions = w.store.item_versions(item).await.unwrap();
    assert_eq!(versions.len(), 2);
    let latest = versions.last().unwrap();
    assert_eq!(latest.origin, VersionOrigin::Writeback);
    assert_eq!(latest.change_id, Some(change.id));
    let tag = w.server.list(BOOK).await.unwrap()[0].etag.clone();
    assert_eq!(latest.etag, tag, "the version carries upstream's new tag");
    assert_eq!(writeback.drain_due().await.unwrap(), 0, "nothing left");
}

#[tokio::test]
async fn a_create_is_idempotent_by_its_href_and_names_the_new_item() {
    let w = World::new().await;
    let target = ChangeTarget::Href {
        collection: w.book,
        href: "grace.vcf".into(),
    };
    let change = w
        .record(
            target.clone(),
            ChangeOp::Create,
            ChangeClass::Create,
            Some(&card("u2", "Grace")),
            None,
        )
        .await;
    let earlier = holtmail_collection::ItemPatch {
        content_type: "text/vcard".into(),
        raw: Bytes::from(card("u2", "Grace")),
        uid: Some("u2".into()),
        fields: None,
    };
    holtmail_collection::CollectionWriter::put(
        &w.server,
        BOOK,
        None,
        &earlier,
        holtmail_collection::Precondition::Absent,
        "grace.vcf",
    )
    .await
    .unwrap();
    let writeback = w.writeback(Arc::new(NoRebase));
    writeback.drain_due().await.unwrap();

    let settled = w.change(change.id).await;
    assert_eq!(settled.state, ItemChangeState::Applied);
    let item = w
        .store
        .get_item(settled.item_id.expect("the item is filled in"))
        .await
        .unwrap()
        .unwrap()
        .item;
    assert_eq!(item.dav_name.as_deref(), Some("grace.vcf"));
    assert_eq!(item.key.as_deref(), Some("u2"));
    assert_eq!(
        w.server.list(BOOK).await.unwrap().len(),
        1,
        "an attempt that already landed is not created twice"
    );
    assert!(w.upstream(&item.remote_id[BOOK.len()..]).await.is_some());

    let effective = w.store.effective(&target).await.unwrap().unwrap();
    assert!(effective.served);
    assert_eq!(effective.item.unwrap().item.id, item.id);
}

#[tokio::test]
async fn a_delete_removes_the_item_upstream_and_marks_it_removed() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w
        .record(
            ChangeTarget::Item { item },
            ChangeOp::Delete,
            ChangeClass::Delete,
            None,
            Some(1),
        )
        .await;
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();
    assert_eq!(w.change(change.id).await.state, ItemChangeState::Applied);
    assert!(w.upstream("ada.vcf").await.is_none());
    let summary = w.store.get_item(item).await.unwrap().unwrap();
    assert!(summary.item.removed_at.is_some());
}

#[tokio::test]
async fn a_failed_precondition_records_upstream_and_makes_a_conflict() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w
        .edit(item, &card("u1", "Ada Lovelace"), ChangeClass::Edit)
        .await;
    w.server.put_item(BOOK, "ada.vcf", &card("u1", "Ada King"));
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();

    let settled = w.change(change.id).await;
    assert_eq!(settled.state, ItemChangeState::Conflict);
    assert_eq!(settled.reason.as_deref(), Some("upstream_changed"));
    assert_eq!(
        w.upstream("ada.vcf").await.unwrap(),
        Bytes::from(card("u1", "Ada King")),
        "upstream keeps its own edit"
    );
    let versions = w.store.item_versions(item).await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[1].origin, VersionOrigin::Upstream);
    let served = w
        .store
        .effective(&ChangeTarget::Item { item })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        served.change.map(|c| c.id),
        Some(change.id),
        "the edit is still served"
    );
}

#[tokio::test]
async fn an_item_gone_upstream_turns_an_edit_into_a_conflict_and_a_delete_into_applied() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let edit = w.edit(item, &card("u1", "Ada L"), ChangeClass::Edit).await;
    w.server.delete_item(BOOK, "ada.vcf");
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();
    let settled = w.change(edit.id).await;
    assert_eq!(settled.state, ItemChangeState::Conflict);
    assert_eq!(settled.reason.as_deref(), Some("upstream_deleted"));

    let other = w.synced("bob.vcf", &card("u3", "Bob")).await;
    let delete = w
        .record(
            ChangeTarget::Item { item: other },
            ChangeOp::Delete,
            ChangeClass::Delete,
            None,
            Some(1),
        )
        .await;
    w.server.put_item(BOOK, "bob.vcf", &card("u3", "Robert"));
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();
    assert_eq!(
        w.change(delete.id).await.reason.as_deref(),
        Some("upstream_changed")
    );

    let third = w.synced("cy.vcf", &card("u4", "Cy")).await;
    let gone = w
        .record(
            ChangeTarget::Item { item: third },
            ChangeOp::Delete,
            ChangeClass::Delete,
            None,
            Some(1),
        )
        .await;
    w.server.delete_item(BOOK, "cy.vcf");
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();
    assert_eq!(w.change(gone.id).await.state, ItemChangeState::Applied);
}

#[tokio::test]
async fn a_reply_is_rebased_once_after_a_failed_precondition() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let reply = w.edit(item, &card("u1", "Ada"), ChangeClass::Reply).await;
    w.server.put_item(BOOK, "ada.vcf", &card("u1", "Ada King"));
    w.writeback(Arc::new(Marking)).drain_due().await.unwrap();
    assert_eq!(w.change(reply.id).await.state, ItemChangeState::Applied);
    assert_eq!(
        w.upstream("ada.vcf").await.unwrap(),
        Bytes::from(card("u1", "Rebased Ada King"))
    );
    let versions = w.store.item_versions(item).await.unwrap();
    assert_eq!(
        versions.iter().map(|v| v.origin).collect::<Vec<_>>(),
        vec![
            VersionOrigin::Upstream,
            VersionOrigin::Upstream,
            VersionOrigin::Writeback
        ]
    );

    let other = w.synced("bob.vcf", &card("u3", "Bob")).await;
    let obsolete = w.edit(other, &card("u3", "Bob"), ChangeClass::Reply).await;
    w.server.put_item(BOOK, "bob.vcf", &card("u3", "Robert"));
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();
    let settled = w.change(obsolete.id).await;
    assert_eq!(settled.state, ItemChangeState::Conflict);
    assert_eq!(settled.reason.as_deref(), Some("reply_obsolete"));
}

#[tokio::test]
async fn a_refusal_rejects_the_change_and_keeps_it_served() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w.edit(item, &card("u1", "Ada L"), ChangeClass::Edit).await;
    w.server.refuse_writes(Some("read-only address book"));
    w.writeback(Arc::new(NoRebase)).drain_due().await.unwrap();
    let settled = w.change(change.id).await;
    assert_eq!(settled.state, ItemChangeState::Rejected);
    assert_eq!(settled.reason.as_deref(), Some("refused"));
    assert!(settled.state.is_open());
    assert_eq!(
        w.upstream("ada.vcf").await.unwrap(),
        Bytes::from(card("u1", "Ada"))
    );
}

#[tokio::test]
async fn a_rejected_sign_in_pauses_the_account_until_the_backoff_passes() {
    let w = World::new().await;
    let a = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let b = w.synced("bob.vcf", &card("u3", "Bob")).await;
    let first = w.edit(a, &card("u1", "Ada L"), ChangeClass::Edit).await;
    let second = w.edit(b, &card("u3", "Bob B"), ChangeClass::Edit).await;
    w.server.reject_sign_in(true);
    let writeback = w.writeback(Arc::new(NoRebase));
    assert_eq!(writeback.drain_due().await.unwrap(), 1, "one sign-in only");
    let held = w.change(first.id).await;
    assert_eq!(held.state, ItemChangeState::Held);
    assert_eq!(held.last_error.as_deref(), Some("auth"));
    assert_eq!(
        held.next_attempt_at,
        Some(Timestamp::from_millis(START + 30_000))
    );
    assert_eq!(w.change(second.id).await.attempts, 0, "the account waits");

    w.advance(Duration::from_secs(10));
    assert_eq!(writeback.drain_due().await.unwrap(), 0, "still paused");
    assert_eq!(
        writeback.drain_collection(w.book).await.unwrap(),
        0,
        "the pause holds per account"
    );

    w.server.reject_sign_in(false);
    w.advance(Duration::from_secs(30));
    assert_eq!(writeback.drain_due().await.unwrap(), 2);
    assert_eq!(w.change(first.id).await.state, ItemChangeState::Applied);
    assert_eq!(w.change(second.id).await.state, ItemChangeState::Applied);
}

#[tokio::test]
async fn network_errors_back_off_up_to_a_day_and_never_drop_the_change() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w.edit(item, &card("u1", "Ada L"), ChangeClass::Edit).await;
    w.gate.always.store(true, Ordering::SeqCst);
    let writeback = w.writeback(Arc::new(NoRebase));
    let day = Duration::from_secs(24 * 3600);
    for _ in 0..20 {
        assert_eq!(writeback.drain_due().await.unwrap(), 1);
        let held = w.change(change.id).await;
        assert_eq!(held.state, ItemChangeState::Held);
        assert_eq!(held.last_error.as_deref(), Some("connection"));
        let wait = held.next_attempt_at.unwrap().as_millis() - w.now().as_millis();
        assert!(wait > 0 && wait <= 24 * 3600 * 1000, "wait {wait}");
        w.advance(day);
    }
    let held = w.change(change.id).await;
    assert_eq!(
        held.next_attempt_at.unwrap().as_millis() - (w.now().as_millis() - 24 * 3600 * 1000),
        24 * 3600 * 1000,
        "the backoff is capped at a day"
    );
    w.gate.always.store(false, Ordering::SeqCst);
    writeback.drain_due().await.unwrap();
    assert_eq!(w.change(change.id).await.state, ItemChangeState::Applied);
}

#[tokio::test]
async fn a_closed_gate_holds_changes_and_other_errors_are_classified() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w.edit(item, &card("u1", "Ada L"), ChangeClass::Edit).await;
    w.gate.closed.store(true, Ordering::SeqCst);
    let writeback = w.writeback(Arc::new(NoRebase));
    writeback.drain_due().await.unwrap();
    let held = w.change(change.id).await;
    assert_eq!(held.state, ItemChangeState::Held);
    assert_eq!(held.last_error.as_deref(), Some("not_allowed"));

    w.gate.closed.store(false, Ordering::SeqCst);
    *w.gate.error.lock().unwrap() = Some(CollectionError::Gone);
    w.advance(Duration::from_secs(60));
    writeback.drain_due().await.unwrap();
    let rejected = w.change(change.id).await;
    assert_eq!(rejected.state, ItemChangeState::Rejected);
    assert_eq!(rejected.reason.as_deref(), Some("collection_gone"));
}

#[tokio::test]
async fn only_the_head_of_a_supersede_chain_is_sent_in_order() {
    let w = World::new().await;
    let a = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let b = w.synced("bob.vcf", &card("u3", "Bob")).await;
    let first = w.edit(a, &card("u1", "Ada 1"), ChangeClass::Edit).await;
    let other = w.edit(b, &card("u3", "Bob 1"), ChangeClass::Edit).await;
    let second = w
        .record(
            ChangeTarget::Item { item: a },
            ChangeOp::Update,
            ChangeClass::Edit,
            Some(&card("u1", "Ada 2")),
            Some(1),
        )
        .await;
    let personal = w
        .record(
            ChangeTarget::Item { item: b },
            ChangeOp::Update,
            ChangeClass::Personal,
            Some(&card("u3", "Bob 1")),
            Some(1),
        )
        .await;
    let writeback = w.writeback(Arc::new(NoRebase));
    assert_eq!(writeback.drain_due().await.unwrap(), 1);
    assert_eq!(w.change(first.id).await.state, ItemChangeState::Superseded);
    assert_eq!(w.change(second.id).await.state, ItemChangeState::Applied);
    assert_eq!(w.change(other.id).await.state, ItemChangeState::Superseded);
    assert_eq!(
        w.change(personal.id).await.state,
        ItemChangeState::Held,
        "personal changes are never sent"
    );
    assert_eq!(
        w.upstream("ada.vcf").await.unwrap(),
        Bytes::from(card("u1", "Ada 2"))
    );
    assert_eq!(
        w.upstream("bob.vcf").await.unwrap(),
        Bytes::from(card("u3", "Bob"))
    );
}

#[tokio::test]
async fn recover_releases_changes_left_sending() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w.edit(item, &card("u1", "Ada L"), ChangeClass::Edit).await;
    let claimed = w
        .store
        .claim_change(w.book, w.now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, change.id);
    let writeback = w.writeback(Arc::new(NoRebase));
    assert_eq!(writeback.drain_due().await.unwrap(), 0, "one in flight");
    assert_eq!(writeback.recover().await.unwrap(), vec![change.id]);
    assert_eq!(writeback.drain_due().await.unwrap(), 1);
    assert_eq!(w.change(change.id).await.state, ItemChangeState::Applied);
}

struct Merging;

impl Rebaser for Merging {
    fn rebased(&self, _: &ItemChange, mine: &[u8], onto: &[u8], _: &str) -> Option<Bytes> {
        let mine = String::from_utf8_lossy(mine);
        let onto = String::from_utf8_lossy(onto);
        Some(Bytes::from(onto.replace(
            "END:VCARD",
            &format!("NOTE:{}\r\nEND:VCARD", mine.len()),
        )))
    }

    fn rebase_reply(&self, _: &ItemChange, _: &[u8], _: &RemoteItem) -> Option<Bytes> {
        None
    }
}

#[tokio::test]
async fn a_rebased_change_sends_the_rebase_on_the_version_it_was_rebased_onto() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let change = w
        .edit(item, &card("u1", "Ada Lovelace"), ChangeClass::Edit)
        .await;
    w.synced("ada.vcf", &card("u1", "Ada Upstream")).await;
    w.store
        .resolve_change(change.id, &Resolution::Rebased { onto: 2 }, w.now())
        .await
        .unwrap()
        .unwrap();
    let writeback = w.writeback(Arc::new(Merging));
    assert_eq!(writeback.drain_due().await.unwrap(), 1);
    assert_eq!(w.change(change.id).await.state, ItemChangeState::Applied);
    let sent = String::from_utf8(w.upstream("ada.vcf").await.unwrap().to_vec()).unwrap();
    assert!(
        sent.contains("FN:Ada Upstream") && sent.contains("NOTE:"),
        "{sent}"
    );
}

#[tokio::test]
async fn a_reply_waits_while_answering_is_off_and_other_changes_go_on() {
    let w = World::new().await;
    let ada = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let grace = w.synced("grace.vcf", &card("u2", "Grace")).await;
    let reply = w
        .edit(ada, &card("u1", "Ada Replied"), ChangeClass::Reply)
        .await;
    let edit = w
        .edit(grace, &card("u2", "Grace Hopper"), ChangeClass::Edit)
        .await;
    w.gate.no_reply.store(true, Ordering::SeqCst);
    let writeback = w.writeback(Arc::new(NoRebase));
    writeback.drain_due().await.unwrap();
    let waiting = w.change(reply.id).await;
    assert_eq!(waiting.state, ItemChangeState::Held);
    assert_eq!(waiting.last_error.as_deref(), Some("respond_off"));
    assert_eq!(w.change(edit.id).await.state, ItemChangeState::Applied);
    assert_eq!(w.upstream("ada.vcf").await.unwrap(), card("u1", "Ada"));

    w.gate.no_reply.store(false, Ordering::SeqCst);
    w.advance(Duration::from_secs(60));
    writeback.drain_due().await.unwrap();
    assert_eq!(w.change(reply.id).await.state, ItemChangeState::Applied);
    assert_eq!(
        w.upstream("ada.vcf").await.unwrap(),
        card("u1", "Ada Replied")
    );
}

struct Answering(bool);

impl Rebaser for Answering {
    fn rebase_reply(&self, _: &ItemChange, _: &[u8], _: &RemoteItem) -> Option<Bytes> {
        None
    }

    fn respond(&self, _: &ItemChange) -> Option<crate::Respond> {
        Some(crate::Respond {
            recurrence_id: Some("20261208T100000Z".into()),
            answer: holtmail_collection::Partstat::Tentative,
            notify: self.0,
        })
    }
}

#[tokio::test]
async fn a_reply_after_an_imip_answer_responds_without_telling_the_organizer() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let reply = w
        .edit(item, &card("u1", "Ada Answered"), ChangeClass::Reply)
        .await;
    let writeback = w.writeback(Arc::new(Answering(false)));
    assert_eq!(writeback.drain_due().await.unwrap(), 1);
    assert_eq!(w.change(reply.id).await.state, ItemChangeState::Applied);
    assert!(w.server.notices().is_empty());
}

#[tokio::test]
async fn a_reply_through_respond_reaches_the_organizer_once() {
    let w = World::new().await;
    let item = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let reply = w
        .edit(item, &card("u1", "Ada Answered"), ChangeClass::Reply)
        .await;
    let writeback = w.writeback(Arc::new(Answering(true)));
    assert_eq!(writeback.drain_due().await.unwrap(), 1);
    assert_eq!(w.change(reply.id).await.state, ItemChangeState::Applied);
    assert_eq!(
        w.server.notices(),
        vec![(
            format!("{BOOK}ada.vcf"),
            Some("20261208T100000Z".to_owned()),
            holtmail_collection::Partstat::Tentative
        )]
    );
    assert_eq!(w.upstream("ada.vcf").await.unwrap(), card("u1", "Ada"));
}

#[tokio::test]
async fn edits_wait_while_editing_is_off_and_replies_go_on() {
    struct NoEdit(FakeWriters);
    #[async_trait]
    impl Writers for NoEdit {
        async fn upstream(&self, c: &Collection) -> Result<Option<Upstream>, CollectionError> {
            self.0.upstream(c).await
        }
        async fn may_edit(&self, _: &Collection) -> Result<bool, CollectionError> {
            Ok(false)
        }
    }
    let w = World::new().await;
    let ada = w.synced("ada.vcf", &card("u1", "Ada")).await;
    let grace = w.synced("grace.vcf", &card("u2", "Grace")).await;
    let reply = w
        .edit(ada, &card("u1", "Ada Replied"), ChangeClass::Reply)
        .await;
    let edit = w
        .edit(grace, &card("u2", "Grace Hopper"), ChangeClass::Edit)
        .await;
    let writeback = Writeback::new(
        w.store.clone(),
        w.blobs.clone(),
        w.ingest.clone(),
        Arc::new(NoEdit(FakeWriters {
            server: w.server.clone(),
            gate: w.gate.clone(),
        })),
        Arc::new(NoRebase),
        WritebackConfig::default(),
    );
    writeback.drain_due().await.unwrap();
    let waiting = w.change(edit.id).await;
    assert_eq!(waiting.state, ItemChangeState::Held);
    assert_eq!(waiting.last_error.as_deref(), Some("edit_off"));
    assert_eq!(w.change(reply.id).await.state, ItemChangeState::Applied);
}
