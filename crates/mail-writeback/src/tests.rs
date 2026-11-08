use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::{BlobId, BlobStore};
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{AccountId, ArchiveId, FolderId, MessageId, Timestamp};
use holtmail_source::testing::{FakeSource, MailboxProbe};
use holtmail_source::{Flag, MailboxWriteCaps, MailboxWriter, SourceError};
use holtmail_store::*;
use holtmail_store_sqlite::SqliteStore;
use serde_json::json;

use crate::{FlagObservation, Writeback, WritebackConfig, Writers};

const START: i64 = 1_800_000_000_000;

#[derive(Default)]
struct FakeWriters {
    writers: Mutex<HashMap<AccountId, Arc<dyn MailboxWriter>>>,
    error: Mutex<Option<SourceError>>,
}

#[async_trait]
impl Writers for FakeWriters {
    async fn writer(
        &self,
        account: AccountId,
        _: &MailOp,
    ) -> Result<Option<Arc<dyn MailboxWriter>>, SourceError> {
        if let Some(error) = self.error.lock().unwrap().take() {
            return Err(error);
        }
        Ok(self.writers.lock().unwrap().get(&account).cloned())
    }
}

struct World {
    _dir: tempfile::TempDir,
    store: Arc<SqliteStore>,
    blobs: Arc<FsBlobStore>,
    server: FakeSource,
    writers: Arc<FakeWriters>,
    writeback: Writeback,
    clock: Arc<AtomicI64>,
    archive: ArchiveId,
    account: AccountId,
    inbox: FolderId,
    archive_folder: FolderId,
    sent: FolderId,
}

fn raw(n: u32) -> Vec<u8> {
    format!(
        "From: Ada <ada@example.com>\r\nTo: bob@example.org\r\nSubject: Note {n}\r\nMessage-ID: <w{n}@example.com>\r\n\r\nBody {n}\r\n"
    )
    .into_bytes()
}

