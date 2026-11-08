use bytes::Bytes;
use holtmail_core::DeviceId;
use mail_builder::headers::date::Date;

/// A client's message, ready for the queue.
pub(crate) struct Prepared {
    pub(crate) bytes: Bytes,
    /// The `Message-ID` the client wrote, without angle brackets.
    pub(crate) client_message_id: Option<String>,
}

/// One header field: its lines, name and value, as they were written.
struct Field<'a> {
    lines: Vec<&'a [u8]>,
    name: &'a [u8],
}

impl Field<'_> {
    fn is(&self, name: &str) -> bool {
        self.name.trim_ascii().eq_ignore_ascii_case(name.as_bytes())
    }

    fn value(&self) -> String {
        let mut value = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            let line = if index == 0 {
                &line[self.name.len() + 1..]
            } else {
                line
            };
            value.push_str(&String::from_utf8_lossy(line));
        }
        value.trim().to_owned()
    }
}

fn split_lines(raw: &[u8]) -> impl Iterator<Item = &[u8]> {
    raw.split_inclusive(|byte| *byte == b'\n')
}

fn is_blank(line: &[u8]) -> bool {
    line == b"\r\n" || line == b"\n"
}

fn brackets(value: &str) -> Option<String> {
    let open = value.find('<')?;
    let close = open + value[open..].find('>')?;
    let inside = value[open + 1..close].trim();
    (!inside.is_empty()).then(|| inside.to_owned())
}

/// RFC 6409 section 8, and no more: adds `Date` and `Message-ID` when missing, and removes `Bcc`
/// and `Resent-Bcc`. Everything else stays byte for byte. No `Received:` is added.
pub(crate) fn prepare(raw: &[u8], domain: &str) -> Prepared {
    let mut fields: Vec<Field<'_>> = Vec::new();
    let mut header_end = 0;
    let mut ended = false;
    for line in split_lines(raw) {
        if is_blank(line) {
            ended = true;
            break;
        }
        if (line.starts_with(b" ") || line.starts_with(b"\t"))
            && let Some(last) = fields.last_mut()
        {
            last.lines.push(line);
            header_end += line.len();
            continue;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            break;
        };
        fields.push(Field {
            lines: vec![line],
            name: &line[..colon],
        });
        header_end += line.len();
    }
    let client_message_id = fields
        .iter()
        .find(|field| field.is("Message-ID"))
        .and_then(|field| brackets(&field.value()));
    let has_date = fields.iter().any(|f| f.is("Date") && !f.value().is_empty());
    let mut out: Vec<u8> = Vec::with_capacity(raw.len() + 128);
    for field in &fields {
        let dropped = field.is("Bcc")
            || field.is("Resent-Bcc")
            || (field.is("Date") && !has_date)
            || (field.is("Message-ID") && client_message_id.is_none());
        if !dropped {
            for line in &field.lines {
                out.extend_from_slice(line);
            }
        }
    }
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.extend_from_slice(b"\r\n");
    }
    if !has_date {
        let now = holtmail_core::Timestamp::now();
        let date = Date::new(now.as_secs()).to_rfc822();
        out.extend_from_slice(format!("Date: {date}\r\n").as_bytes());
    }
    if client_message_id.is_none() {
        let id = uuid::Uuid::now_v7();
        out.extend_from_slice(format!("Message-ID: <{id}@{domain}>\r\n").as_bytes());
    }
    let rest = &raw[header_end..];
    if !ended {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(rest);
    Prepared {
        bytes: Bytes::from(out),
        client_message_id,
    }
}

/// The key that makes a client's retry after a lost `250` one message: the device, the
/// recipients of this transaction and the `Message-ID`, or the bytes when it has none. Recipients
/// count because some clients send one message in several transactions, for example one for the
/// Bcc recipients, under the same `Message-ID`.
pub(crate) fn idempotency_key(
    device: &DeviceId,
    recipients: &[String],
    message_id: Option<&str>,
    raw: &[u8],
) -> String {
    let mut sorted: Vec<String> = recipients.iter().map(|r| r.to_ascii_lowercase()).collect();
    sorted.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(device.to_string().as_bytes());
    for recipient in &sorted {
        hasher.update(&[0]);
        hasher.update(recipient.as_bytes());
    }
    hasher.update(&[1]);
    match message_id {
        Some(id) => hasher.update(id.as_bytes()),
        None => hasher.update(raw),
    };
    format!("smtp:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(prepared: &Prepared) -> String {
        String::from_utf8(prepared.bytes.to_vec()).unwrap()
    }

    #[test]
    fn leaves_a_complete_message_alone() {
        let raw = b"From: Ada <ada@example.com>\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
                    Message-ID: <m@example.com>\r\nX-Folded: a\r\n b\r\n\r\nBody\r\n";
        let prepared = prepare(raw, "example.com");
        assert_eq!(prepared.bytes.as_ref(), raw);
        assert_eq!(prepared.client_message_id.as_deref(), Some("m@example.com"));
    }

    #[test]
    fn removes_bcc_with_its_folded_lines() {
        let raw = b"From: a@example.com\r\nBcc: x@example.org,\r\n  y@example.org\r\nresent-BCC: z@example.org\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\nMessage-ID: <m@example.com>\r\n\r\nBcc: stays in the body\r\n";
        let out = text(&prepare(raw, "example.com"));
        assert_eq!(
            out,
            "From: a@example.com\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\nMessage-ID: <m@example.com>\r\n\r\nBcc: stays in the body\r\n"
        );
    }

    #[test]
    fn adds_a_missing_date_and_message_id() {
        let prepared = prepare(b"From: a@example.com\r\n\r\nBody\r\n", "example.com");
        let out = text(&prepared);
        assert!(prepared.client_message_id.is_none());
        let date = out.find("Date: ").unwrap();
        let id = out.find("Message-ID: <").unwrap();
        let body = out.find("\r\n\r\nBody").unwrap();
        assert!(date < body && id < body, "{out}");
        assert!(out.contains("@example.com>\r\n"), "{out}");
    }

    #[test]
    fn replaces_blank_date_and_message_id() {
        let out = text(&prepare(
            b"From: a@example.com\r\nDate:\r\nMessage-ID: \r\n\r\nBody\r\n",
            "example.org",
        ));
        assert!(!out.contains("Date:\r\n"), "{out}");
        assert!(out.contains("@example.org>"), "{out}");
    }

    #[test]
    fn a_message_without_a_body_still_ends_its_header() {
        let out = text(&prepare(b"From: a@example.com\r\n", "example.org"));
        assert!(out.ends_with("\r\n\r\n"), "{out:?}");
    }

    #[test]
    fn keys_differ_by_device_recipients_and_id() {
        let device = DeviceId::new();
        let to = |a: &str| vec![a.to_owned()];
        let key = idempotency_key(&device, &to("a@example.org"), Some("m"), b"x");
        assert_eq!(
            key,
            idempotency_key(&device, &to("A@Example.org"), Some("m"), b"y")
        );
        assert_ne!(
            key,
            idempotency_key(&DeviceId::new(), &to("a@example.org"), Some("m"), b"x")
        );
        assert_ne!(
            key,
            idempotency_key(&device, &to("b@example.org"), Some("m"), b"x")
        );
        assert_ne!(
            key,
            idempotency_key(&device, &to("a@example.org"), Some("n"), b"x")
        );
    }
}
