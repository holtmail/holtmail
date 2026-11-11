use std::collections::HashSet;
use std::sync::Arc;

use holtmail_core::{ArchiveId, FolderId, MessageId, Timestamp, UserId};
use serde_json::json;

use super::changes::{Setup, message, setup, user};
use crate::*;

pub(super) async fn run(store: Arc<dyn Store>) {
    keys_round_trip();
    open_and_build(&*store).await;
    catch_up_appends_raises_and_tombstones(&*store).await;
    held_move_keeps_its_uid(&*store).await;
    refused_move_gets_a_new_uid(&*store).await;
    upstream_uidvalidity_reset(&*store).await;
    account_rename_and_role_change_keep_uids(&*store).await;
    all_and_kept(&*store).await;
    drafts_view(&*store).await;
    local_flags_raise_modseq(&*store).await;
    local_places_in_role_views(&*store).await;
    concurrent_catch_up(store.clone()).await;
    pruning(&*store).await;
}

fn t(secs: i64) -> Timestamp {
    Timestamp::from_secs(1_800_000_000 + secs)
}

fn not_found<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::NotFound(_)))
}

async fn located(store: &dyn Store, s: &Setup, folder: FolderId, n: u32, uid: u64) -> MessageId {
    let m = message(store, s.archive, n).await;
    store
        .record_location(m, folder, Some(uid), &[], t(0))
        .await
        .unwrap();
    m
}

async fn built(store: &dyn Store, archive: ArchiveId, key: ServedKey) -> ServedMailbox {
    store.open_served_mailbox(archive, key, t(0)).await.unwrap();
    let view = store
        .build_served_mailbox(archive, key, 1000, t(0))
        .await
        .unwrap();
    assert!(view.built);
    view
}

async fn catch_up(store: &dyn Store, archive: ArchiveId, key: ServedKey) -> ServedMailbox {
    store
        .catch_up_served_mailbox(archive, key, t(0))
        .await
        .unwrap()
}

async fn uids(store: &dyn Store, archive: ArchiveId, key: ServedKey) -> Vec<ServedUid> {
    store
        .served_uids(
            archive,
            key,
            &ServedUidQuery {
                limit: 10_000,
                ..ServedUidQuery::default()
            },
        )
        .await
        .unwrap()
}

async fn uid_of(
    store: &dyn Store,
    archive: ArchiveId,
    key: ServedKey,
    m: MessageId,
) -> Option<u32> {
    store
        .served_uid_of(archive, key, &[ServedMember::Message(m)])
        .await
        .unwrap()
        .get(&ServedMember::Message(m))
        .copied()
}

fn messages(list: &[ServedUid]) -> Vec<MessageId> {
    list.iter()
        .map(|u| match u.member {
            ServedMember::Message(m) => m,
            ServedMember::Draft { .. } => panic!("a draft in a message view"),
        })
        .collect()
}

fn keys_round_trip() {
    let folder = FolderId::new();
    let user = UserId::new();
    for key in [
        ServedKey::Role(FolderRole::Inbox),
        ServedKey::Folder(folder),
        ServedKey::All,
        ServedKey::Kept,
        ServedKey::Drafts(user),
    ] {
        assert_eq!(ServedKey::parse(&key.name(), key.user()), Some(key));
    }
    assert_eq!(ServedKey::Folder(folder).name(), format!("folder:{folder}"));
    assert_eq!(ServedKey::Role(FolderRole::Sent).name(), "role:sent");
    assert_eq!(ServedKey::parse("drafts", None), None);
    assert_eq!(ServedKey::parse("all", Some(user)), None);
    assert_eq!(ServedKey::parse("role:nothing", None), None);
}

