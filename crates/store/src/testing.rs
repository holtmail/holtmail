//! Conformance suite every [`Store`] implementation runs in its own tests.

use std::sync::Arc;

use holtmail_blob::BlobId;
use holtmail_core::{Address, ArchiveId, MessageId, Timestamp, UserId};
use serde_json::json;

use crate::*;

/// Checks that `store` keeps the [`Store`] contract. Panics on the first violation.
///
/// `store` must be empty when the suite starts.
pub async fn conformance(store: Arc<dyn Store>) {
    archives_and_accounts(&*store).await;
    folders(&*store).await;
    insert_is_idempotent(&*store).await;
    threads_merge_in_any_order(&*store).await;
    lists_page_newest_first(&*store).await;
    locations_follow_the_server(&*store).await;
    extras_and_unread(&*store).await;
    sync_state(&*store).await;
    users(&*store).await;
    sessions(&*store).await;
    removal(&*store).await;
    user_changes_and_grants(&*store).await;
    remote_copies(&*store).await;
    settings(&*store).await;
}

async fn settings(store: &dyn Store) {
    assert_eq!(store.setting("image_proxy").await.unwrap(), None);
    store
        .set_setting("image_proxy", Some("first"))
        .await
        .unwrap();
    store
        .set_setting("image_proxy", Some("second"))
        .await
        .unwrap();
    assert_eq!(
        store.setting("image_proxy").await.unwrap().as_deref(),
        Some("second")
    );
    store.set_setting("image_proxy", None).await.unwrap();
    store.set_setting("image_proxy", None).await.unwrap();
    assert_eq!(store.setting("image_proxy").await.unwrap(), None);
}

