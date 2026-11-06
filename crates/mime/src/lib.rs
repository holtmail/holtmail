//! Parsing raw RFC 5322 messages into the fields Holtmail stores, shows and indexes.
//!
//! [`parse`] never changes the raw bytes; everything it returns is derived from them and can be
//! produced again at any time.

use bytes::Bytes;
pub use holtmail_core::Address;
use mail_parser::{AddressList, MessageParser, MessagePart};

/// Characters kept in [`ParsedMessage::preview`].
pub const PREVIEW_CHARS: usize = 200;

/// The fields of one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMessage {
    /// `Message-ID` without angle brackets.
    pub message_id: Option<String>,
    /// `In-Reply-To` ids without angle brackets, in header order.
    pub in_reply_to: Vec<String>,
    /// `References` ids without angle brackets, oldest first as in the header.
    pub references: Vec<String>,
    /// Decoded `Subject`.
    pub subject: Option<String>,
    /// `Date` as Unix seconds in UTC; `None` when missing or unparseable.
    pub sent_at: Option<i64>,
    /// `From` mailboxes.
    pub from: Vec<Address>,
    /// `To` mailboxes, with groups flattened.
    pub to: Vec<Address>,
    /// `Cc` mailboxes, with groups flattened.
    pub cc: Vec<Address>,
    /// `Bcc` mailboxes, present only in messages from the sender's own Sent folder.
    pub bcc: Vec<Address>,
    /// `Reply-To` mailboxes.
    pub reply_to: Vec<Address>,
    /// The plain text body; derived from the HTML body when the message has no text part.
    pub text: Option<String>,
    /// The HTML body exactly as sent, unsanitized; `None` when the message has no HTML part.
    pub html: Option<String>,
    /// The start of the body as one line of plain text, at most [`PREVIEW_CHARS`] characters.
    pub preview: String,
    /// Attachments and inline parts that are not the message body, in document order.
    pub attachments: Vec<Attachment>,
    /// Calendar parts (`text/calendar`, `application/ics`) of this message, not of messages
    /// attached to it, in document order, inline or not: text parts converted to UTF-8 from their
    /// charset, others transfer-decoded only.
    pub calendars: Vec<Bytes>,
}

/// A part of a message that is not its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// File name from `Content-Disposition` or `Content-Type`, decoded.
    pub filename: Option<String>,
    /// Lowercase media type, e.g. `application/pdf`; `application/octet-stream` when missing.
    pub content_type: String,
    /// `Content-ID` without angle brackets, used by `cid:` links in the HTML body.
    pub content_id: Option<String>,
    /// Whether the part is meant to be shown inside the body rather than listed as a file.
    pub inline: bool,
    /// The decoded content.
    pub content: Bytes,
}

/// Why [`parse`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MimeError {
    /// The bytes have no header fields, so they are not a message.
    #[error("not an email message")]
    NotAMessage,
}

/// Parses the raw bytes of a message.
///
/// Malformed messages are parsed as far as possible: broken encodings, unknown charsets and damaged
/// MIME structure give partial fields, not an error. Only bytes without any header field fail.
pub fn parse(raw: &[u8]) -> Result<ParsedMessage, MimeError> {
    let message = MessageParser::new()
        .parse(raw)
        .ok_or(MimeError::NotAMessage)?;
    if message.headers().iter().next().is_none() {
        return Err(MimeError::NotAMessage);
    }

    let text = message.body_text(0).map(|body| body.into_owned());
    let html = message
        .html_body()
        .any(|part| part.is_content_type("text", "html"))
        .then(|| message.body_html(0).map(|body| body.into_owned()))
        .flatten();
    let preview = message
        .body_preview(PREVIEW_CHARS)
        .map(|preview| one_line(&preview))
        .unwrap_or_default();

    Ok(ParsedMessage {
        message_id: message.message_id().map(str::to_owned),
        in_reply_to: message
            .in_reply_to()
            .map(|ids| ids.iter().map(str::to_owned).collect())
            .unwrap_or_default(),
        references: message
            .references()
            .map(|ids| ids.iter().map(str::to_owned).collect())
            .unwrap_or_default(),
        subject: message.subject().map(str::to_owned),
        sent_at: message.date().map(|date| date.to_timestamp()),
        from: addresses(message.from()),
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        bcc: addresses(message.bcc()),
        reply_to: addresses(message.reply_to()),
        text,
        html,
        preview,
        attachments: message.attachments().map(attachment).collect(),
        calendars: message
            .root()
            .parts()
            .filter(|part| {
                part.is_content_type("text", "calendar")
                    || part.is_content_type("application", "ics")
            })
            .map(|part| match part.text() {
                Some(text) => Bytes::from(text.into_owned()),
                None => Bytes::from(part.decoded().into_owned()),
            })
            .collect(),
    })
}

/// The original message inside a Microsoft Exchange journal report, byte for byte as it was
/// attached: the first `message/rfc822` part of a message with an `X-MS-Journal-Report` header.
/// `None` for anything else, so the caller keeps the message as it came.
pub fn journal_original(raw: &[u8]) -> Option<Bytes> {
    let message = MessageParser::new().parse(raw)?;
    if !message.headers().contains("X-MS-Journal-Report") {
        return None;
    }
    let inner = message.messages().nth(1)?;
    let bytes = inner.raw();
    (!bytes.is_empty()).then(|| Bytes::copy_from_slice(bytes))
}

fn addresses(list: Option<AddressList<'_>>) -> Vec<Address> {
    list.map(|list| {
        list.mailboxes()
            .filter_map(|mailbox| {
                Some(Address {
                    name: mailbox.name().map(str::to_owned),
                    email: mailbox.address()?.to_owned(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn attachment(part: MessagePart<'_>) -> Attachment {
    let content_type = part
        .content_type()
        .map(|ct| match ct.subtype() {
            Some(subtype) => format!("{}/{}", ct.ctype(), subtype),
            None => ct.ctype().to_owned(),
        })
        .unwrap_or_else(|| "application/octet-stream".to_owned())
        .to_ascii_lowercase();
    Attachment {
        filename: part.attachment_name().map(str::to_owned),
        content_type,
        content_id: part.content_id().map(str::to_owned),
        inline: part.is_inline(),
        content: Bytes::from(part.decoded().into_owned()),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