async fn open_and_build(store: &dyn Store) {
    let s = setup(store, "Served build").await;
    let other = setup(store, "Served build elsewhere").await;
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(located(store, &s, s.inbox, 10_000 + n, u64::from(n) + 1).await);
    }
    let key = ServedKey::Folder(s.inbox);

    assert!(not_found(
        store.catch_up_served_mailbox(s.archive, key, t(0)).await
    ));
    assert_eq!(store.served_mailbox(s.archive, key).await.unwrap(), None);
    assert!(not_found(
        store
            .open_served_mailbox(s.archive, ServedKey::Folder(other.inbox), t(0))
            .await
    ));
    assert!(not_found(
        store
            .open_served_mailbox(s.archive, ServedKey::Drafts(UserId::new()), t(0))
            .await
    ));
    assert!(not_found(
        store
            .open_served_mailbox(ArchiveId::new(), ServedKey::All, t(0))
            .await
    ));

    let head = store.latest_seq().await.unwrap();
    let opened = store
        .open_served_mailbox(s.archive, key, t(1))
        .await
        .unwrap();
    assert_ne!(opened.uidvalidity, 0);
    assert_eq!(opened.uidnext, 1);
    assert!(!opened.built);
    assert_eq!(opened.synced_seq, head.max(1));
    assert_eq!(opened.highest_modseq, head.max(1));
    assert_eq!(opened.lowest_modseq, head.max(1));
    assert_eq!(
        store
            .open_served_mailbox(s.archive, key, t(2))
            .await
            .unwrap(),
        opened,
        "opening again changes nothing"
    );
    assert_eq!(
        store.served_mailbox(s.archive, key).await.unwrap(),
        Some(opened.clone())
    );
    assert_eq!(
        catch_up(store, s.archive, key).await,
        opened,
        "an unbuilt view is left alone"
    );

    let step = store
        .build_served_mailbox(s.archive, key, 2, t(3))
        .await
        .unwrap();
    assert!(!step.built);
    assert_eq!(
        step.progress,
        BuildProgress {
            assigned: 2,
            total: 5
        }
    );
    assert_eq!(step.uidvalidity, opened.uidvalidity);
    let late = located(store, &s, s.inbox, 10_005, 6).await;
    let step = store
        .build_served_mailbox(s.archive, key, 2, t(4))
        .await
        .unwrap();
    assert_eq!(step.progress.assigned, 4);
    assert!(!step.built);
    let done = store
        .build_served_mailbox(s.archive, key, 2, t(5))
        .await
        .unwrap();
    assert!(done.built, "a short step finishes the build");
    assert_eq!(done.uidnext, 7);
    assert_eq!(
        done.progress,
        BuildProgress {
            assigned: 6,
            total: 6
        }
    );
    assert_eq!(done.synced_seq, store.latest_seq().await.unwrap());
    let list = uids(store, s.archive, key).await;
    assert_eq!(
        list.iter().map(|u| u.uid).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    let mut expected = ids.clone();
    expected.push(late);
    assert_eq!(messages(&list), expected, "UIDs go in archived order");
    assert!(list.iter().all(|u| u.modseq <= done.highest_modseq));
    assert_eq!(
        catch_up(store, s.archive, key).await,
        done,
        "nothing changed, nothing moves"
    );

    store
        .open_served_mailbox(s.archive, ServedKey::All, t(6))
        .await
        .unwrap();
    let names: Vec<String> = store
        .served_mailboxes(s.archive)
        .await
        .unwrap()
        .iter()
        .map(|v| v.key.name())
        .collect();
    assert_eq!(names, ["all".to_string(), key.name()]);
}

