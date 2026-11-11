use std::fmt;
use std::str::FromStr;

use holtmail_blob::BlobId;
use holtmail_core::{Address, DraftId, MessageId, ThreadId, Timestamp};

use crate::mailbox::MailboxId;

/// What a member of a mailbox is: an archived message, or one saved version of a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmailRef {
    /// An archived message: `M<id>`.
    Message(MessageId),
    /// A version of a draft, from 1: `D<id>.<version>`.
    Draft {
        /// The draft.
        draft: DraftId,
        /// The version.
        version: u32,
    },
}

impl EmailRef {
    /// The id used by JMAP and in storage.
    pub fn key(&self) -> String {
        match self {
            Self::Message(id) => format!("M{id}"),
            Self::Draft { draft, version } => format!("D{draft}.{version}"),
        }
    }

    /// The email named by [`EmailRef::key`], or `None`.
    pub fn parse(key: &str) -> Option<Self> {
        if let Some(rest) = key.strip_prefix('M') {
            return MessageId::from_str(rest).ok().map(Self::Message);
        }
        let (draft, version) = key.strip_prefix('D')?.split_once('.')?;
        let version: u32 = version.parse().ok().filter(|v| *v > 0)?;
        Some(Self::Draft {
            draft: DraftId::from_str(draft).ok()?,
            version,
        })
    }

    /// The message, when it is one.
    pub fn message(&self) -> Option<MessageId> {
        match self {
            Self::Message(id) => Some(*id),
            Self::Draft { .. } => None,
        }
    }
}

impl fmt::Display for EmailRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}

/// What an email shows without its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The email.
    pub email: EmailRef,
    /// Its thread; `None` for a draft.
    pub thread: Option<ThreadId>,
    /// The BLAKE3 hash of its raw bytes.
    pub blob: BlobId,
    /// Size of the raw bytes.
    pub size: u64,
    /// Size of the raw bytes with every bare LF made CRLF, as IMAP serves them; equal to
    /// `size` for mail that has CRLF line ends.
    pub crlf_size: u64,
    /// `Message-ID` without angle brackets.
    pub rfc_message_id: Option<String>,
    /// The first `In-Reply-To` id, without angle brackets.
    pub in_reply_to: Option<String>,
    /// Subject, decoded.
    pub subject: Option<String>,
    /// `From`.
    pub from: Vec<Address>,
    /// `Sender`; empty when the header is missing.
    pub sender: Vec<Address>,
    /// `Reply-To`; empty when the header is missing.
    pub reply_to: Vec<Address>,
    /// `To`.
    pub to: Vec<Address>,
    /// `Cc`.
    pub cc: Vec<Address>,
    /// `Bcc`.
    pub bcc: Vec<Address>,
    /// The `Date` header.
    pub sent_at: Option<Timestamp>,
    /// When the server received it (IMAP `INTERNALDATE`, JMAP `receivedAt`), or when Holtmail
    /// archived it or the draft was saved when that is unknown.
    pub received_at: Timestamp,
    /// One line from the start of the body.
    pub preview: String,
    /// Whether it has an attachment that is not inline.
    pub has_attachment: bool,
    /// The selectable mailboxes it is a member of for the viewer, sorted by key.
    pub mailboxes: Vec<MailboxId>,
}

/// A range of byte offsets into an email's raw bytes, `start` included, `end` not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
}

impl ByteRange {
    /// How many bytes it covers.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether it covers none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One MIME part of an email, with where it sits in the raw bytes, so a protocol cuts sections
/// (`BODY[1.MIME]`, a JMAP part blob) out of the raw bytes without parsing again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The IMAP section number: empty for the whole message, `1`, `2.1`, ... for parts of a
    /// multipart. A `message/rfc822` part has one child, the embedded message, with the same
    /// id; that message's parts continue it (`2.1`).
    pub id: String,
    /// Media type and subtype, lowercase, e.g. `text/plain`; `text/plain` when missing.
    pub content_type: String,
    /// Content-Type parameters in order, names lowercase, values unquoted.
    pub params: Vec<(String, String)>,
    /// Content-Disposition value, lowercase.
    pub disposition: Option<String>,
    /// Content-Disposition parameters in order, names lowercase, values unquoted.
    pub disposition_params: Vec<(String, String)>,
    /// `Content-ID`, with its angle brackets.
    pub content_id: Option<String>,
    /// `Content-Description`.
    pub description: Option<String>,
    /// `Content-Transfer-Encoding`, lowercase; `7bit` when missing.
    pub transfer_encoding: String,
    /// `Content-Language` values.
    pub language: Vec<String>,
    /// `Content-Location`.
    pub location: Option<String>,
    /// The header, through the empty line that ends it.
    pub header: ByteRange,
    /// The body, encoded, without the line break before the next boundary delimiter.
    pub body: ByteRange,
    /// Line breaks in the body.
    pub lines: u64,
    /// The parts of a multipart, or the embedded message of a `message/rfc822`.
    pub parts: Vec<Part>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for email in [
            EmailRef::Message(MessageId::new()),
            EmailRef::Draft {
                draft: DraftId::new(),
                version: 3,
            },
        ] {
            assert_eq!(EmailRef::parse(&email.key()), Some(email));
        }
        let draft = DraftId::new();
        for bad in [
            String::new(),
            "M".into(),
            "Mx".into(),
            format!("D{draft}"),
            format!("D{draft}.0"),
            format!("D{draft}.x"),
        ] {
            assert_eq!(EmailRef::parse(&bad), None, "{bad}");
        }
    }
}
