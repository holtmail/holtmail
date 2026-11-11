use holtmail_blob::BlobId;
use holtmail_core::{
    ArchiveId, DraftId, IdentityId, MessageId, OutboxId, Timestamp, TransportId, UserId,
};
use serde_json::json;

use crate::*;

pub(super) async fn run(store: &dyn Store) {
    transports(store).await;
    identities(store).await;
    drafts(store).await;
    outbox_queue(store).await;
    outbox_held_for_changes(store).await;
    outbox_claims(store).await;
    outbox_copies(store).await;
    outbox_removal(store).await;
    raw_drafts_and_log(store).await;
}

fn smtp(archive: Option<ArchiveId>) -> Transport {
    Transport {
        id: TransportId::new(),
        archive_id: archive,
        name: "Relay".into(),
        kind: TransportKind::Smtp,
        account_id: None,
        settings: json!({ "host": "smtp.example.com", "port": 587 }),
        sealed_secret: Some(vec![1, 2, 3]),
        allowed_from: vec!["*@example.com".into()],
        limits: TransportLimits {
            max_size: Some(10 << 20),
            max_recipients: Some(50),
            per_hour: Some(100),
        },
        enabled: true,
        smtputf8: false,
        created_at: Timestamp::from_secs(1_700_000_000),
    }
}

async fn account(store: &dyn Store, archive: ArchiveId) -> Account {
    store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Sender".into(),
            address: Some("ada@example.com".into()),
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
}

async fn user(store: &dyn Store, email: &str) -> UserId {
    store
        .create_user(NewUser {
            email: email.into(),
            name: "Sender".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id
}

fn new_identity(archive: ArchiveId, transport: TransportId, by: Option<UserId>) -> NewIdentity {
    NewIdentity {
        archive_id: archive,
        address: "ada@example.com".into(),
        name: Some("Ada Example".into()),
        signature_html: Some("<p>Ada</p>".into()),
        signature_text: Some("Ada".into()),
        reply_to: None,
        transport_id: transport,
        sent_copy: SentCopyPolicy::Append("Sent".into()),
        bcc_self: false,
        account_id: None,
        alias: None,
        plus_addresses: true,
        active: true,
        created_by: by,
    }
}

fn invalid<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::Invalid(_)))
}

fn not_found<T: std::fmt::Debug>(result: Result<T, StoreError>) -> bool {
    matches!(result, Err(StoreError::NotFound(_)))
}

async fn transports(store: &dyn Store) {
    let archive = store.create_archive("Transports").await.unwrap().id;
    let other = store.create_archive("Transports 2").await.unwrap().id;
    let own = smtp(Some(archive));
    store.create_transport(&own).await.unwrap();
    assert_eq!(
        store.get_transport(own.id).await.unwrap(),
        Some(own.clone())
    );
    assert!(
        invalid(store.create_transport(&own).await),
        "an id was taken twice"
    );
    assert!(not_found(
        store.create_transport(&smtp(Some(ArchiveId::new()))).await
    ));
    assert!(
        store
            .get_transport(TransportId::new())
            .await
            .unwrap()
            .is_none()
    );

    let mine = account(store, archive).await;
    let foreign = account(store, other).await;
    let via_account = |account| Transport {
        kind: TransportKind::Account,
        account_id: account,
        ..smtp(Some(archive))
    };
    assert!(invalid(store.create_transport(&via_account(None)).await));
    assert!(invalid(
        store.create_transport(&via_account(Some(foreign.id))).await
    ));
    assert!(invalid(
        store
            .create_transport(&Transport {
                account_id: Some(mine.id),
                ..smtp(Some(archive))
            })
            .await
    ));
    assert!(invalid(
        store
            .create_transport(&Transport {
                kind: TransportKind::Account,
                account_id: Some(mine.id),
                ..smtp(None)
            })
            .await
    ));
    let linked = via_account(Some(mine.id));
    store.create_transport(&linked).await.unwrap();
    store.set_transport_smtputf8(linked.id, true).await.unwrap();
    store.update_transport(&linked).await.unwrap();
    assert!(
        store
            .get_transport(linked.id)
            .await
            .unwrap()
            .unwrap()
            .smtputf8,
        "an edit keeps what the check found"
    );
    store
        .set_transport_smtputf8(linked.id, false)
        .await
        .unwrap();
    assert!(matches!(
        store.set_transport_smtputf8(TransportId::new(), true).await,
        Err(StoreError::NotFound(_))
    ));

    let shared = smtp(None);
    store.create_transport(&shared).await.unwrap();
    assert_eq!(
        store.instance_transports().await.unwrap(),
        std::slice::from_ref(&shared)
    );
    let ids = |list: Vec<Transport>| list.into_iter().map(|t| t.id).collect::<Vec<_>>();
    let mut expected = vec![own.id, linked.id];
    expected.sort();
    let mut listed = ids(store.archive_transports(archive).await.unwrap());
    listed.sort();
    assert_eq!(
        listed, expected,
        "an ungranted instance transport was listed"
    );

    assert!(invalid(store.set_transport_grants(own.id, &[other]).await));
    assert!(not_found(
        store
            .set_transport_grants(shared.id, &[ArchiveId::new()])
            .await
    ));
    assert!(not_found(
        store.set_transport_grants(TransportId::new(), &[]).await
    ));
    store
        .set_transport_grants(shared.id, &[archive, other, archive])
        .await
        .unwrap();
    let mut granted = vec![archive, other];
    granted.sort();
    assert_eq!(store.transport_grants(shared.id).await.unwrap(), granted);
    assert!(ids(store.archive_transports(other).await.unwrap()).contains(&shared.id));
    assert!(store.transport_grants(own.id).await.unwrap().is_empty());

    let identity = store
        .create_identity(new_identity(other, shared.id, None))
        .await
        .unwrap();
    assert!(
        invalid(store.set_transport_grants(shared.id, &[archive]).await),
        "a grant in use was taken away"
    );
    assert!(invalid(store.delete_transport(shared.id).await));

    let changed = Transport {
        archive_id: Some(other),
        kind: TransportKind::Account,
        name: "Renamed".into(),
        allowed_from: vec!["ada@example.com".into()],
        limits: TransportLimits::default(),
        enabled: false,
        sealed_secret: None,
        created_at: Timestamp::from_secs(1),
        ..own.clone()
    };
    store.update_transport(&changed).await.unwrap();
    let stored = store.get_transport(own.id).await.unwrap().unwrap();
    assert_eq!(
        stored.archive_id,
        Some(archive),
        "update moved the transport"
    );
    assert_eq!(stored.kind, TransportKind::Smtp);
    assert_eq!(stored.created_at, own.created_at);
    assert_eq!(stored.name, "Renamed");
    assert_eq!(stored.allowed_from, ["ada@example.com"]);
    assert_eq!(stored.limits, TransportLimits::default());
    assert!(!stored.enabled && stored.sealed_secret.is_none());
    assert!(not_found(
        store.update_transport(&smtp(Some(archive))).await
    ));

    store.delete_account(mine.id).await.unwrap();
    assert_eq!(
        store
            .get_transport(linked.id)
            .await
            .unwrap()
            .unwrap()
            .account_id,
        None,
        "a transport kept a removed account"
    );

    store.delete_identity(identity.id).await.unwrap();
    store.delete_transport(shared.id).await.unwrap();
    assert!(store.get_transport(shared.id).await.unwrap().is_none());
    assert!(store.transport_grants(shared.id).await.unwrap().is_empty());
    assert!(not_found(store.delete_transport(shared.id).await));

    let kept = smtp(None);
    store.create_transport(&kept).await.unwrap();
    store
        .set_transport_grants(kept.id, &[archive])
        .await
        .unwrap();
    store.delete_archive(archive).await.unwrap();
    assert!(store.get_transport(own.id).await.unwrap().is_none());
    assert!(
        store.get_transport(kept.id).await.unwrap().is_some(),
        "an instance transport went with an archive"
    );
    assert!(store.transport_grants(kept.id).await.unwrap().is_empty());
    store.delete_transport(kept.id).await.unwrap();
    store.delete_archive(other).await.unwrap();
    for kind in [TransportKind::Smtp, TransportKind::Account] {
        assert_eq!(TransportKind::parse(kind.as_str()), Some(kind));
    }
}

