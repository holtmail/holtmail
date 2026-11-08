//! Conformance suite every [`Submission`] implementation runs, and an in-memory fake.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, DeviceId, DeviceScope, IdentityId, OutboxId, Timestamp, UserId};

use crate::{
    Actor, DEFAULT_MAX_SIZE, RecipientDelivery, RecipientStatus, SendIdentity, Submission,
    SubmissionError, SubmissionLimits, SubmissionState, Submit, Submitted,
};

/// The identity address every [`World`] has.
pub const IDENTITY_ADDRESS: &str = "ada@example.com";

/// What a [`World`] allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// Messages per user within the hour.
    pub messages_per_hour: u32,
    /// Recipients per user within the day.
    pub recipients_per_day: u32,
    /// The identity's transport takes at most this many bytes.
    pub max_size: u64,
    /// The identity's transport takes internationalized addresses.
    pub smtputf8: bool,
    /// Whether sending is open; `false` closes the instance switch or capability.
    pub open: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            messages_per_hour: 1000,
            recipients_per_day: 10_000,
            max_size: 1024 * 1024,
            smtputf8: true,
            open: true,
        }
    }
}

/// Moves a message the way the real queue would.
#[async_trait]
pub trait Driver: Send + Sync {
    /// Hands the message to its transport, which takes it for every recipient.
    async fn deliver(&self, id: OutboxId);
}

/// A fresh instance for one group of checks.
pub struct World {
    /// The implementation under test.
    pub submission: Arc<dyn Submission>,
    /// Moves messages along.
    pub driver: Arc<dyn Driver>,
    /// The archive messages are sent from.
    pub archive: ArchiveId,
    /// An archive `sender` has no access to.
    pub other_archive: ArchiveId,
    /// May send as the identity [`IDENTITY_ADDRESS`] in `archive`, on a device.
    pub sender: Actor,
    /// A user with access to `archive` and no right to send as that identity.
    pub stranger: Actor,
    /// That identity's id.
    pub identity: IdentityId,
    /// A `Message-ID` (without brackets) of a thread that arrived through an alias.
    pub alias_thread: String,
    /// A recipient of a reply in that thread who would see the real mailbox.
    pub leaking_recipient: String,
}

/// Starts instances for the suite.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Starts a fresh instance as `setup` says.
    async fn start(&self, setup: Setup) -> World;
}

const BOB: &str = "bob@example.org";
const CAROL: &str = "carol@example.org";

fn message(from: &str, id: &str, extra: &str) -> Bytes {
    Bytes::from(format!(
        "From: Ada <{from}>\r\nTo: {BOB}\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
         Message-ID: <{id}@example.com>\r\nSubject: Hello\r\n{extra}\r\nBody\r\n"
    ))
}

fn request(world: &World, id: &str) -> Submit {
    Submit {
        actor: world.sender.clone(),
        archive: world.archive,
        identity: world.identity,
        mail_from: IDENTITY_ADDRESS.into(),
        recipients: vec![BOB.into(), CAROL.into()],
        message: message(IDENTITY_ADDRESS, id, ""),
        send_at: None,
        undo_window: Some(Duration::ZERO),
        idempotency_key: None,
        allow_leaks: false,
        route: None,
    }
}

fn forbidden<T: std::fmt::Debug>(result: Result<T, SubmissionError>, what: &str) {
    assert!(
        matches!(result, Err(SubmissionError::Forbidden(_))),
        "{what}: {result:?}"
    );
}

fn invalid<T: std::fmt::Debug>(result: Result<T, SubmissionError>, what: &str) {
    assert!(
        matches!(result, Err(SubmissionError::Invalid(_))),
        "{what}: {result:?}"
    );
}

fn not_found<T: std::fmt::Debug>(result: Result<T, SubmissionError>, what: &str) {
    assert!(
        matches!(result, Err(SubmissionError::NotFound(_))),
        "{what}: {result:?}"
    );
}

fn without_scope(actor: &Actor) -> Actor {
    Actor {
        scopes: [DeviceScope::Read].into_iter().collect(),
        ..actor.clone()
    }
}

