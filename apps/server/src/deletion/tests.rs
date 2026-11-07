use std::sync::Arc;

use bytes::Bytes;
use holtmail_ingest::Placement;

use super::*;
use crate::config::Config;
use crate::tasks::{NewTask, Tasks};

const INVITE: &[u8] = include_bytes!("../../../../crates/ingest/tests/fixtures/invite-request.eml");
const UPDATE: &[u8] = include_bytes!("../../../../crates/ingest/tests/fixtures/invite-update.eml");

fn plain(n: u32, subject: &str) -> Bytes {
    Bytes::from(format!(
        "From: Ada <ada@example.com>\r\nTo: bob@example.org\r\nSubject: {subject}\r\n\
         Message-ID: <del{n}@example.com>\r\nDate: Mon, 10 Mar 2025 09:0{n}:00 +0000\r\n\r\nBody {n}\r\n"
    ))
}

struct Fixture {
    services: Services,
    ingest: Ingest,
    archive: ArchiveId,
    tasks: Arc<Tasks>,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str(&format!("data_dir = {:?}", dir.path())).unwrap();
    let services = Services::open(&config).await.unwrap();
    let ingest = services.ingest();
    let archive = services.store.create_archive("Mail").await.unwrap().id;
    Fixture {
        services,
        ingest,
        archive,
        tasks: Arc::new(Tasks::default()),
        _dir: dir,
    }
}

impl Fixture {
    async fn add(&self, raw: Bytes) -> MessageId {
        let id = self
            .ingest
            .ingest(self.archive, raw, &Placement::default())
            .await
            .unwrap()
            .message_id;
        self.ingest.commit().await.unwrap();
        id
    }

