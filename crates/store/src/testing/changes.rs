use std::sync::Arc;
use std::time::Duration;

use holtmail_core::{AccountId, ArchiveId, FolderId, MessageId, Timestamp, UserId};
use serde_json::json;

use crate::*;

pub(super) async fn run(store: Arc<dyn Store>) {
    folder_and_message_writers(&*store).await;
    thread_merges(&*store).await;
    mail_change_rows(&*store).await;
    head_and_every_archive(&*store).await;
    heads_per_kind(&*store).await;
    watch_wakes_after_a_commit(&*store).await;
    visibility_in_seq_order(store.clone()).await;
    pruning(&*store).await;
}

fn t(secs: i64) -> Timestamp {
    Timestamp::from_secs(1_798_000_000 + secs)
}

pub(super) struct Setup {
    pub(super) archive: ArchiveId,
    pub(super) account: AccountId,
    pub(super) inbox: FolderId,
    pub(super) other: FolderId,
}

pub(super) async fn setup(store: &dyn Store, name: &str) -> Setup {
    let archive = store.create_archive(name).await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: name.into(),
            address: Some("ada@example.com".into()),
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let inbox = store
        .upsert_folder(account, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    let other = store
        .upsert_folder(account, "Archive", Some(FolderRole::Archive))
        .await
        .unwrap()
        .id;
    Setup {
        archive,
        account,
        inbox,
        other,
    }
}

pub(super) async fn message(store: &dyn Store, archive: ArchiveId, n: u32) -> MessageId {
    store
        .insert_message(crate::testing::message(archive, n, 1000))
        .await
        .unwrap()
        .message_id
}

pub(super) async fn user(store: &dyn Store, email: &str) -> UserId {
    store
        .create_user(NewUser {
            email: email.into(),
            name: "Reader".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id
}

async fn since(store: &dyn Store, archive: ArchiveId, after: u64) -> Vec<ChangeLogEntry> {
    store
        .changes_since(archive, Some(after), 1000)
        .await
        .unwrap()
}

fn flags(names: &[&str]) -> Vec<String> {
    names.iter().map(|f| f.to_string()).collect()
}

async fn folder_and_message_writers(store: &dyn Store) {
    let start = store.latest_seq().await.unwrap();
    let s = setup(store, "Change writers").await;
    let log = since(store, s.archive, start).await;
    assert_eq!(log.len(), 2, "each new folder writes a row");
    assert!(
        log.iter()
            .all(|e| e.kind == ChangeKind::Mailbox && e.user_id.is_none())
    );
    assert_eq!(log[0].folder_id, Some(s.inbox));
    assert!(log[0].seq < log[1].seq);

    let mark = store.latest_seq().await.unwrap();
    store
        .upsert_folder(s.account, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    assert!(
        since(store, s.archive, mark).await.is_empty(),
        "a folder sync that changes nothing writes nothing"
    );
    store
        .upsert_folder(s.account, "Archive", None)
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1, "a role change writes a row");
    assert_eq!(log[0].folder_id, Some(s.other));

    let mark = store.latest_seq().await.unwrap();
    let inserted = store
        .insert_message(crate::testing::message(s.archive, 9100, 1000))
        .await
        .unwrap();
    let m = inserted.message_id;
    store
        .insert_message(crate::testing::message(s.archive, 9100, 1000))
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(
        log.len(),
        1,
        "inserting the same bytes again writes nothing"
    );
    assert_eq!(log[0].kind, ChangeKind::Message);
    assert_eq!(log[0].message_id, Some(m));
    assert_eq!(log[0].thread_id, Some(inserted.thread_id));

    let mark = store.latest_seq().await.unwrap();
    store
        .record_location(m, s.inbox, Some(1), &flags(&["\\Seen"]), t(1))
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1);
    assert_eq!(
        (log[0].kind, log[0].message_id, log[0].folder_id),
        (ChangeKind::Place, Some(m), Some(s.inbox))
    );

    let mark = store.latest_seq().await.unwrap();
    store
        .record_location(m, s.inbox, Some(7), &flags(&["\\Seen", "\\Seen"]), t(2))
        .await
        .unwrap();
    assert!(
        since(store, s.archive, mark).await.is_empty(),
        "a new upstream UID with the same flags is no served change"
    );
    store
        .record_location(m, s.inbox, Some(7), &flags(&["\\Seen", "\\Flagged"]), t(3))
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].kind, ChangeKind::Flags);

    let mark = store.latest_seq().await.unwrap();
    assert!(
        !store
            .set_flags(s.inbox, 7, &flags(&["\\Flagged", "\\Seen"]))
            .await
            .unwrap()
    );
    assert!(
        since(store, s.archive, mark).await.is_empty(),
        "a resync of the same flags writes nothing"
    );
    assert!(
        store
            .set_flags(s.inbox, 7, &flags(&["\\Seen"]))
            .await
            .unwrap()
    );
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1);
    assert_eq!(
        (log[0].kind, log[0].message_id, log[0].folder_id),
        (ChangeKind::Flags, Some(m), Some(s.inbox))
    );

    let mark = store.latest_seq().await.unwrap();
    store.mark_removed(s.inbox, &[7, 99], t(4)).await.unwrap();
    store.mark_removed(s.inbox, &[7], t(5)).await.unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1, "only a live place going away writes a row");
    assert_eq!(
        (log[0].kind, log[0].message_id, log[0].folder_id),
        (ChangeKind::Place, Some(m), Some(s.inbox))
    );
    let mark = store.latest_seq().await.unwrap();
    store
        .record_location(m, s.inbox, Some(8), &flags(&["\\Seen"]), t(6))
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1, "a place that comes back writes a row");
    assert_eq!(log[0].kind, ChangeKind::Place);

    let mark = store.latest_seq().await.unwrap();
    assert!(store.set_labels(m, &flags(&["Work"])).await.unwrap());
    assert!(!store.set_labels(m, &flags(&["Work"])).await.unwrap());
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1, "labels write a row only when they change");
    assert_eq!(
        (log[0].kind, log[0].message_id, log[0].folder_id),
        (ChangeKind::Flags, Some(m), None)
    );

    let mark = store.latest_seq().await.unwrap();
    store.delete_messages(s.archive, &[m], None).await.unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1);
    assert_eq!(
        (log[0].kind, log[0].message_id),
        (ChangeKind::Message, Some(m))
    );

    let mark = store.latest_seq().await.unwrap();
    store.delete_account(s.account).await.unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(
        log.len(),
        2,
        "every folder of a removed account writes a row"
    );
    assert!(log.iter().all(|e| e.kind == ChangeKind::Mailbox));
}

