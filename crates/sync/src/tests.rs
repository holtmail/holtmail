use std::sync::Arc;

use holtmail_blob_fs::FsBlobStore;
use holtmail_index::{Clause, SearchIndex, SearchQuery, SortOrder};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_source::testing::FakeSource;
use holtmail_store::{AccountKind, FolderRole, MessageQuery, NewAccount};
use holtmail_store_sqlite::SqliteStore;

use super::*;

struct Fixture {
    syncer: Syncer,
    store: Arc<dyn Store>,
    index: Arc<dyn SearchIndex>,
    account: Account,
    server: FakeSource,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
    let ingest = Ingest::new(
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap()),
        store.clone(),
        index.clone(),
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
        .unwrap();
    let server = FakeSource::new();
    server.add_folder("INBOX", Some(FolderRole::Inbox));
    server.add_folder("Sent", Some(FolderRole::Sent));
    server.add_parent("Projects");
    Fixture {
        syncer: Syncer::new(store.clone(), ingest),
        store,
        index,
        account,
        server,
        _dir: dir,
    }
}

fn mail(n: u32) -> Vec<u8> {
    format!("From: ada@example.com\r\nSubject: Message {n}\r\nMessage-ID: <{n}@example.com>\r\n\r\nBody {n}\r\n").into_bytes()
}

impl Fixture {
    async fn sync(&self) -> SyncReport {
        self.sync_with(&SyncOptions {
            batch: 2,
            ..SyncOptions::default()
        })
        .await
    }

    async fn sync_with(&self, options: &SyncOptions) -> SyncReport {
        self.syncer
            .sync_account(&self.account, &self.server, options)
            .await
            .unwrap()
    }

    fn folder<'a>(&self, report: &'a SyncReport, path: &str) -> &'a FolderReport {
        report.folders.iter().find(|f| f.path == path).unwrap()
    }

    async fn count(&self) -> usize {
        self.store
            .list_messages(&MessageQuery {
                archive_id: self.account.archive_id,
                folder_id: None,
                after: None,
                limit: 500,
            })
            .await
            .unwrap()
            .items
            .len()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_new_mail_incrementally() {
    let f = fixture().await;
    for n in 0..5 {
        f.server.deliver("INBOX", &mail(n), &["\\Seen"]);
    }
    f.server.deliver("Sent", &mail(100), &[]);

    let first = f.sync().await;
    assert_eq!(f.folder(&first, "INBOX").new, 5);
    assert_eq!(f.folder(&first, "Sent").new, 1);
    assert!(
        first.folders.iter().all(|r| r.path != "Projects"),
        "parent folder was selected"
    );
    assert_eq!(f.count().await, 6);
    assert!(
        f.server.fetches().iter().all(|b| b.len() <= 2),
        "batches are too large"
    );

    f.server.deliver("INBOX", &mail(5), &[]);
    let fetches_before = f.server.fetches().len();
    let second = f.sync().await;
    assert_eq!(f.folder(&second, "INBOX").new, 1);
    assert_eq!(
        f.server.fetches()[fetches_before..],
        [vec![6]],
        "already archived mail was fetched again"
    );

    let found = f
        .index
        .search(
            f.account.archive_id,
            &SearchQuery {
                clauses: vec![Clause::In("sent".into())],
            },
            SortOrder::Newest,
            0,
            10,
        )
        .await
        .unwrap();
    assert_eq!(found.total, 1, "folder roles are not searchable");
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_messages_that_left_the_server() {
    let f = fixture().await;
    let uid = f.server.deliver("INBOX", &mail(1), &[]);
    f.server.deliver("INBOX", &mail(2), &[]);
    f.sync().await;

    f.server.expunge("INBOX", uid);
    let report = f.sync().await;
    assert_eq!(f.folder(&report, "INBOX").removed, 1);
    assert_eq!(f.count().await, 2, "a message left the archive");
    let inbox = f.store.list_folders(f.account.archive_id).await.unwrap();
    let inbox = inbox.iter().find(|s| s.folder.path == "INBOX").unwrap();
    assert_eq!(
        inbox.messages, 1,
        "the removed message still counts in the folder"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn survives_renumbering_without_duplicates() {
    let f = fixture().await;
    for n in 0..3 {
        f.server.deliver("INBOX", &mail(n), &[]);
    }
    f.sync().await;
    f.server.renumber("INBOX");
    let report = f.sync().await;
    let inbox = f.folder(&report, "INBOX");
    assert!(inbox.renumbered);
    assert_eq!((inbox.new, inbox.known), (0, 3));
    assert_eq!(f.count().await, 3);
    let again = f.sync().await;
    assert_eq!(
        f.folder(&again, "INBOX").known,
        0,
        "renumbered folder is fetched every time"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn moves_past_a_broken_message_and_gives_up_on_it_eventually() {
    let f = fixture().await;
    f.server.deliver("INBOX", &mail(1), &[]);
    let broken = f.server.deliver("INBOX", b"\x00\x01 not mail", &[]);
    f.server.deliver("INBOX", &mail(3), &[]);

    let report = f.sync().await;
    let inbox = f.folder(&report, "INBOX");
    assert_eq!((inbox.new, inbox.failed), (2, 1));

    for _ in 0..10 {
        f.sync().await;
    }
    let folder = f
        .store
        .list_folders(f.account.archive_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.folder.path == "INBOX")
        .unwrap();
    let failures = f.store.failures(folder.folder.id).await.unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].uid, broken);
    assert_eq!(failures[0].attempts, 5, "retries do not stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn honours_folder_rules_and_reports_sign_in_errors() {
    let f = fixture().await;
    f.server.deliver("INBOX", &mail(1), &[]);
    f.server.deliver("Sent", &mail(2), &[]);
    let report = f
        .sync_with(&SyncOptions {
            exclude: vec!["Sent".into()],
            ..SyncOptions::default()
        })
        .await;
    assert_eq!(report.folders.len(), 1);
    assert_eq!(report.new_messages(), 1);

    f.server.reject_sign_in(Some("bad password"));
    let error = f
        .syncer
        .sync_account(&f.account, &f.server, &SyncOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, SyncError::Source(SourceError::Auth(_))));
}
