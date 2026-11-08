//! Conformance suite every [`Submitter`] implementation runs in its own tests, against a fake
//! submission server it brings along.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{Envelope, RecipientOutcome, SubmitError, Submitter};

/// How the fake server behaves for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Behavior {
    /// Takes every message for every recipient.
    AcceptAll,
    /// Refuses this recipient permanently and takes the message for the others.
    RefuseRecipient(String),
    /// Refuses every message temporarily once it is complete.
    TemporaryFailure,
    /// Refuses every message permanently once it is complete.
    PermanentFailure,
    /// Reads the whole message, records it, and drops the connection without answering.
    DropAfterData,
    /// Advertises this limit in bytes and refuses larger messages.
    SizeLimit(u64),
}

/// A message as the fake server received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The return path the client gave.
    pub mail_from: String,
    /// The recipients the server took.
    pub recipients: Vec<String>,
    /// The message bytes after transfer encoding was undone.
    pub bytes: Vec<u8>,
}

/// Messages a fake server received, shared between the fake and the suite.
#[derive(Debug, Clone, Default)]
pub struct Deliveries(Arc<Mutex<Vec<Delivery>>>);

impl Deliveries {
    /// Records a received message, including one the fake will not answer.
    pub fn push(&self, delivery: Delivery) {
        self.0.lock().expect("deliveries").push(delivery);
    }

    /// Every message received so far, in order.
    pub fn all(&self) -> Vec<Delivery> {
        self.0.lock().expect("deliveries").clone()
    }
}

/// A started fake server and a submitter pointed at it.
pub struct Fake {
    /// A submitter configured for the fake server.
    pub submitter: Box<dyn Submitter>,
    /// What the fake server receives.
    pub deliveries: Deliveries,
}

/// What the transport under test can express, so the suite holds it only to what its protocol
/// allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// It hands the envelope's return path to the server, the null one included (SMTP, JMAP).
    /// Without it (Graph) the server picks the return path and the suite does not check it.
    pub return_path: bool,
    /// The server takes or refuses each recipient on its own (SMTP, JMAP). Without it (Graph)
    /// one refused recipient refuses the whole message, which must then be a definite,
    /// permanent refusal with nothing sent.
    pub per_recipient: bool,
}

impl Profile {
    /// Everything an SMTP submission server can express.
    pub const FULL: Profile = Profile {
        return_path: true,
        per_recipient: true,
    };
}

/// Starts fake submission servers for the suite.
#[async_trait]
pub trait Receiver: Send + Sync {
    /// Starts a fresh fake server that behaves as `behavior` and returns a submitter for it.
    async fn start(&self, behavior: Behavior) -> Fake;

    /// What the transport can express; [`Profile::FULL`] unless the implementation says less.
    fn profile(&self) -> Profile {
        Profile::FULL
    }
}

const ADA: &str = "ada@example.com";
const BOB: &str = "bob@example.org";
const CAROL: &str = "carol@example.org";
const REFUSED: &str = "nobody@example.org";

fn message() -> Vec<u8> {
    let mut bytes = b"From: Ada Example <ada@example.com>\r\n\
To: Bob Example <bob@example.org>\r\n\
Subject: Conformance\r\n\
Message-ID: <conformance@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
.leading dot\r\n\
.\r\n\
..two dots\r\n\
 trailing space \r\n\
\t\r\n\
Gr\xc3\xbc\xc3\x9fe\r\n"
        .to_vec();
    for line in 0..200 {
        bytes.extend_from_slice(format!("line {line} of the body\r\n").as_bytes());
    }
    bytes
}

fn envelope(recipients: &[&str]) -> Envelope {
    Envelope {
        mail_from: ADA.into(),
        recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
    }
}

/// Checks that the submitters `receiver` hands out keep the [`Submitter`] contract. Panics on the
/// first violation.
pub async fn conformance(receiver: &dyn Receiver) {
    accepts_and_keeps_bytes(receiver).await;
    refuses_invalid_input(receiver).await;
    reports_refused_recipients(receiver).await;
    maps_temporary_failure(receiver).await;
    maps_permanent_failure(receiver).await;
    reports_uncertain_after_data(receiver).await;
    enforces_size_limit(receiver).await;
}