/// Checks that `harness`'s instances keep the [`Submission`] contract. Panics on the first
/// violation.
pub async fn conformance(harness: &dyn Harness) {
    senders(harness).await;
    closed(harness).await;
    queueing(harness).await;
    refusals(harness).await;
    leaks(harness).await;
    idempotency(harness).await;
    rate_limits(harness).await;
    undoing(harness).await;
}

async fn senders(harness: &dyn Harness) {
    let setup = Setup::default();
    let w = harness.start(setup).await;
    let s = &*w.submission;
    for address in [IDENTITY_ADDRESS, "ADA@Example.com", "ada+shop@example.com"] {
        let found = s.sender(&w.sender, w.archive, address).await.unwrap();
        assert_eq!(found.id, w.identity, "{address}");
        assert_eq!(found.address, IDENTITY_ADDRESS);
    }
    forbidden(
        s.sender(&w.sender, w.archive, "").await,
        "the null return path",
    );
    forbidden(
        s.sender(&w.sender, w.archive, "eve@example.org").await,
        "another address",
    );
    forbidden(
        s.sender(&w.sender, w.archive, "adam@example.com").await,
        "a longer local part",
    );
    forbidden(
        s.sender(&w.stranger, w.archive, IDENTITY_ADDRESS).await,
        "a user without the identity",
    );
    forbidden(
        s.sender(&without_scope(&w.sender), w.archive, IDENTITY_ADDRESS)
            .await,
        "a device without send:mail",
    );
    forbidden(
        s.sender(&w.sender, w.other_archive, IDENTITY_ADDRESS).await,
        "another archive",
    );

    let identities = s.identities(&w.sender, w.archive).await.unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].id, w.identity);
    assert!(
        s.identities(&w.stranger, w.archive)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.identities(&without_scope(&w.sender), w.archive)
            .await
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        s.limits(&w.sender, w.archive).await.unwrap(),
        SubmissionLimits {
            max_size: setup.max_size,
            smtputf8: true
        }
    );
    assert_eq!(
        s.limits(&w.stranger, w.archive).await.unwrap(),
        SubmissionLimits {
            max_size: DEFAULT_MAX_SIZE,
            smtputf8: false
        },
        "someone who may not send gets the defaults"
    );
}

async fn closed(harness: &dyn Harness) {
    let w = harness
        .start(Setup {
            open: false,
            ..Setup::default()
        })
        .await;
    let s = &*w.submission;
    forbidden(
        s.sender(&w.sender, w.archive, IDENTITY_ADDRESS).await,
        "a closed lock",
    );
    assert!(s.identities(&w.sender, w.archive).await.unwrap().is_empty());
    forbidden(s.queue(request(&w, "closed")).await, "a closed lock");
}

async fn queueing(harness: &dyn Harness) {
    let w = harness.start(Setup::default()).await;
    let s = &*w.submission;
    let before = Timestamp::now();
    let queued = s.queue(request(&w, "first")).await.unwrap();
    assert_eq!(queued.state, SubmissionState::Queued);
    assert_eq!(queued.archive, w.archive);
    assert_eq!(queued.identity, w.identity);
    assert_eq!(queued.rfc_message_id, "first@example.com");
    assert_eq!(
        queued.recipients,
        vec![
            RecipientStatus {
                address: BOB.into(),
                delivery: RecipientDelivery::Pending
            },
            RecipientStatus {
                address: CAROL.into(),
                delivery: RecipientDelivery::Pending
            }
        ],
        "in submitted order"
    );
    assert_eq!(s.status(&w.sender, queued.id).await.unwrap(), queued);

    let plus = s
        .queue(Submit {
            mail_from: "ada+shop@example.com".into(),
            message: message("ada+shop@example.com", "plus", ""),
            ..request(&w, "plus")
        })
        .await
        .unwrap();
    assert_eq!(plus.identity, w.identity, "a plus address of the identity");

    let windowed = s
        .queue(Submit {
            undo_window: Some(Duration::from_secs(60)),
            ..request(&w, "windowed")
        })
        .await
        .unwrap();
    assert!(
        windowed.send_at.as_millis() >= before.as_millis() + 59_000,
        "not before the undo window ends"
    );
    let later = Timestamp::from_millis(before.as_millis() + 3_600_000);
    let scheduled = s
        .queue(Submit {
            send_at: Some(later),
            ..request(&w, "scheduled")
        })
        .await
        .unwrap();
    assert_eq!(scheduled.send_at, later);

    w.driver.deliver(queued.id).await;
    let sent = s.status(&w.sender, queued.id).await.unwrap();
    assert_eq!(sent.state, SubmissionState::Sent);
    assert!(
        sent.recipients
            .iter()
            .all(|r| r.delivery == RecipientDelivery::Accepted)
    );
}

