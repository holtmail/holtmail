//! Turning drafts, replies and forwards into RFC 5322 bytes.
//!
//! A pure library: no network, no files, no store. [`build`] turns a [`Draft`] into the exact
//! bytes to submit, [`reply`] and [`forward`] prepare drafts from an [`Original`], and
//! [`reply_subject`], [`forward_subject`] and [`shorten_references`] are the rules they follow.
//! [`imip()`] wraps an iTIP object (RFC 5546) in the mail that carries it (RFC 6047).

mod address;
mod build;
mod imip;
mod original;
mod quote;
mod subject;

pub use build::build;
pub use imip::{Answer, ImipKind, Invitation, imip, imip_subject, imip_text};
pub use original::{ForwardKind, Original, ReplyKind, answer_references, forward, reply};
pub use subject::{forward_subject, reply_subject, strip_prefixes};

use holtmail_core::Timestamp;

/// iTIP methods a [`CalendarPart`] may carry (RFC 5546 section 1.4).
pub const ITIP_METHODS: &[&str] = &[
    "PUBLISH",
    "REQUEST",
    "REPLY",
    "ADD",
    "CANCEL",
    "REFRESH",
    "COUNTER",
    "DECLINECOUNTER",
];

/// Most `References` ids a reply carries; see [`shorten_references`].
pub const MAX_REFERENCES: usize = 20;

/// Header fields a draft may add besides the ones [`Draft`] has fields for.
///
/// Names are matched case-insensitively; anything else is refused by [`build`].
pub const ALLOWED_HEADERS: &[&str] = &[
    "Auto-Submitted",
    "X-Auto-Response-Suppress",
    "Precedence",
    "Importance",
    "X-Priority",
];

/// One mailbox: an optional display name and an address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Mailbox {
    /// Display name, unencoded; non-ASCII is encoded by [`build`].
    pub name: Option<String>,
    /// The address, e.g. `ada@example.com`.
    pub address: String,
}

impl Mailbox {
    /// A mailbox without a display name.
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            name: None,
            address: address.into(),
        }
    }

    /// A mailbox with a display name.
    pub fn named(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            address: address.into(),
        }
    }
}

impl From<&holtmail_core::Address> for Mailbox {
    fn from(address: &holtmail_core::Address) -> Self {
        Self {
            name: address.name.clone().filter(|name| !name.trim().is_empty()),
            address: address.email.clone(),
        }
    }
}

/// A file sent with a draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attachment {
    /// File name shown to the recipient; non-ASCII is encoded (RFC 2231).
    pub name: String,
    /// Media type, e.g. `application/pdf`. `message/rfc822` attaches a whole message unchanged.
    pub content_type: String,
    /// The content, unencoded.
    pub bytes: Vec<u8>,
    /// `Content-ID` without angle brackets for a part the HTML body shows through `cid:`.
    /// `None` for an ordinary attachment.
    pub content_id: Option<String>,
}

/// An extra header field from [`ALLOWED_HEADERS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraHeader {
    /// Field name, one of [`ALLOWED_HEADERS`].
    pub name: String,
    /// Field value, ASCII without line breaks.
    pub value: String,
}

/// Everything needed to build one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// The sender.
    pub from: Mailbox,
    /// `To` recipients.
    pub to: Vec<Mailbox>,
    /// `Cc` recipients.
    pub cc: Vec<Mailbox>,
    /// Blind recipients: in [`Built::recipients`], never in the bytes.
    pub bcc: Vec<Mailbox>,
    /// `Reply-To` mailboxes; empty for none.
    pub reply_to: Vec<Mailbox>,
    /// Subject, unencoded.
    pub subject: String,
    /// Plain text body. When empty and `html` is set, a text version of the HTML is sent.
    pub text: String,
    /// HTML body from the editor, sanitized by [`build`] before it is sent.
    pub html: Option<String>,
    /// Attachments and inline parts, in the order they are sent.
    pub attachments: Vec<Attachment>,
    /// `In-Reply-To`: the id of the message this answers, without angle brackets.
    pub in_reply_to: Option<String>,
    /// `References` ids without angle brackets, oldest first.
    pub references: Vec<String>,
    /// Extra header fields, in order.
    pub headers: Vec<ExtraHeader>,
    /// Domain of the generated `Message-ID`; the domain of `from` when `None`.
    pub message_id_domain: Option<String>,
    /// `Date`; the current time when `None`.
    pub date: Option<Timestamp>,
    /// An iCalendar object sent as the last alternative of the body: an iMIP message.
    pub calendar: Option<CalendarPart>,
}

/// The `text/calendar` part of an iMIP message (RFC 6047).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarPart {
    /// The iTIP method, the same as the object's `METHOD`, such as `REQUEST`; written as the
    /// `method` parameter of the part's media type.
    pub method: String,
    /// The iCalendar object, UTF-8.
    pub bytes: Vec<u8>,
}

/// A built message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// The RFC 5322 bytes with CRLF line endings, ready for submission.
    pub bytes: Vec<u8>,
    /// The generated `Message-ID` without angle brackets: `<uuid v7>@<domain>`.
    pub message_id: String,
    /// The envelope recipients: `to`, then `cc`, then `bcc`, each address once (compared without
    /// regard to letter case, first spelling kept), with the domain in its ASCII form.
    pub recipients: Vec<String>,
}

/// Why a draft could not be built or prepared. Messages name the field, never its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    /// An address is not a valid `local@domain` address. Quoted local parts, address literals
    /// and non-ASCII local parts are not supported.
    #[error("invalid address in {0}")]
    Address(&'static str),
    /// A header value or name contains a CR, LF or NUL.
    #[error("line break or NUL in {0}")]
    Injection(&'static str),
    /// A message id is not of the form `left@right` without spaces or angle brackets.
    #[error("invalid message id in {0}")]
    MessageId(&'static str),
    /// An extra header is not in [`ALLOWED_HEADERS`] or its value is not allowed.
    #[error("header {0} is not allowed")]
    Header(String),
    /// An attachment has an invalid media type, name or content id.
    #[error("invalid attachment {0}")]
    Attachment(usize),
    /// The draft has no `To`, `Cc` or `Bcc` recipient.
    #[error("no recipients")]
    NoRecipients,
    /// The calendar part has a method outside RFC 5546 or is not UTF-8.
    #[error("invalid calendar part")]
    Calendar,
    /// A reply to the list was asked for, but the original has no usable `List-Post` address.
    #[error("the original has no list address")]
    NoListAddress,
}

/// `references` fit for a `References` field: ids a message cannot carry are dropped (with
/// their angle brackets removed otherwise), then the list is shortened to [`MAX_REFERENCES`] as
/// [`shorten_references`] does. Never fails, whatever the ids came from.
pub fn usable_references(references: &[String]) -> Vec<String> {
    let valid: Vec<String> = references
        .iter()
        .filter_map(|id| build::message_id_value(id, "References").ok())
        .collect();
    shorten_references(&valid, MAX_REFERENCES)
}

/// Shortens a `References` list to at most `max` ids by dropping ids from the middle, keeping
/// the first id (the thread's root) and the most recent ones, as RFC 5322 section 3.6.4
/// suggests. Repeated ids are dropped first, keeping their first position. `max` below 2 is
/// treated as 2.
pub fn shorten_references(references: &[String], max: usize) -> Vec<String> {
    let max = max.max(2);
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<String> = references
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect();
    if unique.len() <= max {
        return unique;
    }
    let mut out = Vec::with_capacity(max);
    out.push(unique[0].clone());
    out.extend_from_slice(&unique[unique.len() - (max - 1)..]);
    out
}