async fn accepts_and_keeps_bytes(receiver: &dyn Receiver) {
    let fake = receiver.start(Behavior::AcceptAll).await;
    fake.submitter.check().await.expect("check");
    assert!(fake.deliveries.all().is_empty(), "check sent a message");

    let bytes = message();
    let report = fake
        .submitter
        .submit(&envelope(&[BOB, CAROL]), &bytes)
        .await
        .expect("submit");
    assert_eq!(report.recipients.len(), 2, "one result per recipient");
    assert_eq!(
        report.recipients[0].address, BOB,
        "results keep envelope order"
    );
    assert_eq!(
        report.recipients[1].address, CAROL,
        "results keep envelope order"
    );
    assert!(
        report
            .recipients
            .iter()
            .all(|r| r.outcome == RecipientOutcome::Accepted),
        "accepted recipients reported as refused"
    );
    assert_eq!(report.refused().count(), 0);

    let delivered = fake.deliveries.all();
    assert_eq!(delivered.len(), 1, "one submit must deliver once");
    if receiver.profile().return_path {
        assert_eq!(delivered[0].mail_from, ADA, "return path changed");
    }
    assert_eq!(
        delivered[0].recipients,
        vec![BOB, CAROL],
        "recipients changed"
    );
    assert!(delivered[0].bytes == bytes, "bytes changed on the way");

    if !receiver.profile().return_path {
        return;
    }
    let null = Envelope {
        mail_from: String::new(),
        recipients: vec![BOB.into()],
    };
    fake.submitter
        .submit(&null, &bytes)
        .await
        .expect("null return path");
    assert_eq!(
        fake.deliveries.all()[1].mail_from,
        "",
        "null return path changed"
    );
}

async fn refuses_invalid_input(receiver: &dyn Receiver) {
    let fake = receiver.start(Behavior::AcceptAll).await;
    let bytes = message();
    let invalid = [
        fake.submitter.submit(&envelope(&[]), &bytes).await,
        fake.submitter.submit(&envelope(&["bob"]), &bytes).await,
        fake.submitter
            .submit(&envelope(&[BOB]), b"Subject: x\n\nbare LF\n")
            .await,
        fake.submitter
            .submit(&envelope(&[BOB]), b"Subject: x\r\n\r\nno end")
            .await,
    ];
    for (at, result) in invalid.into_iter().enumerate() {
        assert!(
            matches!(result, Err(SubmitError::Invalid(_))),
            "invalid input {at} was not refused as invalid: {result:?}"
        );
    }
    assert!(fake.deliveries.all().is_empty(), "invalid input was sent");
}

