use holtmail_blob::BlobId;
use holtmail_core::{AccountId, ArchiveId, FolderId, MailChangeId, MessageId, OutboxId, Timestamp};
use serde_json::json;

use crate::*;

pub(super) async fn run(store: &dyn Store) {
    enqueue_checks(store).await;
    claims(store).await;
    outcomes(store).await;
    confirmation(store).await;
    lists_and_removal(store).await;
    outbox_and_retention(store).await;
    dismissal(store).await;
    following_moves(store).await;
}

fn t(secs: i64) -> Timestamp {
    Timestamp::from_secs(1_790_000_000 + secs)
}

struct Setup {
    archive: ArchiveId,
    account: AccountId,
    inbox: FolderId,
    archive_folder: FolderId,
}

async fn setup(store: &dyn Store, name: &str) -> Setup {
    let archive = store.create_archive(name).await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
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
    let archive_folder = store
        .upsert_folder(account, "Archive", Some(FolderRole::Archive))
        .await
        .unwrap()
        .id;
    Setup {
        archive,
        account,
        inbox,
        archive_folder,
    }
}

async fn message(store: &dyn Store, archive: ArchiveId, n: u32) -> MessageId {
    store
        .insert_message(crate::testing::message(archive, n, 1000))
        .await
        .unwrap()
        .message_id
}

fn flags(s: &Setup, message: Option<MessageId>, uid: u64, add: &[&str], at: i64) -> NewMailChange {
    NewMailChange {
        account_id: s.account,
        folder_id: s.inbox,
        message_id: message,
        uid: Some(uid),
        op: MailOp::Flags {
            add: add.iter().map(|f| f.to_string()).collect(),
            remove: Vec::new(),
        },
        origin: MailChangeOrigin::User,
        user_id: None,
        device_id: None,
        outbox_id: None,
        at: t(at),
    }
}

fn invalid<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::Invalid(_)))
}

fn not_found<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::NotFound(_)))
}

async fn enqueue_checks(store: &dyn Store) {
    let s = setup(store, "Mail changes").await;
    let other = setup(store, "Mail changes elsewhere").await;
    let m = message(store, s.archive, 8000).await;
    let foreign = message(store, other.archive, 8001).await;

    let new = NewMailChange {
        user_id: None,
        device_id: Some(holtmail_core::DeviceId::new()),
        outbox_id: None,
        ..flags(&s, Some(m), 7, &["\\Seen"], 1)
    };
    let change = store.enqueue_mail_change(&new).await.unwrap();
    assert_eq!(change.new, new, "the change is kept as given");
    assert_eq!(change.state, MailChangeState::Held);
    assert_eq!(change.attempts, 0);
    assert_eq!(change.next_attempt_at, None);
    assert_eq!(change.sent_at, None);
    assert_eq!(change.updated_at, t(1));
    assert_eq!(
        store.get_mail_change(change.id).await.unwrap(),
        Some(change.clone())
    );
    assert!(
        store
            .get_mail_change(MailChangeId::new())
            .await
            .unwrap()
            .is_none()
    );

    for op in [
        MailOp::Labels {
            add: vec!["Projects/2026".into()],
            remove: vec!["Inbox".into()],
        },
        MailOp::Move {
            target: s.archive_folder,
        },
        MailOp::Delete { expunge: true },
    ] {
        let new = NewMailChange {
            op: op.clone(),
            origin: MailChangeOrigin::Rule,
            ..flags(&s, Some(m), 7, &[], 2)
        };
        let stored = store.enqueue_mail_change(&new).await.unwrap();
        assert_eq!(stored.new.op, op, "{} round-trips", op.kind());
    }
    let append = NewMailChange {
        uid: None,
        message_id: None,
        op: MailOp::Append {
            blob_id: BlobId::of(b"mail change append"),
            flags: vec!["\\Seen".into()],
            date: t(3),
        },
        origin: MailChangeOrigin::SentCopy,
        ..flags(&s, None, 0, &[], 3)
    };
    assert_eq!(
        store.enqueue_mail_change(&append).await.unwrap().new,
        append
    );

    assert!(
        invalid(
            store
                .enqueue_mail_change(&NewMailChange {
                    uid: Some(1),
                    ..append.clone()
                })
                .await
        ),
        "an append with a UID"
    );
    assert!(
        invalid(
            store
                .enqueue_mail_change(&NewMailChange {
                    uid: None,
                    ..new.clone()
                })
                .await
        ),
        "a flags change without a UID"
    );
    assert!(
        invalid(store.enqueue_mail_change(&flags(&s, None, 1, &[], 4)).await),
        "a flags change that does nothing"
    );
    assert!(invalid(
        store
            .enqueue_mail_change(&NewMailChange {
                op: MailOp::Labels {
                    add: Vec::new(),
                    remove: Vec::new()
                },
                ..new.clone()
            })
            .await
    ));
    assert!(
        invalid(
            store
                .enqueue_mail_change(&NewMailChange {
                    op: MailOp::Move { target: s.inbox },
                    ..new.clone()
                })
                .await
        ),
        "a move to its own folder"
    );
    assert!(
        invalid(
            store
                .enqueue_mail_change(&NewMailChange {
                    op: MailOp::Move {
                        target: other.inbox
                    },
                    ..new.clone()
                })
                .await
        ),
        "a move to another account's folder"
    );
    assert!(
        invalid(
            store
                .enqueue_mail_change(&NewMailChange {
                    folder_id: other.inbox,
                    ..new.clone()
                })
                .await
        ),
        "a folder of another account"
    );
    assert!(
        invalid(
            store
                .enqueue_mail_change(&NewMailChange {
                    message_id: Some(foreign),
                    ..new.clone()
                })
                .await
        ),
        "a message of another archive"
    );
    assert!(not_found(
        store
            .enqueue_mail_change(&NewMailChange {
                account_id: AccountId::new(),
                ..new.clone()
            })
            .await
    ));
    assert!(not_found(
        store
            .enqueue_mail_change(&NewMailChange {
                folder_id: FolderId::new(),
                ..new.clone()
            })
            .await
    ));
    assert!(not_found(
        store
            .enqueue_mail_change(&NewMailChange {
                message_id: Some(MessageId::new()),
                ..new.clone()
            })
            .await
    ));

    let referenced = store.referenced_blobs(None, 10_000).await.unwrap();
    assert!(
        referenced.contains(&BlobId::of(b"mail change append")),
        "an append's bytes count as referenced"
    );
}