async fn refusals(harness: &dyn Harness) {
    let setup = Setup {
        max_size: 2048,
        ..Setup::default()
    };
    let w = harness.start(setup).await;
    let s = &*w.submission;
    forbidden(
        s.queue(Submit {
            actor: w.stranger.clone(),
            ..request(&w, "stranger")
        })
        .await,
        "a user without the identity",
    );
    forbidden(
        s.queue(Submit {
            actor: without_scope(&w.sender),
            ..request(&w, "scope")
        })
        .await,
        "a device without send:mail",
    );
    forbidden(
        s.queue(Submit {
            mail_from: "eve@example.org".into(),
            ..request(&w, "return-path")
        })
        .await,
        "a return path of another address",
    );
    forbidden(
        s.queue(Submit {
            mail_from: String::new(),
            ..request(&w, "null")
        })
        .await,
        "the null return path",
    );
    forbidden(
        s.queue(Submit {
            identity: IdentityId::new(),
            ..request(&w, "unknown")
        })
        .await,
        "an unknown identity",
    );
    for (bytes, what) in [
        (message("eve@example.org", "from", ""), "another From"),
        (
            Bytes::from_static(
                b"From: ada@example.com, eve@example.org\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
                  Message-ID: <two@example.com>\r\n\r\nBody\r\n",
            ),
            "two From addresses",
        ),
        (
            Bytes::from_static(
                b"From: eve@example.org\r\nFrom: ada@example.com\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
                  Message-ID: <fields-first@example.com>\r\n\r\nBody\r\n",
            ),
            "another From field before the identity's",
        ),
        (
            Bytes::from_static(
                b"From: ada@example.com\r\nFrom: eve@example.org\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
                  Message-ID: <fields-last@example.com>\r\n\r\nBody\r\n",
            ),
            "another From field after the identity's",
        ),
        (
            Bytes::from_static(
                b"Date: Sun, 08 Nov 2026 10:00:00 +0000\r\nMessage-ID: <none@example.com>\r\n\r\nBody\r\n",
            ),
            "no From",
        ),
    ] {
        let result = s
            .queue(Submit {
                message: bytes,
                ..request(&w, "x")
            })
            .await;
        assert!(
            matches!(result, Err(SubmissionError::FromMismatch)),
            "{what}: {result:?}"
        );
    }
    invalid(
        s.queue(Submit {
            message: message(IDENTITY_ADDRESS, "bcc", "Bcc: carol@example.org\r\n"),
            ..request(&w, "bcc")
        })
        .await,
        "a Bcc header",
    );
    invalid(
        s.queue(Submit {
            message: Bytes::from_static(
                b"From: ada@example.com\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\r\nBody\r\n",
            ),
            ..request(&w, "x")
        })
        .await,
        "no Message-ID",
    );
    invalid(
        s.queue(Submit {
            message: Bytes::from_static(
                b"From: ada@example.com\r\nMessage-ID: <d@example.com>\r\n\r\nBody\r\n",
            ),
            ..request(&w, "x")
        })
        .await,
        "no Date",
    );
    invalid(
        s.queue(Submit {
            recipients: Vec::new(),
            ..request(&w, "nobody")
        })
        .await,
        "no recipients",
    );
    let padding = format!("X-Padding: {}\r\n", "x".repeat(4096));
    let result = s
        .queue(Submit {
            message: message(IDENTITY_ADDRESS, "big", &padding),
            ..request(&w, "big")
        })
        .await;
    assert!(
        matches!(result, Err(SubmissionError::TooLarge { limit: 2048 })),
        "{result:?}"
    );
    let fine = s.queue(request(&w, "fine")).await.unwrap();
    assert_eq!(
        fine.state,
        SubmissionState::Queued,
        "refusals left no trace"
    );
}

fn reply(world: &World, id: &str) -> Submit {
    let extra = format!("In-Reply-To: <{}>\r\n", world.alias_thread);
    Submit {
        recipients: vec![world.leaking_recipient.clone()],
        message: message(IDENTITY_ADDRESS, id, &extra),
        ..request(world, id)
    }
}