async fn identities(store: &dyn Store) {
    let archive = store.create_archive("Identities").await.unwrap().id;
    let other = store.create_archive("Identities 2").await.unwrap().id;
    let transport = smtp(Some(archive));
    store.create_transport(&transport).await.unwrap();
    let foreign_transport = smtp(Some(other));
    store.create_transport(&foreign_transport).await.unwrap();
    let ada = user(store, "ada.identities@example.com").await;
    let lin = user(store, "lin.identities@example.com").await;
    let mine = account(store, archive).await;
    let foreign = account(store, other).await;

    let identity = store
        .create_identity(NewIdentity {
            account_id: Some(mine.id),
            ..new_identity(archive, transport.id, Some(ada))
        })
        .await
        .unwrap();
    assert_eq!(
        store.get_identity(identity.id).await.unwrap(),
        Some(identity.clone())
    );
    assert_eq!(store.identity_users(identity.id).await.unwrap(), [ada]);
    assert_eq!(
        store.user_identities(ada, archive).await.unwrap(),
        std::slice::from_ref(&identity)
    );
    assert!(
        store
            .user_identities(lin, archive)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.user_identities(ada, other).await.unwrap().is_empty());
    assert!(invalid(
        store
            .create_identity(new_identity(archive, foreign_transport.id, None))
            .await
    ));
    assert!(invalid(
        store
            .create_identity(NewIdentity {
                account_id: Some(foreign.id),
                ..new_identity(archive, transport.id, None)
            })
            .await
    ));
    assert!(not_found(
        store
            .create_identity(new_identity(archive, TransportId::new(), None))
            .await
    ));
    assert!(not_found(
        store
            .create_identity(new_identity(ArchiveId::new(), transport.id, None))
            .await
    ));

    let second = store
        .create_identity(NewIdentity {
            address: "ada+shop@example.com".into(),
            sent_copy: SentCopyPolicy::Provider,
            alias: Some("shop@example.org".into()),
            ..new_identity(archive, transport.id, None)
        })
        .await
        .unwrap();
    assert!(store.identity_users(second.id).await.unwrap().is_empty());
    assert_eq!(
        store
            .list_identities(archive)
            .await
            .unwrap()
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        [identity.id, second.id]
    );

    store
        .set_identity_users(second.id, &[lin, ada, lin])
        .await
        .unwrap();
    let mut both = vec![ada, lin];
    both.sort();
    assert_eq!(store.identity_users(second.id).await.unwrap(), both);
    assert!(not_found(
        store.set_identity_users(second.id, &[UserId::new()]).await
    ));
    assert!(not_found(
        store.set_identity_users(IdentityId::new(), &[]).await
    ));

    let mut changed = second.clone();
    changed.fields.archive_id = other;
    changed.fields.created_by = Some(lin);
    changed.fields.name = None;
    changed.fields.sent_copy = SentCopyPolicy::None;
    changed.fields.active = false;
    changed.fields.bcc_self = true;
    store.update_identity(&changed).await.unwrap();
    let stored = store.get_identity(second.id).await.unwrap().unwrap();
    assert_eq!(
        stored.fields.archive_id, archive,
        "update moved the identity"
    );
    assert_eq!(stored.fields.created_by, None);
    assert_eq!(stored.fields.sent_copy, SentCopyPolicy::None);
    assert!(stored.fields.bcc_self && !stored.fields.active && stored.fields.name.is_none());
    let mut moved = stored.clone();
    moved.fields.transport_id = foreign_transport.id;
    assert!(invalid(store.update_identity(&moved).await));
    assert!(not_found(
        store
            .update_identity(&Identity {
                id: IdentityId::new(),
                ..stored.clone()
            })
            .await
    ));

    assert_eq!(store.default_identity(ada, archive).await.unwrap(), None);
    store
        .set_default_identity(ada, archive, Some(second.id))
        .await
        .unwrap();
    store
        .set_default_identity(ada, archive, Some(identity.id))
        .await
        .unwrap();
    assert_eq!(
        store.default_identity(ada, archive).await.unwrap(),
        Some(identity.id)
    );
    assert!(invalid(
        store
            .set_default_identity(ada, other, Some(identity.id))
            .await
    ));
    assert!(not_found(
        store
            .set_default_identity(ada, archive, Some(IdentityId::new()))
            .await
    ));
    assert!(not_found(
        store
            .set_default_identity(UserId::new(), archive, None)
            .await
    ));

    store.delete_account(mine.id).await.unwrap();
    assert_eq!(
        store
            .get_identity(identity.id)
            .await
            .unwrap()
            .unwrap()
            .fields
            .account_id,
        None
    );
    store.delete_identity(identity.id).await.unwrap();
    assert!(not_found(store.delete_identity(identity.id).await));
    assert_eq!(store.default_identity(ada, archive).await.unwrap(), None);
    assert!(store.identity_users(identity.id).await.unwrap().is_empty());

    store.delete_user(lin).await.unwrap();
    assert_eq!(store.identity_users(second.id).await.unwrap(), [ada]);
    store.delete_archive(archive).await.unwrap();
    assert!(store.get_identity(second.id).await.unwrap().is_none());
    assert!(
        store
            .user_identities(ada, archive)
            .await
            .unwrap()
            .is_empty()
    );
    store.delete_archive(other).await.unwrap();
    store.delete_user(ada).await.unwrap();
    for policy in [
        SentCopyPolicy::Provider,
        SentCopyPolicy::None,
        SentCopyPolicy::Append("Sent Items".into()),
    ] {
        assert_eq!(
            SentCopyPolicy::from_stored(&policy.to_stored()),
            Some(policy)
        );
    }
    assert_eq!(SentCopyPolicy::from_stored("append:"), None);
}