async fn claims(store: &dyn Store) {
    let s = setup(store, "Mail change claims").await;
    let quiet = setup(store, "Mail change claims, quiet").await;
    let m = message(store, s.archive, 8010).await;
    let n = message(store, s.archive, 8011).await;
    assert!(
        store
            .claim_mail_change(s.account, t(10))
            .await
            .unwrap()
            .is_none()
    );

    let first = store
        .enqueue_mail_change(&flags(&s, Some(m), 1, &["\\Seen"], 10))
        .await
        .unwrap();
    let second = store
        .enqueue_mail_change(&flags(&s, Some(m), 1, &["\\Flagged"], 11))
        .await
        .unwrap();
    let other = store
        .enqueue_mail_change(&flags(&s, Some(n), 2, &["\\Seen"], 12))
        .await
        .unwrap();
    assert!(first.id < second.id && second.id < other.id);
    let due = store.due_mail_accounts(t(20)).await.unwrap();
    assert!(due.contains(&s.account));
    assert!(!due.contains(&quiet.account));

    let claimed = store
        .claim_mail_change(s.account, t(20))
        .await
        .unwrap()
        .expect("the oldest change goes first");
    assert_eq!(claimed.id, first.id);
    assert_eq!(claimed.state, MailChangeState::Sending);
    assert_eq!(claimed.attempts, 1);
    let next = store
        .claim_mail_change(s.account, t(20))
        .await
        .unwrap()
        .expect("another message passes the busy one");
    assert_eq!(next.id, other.id, "one in flight per message");
    assert!(
        store
            .claim_mail_change(s.account, t(20))
            .await
            .unwrap()
            .is_none(),
        "the second change of a message waits for the first"
    );
    assert!(
        store
            .claim_mail_change(quiet.account, t(20))
            .await
            .unwrap()
            .is_none()
    );

    let retried = store
        .retry_mail_change(first.id, t(100), "connection", t(21))
        .await
        .unwrap()
        .expect("a sending change can be retried");
    assert_eq!(retried.state, MailChangeState::Held);
    assert_eq!(retried.next_attempt_at, Some(t(100)));
    assert_eq!(retried.last_error.as_deref(), Some("connection"));
    assert!(
        store
            .claim_mail_change(s.account, t(50))
            .await
            .unwrap()
            .is_none(),
        "a change waits for its retry, and a later change of its message waits behind it"
    );
    assert!(
        !store
            .due_mail_accounts(t(50))
            .await
            .unwrap()
            .contains(&s.account)
    );
    assert!(
        store
            .due_mail_accounts(t(100))
            .await
            .unwrap()
            .contains(&s.account)
    );
    let again = store
        .claim_mail_change(s.account, t(100))
        .await
        .unwrap()
        .expect("due again");
    assert_eq!(again.id, first.id);
    assert_eq!(again.attempts, 2);
    assert_eq!(again.next_attempt_at, None);

    let released = store.release_mail_changes(t(101)).await.unwrap();
    let ids: Vec<_> = released
        .iter()
        .filter(|c| c.new.account_id == s.account)
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, vec![first.id, other.id], "oldest first");
    assert!(released.iter().all(|c| c.state == MailChangeState::Held));
    assert!(not_found(
        store
            .retry_mail_change(MailChangeId::new(), t(1), "x", t(1))
            .await
    ));
    assert!(
        store
            .retry_mail_change(first.id, t(200), "connection", t(102))
            .await
            .unwrap()
            .is_none(),
        "only a sending change is retried"
    );
}

