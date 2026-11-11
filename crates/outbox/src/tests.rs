use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{
    AccountId, Address, ArchiveId, FolderId, MessageId, OutboxId, Timestamp, TransportId, UserId,
};
use holtmail_route::{Reason, Route, Strategy};
use holtmail_store::*;
use holtmail_store_sqlite::SqliteStore;
use holtmail_submit::{
    Envelope, Limits, RecipientOutcome, RecipientResult, Refusal, SubmitError, SubmitReport,
    Submitter,
};
use serde_json::json;

use crate::{Archiver, Outbox, OutboxConfig, OutboxError, Queue, SENT_BY_HOLTMAIL, Transports};

const START: i64 = 1_800_000_000_000;
const ADA: &str = "ada@example.com";
const BOB: &str = "bob@example.org";
const CAROL: &str = "carol@example.org";

type Script = Arc<Mutex<VecDeque<Result<SubmitReport, SubmitError>>>>;

#[derive(Default)]
struct FakeSubmitter {
    script: Script,
    received: Mutex<Vec<(Envelope, Vec<u8>)>>,
}

#[async_trait]
impl Submitter for FakeSubmitter {
    async fn check(&self) -> Result<Limits, SubmitError> {
        Ok(Limits::default())
    }

    async fn submit(
        &self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError> {
        self.received
            .lock()
            .unwrap()
            .push((envelope.clone(), message.to_vec()));
        self.script.lock().unwrap().pop_front().unwrap_or_else(|| {
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
        })
    }

    fn limits(&self) -> Limits {
        Limits::default()
    }
}

struct FakeTransports {
    submitter: Arc<FakeSubmitter>,
    build_error: Mutex<Option<SubmitError>>,
    senders_allowed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl Transports for FakeTransports {
    async fn submitter(&self, _: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        if let Some(error) = self.build_error.lock().unwrap().take() {
            return Err(error);
        }
        Ok(self.submitter.clone())
    }

    async fn may_send(&self, _: ArchiveId, _: UserId) -> Result<bool, OutboxError> {
        Ok(self.senders_allowed.load(Ordering::SeqCst))
    }
}

struct FakeArchiver {
    store: Arc<SqliteStore>,
    calls: Mutex<Vec<Option<FolderId>>>,
    raws: Mutex<Vec<Bytes>>,
}

#[async_trait]
impl Archiver for FakeArchiver {
    async fn archive(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        folder: Option<FolderId>,
        flags: &[String],
    ) -> Result<MessageId, OutboxError> {
        self.calls.lock().unwrap().push(folder);
        self.raws.lock().unwrap().push(raw.clone());
        let parsed = holtmail_mime::parse(&raw).map_err(|e| OutboxError::Archive(e.to_string()))?;
        let inserted = self
            .store
            .insert_message(NewMessage {
                archive_id: archive,
                blob_id: holtmail_blob::BlobId::of(&raw),
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
            })
            .await?;
        if let Some(folder) = folder {
            self.store
                .record_location(inserted.message_id, folder, None, flags, Timestamp::now())
                .await?;
        }
        Ok(inserted.message_id)
    }
}

struct World {
    _dir: tempfile::TempDir,
    store: Arc<SqliteStore>,
    blobs: Arc<FsBlobStore>,
    outbox: Outbox,
    clock: Arc<AtomicI64>,
    script: Script,
    submitter: Arc<FakeSubmitter>,
    transports: Arc<FakeTransports>,
    archiver: Arc<FakeArchiver>,
    archive: ArchiveId,
    account: AccountId,
    sent: FolderId,
    transport: TransportId,
    identity: Identity,
    user: UserId,
}

impl World {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    async fn with(adjust: impl FnOnce(&mut Transport)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let archive = store.create_archive("Sending").await.unwrap().id;
        let account = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Import,
                name: "Ada".into(),
                address: Some(ADA.into()),
                settings: json!({}),
                sealed_secret: None,
                enabled: false,
            })
            .await
            .unwrap()
            .id;
        let sent = store
            .upsert_folder(account, "Sent", Some(FolderRole::Sent))
            .await
            .unwrap()
            .id;
        let mut transport = Transport {
            id: TransportId::new(),
            archive_id: Some(archive),
            name: "Relay".into(),
            kind: TransportKind::Smtp,
            account_id: None,
            settings: json!({}),
            sealed_secret: None,
            allowed_from: vec!["*@example.com".into()],
            limits: TransportLimits {
                max_size: Some(1 << 20),
                max_recipients: Some(5),
                per_hour: None,
            },
            enabled: true,
            smtputf8: false,
            created_at: Timestamp::from_millis(START),
        };
        adjust(&mut transport);
        store.create_transport(&transport).await.unwrap();
        let user = store
            .create_user(NewUser {
                email: ADA.into(),
                name: "Ada".into(),
                role: InstanceRole::Member,
                password_hash: None,
            })
            .await
            .unwrap()
            .id;
        let identity = store
            .create_identity(NewIdentity {
                archive_id: archive,
                address: ADA.into(),
                name: Some("Ada Example".into()),
                signature_html: None,
                signature_text: None,
                reply_to: None,
                transport_id: transport.id,
                sent_copy: SentCopyPolicy::None,
                bcc_self: false,
                account_id: Some(account),
                alias: None,
                plus_addresses: false,
                active: true,
                created_by: Some(user),
            })
            .await
            .unwrap();
        let script: Script = Arc::default();
        let submitter = Arc::new(FakeSubmitter {
            script: script.clone(),
            received: Mutex::default(),
        });
        let transports = Arc::new(FakeTransports {
            submitter: submitter.clone(),
            build_error: Mutex::default(),
            senders_allowed: true.into(),
        });
        let archiver = Arc::new(FakeArchiver {
            store: store.clone(),
            calls: Mutex::default(),
            raws: Mutex::default(),
        });
        let clock = Arc::new(AtomicI64::new(START));
        let now = clock.clone();
        let outbox = Outbox::new(
            store.clone(),
            blobs.clone(),
            transports.clone(),
            archiver.clone(),
            OutboxConfig::default(),
        )
        .with_clock(Arc::new(move || {
            Timestamp::from_millis(now.load(Ordering::SeqCst))
        }));
        Self {
            _dir: dir,
            store,
            blobs,
            outbox,
            clock,
            script,
            submitter,
            transports,
            archiver,
            archive,
            account,
            sent,
            transport: transport.id,
            identity,
            user,
        }
    }