async fn catch_up_appends_raises_and_tombstones(store: &dyn Store) {
    let s = setup(store, "Served catch-up").await;
    let a = located(store, &s, s.inbox, 10_100, 1).await;
    let b = located(store, &s, s.inbox, 10_101, 2).await;
    let key = ServedKey::Folder(s.inbox);
    let first = built(store, s.archive, key).await;
    assert_eq!(uid_of(store, s.archive, key, a).await, Some(1));
    assert_eq!(uid_of(store, s.archive, key, b).await, Some(2));

    let c = located(store, &s, s.inbox, 10_102, 3).await;
    let joined = store.latest_seq().await.unwrap();
    assert_eq!(
        uid_of(store, s.archive, key, c).await,
        None,
        "reads show the last catch-up"
    );
    let view = catch_up(store, s.archive, key).await;
    assert_eq!(view.uidnext, 4);
    assert_eq!(view.synced_seq, joined);
    assert_eq!(view.highest_modseq, joined);
    assert_eq!(view.uidvalidity, first.uidvalidity);
    let list = uids(store, s.archive, key).await;
    assert_eq!(list.last().unwrap().uid, 3);
    assert_eq!(list.last().unwrap().modseq, joined);

    assert!(
        store
            .set_flags(s.inbox, 1, &["\\Seen".to_string()])
            .await
            .unwrap()
    );
    let flagged = store.latest_seq().await.unwrap();
    let view = catch_up(store, s.archive, key).await;
    assert_eq!(view.highest_modseq, flagged);
    let changed = store
        .served_uids(
            s.archive,
            key,
            &ServedUidQuery {
                changed_since: Some(joined),
                limit: 100,
                ..ServedUidQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        changed,
        [ServedUid {
            uid: 1,
            member: ServedMember::Message(a),
            modseq: flagged
        }]
    );
    let paged = store
        .served_uids(
            s.archive,
            key,
            &ServedUidQuery {
                after_uid: 1,
                limit: 1,
                ..ServedUidQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].uid, 2);

    store.mark_removed(s.inbox, &[2], t(1)).await.unwrap();
    let removed = store.latest_seq().await.unwrap();
    let view = catch_up(store, s.archive, key).await;
    assert_eq!(view.highest_modseq, removed);
    assert_eq!(uid_of(store, s.archive, key, b).await, None);
    assert_eq!(
        store
            .served_vanished(s.archive, key, flagged, 0, 100)
            .await
            .unwrap(),
        [2]
    );
    assert!(
        store
            .served_vanished(s.archive, key, removed, 0, 100)
            .await
            .unwrap()
            .is_empty()
    );

    store
        .record_location(b, s.inbox, Some(4), &[], t(2))
        .await
        .unwrap();
    let view = catch_up(store, s.archive, key).await;
    assert_eq!(
        uid_of(store, s.archive, key, b).await,
        Some(4),
        "a member that comes back gets a new UID"
    );
    assert_eq!(view.uidnext, 5);
    assert_eq!(
        store
            .served_vanished(s.archive, key, flagged, 0, 100)
            .await
            .unwrap(),
        [2],
        "the old UID stays gone"
    );
    let all = uids(store, s.archive, key).await;
    assert_eq!(all.iter().map(|u| u.uid).collect::<Vec<_>>(), [1, 3, 4]);

    store.delete_messages(s.archive, &[c], None).await.unwrap();
    catch_up(store, s.archive, key).await;
    assert_eq!(
        uid_of(store, s.archive, key, c).await,
        None,
        "a message deleted from the archive leaves"
    );
}

fn move_change(s: &Setup, m: MessageId, uid: u64, at: Timestamp) -> NewMailChange {
    NewMailChange {
        account_id: s.account,
        folder_id: s.inbox,
        message_id: Some(m),
        uid: Some(uid),
        op: MailOp::Move { target: s.other },
        origin: MailChangeOrigin::User,
        user_id: None,
        device_id: None,
        outbox_id: None,
        at,
    }
}

async fn held_move_keeps_its_uid(store: &dyn Store) {
    let s = setup(store, "Served held move").await;
    let stays = located(store, &s, s.inbox, 10_200, 1).await;
    let m = located(store, &s, s.inbox, 10_201, 2).await;
    let inbox = ServedKey::Folder(s.inbox);
    let target = ServedKey::Folder(s.other);
    let role = ServedKey::Role(FolderRole::Archive);
    built(store, s.archive, inbox).await;
    let before = built(store, s.archive, target).await;
    built(store, s.archive, role).await;
    assert_eq!(before.uidnext, 1);

    let change = store
        .enqueue_mail_change(&move_change(&s, m, 2, t(1)))
        .await
        .unwrap();
    catch_up(store, s.archive, inbox).await;
    let moved = catch_up(store, s.archive, target).await;
    catch_up(store, s.archive, role).await;
    let copied = uid_of(store, s.archive, target, m)
        .await
        .expect("a held move shows in its target at once");
    let in_role = uid_of(store, s.archive, role, m).await.unwrap();
    assert_eq!(moved.uidvalidity, before.uidvalidity);
    assert_eq!(uid_of(store, s.archive, inbox, m).await, None);
    assert_eq!(uid_of(store, s.archive, inbox, stays).await, Some(1));

    store
        .claim_mail_change(s.account, t(2))
        .await
        .unwrap()
        .unwrap();
    store
        .mark_mail_change_sent(change.id, Some(70), t(3))
        .await
        .unwrap()
        .unwrap();
    catch_up(store, s.archive, inbox).await;
    catch_up(store, s.archive, target).await;
    assert_eq!(
        uid_of(store, s.archive, target, m).await,
        Some(copied),
        "an applied move keeps the UID before sync sees it"
    );
    assert_eq!(uid_of(store, s.archive, inbox, m).await, None);

    store
        .record_location(m, s.other, Some(70), &[], t(4))
        .await
        .unwrap();
    store.mark_removed(s.inbox, &[2], t(4)).await.unwrap();
    catch_up(store, s.archive, inbox).await;
    let confirmed = catch_up(store, s.archive, target).await;
    catch_up(store, s.archive, role).await;
    assert_eq!(
        uid_of(store, s.archive, target, m).await,
        Some(copied),
        "sync seeing the message under its new upstream UID keeps the served UID"
    );
    assert_eq!(uid_of(store, s.archive, role, m).await, Some(in_role));
    assert_eq!(uid_of(store, s.archive, inbox, m).await, None);
    assert_eq!(confirmed.uidnext, copied + 1, "no UID was handed out twice");
    assert_eq!(
        store
            .served_vanished(s.archive, target, 0, 0, 100)
            .await
            .unwrap(),
        Vec::<u32>::new()
    );
}

async fn refused_move_gets_a_new_uid(store: &dyn Store) {
    let s = setup(store, "Served refused move").await;
    let m = located(store, &s, s.inbox, 10_300, 5).await;
    let inbox = ServedKey::Folder(s.inbox);
    let target = ServedKey::Folder(s.other);
    built(store, s.archive, inbox).await;
    built(store, s.archive, target).await;
    let old = uid_of(store, s.archive, inbox, m).await.unwrap();

    let change = store
        .enqueue_mail_change(&move_change(&s, m, 5, t(1)))
        .await
        .unwrap();
    catch_up(store, s.archive, inbox).await;
    catch_up(store, s.archive, target).await;
    let copied = uid_of(store, s.archive, target, m).await.unwrap();
    store
        .fail_mail_change(change.id, "refused", t(2))
        .await
        .unwrap()
        .unwrap();
    catch_up(store, s.archive, inbox).await;
    catch_up(store, s.archive, target).await;
    let back = uid_of(store, s.archive, inbox, m)
        .await
        .expect("a refused move puts the message back");
    assert!(back > old, "with a new UID, never the old one");
    assert_eq!(uid_of(store, s.archive, target, m).await, None);
    assert_eq!(
        store
            .served_vanished(s.archive, target, 0, 0, 100)
            .await
            .unwrap(),
        [copied]
    );
    assert_eq!(
        store
            .served_vanished(s.archive, inbox, 0, 0, 100)
            .await
            .unwrap(),
        [old]
    );
}

async fn upstream_uidvalidity_reset(store: &dyn Store) {
    let s = setup(store, "Served UIDVALIDITY reset").await;
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(located(store, &s, s.inbox, 10_400 + n, u64::from(n) + 1).await);
    }
    let key = ServedKey::Folder(s.inbox);
    let before = built(store, s.archive, key).await;
    let served = uids(store, s.archive, key).await;

    store.forget_uids(s.inbox).await.unwrap();
    for (n, &m) in ids.iter().enumerate() {
        store
            .record_location(m, s.inbox, Some(9000 + n as u64), &[], t(5))
            .await
            .unwrap();
    }
    let after = catch_up(store, s.archive, key).await;
    assert_eq!(after.uidvalidity, before.uidvalidity);
    assert_eq!(after.uidnext, before.uidnext);
    let again = uids(store, s.archive, key).await;
    assert_eq!(
        again.iter().map(|u| (u.uid, u.member)).collect::<Vec<_>>(),
        served.iter().map(|u| (u.uid, u.member)).collect::<Vec<_>>(),
        "new upstream UIDs change nothing for clients"
    );
}

