//! `FETCH` data from the mail view's envelopes, MIME structures and raw bytes.

use std::borrow::Cow;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use holtmail_core::{Address as MailAddress, Timestamp};
use holtmail_mailview::{ByteRange, Envelope, Part};
use imap_types::IntoStatic;
use imap_types::body::{
    BasicFields, Body, BodyStructure, Disposition, Language, Location, MultiPartExtensionData,
    SinglePartExtensionData, SpecificFields,
};
use imap_types::core::{AString, IString, NString, Vec1};
use imap_types::datetime::DateTime;
use imap_types::envelope::{Address, Envelope as ImapEnvelope};
use imap_types::fetch::{MacroOrMessageDataItemNames, MessageDataItemName, Section};

/// What a `FETCH` asks for, beyond the data that comes from the store alone.
#[derive(Debug, Default, Clone)]
pub(crate) struct Wanted {
    /// The items in the order asked.
    pub(crate) items: Vec<MessageDataItemName<'static>>,
    pub(crate) flags: bool,
    pub(crate) envelope: bool,
    pub(crate) structure: bool,
    pub(crate) raw: bool,
    /// Items that set `\Seen` when fetched without `.PEEK`.
    pub(crate) sets_seen: bool,
    pub(crate) modseq: bool,
    /// `BINARY` items, which are not offered.
    pub(crate) binary: bool,
}

impl Wanted {
    pub(crate) fn new(names: MacroOrMessageDataItemNames<'static>) -> Self {
        use MessageDataItemName as N;
        let items = match names {
            MacroOrMessageDataItemNames::Macro(m) => m
                .expand()
                .into_iter()
                .map(IntoStatic::into_static)
                .collect(),
            MacroOrMessageDataItemNames::MessageDataItemNames(items) => items,
        };
        let mut wanted = Self::default();
        for item in &items {
            match item {
                N::Flags => wanted.flags = true,
                N::Envelope | N::InternalDate | N::Rfc822Size => wanted.envelope = true,
                N::Body | N::BodyStructure => wanted.structure = true,
                N::BodyExt { section, peek, .. } => {
                    wanted.raw = true;
                    if section.is_some() {
                        wanted.structure = true;
                    }
                    if !peek {
                        wanted.sets_seen = true;
                    }
                }
                N::Rfc822 => {
                    wanted.raw = true;
                    wanted.sets_seen = true;
                }
                N::Rfc822Text => {
                    wanted.raw = true;
                    wanted.structure = true;
                    wanted.sets_seen = true;
                }
                N::Rfc822Header => {
                    wanted.raw = true;
                    wanted.structure = true;
                }
                N::Binary { .. } | N::BinarySize { .. } => wanted.binary = true,
                N::ModSeq => wanted.modseq = true,
                N::Uid => {}
            }
        }
        if wanted.structure || wanted.raw {
            wanted.envelope = true;
        }
        wanted.items = items;
        wanted
    }

    /// Adds an item unless it is there.
    pub(crate) fn ensure(&mut self, item: MessageDataItemName<'static>) {
        if !self.items.contains(&item) {
            self.items.insert(0, item);
        }
    }
}

/// The bytes with every bare LF made CRLF, as IMAP serves them.
pub(crate) fn crlf(bytes: &[u8]) -> Cow<'_, [u8]> {
    let bare = bytes
        .iter()
        .enumerate()
        .any(|(i, b)| *b == b'\n' && (i == 0 || bytes[i - 1] != b'\r'));
    if !bare {
        return Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 32);
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(*b);
    }
    Cow::Owned(out)
}

/// Bytes as an IMAP string. A literal cannot carry NUL, so NUL bytes are sent as 0x80, as
/// other servers do.
pub(crate) fn nstring(bytes: Vec<u8>) -> NString<'static> {
    let bytes = if bytes.contains(&0) {
        bytes
            .into_iter()
            .map(|b| if b == 0 { 0x80 } else { b })
            .collect()
    } else {
        bytes
    };
    NString::try_from(bytes).unwrap_or(NString::NIL)
}

fn istring(text: &str) -> IString<'static> {
    IString::try_from(text.to_string())
        .unwrap_or_else(|_| IString::try_from(text.replace('\0', "")).expect("text without NUL"))
}

fn text_nstring(text: Option<&str>) -> NString<'static> {
    match text {
        Some(text) => NString(Some(istring(text))),
        None => NString::NIL,
    }
}