async fn leaks(harness: &dyn Harness) {
    let w = harness.start(Setup::default()).await;
    let s = &*w.submission;
    let result = s.queue(reply(&w, "leak")).await;
    assert!(matches!(result, Err(SubmissionError::Leak)), "{result:?}");
    let text = result.unwrap_err().to_string();
    assert!(
        !text.contains(&w.leaking_recipient) && !text.contains(IDENTITY_ADDRESS),
        "the refusal names no address"
    );
    s.queue(Submit {
        allow_leaks: true,
        ..reply(&w, "leak")
    })
    .await
    .expect("a confirmed leak goes through");
    s.queue(request(&w, "unrelated"))
        .await
        .expect("a message outside the thread is not checked");
}

async fn idempotency(harness: &dyn Harness) {
    let w = harness
        .start(Setup {
            messages_per_hour: 1,
            ..Setup::default()
        })
        .await;
    let s = &*w.submission;
    let keyed = Submit {
        idempotency_key: Some("device-1:message-1".into()),
        ..request(&w, "retried")
    };
    let first = s.queue(keyed.clone()).await.unwrap();
    let again = s.queue(keyed).await.expect("a retry is not limited");
    assert_eq!(again, first, "one submission");
}

async fn rate_limits(harness: &dyn Harness) {
    let w = harness
        .start(Setup {
            messages_per_hour: 1,
            ..Setup::default()
        })
        .await;
    let s = &*w.submission;
    s.queue(request(&w, "one")).await.unwrap();
    let limited = |result: Result<_, SubmissionError>, what: &str| match result {
        Err(SubmissionError::RateLimited { retry_after }) => {
            assert!(retry_after >= Duration::from_secs(1), "{what}")
        }
        other => panic!("{what}: {other:?}"),
    };
    limited(
        s.queue(request(&w, "two")).await.map(|_| ()),
        "over the hourly messages",
    );
    limited(
        s.sender(&w.sender, w.archive, IDENTITY_ADDRESS)
            .await
            .map(|_| ()),
        "the limit shows at MAIL FROM",
    );

    let w = harness
        .start(Setup {
            recipients_per_day: 3,
            ..Setup::default()
        })
        .await;
    let s = &*w.submission;
    s.queue(request(&w, "one")).await.unwrap();
    limited(
        s.queue(request(&w, "two")).await.map(|_| ()),
        "over the daily recipients",
    );
}

async fn undoing(harness: &dyn Harness) {
    let w = harness.start(Setup::default()).await;
    let s = &*w.submission;
    let queued = s
        .queue(Submit {
            undo_window: Some(Duration::from_secs(3600)),
            ..request(&w, "undone")
        })
        .await
        .unwrap();
    not_found(s.undo(&w.stranger, queued.id).await, "someone else's");
    not_found(s.status(&w.stranger, queued.id).await, "someone else's");
    not_found(s.undo(&w.sender, OutboxId::new()).await, "unknown");
    not_found(s.status(&w.sender, OutboxId::new()).await, "unknown");

    let undone = s.undo(&w.sender, queued.id).await.unwrap();
    assert_eq!(undone.state, SubmissionState::Cancelled);
    assert_eq!(
        s.undo(&w.sender, queued.id).await.unwrap(),
        undone,
        "undoing twice changes nothing"
    );
    assert_eq!(s.status(&w.sender, queued.id).await.unwrap(), undone);

    let gone = s.queue(request(&w, "gone")).await.unwrap();
    w.driver.deliver(gone.id).await;
    let result = s.undo(&w.sender, gone.id).await;
    assert!(
        matches!(result, Err(SubmissionError::TooLate(SubmissionState::Sent))),
        "{result:?}"
    );
}

struct Stored {
    submitted: Submitted,
    user: UserId,
}

#[derive(Default)]
struct FakeState {
    messages: HashMap<OutboxId, Stored>,
    keys: HashMap<(UserId, String), OutboxId>,
    counts: HashMap<UserId, (u32, u32)>,
}

