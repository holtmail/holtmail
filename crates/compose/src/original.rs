use std::collections::HashSet;

use holtmail_core::Timestamp;
use holtmail_mime::ParsedMessage;

use crate::address::same;
use crate::quote::{attribution, escape, mailboxes, quote_text};
use crate::{
    Attachment, ComposeError, Draft, MAX_REFERENCES, Mailbox, forward_subject, reply_subject,
    shorten_references,
};

/// The fields of a stored message that replies and forwards need.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Original {
    /// `From` mailboxes.
    pub from: Vec<Mailbox>,
    /// `Reply-To` mailboxes.
    pub reply_to: Vec<Mailbox>,
    /// `To` mailboxes.
    pub to: Vec<Mailbox>,
    /// `Cc` mailboxes.
    pub cc: Vec<Mailbox>,
    /// `Mail-Followup-To` mailboxes.
    pub mail_followup_to: Vec<Mailbox>,
    /// The address from `List-Post` (`<mailto:list@example.org>`), without `mailto:` and query.
    /// `None` when absent or `NO`.
    pub list_post: Option<String>,
    /// `Message-ID` without angle brackets.
    pub message_id: Option<String>,
    /// `In-Reply-To` ids without angle brackets; used for threading when `references` is empty.
    pub in_reply_to: Vec<String>,
    /// `References` ids without angle brackets, oldest first.
    pub references: Vec<String>,
    /// Decoded subject.
    pub subject: String,
    /// `Date`.
    pub date: Option<Timestamp>,
    /// Plain text body.
    pub text: Option<String>,
    /// HTML body, unsanitized.
    pub html: Option<String>,
}

impl Original {
    /// The fields of a message parsed by `holtmail-mime`.
    ///
    /// `Mail-Followup-To` is read from the header list; a `List-Post` value of `NO` or without a
    /// `mailto:` address gives `list_post: None`.
    pub fn from_parsed(message: &ParsedMessage) -> Self {
        let header = |name: &str| {
            message
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(name))
                .map(|header| header.value.as_str())
        };
        Self {
            from: message.from.iter().map(Mailbox::from).collect(),
            reply_to: message.reply_to.iter().map(Mailbox::from).collect(),
            to: message.to.iter().map(Mailbox::from).collect(),
            cc: message.cc.iter().map(Mailbox::from).collect(),
            mail_followup_to: header("Mail-Followup-To")
                .map(parse_mailboxes)
                .unwrap_or_default(),
            list_post: header("List-Post").and_then(list_post),
            message_id: message.message_id.clone(),
            in_reply_to: message.in_reply_to.clone(),
            references: message.references.clone(),
            subject: message.subject.clone().unwrap_or_default(),
            date: message.sent_at.map(Timestamp::from_secs),
            text: message.text.clone(),
            html: message.html.clone(),
        }
    }

    /// Parses raw RFC 5322 bytes; `None` when they are not a message.
    pub fn parse(raw: &[u8]) -> Option<Self> {
        holtmail_mime::parse(raw)
            .ok()
            .map(|message| Self::from_parsed(&message))
    }
}

/// Who a reply goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    /// The author: `Reply-To` when present, otherwise `From`.
    Sender,
    /// The author and every other recipient, or `Mail-Followup-To` when present.
    All,
    /// The mailing list from `List-Post`.
    List,
}

/// How a message is forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardKind {
    /// The original's headers and body quoted in the new body, its attachments attached again.
    Inline,
    /// The original's raw bytes attached unchanged as `message/rfc822`.
    Attachment,
}