/// Header text as it would appear on the wire: ASCII as it is, anything else as one RFC 2047
/// encoded word.
fn header_text(text: &str) -> String {
    if text.is_ascii() && !text.contains("=?") {
        text.to_string()
    } else {
        format!("=?utf-8?b?{}?=", STANDARD.encode(text.as_bytes()))
    }
}

/// `INTERNALDATE` for a time.
pub(crate) fn date_time(at: Timestamp) -> DateTime {
    let utc = chrono::DateTime::from_timestamp(at.as_secs(), 0).unwrap_or_default();
    DateTime::unvalidated(utc.fixed_offset())
}

fn rfc2822(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .to_rfc2822()
}

fn addresses(list: &[MailAddress]) -> Vec<Address<'static>> {
    list.iter()
        .map(|address| {
            let (mailbox, host) = match address.email.rsplit_once('@') {
                Some((local, domain)) => (local.to_string(), Some(domain.to_string())),
                None => (address.email.clone(), None),
            };
            Address {
                name: text_nstring(address.name.as_deref().map(header_text).as_deref()),
                adl: NString::NIL,
                mailbox: text_nstring(Some(&mailbox)),
                host: text_nstring(host.as_deref()),
            }
        })
        .collect()
}

fn angle(id: Option<&str>) -> NString<'static> {
    text_nstring(id.map(|id| format!("<{id}>")).as_deref())
}

/// The fields of an envelope, wherever they come from.
pub(crate) struct Fields<'a> {
    pub(crate) date: Option<i64>,
    pub(crate) subject: Option<&'a str>,
    pub(crate) from: &'a [MailAddress],
    pub(crate) sender: &'a [MailAddress],
    pub(crate) reply_to: &'a [MailAddress],
    pub(crate) to: &'a [MailAddress],
    pub(crate) cc: &'a [MailAddress],
    pub(crate) bcc: &'a [MailAddress],
    pub(crate) in_reply_to: Option<&'a str>,
    pub(crate) message_id: Option<&'a str>,
}

impl<'a> Fields<'a> {
    pub(crate) fn of(envelope: &'a Envelope) -> Self {
        Self {
            date: envelope.sent_at.map(|at| at.as_secs()),
            subject: envelope.subject.as_deref(),
            from: &envelope.from,
            sender: &envelope.sender,
            reply_to: &envelope.reply_to,
            to: &envelope.to,
            cc: &envelope.cc,
            bcc: &envelope.bcc,
            in_reply_to: envelope.in_reply_to.as_deref(),
            message_id: envelope.rfc_message_id.as_deref(),
        }
    }

    /// `ENVELOPE`. The date is the `Date` header's time written in UTC, since the store keeps
    /// the time and not the header's text. `Sender` and `Reply-To` default to `From`.
    pub(crate) fn envelope(&self) -> ImapEnvelope<'static> {
        let or_from = |list: &'a [MailAddress]| if list.is_empty() { self.from } else { list };
        ImapEnvelope {
            date: text_nstring(self.date.map(rfc2822).as_deref()),
            subject: text_nstring(self.subject.map(header_text).as_deref()),
            from: addresses(self.from),
            sender: addresses(or_from(self.sender)),
            reply_to: addresses(or_from(self.reply_to)),
            to: addresses(self.to),
            cc: addresses(self.cc),
            bcc: addresses(self.bcc),
            in_reply_to: angle(self.in_reply_to),
            message_id: angle(self.message_id),
        }
    }
}

fn slice(raw: &[u8], range: ByteRange) -> &[u8] {
    let end = (range.end as usize).min(raw.len());
    let start = (range.start as usize).min(end);
    &raw[start..end]
}

/// The `ENVELOPE` of an embedded message, parsed from its header.
fn embedded_envelope(header: &[u8]) -> ImapEnvelope<'static> {
    match holtmail_mime::parse(header) {
        Ok(parsed) => Fields {
            date: parsed.sent_at,
            subject: parsed.subject.as_deref(),
            from: &parsed.from,
            sender: &[],
            reply_to: &parsed.reply_to,
            to: &parsed.to,
            cc: &parsed.cc,
            bcc: &parsed.bcc,
            in_reply_to: parsed.in_reply_to.first().map(String::as_str),
            message_id: parsed.message_id.as_deref(),
        }
        .envelope(),
        Err(_) => Fields {
            date: None,
            subject: None,
            from: &[],
            sender: &[],
            reply_to: &[],
            to: &[],
            cc: &[],
            bcc: &[],
            in_reply_to: None,
            message_id: None,
        }
        .envelope(),
    }
}

