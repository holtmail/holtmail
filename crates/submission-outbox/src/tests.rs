use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_access::{Access, WriteSwitches};
use holtmail_blob::BlobId;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{ArchiveId, DeviceId, DeviceScope, FolderId, MessageId, OutboxId, Timestamp};
use holtmail_outbox::{Archiver, Outbox, OutboxConfig, OutboxError, Transports};
use holtmail_route::{Route, Strategy};
use holtmail_store::*;
use holtmail_store_sqlite::SqliteStore;
use holtmail_submission::testing::{self, Driver, Harness, IDENTITY_ADDRESS, Setup, World};
use holtmail_submission::{Actor, SubmissionError, SubmissionState, Submit};
use holtmail_submit::{
    Envelope, Limits, RecipientOutcome, RecipientResult, SubmitError, SubmitReport, Submitter,
};
use serde_json::json;

use crate::{Options, OutboxSubmission};

struct TakesEverything;

#[async_trait]
impl Submitter for TakesEverything {
    async fn check(&self) -> Result<Limits, SubmitError> {
        Ok(Limits::default())
    }

    async fn submit(&self, envelope: &Envelope, _: &[u8]) -> Result<SubmitReport, SubmitError> {
        Ok(SubmitReport {
            recipients: envelope
                .recipients
                .iter()
                .map(|address| RecipientResult {
                    address: address.clone(),
                    outcome: RecipientOutcome::Accepted,
                })
                .collect(),
        })
    }

    fn limits(&self) -> Limits {
        Limits::default()
    }
}

#[async_trait]
impl Transports for TakesEverything {
    async fn submitter(&self, _: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        Ok(Arc::new(TakesEverything))
    }

    async fn may_send(&self, _: ArchiveId, _: holtmail_core::UserId) -> Result<bool, OutboxError> {
        Ok(true)
    }
}

struct InsertOnly(Arc<SqliteStore>);

#[async_trait]
impl Archiver for InsertOnly {
    async fn archive(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        _: Option<FolderId>,
        _: &[String],
    ) -> Result<MessageId, OutboxError> {
        let parsed = holtmail_mime::parse(&raw).map_err(|e| OutboxError::Archive(e.to_string()))?;
        let inserted = self
            .0
            .insert_message(new_message(archive, &raw, &parsed))
            .await?;
        Ok(inserted.message_id)
    }
}

fn new_message(
    archive: ArchiveId,
    raw: &[u8],
    parsed: &holtmail_mime::ParsedMessage,
) -> NewMessage {
    NewMessage {
        archive_id: archive,
        blob_id: BlobId::of(raw),
        size: raw.len() as u64,
        rfc_message_id: parsed.message_id.clone(),
        parent_ids: vec![],
        subject: parsed.subject.clone(),
        from: parsed.from.clone(),
        to: parsed.to.clone(),
        cc: vec![],
        bcc: vec![],
        sent_at: None,
        received_at: None,
        preview: String::new(),
        attachments: vec![],
    }
}

struct Instance {
    _dir: tempfile::TempDir,
    outbox: Arc<Outbox>,
}

#[async_trait]
impl Driver for Instance {
    async fn deliver(&self, _: OutboxId) {
        while self.outbox.send_due().await.expect("send") > 0 {}
    }
}

const ALIAS_THREAD: &str = "thread@alias.example.org";

async fn world(setup: Setup, undo: Duration) -> World {
    world_in(tempfile::tempdir().unwrap(), setup, undo).await
}

