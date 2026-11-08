use mail_parser::{MessageParser, MessagePart, MessageRef};

/// A delivery status notification (RFC 3464, RFC 6533) read from a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dsn {
    /// `Reporting-MTA` without its type, e.g. `mx.example.org`; `None` when missing.
    pub reporting_mta: Option<String>,
    /// `Original-Envelope-Id` as written, angle brackets removed; `None` when missing.
    pub original_envelope_id: Option<String>,
    /// `Message-ID` of the message the report is about, without angle brackets: taken from the
    /// first attached original (`message/rfc822`, `message/global`, `text/rfc822-headers` or
    /// `message/global-headers`) that has one, otherwise [`Dsn::original_envelope_id`].
    pub original_message_id: Option<String>,
    /// One entry per recipient the report covers, in report order.
    pub recipients: Vec<DsnRecipient>,
}

/// What a report says about one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnRecipient {
    /// `Final-Recipient` without its address type or angle brackets; `utf-8` addresses decoded.
    /// `None` when the report does not name the recipient.
    pub final_recipient: Option<String>,
    /// `Original-Recipient`, read the same way as [`DsnRecipient::final_recipient`].
    pub original_recipient: Option<String>,
    /// `Action`; `None` when missing or not one of the RFC 3464 values.
    pub action: Option<DsnAction>,
    /// `Status` as `class.subject.detail`, e.g. `5.1.1`; `None` when missing or malformed.
    pub status: Option<String>,
    /// `Diagnostic-Code` without its type, unfolded to one line, e.g. `550 5.1.1 User unknown`.
    pub diagnostic_code: Option<String>,
}

/// The `Action` of one recipient in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsnAction {
    /// Delivery failed and will not be retried.
    Failed,
    /// Delivery has not succeeded yet and is still being retried.
    Delayed,
    /// The message reached the recipient's mailbox.
    Delivered,
    /// The message went on to a system that does not send reports.
    Relayed,
    /// The message reached a list or alias and was sent on to its members.
    Expanded,
}

/// Reads a delivery status notification: a message with a `message/delivery-status` or
/// `message/global-delivery-status` part of its own (parts of attached messages do not count),
/// normally inside `multipart/report`. Several such parts are read in order as one report.
///
/// `None` when the bytes are not a message or carry no such part. Malformed reports give what
/// can be read: unknown fields are skipped, recipients without any recipient field are dropped,
/// broken values become `None`. Never panics.
pub fn parse_dsn(raw: &[u8]) -> Option<Dsn> {
    let message = MessageParser::new().parse(raw)?;
    let root = message.root();
    let bodies: Vec<String> = root
        .parts()
        .filter(|part| is_status_part(part))
        .map(|part| String::from_utf8_lossy(&part.decoded()).into_owned())
        .collect();
    if bodies.is_empty() {
        return None;
    }

    let mut dsn = Dsn {
        reporting_mta: None,
        original_envelope_id: None,
        original_message_id: None,
        recipients: Vec::new(),
    };
    for group in bodies.iter().flat_map(|body| groups(body)) {
        let mut recipient = DsnRecipient {
            final_recipient: None,
            original_recipient: None,
            action: None,
            status: None,
            diagnostic_code: None,
        };
        let mut is_recipient = false;
        for (name, value) in group {
            match name.to_ascii_lowercase().as_str() {
                "reporting-mta" if dsn.reporting_mta.is_none() => {
                    dsn.reporting_mta = non_empty(strip_type(&value).to_owned());
                }
                "original-envelope-id" if dsn.original_envelope_id.is_none() => {
                    dsn.original_envelope_id = non_empty(unbracket(&value).to_owned());
                }
                "final-recipient" => {
                    is_recipient = true;
                    recipient.final_recipient = address(&value);
                }
                "original-recipient" => {
                    is_recipient = true;
                    recipient.original_recipient = address(&value);
                }
                "action" => {
                    is_recipient = true;
                    recipient.action = action(&value);
                }
                "status" => {
                    is_recipient = true;
                    recipient.status = status_code(&value);
                }
                "diagnostic-code" => {
                    is_recipient = true;
                    recipient.diagnostic_code = non_empty(strip_type(&value).to_owned());
                }
                _ => {}
            }
        }
        if is_recipient {
            dsn.recipients.push(recipient);
        }
    }
    dsn.original_message_id =
        original_message_id(root).or_else(|| dsn.original_envelope_id.clone());
    Some(dsn)
}

/// Recognises a bounce that is not a delivery status notification: a message from a
/// `mailer-daemon` or `postmaster` address with an attached original message that has a
/// `Message-ID`.
///
/// It returns a [`Dsn`] with that `Message-ID` and one recipient whose only known field is
/// [`DsnAction::Failed`]: such bounces do not say reliably who failed or why, so the recipient,
/// status and diagnostic stay `None`. `None` for anything else, including real reports, which
/// [`parse_dsn`] reads; callers try [`parse_dsn`] first. Never panics.
pub fn parse_unstructured_bounce(raw: &[u8]) -> Option<Dsn> {
    let message = MessageParser::new().parse(raw)?;
    let root = message.root();
    if root.parts().any(|part| is_status_part(&part)) {
        return None;
    }
    let from_daemon = root.from().is_some_and(|list| {
        list.mailboxes().any(|mailbox| {
            mailbox.address().is_some_and(|email| {
                let local = email.rsplit_once('@').map_or(email, |(local, _)| local);
                local.eq_ignore_ascii_case("mailer-daemon")
                    || local.eq_ignore_ascii_case("postmaster")
            })
        })
    });
    if !from_daemon {
        return None;
    }
    let original_message_id = Some(original_message_id(root)?);
    Some(Dsn {
        reporting_mta: None,
        original_envelope_id: None,
        original_message_id,
        recipients: vec![DsnRecipient {
            final_recipient: None,
            original_recipient: None,
            action: Some(DsnAction::Failed),
            status: None,
            diagnostic_code: None,
        }],
    })
}

