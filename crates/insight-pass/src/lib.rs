//! Insight extractor for wallet pass (`.pkpass`) attachments.
//!
//! [`PassExtractor`] opens each pkpass attachment the runner hands over as a zip archive within
//! fixed caps, reads its `pass.json` and reports one [`InsightKind::Pass`] item per attachment,
//! with boarding details from the pass's `semantics` for air boarding passes. The pass signature
//! is never checked.
//!
//! [`InsightKind::Pass`]: holtmail_insight::InsightKind::Pass

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod archive;
mod extract;
mod pass;

use std::sync::LazyLock;

use holtmail_insight::{Explanation, Insight, InsightExtractor, InsightInput};

/// The [`InsightExtractor::id`] of [`PassExtractor`].
pub const ID: &str = "pass";

/// The version of the reading code; a hash of the caps below is appended to it.
pub const ALGORITHM: u32 = 1;

/// Entries in the archive's central directory, at most. More refuses the attachment.
pub const MAX_ENTRIES: usize = 64;

/// Bytes of `pass.json`, declared or actually inflated, at most. More refuses the attachment.
pub const MAX_PASS_JSON_BYTES: u64 = 512 * 1024;

/// The sum of every entry's declared uncompressed size, at most. More refuses the attachment.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 10 * 1024 * 1024;

static VERSION: LazyLock<String> = LazyLock::new(|| {
    let caps = format!(
        "pkpass={};attachments={};entries={MAX_ENTRIES};pass_json={MAX_PASS_JSON_BYTES};total={MAX_UNCOMPRESSED_BYTES}",
        holtmail_insight::limits::MAX_PKPASS_BYTES,
        holtmail_insight::limits::MAX_PKPASS_ATTACHMENTS,
    );
    let hash = blake3::hash(caps.as_bytes()).to_hex();
    let short: String = hash.chars().take(12).collect();
    format!("{ALGORITHM}+{short}")
});

/// Reads wallet pass attachments.
///
/// Besides the [`InsightExtractor`] guarantees:
///
/// - **Only [`InsightKind::Pass`](holtmail_insight::InsightKind::Pass) items, at most one per
///   attachment** and at most one per ordinal, in attachment order. Only the first
///   [`MAX_PKPASS_ATTACHMENTS`](holtmail_insight::limits::MAX_PKPASS_ATTACHMENTS) attachments
///   with content are looked at, so there are at most that many items.
/// - **Capped reading.** An attachment is skipped whole when its content exceeds
///   [`MAX_PKPASS_BYTES`](holtmail_insight::limits::MAX_PKPASS_BYTES), it is no zip archive, its
///   central directory lists more than [`MAX_ENTRIES`] entries or one name twice, an entry name is absolute or has a
///   `..` segment, the declared uncompressed sizes add up to more than
///   [`MAX_UNCOMPRESSED_BYTES`], `pass.json` is missing from the root, encrypted, compressed with
///   anything but stored or deflate, declared larger than [`MAX_PASS_JSON_BYTES`], or inflates to
///   more than that whatever it declares. No other entry is ever inflated.
/// - **Required fields.** `organizationName`, `description` and exactly one style key must be
///   present; otherwise the attachment gives nothing. Both texts are display-truncated.
/// - **Times as written.** `relevantDate` becomes [`LocalOrZoned::Zoned`] when it has an offset
///   and [`LocalOrZoned::Local`] otherwise, never converted; a value that does not read as an
///   ISO 8601 date-time gives `None`.
/// - **Boarding from `semantics` only.** `boarding` is set only for a `boardingPass` whose
///   `transitType` is `PKTransitTypeAir` and whose top-level `semantics` has valid departure and
///   destination IATA codes; the field arrays are never read.
/// - **Never verified.** `signature_checked` is always `false`.
/// - Provenance `PkPass`, confidence `Certain`, score 1000, origin the attachment,
///   `forwarded: false`.
///
/// [`LocalOrZoned::Zoned`]: holtmail_insight::LocalOrZoned::Zoned
/// [`LocalOrZoned::Local`]: holtmail_insight::LocalOrZoned::Local
#[derive(Debug, Clone, Default)]
pub struct PassExtractor;

impl PassExtractor {
    /// The extractor.
    pub fn new() -> Self {
        Self
    }
}

impl InsightExtractor for PassExtractor {
    fn id(&self) -> &str {
        ID
    }

    fn version(&self) -> String {
        VERSION.clone()
    }

    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        extract::extract(input, &mut extract::Log::off())
    }

    /// One step per attachment with content and per decision: the archive's entry count, the cap
    /// or rule that refused it, the size of `pass.json`, the style, which fields were found and
    /// how many barcode messages and boarding legs it has. Never a value from the pass.
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
        let mut log = extract::Log::on();
        extract::extract(input, &mut log);
        log.into_steps()
    }
}