async fn outcomes(store: &dyn Store) {
    let s = setup(store, "Mail change outcomes").await;
    let m = message(store, s.archive, 8020).await;
    let mv = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Move {
                target: s.archive_folder,
            },
            ..flags(&s, Some(m), 3, &[], 1)
        })
        .await
        .unwrap();
    let flag = store
        .enqueue_mail_change(&flags(&s, Some(m), 3, &["\\Seen"], 2))
        .await
        .unwrap();
    let doomed = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Delete { expunge: false },
            ..flags(&s, None, 9, &[], 3)
        })
        .await
        .unwrap();

    assert!(
        store
            .mark_mail_change_sent(mv.id, Some(40), t(10))
            .await
            .unwrap()
            .is_none(),
        "a held change cannot be marked sent"
    );
    store.claim_mail_change(s.account, t(10)).await.unwrap();
    let applied = store
        .mark_mail_change_sent(mv.id, Some(40), t(11))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        applied.state,
        MailChangeState::Applied,
        "a move is applied at once"
    );
    assert_eq!(applied.result_uid, Some(40));
    assert_eq!(applied.sent_at, None);

    let claimed = store
        .claim_mail_change(s.account, t(12))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, flag.id);
    let sent = store
        .mark_mail_change_sent(flag.id, None, t(13))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sent.state,
        MailChangeState::Sending,
        "flags wait for a sync"
    );
    assert_eq!(sent.sent_at, Some(t(13)));
    assert!(
        store
            .mark_mail_change_sent(flag.id, None, t(14))
            .await
            .unwrap()
            .is_none(),
        "sent once"
    );
    assert!(
        store
            .retry_mail_change(flag.id, t(20), "connection", t(14))
            .await
            .unwrap()
            .is_none(),
        "a sent change is not retried"
    );
    assert!(
        store
            .release_mail_changes(t(14))
            .await
            .unwrap()
            .iter()
            .all(|c| c.id != flag.id),
        "a sent change is not released"
    );
    let busy = store
        .enqueue_mail_change(&flags(&s, Some(m), 3, &["\\Flagged"], 15))
        .await
        .unwrap();
    let next = store.claim_mail_change(s.account, t(16)).await.unwrap();
    assert_eq!(
        next.map(|c| c.id),
        Some(doomed.id),
        "a message waiting for confirmation is still in flight"
    );

    assert_eq!(
        store
            .settle_mail_changes(s.account, t(13), t(17))
            .await
            .unwrap(),
        0,
        "only changes sent before the cutoff"
    );
    assert_eq!(
        store
            .settle_mail_changes(s.account, t(14), t(17))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store.get_mail_change(flag.id).await.unwrap().unwrap().state,
        MailChangeState::Applied
    );

    let failed = store
        .fail_mail_change(doomed.id, "unsupported", t(18))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, MailChangeState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("unsupported"));
    let dropped = store
        .fail_mail_change(busy.id, "discarded", t(18))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dropped.state,
        MailChangeState::Failed,
        "a held change can be given up"
    );
    assert!(
        store
            .fail_mail_change(doomed.id, "again", t(19))
            .await
            .unwrap()
            .is_none(),
        "a closed change stays closed"
    );
    assert!(not_found(
        store
            .fail_mail_change(MailChangeId::new(), "x", t(19))
            .await
    ));
}