async fn drafts(store: &dyn Store) {
    let archive = store.create_archive("Drafts").await.unwrap().id;
    let ada = user(store, "ada.drafts@example.com").await;
    let lin = user(store, "lin.drafts@example.com").await;
    let photo = BlobId::of(b"draft attachment");
    let t = Timestamp::from_secs;

    let draft = store
        .create_draft(
            archive,
            ada,
            &json!({ "subject": "Plans" }),
            &[photo],
            t(10),
        )
        .await
        .unwrap();
    assert_eq!(draft.current.version, 1);
    assert_eq!(draft.current.blobs, [photo]);
    assert_eq!(draft.created_at, t(10));
    assert_eq!(
        store.get_draft(draft.id).await.unwrap(),
        Some(draft.clone())
    );
    assert!(not_found(
        store
            .create_draft(ArchiveId::new(), ada, &json!({}), &[], t(10))
            .await
    ));
    assert!(not_found(
        store
            .create_draft(archive, UserId::new(), &json!({}), &[], t(10))
            .await
    ));

    let referenced = store.referenced_blobs(None, 1000).await.unwrap();
    assert!(
        referenced.contains(&photo),
        "a draft's blob was not referenced"
    );

    let saved = match store
        .save_draft(draft.id, 1, &json!({ "subject": "Plans v2" }), &[], t(20))
        .await
        .unwrap()
    {
        DraftSave::Saved(saved) => saved,
        DraftSave::Conflict(_) => panic!("a fresh save conflicted"),
    };
    assert_eq!(saved.current.version, 2);
    assert_eq!(saved.current.payload, json!({ "subject": "Plans v2" }));
    assert_eq!(saved.created_at, t(10));
    match store
        .save_draft(draft.id, 1, &json!({ "subject": "Stale" }), &[], t(30))
        .await
        .unwrap()
    {
        DraftSave::Conflict(current) => assert_eq!(current, saved),
        DraftSave::Saved(_) => panic!("a stale save won"),
    }
    assert!(not_found(
        store
            .save_draft(DraftId::new(), 1, &json!({}), &[], t(30))
            .await
    ));
    let versions = store.draft_versions(draft.id).await.unwrap();
    assert_eq!(
        versions.iter().map(|v| v.version).collect::<Vec<_>>(),
        [2, 1]
    );

    let other = store
        .create_draft(archive, ada, &json!({}), &[], t(15))
        .await
        .unwrap();
    store
        .create_draft(archive, lin, &json!({}), &[], t(40))
        .await
        .unwrap();
    assert_eq!(
        store
            .list_drafts(archive, ada)
            .await
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect::<Vec<_>>(),
        [draft.id, other.id],
        "drafts are not most recently saved first"
    );

    let mut version = 2;
    for n in 0..DRAFT_VERSIONS_KEPT {
        let at = t(100 + i64::from(n));
        match store
            .save_draft(other.id, version - 1, &json!({ "n": n }), &[], at)
            .await
            .unwrap()
        {
            DraftSave::Saved(saved) => version = saved.current.version + 1,
            DraftSave::Conflict(_) => panic!("a save in order conflicted"),
        }
    }
    let kept = store.draft_versions(other.id).await.unwrap();
    assert_eq!(kept.len() as u32, DRAFT_VERSIONS_KEPT);
    assert_eq!(kept[0].version, DRAFT_VERSIONS_KEPT + 1);

    store.delete_draft(draft.id).await.unwrap();
    store.delete_draft(draft.id).await.unwrap();
    assert!(store.get_draft(draft.id).await.unwrap().is_none());
    assert!(store.draft_versions(draft.id).await.unwrap().is_empty());
    assert!(
        !store
            .referenced_blobs(None, 1000)
            .await
            .unwrap()
            .contains(&photo),
        "a removed draft kept its blob"
    );

    store.delete_user(lin).await.unwrap();
    assert!(store.list_drafts(archive, lin).await.unwrap().is_empty());
    store.delete_archive(archive).await.unwrap();
    assert!(store.get_draft(other.id).await.unwrap().is_none());
    store.delete_user(ada).await.unwrap();
}