fn params(list: &[(String, String)]) -> Vec<(IString<'static>, IString<'static>)> {
    list.iter()
        .map(|(name, value)| (istring(&name.to_ascii_uppercase()), istring(value)))
        .collect()
}

fn split_type(content_type: &str) -> (String, String) {
    let (kind, sub) = content_type
        .split_once('/')
        .unwrap_or((content_type, "octet-stream"));
    (kind.to_ascii_uppercase(), sub.to_ascii_uppercase())
}

fn is_multipart(part: &Part) -> bool {
    part.content_type.starts_with("multipart/") && !part.parts.is_empty()
}

fn is_message(part: &Part) -> bool {
    part.content_type == "message/rfc822" && part.parts.len() == 1
}

fn size(raw: Option<&[u8]>, range: ByteRange) -> u32 {
    let len = match raw {
        Some(raw) => crlf(slice(raw, range)).len() as u64,
        None => range.len(),
    };
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn disposition(part: &Part) -> Disposition<'static> {
    Disposition {
        disposition: part.disposition.as_ref().map(|d| {
            (
                istring(&d.to_ascii_uppercase()),
                params(&part.disposition_params),
            )
        }),
        tail: Some(Language {
            language: part.language.iter().map(|l| istring(l)).collect(),
            tail: Some(Location {
                location: text_nstring(part.location.as_deref()),
                extensions: Vec::new(),
            }),
        }),
    }
}

/// `BODYSTRUCTURE` (with `extended`) or `BODY`. With `raw`, sizes count the CRLF form served
/// and embedded messages get their envelope; without, sizes are the archived ones.
pub(crate) fn structure(part: &Part, raw: Option<&[u8]>, extended: bool) -> BodyStructure<'static> {
    if is_multipart(part) {
        let bodies: Vec<BodyStructure<'static>> = part
            .parts
            .iter()
            .map(|child| structure(child, raw, extended))
            .collect();
        let (_, subtype) = split_type(&part.content_type);
        return BodyStructure::Multi {
            bodies: Vec1::try_from(bodies).expect("a multipart has parts"),
            subtype: istring(&subtype),
            extension_data: extended.then(|| MultiPartExtensionData {
                parameter_list: params(&part.params),
                tail: Some(disposition(part)),
            }),
        };
    }
    let (kind, subtype) = split_type(&part.content_type);
    let lines = u32::try_from(part.lines).unwrap_or(u32::MAX);
    let specific = if is_message(part) {
        let embedded = &part.parts[0];
        let envelope = match raw {
            Some(raw) => embedded_envelope(slice(raw, embedded.header)),
            None => embedded_envelope(b""),
        };
        SpecificFields::Message {
            envelope: Box::new(envelope),
            body_structure: Box::new(structure(embedded, raw, extended)),
            number_of_lines: lines,
        }
    } else if kind == "TEXT" {
        SpecificFields::Text {
            subtype: istring(&subtype),
            number_of_lines: lines,
        }
    } else {
        SpecificFields::Basic {
            r#type: istring(&kind),
            subtype: istring(&subtype),
        }
    };
    BodyStructure::Single {
        body: Body {
            basic: BasicFields {
                parameter_list: params(&part.params),
                id: text_nstring(part.content_id.as_deref()),
                description: text_nstring(part.description.as_deref()),
                content_transfer_encoding: istring(&part.transfer_encoding.to_ascii_uppercase()),
                size: size(raw, part.body),
            },
            specific,
        },
        extension_data: extended.then(|| SinglePartExtensionData {
            md5: NString::NIL,
            tail: Some(disposition(part)),
        }),
    }
}

/// Whether `part` holds an embedded message anywhere, whose envelope needs the raw bytes.
pub(crate) fn has_message(part: &Part) -> bool {
    is_message(part) || part.parts.iter().any(has_message)
}

/// The part a section number names, from the whole message: part 1 of a single-part message is
/// its body, and the parts of an embedded message continue its number.
fn find<'a>(root: &'a Part, path: &[u32]) -> Option<&'a Part> {
    let mut node = root;
    for n in path {
        let index = usize::try_from(*n).ok()?.checked_sub(1)?;
        let container = if is_message(node) {
            &node.parts[0]
        } else {
            node
        };
        node = if is_multipart(container) {
            container.parts.get(index)?
        } else if index == 0 {
            container
        } else {
            return None;
        };
    }
    Some(node)
}

