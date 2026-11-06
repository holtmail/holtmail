//! Insight extractor for one-time codes.
//!
//! [`CodeExtractor`] finds the one-time code of a sign-in, verification or confirmation message:
//! a WICG origin-bound last line (`@example.com #123456`) as a certain code, otherwise at most one
//! heuristic code scored by keyword distance, emphasis and layout, with named hard negatives for
//! years, money, phone numbers, postcodes, references, promo codes, URLs, footers and forwarded
//! content. The word lists and weights live in `data/code-keywords.toml`, whose hash is part of
//! the version.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod data;
mod doc;
mod extract;
mod matcher;
mod origin;

use holtmail_insight::{Explanation, Insight, InsightExtractor, InsightInput};

/// The [`InsightExtractor::id`] of [`CodeExtractor`].
pub const ID: &str = "code";

/// Finds one-time codes.
///
/// Besides the [`InsightExtractor`] guarantees:
///
/// - **At most one item**, an [`InsightKind::Code`](holtmail_insight::InsightKind::Code).
/// - **Origin-bound codes are certain.** A last text line `@host #code` above any forward marker
///   gives a `Certain` code with `origin_bound` set to the lowercase host and score 1000.
/// - **Heuristic codes need a keyword** from the embedded table in the subject or the first
///   4,096 chars of the body, and no `List-Id` unless `Auto-Submitted` is present. A heuristic code
///   is 4–8 ASCII digits, `ddd ddd` / `ddd-ddd` stored without the separator, or (when no bulk
///   header is set) 6–8 ASCII letters and digits with both. Fullwidth digits are stored as ASCII.
/// - **Unambiguous or nothing.** A heuristic code is returned only when its score beats every
///   other value by the margin of the table; ties and lists of codes give nothing. It is `Likely`
///   at 700 or more and `Possible` below, and its score never reaches 1000.
/// - **Only the message's own content.** Nothing is taken from below an inline-forward marker,
///   after the footer marker, inside a URL, or next to a year, price, phone number, address,
///   reference or promo word.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodeExtractor;

impl CodeExtractor {
    /// The extractor with the embedded table.
    pub fn new() -> Self {
        Self
    }
}

impl InsightExtractor for CodeExtractor {
    fn id(&self) -> &str {
        ID
    }

    fn version(&self) -> String {
        match data::DATA.as_ref() {
            Some(data) => data.version.clone(),
            None => format!("{}+unreadable", data::ALGORITHM),
        }
    }

    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        data::DATA
            .as_ref()
            .and_then(|data| extract::extract(data, input, &mut extract::Log::off()))
            .into_iter()
            .collect()
    }

    /// The gates, every candidate with its skeleton, position, score and nearest keyword or the
    /// hard-negative rule that zeroed it, the ranked values and the outcome.
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
        let Some(data) = data::DATA.as_ref() else {
            return vec![Explanation::new("data_unreadable")];
        };
        let mut log = extract::Log::on();
        extract::extract(data, input, &mut log);
        log.into_steps()
    }
}