struct Sending {
    archive: ArchiveId,
    identity: IdentityId,
    transport: TransportId,
    user: UserId,
}

async fn sending(store: &dyn Store, name: &str) -> Sending {
    let archive = store.create_archive(name).await.unwrap().id;
    let transport = smtp(Some(archive));
    store.create_transport(&transport).await.unwrap();
    let user = user(
        store,
        &format!("{}@example.com", name.to_lowercase().replace(' ', ".")),
    )
    .await;
    let identity = store
        .create_identity(new_identity(archive, transport.id, Some(user)))
        .await
        .unwrap();
    Sending {
        archive,
        identity: identity.id,
        transport: transport.id,
        user,
    }
}

fn entry(s: &Sending, bytes: &[u8], send_at: i64) -> NewOutboxEntry {
    NewOutboxEntry {
        archive_id: s.archive,
        identity_id: s.identity,
        transport_id: s.transport,
        user_id: Some(s.user),
        device_id: None,
        route: json!({ "reasons": [{ "code": "default" }] }),
        mail_from: "ada@example.com".into(),
        recipients: vec!["bob@example.org".into(), "carol@example.org".into()],
        blob_id: BlobId::of(bytes),
        size: bytes.len() as u64,
        rfc_message_id: format!("{send_at}@example.com"),
        send_at: Timestamp::from_secs(send_at),
        state: OutboxState::Queued,
        flow_run: None,
        idempotency_key: None,
        resend_of: None,
        at: Timestamp::from_secs(send_at - 10),
    }
}

async fn cleanup(store: &dyn Store, s: Sending) {
    store.delete_archive(s.archive).await.unwrap();
    store.delete_user(s.user).await.unwrap();
}