/// A message with invented headers; `n` makes its bytes unique.
pub fn message(archive: ArchiveId, n: u32, sent_secs: i64) -> NewMessage {
    NewMessage {
        archive_id: archive,
        blob_id: BlobId::of(format!("message {n}").as_bytes()),
        size: 100 + u64::from(n),
        rfc_message_id: Some(format!("m{n}@example.com")),
        parent_ids: Vec::new(),
        subject: Some(format!("Subject {n}")),
        from: vec![Address {
            name: Some("Ada".into()),
            email: "ada@example.com".into(),
        }],
        to: vec![Address {
            name: None,
            email: "bob@example.org".into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        sent_at: Some(Timestamp::from_secs(sent_secs)),
        received_at: None,
        preview: format!("Preview {n}"),
        attachments: Vec::new(),
    }
}

async fn archive(store: &dyn Store) -> ArchiveId {
    store.create_archive("Test").await.unwrap().id
}

async fn archives_and_accounts(store: &dyn Store) {
    let first = store.create_archive("Personal").await.unwrap();
    let second = store.create_archive("Work").await.unwrap();
    assert_eq!(
        store.get_archive(first.id).await.unwrap(),
        Some(first.clone())
    );
    let names: Vec<_> = store
        .list_archives()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["Personal", "Work"], "archives are not oldest first");
    assert_eq!(
        store.archive_stats(second.id).await.unwrap(),
        ArchiveStats::default()
    );

    let mut account = store
        .create_account(NewAccount {
            archive_id: first.id,
            kind: AccountKind::Imap,
            name: "Mail".into(),
            address: Some("ada@example.com".into()),
            settings: json!({ "host": "imap.example.com", "port": 993 }),
            sealed_secret: Some(vec![1, 2, 3]),
            enabled: true,
        })
        .await
        .unwrap();
    assert_eq!(
        store.get_account(account.id).await.unwrap(),
        Some(account.clone())
    );
    assert_eq!(
        store
            .get_account(account.id)
            .await
            .unwrap()
            .unwrap()
            .sealed_secret,
        Some(vec![1, 2, 3]),
        "sealed secret was not kept"
    );

    account.name = "Old mail".into();
    account.enabled = false;
    account.sealed_secret = None;
    store.update_account(&account).await.unwrap();
    assert_eq!(store.list_accounts(first.id).await.unwrap(), vec![account]);
    assert!(store.list_accounts(second.id).await.unwrap().is_empty());

    let missing = store
        .create_account(NewAccount {
            archive_id: ArchiveId::new(),
            kind: AccountKind::Import,
            name: "Nowhere".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await;
    assert!(
        matches!(missing, Err(StoreError::NotFound(_))),
        "account in a missing archive"
    );
}

async fn import_account(store: &dyn Store, archive: ArchiveId) -> Account {
    store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Import".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
}

async fn folders(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    let again = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    assert_eq!(again.id, inbox.id, "upsert_folder created a second folder");
    assert_eq!(again.role, Some(FolderRole::Inbox), "role was not updated");
    store
        .upsert_folder(account.id, "Archive/2024", Some(FolderRole::Archive))
        .await
        .unwrap();

    let paths: Vec<_> = store
        .list_folders(archive)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.folder.path, f.messages, f.unread))
        .collect();
    assert_eq!(
        paths,
        [("Archive/2024".into(), 0, 0), ("INBOX".to_string(), 0, 0)]
    );
    assert_eq!(
        store.get_folder(inbox.id).await.unwrap().unwrap().path,
        "INBOX"
    );
}

async fn insert_is_idempotent(store: &dyn Store) {
    let archive = archive(store).await;
    let mut new = message(archive, 1, 1_700_000_000);
    new.attachments = vec![
        Attachment {
            ordinal: 0,
            blob_id: BlobId::of(b"logo"),
            filename: Some("logo.png".into()),
            content_type: "image/png".into(),
            size: 4,
            content_id: Some("logo@example.com".into()),
            inline: true,
        },
        Attachment {
            ordinal: 1,
            blob_id: BlobId::of(b"report"),
            filename: Some("report.pdf".into()),
            content_type: "application/pdf".into(),
            size: 6,
            content_id: None,
            inline: false,
        },
    ];
    let first = store.insert_message(new.clone()).await.unwrap();
    assert!(first.created);
    let second = store.insert_message(new.clone()).await.unwrap();
    assert_eq!(
        second.message_id, first.message_id,
        "same bytes made a second message"
    );
    assert!(!second.created);

    let stored = store.get_message(first.message_id).await.unwrap().unwrap();
    assert_eq!(stored.subject.as_deref(), Some("Subject 1"));
    assert_eq!(stored.from, new.from);
    assert_eq!(stored.to, new.to);
    assert_eq!(stored.date, Timestamp::from_secs(1_700_000_000));
    assert_eq!(stored.attachment_count, 1, "inline parts were counted");
    assert_eq!(
        store.attachments(first.message_id).await.unwrap(),
        new.attachments
    );
    assert_eq!(
        store.find_by_blob(archive, &new.blob_id).await.unwrap(),
        Some(first.message_id)
    );

    let other = store.create_archive("Other").await.unwrap().id;
    let mut copy = new.clone();
    copy.archive_id = other;
    let elsewhere = store.insert_message(copy).await.unwrap();
    assert!(
        elsewhere.created,
        "the same bytes in another archive must be a new message"
    );

    let stats = store.archive_stats(archive).await.unwrap();
    assert_eq!(stats.messages, 1);
    assert_eq!(stats.attachments, 2);
    assert_eq!(stats.bytes, new.size);
    assert_eq!(stats.oldest, Some(Timestamp::from_secs(1_700_000_000)));

    let mut undated = message(archive, 2, 0);
    undated.sent_at = None;
    undated.received_at = Some(Timestamp::from_secs(1_600_000_000));
    let undated = store.insert_message(undated).await.unwrap();
    let undated = store
        .get_message(undated.message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        undated.date,
        Timestamp::from_secs(1_600_000_000),
        "date did not fall back"
    );
}

async fn threads_merge_in_any_order(store: &dyn Store) {
    let archive = archive(store).await;
    let root = message(archive, 10, 1_700_000_000);
    let mut reply = message(archive, 11, 1_700_000_100);
    reply.parent_ids = vec!["m10@example.com".into()];
    let mut second_reply = message(archive, 12, 1_700_000_200);
    second_reply.parent_ids = vec!["m10@example.com".into(), "m11@example.com".into()];
    let unrelated = message(archive, 13, 1_700_000_300);

    let second_reply = store.insert_message(second_reply).await.unwrap();
    let reply = store.insert_message(reply).await.unwrap();
    let unrelated = store.insert_message(unrelated).await.unwrap();
    let root = store.insert_message(root).await.unwrap();

    let thread = store
        .get_message(root.message_id)
        .await
        .unwrap()
        .unwrap()
        .thread_id;
    for id in [reply.message_id, second_reply.message_id] {
        assert_eq!(
            store.get_message(id).await.unwrap().unwrap().thread_id,
            thread,
            "replies are not in the root's thread"
        );
    }
    assert_ne!(
        store
            .get_message(unrelated.message_id)
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        thread
    );
    let order: Vec<_> = store
        .thread_messages(thread)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(
        order,
        [root.message_id, reply.message_id, second_reply.message_id]
    );

    let mut stray = message(store.create_archive("Elsewhere").await.unwrap().id, 14, 0);
    stray.parent_ids = vec!["m10@example.com".into()];
    let stray = store.insert_message(stray).await.unwrap();
    assert_ne!(stray.thread_id, thread, "threads crossed archives");
}

async fn lists_page_newest_first(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let folder = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for n in 0..7 {
        let inserted = store
            .insert_message(message(archive, 100 + n, 1_700_000_000 + i64::from(n % 3)))
            .await
            .unwrap();
        if n % 2 == 0 {
            store
                .record_location(
                    inserted.message_id,
                    folder.id,
                    Some(u64::from(n) + 1),
                    &[],
                    Timestamp::now(),
                )
                .await
                .unwrap();
        }
        ids.push(inserted.message_id);
    }

    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .list_messages(&MessageQuery {
                archive_id: archive,
                folder_id: None,
                after: after.clone(),
                limit: 3,
            })
            .await
            .unwrap();
        assert!(page.items.len() <= 3);
        seen.extend(page.items.iter().map(|m| (m.date, m.id)));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 7, "pages lost or repeated messages");
    let mut sorted = seen.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(seen, sorted, "list is not newest first with id tie-break");

    let in_folder = store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id: Some(folder.id),
            after: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert_eq!(in_folder.items.len(), 4);
    assert!(in_folder.next.is_none());

    let picked = store
        .get_messages(&[ids[3], ids[0], MessageId::new()])
        .await
        .unwrap();
    assert_eq!(
        picked.iter().map(|m| m.id).collect::<Vec<_>>(),
        [ids[3], ids[0]],
        "get_messages did not keep the requested order"
    );
}