/// An in-memory [`Submission`] that keeps the whole contract, for tests of protocol front ends.
/// Time-based limits never reset. Make instances with [`FakeSubmission::world`].
pub struct FakeSubmission {
    setup: Setup,
    archive: ArchiveId,
    sender: UserId,
    identity: IdentityId,
    alias_thread: String,
    leaking_recipient: String,
    state: Mutex<FakeState>,
}

fn headers(message: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(message);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push_str(line);
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    out
}

fn header<'h>(all: &'h [(String, String)], name: &str) -> Option<&'h str> {
    all.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

fn brackets(value: &str) -> &str {
    match (value.find('<'), value.rfind('>')) {
        (Some(open), Some(close)) if open < close => &value[open + 1..close],
        _ => value.trim(),
    }
}

impl FakeSubmission {
    /// A fresh fake as `setup` says, in a [`World`].
    pub fn world(setup: Setup) -> World {
        let archive = ArchiveId::new();
        let sender = UserId::new();
        let alias_thread = "thread@alias.example.org".to_owned();
        let leaking_recipient = "dave@example.org".to_owned();
        let identity = IdentityId::new();
        let fake = Arc::new(Self {
            setup,
            archive,
            sender,
            identity,
            alias_thread: alias_thread.clone(),
            leaking_recipient: leaking_recipient.clone(),
            state: Mutex::new(FakeState::default()),
        });
        World {
            submission: fake.clone(),
            driver: fake,
            archive,
            other_archive: ArchiveId::new(),
            sender: Actor {
                user: sender,
                device: Some(DeviceId::new()),
                scopes: BTreeSet::from([DeviceScope::Read, DeviceScope::SendMail]),
            },
            stranger: Actor::web(UserId::new()),
            identity,
            alias_thread,
            leaking_recipient,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake submission")
    }

    fn may_send(&self, actor: &Actor, archive: ArchiveId) -> bool {
        self.setup.open
            && archive == self.archive
            && actor.user == self.sender
            && actor.scopes.contains(&DeviceScope::SendMail)
    }

    fn own(&self) -> SendIdentity {
        SendIdentity {
            id: self.identity,
            address: IDENTITY_ADDRESS.into(),
            name: Some("Ada".into()),
        }
    }

    fn matches(address: &str) -> bool {
        let Some((local, domain)) = address.rsplit_once('@') else {
            return false;
        };
        let base = local.split('+').next().unwrap_or(local);
        format!("{base}@{domain}").eq_ignore_ascii_case(IDENTITY_ADDRESS)
    }

    fn check_limits(&self, user: UserId, recipients: u32) -> Result<(), SubmissionError> {
        let (messages, sent) = self.lock().counts.get(&user).copied().unwrap_or((0, 0));
        if messages >= self.setup.messages_per_hour
            || sent + recipients > self.setup.recipients_per_day
        {
            return Err(SubmissionError::RateLimited {
                retry_after: Duration::from_secs(3600),
            });
        }
        Ok(())
    }

    fn check_message(&self, request: &Submit) -> Result<String, SubmissionError> {
        let all = headers(&request.message);
        let from = header(&all, "from").ok_or(SubmissionError::FromMismatch)?;
        if from.matches('@').count() != 1
            || !Self::matches(brackets(from))
            || all.iter().filter(|(name, _)| name == "from").count() != 1
        {
            return Err(SubmissionError::FromMismatch);
        }
        if header(&all, "bcc").is_some() {
            return Err(SubmissionError::Invalid(
                "the message has a Bcc header".into(),
            ));
        }
        if header(&all, "date").is_none() {
            return Err(SubmissionError::Invalid("the message has no Date".into()));
        }
        let message_id = header(&all, "message-id")
            .map(brackets)
            .ok_or_else(|| SubmissionError::Invalid("the message has no Message-ID".into()))?
            .to_owned();
        if request.recipients.is_empty() {
            return Err(SubmissionError::Invalid("there are no recipients".into()));
        }
        if request.message.len() as u64 > self.setup.max_size {
            return Err(SubmissionError::TooLarge {
                limit: self.setup.max_size,
            });
        }
        let in_thread = header(&all, "in-reply-to").map(brackets) == Some(&self.alias_thread);
        if !request.allow_leaks
            && in_thread
            && request
                .recipients
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&self.leaking_recipient))
        {
            return Err(SubmissionError::Leak);
        }
        Ok(message_id)
    }

    fn visible(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.lock()
            .messages
            .get(&id)
            .filter(|m| m.user == actor.user)
            .map(|m| m.submitted.clone())
            .ok_or(SubmissionError::NotFound("submission"))
    }
}

