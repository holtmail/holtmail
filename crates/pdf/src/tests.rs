use lopdf::{Document as Pdf, Object};

use super::*;

fn message(text: String) -> MessagePdf {
    MessagePdf {
        subject: Some("Квартальный отчёт: harbour figures".into()),
        headers: vec![
            ("From".into(), "Ada Quill <ada@example.com>".into()),
            (
                "To".into(),
                "Bob Stone <bob@example.org>, Борис <boris@example.org>".into(),
            ),
            ("Date".into(), "2025-03-10 09:00 UTC".into()),
        ],
        text,
        attachments: vec![("figures.pdf".into(), 48_213), ("notes.txt".into(), 12)],
        raw: b"From: Ada Quill <ada@example.com>\r\nSubject: figures\r\n\r\nHello\r\n".to_vec(),
    }
}

fn embedded(pdf: &Pdf) -> Vec<u8> {
    let catalog = pdf.catalog().unwrap();
    let names = match catalog.get(b"Names").unwrap() {
        Object::Reference(r) => pdf.get_dictionary(*r).unwrap().clone(),
        Object::Dictionary(d) => d.clone(),
        _ => panic!("no names"),
    };
    let files = names.get(b"EmbeddedFiles").unwrap();
    let files = match files {
        Object::Reference(r) => pdf.get_dictionary(*r).unwrap().clone(),
        Object::Dictionary(d) => d.clone(),
        _ => panic!("no embedded files"),
    };
    let list = files.get(b"Names").unwrap().as_array().unwrap();
    assert_eq!(list[0].as_str().unwrap(), b"message.eml");
    let spec = pdf.get_dictionary(list[1].as_reference().unwrap()).unwrap();
    let ef = spec.get(b"EF").unwrap().as_dict().unwrap();
    let stream = pdf
        .get_object(ef.get(b"F").unwrap().as_reference().unwrap())
        .unwrap()
        .as_stream()
        .unwrap()
        .clone();
    stream.decompressed_content().unwrap_or(stream.content)
}

#[test]
fn writes_pages_with_the_original_inside() {
    let short = render(&message("Hello Bob,\nthe figures are in.\n\nAda".into())).unwrap();
    assert!(short.starts_with(b"%PDF-"));
    let pdf = Pdf::load_mem(&short).unwrap();
    assert_eq!(pdf.get_pages().len(), 1);
    assert_eq!(
        embedded(&pdf),
        message(String::new()).raw,
        "the original changed"
    );

    let long: String = (0..400)
        .map(|n| format!("Строка {n}: the harbour figures for March and a little more text to wrap around the page.\n"))
        .collect();
    let pdf = Pdf::load_mem(&render(&message(long)).unwrap()).unwrap();
    assert!(pdf.get_pages().len() > 5, "{} pages", pdf.get_pages().len());
}

#[test]
fn wraps_inside_the_margins() {
    let width = 200.0;
    let text = format!(
        "short words that wrap nicely {} https://example.com/{}",
        "Длинноеслово".repeat(4),
        "a".repeat(120)
    );
    let lines = wrap(&text, Style::Body, width);
    assert!(lines.len() > 3);
    for line in &lines {
        assert!(regular().width(line, BODY) <= width + 0.01, "{line:?}");
    }
    assert_eq!(
        lines.concat().replace(' ', ""),
        text.replace(' ', ""),
        "text was lost"
    );
    assert_eq!(wrap("a\n\nb", Style::Body, width), ["a", "", "b"]);
    assert_eq!(clean("tab\there\u{7}bell"), "tab    herebell");
}

#[test]
fn says_when_a_body_is_cut() {
    let huge = "word ".repeat(MAX_TEXT / 5 + 10);
    let layout = lay_out(&message(huge));
    let last = layout.pages.last().unwrap();
    assert!(last.iter().any(|p| p.text.contains("left out")));
}