async fn account_rename_and_role_change_keep_uids(store: &dyn Store) {
    let s = setup(store, "Served account rename").await;
    let m = located(store, &s, s.other, 10_500, 1).await;
    let folder = ServedKey::Folder(s.other);
    let role = ServedKey::Role(FolderRole::Archive);
    let before = built(store, s.archive, folder).await;
    built(store, s.archive, role).await;
    let uid = uid_of(store, s.archive, folder, m).await.unwrap();
    let role_uid = uid_of(store, s.archive, role, m).await.unwrap();

    let mut account = store.get_account(s.account).await.unwrap().unwrap();
    account.name = "Renamed account".into();
    store.update_account(&account).await.unwrap();
    store
        .upsert_folder(s.account, "Archive", None)
        .await
        .unwrap();
    let after = catch_up(store, s.archive, folder).await;
    catch_up(store, s.archive, role).await;
    assert_eq!(after.uidvalidity, before.uidvalidity);
    assert_eq!(
        uid_of(store, s.archive, folder, m).await,
        Some(uid),
        "a view keyed by folder id keeps its UIDs whatever the folder is called or does"
    );
    assert_eq!(
        uid_of(store, s.archive, role, m).await,
        None,
        "a folder that loses the role leaves the role view"
    );

    store
        .upsert_folder(s.account, "Archive", Some(FolderRole::Archive))
        .await
        .unwrap();
    catch_up(store, s.archive, role).await;
    let back = uid_of(store, s.archive, role, m).await.unwrap();
    assert!(back > role_uid);
    assert_eq!(uid_of(store, s.archive, folder, m).await, Some(uid));
}

