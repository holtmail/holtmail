//! Contract for extracting searchable text from attachments.
//!
//! The text is derived data for the search index only: it can be cut short, and it can be made
//! again from the attachment's bytes at any time. The stored attachment is never changed.

#[cfg(feature = "testing")]
pub mod testing;

use async_trait::async_trait;
use bytes::Bytes;

/// Files larger than this are refused with [`ExtractError::TooLarge`] without being read.
pub const MAX_INPUT: usize = 25 * 1024 * 1024;

/// The most text, in bytes of UTF-8, returned for one file.
pub const MAX_TEXT: usize = 1024 * 1024;

/// An attachment to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Lowercase media type without parameters, as the message declared it.
    pub content_type: String,
    /// The file name the message gave, if any.
    pub filename: Option<String>,
    /// The decoded bytes.
    pub content: Bytes,
}

/// Why no text came out of a file.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// The file is larger than [`MAX_INPUT`] or would unpack to more than the implementation
    /// allows. Asking again gives the same answer.
    #[error("too large")]
    TooLarge,
    /// The file claims a supported type but cannot be read: damaged, encrypted or not what it
    /// says. Asking again gives the same answer.
    #[error("unreadable: {0}")]
    Unreadable(String),
    /// Reading took longer than the implementation allows. It may work later, e.g. under less
    /// load.
    #[error("timed out")]
    TimedOut,
}

/// Turns attachments into plain text for the search index.
#[async_trait]
pub trait Extractor: Send + Sync {
    /// The plain text of `file`, or `None` when its type is not one the extractor reads.
    ///
    /// The type is taken from `content_type`, or from the file name's extension when the type is
    /// generic such as `application/octet-stream`. The text is at most [`MAX_TEXT`] bytes, cut at
    /// a character boundary. A file larger than [`MAX_INPUT`] fails with
    /// [`ExtractError::TooLarge`]. A malformed file fails with an error and never panics or
    /// hangs the caller; the same input always gives the same answer, except
    /// [`ExtractError::TimedOut`]. Safe to call concurrently.
    async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError>;
}

/// `text` cut to at most `max` bytes at a character boundary.
pub fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