fn names(flags: &[&str]) -> Vec<String> {
    flags.iter().map(|f| f.to_string()).collect()
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let archive = store.create_archive("Writing").await.unwrap().id;
        let account = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Import,
                name: "Ada".into(),
                address: Some("ada@example.com".into()),
                settings: json!({}),
                sealed_secret: None,
                enabled: false,
            })
            .await
            .unwrap()
            .id;
        let folder = |path: &'static str, role| {
            let store = store.clone();
            async move { store.upsert_folder(account, path, role).await.unwrap().id }
        };
        let inbox = folder("INBOX", Some(FolderRole::Inbox)).await;
        let archive_folder = folder("Archive", Some(FolderRole::Archive)).await;
        let sent = folder("Sent", Some(FolderRole::Sent)).await;
        let server = FakeSource::new();
        server.add_folder("INBOX", Some(FolderRole::Inbox));
        server.add_folder("Archive", Some(FolderRole::Archive));
        server.add_folder("Sent", Some(FolderRole::Sent));
        let writers = Arc::new(FakeWriters::default());
        writers
            .writers
            .lock()
            .unwrap()
            .insert(account, Arc::new(server.writer()));
        let clock = Arc::new(AtomicI64::new(START));
        let ticks = clock.clone();
        let writeback = Writeback::new(
            store.clone(),
            blobs.clone(),
            writers.clone(),
            WritebackConfig {
                first_retry: Duration::from_secs(60),
                max_retry: Duration::from_secs(600),
                retry_for: Duration::from_secs(3600),
                confirm_within: Duration::from_secs(300),
                ..WritebackConfig::default()
            },
        )
        .with_clock(Arc::new(move || {
            Timestamp::from_millis(ticks.load(Ordering::SeqCst))
        }));
        Self {
            _dir: dir,
            store,
            blobs,
            server,
            writers,
            writeback,
            clock,
            archive,
            account,
            inbox,
            archive_folder,
            sent,
        }
    }

    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.clock.load(Ordering::SeqCst))
    }

    fn advance(&self, by: Duration) {
        self.clock
            .fetch_add(by.as_millis() as i64, Ordering::SeqCst);
    }

    async fn message(&self, n: u32) -> MessageId {
        self.store
            .insert_message(holtmail_store::testing::message(self.archive, n, 1000))
            .await
            .unwrap()
            .message_id
    }

    async fn enqueue(
        &self,
        message: Option<MessageId>,
        uid: Option<u64>,
        op: MailOp,
    ) -> MailChange {
        self.enqueue_in(self.inbox, message, uid, op).await
    }

    async fn enqueue_in(
        &self,
        folder: FolderId,
        message: Option<MessageId>,
        uid: Option<u64>,
        op: MailOp,
    ) -> MailChange {
        self.store
            .enqueue_mail_change(&NewMailChange {
                account_id: self.account,
                folder_id: folder,
                message_id: message,
                uid,
                op,
                origin: MailChangeOrigin::User,
                user_id: None,
                device_id: None,
                outbox_id: None,
                at: self.now(),
            })
            .await
            .unwrap()
    }

    async fn get(&self, change: &MailChange) -> MailChange {
        self.store
            .get_mail_change(change.id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn flags_on_server(&self, path: &str, uid: u64) -> Vec<String> {
        self.server
            .messages(path)
            .await
            .into_iter()
            .find(|m| m.uid == uid)
            .map(|m| m.flags)
            .unwrap_or_default()
    }
}

fn add(flags: &[&str]) -> MailOp {
    MailOp::Flags {
        add: names(flags),
        remove: Vec::new(),
    }
}

#[tokio::test]
async fn flags_wait_for_sync_and_the_server_wins() {
    let w = World::new().await;
    let one = w.server.deliver("INBOX", &raw(1), &[]);
    let two = w.server.deliver("INBOX", &raw(2), &[]);
    let seen = w
        .enqueue(Some(w.message(1).await), Some(one), add(&["\\Seen"]))
        .await;
    let flagged = w
        .enqueue(Some(w.message(2).await), Some(two), add(&["\\Flagged"]))
        .await;

    assert_eq!(w.writeback.drain_account(w.account).await.unwrap(), 2);
    assert_eq!(w.flags_on_server("INBOX", one).await, names(&["\\Seen"]));
    let sent = w.get(&seen).await;
    assert_eq!(sent.state, MailChangeState::Sending);
    assert!(sent.sent_at.is_some());

    let before = Timestamp::from_millis(START - 1);
    let stale = w
        .writeback
        .confirm(&[FlagObservation {
            folder: w.inbox,
            uid: two,
            flags: Vec::new(),
            labels: None,
            read_at: before,
        }])
        .await
        .unwrap();
    assert!(stale.applied.is_empty() && stale.superseded.is_empty());

    w.advance(Duration::from_secs(1));
    let confirmed = w
        .writeback
        .confirm(&[
            FlagObservation {
                folder: w.inbox,
                uid: one,
                flags: names(&["\\Seen"]),
                labels: None,
                read_at: w.now(),
            },
            FlagObservation {
                folder: w.inbox,
                uid: two,
                flags: Vec::new(),
                labels: None,
                read_at: w.now(),
            },
        ])
        .await
        .unwrap();
    assert_eq!(confirmed.applied, vec![seen.id]);
    assert_eq!(confirmed.superseded, vec![flagged.id]);
    assert_eq!(w.get(&seen).await.state, MailChangeState::Applied);
    assert_eq!(
        w.get(&flagged).await.state,
        MailChangeState::SupersededByServer
    );
}

#[tokio::test]
async fn a_message_changes_in_order_one_at_a_time() {
    let w = World::new().await;
    let uid = w.server.deliver("INBOX", &raw(1), &[]);
    let other = w.server.deliver("INBOX", &raw(2), &[]);
    let message = w.message(1).await;
    let flag = w
        .enqueue(Some(message), Some(uid), add(&["\\Flagged"]))
        .await;
    let moved = w
        .enqueue(
            Some(message),
            Some(uid),
            MailOp::Move {
                target: w.archive_folder,
            },
        )
        .await;
    let unrelated = w
        .enqueue(Some(w.message(2).await), Some(other), add(&["\\Seen"]))
        .await;

    assert_eq!(w.writeback.drain_due().await.unwrap(), 2);
    assert_eq!(w.get(&moved).await.state, MailChangeState::Held);
    assert_eq!(w.get(&unrelated).await.state, MailChangeState::Sending);
    assert!(w.server.messages("Archive").await.is_empty());

    w.advance(Duration::from_secs(1));
    w.writeback
        .confirm(&[FlagObservation {
            folder: w.inbox,
            uid,
            flags: names(&["\\Flagged"]),
            labels: None,
            read_at: w.now(),
        }])
        .await
        .unwrap();
    assert_eq!(w.get(&flag).await.state, MailChangeState::Applied);

    assert_eq!(w.writeback.drain_due().await.unwrap(), 1);
    let moved = w.get(&moved).await;
    assert_eq!(moved.state, MailChangeState::Applied);
    let in_archive = w.server.messages("Archive").await;
    assert_eq!(in_archive.len(), 1);
    assert_eq!(moved.result_uid, Some(in_archive[0].uid));
    assert_eq!(in_archive[0].flags, names(&["\\Flagged"]));
    assert_eq!(w.server.messages("INBOX").await.len(), 1);
}

#[tokio::test]
async fn temporary_failures_back_off_then_give_up() {
    let w = World::new().await;
    let uid = w.server.deliver("INBOX", &raw(1), &[]);
    let change = w
        .enqueue(None, Some(uid), MailOp::Delete { expunge: true })
        .await;
    w.server.fail_writes(Some("down"));

    assert_eq!(w.writeback.drain_account(w.account).await.unwrap(), 1);
    let held = w.get(&change).await;
    assert_eq!(held.state, MailChangeState::Held);
    assert_eq!(held.last_error.as_deref(), Some("connection"));
    assert_eq!(
        held.next_attempt_at,
        Some(Timestamp::from_millis(START + 60_000))
    );
    assert_eq!(w.writeback.drain_account(w.account).await.unwrap(), 0);

    w.advance(Duration::from_secs(60));
    w.writeback.drain_account(w.account).await.unwrap();
    let again = w.get(&change).await;
    assert_eq!(again.attempts, 2);
    assert_eq!(
        again.next_attempt_at,
        Some(Timestamp::from_millis(START + 60_000 + 120_000))
    );

    w.server.fail_writes(None);
    w.advance(Duration::from_secs(120));
    w.writeback.drain_account(w.account).await.unwrap();
    assert_eq!(w.get(&change).await.state, MailChangeState::Applied);
    assert!(w.server.messages("INBOX").await.is_empty());

    let late = w
        .enqueue(None, Some(99), MailOp::Delete { expunge: true })
        .await;
    w.writers
        .error
        .lock()
        .unwrap()
        .replace(SourceError::Auth("rejected".into()));
    w.writeback.drain_account(w.account).await.unwrap();
    assert_eq!(w.get(&late).await.last_error.as_deref(), Some("auth"));
    w.server.fail_writes(Some("down"));
    w.advance(Duration::from_secs(3600));
    w.writeback.drain_account(w.account).await.unwrap();
    let failed = w.get(&late).await;
    assert_eq!(failed.state, MailChangeState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("connection"));
}

#[tokio::test]
async fn a_failed_sign_in_pauses_the_whole_account() {
    let w = World::new().await;
    let mut changes = Vec::new();
    for n in 1..=5 {
        let uid = w.server.deliver("INBOX", &raw(n), &[]);
        changes.push(
            w.enqueue(Some(w.message(n).await), Some(uid), add(&["\\Seen"]))
                .await,
        );
    }
    w.server.fail_writes(Some("down"));
    assert_eq!(w.writeback.drain_due().await.unwrap(), 1);
    assert_eq!(w.server.writes(), 1, "one attempt, not one per change");
    assert_eq!(w.get(&changes[1]).await.attempts, 0);
    assert_eq!(w.writeback.drain_due().await.unwrap(), 0);
    w.advance(Duration::from_secs(30));
    assert_eq!(w.writeback.drain_account(w.account).await.unwrap(), 0);

    w.advance(Duration::from_secs(30));
    assert_eq!(w.writeback.drain_due().await.unwrap(), 1);
    w.advance(Duration::from_secs(60));
    assert_eq!(
        w.writeback.drain_due().await.unwrap(),
        0,
        "the pause doubles"
    );

    w.server.fail_writes(None);
    w.advance(Duration::from_secs(60));
    assert_eq!(w.writeback.drain_due().await.unwrap(), 5);
    for change in &changes {
        assert_eq!(w.get(change).await.state, MailChangeState::Sending);
    }
}

#[tokio::test]
async fn later_changes_follow_an_applied_move() {
    let w = World::new().await;
    let uid = w.server.deliver("INBOX", &raw(1), &[]);
    let message = w.message(1).await;
    let to_archive = MailOp::Move {
        target: w.archive_folder,
    };
    let moved = w
        .enqueue(Some(message), Some(uid), to_archive.clone())
        .await;
    let flag = w
        .enqueue(Some(message), Some(uid), add(&["\\Flagged"]))
        .await;
    assert_eq!(w.writeback.drain_account(w.account).await.unwrap(), 2);
    let new_uid = w.get(&moved).await.result_uid.unwrap();
    let flag = w.get(&flag).await;
    assert_eq!(flag.state, MailChangeState::Sending);
    assert_eq!(flag.new.folder_id, w.archive_folder);
    assert_eq!(flag.new.uid, Some(new_uid));
    assert_eq!(
        w.flags_on_server("Archive", new_uid).await,
        names(&["\\Flagged"])
    );

    w.advance(Duration::from_secs(1));
    w.writeback
        .confirm(&[FlagObservation {
            folder: w.archive_folder,
            uid: new_uid,
            flags: names(&["\\Flagged"]),
            labels: None,
            read_at: w.now(),
        }])
        .await
        .unwrap();
    assert_eq!(w.get(&flag).await.state, MailChangeState::Applied);

    let writes = w.server.writes();
    let again = w.enqueue(Some(message), Some(uid), to_archive).await;
    w.writeback.drain_account(w.account).await.unwrap();
    let again = w.get(&again).await;
    assert_eq!(again.state, MailChangeState::Applied);
    assert_eq!(again.result_uid, Some(new_uid));
    assert_eq!(w.server.writes(), writes, "already there: nothing is sent");
    assert_eq!(w.server.messages("Archive").await.len(), 1);
}

#[tokio::test]
async fn refusals_fail_at_once() {
    let w = World::new().await;
    w.server.set_write_caps(MailboxWriteCaps {
        flags: vec![Flag::Seen, Flag::Flagged],
        keywords: false,
        conditional: false,
        labels: false,
        move_uids: false,
        append_uid: false,
        mark_deleted: false,
    });
    let uid = w.server.deliver("INBOX", &raw(1), &[]);
    let keyword = w.enqueue(None, Some(uid), add(&["$Junk"])).await;
    let gone = w.enqueue(None, Some(uid + 10), add(&["\\Seen"])).await;
    let nowhere = w
        .store
        .upsert_folder(w.account, "Projects", None)
        .await
        .unwrap()
        .id;
    let missing = w.enqueue_in(nowhere, None, Some(1), add(&["\\Seen"])).await;

    assert_eq!(w.writeback.drain_account(w.account).await.unwrap(), 3);
    for (change, code) in [
        (&keyword, "unsupported"),
        (&gone, "gone"),
        (&missing, "missing_folder"),
    ] {
        let change = w.get(change).await;
        assert_eq!(change.state, MailChangeState::Failed);
        assert_eq!(change.last_error.as_deref(), Some(code));
        assert_eq!(change.attempts, 1);
    }
    assert!(w.flags_on_server("INBOX", uid).await.is_empty());

    w.writers.writers.lock().unwrap().clear();
    let off = w.enqueue(None, Some(uid), add(&["\\Seen"])).await;
    let writes = w.server.writes();
    w.writeback.drain_account(w.account).await.unwrap();
    let off = w.get(&off).await;
    assert_eq!(off.state, MailChangeState::Failed);
    assert_eq!(off.last_error.as_deref(), Some("not_allowed"));
    assert_eq!(w.server.writes(), writes);
}

#[tokio::test]
async fn appends_stored_bytes() {
    let w = World::new().await;
    let bytes = Bytes::from(raw(7));
    let blob_id = w.blobs.put(bytes.clone()).await.unwrap();
    let change = w
        .enqueue_in(
            w.sent,
            None,
            None,
            MailOp::Append {
                blob_id,
                flags: names(&["\\Seen"]),
                date: Timestamp::from_secs(1_790_000_000),
            },
        )
        .await;
    let lost = w
        .enqueue_in(
            w.sent,
            None,
            None,
            MailOp::Append {
                blob_id: BlobId::of(b"never stored"),
                flags: Vec::new(),
                date: Timestamp::from_secs(1_790_000_000),
            },
        )
        .await;

    w.writeback.drain_account(w.account).await.unwrap();
    let change = w.get(&change).await;
    assert_eq!(change.state, MailChangeState::Applied);
    let sent = w.server.messages("Sent").await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].raw, bytes);
    assert_eq!(sent[0].flags, names(&["\\Seen"]));
    assert_eq!(change.result_uid, Some(sent[0].uid));
    let lost = w.get(&lost).await;
    assert_eq!(lost.state, MailChangeState::Failed);
    assert_eq!(lost.last_error.as_deref(), Some("missing_bytes"));
}

