use holtmail_extract::{ExtractError, MAX_TEXT};
use lopdf::{Document, LoadOptions};

const MAX_STREAM: usize = 64 * 1024 * 1024;
const MAX_PAGES: usize = 5_000;

pub(crate) fn text(content: &[u8]) -> Result<String, ExtractError> {
    let document = Document::load_mem_with_options(
        content,
        LoadOptions {
            max_decompressed_size: Some(MAX_STREAM),
            ..LoadOptions::default()
        },
    )
    .map_err(|error| match error {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
            ExtractError::TooLarge
        }
        other => ExtractError::Unreadable(other.to_string()),
    })?;
    if document.is_encrypted() {
        return Err(ExtractError::Unreadable("encrypted".into()));
    }
    let mut out = String::new();
    let mut read_any = false;
    for &number in document.get_pages().keys().take(MAX_PAGES) {
        if out.len() > MAX_TEXT {
            break;
        }
        if let Ok(text) = document.extract_text_with_limit(&[number], MAX_STREAM) {
            read_any = true;
            out.push_str(&text);
            out.push('\n');
        }
    }
    if !read_any && !document.get_pages().is_empty() {
        return Err(ExtractError::Unreadable("no page could be read".into()));
    }
    Ok(out)
}