    fn advance(&self, by: Duration) {
        self.clock
            .fetch_add(by.as_millis() as i64, Ordering::SeqCst);
    }

    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.clock.load(Ordering::SeqCst))
    }

    fn script(&self, result: Result<SubmitReport, SubmitError>) {
        self.script.lock().unwrap().push_back(result);
    }

    fn route(&self) -> Route {
        Route {
            identity: self.identity.id.to_string(),
            from: ADA.into(),
            name: Some("Ada Example".into()),
            transport: self.transport.to_string(),
            strategy: Strategy::Direct,
            reasons: vec![Reason::Default],
        }
    }

    fn request(&self, n: u32, recipients: &[&str]) -> Queue {
        Queue {
            archive: self.archive,
            user: Some(self.user),
            device: None,
            route: self.route(),
            envelope: Envelope {
                mail_from: ADA.into(),
                recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
            },
            bytes: message(n, ADA, ""),
            send_at: None,
            hold: false,
            flow_run: None,
            idempotency_key: None,
        }
    }

    async fn get(&self, entry: &OutboxEntry) -> OutboxEntry {
        self.store.get_outbox(entry.id).await.unwrap().unwrap()
    }

    fn received(&self) -> Vec<(Envelope, Vec<u8>)> {
        self.submitter.received.lock().unwrap().clone()
    }
}

fn message(n: u32, from: &str, extra: &str) -> Bytes {
    Bytes::from(format!(
        "From: Ada Example <{from}>\r\nTo: Bob Example <{BOB}>\r\n{extra}Subject: Plans {n}\r\n\
         Message-ID: <plans-{n}@example.com>\r\nDate: Sat, 7 Nov 2026 10:00:00 +0000\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         See you at the caf\u{e9} at noon.\r\n"
    ))
}

fn report(results: &[(&str, Option<u16>)]) -> SubmitReport {
    SubmitReport {
        recipients: results
            .iter()
            .map(|(address, refused)| RecipientResult {
                address: (*address).to_owned(),
                outcome: match refused {
                    None => RecipientOutcome::Accepted,
                    Some(code) => RecipientOutcome::Refused(Refusal {
                        permanent: *code >= 500,
                        code: *code,
                        text: format!("{code} refused"),
                    }),
                },
            })
            .collect(),
    }
}

fn invalid(result: Result<OutboxEntry, OutboxError>) -> bool {
    matches!(result, Err(OutboxError::Invalid(_)))
}

#[tokio::test]
async fn queues_waits_out_the_undo_window_and_sends_the_exact_bytes() {
    let w = World::new().await;
    let request = w.request(1, &[BOB, CAROL]);
    let bytes = request.bytes.clone();
    let queued = w.outbox.queue(request).await.unwrap();
    assert_eq!(queued.state, OutboxState::Queued);
    assert_eq!(queued.new.send_at, Timestamp::from_millis(START + 10_000));
    assert_eq!(queued.new.rfc_message_id, "plans-1@example.com");
    assert_eq!(queued.new.route["reasons"][0]["code"], "default");
    assert_eq!(
        w.blobs.get(&queued.new.blob_id).await.unwrap(),
        Some(bytes.clone()),
        "the bytes were not stored before queueing"
    );
    assert!(queued.message_id.is_none());

    w.advance(Duration::from_secs(9));
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "sent inside the undo window"
    );
    w.advance(Duration::from_secs(1));
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);

    let sent = w.get(&queued).await;
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.sent_at, Some(w.now()));
    assert_eq!(sent.attempts, 1);
    assert!(
        sent.recipients
            .iter()
            .all(|r| r.state == RecipientState::Accepted)
    );
    let received = w.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].1,
        bytes.to_vec(),
        "the bytes changed on the way"
    );
    assert_eq!(received[0].0.recipients, [BOB, CAROL]);

    let message = sent.message_id.expect("sent bytes are archived");
    let stored = w.store.get_message(message).await.unwrap().unwrap();
    assert_eq!(stored.blob_id, queued.new.blob_id);
    let locations = w.store.locations(message).await.unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].folder_id, w.sent);
    assert_eq!(locations[0].uid, None);
    assert!(locations[0].flags.contains(&SENT_BY_HOLTMAIL.to_owned()));
    assert_eq!(
        *w.archiver.calls.lock().unwrap(),
        [None, Some(w.sent)],
        "archived only after the transport took it, or not at all"
    );

    let audit = w
        .store
        .list_audit(&AuditQuery {
            archive: Some(w.archive),
            before: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].entry.kind, AuditKind::MailSend);
    assert_eq!(audit[0].entry.user, Some(w.user));
    assert_eq!(audit[0].entry.account, Some(w.account));
    assert_eq!(audit[0].entry.count, 2);
    assert_eq!(
        audit[0].entry.targets,
        [sent.id.to_string(), message.to_string()]
    );
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "a message was sent twice"
    );
}