#[tokio::test]
async fn unconfirmed_changes_settle_and_interrupted_ones_go_again() {
    let w = World::new().await;
    let uid = w.server.deliver("INBOX", &raw(1), &[]);
    let change = w.enqueue(None, Some(uid), add(&["\\Seen"])).await;
    w.writeback.drain_account(w.account).await.unwrap();

    w.advance(Duration::from_secs(299));
    assert_eq!(w.writeback.settle_all().await.unwrap(), 0);
    w.advance(Duration::from_secs(2));
    assert_eq!(w.writeback.settle_all().await.unwrap(), 1);
    assert_eq!(w.get(&change).await.state, MailChangeState::Applied);

    let interrupted = w.enqueue(None, Some(uid), add(&["\\Flagged"])).await;
    let claimed = w
        .store
        .claim_mail_change(w.account, w.now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, interrupted.id);
    assert_eq!(w.writeback.recover().await.unwrap(), vec![interrupted.id]);
    assert_eq!(w.get(&interrupted).await.state, MailChangeState::Held);
    w.writeback.drain_due().await.unwrap();
    assert_eq!(
        w.flags_on_server("INBOX", uid).await,
        names(&["\\Seen", "\\Flagged"])
    );
}

#[test]
fn names_the_lock_each_change_needs() {
    let caps = |list: &[Capability]| list.iter().copied().collect::<Capabilities>();
    let target = FolderId::new();
    let flags = MailOp::Flags {
        add: names(&["\\Seen"]),
        remove: Vec::new(),
    };
    let to = MailOp::Move { target };
    let expunge = MailOp::Delete { expunge: true };
    assert!(crate::allows(
        &flags,
        false,
        &caps(&[Capability::MailFlags])
    ));
    assert!(!crate::allows(
        &flags,
        false,
        &caps(&[Capability::MailMove])
    ));
    assert!(crate::allows(&to, false, &caps(&[Capability::MailMove])));
    assert!(!crate::allows(&to, false, &caps(&[Capability::MailDelete])));
    assert!(
        crate::allows(&to, true, &caps(&[Capability::MailDelete])),
        "deleting to the trash is a move"
    );
    assert_eq!(
        crate::capability_for(
            &to,
            true,
            &caps(&[Capability::MailMove, Capability::MailDelete])
        ),
        Capability::MailMove
    );
    assert!(!crate::allows(
        &expunge,
        false,
        &caps(&[Capability::MailExpunge])
    ));
    assert!(crate::allows(
        &expunge,
        false,
        &caps(&[Capability::MailExpunge, Capability::MailDelete])
    ));
}