async fn world_in(dir: tempfile::TempDir, setup: Setup, undo: Duration) -> World {
    let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
    let archive = store.create_archive("Sending").await.unwrap().id;
    let other_archive = store.create_archive("Elsewhere").await.unwrap().id;
    let user = store
        .create_user(NewUser {
            email: IDENTITY_ADDRESS.into(),
            name: "Ada".into(),
            role: InstanceRole::Owner,
            password_hash: None,
        })
        .await
        .unwrap()
        .id;
    let stranger = store
        .create_user(NewUser {
            email: "stranger@example.com".into(),
            name: "Stranger".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id;
    store.set_grants(stranger, &[archive]).await.unwrap();
    let transport = Transport {
        id: holtmail_core::TransportId::new(),
        archive_id: Some(archive),
        name: "Relay".into(),
        kind: TransportKind::Smtp,
        account_id: None,
        settings: json!({}),
        sealed_secret: None,
        allowed_from: vec!["*@example.com".into()],
        limits: TransportLimits {
            max_size: Some(setup.max_size),
            max_recipients: None,
            per_hour: None,
        },
        enabled: true,
        smtputf8: setup.smtputf8,
        created_at: Timestamp::from_millis(1),
    };
    store.create_transport(&transport).await.unwrap();
    let identity = store
        .create_identity(NewIdentity {
            archive_id: archive,
            address: IDENTITY_ADDRESS.into(),
            name: Some("Ada Example".into()),
            signature_html: None,
            signature_text: None,
            reply_to: None,
            transport_id: transport.id,
            sent_copy: SentCopyPolicy::None,
            bcc_self: false,
            account_id: None,
            alias: None,
            plus_addresses: true,
            active: true,
            created_by: Some(user),
        })
        .await
        .unwrap();

    let original = format!(
        "From: Shop <shop@example.net>\r\nTo: hidden@alias.example.org\r\n\
         Date: Sun, 08 Nov 2026 09:00:00 +0000\r\nMessage-ID: <{ALIAS_THREAD}>\r\n\
         Subject: Your order\r\n\r\nHello\r\n"
    );
    let parsed = holtmail_mime::parse(original.as_bytes()).unwrap();
    let inserted = store
        .insert_message(new_message(archive, original.as_bytes(), &parsed))
        .await
        .unwrap();
    store
        .set_message_alias(
            inserted.message_id,
            Some(&MessageAlias {
                provider: "simplelogin".into(),
                alias: "hidden@alias.example.org".into(),
                sender: None,
                reverse_alias: None,
                signed: false,
            }),
        )
        .await
        .unwrap();

    let takes = Arc::new(TakesEverything);
    let outbox = Arc::new(Outbox::new(
        store.clone(),
        blobs.clone(),
        takes,
        Arc::new(InsertOnly(store.clone())),
        OutboxConfig {
            undo_window: undo,
            ..OutboxConfig::default()
        },
    ));
    let access = Access::new(
        store.clone(),
        WriteSwitches {
            send: setup.open,
            ..WriteSwitches::default()
        },
    );
    let options = Options {
        messages_per_hour: setup.messages_per_hour,
        recipients_per_day: setup.recipients_per_day,
    };
    let submission = Arc::new(OutboxSubmission::new(
        store.clone(),
        blobs,
        access,
        outbox.clone(),
        options,
    ));
    World {
        submission,
        driver: Arc::new(Instance { _dir: dir, outbox }),
        archive,
        other_archive,
        sender: Actor {
            user,
            device: Some(DeviceId::new()),
            scopes: BTreeSet::from([DeviceScope::Read, DeviceScope::SendMail]),
        },
        stranger: Actor::web(stranger),
        identity: identity.id,
        alias_thread: ALIAS_THREAD.into(),
        leaking_recipient: "dave@example.org".into(),
    }
}

struct Real;

#[async_trait]
impl Harness for Real {
    async fn start(&self, setup: Setup) -> World {
        world(setup, Duration::from_secs(10)).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_contract() {
    testing::conformance(&Real).await;
}

fn message(id: &str, extra: &str) -> Bytes {
    Bytes::from(format!(
        "From: Ada <{IDENTITY_ADDRESS}>\r\nTo: bob@example.org\r\n\
         Date: Sun, 08 Nov 2026 10:00:00 +0000\r\nMessage-ID: <{id}@example.com>\r\n\
         Subject: Hello\r\n{extra}\r\nBody\r\n"
    ))
}

fn submit(w: &World, id: &str, actor: Actor, recipients: &[&str], extra: &str) -> Submit {
    Submit {
        actor,
        archive: w.archive,
        identity: w.identity,
        mail_from: IDENTITY_ADDRESS.into(),
        recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
        message: message(id, extra),
        send_at: None,
        undo_window: Some(Duration::ZERO),
        idempotency_key: None,
        allow_leaks: false,
        route: None,
    }
}

fn web(w: &World) -> Actor {
    Actor::web(w.sender.user)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_web_app_is_not_limited_or_guarded_again_with_a_route() {
    let setup = Setup {
        messages_per_hour: 1,
        recipients_per_day: 1,
        ..Setup::default()
    };
    let w = world(setup, Duration::ZERO).await;
    let s = &*w.submission;
    for n in 0..3 {
        s.queue(submit(
            &w,
            &format!("web-{n}"),
            web(&w),
            &["bob@example.org"],
            "",
        ))
        .await
        .expect("a person is not counted");
    }
    let leak = format!("In-Reply-To: <{ALIAS_THREAD}>\r\n");
    let unrouted = submit(&w, "reply", web(&w), &[&w.leaking_recipient], &leak);
    assert!(matches!(
        s.queue(unrouted.clone()).await,
        Err(SubmissionError::Leak)
    ));
    let routed = Submit {
        route: Some(Route {
            identity: w.identity.to_string(),
            from: IDENTITY_ADDRESS.into(),
            name: None,
            transport: "unused".into(),
            strategy: Strategy::Direct,
            reasons: Vec::new(),
        }),
        ..unrouted
    };
    let refused = s.queue(routed).await;
    assert!(
        matches!(refused, Err(SubmissionError::Invalid(_))),
        "the route still has to be the identity's: {refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_speaks_for_the_address_but_only_for_its_identity() {
    let w = world(Setup::default(), Duration::ZERO).await;
    let route = Route {
        identity: w.identity.to_string(),
        from: "shop@example.com".into(),
        name: None,
        transport: "unused".into(),
        strategy: Strategy::Direct,
        reasons: Vec::new(),
    };
    let base = submit(&w, "aliased", web(&w), &["bob@example.org"], "");
    let as_alias = Bytes::from(
        String::from_utf8(base.message.to_vec())
            .unwrap()
            .replace(IDENTITY_ADDRESS, "shop@example.com"),
    );
    let foreign = Submit {
        route: Some(Route {
            identity: holtmail_core::IdentityId::new().to_string(),
            ..route.clone()
        }),
        mail_from: "shop@example.com".into(),
        message: as_alias.clone(),
        ..base.clone()
    };
    assert!(matches!(
        w.submission.queue(foreign).await,
        Err(SubmissionError::Forbidden(_))
    ));
    let other_from = Submit {
        route: Some(route.clone()),
        mail_from: IDENTITY_ADDRESS.into(),
        ..base.clone()
    };
    assert!(matches!(
        w.submission.queue(other_from).await,
        Err(SubmissionError::Forbidden(_))
    ));
    let header_differs = Submit {
        route: Some(route.clone()),
        mail_from: "shop@example.com".into(),
        ..base.clone()
    };
    assert!(matches!(
        w.submission.queue(header_differs).await,
        Err(SubmissionError::FromMismatch)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_a_failed_audit_still_sends() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let w = world_in(dir, Setup::default(), Duration::ZERO).await;
    let sql = sqlx::SqlitePool::connect(&format!("sqlite:{}", db.display()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER no_audit BEFORE INSERT ON audit_log \
         BEGIN SELECT RAISE(ABORT, 'no audit'); END",
    )
    .execute(&sql)
    .await
    .unwrap();
    let request = Submit {
        idempotency_key: Some("retried".into()),
        ..submit(&w, "audited", w.sender.clone(), &["bob@example.org"], "")
    };
    assert!(matches!(
        w.submission.queue(request.clone()).await,
        Err(SubmissionError::Unavailable(_))
    ));
    w.driver.deliver(OutboxId::new()).await;

    sqlx::query("DROP TRIGGER no_audit")
        .execute(&sql)
        .await
        .unwrap();
    let queued = w.submission.queue(request).await.unwrap();
    assert_eq!(queued.state, SubmissionState::Queued);
    w.driver.deliver(queued.id).await;
    let sent = w.submission.status(&w.sender, queued.id).await.unwrap();
    assert_eq!(sent.state, SubmissionState::Sent);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_reverse_alias_is_no_leak() {
    let w = world(Setup::default(), Duration::ZERO).await;
    let s = &*w.submission;
    let leak = format!("In-Reply-To: <{ALIAS_THREAD}>\r\n");
    let mut request = submit(
        &w,
        "from-alias",
        w.sender.clone(),
        &["dave@example.org"],
        &leak,
    );
    request.mail_from = IDENTITY_ADDRESS.into();
    assert!(matches!(s.queue(request).await, Err(SubmissionError::Leak)));
    let queued = s
        .queue(submit(
            &w,
            "alias-self",
            w.sender.clone(),
            &["hidden@alias.example.org"],
            &leak,
        ))
        .await
        .expect("the alias itself sees nothing new");
    assert_eq!(queued.state, SubmissionState::Queued);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_message_gives_back_its_place_in_the_limit() {
    let setup = Setup {
        messages_per_hour: 1,
        ..Setup::default()
    };
    let w = world(setup, Duration::ZERO).await;
    let s = &*w.submission;
    let held = s
        .queue(Submit {
            undo_window: Some(Duration::from_secs(600)),
            ..submit(&w, "one", w.sender.clone(), &["bob@example.org"], "")
        })
        .await
        .unwrap();
    assert!(matches!(
        s.sender(&w.sender, w.archive, IDENTITY_ADDRESS).await,
        Err(SubmissionError::RateLimited { .. })
    ));
    s.undo(&w.sender, held.id).await.unwrap();
    s.sender(&w.sender, w.archive, IDENTITY_ADDRESS)
        .await
        .expect("an undone message does not count");
}