async fn all_and_kept(store: &dyn Store) {
    let s = setup(store, "Served all and kept").await;
    let placed = located(store, &s, s.inbox, 10_600, 1).await;
    let gone = located(store, &s, s.inbox, 10_601, 2).await;
    let never = message(store, s.archive, 10_602).await;
    store.mark_removed(s.inbox, &[2], t(1)).await.unwrap();
    built(store, s.archive, ServedKey::All).await;
    built(store, s.archive, ServedKey::Kept).await;
    assert_eq!(
        messages(&uids(store, s.archive, ServedKey::All).await),
        [placed, gone, never]
    );
    assert_eq!(
        messages(&uids(store, s.archive, ServedKey::Kept).await),
        [gone, never]
    );

    store
        .enqueue_mail_change(&NewMailChange {
            account_id: s.account,
            folder_id: s.inbox,
            message_id: Some(placed),
            uid: Some(1),
            op: MailOp::Delete { expunge: true },
            origin: MailChangeOrigin::User,
            user_id: None,
            device_id: None,
            outbox_id: None,
            at: t(2),
        })
        .await
        .unwrap();
    catch_up(store, s.archive, ServedKey::Kept).await;
    catch_up(store, s.archive, ServedKey::All).await;
    assert_eq!(
        messages(&uids(store, s.archive, ServedKey::Kept).await),
        [gone, never, placed],
        "an expunge leaves only Holtmail's copy"
    );
    assert_eq!(uids(store, s.archive, ServedKey::All).await.len(), 3);

    store
        .record_location(never, s.other, Some(1), &[], t(3))
        .await
        .unwrap();
    catch_up(store, s.archive, ServedKey::Kept).await;
    assert_eq!(
        messages(&uids(store, s.archive, ServedKey::Kept).await),
        [gone, placed]
    );
}

