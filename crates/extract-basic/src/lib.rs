//! Holtmail's [`Extractor`] in pure Rust: plain text, CSV, HTML, PDF, DOCX, XLSX, PPTX, ODT, ODS
//! and ODP.

mod office;
mod pdf;

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_extract::{ExtractError, Extractor, File, MAX_INPUT, MAX_TEXT, cut};

/// How long one file may take before [`ExtractError::TimedOut`].
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The most bytes a compressed file may unpack to, all parts together.
pub const MAX_UNPACKED: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Html,
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    OpenDocument,
}

/// Reads text with pure-Rust parsers on the blocking thread pool, one file at a time per call.
#[derive(Debug, Clone)]
pub struct BasicExtractor {
    timeout: Duration,
}

impl Default for BasicExtractor {
    fn default() -> Self {
        Self { timeout: TIMEOUT }
    }
}

impl BasicExtractor {
    /// An extractor that gives up after [`TIMEOUT`].
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Extractor for BasicExtractor {
    async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError> {
        let Some(kind) = kind(&file.content_type, file.filename.as_deref()) else {
            return Ok(None);
        };
        if file.content.len() > MAX_INPUT {
            return Err(ExtractError::TooLarge);
        }
        let content = file.content.clone();
        let work = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| read(kind, &content)))
                .unwrap_or_else(|_| Err(ExtractError::Unreadable("the parser failed".into())))
        });
        match tokio::time::timeout(self.timeout, work).await {
            Ok(Ok(result)) => result.map(|text| Some(cut(tidy(&text), MAX_TEXT))),
            Ok(Err(_)) => Err(ExtractError::Unreadable("the parser failed".into())),
            Err(_) => Err(ExtractError::TimedOut),
        }
    }
}

fn read(kind: Kind, content: &[u8]) -> Result<String, ExtractError> {
    match kind {
        Kind::Text => Ok(String::from_utf8_lossy(content).into_owned()),
        Kind::Html => Ok(mail_parser::decoders::html::html_to_text(
            &String::from_utf8_lossy(content),
        )),
        Kind::Pdf => pdf::text(content),
        Kind::Docx => office::docx(content),
        Kind::Xlsx => office::xlsx(content),
        Kind::Pptx => office::pptx(content),
        Kind::OpenDocument => office::open_document(content),
    }
}

fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT + 4));
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        out.push_str(&line);
        out.push('\n');
        if out.len() > MAX_TEXT {
            break;
        }
    }
    out
}

fn kind(content_type: &str, filename: Option<&str>) -> Option<Kind> {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let by_type = match content_type.as_str() {
        "text/html" | "application/xhtml+xml" => Some(Kind::Html),
        "application/pdf" | "application/x-pdf" => Some(Kind::Pdf),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(Kind::Docx)
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some(Kind::Xlsx),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(Kind::Pptx)
        }
        "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.spreadsheet"
        | "application/vnd.oasis.opendocument.presentation" => Some(Kind::OpenDocument),
        "application/json" | "application/xml" | "application/csv" | "application/x-csv" => {
            Some(Kind::Text)
        }
        t if t.starts_with("text/") => Some(Kind::Text),
        _ => None,
    };
    let generic = matches!(
        content_type.as_str(),
        "" | "application/octet-stream"
            | "binary/octet-stream"
            | "application/zip"
            | "application/x-zip-compressed"
            | "application/download"
            | "application/x-download"
            | "application/force-download"
            | "application/unknown"
    );
    if by_type.is_some() || !generic {
        return by_type;
    }
    let extension = filename?.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "txt" | "text" | "csv" | "tsv" | "md" | "log" | "json" | "xml" | "ics" | "vcf" => {
            Some(Kind::Text)
        }
        "html" | "htm" => Some(Kind::Html),
        "pdf" => Some(Kind::Pdf),
        "docx" => Some(Kind::Docx),
        "xlsx" => Some(Kind::Xlsx),
        "pptx" => Some(Kind::Pptx),
        "odt" | "ods" | "odp" => Some(Kind::OpenDocument),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
