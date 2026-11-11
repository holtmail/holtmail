use holtmail_core::{FolderId, MessageId, Timestamp};

use super::changes::{message, setup, user};
use crate::*;

pub(super) async fn run(store: &dyn Store) {
    effective_flags(store).await;
    local_flags(store).await;
    local_places(store).await;
    subscriptions(store).await;
    folder_counts(store).await;
}

fn t(secs: i64) -> Timestamp {
    Timestamp::from_secs(1_799_000_000 + secs)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|f| f.to_string()).collect()
}

fn invalid<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::Invalid(_)))
}

fn not_found<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::NotFound(_)))
}

async fn effective_flags(store: &dyn Store) {
    let s = setup(store, "Effective flags").await;
    let reader = user(store, "effective@example.com").await;
    let m = message(store, s.archive, 9700).await;
    let kept = message(store, s.archive, 9701).await;
    store
        .record_location(m, s.inbox, Some(1), &names(&["\\Seen"]), t(0))
        .await
        .unwrap();
    store
        .record_location(m, s.other, Some(2), &names(&["\\flagged"]), t(0))
        .await
        .unwrap();
    store
        .record_location(kept, s.inbox, Some(3), &names(&["\\Seen", "$Work"]), t(0))
        .await
        .unwrap();
    store.mark_removed(s.inbox, &[3], t(1)).await.unwrap();
    let held = store
        .enqueue_mail_change(&NewMailChange {
            account_id: s.account,
            folder_id: s.other,
            message_id: Some(m),
            uid: Some(2),
            op: MailOp::Flags {
                add: names(&["\\Answered"]),
                remove: names(&["\\Flagged"]),
            },
            origin: MailChangeOrigin::User,
            user_id: None,
            device_id: None,
            outbox_id: None,
            at: t(2),
        })
        .await
        .unwrap();
    let query = |folders: Option<Vec<FolderId>>, local: bool| EffectiveFlagsQuery {
        user: reader,
        messages: vec![m, kept, MessageId::new(), m],
        folders,
        local_flags: local,
    };

    let all = store.effective_flags(&query(None, true)).await.unwrap();
    assert_eq!(all.len(), 2, "unknown messages are left out");
    assert_eq!(
        all[&m],
        names(&["\\Answered", "\\Seen"]),
        "a held change applies to its own place, removing ignores case"
    );
    assert_eq!(
        all[&kept],
        names(&["$Work", "\\Seen"]),
        "a message with no live place keeps its last flags"
    );
    let inbox = store
        .effective_flags(&query(Some(vec![s.inbox]), true))
        .await
        .unwrap();
    assert_eq!(inbox[&m], names(&["\\Seen"]));
    assert!(
        inbox[&kept].is_empty(),
        "a removed place does not count in a folder"
    );

    store
        .set_local_flags(reader, m, &names(&["$Local"]), &names(&["\\SEEN"]), t(3))
        .await
        .unwrap();
    store
        .set_local_flags(reader, kept, &names(&["\\Flagged"]), &[], t(3))
        .await
        .unwrap();
    let local = store.effective_flags(&query(None, true)).await.unwrap();
    assert_eq!(local[&m], names(&["$Local", "\\Answered"]));
    assert_eq!(local[&kept], names(&["$Work", "\\Flagged", "\\Seen"]));
    let in_inbox = store
        .effective_flags(&query(Some(vec![s.inbox]), true))
        .await
        .unwrap();
    assert_eq!(
        in_inbox[&kept],
        names(&["\\Flagged"]),
        "local flags apply everywhere"
    );
    let off = store.effective_flags(&query(None, false)).await.unwrap();
    assert_eq!(off, all, "local flags are left out when switched off");
    let other = user(store, "effective.other@example.com").await;
    let theirs = store
        .effective_flags(&EffectiveFlagsQuery {
            user: other,
            ..query(None, true)
        })
        .await
        .unwrap();
    assert_eq!(theirs, all, "local flags are their user's own");

    store
        .fail_mail_change(held.id, "refused", t(4))
        .await
        .unwrap()
        .unwrap();
    let closed = store.effective_flags(&query(None, false)).await.unwrap();
    assert_eq!(
        closed[&m],
        names(&["\\Seen", "\\flagged"]),
        "a closed change no longer shows"
    );
}