    async fn delete(&self, ids: &[MessageId], actor: Option<UserId>) -> Done {
        let task = self.tasks.start(NewTask {
            kind: holtmail_api::TaskKind::DeleteMessages,
            archive: Some(self.archive),
            account: None,
            title: "test".into(),
            cancellable: true,
            job: None,
        });
        delete(
            &self.services,
            &self.ingest,
            self.archive,
            ids,
            actor,
            &task,
        )
        .await
        .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reviews_a_search_and_confirms_it_once() {
    let f = fixture().await;
    let quarterly: Vec<_> = [
        f.add(plain(1, "Quarterly figures")).await,
        f.add(plain(2, "Quarterly figures again")).await,
    ]
    .into();
    let kept = f.add(plain(3, "Lunch")).await;
    let other = f.services.store.create_archive("Other").await.unwrap().id;
    let reviews = Reviews::default();
    let ada = Some(UserId::new());

    let review = reviews
        .review(
            &f.services,
            f.archive,
            DeletionSelection::Query("subject:quarterly".into()),
            ada,
        )
        .await
        .unwrap();
    assert_eq!(review.count, 2);
    assert_eq!(review.query.as_deref(), Some("subject:quarterly"));
    assert_eq!(review.sample.len(), 2);
    assert_eq!(review.sample[0].id, quarterly[1], "not newest first");
    assert!(review.bytes > 0 && review.oldest < review.newest);

    for (selection, archive) in [
        (DeletionSelection::Query("".into()), f.archive),
        (
            DeletionSelection::Query("subject:nothing-like-this".into()),
            f.archive,
        ),
        (DeletionSelection::Messages(vec![kept]), other),
        (
            DeletionSelection::Query("subject:quarterly".into()),
            ArchiveId::new(),
        ),
    ] {
        assert!(matches!(
            reviews.review(&f.services, archive, selection, ada).await,
            Err(MailboxError::Invalid(_))
        ));
    }
    let picked = reviews
        .review(
            &f.services,
            f.archive,
            DeletionSelection::Messages(vec![kept, kept, MessageId::new()]),
            ada,
        )
        .await
        .unwrap();
    assert_eq!(picked.count, 1);

    assert!(
        reviews.confirm(&review.id, 3, ada).is_err(),
        "another count"
    );
    assert!(
        reviews.confirm(&review.id, 2, Some(UserId::new())).is_err(),
        "another user"
    );
    assert!(reviews.confirm("unknown", 2, ada).is_err());
    let confirmed = reviews.confirm(&review.id, 2, ada).unwrap();
    assert_eq!(confirmed.ids, [quarterly[1], quarterly[0]]);
    assert!(
        reviews.confirm(&review.id, 2, ada).is_err(),
        "confirmed twice"
    );

    let done = f.delete(&confirmed.ids, ada).await;
    assert_eq!(done.messages, 2);
    assert!(done.blobs >= 2);
    for id in &quarterly {
        assert!(f.services.store.get_message(*id).await.unwrap().is_none());
    }
    let left = f
        .services
        .index
        .search(
            f.archive,
            &holtmail_query::parse("body").unwrap(),
            SortOrder::Newest,
            0,
            10,
        )
        .await
        .unwrap();
    assert_eq!(left.hits, [kept], "deleted messages are still found");
    let head = f.services.store.ledger_head().await.unwrap().unwrap();
    assert_eq!(head.actor, ada);
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_events_read_only_from_deleted_mail_with_it() {
    let f = fixture().await;
    let request = f.add(Bytes::from_static(INVITE)).await;
    let update = f.add(Bytes::from_static(UPDATE)).await;
    let store = &f.services.store;
    let item = store.message_versions(request).await.unwrap()[0].item_id;
    assert_eq!(store.item_versions(item).await.unwrap().len(), 2);

    f.delete(&[update], None).await;
    let versions = store.item_versions(item).await.unwrap();
    assert_eq!(versions.len(), 1, "the update's version stayed");
    assert_eq!(versions[0].number, 1);
    let summary = store.get_item(item).await.unwrap().unwrap();
    assert_eq!(summary.item.versions, 1);

    f.delete(&[request], None).await;
    assert!(store.get_item(item).await.unwrap().is_none());
    assert!(store.ledger_gaps(10).await.unwrap().is_empty());
    assert!(
        f.ingest
            .ingest(f.archive, Bytes::from_static(INVITE), &Placement::default())
            .await
            .is_err_and(|e| matches!(e, holtmail_ingest::IngestError::Deleted)),
        "the deleted invitation was archived again"
    );
}

fn coded(n: u32) -> Bytes {
    Bytes::from(format!(
        "From: Trips <hello@trips.example.com>\r\nTo: bob@example.org\r\nSubject: Booking {n}\r\n\
         Message-ID: <trip{n}@trips.example.com>\r\nDate: Mon, 10 Mar 2025 09:0{n}:00 +0000\r\n\r\n\
         Your verification code is 4829{n:02}.\r\n"
    ))
}

impl Fixture {
    async fn review(&self, query: &str) -> Result<DeletionReview, MailboxError> {
        Reviews::default()
            .review(
                &self.services,
                self.archive,
                DeletionSelection::Query(query.into()),
                None,
            )
            .await
    }

    async fn drain(&self) {
        let store = &self.services.store;
        let queued = store.reindex_queue(self.archive, None, 100).await.unwrap();
        for (id, _) in &queued {
            self.ingest.reindex(*id).await.unwrap();
        }
        self.ingest.commit().await.unwrap();
        store.dequeue_reindex(&queued).await.unwrap();
    }
}

fn still_reading(result: Result<DeletionReview, MailboxError>) -> bool {
    matches!(result, Err(MailboxError::Conflict(message)) if message == STILL_READING)
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_insight_searches_while_search_is_behind() {
    use holtmail_category::{Topic, TopicSet};
    use holtmail_store::{CategoryOverride, CategoryOverrideId, OverrideScope};

    let f = fixture().await;
    let store = f.services.store.clone();
    let policy = f.services.insights.as_ref().unwrap().policy();
    store
        .set_setting(crate::REINDEX_SETTING, None)
        .await
        .unwrap();
    let trips = f.add(coded(1)).await;
    f.add(plain(2, "Quarterly figures")).await;
    let first = f.review("has:code").await;
    assert!(first.is_ok(), "{:?}", first.err());

    let plain_ingest = Ingest::new(
        f.services.blobs.clone(),
        store.clone(),
        f.services.index.clone(),
    );
    let late = plain_ingest
        .ingest(f.archive, coded(3), &Placement::default())
        .await
        .unwrap()
        .message_id;
    plain_ingest.commit().await.unwrap();
    for query in ["category:travel", "has:code", "is:newsletter", "-has:code"] {
        assert!(
            still_reading(f.review(query).await),
            "{query} reviewed with insights pending"
        );
    }
    assert_eq!(f.review("subject:quarterly").await.unwrap().count, 1);
    assert!(f.ingest.refresh_insights(late).await.unwrap());
    f.ingest.commit().await.unwrap();
    assert!(
        still_reading(f.review("has:code").await),
        "refreshed but still queued"
    );
    f.drain().await;
    assert_eq!(f.review("has:code").await.unwrap().count, 2);

    let (stored, changed) = store
        .put_category_override(
            &CategoryOverride {
                id: CategoryOverrideId::new(),
                archive_id: f.archive,
                scope: OverrideScope::Domain,
                key: "example.com".into(),
                kind: None,
                topics: Some(TopicSet::from(Topic::Travel)),
                created_at: Timestamp::from_secs(10),
                created_by: UserId::new(),
            },
            policy,
        )
        .await
        .unwrap();
    assert!(changed.contains(&trips));
    assert!(still_reading(f.review("category:travel").await));
    assert_eq!(f.review("subject:quarterly").await.unwrap().count, 1);
    f.drain().await;
    assert_eq!(f.review("category:travel").await.unwrap().count, 3);

    store
        .set_setting(crate::REINDEX_SETTING, Some("needed"))
        .await
        .unwrap();
    assert!(still_reading(f.review("category:travel").await));
    assert_eq!(f.review("subject:quarterly").await.unwrap().count, 1);
    store
        .set_setting(crate::REINDEX_SETTING, None)
        .await
        .unwrap();
    assert_eq!(f.review("category:travel").await.unwrap().count, 3);

    store
        .delete_category_override(f.archive, stored.id, policy)
        .await
        .unwrap();
    assert!(still_reading(f.review("category:travel").await));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_insight_searches_while_categories_follow_older_settings() {
    use holtmail_store::{AccountKind, NewAccount};

    let f = fixture().await;
    let store = f.services.store.clone();
    let insights = f.services.insights.as_ref().unwrap();
    store
        .set_setting(crate::REINDEX_SETTING, None)
        .await
        .unwrap();
    f.add(coded(1)).await;
    assert_eq!(f.review("has:code").await.unwrap().count, 1);

    let account = store
        .create_account(NewAccount {
            archive_id: f.archive,
            kind: AccountKind::Imap,
            name: "Mail".into(),
            address: Some("bob@example.org".into()),
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let rederive = || async {
        crate::derive::derive_if_needed(&*store, &insights.deriver, insights.policy())
            .await
            .unwrap();
        f.drain().await;
    };
    for query in ["category:finance", "is:newsletter", "-category:travel"] {
        assert!(
            still_reading(f.review(query).await),
            "{query} reviewed with categories from another own-address set"
        );
    }
    assert_eq!(f.review("subject:booking").await.unwrap().count, 1);
    rederive().await;
    assert_eq!(f.review("has:code").await.unwrap().count, 1);

    store
        .set_trusted_authserv_ids(account, &["mx.example.org".into()])
        .await
        .unwrap();
    assert!(still_reading(f.review("has:code").await));
    rederive().await;
    assert_eq!(f.review("has:code").await.unwrap().count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_insights_off_insight_searches_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str(&format!(
        "data_dir = {:?}\n[insights]\nenabled = false",
        dir.path()
    ))
    .unwrap();
    let services = Services::open(&config).await.unwrap();
    assert!(services.insights.is_none());
    let ingest = services.ingest();
    let archive = services.store.create_archive("Mail").await.unwrap().id;
    ingest
        .ingest(archive, coded(1), &Placement::default())
        .await
        .unwrap();
    ingest.commit().await.unwrap();
    services
        .store
        .set_setting(crate::REINDEX_SETTING, Some("needed"))
        .await
        .unwrap();
    let reviews = Reviews::default();
    for query in ["has:code", "category:travel"] {
        assert!(matches!(
            reviews
                .review(&services, archive, DeletionSelection::Query(query.into()), None)
                .await,
            Err(MailboxError::Invalid(message)) if message == INSIGHTS_OFF
        ));
    }
    assert!(
        reviews
            .review(
                &services,
                archive,
                DeletionSelection::Query("subject:booking".into()),
                None
            )
            .await
            .is_ok()
    );
}