async fn drafts_view(store: &dyn Store) {
    let s = setup(store, "Served drafts").await;
    let ada = user(store, "served.drafts@example.com").await;
    let bob = user(store, "served.drafts.other@example.com").await;
    let drafts_folder = store
        .upsert_folder(s.account, "Drafts", Some(FolderRole::Drafts))
        .await
        .unwrap()
        .id;
    let upstream = located(store, &s, drafts_folder, 10_700, 1).await;
    let mine = store
        .create_draft(s.archive, ada, &json!({ "subject": "One" }), &[], t(1))
        .await
        .unwrap();
    store
        .create_draft(s.archive, bob, &json!({ "subject": "Theirs" }), &[], t(1))
        .await
        .unwrap();
    let key = ServedKey::Drafts(ada);
    let view = built(store, s.archive, key).await;
    let first = ServedMember::Draft {
        draft: mine.id,
        version: 1,
    };
    let members: HashSet<ServedMember> = uids(store, s.archive, key)
        .await
        .iter()
        .map(|u| u.member)
        .collect();
    assert_eq!(
        members,
        HashSet::from([first, ServedMember::Message(upstream)])
    );
    assert_eq!(
        store
            .served_mailbox(s.archive, ServedKey::Drafts(bob))
            .await
            .unwrap(),
        None,
        "drafts views are per user"
    );

    let saved = store
        .save_draft(mine.id, 1, &json!({ "subject": "Two" }), &[], t(2))
        .await
        .unwrap();
    assert!(matches!(saved, DraftSave::Saved(_)));
    let after = catch_up(store, s.archive, key).await;
    let second = ServedMember::Draft {
        draft: mine.id,
        version: 2,
    };
    let found = store
        .served_uid_of(s.archive, key, &[first, second])
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "a new version replaces the old one");
    assert_eq!(found[&second], view.uidnext, "with a new UID");
    assert_eq!(after.uidnext, view.uidnext + 1);

    store.delete_draft(mine.id).await.unwrap();
    catch_up(store, s.archive, key).await;
    assert_eq!(
        messages(&uids(store, s.archive, key).await),
        [upstream],
        "a deleted draft leaves"
    );
}

async fn local_flags_raise_modseq(store: &dyn Store) {
    let s = setup(store, "Served local flags").await;
    let reader = user(store, "served.local@example.com").await;
    let m = located(store, &s, s.inbox, 10_800, 1).await;
    let key = ServedKey::Folder(s.inbox);
    built(store, s.archive, key).await;
    store
        .set_local_flags(reader, m, &["\\Seen".to_string()], &[], t(1))
        .await
        .unwrap();
    let flagged = store.latest_seq().await.unwrap();
    let view = catch_up(store, s.archive, key).await;
    assert_eq!(view.highest_modseq, flagged);
    assert_eq!(
        uids(store, s.archive, key).await[0].modseq,
        flagged,
        "a local flag raises MODSEQ for everyone reading the shared view"
    );

    store
        .set_subscription(reader, s.archive, &key.name(), false, t(2))
        .await
        .unwrap();
    let view = catch_up(store, s.archive, key).await;
    assert_eq!(
        view.highest_modseq, flagged,
        "a subscription changes no member"
    );
}

async fn local_places_in_role_views(store: &dyn Store) {
    let s = setup(store, "Served local places").await;
    let reader = user(store, "served.places@example.com").await;
    let shared = located(store, &s, s.inbox, 10_900, 1).await;
    let mine = message(store, s.archive, 10_901).await;
    let role = ServedKey::Role(FolderRole::Inbox);
    let folder = ServedKey::Folder(s.inbox);
    built(store, s.archive, role).await;
    built(store, s.archive, folder).await;
    store
        .add_local_place(reader, FolderRole::Inbox, mine, t(1))
        .await
        .unwrap();
    catch_up(store, s.archive, role).await;
    catch_up(store, s.archive, folder).await;
    assert_eq!(
        messages(&uids(store, s.archive, role).await),
        [shared, mine],
        "a local place gets a UID in its role view"
    );
    assert_eq!(
        messages(&uids(store, s.archive, folder).await),
        [shared],
        "not in a folder view"
    );
    let at = uid_of(store, s.archive, role, mine).await.unwrap();
    store
        .remove_local_place(reader, FolderRole::Inbox, mine, t(2))
        .await
        .unwrap();
    let view = catch_up(store, s.archive, role).await;
    assert_eq!(uid_of(store, s.archive, role, mine).await, None);
    assert_eq!(
        store
            .served_vanished(s.archive, role, 0, 0, 100)
            .await
            .unwrap(),
        [at]
    );
    assert!(view.highest_modseq >= store.latest_seq().await.unwrap());
}

