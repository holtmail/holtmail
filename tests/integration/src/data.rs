//! Generators of invented messages, mbox files, vCards and iCalendar objects. Addresses are on
//! `example.com` and `example.org`; names are made up.

use chrono::{DateTime, Utc};

/// The current time.
pub fn now() -> DateTime<Utc> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970");
    DateTime::from_timestamp_millis(since.as_millis() as i64).expect("a valid time")
}

/// One invented message, built into RFC 5322 bytes with CRLF line ends.
#[derive(Debug, Clone)]
pub struct Message {
    /// `From` address.
    pub from: String,
    /// `From` display name.
    pub from_name: String,
    /// `To` address.
    pub to: String,
    /// `Subject`, ASCII.
    pub subject: String,
    /// `Date`.
    pub date: DateTime<Utc>,
    /// `Message-ID` without angle brackets.
    pub message_id: String,
    /// Plain text body, lines joined with CRLF.
    pub body: String,
    /// Extra header lines, written as given.
    pub headers: Vec<String>,
    /// `Content-Type` of the body.
    pub content_type: String,
}

impl Message {
    /// A message from an invented sender to `to`, with a unique subject and `Message-ID`.
    pub fn new(to: &str) -> Self {
        let token = crate::unique("msg");
        Self {
            from: format!("{token}@example.org"),
            from_name: "Sender Example".into(),
            to: to.to_owned(),
            subject: format!("Test message {token}"),
            date: now(),
            message_id: format!("{token}@example.org"),
            body: format!("Hello,\r\nthis is an invented message {token}.\r\n"),
            headers: Vec::new(),
            content_type: "text/plain; charset=utf-8".into(),
        }
    }

    /// The same message with `html` as an HTML body instead of text.
    pub fn html(mut self, html: &str) -> Self {
        self.content_type = "text/html; charset=utf-8".into();
        self.body(html)
    }

    /// The same message with `subject`.
    pub fn subject(mut self, subject: &str) -> Self {
        self.subject = subject.to_owned();
        self
    }

    /// The same message dated `date`.
    pub fn date(mut self, date: DateTime<Utc>) -> Self {
        self.date = date;
        self
    }

    /// The same message with `body` as its text.
    pub fn body(mut self, body: &str) -> Self {
        self.body = body.replace("\r\n", "\n").replace('\n', "\r\n");
        self
    }

    /// The same message with one more header line such as `In-Reply-To: <a@example.org>`.
    pub fn header(mut self, line: &str) -> Self {
        self.headers.push(line.to_owned());
        self
    }

    /// The message's bytes.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = format!(
            "From: {} <{}>\r\nTo: <{}>\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <{}>\r\nMIME-Version: 1.0\r\nContent-Type: {}\r\n",
            self.from_name,
            self.from,
            self.to,
            self.subject,
            self.date.to_rfc2822(),
            self.message_id,
            self.content_type
        );
        for line in &self.headers {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(&self.body);
        if !self.body.ends_with("\r\n") {
            out.push_str("\r\n");
        }
        out.into_bytes()
    }
}

/// An mbox file (`mboxrd`) holding `messages`, with LF line ends as mail programs write it.
pub fn mbox(messages: &[Message]) -> Vec<u8> {
    let mut out = String::new();
    for message in messages {
        out.push_str(&format!(
            "From {} {}\n",
            message.from,
            message.date.format("%a %b %e %H:%M:%S %Y")
        ));
        let text = String::from_utf8(message.bytes()).expect("ASCII message");
        for line in text.split("\r\n") {
            let quoted = line.trim_start_matches('>');
            if quoted.starts_with("From ") {
                out.push('>');
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out.into_bytes()
}

/// A vCard 4.0 with a `UID`, a name, an email and optionally a phone.
pub fn vcard(uid: &str, name: &str, email: &str, phone: Option<&str>) -> String {
    let (given, family) = name.split_once(' ').unwrap_or((name, ""));
    let mut card = format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\nN:{family};{given};;;\r\nEMAIL:{email}\r\n"
    );
    if let Some(phone) = phone {
        card.push_str(&format!("TEL;VALUE=uri:tel:{phone}\r\n"));
    }
    card.push_str("END:VCARD\r\n");
    card
}

/// A `VCALENDAR` with one `VEVENT` in UTC, and an `RRULE` when given.
pub fn icalendar_event(
    uid: &str,
    summary: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    rrule: Option<&str>,
) -> String {
    let stamp = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
    let mut event = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Integration tests//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{summary}\r\n",
        stamp(start),
        stamp(start),
        stamp(end)
    );
    if let Some(rule) = rrule {
        event.push_str(&format!("RRULE:{rule}\r\n"));
    }
    event.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_messages_with_crlf_and_unique_ids() {
        let a = Message::new("ada@example.com").body("one\ntwo");
        let b = Message::new("ada@example.com");
        assert_ne!(a.message_id, b.message_id);
        let text = String::from_utf8(a.bytes()).unwrap();
        assert!(text.contains("\r\n\r\none\r\ntwo\r\n"));
        assert!(!text.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn quotes_from_lines_in_mbox() {
        let message = Message::new("ada@example.com").body("From here on\n>From there");
        let mbox = String::from_utf8(mbox(&[message])).unwrap();
        assert!(mbox.starts_with("From "));
        assert!(mbox.contains("\n>From here on\n>>From there\n"));
    }

    #[test]
    fn writes_cards_and_events() {
        assert!(
            vcard("u1", "Ada Example", "ada@example.com", Some("+15550100"))
                .contains("N:Example;Ada;;;\r\n")
        );
        let start = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let event = icalendar_event("e1", "Review", start, start, Some("FREQ=WEEKLY;COUNT=3"));
        assert!(event.contains("DTSTART:20231114T221320Z\r\nDTEND:"));
        assert!(event.contains("RRULE:FREQ=WEEKLY;COUNT=3\r\n"));
    }
}
