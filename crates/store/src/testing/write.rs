use holtmail_core::{ArchiveId, Timestamp, UserId};
use serde_json::json;

use crate::*;

pub(super) async fn run(store: &dyn Store) {
    capabilities(store).await;
    grant_roles(store).await;
    audit(store).await;
    idempotency(store).await;
}

async fn account(store: &dyn Store, archive: ArchiveId) -> Account {
    store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Writer".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
}

fn user(email: &str) -> NewUser {
    NewUser {
        email: email.into(),
        name: "Writer".into(),
        role: InstanceRole::Member,
        password_hash: None,
    }
}

async fn capabilities(store: &dyn Store) {
    let archive = store.create_archive("Capabilities").await.unwrap().id;
    let first = account(store, archive).await;
    let second = account(store, archive).await;
    assert!(
        store
            .account_capabilities(first.id)
            .await
            .unwrap()
            .is_empty(),
        "a new account may write"
    );
    let caps: Capabilities = [Capability::MailFlags, Capability::MailSend]
        .into_iter()
        .collect();
    store
        .set_account_capabilities(first.id, &caps)
        .await
        .unwrap();
    store
        .set_account_capabilities(first.id, &caps)
        .await
        .unwrap();
    assert_eq!(store.account_capabilities(first.id).await.unwrap(), caps);
    assert!(
        store
            .account_capabilities(second.id)
            .await
            .unwrap()
            .is_empty()
    );
    let mut fewer = caps.clone();
    fewer.set(Capability::MailSend, false);
    fewer.set(Capability::PimRespond, true);
    store
        .set_account_capabilities(first.id, &fewer)
        .await
        .unwrap();
    let read = store.account_capabilities(first.id).await.unwrap();
    assert!(read.has(Capability::MailFlags) && read.has(Capability::PimRespond));
    assert!(!read.has(Capability::MailSend));
    assert!(matches!(
        store
            .set_account_capabilities(AccountId::new(), &caps)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        store
            .account_capabilities(AccountId::new())
            .await
            .unwrap()
            .is_empty()
    );
    store.delete_account(first.id).await.unwrap();
    assert!(
        store
            .account_capabilities(first.id)
            .await
            .unwrap()
            .is_empty(),
        "capabilities outlived their account"
    );
    store
        .set_account_capabilities(second.id, &caps)
        .await
        .unwrap();
    store.delete_archive(archive).await.unwrap();
    assert!(
        store
            .account_capabilities(second.id)
            .await
            .unwrap()
            .is_empty()
    );
    for capability in Capability::ALL {
        assert_eq!(Capability::parse(capability.as_str()), Some(capability));
    }
}

async fn grant_roles(store: &dyn Store) {
    let first = store.create_archive("Roles 1").await.unwrap().id;
    let second = store.create_archive("Roles 2").await.unwrap().id;
    let (first, second) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    let lin = store.create_user(user("lin@example.org")).await.unwrap();
    store.set_grants(lin.id, &[first]).await.unwrap();
    assert_eq!(
        store.grant_roles(lin.id).await.unwrap(),
        [Grant {
            archive: first,
            role: GrantRole::Reader
        }],
        "an existing grant is not a reader grant"
    );
    store
        .set_grant_role(lin.id, first, Some(GrantRole::Sender))
        .await
        .unwrap();
    store
        .set_grant_role(lin.id, second, Some(GrantRole::Editor))
        .await
        .unwrap();
    assert_eq!(
        store.grant_roles(lin.id).await.unwrap(),
        [
            Grant {
                archive: first,
                role: GrantRole::Sender
            },
            Grant {
                archive: second,
                role: GrantRole::Editor
            }
        ]
    );
    assert_eq!(store.grants(lin.id).await.unwrap(), [first, second]);
    store.set_grants(lin.id, &[first]).await.unwrap();
    assert_eq!(
        store.grant_roles(lin.id).await.unwrap(),
        [Grant {
            archive: first,
            role: GrantRole::Sender
        }],
        "set_grants changed the role of a kept archive"
    );
    store.set_grant_role(lin.id, first, None).await.unwrap();
    store.set_grant_role(lin.id, first, None).await.unwrap();
    assert!(store.grants(lin.id).await.unwrap().is_empty());
    assert!(matches!(
        store
            .set_grant_role(UserId::new(), first, Some(GrantRole::Reader))
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store
            .set_grant_role(lin.id, ArchiveId::new(), Some(GrantRole::Reader))
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(GrantRole::Sender > GrantRole::Editor && GrantRole::Editor > GrantRole::Reader);
    store.delete_user(lin.id).await.unwrap();
    assert!(store.grant_roles(lin.id).await.unwrap().is_empty());
    store.delete_archive(first).await.unwrap();
    store.delete_archive(second).await.unwrap();
}

async fn audit(store: &dyn Store) {
    let archive = store.create_archive("Audit").await.unwrap().id;
    let other = store.create_archive("Audit 2").await.unwrap().id;
    let account = account(store, archive).await;
    let mira = store.create_user(user("mira@example.org")).await.unwrap();
    assert!(
        store
            .list_audit(&AuditQuery {
                archive: Some(archive),
                before: None,
                limit: 10
            })
            .await
            .unwrap()
            .is_empty()
    );
    let entry = |n: u64, archive: ArchiveId| NewAuditEntry {
        at: Timestamp::from_secs(1_700_000_000 + n as i64),
        user: Some(mira.id),
        kind: AuditKind::CapabilitiesChanged,
        archive: Some(archive),
        account: Some(account.id),
        targets: vec!["mail.flags".into()],
        count: n,
    };
    let first = store.append_audit(&entry(1, archive)).await.unwrap();
    let second = store.append_audit(&entry(2, other)).await.unwrap();
    let third = store
        .append_audit(&NewAuditEntry {
            user: None,
            account: None,
            kind: AuditKind::MailSend,
            targets: Vec::new(),
            ..entry(3, archive)
        })
        .await
        .unwrap();
    assert!(first.seq >= 1 && first.seq < second.seq && second.seq < third.seq);
    assert_eq!(first.entry, entry(1, archive));
    let all = |archive, before, limit| {
        let query = AuditQuery {
            archive,
            before,
            limit,
        };
        async move { store.list_audit(&query).await.unwrap() }
    };
    assert_eq!(
        all(Some(archive), None, 10).await,
        [third.clone(), first.clone()]
    );
    assert_eq!(all(None, Some(third.seq), 1).await, [second]);
    assert!(all(Some(archive), Some(first.seq), 10).await.is_empty());
    store.delete_user(mira.id).await.unwrap();
    store.delete_archive(archive).await.unwrap();
    assert_eq!(
        all(Some(archive), None, 10).await,
        [third, first],
        "audit entries went with their archive or user"
    );
    store.delete_archive(other).await.unwrap();
    for kind in AuditKind::ALL {
        assert_eq!(AuditKind::parse(kind.as_str()), Some(kind));
    }
}

async fn idempotency(store: &dyn Store) {
    let noor = store.create_user(user("noor@example.org")).await.unwrap();
    let key = IdempotencyKey {
        user: noor.id,
        key: "k-1".into(),
        route: "POST /api/things".into(),
    };
    let at = Timestamp::from_secs(1_800_000_000);
    let since = Timestamp::from_secs(1_800_000_000 - 86_400);
    let abandoned = Timestamp::from_secs(1_800_000_000 - 600);
    assert_eq!(
        store
            .claim_idempotency_key(&key, "fp", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::Claimed
    );
    assert_eq!(
        store
            .claim_idempotency_key(&key, "fp", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::InFlight
    );
    assert_eq!(
        store
            .claim_idempotency_key(&key, "other", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::Mismatch
    );
    let other_route = IdempotencyKey {
        route: "POST /api/others".into(),
        ..key.clone()
    };
    assert_eq!(
        store
            .claim_idempotency_key(&other_route, "fp", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::Claimed,
        "a key was shared between routes"
    );
    let dropped = IdempotencyKey {
        route: "POST /api/dropped".into(),
        ..key.clone()
    };
    assert_eq!(
        store
            .claim_idempotency_key(&dropped, "fp", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::Claimed
    );
    let soon = Timestamp::from_secs(1_800_000_000 + 900);
    assert_eq!(
        store
            .claim_idempotency_key(
                &dropped,
                "other",
                soon,
                since,
                Timestamp::from_secs(1_800_000_000 + 300)
            )
            .await
            .unwrap(),
        IdempotencyClaim::Claimed,
        "an abandoned claim still blocked its key"
    );
    store.release_idempotency_key(&dropped).await.unwrap();
    let response = StoredResponse {
        status: 201,
        content_type: Some("application/json".into()),
        body: b"{\"id\":1}".to_vec(),
    };
    store
        .complete_idempotency_key(&key, &response)
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_idempotency_key(&key, "fp", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::Done(response.clone())
    );
    let later = Timestamp::from_secs(1_800_000_000 + 2 * 86_400);
    assert_eq!(
        store
            .claim_idempotency_key(
                &key,
                "fp",
                later,
                Timestamp::from_secs(1_800_000_000 + 86_400),
                abandoned,
            )
            .await
            .unwrap(),
        IdempotencyClaim::Claimed,
        "an expired key still answered"
    );
    store.release_idempotency_key(&key).await.unwrap();
    store.release_idempotency_key(&key).await.unwrap();
    assert!(matches!(
        store.complete_idempotency_key(&key, &response).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store
            .prune_idempotency_keys(Timestamp::from_secs(1_800_000_001))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .claim_idempotency_key(&other_route, "fp", at, since, abandoned)
            .await
            .unwrap(),
        IdempotencyClaim::Claimed
    );
    store.delete_user(noor.id).await.unwrap();
}