async fn thread_merges(store: &dyn Store) {
    let archive = store.create_archive("Change threads").await.unwrap().id;
    let first = store
        .insert_message(crate::testing::message(archive, 9200, 1000))
        .await
        .unwrap();
    let second = store
        .insert_message(crate::testing::message(archive, 9201, 1001))
        .await
        .unwrap();
    assert_ne!(first.thread_id, second.thread_id);
    let mark = store.latest_seq().await.unwrap();
    let joined = store
        .insert_message(NewMessage {
            parent_ids: vec!["m9200@example.com".into(), "m9201@example.com".into()],
            ..crate::testing::message(archive, 9202, 1002)
        })
        .await
        .unwrap();
    let log = since(store, archive, mark).await;
    let (kept, left) = if joined.thread_id == first.thread_id {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(joined.thread_id, kept.thread_id);
    assert_eq!(
        log.len(),
        2,
        "one row for the moved message, one for the new one"
    );
    assert_eq!(
        (log[0].kind, log[0].message_id, log[0].thread_id),
        (
            ChangeKind::Thread,
            Some(left.message_id),
            Some(left.thread_id)
        ),
        "the moved message names the thread it left"
    );
    assert_eq!(log[1].kind, ChangeKind::Message);
    assert_eq!(log[1].thread_id, Some(kept.thread_id));
}

async fn mail_change_rows(store: &dyn Store) {
    let s = setup(store, "Change mail changes").await;
    let m = message(store, s.archive, 9300).await;
    store
        .record_location(m, s.inbox, Some(3), &[], t(0))
        .await
        .unwrap();
    let flags_change = |add: &[&str], at| NewMailChange {
        account_id: s.account,
        folder_id: s.inbox,
        message_id: Some(m),
        uid: Some(3),
        op: MailOp::Flags {
            add: flags(add),
            remove: Vec::new(),
        },
        origin: MailChangeOrigin::User,
        user_id: None,
        device_id: None,
        outbox_id: None,
        at,
    };

    let mark = store.latest_seq().await.unwrap();
    let change = store
        .enqueue_mail_change(&flags_change(&["\\Seen"], t(1)))
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 1, "a held change shows at once");
    assert_eq!(
        (log[0].kind, log[0].message_id, log[0].folder_id),
        (ChangeKind::Flags, Some(m), Some(s.inbox))
    );

    let mark = store.latest_seq().await.unwrap();
    let claimed = store
        .claim_mail_change(s.account, t(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, change.id);
    store
        .mark_mail_change_sent(change.id, None, t(3))
        .await
        .unwrap()
        .unwrap();
    assert!(
        since(store, s.archive, mark).await.is_empty(),
        "sending changes nothing served"
    );
    let closed = store
        .confirm_mail_flags(s.inbox, 3, &flags(&["\\Seen"]), None, t(4))
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(
        since(store, s.archive, mark).await.len(),
        1,
        "closing a change writes a row"
    );

    let mark = store.latest_seq().await.unwrap();
    let moving = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Move { target: s.other },
            ..flags_change(&[], t(5))
        })
        .await
        .unwrap();
    let log = since(store, s.archive, mark).await;
    assert_eq!(log.len(), 2, "a move touches its folder and its target");
    assert!(log.iter().all(|e| e.kind == ChangeKind::Place));
    assert_eq!(log[0].folder_id, Some(s.inbox));
    assert_eq!(log[1].folder_id, Some(s.other));

    let mark = store.latest_seq().await.unwrap();
    store
        .claim_mail_change(s.account, t(6))
        .await
        .unwrap()
        .unwrap();
    store
        .retry_mail_change(moving.id, t(7), "connection", t(6))
        .await
        .unwrap()
        .unwrap();
    assert!(
        since(store, s.archive, mark).await.is_empty(),
        "a retry stays open"
    );
    store
        .fail_mail_change(moving.id, "refused", t(8))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(since(store, s.archive, mark).await.len(), 2);
    assert!(
        store
            .fail_mail_change(moving.id, "refused", t(9))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        since(store, s.archive, mark).await.len(),
        2,
        "a refused transition writes nothing"
    );

    let mark = store.latest_seq().await.unwrap();
    let late = store
        .enqueue_mail_change(&flags_change(&["\\Flagged"], t(10)))
        .await
        .unwrap();
    store
        .claim_mail_change(s.account, t(11))
        .await
        .unwrap()
        .unwrap();
    store
        .mark_mail_change_sent(late.id, None, t(12))
        .await
        .unwrap();
    assert_eq!(
        store
            .settle_mail_changes(s.account, t(13), t(14))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        since(store, s.archive, mark).await.len(),
        2,
        "settling writes a row per change"
    );
}

