//! Contract for handing prebuilt messages to a submission server.
//!
//! Holtmail is not an MTA: it never delivers to recipient hosts. A [`Submitter`] hands the exact
//! bytes of a message, with an explicit [`Envelope`], to a submission server (SMTP submission,
//! JMAP, Graph) that signs, queues and delivers it.

use async_trait::async_trait;

#[cfg(feature = "testing")]
pub mod testing;

/// Who a message is from and who it goes to, separate from its headers.
///
/// Bcc recipients are in `recipients` and never in the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The return path (`MAIL FROM`) that bounces go to. Empty is the null return path `<>`.
    pub mail_from: String,
    /// Every recipient (`RCPT TO`), in the order they are offered to the server.
    pub recipients: Vec<String>,
}

impl Envelope {
    /// Checks the envelope and the message against what every [`Submitter`] accepts, so a caller
    /// can refuse a message before queueing it.
    ///
    /// Fails with [`SubmitError::Invalid`] when there is no recipient, when an address is empty
    /// (other than the null return path), has no `@`, or holds whitespace, control characters or
    /// angle brackets, and when the message is empty, holds a NUL, a `CR` or `LF` outside a
    /// `CRLF` pair, or does not end with `CRLF`.
    pub fn validate(&self, message: &[u8]) -> Result<(), SubmitError> {
        if self.recipients.is_empty() {
            return Err(SubmitError::Invalid("the envelope has no recipient".into()));
        }
        if !self.mail_from.is_empty() && !valid_address(&self.mail_from) {
            return Err(SubmitError::Invalid(
                "the return path is not an address".into(),
            ));
        }
        if let Some(at) = self.recipients.iter().position(|r| !valid_address(r)) {
            return Err(SubmitError::Invalid(format!(
                "recipient {} is not an address",
                at + 1
            )));
        }
        if message.is_empty() {
            return Err(SubmitError::Invalid("the message is empty".into()));
        }
        if !message.ends_with(b"\r\n") {
            return Err(SubmitError::Invalid(
                "the message does not end with CRLF".into(),
            ));
        }
        let mut previous = 0u8;
        for (at, &byte) in message.iter().enumerate() {
            let bare_cr = previous == b'\r' && byte != b'\n';
            let bare_lf = byte == b'\n' && previous != b'\r';
            if byte == 0 || bare_cr || bare_lf {
                return Err(SubmitError::Invalid(format!(
                    "the message has a NUL or a bare CR or LF at byte {at}"
                )));
            }
            previous = byte;
        }
        Ok(())
    }
}

fn valid_address(address: &str) -> bool {
    address.contains('@')
        && !address
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
}

/// What the server answered to a refused recipient or message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// `true` when the server said not to try again (an SMTP `5xx`), `false` for a temporary
    /// refusal (`4xx`).
    pub permanent: bool,
    /// The SMTP reply code, or the closest SMTP code for transports that do not speak SMTP.
    pub code: u16,
    /// The server's text, with its enhanced status code when it gave one. It may name addresses:
    /// never log it.
    pub text: String,
}

/// What became of one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientOutcome {
    /// The server took the message for this recipient.
    Accepted,
    /// The server refused this recipient; the message did not go to it.
    Refused(Refusal),
}

/// One recipient of the envelope and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientResult {
    /// The address as it was in the envelope.
    pub address: String,
    /// Whether the server took the message for it.
    pub outcome: RecipientOutcome,
}

/// The result of a message the server took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitReport {
    /// One entry per envelope recipient, in envelope order. At least one is accepted.
    pub recipients: Vec<RecipientResult>,
}

impl SubmitReport {
    /// The recipients the server refused.
    pub fn refused(&self) -> impl Iterator<Item = (&str, &Refusal)> {
        self.recipients.iter().filter_map(|r| match &r.outcome {
            RecipientOutcome::Refused(refusal) => Some((r.address.as_str(), refusal)),
            RecipientOutcome::Accepted => None,
        })
    }
}

/// What a submission server has said about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Limits {
    /// The largest message the server takes, in bytes, when it said so.
    pub max_size: Option<u64>,
    /// The server takes internationalized addresses (RFC 6531). False when it did not say so.
    pub smtputf8: bool,
}

/// Why a message was not handed over.
///
/// Every error except [`SubmitError::Uncertain`] guarantees the server did not take the message
/// for any recipient, so sending it again cannot deliver it twice. [`SubmitError::is_retryable`]
/// tells which of those are worth trying again later without a change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// The server refused the credentials, or signing in is not possible with them.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The server refused the whole message.
    #[error("refused ({code}): {text}")]
    Refused {
        /// `true` for a permanent refusal (`5xx`), `false` for a temporary one (`4xx`).
        permanent: bool,
        /// The SMTP reply code or its closest equivalent.
        code: u16,
        /// The server's text. It may name addresses: never log it.
        text: String,
    },
    /// The server refused every recipient, so nothing was sent.
    #[error("every recipient was refused")]
    RecipientsRefused(Vec<RecipientResult>),
    /// The session broke before the message was complete: the server cannot have taken it.
    #[error("temporary failure: {0}")]
    Temporary(String),
    /// The connection was lost after the whole message was sent and before the server answered,
    /// or the server's answer did not say whether it took the message. It may have been
    /// delivered; only a person can decide whether to send it again.
    #[error("the outcome is unknown: {0}")]
    Uncertain(String),
    /// The message is larger than the server takes.
    #[error("the message is too large")]
    TooLarge {
        /// The server's limit in bytes, when it said one.
        limit: Option<u64>,
    },
    /// The envelope, the message or the configuration cannot be submitted as it is.
    #[error("invalid: {0}")]
    Invalid(String),
    /// The server could not be reached, or would not start a session (connection refused, TLS
    /// failure, a `421` greeting).
    #[error("unavailable: {0}")]
    Unavailable(String),
}

