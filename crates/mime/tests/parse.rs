//! Parsing the invented messages in `tests/fixtures`.

use holtmail_mime::{Address, MimeError, parse};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn address(name: Option<&str>, email: &str) -> Address {
    Address {
        name: name.map(str::to_owned),
        email: email.to_owned(),
    }
}

#[test]
fn reads_headers_of_a_plain_message() {
    let message = parse(&fixture("plain.eml")).unwrap();
    assert_eq!(message.message_id.as_deref(), Some("lunch-1@example.com"));
    assert_eq!(message.subject.as_deref(), Some("Lunch on Friday"));
    assert_eq!(message.sent_at, Some(1_741_941_000));
    assert_eq!(
        message.from,
        vec![address(Some("Ada Lovelace"), "ada@example.com")]
    );
    assert_eq!(
        message.to,
        vec![
            address(Some("Bob"), "bob@example.org"),
            address(None, "carol@example.org"),
        ]
    );
    assert_eq!(message.cc, vec![address(Some("Team"), "team@example.org")]);
    assert_eq!(
        message.text.as_deref().map(str::trim),
        Some("Shall we meet at noon?")
    );
    assert_eq!(message.html, None);
    assert_eq!(message.preview, "Shall we meet at noon?");
    assert!(message.attachments.is_empty());
}

#[test]
fn reads_threading_bodies_and_attachments() {
    let message = parse(&fixture("alternative-with-attachment.eml")).unwrap();
    assert_eq!(message.in_reply_to, vec!["lunch-1@example.com"]);
    assert_eq!(
        message.references,
        vec!["lunch-0@example.com", "lunch-1@example.com"]
    );
    assert_eq!(
        message.text.as_deref().map(str::trim),
        Some("Noon works. Menu attached.")
    );
    assert!(message.html.unwrap().contains("<b>Menu</b>"));

    let [menu] = message.attachments.as_slice() else {
        panic!("expected one attachment, got {:?}", message.attachments);
    };
    assert_eq!(menu.filename.as_deref(), Some("menu.txt"));
    assert_eq!(menu.content_type, "text/plain");
    assert!(!menu.inline);
    assert_eq!(&menu.content[..], b"Soup, bread, tea");
}

#[test]
fn decodes_legacy_charsets() {
    let message = parse(&fixture("encoded-words.eml")).unwrap();
    assert_eq!(message.subject.as_deref(), Some("Отчёт за март"));
    assert_eq!(message.from[0].name.as_deref(), Some("Иван Петров"));
    assert_eq!(message.text.as_deref().map(str::trim), Some("Привет"));
}

#[test]
fn derives_text_from_html_only_messages() {
    let raw =
        b"From: ada@example.com\r\nContent-Type: text/html\r\n\r\n<p>Hello <b>there</b></p>\r\n";
    let message = parse(raw).unwrap();
    assert!(message.html.is_some());
    assert_eq!(message.text.as_deref().map(str::trim), Some("Hello there"));
    assert_eq!(message.preview, "Hello there");
}

#[test]
fn rejects_bytes_without_headers() {
    assert_eq!(
        parse(&fixture("not-a-message.eml")),
        Err(MimeError::NotAMessage)
    );
    assert_eq!(parse(b""), Err(MimeError::NotAMessage));
}

#[test]
fn finds_calendar_parts_inline_and_attached() {
    let message = parse(&fixture("invitation.eml")).unwrap();
    assert_eq!(message.calendars.len(), 2, "{:?}", message.calendars);
    let inline = std::str::from_utf8(&message.calendars[0]).unwrap();
    assert!(inline.contains("METHOD:REQUEST"));
    assert!(inline.contains("Harbour planning \u{2013} dock"));
    assert_eq!(
        &message.calendars[1][..],
        b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n"
    );
    let types: Vec<_> = message
        .attachments
        .iter()
        .map(|a| a.content_type.as_str())
        .collect();
    assert_eq!(
        types,
        ["text/calendar", "application/ics", "message/rfc822"]
    );
}

#[test]
fn unwraps_an_exchange_journal_report() {
    let report = fixture("journal-report.eml");
    let original = holtmail_mime::journal_original(&report).expect("a journal report");
    let text = String::from_utf8_lossy(&original);
    assert!(
        text.starts_with("From: Ada Quill <ada@contoso.example.com>\r\n")
            || text.starts_with("From: Ada Quill <ada@contoso.example.com>\n"),
        "{text}"
    );
    assert!(text.contains("The harbour figures for March"));
    assert!(!text.contains("Bcc: carol"), "the envelope came along");
    let inner = holtmail_mime::parse(&original).unwrap();
    assert_eq!(
        inner.message_id.as_deref(),
        Some("figures-1@contoso.example.com")
    );
    let start = report
        .windows(original.len())
        .position(|w| w == &original[..]);
    assert!(start.is_some(), "not byte for byte");

    assert!(holtmail_mime::journal_original(&fixture("plain.eml")).is_none());
    assert!(holtmail_mime::journal_original(&fixture("alternative-with-attachment.eml")).is_none());
}
