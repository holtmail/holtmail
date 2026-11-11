use holtmail_schedule::{ItipKey, ItipMethod};

/// The key of the first `text/calendar` part that carries an iTIP `REQUEST`, `REPLY` or
/// `CANCEL`, from its first event: `UID`, `SEQUENCE`, method, `RECURRENCE-ID` and, for a
/// `REPLY`, the attendee's `PARTSTAT`. `None` for any other message.
pub(crate) fn key(message: &[u8]) -> Option<ItipKey> {
    let parsed = holtmail_mime::parse(message).ok()?;
    parsed.calendars.iter().find_map(|part| of_calendar(part))
}

fn of_calendar(part: &[u8]) -> Option<ItipKey> {
    let text = String::from_utf8_lossy(part);
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(more), Some(last)) => last.push_str(more),
            _ => lines.push(line.to_owned()),
        }
    }
    let value = |line: &str, name: &str| -> Option<String> {
        let (head, value) = line.split_once(':')?;
        let field = head.split(';').next()?;
        field
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    };
    let method = lines
        .iter()
        .find_map(|l| value(l, "METHOD"))
        .and_then(|m| ItipMethod::parse(&m))?;
    let start = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("BEGIN:VEVENT"))?;
    let mut uid = None;
    let mut sequence = 0;
    let mut recurrence_id = None;
    let mut partstat = None;
    for line in &lines[start..] {
        if line.trim().eq_ignore_ascii_case("END:VEVENT") {
            break;
        }
        if let Some(v) = value(line, "UID") {
            uid = Some(v);
        } else if let Some(v) = value(line, "SEQUENCE") {
            sequence = v.parse().unwrap_or(0);
        } else if let Some(v) = value(line, "RECURRENCE-ID") {
            recurrence_id = Some(v);
        } else if method == ItipMethod::Reply
            && partstat.is_none()
            && value(line, "ATTENDEE").is_some()
        {
            partstat = Some(param(line, "PARTSTAT").unwrap_or_else(|| "NEEDS-ACTION".into()));
        }
    }
    Some(ItipKey {
        uid: uid?,
        sequence,
        method,
        recurrence_id,
        partstat,
    })
}

fn param(line: &str, name: &str) -> Option<String> {
    let (head, _) = line.split_once(':')?;
    head.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().trim_matches('"').to_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_key_of_an_imip_message() {
        let message = b"From: Ada <ada@example.com>\r\nTo: bob@example.org\r\nSubject: Lunch\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:lunch-1\r\n @example.com\r\nSEQUENCE:2\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(
            key(message),
            Some(ItipKey {
                uid: "lunch-1@example.com".into(),
                sequence: 2,
                method: ItipMethod::Request,
                recurrence_id: None,
                partstat: None,
            })
        );
        let reply = b"From: Bob <bob@example.org>\r\nTo: ada@example.com\r\nSubject: Lunch\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; method=REPLY\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:lunch-1@example.com\r\nRECURRENCE-ID:20261214T120000Z\r\nSEQUENCE:2\r\nORGANIZER:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=declined:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(
            key(reply),
            Some(ItipKey {
                uid: "lunch-1@example.com".into(),
                sequence: 2,
                method: ItipMethod::Reply,
                recurrence_id: Some("20261214T120000Z".into()),
                partstat: Some("DECLINED".into()),
            })
        );
        assert_eq!(
            key(b"From: a@example.com\r\nSubject: x\r\n\r\nplain\r\n"),
            None
        );
    }
}
