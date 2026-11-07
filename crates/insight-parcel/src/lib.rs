//! Insight extractor for parcel tracking numbers.
//!
//! [`ParcelExtractor`] finds the tracking numbers of UPU S10 items, UPS, USPS, FedEx, DHL Express
//! and Russian Post in a message's own visible text. A number counts only when its check digit is
//! right and a tracking keyword sits next to it or the sender's domain belongs to the carrier.
//! Patterns, keywords, sender hints, scores and tracking URL templates live in
//! `data/carriers.toml`, whose hash is part of the version.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod check;
mod data;
mod doc;
mod extract;

use std::fmt;
use std::sync::Arc;

use holtmail_insight::{Explanation, Insight, InsightExtractor, InsightInput};

pub use check::check_digit;
pub use data::ALGORITHM;

/// The [`InsightExtractor::id`] of [`ParcelExtractor`].
pub const ID: &str = "parcel";

/// Finds parcel tracking numbers.
///
/// Besides the [`InsightExtractor`] guarantees:
///
/// - **Only [`InsightKind::Parcel`](holtmail_insight::InsightKind::Parcel) items, at most 4**,
///   highest score first, then in text order; at most one per carrier and number.
/// - **Only the message's own content.** Candidates come from the HTML segments when the message
///   has HTML, else from the plain text, never from anything below an inline-forward marker or
///   at or after the footer marker. Anchor `href`s are only context, never the source of a number.
/// - **Exact shapes.** A number matches a carrier pattern exactly, in compact uppercase form;
///   single spaces inside a grouping (`1Z 999 AA1 01 2345 6784`) are dropped. A number that is
///   part of a longer run of digit groups, or that touches digits across `-`, `/`, `.`, `,` or a
///   leading `+`, is no candidate.
/// - **Check digit required.** A number whose check digit is wrong gives nothing; every item has
///   `check_digit: true`.
/// - **Evidence required.** A tracking keyword from the table within the keyword window, or an
///   anchor whose `href` holds the number and whose text holds a keyword, gives 850; a `From`
///   domain equal to or under one of the carrier's host hints gives 900; both give 950. All are
///   `Likely`. A number with neither gives nothing, and so does a number with an order, invoice,
///   account or phone word just before it and no tracking keyword closer.
/// - **Unambiguous.** When one number fits several carriers, the carrier with a host hint, else
///   with its own keyword, is kept; otherwise none is.
/// - **`track_url` only from the table's https template**, so its host never comes from mail;
///   `None` when the carrier (or an S10 number's country) has no template.
/// - Provenance `Heuristic`, origin `Html` or `Text`, `forwarded: false`.
#[derive(Debug, Clone)]
pub struct ParcelExtractor {
    data: Option<Arc<data::Data>>,
}

/// A carrier table that does not parse or breaks the table rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataError(String);

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "carrier table: {}", self.0)
    }
}

impl std::error::Error for DataError {}

impl ParcelExtractor {
    /// The extractor with the embedded `data/carriers.toml`.
    pub fn new() -> Self {
        Self {
            data: data::DATA.clone(),
        }
    }

    /// The extractor with another carrier table in the `carriers.toml` format, for tests with
    /// invented host hints. Its version carries that table's hash.
    pub fn with_data(toml: &str) -> Result<Self, DataError> {
        data::Data::parse(toml)
            .map(|data| Self {
                data: Some(Arc::new(data)),
            })
            .map_err(DataError)
    }
}

impl Default for ParcelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl InsightExtractor for ParcelExtractor {
    fn id(&self) -> &str {
        ID
    }

    fn version(&self) -> String {
        match &self.data {
            Some(data) => data.version.clone(),
            None => format!("{ALGORITHM}+unreadable"),
        }
    }

    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        match &self.data {
            Some(data) => extract::extract(data, input, &mut extract::Log::off()),
            None => Vec::new(),
        }
    }

    /// Every candidate with its skeleton and position: the `pattern` it fits, whether the
    /// `check_digit` holds, the `forwarded`, `footer`, `boundary` and `negative` rules that drop
    /// it, the `keyword` (with its distance as the score), `anchor` and `host_hint` evidence,
    /// `ambiguous` numbers and each `chosen` item with its score.
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
        let Some(data) = &self.data else {
            return vec![Explanation::new("data_unreadable")];
        };
        let mut log = extract::Log::on();
        extract::extract(data, input, &mut log);
        log.into_steps()
    }
}