#[async_trait]
impl Submission for FakeSubmission {
    async fn identities(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<SendIdentity>, SubmissionError> {
        Ok(if self.may_send(actor, archive) {
            vec![self.own()]
        } else {
            Vec::new()
        })
    }

    async fn limits(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<SubmissionLimits, SubmissionError> {
        Ok(if self.may_send(actor, archive) {
            SubmissionLimits {
                max_size: self.setup.max_size,
                smtputf8: self.setup.smtputf8,
            }
        } else {
            SubmissionLimits {
                max_size: DEFAULT_MAX_SIZE,
                smtputf8: false,
            }
        })
    }

    async fn sender(
        &self,
        actor: &Actor,
        archive: ArchiveId,
        address: &str,
    ) -> Result<SendIdentity, SubmissionError> {
        if !self.may_send(actor, archive) || !Self::matches(address) {
            return Err(SubmissionError::Forbidden("no such identity".into()));
        }
        self.check_limits(actor.user, 0)?;
        Ok(self.own())
    }

    async fn queue(&self, request: Submit) -> Result<Submitted, SubmissionError> {
        let key = request
            .idempotency_key
            .clone()
            .map(|k| (request.actor.user, k));
        if let Some(key) = &key {
            let state = self.lock();
            if let Some(id) = state.keys.get(key) {
                return Ok(state.messages[id].submitted.clone());
            }
        }
        if !self.may_send(&request.actor, request.archive)
            || request.identity != self.identity
            || !Self::matches(&request.mail_from)
        {
            return Err(SubmissionError::Forbidden("no such identity".into()));
        }
        self.check_limits(request.actor.user, request.recipients.len() as u32)?;
        let rfc_message_id = self.check_message(&request)?;
        let now = Timestamp::now().as_millis();
        let window = request.undo_window.unwrap_or(Duration::from_secs(10));
        let earliest = now + window.as_millis() as i64;
        let send_at = request
            .send_at
            .map_or(earliest, |at| at.as_millis().max(earliest));
        let id = OutboxId::new();
        let submitted = Submitted {
            id,
            archive: request.archive,
            identity: request.identity,
            state: SubmissionState::Queued,
            send_at: Timestamp::from_millis(send_at),
            recipients: request
                .recipients
                .iter()
                .map(|address| RecipientStatus {
                    address: address.clone(),
                    delivery: RecipientDelivery::Pending,
                })
                .collect(),
            rfc_message_id,
        };
        let mut state = self.lock();
        state.messages.insert(
            id,
            Stored {
                submitted: submitted.clone(),
                user: request.actor.user,
            },
        );
        if let Some(key) = key {
            state.keys.insert(key, id);
        }
        let count = state.counts.entry(request.actor.user).or_default();
        count.0 += 1;
        count.1 += request.recipients.len() as u32;
        Ok(submitted)
    }

    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        let current = self.visible(actor, id)?;
        match current.state {
            SubmissionState::Cancelled => Ok(current),
            SubmissionState::Queued => {
                let mut state = self.lock();
                let stored = state
                    .messages
                    .get_mut(&id)
                    .ok_or(SubmissionError::NotFound("submission"))?;
                stored.submitted.state = SubmissionState::Cancelled;
                Ok(stored.submitted.clone())
            }
            other => Err(SubmissionError::TooLate(other)),
        }
    }

    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.visible(actor, id)
    }
}

#[async_trait]
impl Driver for FakeSubmission {
    async fn deliver(&self, id: OutboxId) {
        let mut state = self.lock();
        let stored = state.messages.get_mut(&id).expect("a submitted message");
        stored.submitted.state = SubmissionState::Sent;
        for recipient in &mut stored.submitted.recipients {
            recipient.delivery = RecipientDelivery::Accepted;
        }
    }
}

/// Starts [`FakeSubmission`] instances, so the suite can run against the fake.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeHarness;

#[async_trait]
impl Harness for FakeHarness {
    async fn start(&self, setup: Setup) -> World {
        FakeSubmission::world(setup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fake_keeps_the_contract() {
        conformance(&FakeHarness).await;
    }
}
