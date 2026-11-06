use std::io::{Cursor, Write};

use bytes::Bytes;
use holtmail_extract::{ExtractError, Extractor, File};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use zip::write::SimpleFileOptions;

use super::*;

fn file(content_type: &str, filename: Option<&str>, content: Vec<u8>) -> File {
    File {
        content_type: content_type.into(),
        filename: filename.map(str::to_owned),
        content: Bytes::from(content),
    }
}

async fn text(content_type: &str, filename: Option<&str>, content: Vec<u8>) -> String {
    BasicExtractor::new()
        .extract(&file(content_type, filename, content))
        .await
        .unwrap()
        .unwrap()
}

fn package(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in parts {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn pdf(pages: &[&str]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let mut kids = Vec::new();
    for line in pages {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*line)]),
                Operation::new("ET", vec![]),
            ],
        };
        let stream = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        kids.push(Object::from(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => stream,
        })));
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[tokio::test]
async fn keeps_the_extractor_contract() {
    holtmail_extract::testing::conformance(&BasicExtractor::new()).await;
}

#[tokio::test]
async fn reads_pdf_pages() {
    let text = text(
        "application/pdf",
        Some("report.pdf"),
        pdf(&["Lighthouse inspection", "Harbour budget"]),
    )
    .await;
    assert!(text.contains("Lighthouse inspection"), "{text:?}");
    assert!(text.contains("Harbour budget"), "{text:?}");
}

#[tokio::test]
async fn reads_a_pdf_named_but_untyped() {
    let text = text(
        "application/octet-stream",
        Some("Scan.PDF"),
        pdf(&["Ferry"]),
    )
    .await;
    assert!(text.contains("Ferry"));
}

#[tokio::test]
async fn reads_docx_paragraphs_and_entities() {
    let docx = package(&[(
        "word/document.xml",
        r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Tide</w:t></w:r><w:r><w:t xml:space="preserve"> tables &amp; charts</w:t></w:r></w:p><w:p><w:r><w:instrText>PAGE</w:instrText><w:t>Second</w:t></w:r></w:p></w:body></w:document>"#,
    )]);
    let text = text(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        None,
        docx,
    )
    .await;
    assert_eq!(text, "Tide tables & charts\nSecond\n");
}

#[tokio::test]
async fn reads_xlsx_cells() {
    let xlsx = package(&[
        (
            "xl/sharedStrings.xml",
            r#"<sst><si><t>Invoice</t></si><si><r><t>Total</t></r><r><t> due</t></r></si></sst>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c><v>4711</v></c></row><row><c t="s"><v>1</v></c><c t="inlineStr"><is><t>Paid</t></is></c></row></sheetData></worksheet>"#,
        ),
    ]);
    let text = text(
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("book.xlsx"),
        xlsx,
    )
    .await;
    assert_eq!(text, "Invoice 4711\nTotal due Paid\n");
}

#[tokio::test]
async fn reads_pptx_slides_in_order() {
    let slide = |t: &str| format!(r#"<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#);
    let (two, ten) = (slide("Two"), slide("Ten"));
    let pptx = package(&[
        ("ppt/slides/slide10.xml", ten.as_str()),
        ("ppt/slides/slide2.xml", two.as_str()),
    ]);
    let text = text("application/octet-stream", Some("deck.pptx"), pptx).await;
    assert_eq!(text, "Two\nTen\n");
}

#[tokio::test]
async fn reads_open_document_text_and_sheets() {
    let odt = package(&[(
        "content.xml",
        r#"<office:document-content><office:body><office:text><text:h>Agenda</text:h><text:p>Pier<text:s/>repairs</text:p></office:text></office:body></office:document-content>"#,
    )]);
    assert_eq!(
        text("application/vnd.oasis.opendocument.text", None, odt).await,
        "Agenda\nPier repairs\n"
    );
    let ods = package(&[(
        "content.xml",
        r#"<office:document-content><office:body><office:spreadsheet><table:table><table:table-row><table:table-cell><text:p>Buoys</text:p></table:table-cell><table:table-cell><text:p>12</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
    )]);
    let text = text("application/octet-stream", Some("count.ods"), ods).await;
    assert!(text.contains("Buoys") && text.contains("12"), "{text:?}");
}

#[tokio::test]
async fn reads_html_and_csv() {
    let html = text(
        "text/html",
        None,
        b"<html><style>p{}</style><body><p>Keeper&#39;s <b>log</b></p></body></html>".to_vec(),
    )
    .await;
    assert!(html.contains("Keeper's log"), "{html:?}");
    assert!(!html.contains('<'));
    let csv = text("text/csv", None, b"name,count\nrope,3\n".to_vec()).await;
    assert_eq!(csv, "name,count\nrope,3\n");
}

#[tokio::test]
async fn refuses_a_zip_bomb() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "word/document.xml",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    let zeros = vec![b' '; 1024 * 1024];
    for _ in 0..(MAX_UNPACKED / zeros.len() + 1) {
        zip.write_all(&zeros).unwrap();
    }
    let bomb = zip.finish().unwrap().into_inner();
    assert!(bomb.len() < MAX_INPUT);
    let result = BasicExtractor::new()
        .extract(&file(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
            bomb,
        ))
        .await;
    assert!(matches!(result, Err(ExtractError::TooLarge)), "{result:?}");
}

