use std::collections::HashSet;

use holtmail_core::Timestamp;
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;
use mail_builder::headers::text::Text;
use mail_builder::mime::{BodyPart, MimePart};

use crate::address::{ascii_domain, no_breaks, normalize};
use crate::{ALLOWED_HEADERS, Attachment, Built, ComposeError, Draft, ITIP_METHODS, Mailbox};

const AUTO_SUBMITTED: &[&str] = &["no", "auto-generated", "auto-replied", "auto-notified"];
const MAX_LINE: usize = 998;

/// Builds the bytes of `draft`.
///
/// Guarantees:
/// - every address is validated and its domain written in ASCII; a CR, LF or NUL in any header
///   value, name, address or attachment name is refused, so no header can be injected;
/// - non-ASCII display names and the subject are encoded (RFC 2047), non-ASCII file names too
///   (RFC 2231);
/// - `bcc` never appears in the bytes; when there is no `To` or `Cc`, `To` is the empty group
///   `undisclosed-recipients:;`;
/// - the HTML body is sanitized with the outgoing profile of `holtmail-sanitize`, and sent with a
///   plain text alternative (the draft's text, or the HTML converted to text when that is empty);
/// - a calendar part is the last alternative, `text/calendar; method=...; charset=utf-8` with
///   CRLF line endings (RFC 6047), after the text and the HTML;
/// - parts with a content id are sent as `multipart/related` with the HTML; `message/rfc822`
///   attachments keep their bytes (line endings made CRLF) with `7bit` or `8bit` encoding, or
///   base64 when a line is longer than 998 bytes;
/// - the bytes use CRLF line endings and end with CRLF, as submission needs;
/// - the `Message-ID` is new on every call: `<uuid v7@domain>`.
pub fn build(draft: &Draft) -> Result<Built, ComposeError> {
    let from = mailbox(&draft.from, "from")?;
    let to = mailboxes(&draft.to, "to")?;
    let cc = mailboxes(&draft.cc, "cc")?;
    let bcc = mailboxes(&draft.bcc, "bcc")?;
    let reply_to = mailboxes(&draft.reply_to, "reply_to")?;

    let mut seen = HashSet::new();
    let recipients: Vec<String> = to
        .iter()
        .chain(&cc)
        .chain(&bcc)
        .map(|(_, address)| address.clone())
        .filter(|address| seen.insert(address.to_ascii_lowercase()))
        .collect();
    if recipients.is_empty() {
        return Err(ComposeError::NoRecipients);
    }

    no_breaks(&draft.subject, "subject")?;
    let domain = match &draft.message_id_domain {
        Some(domain) => {
            no_breaks(domain, "message_id_domain")?;
            ascii_domain(domain).ok_or(ComposeError::Address("message_id_domain"))?
        }
        None => from
            .1
            .rsplit_once('@')
            .map(|(_, d)| d.to_owned())
            .unwrap_or_default(),
    };
    let message_id = format!("{}@{domain}", uuid::Uuid::now_v7());
    let in_reply_to = draft
        .in_reply_to
        .as_deref()
        .map(|id| message_id_value(id, "in_reply_to"))
        .transpose()?;
    let references = draft
        .references
        .iter()
        .map(|id| message_id_value(id, "references"))
        .collect::<Result<Vec<_>, _>>()?;
    let extra = draft
        .headers
        .iter()
        .map(|header| extra_header(&header.name, &header.value))
        .collect::<Result<Vec<_>, _>>()?;

    let mut message = MessageBuilder::new().from(address(&from));
    if !reply_to.is_empty() {
        message = message.reply_to(list(&reply_to));
    }
    if !to.is_empty() {
        message = message.to(list(&to));
    } else if cc.is_empty() {
        message = message.header("To", Raw::new("undisclosed-recipients:;"));
    }
    if !cc.is_empty() {
        message = message.cc(list(&cc));
    }
    if !draft.subject.trim().is_empty() {
        message = message.subject(Text::new(draft.subject.trim().to_owned()));
    }
    let date = draft.date.unwrap_or_else(Timestamp::now);
    message = message
        .date(Date::new(date.as_secs()))
        .message_id(MessageId::new(message_id.clone()));
    if let Some(id) = in_reply_to {
        message = message.in_reply_to(MessageId::new(id));
    }
    if !references.is_empty() {
        message = message.references(MessageId::from(references));
    }
    for (name, value) in extra {
        message = message.header(name, Text::new(value));
    }

    let body = body(draft)?;
    let mut bytes = message.body(body).write_to_vec().unwrap_or_default();
    if !bytes.ends_with(b"\r\n") {
        bytes.extend_from_slice(b"\r\n");
    }
    Ok(Built {
        bytes,
        message_id,
        recipients,
    })
}

fn mailbox(
    mailbox: &Mailbox,
    field: &'static str,
) -> Result<(Option<String>, String), ComposeError> {
    let address = normalize(mailbox.address.trim(), field)?;
    let name = match mailbox.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => {
            no_breaks(name, field)?;
            Some(name.to_owned())
        }
        _ => None,
    };
    Ok((name, address))
}

fn mailboxes(
    list: &[Mailbox],
    field: &'static str,
) -> Result<Vec<(Option<String>, String)>, ComposeError> {
    list.iter().map(|m| mailbox(m, field)).collect()
}