#[tokio::test]
async fn undo_release_schedule_and_cancel() {
    let w = World::new().await;
    let queued = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    let held = w.outbox.undo(queued.id).await.unwrap();
    assert_eq!(held.state, OutboxState::Draft);
    w.advance(Duration::from_secs(60));
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "an undone message was sent"
    );

    let later = Timestamp::from_millis(w.now().as_millis() + 3_600_000);
    let released = w.outbox.release(queued.id, Some(later)).await.unwrap();
    assert_eq!(released.state, OutboxState::Queued);
    assert_eq!(released.new.send_at, later);
    w.advance(Duration::from_secs(1800));
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "sent before its time"
    );
    let cancelled = w.outbox.cancel(queued.id).await.unwrap();
    assert_eq!(cancelled.state, OutboxState::Cancelled);
    assert!(matches!(
        w.outbox.undo(queued.id).await,
        Err(OutboxError::State(OutboxState::Cancelled))
    ));
    assert!(matches!(
        w.outbox.retry(queued.id, Some(w.user)).await,
        Err(OutboxError::State(OutboxState::Cancelled))
    ));
    w.advance(Duration::from_secs(7200));
    assert_eq!(w.outbox.send_due().await.unwrap(), 0);
    assert!(w.received().is_empty());

    let past = Timestamp::from_millis(START);
    let scheduled = w
        .outbox
        .queue(Queue {
            send_at: Some(past),
            hold: true,
            ..w.request(2, &[BOB])
        })
        .await
        .unwrap();
    assert_eq!(scheduled.state, OutboxState::Draft);
    assert_eq!(
        scheduled.new.send_at,
        Timestamp::from_millis(w.now().as_millis() + 10_000),
        "a past send time skipped the undo window"
    );
    assert!(matches!(
        w.outbox.undo(OutboxId::new()).await,
        Err(OutboxError::NotFound(_))
    ));
}

#[tokio::test]
async fn refuses_what_the_identity_or_transport_may_not_send() {
    let w = World::new().await;
    let lin = w
        .store
        .create_user(NewUser {
            email: "lin@example.org".into(),
            name: "Lin".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id;
    assert!(invalid(
        w.outbox
            .queue(Queue {
                user: Some(lin),
                ..w.request(1, &[BOB])
            })
            .await
    ));
    assert!(invalid(
        w.outbox
            .queue(Queue {
                bytes: message(1, "ada@example.org", ""),
                ..w.request(1, &[BOB])
            })
            .await
    ));
    let mut route = w.route();
    route.from = "mallory@example.org".into();
    assert!(invalid(
        w.outbox
            .queue(Queue {
                route,
                bytes: message(1, "mallory@example.org", ""),
                ..w.request(1, &[BOB])
            })
            .await
    ));
    assert!(invalid(
        w.outbox
            .queue(Queue {
                bytes: message(1, ADA, "Bcc: carol@example.org\r\n"),
                ..w.request(1, &[BOB])
            })
            .await
    ));
    assert!(invalid(
        w.outbox
            .queue(w.request(
                1,
                &[
                    BOB,
                    CAROL,
                    "d@example.org",
                    "e@example.org",
                    "f@example.org",
                    "g@example.org"
                ]
            ))
            .await
    ));
    assert!(invalid(
        w.outbox
            .queue(Queue {
                bytes: Bytes::from_static(b"From: ada@example.com\r\nSubject: x\r\n\r\nx\r\n"),
                ..w.request(1, &[BOB])
            })
            .await
    ));
    assert!(invalid(
        w.outbox
            .queue(Queue {
                bytes: Bytes::from_static(
                    b"From: ada@example.com\nMessage-ID: <x@example.com>\n\nx\n"
                ),
                ..w.request(1, &[BOB])
            })
            .await
    ));
    assert!(invalid(
        w.outbox
            .queue(Queue {
                envelope: Envelope {
                    mail_from: "bounces@example.org".into(),
                    recipients: vec![BOB.into()],
                },
                ..w.request(1, &[BOB])
            })
            .await
    ));
    let mut route = w.route();
    route.transport = TransportId::new().to_string();
    assert!(invalid(
        w.outbox
            .queue(Queue {
                route,
                ..w.request(1, &[BOB])
            })
            .await
    ));

    let mut inactive = w.identity.clone();
    inactive.fields.active = false;
    w.store.update_identity(&inactive).await.unwrap();
    assert!(invalid(w.outbox.queue(w.request(1, &[BOB])).await));
    let mut copy = w.identity.clone();
    copy.fields.bcc_self = true;
    w.store.update_identity(&copy).await.unwrap();
    let queued = w
        .outbox
        .queue(Queue {
            user: None,
            ..w.request(1, &[BOB])
        })
        .await
        .unwrap();
    assert_eq!(
        queued.new.recipients,
        [BOB, ADA],
        "Bcc-to-self was not added"
    );
    let parsed =
        holtmail_mime::parse(&w.blobs.get(&queued.new.blob_id).await.unwrap().unwrap()).unwrap();
    assert!(parsed.bcc.is_empty());
    assert_eq!(
        parsed.from,
        [Address {
            name: Some("Ada Example".into()),
            email: ADA.into()
        }]
    );

    let mut off = w.store.get_transport(w.transport).await.unwrap().unwrap();
    off.enabled = false;
    w.store.update_transport(&off).await.unwrap();
    assert!(invalid(w.outbox.queue(w.request(2, &[BOB])).await));
    w.advance(Duration::from_secs(60));
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "a turned-off transport sent"
    );
    assert_eq!(w.get(&queued).await.state, OutboxState::Queued);
}

#[tokio::test]
async fn queueing_is_idempotent_per_key() {
    let w = World::new().await;
    let device = holtmail_core::DeviceId::new();
    let keyed = Queue {
        device: Some(device),
        idempotency_key: Some("send-1".into()),
        ..w.request(1, &[BOB])
    };
    let first = w.outbox.queue(keyed.clone()).await.unwrap();
    let second = w.outbox.queue(keyed).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first.new.device_id,
        Some(device),
        "the row names the device"
    );
    w.advance(Duration::from_secs(10));
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);
    assert_eq!(w.outbox.send_due().await.unwrap(), 0);
    assert_eq!(w.received().len(), 1);
}

