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
    async fn find(&self, clause: Clause) -> Vec<holtmail_core::MessageId> {
        self.index
            .search(
                self.account.archive_id,
                &SearchQuery {
                    clauses: vec![clause],
                },
                SortOrder::Newest,
                0,
                50,
            )
            .await
            .unwrap()
            .hits
    }

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
                label: None,
                category: None,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_run_for_named_folders_looks_only_at_them() {
    let f = fixture().await;
    f.server.deliver("INBOX", &mail(1), &[]);
    f.server.deliver("Sent", &mail(2), &[]);
    let report = f
        .sync_with(&SyncOptions {
            only: Some(vec!["INBOX".into()]),
            ..SyncOptions::default()
        })
        .await;
    assert_eq!(report.folders.len(), 1);
    assert_eq!(report.folders[0].path, "INBOX");
    assert_eq!(report.new_messages(), 1);
    let report = f
        .sync_with(&SyncOptions {
            only: Some(vec!["Sent".into()]),
            exclude: vec!["Sent".into()],
            ..SyncOptions::default()
        })
        .await;
    assert!(report.folders.is_empty(), "exclude still wins");
    assert_eq!(
        f.sync().await.new_messages(),
        1,
        "Sent was left for a full run"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_gmail_once_from_all_mail_with_labels() {
    let f = fixture().await;
    f.server.enable_condstore();
    f.server
        .add_folder("[Gmail]/All Mail", Some(FolderRole::All));
    f.server.add_folder("[Gmail]/Spam", Some(FolderRole::Junk));
    f.server.add_folder("Work", None);
    for label_folder in ["INBOX", "Sent", "Work"] {
        f.server.mark_label(label_folder);
    }
    f.server.deliver("INBOX", &mail(1), &[]);
    f.server.deliver("Work", &mail(1), &[]);
    let all = f.server.deliver_labeled(
        "[Gmail]/All Mail",
        &mail(1),
        &["\\Seen"],
        &["Inbox", "Work"],
    );
    f.server
        .deliver_labeled("[Gmail]/All Mail", &mail(2), &[], &["Sent"]);
    f.server.deliver("[Gmail]/Spam", &mail(3), &[]);

    let report = f.sync().await;
    let mut paths: Vec<_> = report.folders.iter().map(|r| r.path.as_str()).collect();
    paths.sort();
    assert_eq!(
        paths,
        ["[Gmail]/All Mail", "[Gmail]/Spam"],
        "label folders were downloaded again"
    );
    assert_eq!(f.count().await, 3);
    let labels = f.store.list_labels(f.account.archive_id).await.unwrap();
    let names: Vec<_> = labels
        .iter()
        .map(|l| (l.name.as_str(), l.messages, l.unread))
        .collect();
    assert_eq!(names, [("Inbox", 1, 0), ("Sent", 1, 1), ("Work", 1, 0)]);
    assert_eq!(f.find(Clause::Label("work".into())).await.len(), 1);

    f.server
        .set_labels("[Gmail]/All Mail", all, &["Work", "Done"]);
    let report = f.sync().await;
    assert_eq!(f.folder(&report, "[Gmail]/All Mail").flags_changed, 1);
    assert!(f.find(Clause::Label("inbox".into())).await.is_empty());
    assert_eq!(f.find(Clause::Label("done".into())).await.len(), 1);

    let named = f
        .sync_with(&SyncOptions {
            include: Some(vec!["[Gmail]/All Mail".into(), "Work".into()]),
            ..SyncOptions::default()
        })
        .await;
    assert!(
        named.folders.iter().any(|r| r.path == "Work"),
        "a label folder named in the rules was skipped"
    );
    let without_all = f
        .sync_with(&SyncOptions {
            exclude: vec!["[Gmail]/All Mail".into()],
            ..SyncOptions::default()
        })
        .await;
    assert!(
        without_all.folders.iter().any(|r| r.path == "INBOX"),
        "label folders skipped while all mail is excluded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_in_flag_changes_since_the_last_modseq() {
    let f = fixture().await;
    f.server.enable_condstore();
    let first = f.server.deliver("INBOX", &mail(1), &[]);
    f.server.deliver("INBOX", &mail(2), &[]);
    f.sync().await;
    assert!(
        f.server.flag_reads().is_empty(),
        "flags read on the first sync"
    );
    assert!(f.find(Clause::Flag("\\Seen".into())).await.is_empty());

    let quiet = f.sync().await;
    assert_eq!(f.folder(&quiet, "INBOX").flags_changed, 0);
    assert!(
        f.server.flag_reads().is_empty(),
        "flags read without changes"
    );

    f.server.set_flags("INBOX", first, &["\\Seen", "\\Flagged"]);
    let changed = f.sync().await;
    assert_eq!(f.folder(&changed, "INBOX").flags_changed, 1);
    let reads = f.server.flag_reads();
    assert_eq!(reads.len(), 1);
    assert!(reads[0].1.is_some(), "changes read without CHANGEDSINCE");
    assert_eq!(f.find(Clause::Flag("\\Seen".into())).await.len(), 1);
    let inbox = f.store.list_folders(f.account.archive_id).await.unwrap();
    let inbox = inbox.iter().find(|s| s.folder.path == "INBOX").unwrap();
    assert_eq!(inbox.unread, 1);

    f.sync().await;
    assert_eq!(f.server.flag_reads().len(), 1, "the same change read twice");
}

#[tokio::test(flavor = "multi_thread")]
async fn scans_flags_once_a_day_without_condstore() {
    let f = fixture().await;
    let first = f.server.deliver("INBOX", &mail(1), &[]);
    for n in 2..6 {
        f.server.deliver("INBOX", &mail(n), &[]);
    }
    f.sync().await;
    f.server.set_flags("INBOX", first, &["\\Seen"]);
    let soon = f.sync().await;
    assert_eq!(f.folder(&soon, "INBOX").flags_changed, 0);
    assert!(
        f.server.flag_reads().is_empty(),
        "flags scanned before a day passed"
    );

    let options = SyncOptions {
        flag_scan_interval: std::time::Duration::ZERO,
        flag_batch: 2,
        ..SyncOptions::default()
    };
    let scanned = f.sync_with(&options).await;
    assert_eq!(f.folder(&scanned, "INBOX").flags_changed, 1);
    let inbox: Vec<_> = f
        .server
        .flag_reads()
        .into_iter()
        .filter(|(_, since)| since.is_none())
        .map(|(range, _)| range)
        .collect();
    assert_eq!(inbox, [1..=2, 3..=4, 5..=5], "not read in bounded batches");
    assert_eq!(f.find(Clause::Flag("\\Seen".into())).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_every_flag_once_for_folders_archived_before_flag_checks() {
    let f = fixture().await;
    f.server.enable_condstore();
    let uid = f.server.deliver("INBOX", &mail(1), &[]);
    f.sync().await;
    let inbox = f
        .store
        .list_folders(f.account.archive_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.folder.path == "INBOX")
        .unwrap()
        .folder
        .id;
    let mut cursor = f.store.cursor(inbox).await.unwrap();
    cursor.flags_checked_at = None;
    f.store.set_cursor(inbox, &cursor).await.unwrap();
    f.server.set_flags("INBOX", uid, &["\\Seen"]);

    let report = f.sync().await;
    assert_eq!(f.folder(&report, "INBOX").flags_changed, 1);
    assert_eq!(
        f.server.flag_reads().last().unwrap().1,
        None,
        "an upgraded folder trusted a mod-sequence nobody read changes for"
    );
    assert!(
        f.store
            .cursor(inbox)
            .await
            .unwrap()
            .flags_checked_at
            .is_some()
    );
}

fn dated(n: u32, date: &str) -> Vec<u8> {
    format!("From: ada@example.com\r\nDate: {date}\r\nSubject: Message {n}\r\nMessage-ID: <{n}@example.com>\r\n\r\nBody {n}\r\n").into_bytes()
}

fn days_ago(days: i64) -> Timestamp {
    Timestamp::from_millis(Timestamp::now().as_millis() - days * 24 * 60 * 60 * 1000)
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_young_mail_for_a_later_sync() {
    let f = fixture().await;
    f.server.deliver_received("INBOX", &mail(1), days_ago(40));
    f.server.deliver_received("INBOX", &mail(2), days_ago(35));
    let young = f.server.deliver_received("INBOX", &mail(3), days_ago(2));
    f.server.deliver_received("INBOX", &mail(4), days_ago(50));
    f.server.deliver_received("INBOX", &mail(5), days_ago(1));
    let thirty_days = SyncOptions {
        batch: 2,
        min_age: Some(Duration::from_secs(30 * 24 * 60 * 60)),
        ..SyncOptions::default()
    };

    let first = f.sync_with(&thirty_days).await;
    let inbox = f.folder(&first, "INBOX");
    assert_eq!((inbox.new, inbox.deferred), (2, 3));
    assert_eq!(f.count().await, 2);
    let folder = f
        .store
        .list_folders(f.account.archive_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.folder.path == "INBOX")
        .unwrap()
        .folder;
    assert_eq!(
        f.store.cursor(folder.id).await.unwrap().last_uid,
        Some(young - 1),
        "the cursor moved past a message that was left on the server"
    );
    assert!(
        f.server.fetches().iter().all(|batch| !batch.contains(&5)),
        "the sync went on fetching after the first young message"
    );

    let again = f.sync_with(&thirty_days).await;
    assert_eq!(
        (
            f.folder(&again, "INBOX").new,
            f.folder(&again, "INBOX").deferred
        ),
        (0, 3)
    );

    let one_day = SyncOptions {
        batch: 2,
        min_age: Some(Duration::from_secs(36 * 60 * 60)),
        ..SyncOptions::default()
    };
    let later = f.sync_with(&one_day).await;
    assert_eq!(
        (
            f.folder(&later, "INBOX").new,
            f.folder(&later, "INBOX").deferred
        ),
        (2, 1),
        "mail old enough by now was not picked up"
    );

    let all = f.sync().await;
    assert_eq!(
        (
            f.folder(&all, "INBOX").new,
            f.folder(&all, "INBOX").deferred
        ),
        (1, 0)
    );
    assert_eq!(f.count().await, 5, "a message was lost");
}

#[tokio::test(flavor = "multi_thread")]
async fn judges_age_by_the_date_header_without_a_received_time() {
    let f = fixture().await;
    f.server
        .deliver("INBOX", &dated(1, "Mon, 01 Jan 2024 10:00:00 +0000"), &[]);
    f.server
        .deliver("INBOX", &dated(2, "Fri, 01 Jan 2100 10:00:00 +0000"), &[]);
    f.server.deliver("INBOX", &mail(3), &[]);
    let report = f
        .sync_with(&SyncOptions {
            min_age: Some(Duration::from_secs(24 * 60 * 60)),
            ..SyncOptions::default()
        })
        .await;
    let inbox = f.folder(&report, "INBOX");
    assert_eq!((inbox.new, inbox.deferred), (1, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn finds_new_mail_below_archived_uids_when_uids_do_not_ascend() {
    let f = fixture().await;
    f.server.mark_unordered("INBOX");
    f.server.deliver_at("INBOX", 500, &mail(1), &[]);
    f.server.deliver_at("INBOX", 900, &mail(2), &[]);
    let first = f.sync().await;
    assert_eq!(f.folder(&first, "INBOX").new, 2);

    f.server.deliver_at("INBOX", 700, &mail(3), &["\\Seen"]);
    f.server.deliver_at("INBOX", 100, b"\x00\x01 not mail", &[]);
    let fetches_before = f.server.fetches().len();
    let second = f.sync().await;
    let inbox = f.folder(&second, "INBOX");
    assert_eq!((inbox.new, inbox.failed), (1, 1));
    assert_eq!(
        f.server.fetches()[fetches_before..],
        [vec![100, 700]],
        "archived mail was fetched again or new mail below the watermark was missed"
    );
    assert_eq!(f.count().await, 3);

    for _ in 0..10 {
        f.sync().await;
    }
    let fetches_before = f.server.fetches().len();
    f.sync().await;
    assert!(
        f.server.fetches()[fetches_before..].is_empty(),
        "a message that keeps failing is fetched forever"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skips_unchanged_folders_until_something_is_left_behind() {
    let f = fixture().await;
    f.server.track_changes();
    f.server.mark_unordered("INBOX");
    f.server.deliver_at("INBOX", 500, &mail(1), &[]);
    f.server.deliver("Sent", &mail(2), &[]);
    let first = f.sync().await;
    assert_eq!(f.folder(&first, "INBOX").new, 1);
    assert!(!f.folder(&first, "INBOX").unchanged);

    let listings = f.server.listings();
    let quiet = f.sync().await;
    assert!(f.folder(&quiet, "INBOX").unchanged && f.folder(&quiet, "Sent").unchanged);
    assert_eq!(
        f.server.listings(),
        listings,
        "an unchanged folder was listed"
    );

    f.server.deliver_at("INBOX", 100, b"\x00\x01 not mail", &[]);
    let broken = f.sync().await;
    assert_eq!(f.folder(&broken, "INBOX").failed, 1);
    let retried = f.sync().await;
    assert!(
        !f.folder(&retried, "INBOX").unchanged,
        "a message left to retry was skipped"
    );
    assert_eq!(f.folder(&retried, "INBOX").failed, 1);
    for _ in 0..5 {
        f.sync().await;
    }
    let given_up = f.sync().await;
    assert!(f.folder(&given_up, "INBOX").unchanged);

    f.server
        .deliver_received("Sent", &mail(3), Timestamp::now());
    let wait = SyncOptions {
        batch: 2,
        min_age: Some(Duration::from_secs(24 * 60 * 60)),
        ..SyncOptions::default()
    };
    assert_eq!(f.folder(&f.sync_with(&wait).await, "Sent").deferred, 1);
    let later = f.sync_with(&wait).await;
    assert!(
        !f.folder(&later, "Sent").unchanged,
        "a deferred message was skipped"
    );
    let all = f.sync().await;
    assert_eq!(f.folder(&all, "Sent").new, 1);
    assert!(f.folder(&f.sync().await, "Sent").unchanged);
    assert_eq!(f.count().await, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_due_flag_scan_is_not_skipped_by_an_unchanged_state() {
    let f = fixture().await;
    f.server.track_changes();
    let uid = f.server.deliver("INBOX", &mail(1), &[]);
    f.sync().await;
    f.server.set_flags("INBOX", uid, &["\\Seen"]);
    let between = f.sync().await;
    assert!(!f.folder(&between, "INBOX").unchanged);
    assert_eq!(
        f.folder(&between, "INBOX").flags_changed,
        0,
        "the daily scan is not due yet"
    );
    assert!(f.folder(&f.sync().await, "INBOX").unchanged);
    let daily = SyncOptions {
        flag_scan_interval: Duration::ZERO,
        ..SyncOptions::default()
    };
    let scanned = f.sync_with(&daily).await;
    assert!(!f.folder(&scanned, "INBOX").unchanged);
    assert_eq!(f.folder(&scanned, "INBOX").flags_changed, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn never_brings_back_a_deleted_message() {
    for unordered in [false, true] {
        let f = fixture().await;
        if unordered {
            f.server.mark_unordered("INBOX");
        }
        for n in 0..3 {
            f.server.deliver("INBOX", &mail(n), &[]);
        }
        f.sync().await;
        let page = f
            .store
            .list_messages(&MessageQuery {
                archive_id: f.account.archive_id,
                folder_id: None,
                label: None,
                category: None,
                after: None,
                limit: 10,
            })
            .await
            .unwrap();
        let gone = page.items[0].id;
        f.store
            .delete_messages(f.account.archive_id, &[gone], None)
            .await
            .unwrap();
        let fetched_before = f.server.fetches().len();
        let report = f.sync().await;
        let inbox = f.folder(&report, "INBOX");
        assert_eq!(
            (inbox.new, inbox.known, inbox.deleted),
            (0, 0, 0),
            "{unordered}"
        );
        assert_eq!(
            f.server.fetches().len(),
            fetched_before,
            "asked for the deleted message"
        );
        assert_eq!(f.count().await, 2);

        f.server.renumber("INBOX");
        let report = f.sync().await;
        let inbox = f.folder(&report, "INBOX");
        assert_eq!((inbox.new, inbox.deleted), (0, 1), "{unordered}");
        assert_eq!(f.count().await, 2, "the deleted message came back");
        let fetched_before = f.server.fetches().len();
        let again = f.sync().await;
        assert_eq!(f.folder(&again, "INBOX").deleted, 0);
        if unordered {
            assert_eq!(
                f.server.fetches().len(),
                fetched_before,
                "asked again after renumbering"
            );
        }
    }
}

#[derive(Default)]
struct Arrivals(std::sync::Mutex<Vec<(Option<String>, bool)>>);

#[async_trait::async_trait]
impl holtmail_ingest::Observer for Arrivals {
    async fn added(
        &self,
        _: holtmail_core::ArchiveId,
        _: holtmail_core::MessageId,
        _: &[u8],
        parsed: &holtmail_mime::ParsedMessage,
        placement: &Placement,
    ) {
        self.0
            .lock()
            .unwrap()
            .push((parsed.message_id.clone(), placement.live));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_mail_after_a_finished_sync_is_live() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture().await;
    let arrivals = Arc::new(Arrivals::default());
    let ingest = Ingest::new(
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap()),
        f.store.clone(),
        f.index.clone(),
    )
    .with_observer(arrivals.clone());
    let syncer = Syncer::new(f.store.clone(), ingest);
    f.server.deliver("INBOX", &mail(1), &[]);
    let options = SyncOptions::default();
    syncer
        .sync_account(&f.account, &f.server, &options)
        .await
        .unwrap();
    f.server.deliver("INBOX", &mail(2), &[]);
    f.server.deliver("Sent", &mail(3), &[]);
    syncer
        .sync_account(&f.account, &f.server, &options)
        .await
        .unwrap();
    assert_eq!(
        *arrivals.0.lock().unwrap(),
        [
            (Some("1@example.com".into()), false),
            (Some("2@example.com".into()), true),
            (Some("3@example.com".into()), true),
        ],
        "a folder synced empty before is followed, so its first message is live"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn confirms_held_flag_changes_and_lets_the_server_win() {
    let f = fixture().await;
    let uid = f.server.deliver("INBOX", &mail(1), &[]);
    let other = f.server.deliver("INBOX", &mail(2), &[]);
    f.sync().await;
    let folders = f.store.list_folders(f.account.archive_id).await.unwrap();
    let inbox = folders
        .iter()
        .find(|s| s.folder.path == "INBOX")
        .unwrap()
        .folder
        .id;
    let change = |uid: u64, flag: &str| holtmail_store::NewMailChange {
        account_id: f.account.id,
        folder_id: inbox,
        message_id: None,
        uid: Some(uid),
        op: holtmail_store::MailOp::Flags {
            add: vec![flag.into()],
            remove: Vec::new(),
        },
        origin: holtmail_store::MailChangeOrigin::User,
        user_id: None,
        device_id: None,
        outbox_id: None,
        at: Timestamp::from_millis(Timestamp::now().as_millis() - 60_000),
    };
    let seen = f
        .store
        .enqueue_mail_change(&change(uid, "\\Seen"))
        .await
        .unwrap();
    let flagged = f
        .store
        .enqueue_mail_change(&change(other, "\\Flagged"))
        .await
        .unwrap();
    let sent_at = Timestamp::from_millis(Timestamp::now().as_millis() - 30_000);
    for _ in 0..2 {
        let claimed = f
            .store
            .claim_mail_change(f.account.id, sent_at)
            .await
            .unwrap()
            .unwrap();
        f.store
            .mark_mail_change_sent(claimed.id, None, sent_at)
            .await
            .unwrap();
    }
    f.server.set_flags("INBOX", uid, &["\\Seen"]);

    let report = f.sync().await;
    assert_eq!(f.folder(&report, "INBOX").flags_changed, 1);
    let state = |id| {
        let store = f.store.clone();
        async move { store.get_mail_change(id).await.unwrap().unwrap().state }
    };
    assert_eq!(state(seen.id).await, MailChangeState::Applied);
    assert_eq!(
        state(flagged.id).await,
        MailChangeState::SupersededByServer,
        "the server shows no flag after the change was sent"
    );
    assert_eq!(f.find(Clause::Flag("\\Seen".into())).await.len(), 1);
    let reads = f.server.flag_reads().len();
    f.sync().await;
    assert_eq!(
        f.server.flag_reads().len(),
        reads,
        "nothing left to confirm"
    );
}