#[tokio::test]
async fn plans_retention_as_moves_to_the_trash_or_expunges() {
    let w = World::new().await;
    let trash = w
        .store
        .upsert_folder(w.account, "Trash", Some(FolderRole::Trash))
        .await
        .unwrap()
        .id;
    let day = 86_400;
    let start = START / 1000;
    let dated = |n: u32, days: i64| {
        let store = w.store.clone();
        let archive = w.archive;
        async move {
            store
                .insert_message(holtmail_store::testing::message(
                    archive,
                    n,
                    start + days * day,
                ))
                .await
                .unwrap()
                .message_id
        }
    };
    let old = dated(1, 0).await;
    let young = dated(2, 20).await;
    let in_trash = dated(3, 0).await;
    for (message, folder, uid) in [(old, w.inbox, 1), (young, w.inbox, 2), (in_trash, trash, 3)] {
        w.store
            .record_location(message, folder, Some(uid), &[], w.now())
            .await
            .unwrap();
    }
    let account = w.store.get_account(w.account).await.unwrap().unwrap();
    let now = Timestamp::from_secs(start + 40 * day);
    let plan = |limit| crate::plan_retention(&*w.store, &account, 30, now, None, limit);
    assert!(
        plan(10).await.unwrap().changes.is_empty(),
        "nothing without mail.delete"
    );

    let set = |list: &[Capability]| {
        let store = w.store.clone();
        let caps: Capabilities = list.iter().copied().collect();
        let account = w.account;
        async move {
            store
                .set_account_capabilities(account, &caps)
                .await
                .unwrap()
        }
    };
    set(&[Capability::MailDelete]).await;
    let moves = plan(10).await.unwrap();
    assert!(!moves.expunge && !moves.more);
    assert_eq!(
        moves.changes.len(),
        1,
        "only the old message outside the trash"
    );
    let change = &moves.changes[0];
    assert_eq!(change.message_id, Some(old));
    assert_eq!(change.op, MailOp::Move { target: trash });
    assert_eq!(change.origin, MailChangeOrigin::Retention);

    set(&[Capability::MailDelete, Capability::MailExpunge]).await;
    let removals = plan(1).await.unwrap();
    assert!(removals.expunge && removals.more);
    assert_eq!(removals.changes.len(), 1);
    assert_eq!(removals.changes[0].op, MailOp::Delete { expunge: true });

    w.store
        .enqueue_mail_change(&removals.changes[0])
        .await
        .unwrap();
    let again = plan(10).await.unwrap();
    assert_eq!(
        again.changes.len(),
        1,
        "a location with an open change waits"
    );
    assert_ne!(again.changes[0].uid, removals.changes[0].uid);
}