fn is_status_part(part: &MessagePart<'_>) -> bool {
    part.is_content_type("message", "delivery-status")
        || part.is_content_type("message", "global-delivery-status")
}

fn original_message_id(root: MessageRef<'_>) -> Option<String> {
    root.parts().find_map(|part| {
        let whole =
            part.is_content_type("message", "rfc822") || part.is_content_type("message", "global");
        let headers = part.is_content_type("text", "rfc822-headers")
            || part.is_content_type("message", "global-headers");
        if !whole && !headers {
            return None;
        }
        if let Some(nested) = part.nested() {
            return nested.message_id().and_then(clean_id);
        }
        let decoded = part.decoded();
        let inner = MessageParser::new().parse(&decoded)?;
        inner.root().message_id().and_then(clean_id)
    })
}

fn clean_id(id: &str) -> Option<String> {
    non_empty(unbracket(id).to_owned())
}

fn groups(body: &str) -> Vec<Vec<(String, String)>> {
    let mut groups = Vec::new();
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut continuable = false;
    for line in body.lines() {
        if line.trim().is_empty() {
            if !fields.is_empty() {
                groups.push(std::mem::take(&mut fields));
            }
            continuable = false;
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if continuable && let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        match line.split_once(':') {
            Some((name, value)) if !name.trim().is_empty() && !name.contains(' ') => {
                fields.push((name.trim().to_owned(), value.trim().to_owned()));
                continuable = true;
            }
            _ => continuable = false,
        }
    }
    if !fields.is_empty() {
        groups.push(fields);
    }
    for group in &mut groups {
        for (_, value) in group.iter_mut() {
            *value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    groups
}

fn strip_type(value: &str) -> &str {
    value.split_once(';').map_or(value, |(_, rest)| rest).trim()
}

fn unbracket(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(value)
        .trim()
}

fn address(value: &str) -> Option<String> {
    let (kind, rest) = value.split_once(';').unwrap_or(("rfc822", value));
    let rest = unbracket(rest);
    let rest = if kind.trim().eq_ignore_ascii_case("utf-8") {
        decode_utf8_xtext(rest)
    } else {
        rest.to_owned()
    };
    non_empty(rest)
}

fn decode_utf8_xtext(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("\\x{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let decoded = after.split_once('}').and_then(|(hex, tail)| {
            let code = u32::from_str_radix(hex, 16).ok()?;
            Some((char::from_u32(code)?, tail))
        });
        match decoded {
            Some((ch, tail)) => {
                out.push(ch);
                rest = tail;
            }
            None => {
                out.push_str("\\x{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn action(value: &str) -> Option<DsnAction> {
    let word = value
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()?
        .to_ascii_lowercase();
    Some(match word.as_str() {
        "failed" => DsnAction::Failed,
        "delayed" => DsnAction::Delayed,
        "delivered" => DsnAction::Delivered,
        "relayed" => DsnAction::Relayed,
        "expanded" => DsnAction::Expanded,
        _ => return None,
    })
}

fn status_code(value: &str) -> Option<String> {
    let code = value
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()?;
    let mut parts = code.split('.');
    let class = parts.next()?;
    let subject = parts.next()?;
    let detail = parts.next()?;
    let digits = |part: &str, max: usize| {
        !part.is_empty() && part.len() <= max && part.bytes().all(|b| b.is_ascii_digit())
    };
    (parts.next().is_none()
        && matches!(class, "2" | "4" | "5")
        && digits(subject, 3)
        && digits(detail, 3))
    .then(|| code.to_owned())
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_status_codes() {
        assert_eq!(status_code("5.1.1"), Some("5.1.1".to_owned()));
        assert_eq!(
            status_code("4.4.7 (delivery time expired)"),
            Some("4.4.7".to_owned())
        );
        assert_eq!(status_code("5.1"), None);
        assert_eq!(status_code("3.1.1"), None);
        assert_eq!(status_code("5.1.1.1"), None);
        assert_eq!(status_code("5.x.1"), None);
        assert_eq!(status_code(""), None);
    }

    #[test]
    fn reads_actions_case_insensitively() {
        assert_eq!(action("FAILED"), Some(DsnAction::Failed));
        assert_eq!(action("delayed (retrying)"), Some(DsnAction::Delayed));
        assert_eq!(action("bounced"), None);
        assert_eq!(action(""), None);
    }

    #[test]
    fn reads_addresses() {
        assert_eq!(
            address("rfc822; <bob@example.org>"),
            Some("bob@example.org".to_owned())
        );
        assert_eq!(
            address("bob@example.org"),
            Some("bob@example.org".to_owned())
        );
        assert_eq!(
            address("utf-8; j\\x{00F6}rg@example.org"),
            Some("jörg@example.org".to_owned())
        );
        assert_eq!(
            address("utf-8; a\\x{zz}b\\x{"),
            Some("a\\x{zz}b\\x{".to_owned())
        );
        assert_eq!(address("rfc822;"), None);
    }

    #[test]
    fn unfolds_and_groups_fields() {
        let body = "\r\n\r\nReporting-MTA: dns; mx.example.org\r\n\r\nFinal-Recipient: rfc822; a@example.org\r\nDiagnostic-Code: smtp; 550 one\r\n  two\r\n\r\n\r\n";
        let groups = groups(body);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1][1].1, "smtp; 550 one two");
    }
}