async fn outbox_queue(store: &dyn Store) {
    let s = sending(store, "Outbox queue").await;
    let t = Timestamp::from_secs;
    let first = entry(&s, b"first", 1000);
    let (queued, created) = store.enqueue_outbox(&first).await.unwrap();
    assert!(created);
    assert_eq!(queued.new, first);
    assert_eq!(queued.state, OutboxState::Queued);
    assert_eq!(queued.attempts, 0);
    assert!(
        queued
            .recipients
            .iter()
            .all(|r| r.state == RecipientState::Pending)
    );
    assert_eq!(
        queued
            .recipients
            .iter()
            .map(|r| r.address.as_str())
            .collect::<Vec<_>>(),
        ["bob@example.org", "carol@example.org"]
    );
    assert_eq!(
        store.get_outbox(queued.id).await.unwrap(),
        Some(queued.clone())
    );
    assert!(store.get_outbox(OutboxId::new()).await.unwrap().is_none());

    let keyed = NewOutboxEntry {
        device_id: Some(holtmail_core::DeviceId::new()),
        idempotency_key: Some("k-1".into()),
        ..entry(&s, b"second", 2000)
    };
    let (once, created) = store.enqueue_outbox(&keyed).await.unwrap();
    assert!(created);
    assert_eq!(once.new, keyed, "the device is kept");
    let (twice, created) = store
        .enqueue_outbox(&NewOutboxEntry {
            blob_id: BlobId::of(b"other"),
            ..keyed.clone()
        })
        .await
        .unwrap();
    assert!(!created, "an idempotency key was used twice");
    assert_eq!(twice, once);
    assert_eq!(
        store
            .outbox_by_idempotency_key(s.user, "k-1")
            .await
            .unwrap(),
        Some(once.clone())
    );
    assert!(
        store
            .outbox_by_idempotency_key(s.user, "k-2")
            .await
            .unwrap()
            .is_none()
    );
    let all = store.device_sends_since(s.user, t(0)).await.unwrap();
    assert_eq!(
        all,
        DeviceSends {
            messages: 1,
            recipients: 2,
            oldest: Some(t(1990)),
        },
        "the web app's messages do not count"
    );
    assert_eq!(
        store.device_sends_since(s.user, t(1991)).await.unwrap(),
        DeviceSends::default()
    );
    assert!(invalid(
        store
            .enqueue_outbox(&NewOutboxEntry {
                state: OutboxState::Sent,
                ..entry(&s, b"x", 1)
            })
            .await
    ));
    assert!(invalid(
        store
            .enqueue_outbox(&NewOutboxEntry {
                recipients: vec![],
                ..entry(&s, b"x", 1)
            })
            .await
    ));
    assert!(not_found(
        store
            .enqueue_outbox(&NewOutboxEntry {
                archive_id: ArchiveId::new(),
                ..entry(&s, b"x", 1)
            })
            .await
    ));
    let (held, _) = store
        .enqueue_outbox(&NewOutboxEntry {
            state: OutboxState::Draft,
            ..entry(&s, b"held", 3000)
        })
        .await
        .unwrap();
    assert_eq!(held.state, OutboxState::Draft);
    assert_eq!(held.new.state, OutboxState::Draft);

    let list = |states: Vec<OutboxState>, before: Option<OutboxId>| OutboxQuery {
        archive: s.archive,
        states,
        before,
        user: None,
        limit: 10,
    };
    assert_eq!(
        store
            .list_outbox(&list(vec![], None))
            .await
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        [held.id, once.id, queued.id]
    );
    assert_eq!(
        store
            .list_outbox(&list(vec![OutboxState::Draft], None))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_outbox(&list(vec![], Some(once.id)))
            .await
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        [queued.id]
    );
    assert_eq!(
        store
            .list_outbox(&OutboxQuery {
                user: Some(s.user),
                limit: 1,
                ..list(vec![], None)
            })
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .list_outbox(&OutboxQuery {
                user: Some(UserId::new()),
                ..list(vec![], None)
            })
            .await
            .unwrap()
            .is_empty(),
        "another user's messages were listed"
    );

    let undone = store
        .change_outbox(
            queued.id,
            &[OutboxState::Queued],
            &OutboxChange::to(OutboxState::Draft, t(1001)),
        )
        .await
        .unwrap()
        .expect("undo from queued");
    assert_eq!(undone.state, OutboxState::Draft);
    assert_eq!(undone.updated_at, t(1001));
    assert!(
        store
            .change_outbox(
                queued.id,
                &[OutboxState::Queued],
                &OutboxChange::to(OutboxState::Cancelled, t(1002)),
            )
            .await
            .unwrap()
            .is_none(),
        "a change from the wrong state was applied"
    );
    let message = store
        .insert_message(crate::testing::message(s.archive, 7000, 1000))
        .await
        .unwrap()
        .message_id;
    let refused = RecipientState::Refused {
        permanent: true,
        code: 550,
        text: "5.1.1 no such user".into(),
    };
    let change = OutboxChange {
        state: OutboxState::Sent,
        at: t(1003),
        next_attempt_at: None,
        send_at: Some(t(999)),
        recipients: Some(vec![
            OutboxRecipient {
                address: "bob@example.org".into(),
                state: RecipientState::Accepted,
            },
            OutboxRecipient {
                address: "carol@example.org".into(),
                state: refused.clone(),
            },
        ]),
        error: Some("one recipient refused".into()),
        sent_at: Some(t(1003)),
        message_id: Some(message),
    };
    let sent = store
        .change_outbox(queued.id, &[OutboxState::Draft], &change)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.new.send_at, t(999));
    assert_eq!(sent.recipients[0].state, RecipientState::Accepted);
    assert_eq!(sent.recipients[1].state, refused);
    assert_eq!(sent.sent_at, Some(t(1003)));
    assert_eq!(sent.message_id, Some(message));
    assert_eq!(sent.last_error.as_deref(), Some("one recipient refused"));
    let bounced = store
        .change_outbox(
            queued.id,
            &[OutboxState::Sent],
            &OutboxChange::to(OutboxState::Bounced, t(1004)),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bounced.sent_at, Some(t(1003)), "a change cleared sent_at");
    assert_eq!(bounced.message_id, Some(message));
    assert_eq!(bounced.last_error, None);
    let wrong = OutboxChange {
        recipients: Some(vec![OutboxRecipient {
            address: "mallory@example.org".into(),
            state: RecipientState::Accepted,
        }]),
        ..OutboxChange::to(OutboxState::Sent, t(1005))
    };
    assert!(invalid(
        store
            .change_outbox(once.id, &[OutboxState::Queued], &wrong)
            .await
    ));
    assert!(not_found(
        store
            .change_outbox(
                OutboxId::new(),
                &[OutboxState::Queued],
                &OutboxChange::to(OutboxState::Sent, t(1))
            )
            .await
    ));
    for state in OutboxState::ALL {
        assert_eq!(OutboxState::parse(state.as_str()), Some(state));
    }
    cleanup(store, s).await;
}