async fn head_and_every_archive(store: &dyn Store) {
    let one = store.create_archive("Change head one").await.unwrap().id;
    let two = store.create_archive("Change head two").await.unwrap().id;
    let mark = store.latest_seq().await.unwrap();
    let a = message(store, one, 9400).await;
    let b = message(store, two, 9401).await;
    let all = store.all_changes_since(mark, 1000).await.unwrap();
    assert_eq!(
        all.iter().map(|e| e.message_id).collect::<Vec<_>>(),
        [Some(a), Some(b)],
        "every archive, oldest first"
    );
    let head = store.latest_seq().await.unwrap();
    assert_eq!(head, all[1].seq);
    assert_eq!(store.all_changes_since(mark, 1).await.unwrap().len(), 1);
    assert_eq!(
        store
            .changes_since(two, Some(mark), 1000)
            .await
            .unwrap()
            .len(),
        1
    );
    store.delete_archive(two).await.unwrap();
    assert_eq!(
        store.latest_seq().await.unwrap(),
        head,
        "the head stays when the newest rows go with their archive"
    );
    let c = message(store, one, 9402).await;
    let next = store.all_changes_since(head, 10).await.unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].message_id, Some(c));
    assert!(next[0].seq > head, "seq is never reused");
}

async fn heads_per_kind(store: &dyn Store) {
    let s = setup(store, "Change heads").await;
    let other = store.create_archive("Change heads other").await.unwrap().id;
    let (ada, bob) = (
        user(store, "heads-ada@example.com").await,
        user(store, "heads-bob@example.com").await,
    );
    assert!(store.archive_heads(s.archive, None).await.unwrap().len() == 1);
    let m = message(store, s.archive, 9500).await;
    let after_message = store.latest_seq().await.unwrap();
    message(store, other, 9501).await;
    store
        .set_local_flags(ada, m, &["\\Seen".to_string()], &[], t(1))
        .await
        .unwrap();
    let local = store.latest_seq().await.unwrap();
    let heads = store.archive_heads(s.archive, Some(ada)).await.unwrap();
    assert_eq!(heads.get(&ChangeKind::Message), Some(&after_message));
    assert_eq!(
        heads.get(&ChangeKind::Flags),
        Some(&local),
        "the user's own row"
    );
    assert!(!heads.contains_key(&ChangeKind::Draft));
    let theirs = store.archive_heads(s.archive, Some(bob)).await.unwrap();
    assert_eq!(
        theirs.get(&ChangeKind::Flags),
        None,
        "another user's own row is left out"
    );
    let everyone = store.archive_heads(s.archive, None).await.unwrap();
    assert_eq!(everyone.get(&ChangeKind::Flags), None);
    assert_eq!(everyone.get(&ChangeKind::Message), Some(&after_message));
}

