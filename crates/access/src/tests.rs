use holtmail_store::{ArchiveStore, NewAccount, NewUser};
use holtmail_store_sqlite::SqliteStore;

use super::*;

fn caps(list: &[Capability]) -> Capabilities {
    list.iter().copied().collect()
}

#[test]
fn opens_only_when_every_lock_is_open() {
    let on = WriteSwitches::default();
    let all = Scopes::all();
    let flags = caps(&[Capability::MailFlags, Capability::MailSend]);
    let editor = Some(GrantRole::Editor);
    let sender = Some(GrantRole::Sender);
    assert_eq!(
        decide(on, editor, &flags, &all, Capability::MailFlags),
        Ok(())
    );
    assert_eq!(
        decide(on, sender, &flags, &all, Capability::MailSend),
        Ok(())
    );
    assert_eq!(
        decide(on, editor, &flags, &all, Capability::MailSend),
        Err(Denial::Grant)
    );
    assert_eq!(
        decide(
            on,
            Some(GrantRole::Reader),
            &flags,
            &all,
            Capability::MailFlags
        ),
        Err(Denial::Grant)
    );
    assert_eq!(
        decide(on, None, &flags, &all, Capability::MailFlags),
        Err(Denial::Grant)
    );
    assert_eq!(
        decide(on, sender, &flags, &all, Capability::MailMove),
        Err(Denial::Capability)
    );
    assert_eq!(
        decide(on, sender, &Capabilities::none(), &all, Capability::PimEdit),
        Err(Denial::Capability)
    );
    let off = WriteSwitches {
        enabled: false,
        ..on
    };
    assert_eq!(
        decide(off, sender, &flags, &all, Capability::MailFlags),
        Err(Denial::Switch)
    );
    let no_send = WriteSwitches { send: false, ..on };
    assert_eq!(
        decide(no_send, sender, &flags, &all, Capability::MailSend),
        Err(Denial::Switch)
    );
    assert_eq!(
        decide(no_send, sender, &flags, &all, Capability::MailFlags),
        Ok(())
    );
    let read_only = Scopes::only([Scope::Read]);
    assert_eq!(
        decide(on, sender, &flags, &read_only, Capability::MailFlags),
        Err(Denial::Scope)
    );
    let mail = Scopes::only([Scope::Read, Scope::WriteMail]);
    assert_eq!(
        decide(on, sender, &flags, &mail, Capability::MailFlags),
        Ok(())
    );
    assert_eq!(
        decide(on, sender, &flags, &mail, Capability::MailSend),
        Err(Denial::Scope)
    );
}

#[test]
fn expunges_only_with_delete() {
    let on = WriteSwitches::default();
    let all = Scopes::all();
    let editor = Some(GrantRole::Editor);
    assert_eq!(
        decide(
            on,
            editor,
            &caps(&[Capability::MailExpunge]),
            &all,
            Capability::MailExpunge
        ),
        Err(Denial::Capability)
    );
    assert_eq!(
        decide(
            on,
            editor,
            &caps(&[Capability::MailExpunge, Capability::MailDelete]),
            &all,
            Capability::MailExpunge
        ),
        Ok(())
    );
}

#[test]
fn names_scopes_and_maps_device_scopes() {
    for scope in Scope::ALL {
        assert_eq!(Scope::parse(scope.as_str()), Some(scope));
        assert!(Scopes::all().has(scope));
    }
    let device = Scopes::of_device(&BTreeSet::from([DeviceScope::Read, DeviceScope::Notify]));
    assert_eq!(device, Scopes::only([Scope::Read]));
    assert!(!device.has(Scope::Rules));
    let every = Scopes::of_device(&DeviceScope::ALL.into());
    assert!(
        every.has(Scope::WriteMail) && every.has(Scope::SendMail) && every.has(Scope::WritePim)
    );
    assert!(!every.has(Scope::Rules), "a device never has rules");
}