async fn outbox_held_for_changes(store: &dyn Store) {
    let s = sending(store, "Outbox held for changes").await;
    let (held, _) = store
        .enqueue_outbox(&NewOutboxEntry {
            state: OutboxState::Draft,
            ..entry(&s, b"held for a change", 4000)
        })
        .await
        .unwrap();
    let (lone, _) = store
        .enqueue_outbox(&NewOutboxEntry {
            state: OutboxState::Draft,
            ..entry(&s, b"held for nothing", 4100)
        })
        .await
        .unwrap();
    let account = store
        .create_account(NewAccount {
            archive_id: s.archive,
            kind: AccountKind::Imap,
            name: "Held".into(),
            address: Some("ada@example.com".into()),
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let calendar = store
        .upsert_collection(NewCollection {
            archive_id: s.archive,
            account_id: Some(account),
            kind: CollectionKind::Calendar,
            remote_id: "/held/".into(),
            name: "Held".into(),
            color: None,
        })
        .await
        .unwrap();
    let recorded = store
        .record_change(
            &NewItemChange {
                target: ChangeTarget::Href {
                    collection: calendar.id,
                    href: "/held/a.ics".into(),
                },
                op: ChangeOp::Create,
                class: ChangeClass::Create,
                content: Some(ChangeContent {
                    blob_id: BlobId::of(b"held event"),
                    content_type: "text/calendar".into(),
                }),
                patch: None,
                base_version: None,
                origin: ChangeOrigin::Api,
                user_id: None,
                device: None,
                at: Timestamp::from_secs(4200),
                not_before: Some(Timestamp::from_secs(4300)),
                messages: vec![held.id],
                schedule_tag: None,
            },
            Expect::Absent,
        )
        .await
        .unwrap();
    let RecordedChange::Recorded(change) = recorded else {
        panic!("not recorded");
    };
    assert_eq!(change.next_attempt_at, Some(Timestamp::from_secs(4300)));
    assert_eq!(store.held_change_messages().await.unwrap(), [held.id]);
    let _ = lone;
    store
        .change_outbox(
            held.id,
            &[OutboxState::Draft],
            &OutboxChange::to(OutboxState::Queued, Timestamp::from_secs(4400)),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(store.held_change_messages().await.unwrap().is_empty());

    let message = store
        .insert_message(crate::testing::message(s.archive, 7100, 4000))
        .await
        .unwrap()
        .message_id;
    let at = Timestamp::from_secs;
    let first = store
        .defer_reply(s.archive, message, at(100), at(200))
        .await
        .unwrap();
    assert_eq!(first.attempts, 1);
    let again = store
        .defer_reply(s.archive, message, at(150), at(300))
        .await
        .unwrap();
    assert_eq!((again.attempts, again.since), (2, at(100)));
    assert!(store.due_replies(at(299), 10).await.unwrap().is_empty());
    assert_eq!(store.due_replies(at(300), 10).await.unwrap(), [again]);
    store.forget_reply(message).await.unwrap();
    store.forget_reply(message).await.unwrap();
    assert!(store.due_replies(at(300), 10).await.unwrap().is_empty());
    cleanup(store, s).await;
}

async fn outbox_claims(store: &dyn Store) {
    let a = sending(store, "Outbox claims").await;
    let b = sending(store, "Outbox claims 2").await;
    let t = Timestamp::from_secs;
    let (early, _) = store
        .enqueue_outbox(&entry(&a, b"early", 100))
        .await
        .unwrap();
    let (late, _) = store
        .enqueue_outbox(&entry(&a, b"late", 200))
        .await
        .unwrap();
    let (other, _) = store
        .enqueue_outbox(&entry(&b, b"other", 150))
        .await
        .unwrap();

    assert!(store.due_transports(t(99)).await.unwrap().is_empty());
    assert!(
        store
            .claim_outbox(a.transport, t(99))
            .await
            .unwrap()
            .is_none()
    );
    let mut both = vec![a.transport, b.transport];
    both.sort();
    assert_eq!(store.due_transports(t(250)).await.unwrap(), both);

    let claimed = store
        .claim_outbox(a.transport, t(250))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        claimed.id, early.id,
        "the earliest due message was not taken"
    );
    assert_eq!(claimed.state, OutboxState::Sending);
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.last_attempt_at, Some(t(250)));
    assert!(
        store
            .claim_outbox(a.transport, t(250))
            .await
            .unwrap()
            .is_none(),
        "two messages of one transport were sending"
    );
    let parallel = store
        .claim_outbox(b.transport, t(250))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parallel.id, other.id);
    assert_eq!(
        store
            .outbox_attempts_since(a.transport, t(250))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .outbox_attempts_since(a.transport, t(251))
            .await
            .unwrap(),
        0
    );

    let retry = OutboxChange {
        next_attempt_at: Some(t(400)),
        error: Some("451 try later".into()),
        ..OutboxChange::to(OutboxState::Retrying, t(251))
    };
    let retrying = store
        .change_outbox(early.id, &[OutboxState::Sending], &retry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retrying.next_attempt_at, Some(t(400)));
    let next = store
        .claim_outbox(a.transport, t(300))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.id, late.id, "a retry was taken before it was due");
    store
        .change_outbox(
            late.id,
            &[OutboxState::Sending],
            &OutboxChange::to(OutboxState::Sent, t(301)),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .claim_outbox(a.transport, t(399))
            .await
            .unwrap()
            .is_none()
    );
    let again = store
        .claim_outbox(a.transport, t(400))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.id, early.id);
    assert_eq!(again.attempts, 2);
    assert_eq!(again.next_attempt_at, None);
    assert_eq!(again.last_error.as_deref(), Some("451 try later"));

    let interrupted = store
        .interrupt_sending(t(500), "interrupted")
        .await
        .unwrap();
    let mut ids: Vec<_> = interrupted.iter().map(|e| e.id).collect();
    ids.sort();
    let mut expected = vec![early.id, other.id];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(interrupted.iter().all(
        |e| e.state == OutboxState::Uncertain && e.last_error.as_deref() == Some("interrupted")
    ));
    assert!(
        store
            .interrupt_sending(t(501), "x")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.due_transports(t(10_000)).await.unwrap().is_empty());
    cleanup(store, a).await;
    cleanup(store, b).await;
}