/// A reply draft to `original`, sent as `from`.
///
/// Recipients:
/// - [`ReplyKind::Sender`]: `Reply-To` when present, otherwise `From`; when those are all in
///   `own` or are `from` (a reply to one's own message), the original's `To` instead.
/// - [`ReplyKind::All`]: `Mail-Followup-To` when present; otherwise the sender as above plus the
///   original's `To` in `To`, and its `Cc` in `Cc`. Every address in `own` and `from`'s address is
///   removed. When `To` ends up empty, `Cc` moves to `To`.
/// - [`ReplyKind::List`]: the `List-Post` address; [`ComposeError::NoListAddress`] without one.
///
/// Addresses compare without regard to letter case and nobody appears twice. `In-Reply-To` is the
/// original's id and `References` its references (or its `In-Reply-To` when it has none) plus its
/// id, shortened by [`shorten_references`] to [`MAX_REFERENCES`]. The subject gets exactly one
/// `Re:`. The body quotes the original: text lines with `> ` under an attribution line, and, when
/// the original has HTML, the HTML inside `<blockquote type="cite">`.
pub fn reply(
    original: &Original,
    kind: ReplyKind,
    own: &[String],
    from: Mailbox,
) -> Result<Draft, ComposeError> {
    let is_own =
        |m: &Mailbox| same(&m.address, &from.address) || own.iter().any(|o| same(o, &m.address));
    let author = if original.reply_to.is_empty() {
        &original.from
    } else {
        &original.reply_to
    };
    let author: Vec<Mailbox> = if !author.is_empty() && author.iter().all(is_own) {
        original.to.clone()
    } else {
        author.clone()
    };

    let (to, cc) = match kind {
        ReplyKind::Sender => (author, Vec::new()),
        ReplyKind::List => {
            let list = original
                .list_post
                .clone()
                .ok_or(ComposeError::NoListAddress)?;
            (vec![Mailbox::new(list)], Vec::new())
        }
        ReplyKind::All if !original.mail_followup_to.is_empty() => {
            (original.mail_followup_to.clone(), Vec::new())
        }
        ReplyKind::All => {
            let to: Vec<Mailbox> = author.into_iter().chain(original.to.clone()).collect();
            (to, original.cc.clone())
        }
    };
    let (mut to, mut cc) = if kind == ReplyKind::All {
        (
            to.into_iter().filter(|m| !is_own(m)).collect(),
            cc.into_iter().filter(|m| !is_own(m)).collect::<Vec<_>>(),
        )
    } else {
        (to, cc)
    };
    let mut seen = HashSet::new();
    to.retain(|m| seen.insert(m.address.trim().to_ascii_lowercase()));
    cc.retain(|m| seen.insert(m.address.trim().to_ascii_lowercase()));
    if to.is_empty() {
        to = std::mem::take(&mut cc);
    }

    let attribution = attribution(&original.from, original.date);
    let quoted = original_text(original).map(|text| quote_text(&text));
    let text = match &quoted {
        Some(quoted) => format!("\n\n{attribution}\n{quoted}"),
        None => String::new(),
    };
    let html = original.html.as_deref().map(|html| {
        format!(
            "<p><br></p>\n<div class=\"holtmail-quote\"><p>{}</p>\n<blockquote type=\"cite\">\n{}\n</blockquote></div>",
            escape(&attribution),
            holtmail_sanitize::outgoing(html)
        )
    });

    Ok(Draft {
        from,
        to,
        cc,
        subject: reply_subject(&original.subject),
        text,
        html,
        in_reply_to: original.message_id.clone(),
        references: thread(original),
        ..Draft::default()
    })
}

/// The `References` of an answer to `original`: its `References` (or its `In-Reply-To` when it
/// has none) plus its id, shortened to [`MAX_REFERENCES`].
pub(crate) fn thread(original: &Original) -> Vec<String> {
    let mut references = if original.references.is_empty() {
        original.in_reply_to.clone()
    } else {
        original.references.clone()
    };
    references.extend(original.message_id.clone());
    shorten_references(&references, MAX_REFERENCES)
}

/// The `References` of an answer to `original` that any message can carry: [`thread`]'s list
/// with ids a message cannot carry dropped, at most [`MAX_REFERENCES`], the original's own id
/// last when it is usable.
pub fn answer_references(original: &Original) -> Vec<String> {
    let mut references = if original.references.is_empty() {
        original.in_reply_to.clone()
    } else {
        original.references.clone()
    };
    references.extend(original.message_id.clone());
    crate::usable_references(&references)
}