#[test]
fn switches_turn_off_whole_capabilities() {
    let on = WriteSwitches::default();
    assert!(on.allow(Capability::MailSend) && on.allow(Capability::MailFlags));
    let no_send = WriteSwitches { send: false, ..on };
    assert!(!no_send.allow(Capability::MailSend));
    assert!(no_send.allow(Capability::MailFlags));
    let off = WriteSwitches {
        enabled: false,
        ..on
    };
    assert!(!off.allow(Capability::MailFlags));
}

struct Fixture {
    store: Arc<dyn Store>,
    archive: ArchiveId,
    account: AccountId,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let archive = store.create_archive("Mail").await.unwrap().id;
    let account = store
        .create_account(account(archive, AccountKind::Imap, serde_json::json!({})))
        .await
        .unwrap()
        .id;
    Fixture {
        store,
        archive,
        account,
        _dir: dir,
    }
}

fn account(archive: ArchiveId, kind: AccountKind, settings: serde_json::Value) -> NewAccount {
    NewAccount {
        archive_id: archive,
        kind,
        name: "Mail".into(),
        address: Some("ada@example.com".into()),
        settings,
        sealed_secret: None,
        enabled: true,
    }
}

impl Fixture {
    async fn user(&self, email: &str, role: InstanceRole) -> User {
        self.store
            .create_user(NewUser {
                email: email.into(),
                name: "Someone".into(),
                role,
                password_hash: None,
            })
            .await
            .unwrap()
    }

    fn access(&self) -> Access {
        Access::new(self.store.clone(), WriteSwitches::default())
    }