async fn locations_follow_the_server(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    let a = store
        .insert_message(message(archive, 200, 1))
        .await
        .unwrap()
        .message_id;
    let b = store
        .insert_message(message(archive, 201, 2))
        .await
        .unwrap()
        .message_id;
    let seen = ["\\Seen".to_string()];

    store
        .record_location(a, inbox.id, Some(5), &[], Timestamp::from_secs(10))
        .await
        .unwrap();
    store
        .record_location(a, inbox.id, Some(5), &seen, Timestamp::from_secs(20))
        .await
        .unwrap();
    let [location] = store.locations(a).await.unwrap().try_into().unwrap();
    assert_eq!(location.flags, seen);
    assert_eq!(location.first_seen_at, Timestamp::from_secs(10));
    assert_eq!(location.last_seen_at, Timestamp::from_secs(20));
    assert_eq!(store.message_at_uid(inbox.id, 5).await.unwrap(), Some(a));

    store
        .record_location(b, inbox.id, Some(6), &[], Timestamp::from_secs(20))
        .await
        .unwrap();
    assert_eq!(store.folder_uids(inbox.id).await.unwrap(), [5, 6]);
    let counts = store.list_folders(archive).await.unwrap();
    assert_eq!(counts[0].messages, 2);

    store
        .mark_removed(inbox.id, &[5], Timestamp::from_secs(30))
        .await
        .unwrap();
    assert_eq!(store.folder_uids(inbox.id).await.unwrap(), [6]);
    assert_eq!(
        store.locations(a).await.unwrap()[0].removed_at,
        Some(Timestamp::from_secs(30))
    );
    assert!(
        store.get_message(a).await.unwrap().is_some(),
        "removal deleted the message"
    );

    store
        .record_location(a, inbox.id, Some(5), &[], Timestamp::from_secs(40))
        .await
        .unwrap();
    assert_eq!(store.locations(a).await.unwrap()[0].removed_at, None);

    store.forget_uids(inbox.id).await.unwrap();
    assert!(store.folder_uids(inbox.id).await.unwrap().is_empty());
    assert_eq!(
        store.locations(a).await.unwrap().len(),
        1,
        "forget_uids dropped locations"
    );
    store
        .record_location(b, inbox.id, Some(1), &[], Timestamp::from_secs(50))
        .await
        .unwrap();
    store
        .record_location(a, inbox.id, Some(1), &[], Timestamp::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        store.message_at_uid(inbox.id, 1).await.unwrap(),
        Some(a),
        "a UID did not move to its new message"
    );
    assert_eq!(store.locations(b).await.unwrap()[0].uid, None);
}