async fn concurrent_catch_up(store: Arc<dyn Store>) {
    let s = setup(&*store, "Served concurrency").await;
    let key = ServedKey::Folder(s.inbox);
    built(&*store, s.archive, key).await;
    let writer = {
        let store = store.clone();
        let (archive, inbox) = (s.archive, s.inbox);
        tokio::spawn(async move {
            for n in 0..40u32 {
                let m = store
                    .insert_message(crate::testing::message(archive, 10_900 + n, 1000))
                    .await
                    .unwrap()
                    .message_id;
                store
                    .record_location(m, inbox, Some(u64::from(n) + 1), &[], t(0))
                    .await
                    .unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..6)
        .map(|_| {
            let store = store.clone();
            let archive = s.archive;
            tokio::spawn(async move {
                for _ in 0..20 {
                    store
                        .catch_up_served_mailbox(archive, key, t(0))
                        .await
                        .unwrap();
                    tokio::task::yield_now().await;
                }
            })
        })
        .collect();
    writer.await.unwrap();
    for reader in readers {
        reader.await.unwrap();
    }
    let view = catch_up(&*store, s.archive, key).await;
    let list = uids(&*store, s.archive, key).await;
    assert_eq!(list.len(), 40);
    let distinct: HashSet<u32> = list.iter().map(|u| u.uid).collect();
    assert_eq!(distinct.len(), 40, "no UID handed out twice");
    let members: HashSet<ServedMember> = list.iter().map(|u| u.member).collect();
    assert_eq!(members.len(), 40, "no member twice");
    assert_eq!(view.uidnext, 41);
}

async fn pruning(store: &dyn Store) {
    let s = setup(store, "Served pruning").await;
    let a = located(store, &s, s.inbox, 11_000, 1).await;
    let b = located(store, &s, s.inbox, 11_001, 2).await;
    let key = ServedKey::Folder(s.inbox);
    let lagging = ServedKey::All;
    built(store, s.archive, key).await;
    built(store, s.archive, lagging).await;
    store.mark_removed(s.inbox, &[2], t(1)).await.unwrap();
    let removed = catch_up(store, s.archive, key).await;
    assert_eq!(
        store
            .served_vanished(s.archive, key, 0, 0, 100)
            .await
            .unwrap(),
        [2]
    );
    let all_before = uids(store, s.archive, lagging).await;
    store
        .set_flags(s.inbox, 1, &["\\Flagged".to_string()])
        .await
        .unwrap();

    let through = removed.highest_modseq;
    let pruned = store.prune_served(through, t(2)).await.unwrap();
    assert!(pruned.tombstones >= 1);
    assert!(pruned.stale >= 1);
    let view = store.served_mailbox(s.archive, key).await.unwrap().unwrap();
    assert_eq!(view.lowest_modseq, through);
    assert!(
        store
            .served_vanished(s.archive, key, 0, 0, 100)
            .await
            .unwrap()
            .is_empty(),
        "tombstones at or below the prune point are gone"
    );
    assert_eq!(
        store.prune_served(through, t(3)).await.unwrap().tombstones,
        0,
        "pruning again drops nothing"
    );

    let head = store.latest_seq().await.unwrap();
    let rescanned = catch_up(store, s.archive, lagging).await;
    assert_eq!(rescanned.synced_seq, head);
    let all_after = uids(store, s.archive, lagging).await;
    assert_eq!(
        all_after
            .iter()
            .map(|u| (u.uid, u.member))
            .collect::<Vec<_>>(),
        all_before
            .iter()
            .map(|u| (u.uid, u.member))
            .collect::<Vec<_>>(),
        "a rescan keeps every UID"
    );
    assert!(
        all_after.iter().all(|u| u.modseq == head),
        "a rescan gives every member a new MODSEQ"
    );
    assert_eq!(messages(&all_after), [a, b]);
    let next = store
        .build_served_mailbox(s.archive, key, 10, t(4))
        .await
        .unwrap();
    assert!(next.built);
    assert_eq!(uid_of(store, s.archive, key, a).await, Some(1));

    store.delete_archive(s.archive).await.unwrap();
    assert!(
        store.served_mailboxes(s.archive).await.unwrap().is_empty(),
        "served state goes with its archive"
    );
}