    async fn allow(&self, list: &[Capability]) {
        self.store
            .set_account_capabilities(self.account, &caps(list))
            .await
            .unwrap();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Yes,
    Missing(&'static str),
    Denied(Denial),
    Consent,
}

fn answer<T>(result: Result<T, AccessError>) -> Answer {
    match result {
        Ok(_) => Answer::Yes,
        Err(AccessError::NotFound(what)) => Answer::Missing(what),
        Err(AccessError::Denied(denial)) => Answer::Denied(denial),
        Err(AccessError::ConsentMissing) => Answer::Consent,
        Err(AccessError::Store(error)) => panic!("the store failed: {error}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_only_when_every_lock_is_open() {
    let f = fixture().await;
    let owner = f.user("ada@example.com", InstanceRole::Owner).await;
    let access = f.access();
    let all = Scopes::all();

    assert_eq!(
        answer(
            access
                .may_write(&owner, &all, f.account, Capability::MailFlags)
                .await
        ),
        Answer::Denied(Denial::Capability),
        "a capability that is off allowed a write"
    );
    f.allow(&[Capability::MailFlags, Capability::MailSend])
        .await;
    assert_eq!(
        answer(
            access
                .may_write(&owner, &all, f.account, Capability::MailSend)
                .await
        ),
        Answer::Yes
    );
    assert_eq!(
        answer(
            access
                .may_write(&owner, &all, AccountId::new(), Capability::MailSend)
                .await
        ),
        Answer::Missing("account")
    );
    assert_eq!(
        answer(
            access
                .may_write(
                    &owner,
                    &Scopes::only([Scope::Read]),
                    f.account,
                    Capability::MailFlags
                )
                .await
        ),
        Answer::Denied(Denial::Scope)
    );

    let grace = f.user("grace@example.org", InstanceRole::Member).await;
    assert_eq!(
        answer(
            access
                .may_write(&grace, &all, f.account, Capability::MailFlags)
                .await
        ),
        Answer::Missing("account")
    );
    f.store.set_grants(grace.id, &[f.archive]).await.unwrap();
    assert_eq!(
        answer(
            access
                .may_write(&grace, &all, f.account, Capability::MailFlags)
                .await
        ),
        Answer::Denied(Denial::Grant),
        "a reader wrote"
    );
    f.store
        .set_grant_role(grace.id, f.archive, Some(GrantRole::Editor))
        .await
        .unwrap();
    assert_eq!(
        answer(
            access
                .may_write(&grace, &all, f.account, Capability::MailFlags)
                .await
        ),
        Answer::Yes
    );
    assert_eq!(
        answer(
            access
                .may_write(&grace, &all, f.account, Capability::MailSend)
                .await
        ),
        Answer::Denied(Denial::Grant),
        "an editor sent"
    );

    let alan = f.user("alan@example.org", InstanceRole::Admin).await;
    assert_eq!(
        answer(
            access
                .may_write(&alan, &all, f.account, Capability::MailFlags)
                .await
        ),
        Answer::Denied(Denial::Grant),
        "an admin wrote without a grant"
    );

    let off = Access::new(
        f.store.clone(),
        WriteSwitches {
            send: false,
            ..WriteSwitches::default()
        },
    );
    assert_eq!(
        answer(
            off.may_write(&owner, &all, f.account, Capability::MailSend)
                .await
        ),
        Answer::Denied(Denial::Switch),
        "the send switch did not close sending"
    );
    assert_eq!(
        answer(
            off.may_write(&owner, &all, f.account, Capability::MailFlags)
                .await
        ),
        Answer::Yes
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn asks_for_the_sign_in_consent_only_for_mail() {
    let f = fixture().await;
    let owner = f.user("ada@example.com", InstanceRole::Owner).await;
    let graph = f
        .store
        .create_account(account(
            f.archive,
            AccountKind::Graph,
            serde_json::json!({ "oauth": { "write": false } }),
        ))
        .await
        .unwrap();
    assert!(write_consent_missing(&graph));
    f.store
        .set_account_capabilities(
            graph.id,
            &caps(&[Capability::MailFlags, Capability::MailSend]),
        )
        .await
        .unwrap();
    let access = f.access();
    let all = Scopes::all();
    assert_eq!(
        answer(
            access
                .may_write(&owner, &all, graph.id, Capability::MailFlags)
                .await
        ),
        Answer::Consent
    );
    assert_eq!(
        answer(
            access
                .may_write(&owner, &all, graph.id, Capability::MailSend)
                .await
        ),
        Answer::Yes,
        "sending is not a change to mail on the server"
    );
    assert!(
        access
            .writable(&owner, &all, &graph)
            .await
            .unwrap()
            .is_empty(),
        "an account without the consent offers no mailbox changes"
    );

    let mut own_token = graph.clone();
    own_token.settings = serde_json::json!({});
    assert!(!write_consent_missing(&own_token));
    own_token.settings = serde_json::json!({ "oauth": { "write": true } });
    assert!(!write_consent_missing(&own_token));
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_what_is_writable_now() {
    let f = fixture().await;
    let owner = f.user("ada@example.com", InstanceRole::Owner).await;
    f.allow(&[
        Capability::MailFlags,
        Capability::MailExpunge,
        Capability::MailSend,
    ])
    .await;
    let stored = f.store.get_account(f.account).await.unwrap().unwrap();
    assert_eq!(
        f.access()
            .writable(&owner, &Scopes::all(), &stored)
            .await
            .unwrap(),
        vec![Capability::MailFlags],
        "mail.send is not a mailbox change and mail.expunge needs mail.delete"
    );
    let read_only = Scopes::only([Scope::Read]);
    assert!(
        f.access()
            .writable(&owner, &read_only, &stored)
            .await
            .unwrap()
            .is_empty()
    );
    let import = f
        .store
        .create_account(account(
            f.archive,
            AccountKind::Import,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    f.store
        .set_account_capabilities(import.id, &caps(&[Capability::MailFlags]))
        .await
        .unwrap();
    assert!(
        f.access()
            .writable(&owner, &Scopes::all(), &import)
            .await
            .unwrap()
            .is_empty(),
        "an import account has no server to change"
    );
}

fn transport(kind: TransportKind, account: Option<AccountId>, archive: ArchiveId) -> Transport {
    Transport {
        id: holtmail_core::TransportId::new(),
        archive_id: Some(archive),
        name: "Out".into(),
        kind,
        account_id: account,
        settings: serde_json::json!({}),
        sealed_secret: None,
        allowed_from: vec!["*@example.org".into()],
        limits: Default::default(),
        enabled: true,
        smtputf8: false,
        created_at: holtmail_core::Timestamp::from_secs(1),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_only_with_a_sender_grant_and_the_transport_allowing_it() {
    let f = fixture().await;
    let owner = f.user("ada@example.com", InstanceRole::Owner).await;
    let grace = f.user("grace@example.org", InstanceRole::Member).await;
    let access = f.access();
    let all = Scopes::all();
    let smtp = transport(TransportKind::Smtp, None, f.archive);
    let account_transport = transport(TransportKind::Account, Some(f.account), f.archive);
    let orphan = transport(TransportKind::Account, None, f.archive);

    assert_eq!(
        answer(access.may_send(&owner, &all, f.archive, &smtp).await),
        Answer::Yes
    );
    assert_eq!(
        answer(
            access
                .may_send(&owner, &all, f.archive, &account_transport)
                .await
        ),
        Answer::Denied(Denial::Capability),
        "the account does not allow sending yet"
    );
    assert_eq!(
        answer(access.may_send(&owner, &all, f.archive, &orphan).await),
        Answer::Denied(Denial::Capability),
        "a transport whose account is gone"
    );
    f.allow(&[Capability::MailSend]).await;
    assert_eq!(
        answer(
            access
                .may_send(&owner, &all, f.archive, &account_transport)
                .await
        ),
        Answer::Yes
    );

    assert_eq!(
        answer(access.may_send(&grace, &all, f.archive, &smtp).await),
        Answer::Missing("archive")
    );
    assert_eq!(
        answer(access.may_send_somehow(&grace, &all, f.archive).await),
        Answer::Denied(Denial::Grant),
        "without a transport no grant is a closed lock, not a missing archive"
    );
    f.store.set_grants(grace.id, &[f.archive]).await.unwrap();
    f.store
        .set_grant_role(grace.id, f.archive, Some(GrantRole::Sender))
        .await
        .unwrap();
    assert_eq!(
        answer(access.may_send(&grace, &all, f.archive, &smtp).await),
        Answer::Yes
    );
    assert_eq!(
        answer(
            access
                .may_send(&grace, &Scopes::only([Scope::Read]), f.archive, &smtp)
                .await
        ),
        Answer::Denied(Denial::Scope)
    );
    let off = Access::new(
        f.store.clone(),
        WriteSwitches {
            send: false,
            ..WriteSwitches::default()
        },
    );
    assert_eq!(
        answer(off.may_send(&grace, &all, f.archive, &smtp).await),
        Answer::Denied(Denial::Switch)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn uses_rules_only_with_the_switch_grant_and_scope() {
    let f = fixture().await;
    let owner = f.user("ada@example.com", InstanceRole::Owner).await;
    let access = f.access();
    let all = Scopes::all();
    assert_eq!(
        answer(access.may_use_rules(&owner, &all, f.archive).await),
        Answer::Yes
    );
    assert_eq!(
        answer(
            access
                .may_use_rules(&owner, &Scopes::only([Scope::Read]), f.archive)
                .await
        ),
        Answer::Denied(Denial::Scope)
    );
    let grace = f.user("grace@example.org", InstanceRole::Member).await;
    assert_eq!(
        answer(access.may_use_rules(&grace, &all, f.archive).await),
        Answer::Missing("archive")
    );
    f.store.set_grants(grace.id, &[f.archive]).await.unwrap();
    assert_eq!(
        answer(access.may_use_rules(&grace, &all, f.archive).await),
        Answer::Denied(Denial::Grant)
    );
    let off = Access::new(
        f.store.clone(),
        WriteSwitches {
            rules: false,
            ..WriteSwitches::default()
        },
    );
    assert_eq!(
        answer(off.may_use_rules(&owner, &all, f.archive).await),
        Answer::Denied(Denial::Switch)
    );
}