async fn reports_refused_recipients(receiver: &dyn Receiver) {
    let fake = receiver
        .start(Behavior::RefuseRecipient(REFUSED.into()))
        .await;
    let bytes = message();
    if !receiver.profile().per_recipient {
        let error = fake
            .submitter
            .submit(&envelope(&[BOB, REFUSED, CAROL]), &bytes)
            .await
            .expect_err("a refused recipient refuses the whole message");
        assert!(
            matches!(
                error,
                SubmitError::Refused {
                    permanent: true,
                    ..
                } | SubmitError::RecipientsRefused(_)
            ),
            "a refused recipient mapped to {error:?}"
        );
        assert!(!error.is_retryable());
        assert!(!error.may_have_been_sent());
        assert!(
            fake.deliveries.all().is_empty(),
            "a refused message was sent"
        );
        return;
    }
    let report = fake
        .submitter
        .submit(&envelope(&[BOB, REFUSED, CAROL]), &bytes)
        .await
        .expect("partial refusal still sends");
    assert_eq!(report.recipients.len(), 3);
    assert_eq!(report.recipients[0].outcome, RecipientOutcome::Accepted);
    assert_eq!(report.recipients[2].outcome, RecipientOutcome::Accepted);
    let refused: Vec<_> = report.refused().collect();
    assert_eq!(refused.len(), 1, "one recipient refused");
    assert_eq!(refused[0].0, REFUSED);
    assert!(refused[0].1.permanent, "refusal should be permanent");
    assert!(
        (500..600).contains(&refused[0].1.code),
        "permanent refusal code {}",
        refused[0].1.code
    );
    let delivered = fake.deliveries.all();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].recipients, vec![BOB, CAROL]);
    assert!(delivered[0].bytes == bytes, "bytes changed on the way");

    let all = fake.submitter.submit(&envelope(&[REFUSED]), &bytes).await;
    match all {
        Err(error @ SubmitError::RecipientsRefused(_)) => {
            let SubmitError::RecipientsRefused(results) = &error else {
                unreachable!()
            };
            assert_eq!(results.len(), 1);
            assert!(
                !error.is_retryable(),
                "permanent refusals are not retryable"
            );
            assert!(!error.may_have_been_sent());
        }
        other => panic!("every recipient refused gave {other:?}"),
    }
    assert_eq!(
        fake.deliveries.all().len(),
        1,
        "a message with no recipient was sent"
    );
}

async fn maps_temporary_failure(receiver: &dyn Receiver) {
    let fake = receiver.start(Behavior::TemporaryFailure).await;
    let error = fake
        .submitter
        .submit(&envelope(&[BOB]), &message())
        .await
        .expect_err("temporary failure");
    assert!(
        matches!(
            error,
            SubmitError::Refused {
                permanent: false,
                ..
            } | SubmitError::Temporary(_)
        ),
        "temporary failure mapped to {error:?}"
    );
    assert!(error.is_retryable(), "temporary failure must be retryable");
    assert!(!error.may_have_been_sent());
}

async fn maps_permanent_failure(receiver: &dyn Receiver) {
    let fake = receiver.start(Behavior::PermanentFailure).await;
    let error = fake
        .submitter
        .submit(&envelope(&[BOB]), &message())
        .await
        .expect_err("permanent failure");
    assert!(
        matches!(
            error,
            SubmitError::Refused {
                permanent: true,
                code: 500..600,
                ..
            }
        ),
        "permanent failure mapped to {error:?}"
    );
    assert!(!error.is_retryable());
    assert!(!error.may_have_been_sent());
}

async fn reports_uncertain_after_data(receiver: &dyn Receiver) {
    let fake = receiver.start(Behavior::DropAfterData).await;
    let bytes = message();
    let error = fake
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .expect_err("dropped connection");
    assert!(
        matches!(error, SubmitError::Uncertain(_)),
        "drop after the message mapped to {error:?}"
    );
    assert!(error.may_have_been_sent());
    assert!(
        !error.is_retryable(),
        "uncertain must never be retried on its own"
    );
    let delivered = fake.deliveries.all();
    assert_eq!(delivered.len(), 1, "the submitter retried on its own");
    assert!(delivered[0].bytes == bytes, "bytes changed on the way");
}

async fn enforces_size_limit(receiver: &dyn Receiver) {
    let bytes = message();
    let limit = bytes.len() as u64 - 1;
    let fake = receiver.start(Behavior::SizeLimit(limit)).await;
    let limits = fake.submitter.check().await.expect("check");
    assert_eq!(limits.max_size, Some(limit), "advertised size not reported");
    assert_eq!(
        fake.submitter.limits(),
        limits,
        "limits not kept after check"
    );

    let error = fake
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .expect_err("too large");
    assert_eq!(error, SubmitError::TooLarge { limit: Some(limit) });
    assert!(!error.is_retryable());
    assert!(
        fake.deliveries.all().is_empty(),
        "an oversized message was sent"
    );

    let small = b"Subject: Small\r\n\r\nHi\r\n";
    fake.submitter
        .submit(&envelope(&[BOB]), small)
        .await
        .expect("a message under the limit");
    assert_eq!(fake.deliveries.all().len(), 1);
}