async fn extras_and_unread(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    let starred = store
        .upsert_folder(account.id, "Starred", Some(FolderRole::Flagged))
        .await
        .unwrap();
    let parent = store
        .insert_message(message(archive, 300, 1))
        .await
        .unwrap()
        .message_id;
    let mut reply = message(archive, 301, 2);
    reply.parent_ids = vec!["m300@example.com".into()];
    let reply = store.insert_message(reply).await.unwrap().message_id;
    let alone = store
        .insert_message(message(archive, 302, 3))
        .await
        .unwrap()
        .message_id;
    let flags = |list: &[&str]| list.iter().map(|f| f.to_string()).collect::<Vec<_>>();
    let at = Timestamp::from_secs(10);

    store
        .record_location(parent, inbox.id, Some(1), &flags(&["\\seen"]), at)
        .await
        .unwrap();
    store
        .record_location(
            parent,
            starred.id,
            Some(1),
            &flags(&["\\Seen", "\\Flagged"]),
            at,
        )
        .await
        .unwrap();
    store
        .record_location(reply, inbox.id, Some(2), &[], at)
        .await
        .unwrap();
    store
        .record_location(alone, inbox.id, Some(3), &flags(&["$label1"]), at)
        .await
        .unwrap();
    store
        .record_location(alone, starred.id, Some(3), &flags(&["\\Flagged"]), at)
        .await
        .unwrap();
    store
        .mark_removed(starred.id, &[3], Timestamp::from_secs(20))
        .await
        .unwrap();

    let counts: Vec<_> = store
        .list_folders(archive)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.folder.path, f.messages, f.unread))
        .collect();
    assert_eq!(
        counts,
        [("INBOX".into(), 3, 2), ("Starred".to_string(), 1, 0)],
        "unread counts are wrong"
    );

    let missing = MessageId::new();
    let all = store
        .message_extras(&[parent, reply, alone, missing], None)
        .await
        .unwrap();
    assert_eq!(all.len(), 3, "extras for a missing message");
    let mut both = vec![inbox.id, starred.id];
    both.sort();
    assert_eq!(
        all[&parent],
        MessageExtras {
            flags: flags(&["\\Flagged", "\\Seen", "\\seen"]),
            thread_count: 2,
            folder_ids: both,
        }
    );
    assert_eq!(all[&reply].thread_count, 2);
    assert!(all[&reply].flags.is_empty());
    assert_eq!(
        all[&alone],
        MessageExtras {
            flags: flags(&["$label1"]),
            thread_count: 1,
            folder_ids: vec![inbox.id],
        },
        "a removed location counted"
    );

    let in_inbox = store
        .message_extras(&[parent, alone], Some(inbox.id))
        .await
        .unwrap();
    assert_eq!(in_inbox[&parent].flags, flags(&["\\seen"]));
    assert_eq!(in_inbox[&parent].folder_ids.len(), 2);
    let in_starred = store
        .message_extras(&[alone], Some(starred.id))
        .await
        .unwrap();
    assert!(in_starred[&alone].flags.is_empty());
    assert!(store.message_extras(&[], None).await.unwrap().is_empty());
}