async fn watch_wakes_after_a_commit(store: &dyn Store) {
    let archive = store.create_archive("Change watch").await.unwrap().id;
    let mut watch = store.watch_changes().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), watch.next())
            .await
            .is_err(),
        "a watch waits for a newer row"
    );
    message(store, archive, 9500).await;
    let seq = tokio::time::timeout(Duration::from_secs(10), watch.next())
        .await
        .expect("a commit wakes the watch well before the poll");
    assert_eq!(seq, store.latest_seq().await.unwrap());
    message(store, archive, 9501).await;
    message(store, archive, 9502).await;
    let seq = tokio::time::timeout(Duration::from_secs(10), watch.next())
        .await
        .unwrap();
    assert_eq!(
        seq,
        store.latest_seq().await.unwrap(),
        "wake-ups coalesce to the head"
    );
}

async fn visibility_in_seq_order(store: Arc<dyn Store>) {
    let archive = store.create_archive("Change visibility").await.unwrap().id;
    let reader = user(&*store, "visibility@example.com").await;
    let mut messages = Vec::new();
    for n in 0..8 {
        messages.push(message(&*store, archive, 9600 + n).await);
    }
    let start = store.latest_seq().await.unwrap();
    let writers: Vec<_> = messages
        .iter()
        .map(|&m| {
            let store = store.clone();
            tokio::spawn(async move {
                for round in 0..15 {
                    let flag = format!("$round{round}");
                    store
                        .set_local_flags(reader, m, &[flag], &[], t(round))
                        .await
                        .unwrap();
                }
            })
        })
        .collect();
    let mut seen = Vec::new();
    let mut last = start;
    loop {
        let done = writers.iter().all(|w| w.is_finished());
        for entry in store
            .changes_since(archive, Some(last), 1000)
            .await
            .unwrap()
        {
            assert!(entry.seq > last, "a row appeared below one already read");
            last = entry.seq;
            seen.push(entry.seq);
        }
        if done {
            break;
        }
        tokio::task::yield_now().await;
    }
    for writer in writers {
        writer.await.unwrap();
    }
    let all: Vec<u64> = store
        .changes_since(archive, Some(start), 1000)
        .await
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(all.len(), 8 * 15);
    assert_eq!(seen, all, "a reader following the head missed no row");
}

/// Runs last: it prunes every older row of the instance.
async fn pruning(store: &dyn Store) {
    let far = |secs: i64| Timestamp::from_secs(4_102_444_800 + secs);
    let archive = store.create_archive("Change pruning").await.unwrap().id;
    let reader = user(store, "pruning@example.com").await;
    let mark = store.pruned_through().await.unwrap();
    for (n, at) in [(1, 1), (2, 2), (3, 10)] {
        store
            .set_subscription(reader, archive, &format!("folder:{n}"), true, far(at))
            .await
            .unwrap();
    }
    let rows = store.changes_since(archive, None, 100).await.unwrap();
    assert_eq!(rows.len(), 3);
    let latest = store.latest_seq().await.unwrap();

    let through = store.prune_changes(far(5)).await.unwrap();
    assert_eq!(
        through, rows[1].seq,
        "up to the first row written after the cut"
    );
    assert!(through >= mark);
    assert_eq!(store.pruned_through().await.unwrap(), through);
    let kept = store.changes_since(archive, None, 100).await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].seq, rows[2].seq);
    assert!(
        store.all_changes_since(0, 100).await.unwrap()[0].seq > through,
        "every archive is pruned"
    );
    assert_eq!(store.latest_seq().await.unwrap(), latest, "the head stays");

    assert_eq!(
        store.prune_changes(far(5)).await.unwrap(),
        through,
        "idempotent"
    );
    assert_eq!(
        store.prune_changes(far(0)).await.unwrap(),
        through,
        "the mark never goes down"
    );
    assert_eq!(
        store.changes_since(archive, None, 100).await.unwrap().len(),
        1
    );

    let all = store.prune_changes(far(100)).await.unwrap();
    assert_eq!(all, latest, "nothing newer: everything goes");
    assert!(
        store
            .changes_since(archive, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .set_subscription(reader, archive, "folder:4", true, far(101))
        .await
        .unwrap();
    assert_eq!(
        store
            .changes_since(archive, Some(all), 100)
            .await
            .unwrap()
            .len(),
        1,
        "the log goes on after the mark"
    );
}