#[tokio::test]
async fn retries_temporary_failures_with_backoff_until_the_window_closes() {
    let w = World::new().await;
    let queued = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::Temporary("connection reset".into())));
    w.outbox.send_due().await.unwrap();
    let first = w.get(&queued).await;
    assert_eq!(first.state, OutboxState::Retrying);
    assert_eq!(
        first.next_attempt_at,
        Some(Timestamp::from_millis(w.now().as_millis() + 60_000))
    );
    assert!(first.last_error.is_some());

    w.advance(Duration::from_secs(59));
    assert_eq!(w.outbox.send_due().await.unwrap(), 0, "retried too early");
    w.advance(Duration::from_secs(1));
    w.script(Err(SubmitError::Refused {
        permanent: false,
        code: 451,
        text: "try later".into(),
    }));
    w.outbox.send_due().await.unwrap();
    let second = w.get(&queued).await;
    assert_eq!(second.attempts, 2);
    assert_eq!(
        second.next_attempt_at,
        Some(Timestamp::from_millis(w.now().as_millis() + 120_000)),
        "the wait did not double"
    );

    w.advance(Duration::from_secs(120));
    w.outbox.send_due().await.unwrap();
    let sent = w.get(&queued).await;
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.last_error, None);
    assert_eq!(sent.attempts, 3);
    assert_eq!(w.received().len(), 3);

    let late = w.outbox.queue(w.request(2, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::Unavailable("refused".into())));
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&late).await.state, OutboxState::Retrying);
    w.advance(Duration::from_secs(24 * 3600));
    w.script(Err(SubmitError::Unavailable("refused".into())));
    w.outbox.send_due().await.unwrap();
    let given_up = w.get(&late).await;
    assert_eq!(given_up.state, OutboxState::Failed, "retried past 24 hours");

    let retried = w.outbox.retry(late.id, Some(w.user)).await.unwrap();
    assert_eq!(retried.state, OutboxState::Queued);
    assert_eq!(retried.new.send_at, w.now());
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&late).await.state, OutboxState::Sent);

    assert_eq!(w.outbox.backoff(1), Duration::from_secs(60));
    assert_eq!(w.outbox.backoff(7), Duration::from_secs(3600));
    assert_eq!(w.outbox.backoff(40), Duration::from_secs(3600));
}

#[tokio::test]
async fn permanent_refusals_fail_and_partial_ones_are_reported() {
    let w = World::new().await;
    let refused = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::Refused {
        permanent: true,
        code: 554,
        text: "policy".into(),
    }));
    w.outbox.send_due().await.unwrap();
    let failed = w.get(&refused).await;
    assert_eq!(failed.state, OutboxState::Failed);
    assert!(failed.last_error.unwrap().contains("554"));

    let everyone = w.outbox.queue(w.request(2, &[BOB, CAROL])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::RecipientsRefused(
        report(&[(BOB, Some(550)), (CAROL, Some(550))]).recipients,
    )));
    w.outbox.send_due().await.unwrap();
    let nobody = w.get(&everyone).await;
    assert_eq!(nobody.state, OutboxState::Failed);
    assert!(nobody.recipients.iter().all(|r| matches!(
        r.state,
        RecipientState::Refused {
            code: 550,
            permanent: true,
            ..
        }
    )));

    let partial = w.outbox.queue(w.request(3, &[BOB, CAROL])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Ok(report(&[(BOB, None), (CAROL, Some(550))])));
    w.outbox.send_due().await.unwrap();
    let sent = w.get(&partial).await;
    assert_eq!(sent.state, OutboxState::Sent);
    assert_eq!(sent.recipients[0].state, RecipientState::Accepted);
    assert!(matches!(
        &sent.recipients[1].state,
        RecipientState::Refused { code: 550, text, .. } if text == "550 refused"
    ));

    let again = w
        .outbox
        .resend(partial.id, Some(w.user), None)
        .await
        .unwrap();
    assert_eq!(
        again.new.recipients,
        [CAROL],
        "resent to more than the refused"
    );
    assert_eq!(again.new.resend_of, Some(partial.id));
    assert_eq!(again.new.blob_id, partial.new.blob_id);
    assert!(invalid(
        w.outbox
            .resend(
                partial.id,
                Some(w.user),
                Some(vec!["mallory@example.org".into()])
            )
            .await
    ));
    assert!(matches!(
        w.outbox.resend(again.id, Some(w.user), None).await,
        Err(OutboxError::State(OutboxState::Queued))
    ));
    w.advance(Duration::from_secs(10));
    w.outbox.send_due().await.unwrap();
    let resent = w.get(&again).await;
    assert_eq!(resent.state, OutboxState::Sent);
    assert_eq!(
        resent.message_id, sent.message_id,
        "the same bytes became two messages"
    );
    assert_eq!(w.received().last().unwrap().0.recipients, [CAROL]);
}