async fn sync_state(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let folder = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    assert_eq!(
        store.cursor(folder.id).await.unwrap(),
        FolderCursor::default()
    );
    let cursor = FolderCursor {
        uid_validity: Some(42),
        last_uid: Some(100),
        highest_modseq: Some(7),
        state: Some("opaque".into()),
        synced_at: Some(Timestamp::from_secs(1)),
    };
    store.set_cursor(folder.id, &cursor).await.unwrap();
    assert_eq!(store.cursor(folder.id).await.unwrap(), cursor);

    store
        .record_failure(folder.id, 9, "parse error", Timestamp::from_secs(1))
        .await
        .unwrap();
    store
        .record_failure(folder.id, 9, "timeout", Timestamp::from_secs(2))
        .await
        .unwrap();
    let [failure] = store.failures(folder.id).await.unwrap().try_into().unwrap();
    assert_eq!(failure.attempts, 2);
    assert_eq!(failure.reason, "timeout");
    assert_eq!(failure.failed_at, Timestamp::from_secs(2));
    store.clear_failure(folder.id, 9).await.unwrap();
    assert!(store.failures(folder.id).await.unwrap().is_empty());
}

fn new_user(email: &str, role: InstanceRole) -> NewUser {
    NewUser {
        email: email.into(),
        name: "Ada Lovelace".into(),
        role,
        password_hash: Some("$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaA".into()),
    }
}

async fn users(store: &dyn Store) {
    assert_eq!(
        store.count_users().await.unwrap(),
        0,
        "the store is not empty"
    );
    let ada = store
        .create_user(new_user("Ada@Example.com", InstanceRole::Owner))
        .await
        .unwrap();
    assert_eq!(
        ada.email, "Ada@Example.com",
        "email was not kept as entered"
    );
    assert_eq!(ada.role, InstanceRole::Owner);
    assert_eq!(store.count_users().await.unwrap(), 1);
    assert_eq!(store.get_user(ada.id).await.unwrap(), Some(ada.clone()));
    assert_eq!(
        store.find_user_by_email("ada@EXAMPLE.com").await.unwrap(),
        Some(ada.clone()),
        "email lookup is case-sensitive"
    );
    assert_eq!(
        store.find_user_by_email("bob@example.com").await.unwrap(),
        None
    );
    assert!(store.get_user(UserId::new()).await.unwrap().is_none());

    let duplicate = store
        .create_user(new_user("ADA@example.COM", InstanceRole::Member))
        .await;
    assert!(
        matches!(duplicate, Err(StoreError::Invalid(_))),
        "a second user with the same email"
    );
    assert_eq!(store.count_users().await.unwrap(), 1);

    let mut no_password = new_user("bob@example.org", InstanceRole::Member);
    no_password.password_hash = None;
    let bob = store.create_user(no_password).await.unwrap();
    assert_eq!(store.password_hash(bob.id).await.unwrap(), None);
    assert_eq!(
        store.password_hash(ada.id).await.unwrap().as_deref(),
        Some("$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaA")
    );
    store.set_password_hash(bob.id, "$new").await.unwrap();
    assert_eq!(
        store.password_hash(bob.id).await.unwrap().as_deref(),
        Some("$new")
    );
    assert!(matches!(
        store.password_hash(UserId::new()).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.set_password_hash(UserId::new(), "$x").await,
        Err(StoreError::NotFound(_))
    ));
    let ids: Vec<_> = store
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .map(|u| u.id)
        .collect();
    assert_eq!(ids, [ada.id, bob.id], "users are not oldest first");
}