/// A forward draft of `original`, whose archived bytes are `raw`, sent as `from`.
///
/// Recipients are empty for the user to fill in; the subject gets exactly one `Fwd:`.
/// [`ForwardKind::Inline`] quotes the original's `From`, `Date`, `Subject`, `To` and `Cc` above its
/// body in text and, when it has HTML, in HTML, and attaches its attachments and inline parts
/// again. [`ForwardKind::Attachment`] attaches `raw` unchanged as `message/rfc822`, named after the
/// subject; this works for messages long deleted from the server.
pub fn forward(original: &Original, raw: &[u8], kind: ForwardKind, from: Mailbox) -> Draft {
    let subject = forward_subject(&original.subject);
    match kind {
        ForwardKind::Attachment => Draft {
            from,
            subject,
            attachments: vec![Attachment {
                name: format!(
                    "{}.eml",
                    file_stem(crate::strip_prefixes(&original.subject))
                ),
                content_type: "message/rfc822".to_owned(),
                bytes: raw.to_vec(),
                content_id: None,
            }],
            ..Draft::default()
        },
        ForwardKind::Inline => {
            let mut fields = vec![("From", mailboxes(&original.from))];
            if let Some(date) = original.date.and_then(crate::quote::date) {
                fields.push(("Date", date));
            }
            fields.push(("Subject", original.subject.clone()));
            if !original.to.is_empty() {
                fields.push(("To", mailboxes(&original.to)));
            }
            if !original.cc.is_empty() {
                fields.push(("Cc", mailboxes(&original.cc)));
            }
            let mut text = String::from("\n\n---------- Forwarded message ----------\n");
            for (name, value) in &fields {
                text.push_str(&format!("{name}: {value}\n"));
            }
            text.push('\n');
            text.push_str(original_text(original).as_deref().unwrap_or_default());
            let html = original.html.as_deref().map(|html| {
                let mut out = String::from(
                    "<p><br></p>\n<div class=\"holtmail-forward\"><p>---------- Forwarded message ----------<br>\n",
                );
                for (name, value) in &fields {
                    out.push_str(&format!("{name}: {}<br>\n", escape(value)));
                }
                out.push_str("</p>\n");
                out.push_str(&holtmail_sanitize::outgoing(html));
                out.push_str("\n</div>");
                out
            });
            let attachments = holtmail_mime::parse(raw)
                .map(|message| {
                    message
                        .attachments
                        .into_iter()
                        .map(|part| Attachment {
                            name: part.filename.unwrap_or_default(),
                            content_type: part.content_type,
                            bytes: part.content.to_vec(),
                            content_id: part.content_id.filter(|_| part.inline),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Draft {
                from,
                subject,
                text,
                html,
                attachments,
                ..Draft::default()
            }
        }
    }
}

fn original_text(original: &Original) -> Option<String> {
    original
        .text
        .clone()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| {
            original
                .html
                .as_deref()
                .map(mail_parser::decoders::html::html_to_text)
        })
}

fn file_stem(subject: &str) -> String {
    let stem: String = subject
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(80)
        .collect();
    match stem.trim() {
        "" => "message".to_owned(),
        stem => stem.to_owned(),
    }
}

fn parse_mailboxes(value: &str) -> Vec<Mailbox> {
    let header = format!("To: {value}\r\n\r\n");
    holtmail_mime::parse(header.as_bytes())
        .map(|message| message.to.iter().map(Mailbox::from).collect())
        .unwrap_or_default()
}

fn list_post(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let start = lower.find("mailto:")? + "mailto:".len();
    let rest = &value[start..];
    let end = rest.find(['>', '?', ',', ' ']).unwrap_or(rest.len());
    let address = rest[..end].trim();
    (!address.is_empty() && address.contains('@')).then(|| address.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_list_post() {
        assert_eq!(
            list_post("<mailto:dev@lists.example.org>").as_deref(),
            Some("dev@lists.example.org")
        );
        assert_eq!(
            list_post("<mailto:dev@lists.example.org?subject=hi>, <https://example.org>")
                .as_deref(),
            Some("dev@lists.example.org")
        );
        assert_eq!(list_post("NO"), None);
        assert_eq!(list_post("<https://example.org/post>"), None);
    }

    #[test]
    fn reads_followup_mailboxes() {
        assert_eq!(
            parse_mailboxes("\"Ada L.\" <ada@example.com>, bob@example.org"),
            vec![
                Mailbox::named("Ada L.", "ada@example.com"),
                Mailbox::new("bob@example.org")
            ]
        );
    }
}