#[tokio::test]
async fn a_drop_after_the_final_dot_is_uncertain_until_a_person_retries() {
    let w = World::new().await;
    let queued = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::Uncertain("connection lost".into())));
    w.outbox.send_due().await.unwrap();
    let uncertain = w.get(&queued).await;
    assert_eq!(uncertain.state, OutboxState::Uncertain);
    assert!(uncertain.next_attempt_at.is_none());
    w.advance(Duration::from_secs(48 * 3600));
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "an uncertain send was repeated"
    );

    w.outbox.retry(queued.id, Some(w.user)).await.unwrap();
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&queued).await.state, OutboxState::Sent);
    assert_eq!(w.received().len(), 2);
}

#[tokio::test]
async fn sending_rows_found_at_start_become_uncertain() {
    let w = World::new().await;
    let queued = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    let claimed = w
        .store
        .claim_outbox(w.transport, w.now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.state, OutboxState::Sending);
    assert_eq!(w.outbox.recover().await.unwrap(), [queued.id]);
    let after = w.get(&queued).await;
    assert_eq!(after.state, OutboxState::Uncertain);
    assert!(after.last_error.is_some());
    assert!(w.outbox.recover().await.unwrap().is_empty());
    assert_eq!(w.outbox.send_due().await.unwrap(), 0);
}

#[tokio::test]
async fn one_message_in_flight_per_transport_within_the_hourly_limit() {
    let w = World::with(|t| t.limits.per_hour = Some(2)).await;
    let a = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    let b = w.outbox.queue(w.request(2, &[BOB])).await.unwrap();
    let c = w.outbox.queue(w.request(3, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        1,
        "two at once on one transport"
    );
    assert_eq!(w.get(&a).await.state, OutboxState::Sent);
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);
    assert_eq!(w.get(&b).await.state, OutboxState::Sent);
    assert_eq!(
        w.outbox.send_due().await.unwrap(),
        0,
        "the hourly limit was passed"
    );
    assert_eq!(w.get(&c).await.state, OutboxState::Queued);
    w.advance(Duration::from_secs(3600));
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);
    assert_eq!(w.get(&c).await.state, OutboxState::Sent);
}

#[tokio::test]
async fn transports_that_cannot_be_built_fail_or_wait() {
    let w = World::new().await;
    let queued = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    *w.transports.build_error.lock().unwrap() =
        Some(SubmitError::Unavailable("secret store locked".into()));
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&queued).await.state, OutboxState::Retrying);
    w.advance(Duration::from_secs(60));
    *w.transports.build_error.lock().unwrap() = Some(SubmitError::Auth("password refused".into()));
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&queued).await.state, OutboxState::Failed);
    assert!(w.received().is_empty());

    let orphan = w.outbox.queue(w.request(2, &[BOB])).await.unwrap();
    w.store.delete_identity(w.identity.id).await.unwrap();
    w.store.delete_transport(w.transport).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.outbox.send_due().await.unwrap();
    let failed = w.get(&orphan).await;
    assert_eq!(failed.state, OutboxState::Failed);
    assert!(w.received().is_empty());
}

fn blob_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(blob_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[tokio::test]
async fn a_failure_before_the_transport_retries_and_frees_the_transport() {
    let w = World::new().await;
    let queued = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    let files = blob_files(&w._dir.path().join("blobs"));
    let originals: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();
    for file in &files {
        std::fs::write(file, b"damaged").unwrap();
    }
    w.advance(Duration::from_secs(10));
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);
    let retrying = w.get(&queued).await;
    assert_eq!(
        retrying.state,
        OutboxState::Retrying,
        "left sending after a blob error"
    );
    assert!(w.received().is_empty());

    for (file, original) in files.iter().zip(&originals) {
        std::fs::write(file, original).unwrap();
    }
    w.advance(Duration::from_secs(60));
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);
    assert_eq!(w.get(&queued).await.state, OutboxState::Sent);
    assert_eq!(w.received().len(), 1);
}