async fn local_flags(store: &dyn Store) {
    let s = setup(store, "Local flags").await;
    let ada = user(store, "local.flags@example.com").await;
    let m = message(store, s.archive, 9800).await;
    let n = message(store, s.archive, 9801).await;

    let mark = store.latest_seq().await.unwrap();
    assert!(
        store
            .set_local_flags(ada, m, &names(&["\\Seen", "$Todo"]), &[], t(1))
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_local_flags(ada, m, &names(&["\\seen"]), &[], t(2))
            .await
            .unwrap(),
        "setting what is set changes nothing"
    );
    let log = store
        .changes_since(s.archive, Some(mark), 100)
        .await
        .unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(
        (
            log[0].kind,
            log[0].message_id,
            log[0].user_id,
            log[0].folder_id
        ),
        (ChangeKind::Flags, Some(m), Some(ada), None)
    );
    assert_eq!(
        store.local_flags(ada, m).await.unwrap(),
        [
            LocalFlag {
                flag: "$Todo".into(),
                present: true
            },
            LocalFlag {
                flag: "\\Seen".into(),
                present: true
            },
        ],
        "by flag, spelled as first written"
    );
    assert!(
        store
            .set_local_flags(ada, m, &[], &names(&["$todo"]), t(3))
            .await
            .unwrap()
    );
    assert!(
        store
            .local_flags(ada, m)
            .await
            .unwrap()
            .contains(&LocalFlag {
                flag: "$Todo".into(),
                present: false
            })
    );

    assert!(invalid(
        store
            .set_local_flags(ada, m, &names(&["\\deleted"]), &[], t(4))
            .await
    ));
    assert!(invalid(
        store
            .set_local_flags(ada, m, &names(&["$A"]), &names(&["$a"]), t(4))
            .await
    ));
    assert!(invalid(
        store
            .set_local_flags(ada, m, &names(&[" "]), &[], t(4))
            .await
    ));
    assert!(not_found(
        store
            .set_local_flags(ada, MessageId::new(), &names(&["$A"]), &[], t(4))
            .await
    ));
    assert!(not_found(
        store
            .set_local_flags(holtmail_core::UserId::new(), m, &names(&["$A"]), &[], t(4))
            .await
    ));

    store
        .set_local_flags(ada, n, &names(&["\\Flagged"]), &[], t(5))
        .await
        .unwrap();
    let flagged = store.local_flagged(ada, s.archive).await.unwrap();
    let mut expected = vec![m, n];
    expected.sort();
    assert_eq!(
        flagged.iter().map(|f| f.message_id).collect::<Vec<_>>(),
        expected
    );
    assert!(
        store
            .local_flagged(ada, ArchiveId::new())
            .await
            .unwrap()
            .is_empty()
    );

    let mark = store.latest_seq().await.unwrap();
    assert_eq!(
        store
            .drop_local_flags(ada, s.archive, Some(n), t(6))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .drop_local_flags(ada, s.archive, None, t(7))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .drop_local_flags(ada, s.archive, None, t(8))
            .await
            .unwrap(),
        0
    );
    assert!(store.local_flags(ada, m).await.unwrap().is_empty());
    let log = store
        .changes_since(s.archive, Some(mark), 100)
        .await
        .unwrap();
    assert_eq!(log.len(), 2, "one row per message whose local flags went");

    store
        .set_local_flags(ada, m, &names(&["$Gone"]), &[], t(9))
        .await
        .unwrap();
    store.delete_messages(s.archive, &[m], None).await.unwrap();
    assert!(
        store
            .local_flagged(ada, s.archive)
            .await
            .unwrap()
            .is_empty(),
        "local flags go with their message"
    );
}