fn address((name, email): &(Option<String>, String)) -> Address<'static> {
    Address::new_address(name.clone(), email.clone())
}

fn list(list: &[(Option<String>, String)]) -> Address<'static> {
    Address::new_list(list.iter().map(address).collect())
}

fn message_id_value(id: &str, field: &'static str) -> Result<String, ComposeError> {
    no_breaks(id, field)?;
    let id = id.trim();
    let id = id
        .strip_prefix('<')
        .and_then(|id| id.strip_suffix('>'))
        .unwrap_or(id);
    let valid = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>');
    if valid {
        Ok(id.to_owned())
    } else {
        Err(ComposeError::MessageId(field))
    }
}

fn extra_header(name: &str, value: &str) -> Result<(&'static str, String), ComposeError> {
    let refuse = || ComposeError::Header(name.chars().filter(|c| c.is_ascii_graphic()).collect());
    let canonical = ALLOWED_HEADERS
        .iter()
        .find(|allowed| allowed.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(refuse)?;
    no_breaks(value, "headers")?;
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
        return Err(refuse());
    }
    if *canonical == "Auto-Submitted" {
        let keyword = value.split(';').next().unwrap_or_default().trim();
        if !AUTO_SUBMITTED
            .iter()
            .any(|k| k.eq_ignore_ascii_case(keyword))
        {
            return Err(refuse());
        }
    }
    Ok((canonical, value.to_owned()))
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

fn crlf_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 32);
    let mut previous = 0u8;
    for &b in bytes {
        if b == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        previous = b;
    }
    out
}

fn body(draft: &Draft) -> Result<MimePart<'static>, ComposeError> {
    let html = draft.html.as_deref().map(holtmail_sanitize::outgoing);
    let text = match &html {
        Some(html) if draft.text.trim().is_empty() => {
            mail_parser::decoders::html::html_to_text(html)
        }
        _ => draft.text.clone(),
    };
    let text = MimePart::new("text/plain", BodyPart::Text(crlf(&text).into()));
    let mut alternatives = vec![text];
    if let Some(html) = &html {
        let page = format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"></head><body>\n{html}\n</body></html>\n"
        );
        alternatives.push(MimePart::new(
            "text/html",
            BodyPart::Text(crlf(&page).into()),
        ));
    }
    if let Some(calendar) = &draft.calendar {
        let method = calendar.method.trim().to_ascii_uppercase();
        if !ITIP_METHODS.contains(&method.as_str()) {
            return Err(ComposeError::Calendar);
        }
        let ics = std::str::from_utf8(&calendar.bytes).map_err(|_| ComposeError::Calendar)?;
        let kind = ContentType::new("text/calendar")
            .attribute("method", method)
            .attribute("charset", "utf-8");
        alternatives.push(MimePart::new(kind, BodyPart::Text(crlf(ics).into())));
    }
    let mut body = if alternatives.len() == 1 {
        alternatives.remove(0)
    } else {
        MimePart::new("multipart/alternative", alternatives)
    };

    let mut inline = Vec::new();
    let mut files = Vec::new();
    for (index, attachment) in draft.attachments.iter().enumerate() {
        let part = attachment_part(attachment, index)?;
        if attachment.content_id.is_some() && html.is_some() {
            inline.push(part);
        } else {
            files.push(part);
        }
    }
    if !inline.is_empty() {
        inline.insert(0, body);
        body = MimePart::new("multipart/related", inline);
    }
    if !files.is_empty() {
        files.insert(0, body);
        body = MimePart::new("multipart/mixed", files);
    }
    Ok(body)
}

fn attachment_part(
    attachment: &Attachment,
    index: usize,
) -> Result<MimePart<'static>, ComposeError> {
    let invalid = || ComposeError::Attachment(index);
    let content_type = attachment.content_type.trim().to_ascii_lowercase();
    if !valid_media_type(&content_type) {
        return Err(invalid());
    }
    if attachment.name.contains(['\r', '\n', '\0']) {
        return Err(ComposeError::Injection("attachments"));
    }
    let name = match attachment.name.trim() {
        "" => "attachment".to_owned(),
        name => name.to_owned(),
    };
    if content_type == "message/rfc822" {
        let bytes = crlf_bytes(&attachment.bytes);
        let fits = bytes
            .split(|&b| b == b'\n')
            .all(|line| line.len() <= MAX_LINE + 1)
            && !bytes.contains(&0);
        let part =
            MimePart::new(content_type, BodyPart::Binary(bytes.clone().into())).attachment(name);
        if !fits {
            return Ok(part);
        }
        let encoding = if bytes.is_ascii() { "7bit" } else { "8bit" };
        return Ok(part.header("Content-Transfer-Encoding", Raw::new(encoding)));
    }
    let part = MimePart::new(
        content_type,
        BodyPart::Binary(attachment.bytes.clone().into()),
    );
    match attachment.content_id.as_deref() {
        Some(cid) => {
            let cid = message_id_value(cid, "attachments").map_err(|_| invalid())?;
            Ok(part.inline().cid(cid))
        }
        None => Ok(part.attachment(name)),
    }
}

fn valid_media_type(value: &str) -> bool {
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    value
        .split_once('/')
        .is_some_and(|(kind, sub)| token(kind) && token(sub))
}