async fn confirmation(store: &dyn Store) {
    let s = setup(store, "Mail change confirmation").await;
    let m = message(store, s.archive, 8030).await;
    let seen = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Flags {
                add: vec!["\\Seen".into()],
                remove: vec!["$Junk".into()],
            },
            ..flags(&s, Some(m), 5, &[], 1)
        })
        .await
        .unwrap();
    store.claim_mail_change(s.account, t(2)).await.unwrap();
    store
        .mark_mail_change_sent(seen.id, None, t(3))
        .await
        .unwrap();
    let flagged = store
        .enqueue_mail_change(&flags(&s, Some(m), 5, &["\\Flagged"], 4))
        .await
        .unwrap();
    let labels = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Labels {
                add: vec!["Projects".into()],
                remove: Vec::new(),
            },
            ..flags(&s, Some(m), 5, &[], 5)
        })
        .await
        .unwrap();

    let closed = store
        .confirm_mail_flags(s.inbox, 5, &["$Junk".into()], None, t(2))
        .await
        .unwrap();
    assert!(
        closed.is_empty(),
        "a read from before the send proves nothing"
    );

    let closed = store
        .confirm_mail_flags(s.inbox, 5, &["$junk".into()], None, t(6))
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].id, seen.id);
    assert_eq!(
        closed[0].state,
        MailChangeState::SupersededByServer,
        "the server wins over a sent change"
    );
    assert_eq!(
        store
            .get_mail_change(flagged.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        MailChangeState::Held,
        "a held change the server contradicts just waits"
    );

    let closed = store
        .confirm_mail_flags(
            s.inbox,
            5,
            &["\\flagged".into()],
            Some(&["Projects".into()]),
            t(7),
        )
        .await
        .unwrap();
    let ids: Vec<_> = closed.iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        vec![flagged.id, labels.id],
        "satisfied changes are applied, oldest first"
    );
    assert!(closed.iter().all(|c| c.state == MailChangeState::Applied));
    assert!(
        store
            .confirm_mail_flags(s.inbox, 6, &[], None, t(8))
            .await
            .unwrap()
            .is_empty(),
        "another UID is another message"
    );

    let in_flight = store
        .enqueue_mail_change(&flags(&s, Some(m), 5, &["\\Answered"], 9))
        .await
        .unwrap();
    store.claim_mail_change(s.account, t(10)).await.unwrap();
    assert!(
        store
            .confirm_mail_flags(s.inbox, 5, &["\\Answered".into()], None, t(11))
            .await
            .unwrap()
            .is_empty(),
        "a change being sent right now is left alone"
    );
    assert_eq!(
        store
            .get_mail_change(in_flight.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        MailChangeState::Sending
    );

    let m2 = message(store, s.archive, 8031).await;
    let read = store
        .enqueue_mail_change(&flags(&s, Some(m2), 12, &["\\Seen"], 12))
        .await
        .unwrap();
    let unread = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Flags {
                add: Vec::new(),
                remove: vec!["\\Seen".into()],
            },
            ..flags(&s, Some(m2), 12, &[], 13)
        })
        .await
        .unwrap();
    assert!(
        store
            .confirm_mail_flags(s.inbox, 12, &[], None, t(14))
            .await
            .unwrap()
            .is_empty(),
        "a newer change is never closed ahead of an older one it undoes"
    );
    for id in [read.id, unread.id] {
        assert_eq!(
            store.get_mail_change(id).await.unwrap().unwrap().state,
            MailChangeState::Held
        );
    }
}