async fn outbox_copies(store: &dyn Store) {
    let s = sending(store, "Outbox copies").await;
    let t = Timestamp::from_secs;
    let (first, _) = store
        .enqueue_outbox(&entry(&s, b"copy a", 500))
        .await
        .unwrap();
    let (second, _) = store
        .enqueue_outbox(&NewOutboxEntry {
            rfc_message_id: first.new.rfc_message_id.clone(),
            ..entry(&s, b"copy b", 600)
        })
        .await
        .unwrap();
    store
        .enqueue_outbox(&entry(&s, b"copy c", 700))
        .await
        .unwrap();
    let found = store
        .outbox_by_rfc_message_id(s.archive, &first.new.rfc_message_id)
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|e| e.id).collect::<Vec<_>>(),
        [first.id, second.id]
    );
    let elsewhere = sending(store, "Outbox copies 2").await;
    assert!(
        store
            .outbox_by_rfc_message_id(elsewhere.archive, &first.new.rfc_message_id)
            .await
            .unwrap()
            .is_empty(),
        "another archive's outbox was searched"
    );
    assert!(first.synced_copy.is_none() && first.note.is_none());

    let sent = store
        .insert_message(crate::testing::message(s.archive, 7100, 1000))
        .await
        .unwrap()
        .message_id;
    let copy = store
        .insert_message(crate::testing::message(s.archive, 7101, 1000))
        .await
        .unwrap()
        .message_id;
    let foreign = store
        .insert_message(crate::testing::message(elsewhere.archive, 7102, 1000))
        .await
        .unwrap()
        .message_id;
    let mut archived = OutboxChange::to(OutboxState::Sent, t(501));
    archived.message_id = Some(sent);
    store
        .change_outbox(first.id, &[OutboxState::Queued], &archived)
        .await
        .unwrap()
        .unwrap();
    let linked = store
        .link_outbox_copy(first.id, copy, t(502))
        .await
        .unwrap();
    assert_eq!(linked.synced_copy, Some(copy));
    assert_eq!(linked.state, OutboxState::Sent);
    assert_eq!(linked.message_id, Some(sent));
    assert_eq!(linked.updated_at, t(502));
    assert_eq!(
        store.get_outbox(first.id).await.unwrap(),
        Some(linked.clone())
    );
    for message in [sent, copy] {
        assert_eq!(
            store
                .outbox_for_message(message)
                .await
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            [first.id],
            "both copies lead to the outbox message"
        );
    }
    assert!(store.outbox_for_message(foreign).await.unwrap().is_empty());
    assert!(invalid(
        store.link_outbox_copy(first.id, foreign, t(503)).await
    ));
    assert!(not_found(
        store.link_outbox_copy(OutboxId::new(), copy, t(503)).await
    ));
    assert!(not_found(
        store
            .link_outbox_copy(first.id, MessageId::new(), t(503))
            .await
    ));

    let noted = store
        .set_outbox_note(first.id, Some("delivery is delayed"), t(504))
        .await
        .unwrap();
    assert_eq!(noted.note.as_deref(), Some("delivery is delayed"));
    assert_eq!(noted.state, OutboxState::Sent);
    assert_eq!(noted.synced_copy, Some(copy));
    let cleared = store.set_outbox_note(first.id, None, t(505)).await.unwrap();
    assert!(cleared.note.is_none());
    assert!(not_found(
        store.set_outbox_note(OutboxId::new(), None, t(505)).await
    ));

    let rewritten = RewrittenFrom {
        seen: "ada.lovelace@example.net".into(),
        at: t(506),
    };
    store
        .set_identity_rewritten_from(s.identity, Some(&rewritten))
        .await
        .unwrap();
    store
        .set_identity_rewritten_from(s.identity, Some(&rewritten))
        .await
        .unwrap();
    let mut identity = store.get_identity(s.identity).await.unwrap().unwrap();
    assert_eq!(identity.rewritten_from, Some(rewritten.clone()));
    identity.fields.name = Some("Ada L.".into());
    store.update_identity(&identity).await.unwrap();
    assert_eq!(
        store
            .get_identity(s.identity)
            .await
            .unwrap()
            .unwrap()
            .rewritten_from,
        Some(rewritten.clone()),
        "an update that kept the address cleared the warning"
    );
    identity.fields.address = "ada.l@example.com".into();
    store.update_identity(&identity).await.unwrap();
    assert!(
        store
            .get_identity(s.identity)
            .await
            .unwrap()
            .unwrap()
            .rewritten_from
            .is_none(),
        "a new address kept the warning"
    );
    store
        .set_identity_rewritten_from(s.identity, Some(&rewritten))
        .await
        .unwrap();
    store
        .set_identity_rewritten_from(s.identity, None)
        .await
        .unwrap();
    assert!(
        store
            .get_identity(s.identity)
            .await
            .unwrap()
            .unwrap()
            .rewritten_from
            .is_none()
    );
    assert!(not_found(
        store
            .set_identity_rewritten_from(IdentityId::new(), None)
            .await
    ));

    for kind in [TransportKind::Jmap, TransportKind::Graph] {
        let transport = Transport {
            kind,
            settings: json!({ "url": "https://jmap.example.com" }),
            ..smtp(Some(s.archive))
        };
        store.create_transport(&transport).await.unwrap();
        assert_eq!(
            store.get_transport(transport.id).await.unwrap(),
            Some(transport.clone())
        );
        let with_account = Transport {
            id: TransportId::new(),
            account_id: Some(account(store, s.archive).await.id),
            ..transport.clone()
        };
        assert!(invalid(store.create_transport(&with_account).await));
        assert_eq!(TransportKind::parse(kind.as_str()), Some(kind));
    }
    cleanup(store, s).await;
    cleanup(store, elsewhere).await;
}