async fn sessions(store: &dyn Store) {
    let user = store
        .create_user(new_user("carol@example.com", InstanceRole::Admin))
        .await
        .unwrap();
    let other = store
        .create_user(new_user("dave@example.com", InstanceRole::Member))
        .await
        .unwrap();
    let live = [1u8; 32];
    let short = [2u8; 32];
    let others = [3u8; 32];
    store
        .create_session(&live, user.id, Timestamp::from_secs(1_000))
        .await
        .unwrap();
    store
        .create_session(&short, user.id, Timestamp::from_secs(100))
        .await
        .unwrap();
    store
        .create_session(&others, other.id, Timestamp::from_secs(1_000))
        .await
        .unwrap();
    assert!(
        matches!(
            store
                .create_session(&live, other.id, Timestamp::from_secs(5_000))
                .await,
            Err(StoreError::Invalid(_))
        ),
        "a session hash was reused"
    );
    assert!(matches!(
        store
            .create_session(&[4u8; 32], UserId::new(), Timestamp::from_secs(1_000))
            .await,
        Err(StoreError::NotFound(_))
    ));

    let at = |secs| Timestamp::from_secs(secs);
    assert_eq!(
        store.session_user(&live, at(50)).await.unwrap(),
        Some(user.clone())
    );
    assert_eq!(
        store.session_user(&short, at(99)).await.unwrap(),
        Some(user.clone())
    );
    assert_eq!(
        store.session_user(&short, at(100)).await.unwrap(),
        None,
        "an expired session still signs in"
    );
    assert_eq!(store.session_user(&[9u8; 32], at(0)).await.unwrap(), None);

    assert_eq!(store.delete_expired_sessions(at(500)).await.unwrap(), 1);
    assert_eq!(store.delete_expired_sessions(at(500)).await.unwrap(), 0);
    assert_eq!(store.session_user(&short, at(0)).await.unwrap(), None);

    store.delete_session(&live).await.unwrap();
    store.delete_session(&live).await.unwrap();
    assert_eq!(store.session_user(&live, at(50)).await.unwrap(), None);

    store
        .create_session(&live, user.id, at(1_000))
        .await
        .unwrap();
    store.delete_user_sessions(user.id).await.unwrap();
    assert_eq!(store.session_user(&live, at(50)).await.unwrap(), None);
    assert_eq!(
        store.session_user(&others, at(50)).await.unwrap(),
        Some(other),
        "delete_user_sessions ended another user's session"
    );
}

