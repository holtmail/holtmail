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