async fn lists_and_removal(store: &dyn Store) {
    let s = setup(store, "Mail change lists").await;
    let m = message(store, s.archive, 8040).await;
    let a = store
        .enqueue_mail_change(&flags(&s, Some(m), 1, &["\\Seen"], 1))
        .await
        .unwrap();
    let b = store
        .enqueue_mail_change(&flags(&s, Some(m), 1, &["\\Flagged"], 2))
        .await
        .unwrap();
    let c = store
        .enqueue_mail_change(&flags(&s, None, 2, &["\\Seen"], 3))
        .await
        .unwrap();
    store
        .fail_mail_change(b.id, "discarded", t(4))
        .await
        .unwrap();

    let pending: Vec<_> = store
        .pending_mail_changes(m)
        .await
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(pending, vec![a.id], "open changes of the message only");

    let all = store
        .list_mail_changes(&MailChangeQuery {
            account: s.account,
            states: Vec::new(),
            limit: 10,
        })
        .await
        .unwrap();
    let ids: Vec<_> = all.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![c.id, b.id, a.id], "newest first");
    let held = store
        .list_mail_changes(&MailChangeQuery {
            account: s.account,
            states: vec![MailChangeState::Held],
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, c.id);

    store.delete_account(s.account).await.unwrap();
    assert!(store.get_mail_change(a.id).await.unwrap().is_none());
    assert!(store.pending_mail_changes(m).await.unwrap().is_empty());

    let t2 = setup(store, "Mail change removal").await;
    let gone = store
        .enqueue_mail_change(&NewMailChange {
            uid: None,
            message_id: None,
            op: MailOp::Append {
                blob_id: BlobId::of(b"mail change restore"),
                flags: Vec::new(),
                date: t(5),
            },
            ..flags(&t2, None, 0, &[], 5)
        })
        .await
        .unwrap();
    let released = store.delete_archive(t2.archive).await.unwrap();
    assert!(store.get_mail_change(gone.id).await.unwrap().is_none());
    assert!(
        released.contains(&BlobId::of(b"mail change restore")),
        "an append's bytes are released with the archive"
    );
}

async fn outbox_and_retention(store: &dyn Store) {
    let s = setup(store, "Mail change retention").await;
    let trash = store
        .upsert_folder(s.account, "Trash", Some(FolderRole::Trash))
        .await
        .unwrap()
        .id;
    let old = store
        .insert_message(crate::testing::message(s.archive, 8050, 1_000))
        .await
        .unwrap()
        .message_id;
    let older = store
        .insert_message(crate::testing::message(s.archive, 8051, 500))
        .await
        .unwrap()
        .message_id;
    let young = store
        .insert_message(crate::testing::message(s.archive, 8052, 9_000))
        .await
        .unwrap()
        .message_id;
    let busy = store
        .insert_message(crate::testing::message(s.archive, 8053, 600))
        .await
        .unwrap()
        .message_id;
    let gone = store
        .insert_message(crate::testing::message(s.archive, 8054, 700))
        .await
        .unwrap()
        .message_id;
    let unplaced = store
        .insert_message(crate::testing::message(s.archive, 8055, 800))
        .await
        .unwrap()
        .message_id;
    let at = Timestamp::from_secs(10_000);
    for (message, folder, uid) in [
        (old, s.inbox, Some(1)),
        (older, trash, Some(2)),
        (young, s.inbox, Some(3)),
        (busy, s.inbox, Some(4)),
        (gone, s.inbox, Some(5)),
        (unplaced, s.inbox, None),
    ] {
        store
            .record_location(message, folder, uid, &[], at)
            .await
            .unwrap();
    }
    store.mark_removed(s.inbox, &[5], at).await.unwrap();
    store
        .enqueue_mail_change(&flags(&s, Some(busy), 4, &["\\Seen"], 1))
        .await
        .unwrap();

    let before = Timestamp::from_secs(5_000);
    let found = store
        .retention_candidates(s.account, before, false, 10)
        .await
        .unwrap();
    assert_eq!(
        found,
        vec![
            RetentionCandidate {
                message_id: older,
                folder_id: trash,
                uid: 2
            },
            RetentionCandidate {
                message_id: old,
                folder_id: s.inbox,
                uid: 1
            },
        ],
        "old, live, located and not busy; oldest first"
    );
    let kept_trash = store
        .retention_candidates(s.account, before, true, 10)
        .await
        .unwrap();
    assert_eq!(kept_trash.len(), 1);
    assert_eq!(kept_trash[0].message_id, old);
    assert_eq!(
        store
            .retention_candidates(s.account, before, false, 1)
            .await
            .unwrap()
            .len(),
        1
    );

    let refused = store
        .enqueue_mail_change(&NewMailChange {
            account_id: s.account,
            folder_id: s.inbox,
            message_id: Some(old),
            uid: Some(1),
            op: MailOp::Delete { expunge: true },
            origin: MailChangeOrigin::Retention,
            user_id: None,
            device_id: None,
            outbox_id: None,
            at: t(3),
        })
        .await
        .unwrap();
    store
        .fail_mail_change(refused.id, "missing_folder", t(5))
        .await
        .unwrap()
        .unwrap();
    let found = store
        .retention_candidates(s.account, before, false, 10)
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|c| c.uid).collect::<Vec<_>>(),
        vec![2],
        "a location whose retention change failed is not a candidate again"
    );

    let outbox = OutboxId::new();
    let copy = store
        .enqueue_mail_change(&NewMailChange {
            uid: None,
            message_id: None,
            op: MailOp::Append {
                blob_id: BlobId::of(b"sent copy"),
                flags: vec!["\\Seen".into()],
                date: t(6),
            },
            origin: MailChangeOrigin::SentCopy,
            outbox_id: Some(outbox),
            ..flags(&s, None, 0, &[], 6)
        })
        .await
        .unwrap();
    let listed: Vec<_> = store
        .outbox_mail_changes(outbox)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(listed, vec![copy.id]);
    assert!(
        store
            .outbox_mail_changes(OutboxId::new())
            .await
            .unwrap()
            .is_empty()
    );
}