async fn removal(store: &dyn Store) {
    let gone = store.create_archive("Old").await.unwrap().id;
    let kept = store.create_archive("Kept").await.unwrap().id;
    store.rename_archive(kept, "Kept mail").await.unwrap();
    assert_eq!(
        store.get_archive(kept).await.unwrap().unwrap().name,
        "Kept mail"
    );
    assert!(matches!(
        store.rename_archive(ArchiveId::new(), "x").await,
        Err(StoreError::NotFound(_))
    ));

    let attachment = BlobId::of(b"attachment only in the old archive");
    let mut shared = message(gone, 9001, 1_700_000_000);
    shared.attachments.push(Attachment {
        ordinal: 0,
        blob_id: attachment,
        filename: Some("a.txt".into()),
        content_type: "text/plain".into(),
        size: 3,
        content_id: None,
        inline: false,
    });
    let own = message(gone, 9002, 1_700_000_001);
    let own_blob = own.blob_id;
    let in_gone = store.insert_message(shared).await.unwrap().message_id;
    store.insert_message(own).await.unwrap();
    let in_kept = store
        .insert_message(message(kept, 9001, 1_700_000_000))
        .await
        .unwrap()
        .message_id;

    let account = import_account(store, gone).await;
    let folder = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    store
        .record_location(in_gone, folder, Some(1), &[], Timestamp::from_secs(1))
        .await
        .unwrap();
    store
        .set_cursor(
            folder,
            &FolderCursor {
                last_uid: Some(1),
                ..FolderCursor::default()
            },
        )
        .await
        .unwrap();
    store
        .record_failure(folder, 2, "broken", Timestamp::from_secs(1))
        .await
        .unwrap();

    let mut orphans = vec![attachment, own_blob];
    orphans.sort();
    assert_eq!(
        store.delete_archive(gone).await.unwrap(),
        orphans,
        "orphaned blobs are wrong; a blob still used by another archive must not be listed"
    );
    assert!(store.get_archive(gone).await.unwrap().is_none());
    assert!(store.get_account(account.id).await.unwrap().is_none());
    assert!(store.get_message(in_gone).await.unwrap().is_none());
    assert!(store.get_message(in_kept).await.unwrap().is_some());
    assert!(matches!(
        store.delete_archive(gone).await,
        Err(StoreError::NotFound(_))
    ));

    let account = import_account(store, kept).await;
    let folder = store
        .upsert_folder(account.id, "Sent", None)
        .await
        .unwrap()
        .id;
    store
        .record_location(in_kept, folder, Some(5), &[], Timestamp::from_secs(1))
        .await
        .unwrap();
    store
        .record_failure(folder, 6, "broken", Timestamp::from_secs(1))
        .await
        .unwrap();
    store.delete_account(account.id).await.unwrap();
    assert!(store.get_account(account.id).await.unwrap().is_none());
    assert!(store.list_folders(kept).await.unwrap().is_empty());
    assert!(
        store.get_message(in_kept).await.unwrap().is_some(),
        "removing an account removed its messages"
    );
    assert!(store.locations(in_kept).await.unwrap().is_empty());
    assert!(matches!(
        store.delete_account(account.id).await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_archive(kept).await.unwrap();
}

async fn user_changes_and_grants(store: &dyn Store) {
    let grace = store
        .create_user(new_user("grace@example.com", InstanceRole::Member))
        .await
        .unwrap();
    let alan = store
        .create_user(new_user("alan@example.org", InstanceRole::Member))
        .await
        .unwrap();
    let changed = User {
        email: "Grace@Example.org".into(),
        name: "Grace Hopper".into(),
        role: InstanceRole::Admin,
        ..grace.clone()
    };
    store.update_user(&changed).await.unwrap();
    assert_eq!(store.get_user(grace.id).await.unwrap().unwrap(), changed);
    assert_eq!(
        store
            .find_user_by_email("grace@example.org")
            .await
            .unwrap()
            .map(|u| u.id),
        Some(grace.id)
    );
    assert!(matches!(
        store
            .update_user(&User {
                email: "ALAN@example.org".into(),
                ..changed.clone()
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store
            .update_user(&User {
                id: UserId::new(),
                ..changed.clone()
            })
            .await,
        Err(StoreError::NotFound(_))
    ));

    let first = store.create_archive("First").await.unwrap().id;
    let second = store.create_archive("Second").await.unwrap().id;
    assert!(store.grants(alan.id).await.unwrap().is_empty());
    store
        .set_grants(alan.id, &[second, ArchiveId::new(), first, first])
        .await
        .unwrap();
    assert_eq!(store.grants(alan.id).await.unwrap(), [first, second]);
    store.set_grants(alan.id, &[second]).await.unwrap();
    assert_eq!(store.grants(alan.id).await.unwrap(), [second]);
    assert!(matches!(
        store.set_grants(UserId::new(), &[first]).await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_archive(second).await.unwrap();
    assert!(
        store.grants(alan.id).await.unwrap().is_empty(),
        "a removed archive stayed granted"
    );

    store.set_grants(alan.id, &[first]).await.unwrap();
    let hash: SessionHash = [42; 32];
    store
        .create_session(&hash, alan.id, Timestamp::from_secs(4_000_000_000))
        .await
        .unwrap();
    store.delete_user(alan.id).await.unwrap();
    assert!(store.get_user(alan.id).await.unwrap().is_none());
    assert!(
        store
            .session_user(&hash, Timestamp::from_secs(1))
            .await
            .unwrap()
            .is_none(),
        "a removed user kept a session"
    );
    assert!(store.grants(alan.id).await.unwrap().is_empty());
    assert!(matches!(
        store.delete_user(alan.id).await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_archive(first).await.unwrap();
}

async fn remote_copies(store: &dyn Store) {
    let first = store.create_archive("Newsletters").await.unwrap();
    assert!(
        !first.save_remote_images,
        "saving remote images is on by default"
    );
    assert_eq!(first.save_remote_images_since, None);
    assert!(matches!(
        store.set_save_remote_images(ArchiveId::new(), true).await,
        Err(StoreError::NotFound(_))
    ));
    let second = store.create_archive("Elsewhere").await.unwrap().id;

    let a = store
        .insert_message(message(first.id, 9101, 1_700_000_000))
        .await
        .unwrap()
        .message_id;
    let b = store
        .insert_message(message(first.id, 9102, 1_700_000_001))
        .await
        .unwrap()
        .message_id;
    let c = store
        .insert_message(message(second, 9103, 1_700_000_002))
        .await
        .unwrap()
        .message_id;
    let all = ArchivedWhen::Any;
    assert_eq!(
        store.unscanned_messages(first.id, all, 10).await.unwrap(),
        [a, b]
    );
    assert_eq!(store.count_unscanned(first.id, all).await.unwrap(), 2);

    std::thread::sleep(std::time::Duration::from_millis(5));
    store.set_save_remote_images(first.id, true).await.unwrap();
    let since = store
        .get_archive(first.id)
        .await
        .unwrap()
        .unwrap()
        .save_remote_images_since
        .expect("turning saving on records when");
    store.set_save_remote_images(first.id, true).await.unwrap();
    assert_eq!(
        store
            .get_archive(first.id)
            .await
            .unwrap()
            .unwrap()
            .save_remote_images_since,
        Some(since),
        "turning saving on again moved its start"
    );
    let later = store
        .insert_message(message(first.id, 9104, 1_600_000_000))
        .await
        .unwrap()
        .message_id;
    assert_eq!(
        store
            .unscanned_messages(first.id, ArchivedWhen::Since(since), 10)
            .await
            .unwrap(),
        [later],
        "messages are picked by when they were archived, not sent"
    );
    assert_eq!(
        store
            .count_unscanned(first.id, ArchivedWhen::Before(since))
            .await
            .unwrap(),
        2
    );
    store.record_remote_scan(later, &[]).await.unwrap();

    let logo_blob = BlobId::of(b"logo bytes");
    let logo = RemoteCopy {
        key: "logo".into(),
        url: "https://example.com/logo.png".into(),
        blob_id: Some(logo_blob),
        content_type: Some("image/png".into()),
        size: 10,
        fetched_at: Timestamp::from_secs(1),
        error: None,
    };
    let broken = RemoteCopy {
        key: "broken".into(),
        url: "https://example.org/gone.png".into(),
        blob_id: None,
        content_type: None,
        size: 0,
        fetched_at: Timestamp::from_secs(1),
        error: Some("HTTP 404".into()),
    };
    store.save_remote_copy(&logo).await.unwrap();
    store.save_remote_copy(&broken).await.unwrap();
    store.save_remote_copy(&broken).await.unwrap();
    assert_eq!(store.remote_copy("logo").await.unwrap(), Some(logo.clone()));
    assert_eq!(store.remote_copy("missing").await.unwrap(), None);

    let keys = ["logo".to_string(), "broken".into(), "unknown".into()];
    store.record_remote_scan(a, &keys).await.unwrap();
    store.record_remote_scan(a, &keys).await.unwrap();
    store.record_remote_scan(c, &keys[..1]).await.unwrap();
    assert!(matches!(
        store.record_remote_scan(MessageId::new(), &[]).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store.unscanned_messages(first.id, all, 10).await.unwrap(),
        [b]
    );
    assert_eq!(
        store.remote_copy_totals(first.id).await.unwrap(),
        RemoteCopyTotals {
            images: 1,
            bytes: 10,
            failed: 1,
        },
        "copies counted twice or in the wrong column"
    );
    assert_eq!(
        store
            .failed_remote_copies(first.id, Timestamp::from_secs(2), 10)
            .await
            .unwrap(),
        std::slice::from_ref(&broken)
    );
    assert!(
        store
            .failed_remote_copies(first.id, Timestamp::from_secs(1), 10)
            .await
            .unwrap()
            .is_empty(),
        "a copy tried after the cut-off was listed"
    );
    assert_eq!(
        store.message_remote_copies(a).await.unwrap(),
        std::slice::from_ref(&logo),
        "only copies with saved bytes are listed"
    );
    assert!(store.message_remote_copies(b).await.unwrap().is_empty());

    store.set_save_remote_images(first.id, false).await.unwrap();
    let off = store.get_archive(first.id).await.unwrap().unwrap();
    assert!(!off.save_remote_images);
    assert_eq!(off.save_remote_images_since, None);

    let orphans = store.delete_archive(first.id).await.unwrap();
    assert!(
        !orphans.contains(&logo_blob),
        "a copy another archive still links was orphaned"
    );
    assert_eq!(store.remote_copy("broken").await.unwrap(), None);
    assert_eq!(store.remote_copy("logo").await.unwrap(), Some(logo));

    let orphans = store.delete_archive(second).await.unwrap();
    assert!(
        orphans.contains(&logo_blob),
        "the last link did not free the copy"
    );
    assert_eq!(store.remote_copy("logo").await.unwrap(), None);
}