async fn outbox_removal(store: &dyn Store) {
    let s = sending(store, "Outbox removal").await;
    let bytes = b"outbox bytes only";
    let (queued, _) = store.enqueue_outbox(&entry(&s, bytes, 100)).await.unwrap();
    assert!(
        store
            .referenced_blobs(None, 1000)
            .await
            .unwrap()
            .contains(&queued.new.blob_id),
        "an outbox message's bytes were not referenced"
    );
    let archive = s.archive;
    let released = store.delete_archive(archive).await.unwrap();
    assert!(released.contains(&queued.new.blob_id));
    assert!(store.get_outbox(queued.id).await.unwrap().is_none());
    store.delete_user(s.user).await.unwrap();
}

async fn kinds_since(store: &dyn Store, archive: ArchiveId, after: u64) -> Vec<ChangeLogEntry> {
    store
        .changes_since(archive, Some(after), 1000)
        .await
        .unwrap()
}

async fn raw_drafts_and_log(store: &dyn Store) {
    let s = sending(store, "Raw drafts and log").await;
    let t = Timestamp::from_secs;
    let start = store.latest_seq().await.unwrap();

    let raw = BlobId::of(b"From: ada@example.com\r\nSubject: Draft\r\n\r\nHello\r\n");
    let draft = store
        .create_raw_draft(s.archive, s.user, &raw, t(10))
        .await
        .unwrap();
    assert_eq!(draft.current.raw, Some(raw));
    assert_eq!(draft.current.payload, serde_json::Value::Null);
    assert_eq!(
        draft.current.blobs,
        [raw],
        "the raw bytes are kept as a draft blob"
    );
    assert_eq!(
        store.get_draft(draft.id).await.unwrap(),
        Some(draft.clone())
    );
    assert!(not_found(
        store
            .create_raw_draft(ArchiveId::new(), s.user, &raw, t(10))
            .await
    ));
    let log = kinds_since(store, s.archive, start).await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].kind, ChangeKind::Draft);
    assert_eq!(log[0].draft_id, Some(draft.id));
    assert_eq!(
        log[0].user_id,
        Some(s.user),
        "a draft row is its user's own"
    );

    let DraftSave::Saved(saved) = store
        .save_draft(draft.id, 1, &json!({ "subject": "Edited" }), &[], t(20))
        .await
        .unwrap()
    else {
        panic!("the save conflicted");
    };
    assert_eq!(saved.current.raw, None, "a save stores a compose version");
    assert_eq!(
        store.draft_versions(draft.id).await.unwrap()[1].raw,
        Some(raw),
        "the raw version is kept"
    );
    let after_save = kinds_since(store, s.archive, start).await;
    assert_eq!(after_save.len(), 2);
    let DraftSave::Conflict(_) = store
        .save_draft(draft.id, 1, &json!({}), &[], t(21))
        .await
        .unwrap()
    else {
        panic!("a stale save was stored");
    };
    assert_eq!(
        kinds_since(store, s.archive, start).await.len(),
        2,
        "a conflict writes nothing"
    );
    store.delete_draft(draft.id).await.unwrap();
    store.delete_draft(draft.id).await.unwrap();
    let after_delete = kinds_since(store, s.archive, start).await;
    assert_eq!(after_delete.len(), 3, "deleting twice writes one row");
    assert!(after_delete.iter().all(|e| e.kind == ChangeKind::Draft));

    let mark = store.latest_seq().await.unwrap();
    let (queued, _) = store
        .enqueue_outbox(&entry(&s, b"logged", 1000))
        .await
        .unwrap();
    let (_, created) = store
        .enqueue_outbox(&entry(&s, b"logged", 1000))
        .await
        .unwrap();
    assert!(created, "without a key every enqueue adds a message");
    let log = kinds_since(store, s.archive, mark).await;
    assert_eq!(log.len(), 2);
    assert!(
        log.iter()
            .all(|e| e.kind == ChangeKind::Submission && e.user_id.is_none())
    );
    assert_eq!(log[0].outbox_id, Some(queued.id));

    let mark = store.latest_seq().await.unwrap();
    assert!(
        store
            .change_outbox(
                queued.id,
                &[OutboxState::Sent],
                &OutboxChange::to(OutboxState::Failed, t(1100))
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        kinds_since(store, s.archive, mark).await.is_empty(),
        "a refused change writes nothing"
    );
    store
        .change_outbox(
            queued.id,
            &[OutboxState::Queued],
            &OutboxChange::to(OutboxState::Draft, t(1100)),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kinds_since(store, s.archive, mark).await.len(), 1);
    let mark = store.latest_seq().await.unwrap();
    store
        .set_outbox_note(queued.id, Some("delayed"), t(1200))
        .await
        .unwrap();
    store
        .set_outbox_note(queued.id, Some("delayed"), t(1300))
        .await
        .unwrap();
    assert_eq!(
        kinds_since(store, s.archive, mark).await.len(),
        1,
        "the same note again writes nothing"
    );
    cleanup(store, s).await;
}