async fn dismissal(store: &dyn Store) {
    let s = setup(store, "Mail change dismissal").await;
    let m = message(store, s.archive, 8090).await;
    let failed = store
        .enqueue_mail_change(&flags(&s, Some(m), 4, &["\\Seen"], 1))
        .await
        .unwrap();
    let open = store
        .enqueue_mail_change(&flags(&s, Some(m), 4, &["\\Flagged"], 2))
        .await
        .unwrap();
    assert!(store.failed_mail_changes(m).await.unwrap().is_empty());
    assert!(
        store
            .dismiss_mail_change(open.id, t(3))
            .await
            .unwrap()
            .is_none(),
        "only a failed change can be dismissed"
    );
    store
        .fail_mail_change(failed.id, "unsupported", t(4))
        .await
        .unwrap()
        .unwrap();
    let listed = store.failed_mail_changes(m).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, failed.id);
    assert_eq!(listed[0].last_error.as_deref(), Some("unsupported"));

    let dismissed = store
        .dismiss_mail_change(failed.id, t(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dismissed.state, MailChangeState::Failed, "the state stays");
    assert_eq!(dismissed.updated_at, t(5));
    assert!(store.failed_mail_changes(m).await.unwrap().is_empty());
    assert!(
        store
            .dismiss_mail_change(failed.id, t(6))
            .await
            .unwrap()
            .is_none(),
        "dismissed once"
    );
    assert!(not_found(
        store.dismiss_mail_change(MailChangeId::new(), t(6)).await
    ));
    assert_eq!(store.pending_mail_changes(m).await.unwrap().len(), 1);
}

async fn following_moves(store: &dyn Store) {
    let s = setup(store, "Mail change moves").await;
    let m = message(store, s.archive, 8100).await;
    let moved = store
        .enqueue_mail_change(&NewMailChange {
            op: MailOp::Move {
                target: s.archive_folder,
            },
            ..flags(&s, Some(m), 7, &[], 1)
        })
        .await
        .unwrap();
    let later = store
        .enqueue_mail_change(&flags(&s, Some(m), 7, &["\\Flagged"], 2))
        .await
        .unwrap();
    assert!(
        store
            .applied_move_from(m, s.inbox, 7, later.id)
            .await
            .unwrap()
            .is_none(),
        "only applied moves count"
    );
    store.claim_mail_change(s.account, t(3)).await.unwrap();
    store
        .mark_mail_change_sent(moved.id, Some(70), t(4))
        .await
        .unwrap();
    let found = store
        .applied_move_from(m, s.inbox, 7, later.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, moved.id);
    assert_eq!(found.result_uid, Some(70));
    assert!(
        store
            .applied_move_from(m, s.inbox, 7, moved.id)
            .await
            .unwrap()
            .is_none(),
        "a move counts only for changes recorded after it"
    );

    assert!(
        store
            .retarget_mail_change(later.id, s.archive_folder, 70, t(5))
            .await
            .unwrap()
            .is_none(),
        "only a change being sent is retargeted"
    );
    let claimed = store.claim_mail_change(s.account, t(5)).await.unwrap();
    assert_eq!(claimed.map(|c| c.id), Some(later.id));
    let other = setup(store, "Mail change moves elsewhere").await;
    assert!(invalid(
        store
            .retarget_mail_change(later.id, other.inbox, 70, t(6))
            .await
    ));
    let retargeted = store
        .retarget_mail_change(later.id, s.archive_folder, 70, t(6))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retargeted.new.folder_id, s.archive_folder);
    assert_eq!(retargeted.new.uid, Some(70));
    assert_eq!(retargeted.state, MailChangeState::Sending);
}