async fn local_places(store: &dyn Store) {
    let s = setup(store, "Local places").await;
    let ada = user(store, "local.places@example.com").await;
    let lin = user(store, "local.places.lin@example.com").await;
    let dsn = message(store, s.archive, 9900).await;
    let copy = message(store, s.archive, 9901).await;

    let mark = store.latest_seq().await.unwrap();
    assert!(
        store
            .add_local_place(ada, FolderRole::Inbox, dsn, t(1))
            .await
            .unwrap()
    );
    assert!(
        !store
            .add_local_place(ada, FolderRole::Inbox, dsn, t(2))
            .await
            .unwrap()
    );
    store
        .add_local_place(ada, FolderRole::Sent, copy, t(3))
        .await
        .unwrap();
    let log = store
        .changes_since(s.archive, Some(mark), 100)
        .await
        .unwrap();
    assert_eq!(log.len(), 2);
    assert!(
        log.iter()
            .all(|e| e.kind == ChangeKind::Place && e.user_id == Some(ada))
    );
    assert_eq!(log[0].message_id, Some(dsn));

    let all = store.local_places(ada, s.archive, None).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|p| (p.role, p.message_id))
            .collect::<Vec<_>>(),
        [(FolderRole::Inbox, dsn), (FolderRole::Sent, copy)]
    );
    assert_eq!(all[0].archive_id, s.archive);
    assert_eq!(all[0].created_at, t(1));
    assert_eq!(
        store
            .local_places(ada, s.archive, Some(FolderRole::Sent))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .local_places(lin, s.archive, None)
            .await
            .unwrap()
            .is_empty(),
        "local places are their user's own"
    );
    assert!(not_found(
        store
            .add_local_place(ada, FolderRole::Inbox, MessageId::new(), t(4))
            .await
    ));

    let mark = store.latest_seq().await.unwrap();
    assert!(
        store
            .remove_local_place(ada, FolderRole::Inbox, dsn, t(5))
            .await
            .unwrap()
    );
    assert!(
        !store
            .remove_local_place(ada, FolderRole::Inbox, dsn, t(6))
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .changes_since(s.archive, Some(mark), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    store.delete_user(ada).await.unwrap();
    assert!(
        store
            .local_places(ada, s.archive, None)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn subscriptions(store: &dyn Store) {
    let s = setup(store, "Subscriptions").await;
    let ada = user(store, "subscriptions@example.com").await;

    let mark = store.latest_seq().await.unwrap();
    assert!(
        store
            .set_subscription(ada, s.archive, "all", false, t(1))
            .await
            .unwrap(),
        "a first choice is a change"
    );
    assert!(
        !store
            .set_subscription(ada, s.archive, "all", false, t(2))
            .await
            .unwrap()
    );
    assert!(
        store
            .set_subscription(ada, s.archive, "role:inbox", true, t(3))
            .await
            .unwrap()
    );
    assert!(
        store
            .set_subscription(ada, s.archive, "all", true, t(4))
            .await
            .unwrap()
    );
    let log = store
        .changes_since(s.archive, Some(mark), 100)
        .await
        .unwrap();
    assert_eq!(log.len(), 3);
    assert!(
        log.iter()
            .all(|e| e.kind == ChangeKind::Mailbox && e.user_id == Some(ada))
    );
    assert_eq!(
        store.subscriptions(ada, s.archive).await.unwrap(),
        [
            Subscription {
                mailbox: "all".into(),
                subscribed: true
            },
            Subscription {
                mailbox: "role:inbox".into(),
                subscribed: true
            },
        ]
    );
    assert!(invalid(
        store.set_subscription(ada, s.archive, "", true, t(5)).await
    ));
    assert!(not_found(
        store
            .set_subscription(ada, ArchiveId::new(), "all", true, t(5))
            .await
    ));
    store.delete_archive(s.archive).await.unwrap();
    assert!(
        store
            .subscriptions(ada, s.archive)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn folder_counts(store: &dyn Store) {
    let s = setup(store, "Folder counts").await;
    let a = message(store, s.archive, 9950).await;
    let b = message(store, s.archive, 9951).await;
    let c = message(store, s.archive, 9952).await;
    store
        .record_location(a, s.inbox, Some(1), &names(&["\\SEEN"]), t(0))
        .await
        .unwrap();
    store
        .record_location(b, s.inbox, Some(2), &[], t(0))
        .await
        .unwrap();
    store
        .record_location(c, s.inbox, Some(3), &[], t(0))
        .await
        .unwrap();
    store.mark_removed(s.inbox, &[3], t(1)).await.unwrap();
    let unknown = FolderId::new();
    let counts = store
        .folder_counts(&[s.inbox, s.other, unknown, s.inbox])
        .await
        .unwrap();
    assert_eq!(counts.len(), 2, "unknown folders are left out");
    assert_eq!(
        counts[&s.inbox],
        FolderCounts {
            messages: 2,
            unseen: 1
        }
    );
    assert_eq!(counts[&s.other], FolderCounts::default());
}