fn header_fields(header: &[u8], names: &[AString<'_>], keep: bool) -> Vec<u8> {
    let wanted: Vec<String> = names
        .iter()
        .map(|n| String::from_utf8_lossy(n.as_ref()).to_ascii_lowercase())
        .collect();
    let mut out = Vec::new();
    let mut include = false;
    for line in header.split_inclusive(|b| *b == b'\n') {
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if !matches!(line.first(), Some(b' ' | b'\t')) {
            let name = line.iter().position(|b| *b == b':').map(|at| {
                String::from_utf8_lossy(&line[..at])
                    .trim()
                    .to_ascii_lowercase()
            });
            include = name.is_some_and(|name| wanted.contains(&name)) == keep;
        }
        if include {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// The bytes of a `BODY[section]`, before CRLF and partial; `None` when the section does not
/// exist in this message, which is answered with an empty string.
pub(crate) fn section(raw: &[u8], root: &Part, section: Option<&Section<'_>>) -> Option<Vec<u8>> {
    let embedded = |part: &Option<imap_types::fetch::Part>| -> Option<&Part> {
        match part {
            None => Some(root),
            Some(part) => {
                let path: Vec<u32> = part.0.as_ref().iter().map(|n| n.get()).collect();
                let node = find(root, &path)?;
                is_message(node).then(|| &node.parts[0])
            }
        }
    };
    let bytes = match section {
        None => raw.to_vec(),
        Some(Section::Part(part)) => {
            let path: Vec<u32> = part.0.as_ref().iter().map(|n| n.get()).collect();
            slice(raw, find(root, &path)?.body).to_vec()
        }
        Some(Section::Mime(part)) => {
            let path: Vec<u32> = part.0.as_ref().iter().map(|n| n.get()).collect();
            slice(raw, find(root, &path)?.header).to_vec()
        }
        Some(Section::Header(part)) => slice(raw, embedded(part)?.header).to_vec(),
        Some(Section::Text(part)) => slice(raw, embedded(part)?.body).to_vec(),
        Some(Section::HeaderFields(part, names)) => {
            header_fields(slice(raw, embedded(part)?.header), names.as_ref(), true)
        }
        Some(Section::HeaderFieldsNot(part, names)) => {
            header_fields(slice(raw, embedded(part)?.header), names.as_ref(), false)
        }
    };
    Some(bytes)
}

/// The served form of a section with a partial range applied.
pub(crate) fn served(bytes: &[u8], partial: Option<(u32, std::num::NonZeroU32)>) -> Vec<u8> {
    let bytes = crlf(bytes);
    match partial {
        None => bytes.into_owned(),
        Some((origin, length)) => {
            let start = (origin as usize).min(bytes.len());
            let end = start.saturating_add(length.get() as usize).min(bytes.len());
            bytes[start..end].to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_only_where_bare() {
        assert_eq!(&*crlf(b"a\r\nb\nc"), b"a\r\nb\r\nc");
        assert!(matches!(crlf(b"a\r\nb\r\n"), Cow::Borrowed(_)));
        assert_eq!(&*crlf(b"\n"), b"\r\n");
    }

    #[test]
    fn header_field_filter_keeps_folded_lines() {
        let header = b"From: a@example.com\r\nSubject: one\r\n two\r\nTo: b@example.org\r\n\r\n";
        let names = [AString::try_from("subject").unwrap()];
        assert_eq!(
            header_fields(header, &names, true),
            b"Subject: one\r\n two\r\n\r\n".to_vec()
        );
        assert_eq!(
            header_fields(header, &names, false),
            b"From: a@example.com\r\nTo: b@example.org\r\n\r\n".to_vec()
        );
    }

    #[test]
    fn encodes_non_ascii_header_text() {
        assert_eq!(header_text("Hello"), "Hello");
        assert_eq!(header_text("Grüße"), "=?utf-8?b?R3LDvMOfZQ==?=");
    }

    #[test]
    fn partial_ranges() {
        let length = std::num::NonZeroU32::new(3).unwrap();
        assert_eq!(served(b"abcdef", Some((2, length))), b"cde".to_vec());
        assert_eq!(served(b"ab", Some((5, length))), b"".to_vec());
    }
}