#[tokio::test]
async fn damaged_files_are_unreadable() {
    let mut broken = pdf(&["Gone"]);
    broken.truncate(40);
    let extractor = BasicExtractor::new();
    for (content_type, content) in [
        ("application/pdf", broken),
        (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            package(&[("other.xml", "<x/>")]),
        ),
        (
            "application/vnd.oasis.opendocument.text",
            package(&[("content.xml", "<a><b></a>")]),
        ),
    ] {
        let result = extractor.extract(&file(content_type, None, content)).await;
        assert!(
            matches!(result, Err(ExtractError::Unreadable(_))),
            "{content_type}: {result:?}"
        );
    }
}

#[tokio::test]
async fn gives_up_after_the_timeout() {
    let extractor = BasicExtractor {
        timeout: std::time::Duration::ZERO,
    };
    let result = extractor
        .extract(&file("application/pdf", None, pdf(&["Slow"])))
        .await;
    assert!(
        matches!(result, Err(ExtractError::TimedOut) | Ok(Some(_))),
        "{result:?}"
    );
}

#[test]
fn picks_the_kind_from_the_type_then_the_name() {
    assert_eq!(kind("TEXT/PLAIN; charset=utf-8", None), Some(Kind::Text));
    assert_eq!(kind("application/msword", Some("a.docx")), None);
    assert_eq!(kind("application/zip", Some("a.docx")), Some(Kind::Docx));
    assert_eq!(kind("application/octet-stream", Some("archive")), None);
    assert_eq!(kind("image/png", Some("a.txt")), None);
}

#[tokio::test]
async fn reads_calendar_and_card_words_without_their_syntax() {
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:x1@example.com\r\nDTSTART:20250310T090000Z\r\nSUMMARY:Harbour walk\\, then lunch\r\nLOCATION:Pier 4\r\nDESCRIPTION:Bring boots.\\nWe meet at the gate\r\n  near the café.\r\nORGANIZER;CN=\"Doe, Jane\":mailto:jane@example.com\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Ada:MAILTO:ada@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let got = text("text/calendar; method=REQUEST", None, ics.into()).await;
    for word in [
        "Harbour walk, then lunch",
        "Pier 4",
        "Bring boots.",
        "We meet at the gate near the café.",
        "Doe, Jane",
        "jane@example.com",
        "Ada",
        "ada@example.org",
    ] {
        assert!(got.contains(word), "{word} missing in {got:?}");
    }
    for syntax in [
        "BEGIN",
        "VEVENT",
        "DTSTART",
        "PRODID",
        "mailto",
        "MAILTO",
        "x1@example.com",
        "2025",
    ] {
        assert!(!got.contains(syntax), "{syntax} kept in {got:?}");
    }

    let vcf = b"BEGIN:VCARD\nVERSION:3.0\nFN:Ada Lovelace\nORG:Analytical Engines;Research\nitem1.EMAIL;TYPE=INTERNET:ada@example.org\nADR;TYPE=HOME:;;12 Harbour Road;Bristol;;BS1 4ND;UK\nREV:20250101T000000Z\nEND:VCARD\n".to_vec();
    let got = text("application/octet-stream", Some("ada.vcf"), vcf).await;
    for word in [
        "Ada Lovelace",
        "Analytical Engines Research",
        "ada@example.org",
        "12 Harbour Road",
        "Bristol",
    ] {
        assert!(got.contains(word), "{word} missing in {got:?}");
    }
    assert!(!got.contains("VCARD") && !got.contains("REV"), "{got:?}");
}