impl SubmitError {
    /// Whether the same submission may succeed later without a change: temporary refusals,
    /// broken sessions, unreachable servers, and recipient refusals of which at least one is
    /// temporary. Never true for [`SubmitError::Uncertain`].
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Refused { permanent, .. } => !permanent,
            Self::RecipientsRefused(results) => results.iter().any(
                |r| matches!(&r.outcome, RecipientOutcome::Refused(refusal) if !refusal.permanent),
            ),
            Self::Temporary(_) | Self::Unavailable(_) => true,
            Self::Auth(_) | Self::Uncertain(_) | Self::TooLarge { .. } | Self::Invalid(_) => false,
        }
    }

    /// Whether the server may have taken the message: only for [`SubmitError::Uncertain`].
    pub fn may_have_been_sent(&self) -> bool {
        matches!(self, Self::Uncertain(_))
    }
}

/// Hands prebuilt messages to one submission server.
///
/// Implementations are safe to share between tasks; concurrent calls do not interfere.
#[async_trait]
pub trait Submitter: Send + Sync {
    /// Connects and signs in without sending anything, and returns what the server said about
    /// itself. Also refreshes [`Submitter::limits`].
    async fn check(&self) -> Result<Limits, SubmitError>;

    /// Hands `message` to the server for the recipients in `envelope`.
    ///
    /// - **At most once.** One call offers the message to the server at most once and never
    ///   retries on its own, so a message is never delivered twice by one call.
    /// - **Bytes unchanged.** The server receives exactly `message`: no header is added, removed
    ///   or rewritten and line endings are not touched (transfer encoding such as dot-stuffing is
    ///   undone by the server).
    /// - Inputs that fail [`Envelope::validate`] are refused with [`SubmitError::Invalid`] before
    ///   connecting. A message larger than a known [`Limits::max_size`] is refused with
    ///   [`SubmitError::TooLarge`] before it is sent.
    /// - `Ok` means the server took the message for at least one recipient; the report says what
    ///   became of each. When every recipient is refused nothing is sent and the result is
    ///   [`SubmitError::RecipientsRefused`].
    async fn submit(
        &self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError>;

    /// What the server said about itself on the most recent connection; empty before the first.
    fn limits(&self) -> Limits;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> Envelope {
        Envelope {
            mail_from: "ada@example.com".into(),
            recipients: vec!["bob@example.org".into()],
        }
    }

    #[test]
    fn validates_envelopes_and_line_endings() {
        let good = b"Subject: Hi\r\n\r\nHello\r\n";
        assert_eq!(envelope().validate(good), Ok(()));
        let null = Envelope {
            mail_from: String::new(),
            ..envelope()
        };
        assert_eq!(null.validate(good), Ok(()));

        let invalid =
            |e: &Envelope, m: &[u8]| matches!(e.validate(m), Err(SubmitError::Invalid(_)));
        let none = Envelope {
            recipients: vec![],
            ..envelope()
        };
        assert!(invalid(&none, good));
        for bad in [
            "bob",
            "bob @example.org",
            "<bob@example.org>",
            "bob@example.org\r\nRCPT",
        ] {
            let e = Envelope {
                recipients: vec![bad.into()],
                ..envelope()
            };
            assert!(invalid(&e, good), "{bad:?} passed");
        }
        assert!(invalid(&envelope(), b""));
        assert!(invalid(&envelope(), b"Subject: Hi\r\n\r\nHello"));
        assert!(invalid(&envelope(), b"Subject: Hi\n\nHello\r\n"));
        assert!(invalid(&envelope(), b"Subject: Hi\r\n\r\nA\rB\r\n"));
        assert!(invalid(&envelope(), b"Subject: Hi\r\n\r\nA\0B\r\n"));
    }

    #[test]
    fn tells_which_errors_may_be_retried() {
        let refusal = |permanent| Refusal {
            permanent,
            code: if permanent { 550 } else { 450 },
            text: String::new(),
        };
        let refused = |permanent| {
            SubmitError::RecipientsRefused(vec![RecipientResult {
                address: "bob@example.org".into(),
                outcome: RecipientOutcome::Refused(refusal(permanent)),
            }])
        };
        assert!(refused(false).is_retryable());
        assert!(!refused(true).is_retryable());
        assert!(SubmitError::Temporary(String::new()).is_retryable());
        assert!(!SubmitError::Uncertain(String::new()).is_retryable());
        assert!(SubmitError::Uncertain(String::new()).may_have_been_sent());
        assert!(!SubmitError::Temporary(String::new()).may_have_been_sent());
        assert!(
            SubmitError::Refused {
                permanent: false,
                code: 451,
                text: String::new()
            }
            .is_retryable()
        );
    }
}