#[tokio::test]
async fn rights_taken_away_while_waiting_fail_the_message() {
    let w = World::new().await;
    let revoked = w.outbox.queue(w.request(1, &[BOB])).await.unwrap();
    w.transports.senders_allowed.store(false, Ordering::SeqCst);
    w.advance(Duration::from_secs(10));
    w.outbox.send_due().await.unwrap();
    let failed = w.get(&revoked).await;
    assert_eq!(failed.state, OutboxState::Failed);
    assert!(
        w.received().is_empty(),
        "sent after the grant was taken away"
    );
    w.transports.senders_allowed.store(true, Ordering::SeqCst);

    let narrowed = w.outbox.queue(w.request(2, &[BOB])).await.unwrap();
    let mut transport = w.store.get_transport(w.transport).await.unwrap().unwrap();
    transport.allowed_from = vec!["*@example.net".into()];
    w.store.update_transport(&transport).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&narrowed).await.state, OutboxState::Failed);
    assert!(
        w.received().is_empty(),
        "sent outside the narrowed allowed_from"
    );
    transport.allowed_from = vec!["*@example.com".into()];
    w.store.update_transport(&transport).await.unwrap();

    let stranger = w
        .store
        .create_user(NewUser {
            email: CAROL.into(),
            name: "Carol".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id;
    assert!(
        matches!(
            w.outbox.retry(narrowed.id, Some(stranger)).await,
            Err(OutboxError::Invalid(_))
        ),
        "retried as an identity the user may not use"
    );
    w.outbox.retry(narrowed.id, Some(w.user)).await.unwrap();
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&narrowed).await.state, OutboxState::Sent);
}

async fn sent_one(w: &World, n: u32, recipients: &[&str]) -> OutboxEntry {
    let queued = w.outbox.queue(w.request(n, recipients)).await.unwrap();
    w.advance(Duration::from_secs(11));
    assert_eq!(w.outbox.send_due().await.unwrap(), 1);
    let sent = w.get(&queued).await;
    assert_eq!(sent.state, OutboxState::Sent);
    sent
}

async fn arrive(
    w: &World,
    raw: &[u8],
    folder: Option<FolderId>,
    labels: Option<&[String]>,
    live: bool,
) -> crate::Observed {
    let raw = Bytes::copy_from_slice(raw);
    let message = w
        .archiver
        .archive(w.archive, raw.clone(), folder, &[])
        .await
        .unwrap();
    let parsed = holtmail_mime::parse(&raw).unwrap();
    w.outbox
        .observe(crate::Arrival {
            archive: w.archive,
            message,
            raw: &raw,
            parsed: &parsed,
            folder,
            labels,
            live,
        })
        .await
        .unwrap()
}

fn dsn(original: &str, groups: &[(&str, &str, &str, &str)]) -> Vec<u8> {
    let mut status = String::from("Reporting-MTA: dns; mx.example.org\r\n");
    for (recipient, action, code, diagnostic) in groups {
        status.push_str(&format!(
            "\r\nFinal-Recipient: rfc822; {recipient}\r\nAction: {action}\r\nStatus: {code}\r\n\
             Diagnostic-Code: smtp; {diagnostic}\r\n"
        ));
    }
    format!(
        "From: Mail Delivery System <MAILER-DAEMON@mx.example.org>\r\nTo: {ADA}\r\n\
         Subject: Undelivered Mail Returned to Sender\r\nMessage-ID: <dsn-{}@mx.example.org>\r\n\
         Date: Sat, 7 Nov 2026 10:05:00 +0000\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/report; report-type=delivery-status; boundary=r\r\n\r\n\
         --r\r\nContent-Type: text/plain\r\n\r\nYour message could not be delivered.\r\n\
         --r\r\nContent-Type: message/delivery-status\r\n\r\n{status}\
         --r\r\nContent-Type: text/rfc822-headers\r\n\r\nFrom: {ADA}\r\n\
         Message-ID: <{original}>\r\nSubject: Plans\r\n\r\n--r--\r\n",
        groups.len() + original.len()
    )
    .into_bytes()
}

#[tokio::test]
async fn links_the_providers_sent_copy_and_flags_a_rewritten_from() {
    let w = World::new().await;
    let inbox = w
        .store
        .upsert_folder(w.account, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    let mut later = w.request(9, &[BOB]);
    later.send_at = Some(Timestamp::from_millis(START + 3_600_000));
    let waiting = w.outbox.queue(later).await.unwrap();
    let sent = sent_one(&w, 1, &[BOB]).await;
    let ours = w.blobs.get(&sent.new.blob_id).await.unwrap().unwrap();

    let none = arrive(&w, &ours, Some(w.sent), None, true).await;
    assert!(
        none.linked.is_empty(),
        "the sent bytes linked to themselves"
    );
    let elsewhere = arrive(
        &w,
        &message(1, ADA, "X-Copy: inbox\r\n"),
        Some(inbox),
        None,
        true,
    )
    .await;
    assert!(
        elsewhere.linked.is_empty(),
        "a copy outside Sent was linked"
    );

    let rewritten = message(1, "ada.example@mail.example.net", "Received: by smtp\r\n");
    let observed = arrive(&w, &rewritten, Some(w.sent), None, false).await;
    assert_eq!(observed.linked, [sent.id]);
    let linked = w.get(&sent).await;
    let copy = linked.synced_copy.expect("the copy is linked");
    assert_ne!(Some(copy), linked.message_id);
    assert_eq!(linked.state, OutboxState::Sent);
    let both = w.store.outbox_for_message(copy).await.unwrap();
    assert_eq!(both.len(), 1);
    let identity = w.store.get_identity(w.identity.id).await.unwrap().unwrap();
    let flag = identity.rewritten_from.expect("the identity is flagged");
    assert_eq!(flag.seen, "ada.example@mail.example.net");

    let labelled = message(1, ADA, "X-Gm-Copy: all mail\r\n");
    let labels = vec!["Sent".to_owned()];
    let observed = arrive(&w, &labelled, Some(inbox), Some(&labels), true).await;
    assert_eq!(
        observed.linked,
        [sent.id],
        "Gmail's Sent label is a Sent copy"
    );
    assert!(
        w.store
            .get_identity(w.identity.id)
            .await
            .unwrap()
            .unwrap()
            .rewritten_from
            .is_none(),
        "a copy with the identity's own From kept the warning"
    );

    let early = arrive(
        &w,
        &message(9, ADA, "X-Copy: 1\r\n"),
        Some(w.sent),
        None,
        true,
    )
    .await;
    assert!(
        early.linked.is_empty(),
        "a message not handed over was linked"
    );
    assert_eq!(w.get(&waiting).await.synced_copy, None);
}

#[tokio::test]
async fn failure_reports_bounce_recipients_and_delays_leave_a_note() {
    let w = World::new().await;
    let sent = sent_one(&w, 2, &[BOB, CAROL]).await;
    let report = dsn(
        "plans-2@example.com",
        &[(CAROL, "failed", "5.1.1", "550 5.1.1 no such user")],
    );
    let quiet = arrive(&w, &report, None, None, false).await;
    assert!(quiet.bounced.is_empty(), "a backfilled report counted");
    assert_eq!(w.get(&sent).await.state, OutboxState::Sent);

    let report = dsn(
        "plans-2@example.com",
        &[
            (CAROL, "failed", "5.1.1", "550 5.1.1 no such user"),
            ("stranger@example.net", "failed", "5.1.1", "550 unknown"),
        ],
    );
    let observed = arrive(&w, &report, None, None, true).await;
    assert_eq!(observed.bounced, [sent.id]);
    let bounced = w.get(&sent).await;
    assert_eq!(bounced.state, OutboxState::Bounced);
    assert_eq!(bounced.recipients[0].state, RecipientState::Accepted);
    assert_eq!(
        bounced.recipients[1].state,
        RecipientState::Bounced {
            status: Some("5.1.1".into()),
            text: Some("550 5.1.1 no such user".into()),
        }
    );
    let audit = w
        .store
        .list_audit(&AuditQuery {
            archive: Some(w.archive),
            before: None,
            limit: 20,
        })
        .await
        .unwrap();
    assert!(
        audit
            .iter()
            .any(|e| e.entry.kind == AuditKind::OutboxChanged
                && e.entry.targets == [sent.id.to_string(), "bounced".to_owned()]),
        "the bounce was not audited"
    );
    let again = arrive(&w, &[report.as_slice(), b" "].concat(), None, None, true).await;
    assert!(again.bounced.is_empty(), "the same bounce counted twice");

    let delay = dsn(
        "plans-2@example.com",
        &[(BOB, "delayed", "4.4.7", "421 try later")],
    );
    let observed = arrive(&w, &delay, None, None, true).await;
    assert_eq!(observed.delayed, [sent.id]);
    let noted = w.get(&sent).await;
    assert_eq!(noted.state, OutboxState::Bounced);
    assert_eq!(noted.recipients[0].state, RecipientState::Accepted);
    assert!(
        noted
            .note
            .as_deref()
            .unwrap()
            .contains("bob@example.org (4.4.7)")
    );

    let unknown = dsn(
        "someone-else@example.com",
        &[(BOB, "failed", "5.1.1", "550 no")],
    );
    assert_eq!(
        arrive(&w, &unknown, None, None, true).await,
        crate::Observed::default()
    );
}

#[tokio::test]
async fn an_unstructured_bounce_counts_for_the_only_recipient() {
    let w = World::new().await;
    let single = sent_one(&w, 3, &[BOB]).await;
    let several = sent_one(&w, 4, &[BOB, CAROL]).await;
    let bounce = |n: u32| {
        format!(
            "From: Mail Administrator <Postmaster@legacy.example.net>\r\nTo: {ADA}\r\n\
             Subject: Returned mail\r\nMessage-ID: <legacy-{n}@legacy.example.net>\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=l\r\n\r\n\
             --l\r\nContent-Type: text/plain\r\n\r\nCould not be delivered.\r\n\
             --l\r\nContent-Type: message/rfc822\r\n\r\nFrom: {ADA}\r\nTo: {BOB}\r\n\
             Message-ID: <plans-{n}@example.com>\r\n\r\nHello.\r\n--l--\r\n"
        )
    };
    let observed = arrive(&w, bounce(3).as_bytes(), None, None, true).await;
    assert_eq!(observed.bounced, [single.id]);
    assert_eq!(
        w.get(&single).await.recipients[0].state,
        RecipientState::Bounced {
            status: None,
            text: None
        }
    );
    let observed = arrive(&w, bounce(4).as_bytes(), None, None, true).await;
    assert_eq!(observed.bounced, [several.id]);
    let marked = w.get(&several).await;
    assert_eq!(marked.state, OutboxState::Bounced);
    assert!(
        marked
            .recipients
            .iter()
            .all(|r| r.state == RecipientState::Accepted),
        "a bounce that names nobody was pinned on someone"
    );
    assert!(marked.note.is_some());
}

#[tokio::test]
async fn an_append_policy_holds_the_sent_copy_for_the_account() {
    let w = World::new().await;
    let mut identity = w.identity.clone();
    identity.fields.sent_copy = SentCopyPolicy::Append("Sent".into());
    w.store.update_identity(&identity).await.unwrap();

    let refused = sent_one(&w, 70, &[BOB]).await;
    assert!(
        w.store
            .outbox_mail_changes(refused.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        w.get(&refused)
            .await
            .note
            .is_some_and(|n| n.contains("does not allow")),
        "without mail.append the message says why"
    );

    w.store
        .set_account_capabilities(w.account, &[Capability::MailAppend].into_iter().collect())
        .await
        .unwrap();
    let sent = sent_one(&w, 71, &[BOB]).await;
    let changes = w.store.outbox_mail_changes(sent.id).await.unwrap();
    assert_eq!(changes.len(), 1);
    let change = &changes[0];
    assert_eq!(change.state, MailChangeState::Held);
    assert_eq!(change.new.origin, MailChangeOrigin::SentCopy);
    assert_eq!(change.new.folder_id, w.sent);
    assert_eq!(change.new.message_id, sent.message_id);
    assert_eq!(
        change.new.op,
        MailOp::Append {
            blob_id: sent.new.blob_id,
            flags: vec!["\\Seen".into()],
            date: sent.sent_at.unwrap(),
        }
    );
    let audit = w
        .store
        .list_audit(&AuditQuery {
            archive: Some(w.archive),
            before: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(audit.iter().any(|e| e.entry.kind == AuditKind::MailAppend
        && e.entry.targets.contains(&change.id.to_string())));

    identity.fields.sent_copy = SentCopyPolicy::Append("Elsewhere".into());
    w.store.update_identity(&identity).await.unwrap();
    let lost = sent_one(&w, 72, &[BOB]).await;
    assert!(
        w.store
            .outbox_mail_changes(lost.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(w.get(&lost).await.note.is_some());
}

fn reports(w: &World) -> Vec<holtmail_mime::Dsn> {
    w.archiver
        .raws
        .lock()
        .unwrap()
        .iter()
        .filter_map(|raw| holtmail_mime::parse_dsn(raw))
        .collect()
}

fn from_device(w: &World, n: u32, recipients: &[&str]) -> Queue {
    Queue {
        device: Some(holtmail_core::DeviceId::new()),
        ..w.request(n, recipients)
    }
}

#[tokio::test]
async fn a_device_gets_a_report_for_mail_that_failed_after_it_was_taken() {
    let w = World::new().await;
    let refused = w
        .outbox
        .queue(from_device(&w, 1, &[BOB, CAROL]))
        .await
        .unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::RecipientsRefused(
        report(&[(BOB, Some(550)), (CAROL, Some(550))]).recipients,
    )));
    w.outbox.send_due().await.unwrap();
    let failed = w.get(&refused).await;
    assert_eq!(failed.state, OutboxState::Failed);
    assert!(
        failed
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("report")
    );
    let found = reports(&w);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].original_message_id.as_deref(),
        Some("plans-1@example.com")
    );
    let addressed: Vec<_> = found[0]
        .recipients
        .iter()
        .map(|r| r.final_recipient.as_deref().unwrap())
        .collect();
    assert_eq!(addressed, [BOB, CAROL]);
    assert_eq!(
        found[0].recipients[0].action,
        Some(holtmail_mime::DsnAction::Failed)
    );
    assert_eq!(found[0].recipients[0].status.as_deref(), Some("5.0.0"));
    let placed = w
        .store
        .local_places(w.user, w.archive, Some(FolderRole::Inbox))
        .await
        .unwrap();
    assert_eq!(placed.len(), 1, "the report shows in the user's inbox");
    assert!(
        w.store
            .locations(placed[0].message_id)
            .await
            .unwrap()
            .is_empty(),
        "never as an upstream place"
    );
}

#[tokio::test]
async fn a_report_names_only_who_was_refused_and_never_changes_the_row() {
    let w = World::new().await;
    let partly = w
        .outbox
        .queue(from_device(&w, 2, &[BOB, CAROL]))
        .await
        .unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Ok(report(&[(BOB, None), (CAROL, Some(550))])));
    w.outbox.send_due().await.unwrap();
    let sent = w.get(&partly).await;
    assert_eq!(sent.state, OutboxState::Sent);
    let found = reports(&w);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].recipients.len(), 1);
    assert_eq!(
        found[0].recipients[0].final_recipient.as_deref(),
        Some(CAROL)
    );
}

#[tokio::test]
async fn no_report_for_the_web_app_or_for_mail_that_left() {
    let w = World::new().await;
    let web = w.outbox.queue(w.request(3, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.script(Err(SubmitError::RecipientsRefused(
        report(&[(BOB, Some(550))]).recipients,
    )));
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&web).await.state, OutboxState::Failed);
    let device = w.outbox.queue(from_device(&w, 4, &[BOB])).await.unwrap();
    w.advance(Duration::from_secs(10));
    w.outbox.send_due().await.unwrap();
    assert_eq!(w.get(&device).await.state, OutboxState::Sent);
    assert!(reports(&w).is_empty());
    assert!(
        w.store
            .local_places(w.user, w.archive, None)
            .await
            .unwrap()
            .is_empty()
    );
}
