//! Runs insight extractors over a parsed message: input caps, auth facts, merge, validation and fingerprints.
//!
//! [`Runner`] is stage A of message insights: it turns one [`ParsedMessage`] into everything the
//! `message_insights` row stores ([`Extracted`]). It is deterministic: equal messages and equal
//! extractors give byte-equal rows.
//!
//! [`Deriver`] is stage B: it turns the stored stage-A columns, the stored alias row, the trust
//! lists and the archive's own addresses into trust, kind and topics, with no blob read.

#![forbid(unsafe_code)]

mod auth;
mod derive;
mod explain;
mod input;
mod merge;
mod scope;
mod signals;
mod text;

use std::sync::Arc;

use holtmail_core::{ArchiveId, MessageId, Timestamp};
use holtmail_insight::{
    Confidence, Insight, InsightExtractor, InsightInput, Items, KindTags, Provenance, Signal,
    Signals,
};
use holtmail_mime::ParsedMessage;
use holtmail_store::{InsightOutcome, InsightRow};

pub use derive::{
    ALWAYS_ON_SOURCE, AuthSummary, DERIVE_VERSION, DeriveConfig, DeriveContext, Deriver,
    SUGGEST_MIN_MESSAGES, SUGGEST_SHARE_PERCENT, SUGGEST_WINDOW, auth_summary, override_key,
    suggest_authserv,
};
pub use scope::{install_panic_hook, with_message};
pub use text::NormalizedText;

/// The version of the runner's own behaviour: input caps, signal bits, the auth facts shape,
/// merge, validation and the row caps. Part of [`Runner::extract_fp`].
pub const VERSION: &str = "4";

/// Items in one row, at most.
pub const MAX_ITEMS: usize = merge::MAX_ITEMS;
/// Bytes of a row's items JSON, at most.
pub const MAX_ITEMS_JSON_BYTES: usize = merge::MAX_ITEMS_JSON_BYTES;

/// Stage A for one message: what the `message_insights` row stores, except its ids and time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// [`InsightOutcome::Ok`] when every extractor ran, [`InsightOutcome::Partial`] when one
    /// panicked and its items were dropped. Never another outcome.
    pub outcome: InsightOutcome,
    /// Some input hit a cap of [`holtmail_insight::limits`], or items were dropped to keep the
    /// row within [`MAX_ITEMS`] and [`MAX_ITEMS_JSON_BYTES`].
    pub truncated: bool,
    /// The first `From` address, lowercase with an A-label domain; `None` when it has no usable
    /// domain.
    pub from_address: Option<String>,
    /// Header signals, plus [`Signal::Structured`] when a non-forwarded `Certain` item from
    /// JSON-LD, microdata or a pkpass was kept.
    pub signals: Signals,
    /// The kinds of the kept items at [`Confidence::Likely`] or higher that are not forwarded.
    pub kinds: KindTags,
    /// The auth facts JSON: `Authentication-Results` and `DKIM-Signature` facts and field counts.
    pub auth: String,
    /// The kept items, as [`Items::to_json`] writes them.
    pub items: Items,
}

impl Extracted {
    /// The store row for `message` of `archive`, computed under `fingerprint` at `computed_at`.
    pub fn into_row(
        self,
        message: MessageId,
        archive: ArchiveId,
        fingerprint: String,
        computed_at: Timestamp,
    ) -> InsightRow {
        InsightRow {
            message,
            archive,
            fingerprint,
            outcome: self.outcome,
            truncated: self.truncated,
            from_address: self.from_address,
            signals: self.signals.bits(),
            kinds: self.kinds.bits(),
            auth: self.auth,
            items: self.items.to_json(),
            computed_at,
        }
    }
}

/// Runs a fixed list of extractors over messages.
///
/// Guarantees:
///
/// - **Deterministic.** No clock, randomness, locale or hash-map order reaches the result.
/// - **Isolated.** A panicking extractor costs only its own items and turns the outcome into
///   [`InsightOutcome::Partial`]; the panic never reaches the caller. With
///   [`install_panic_hook`] in place, its report carries no message content.
/// - **Bounded.** Input is capped as [`holtmail_insight::limits`] says, each extractor
///   contributes at most [`MAX_ITEMS_PER_EXTRACTOR`](holtmail_insight::limits::MAX_ITEMS_PER_EXTRACTOR)
///   items, and the row at most [`MAX_ITEMS`] items and [`MAX_ITEMS_JSON_BYTES`] of JSON.
/// - **Actions unwrapped.** Every kept action's `destination`, `via` and `tracked` come from
///   [`holtmail_links::unwrap`] and [`holtmail_links::tracker`] over its `url`, never from the
///   extractor; an action whose destination is not a valid [`Url`](holtmail_insight::Url) within
///   [`MAX_DESTINATION_BYTES`](holtmail_insight::limits::MAX_DESTINATION_BYTES) is dropped. A
///   heuristic action yields to a JSON-LD, microdata or pkpass action of the same kind or with the
///   same destination, and a code equal to a reservation, order, invoice or parcel reference is
///   dropped.
/// - **One item per fact.** Reservations, orders, invoices, parcels and passes with equal merge
///   keys (docs/INSIGHTS.md §7.5) are kept once, as the item of the highest provenance (JSON-LD,
///   microdata, pkpass, heuristic), then confidence, then score, then the earliest; it is
///   forwarded only when every joined item was.
/// - **IATA codes.** Kept flight airports are three uppercase ASCII letters and carriers two
///   uppercase ASCII letters or digits; a flight or boarding leg with another airport is dropped.
pub struct Runner {
    extractors: Vec<(Arc<dyn InsightExtractor>, String)>,
    fingerprint: String,
}

impl Runner {
    /// A runner over `extractors`, run in this order and listed in this order in the items.
    /// Each extractor's version is read once, here.
    pub fn new(extractors: Vec<Arc<dyn InsightExtractor>>) -> Self {
        let extractors: Vec<_> = extractors
            .into_iter()
            .map(|extractor| {
                let version = extractor.version();
                (extractor, version)
            })
            .collect();
        let mut fingerprint = format!(
            "insights={VERSION}\nhtml-scan={}\nauth-results={}\nlinks={}\n",
            holtmail_html_scan::VERSION,
            holtmail_auth_results::VERSION,
            holtmail_links::version()
        );
        for (extractor, version) in &extractors {
            fingerprint.push_str(&format!("{}={version}\n", extractor.id()));
        }
        Self {
            extractors,
            fingerprint: blake3::hash(fingerprint.as_bytes()).to_hex().to_string(),
        }
    }

    /// The extraction fingerprint: a hex BLAKE3 hash of [`VERSION`], the HTML scanner's, the auth
    /// grammar's and the link rules' versions ([`holtmail_links::version`], which covers the
    /// unwrap allow-list and the tracker list), and each extractor's `id=version` in order. Equal for the life of
    /// the runner; changes whenever a stored row could differ.
    pub fn extract_fp(&self) -> String {
        self.fingerprint.clone()
    }

    /// The input extractors see for `message`, and whether some part of it hit a cap.
    ///
    /// The text is the first [`MAX_TEXT_BYTES`](holtmail_insight::limits::MAX_TEXT_BYTES) of NFKC
    /// output; the HTML is scanned from its first
    /// [`MAX_HTML_BYTES`](holtmail_insight::limits::MAX_HTML_BYTES); every attachment is listed,
    /// with content only for pkpass files within their caps. Deterministic; never panics on any
    /// message `holtmail_mime` parsed.
    pub fn input(message: &ParsedMessage) -> (InsightInput, bool) {
        input::build(message)
    }

    /// Stage A for `message`.
    pub fn extract(&self, message: &ParsedMessage) -> Extracted {
        let (input, truncated) = Self::input(message);
        self.extract_input(&input, truncated)
    }

    /// A report of how stage A reads `message`, for an operator's terminal: the fingerprint, the
    /// input's sizes and caps, the header signals, every extractor's
    /// [`explain`](InsightExtractor::explain) steps and the kept items by kind, confidence,
    /// provenance, score and shape.
    ///
    /// Carries no message content: no values, hosts, URLs, addresses or text, only what an
    /// [`Explanation`](holtmail_insight::Explanation) holds and the shape of each kept value.
    /// Never panics; an extractor whose `explain` panics is reported as such.
    pub fn explain(&self, message: &ParsedMessage) -> String {
        let (input, truncated) = Self::input(message);
        let extracted = self.extract_input(&input, truncated);
        explain::report(self, &input, truncated, &extracted)
    }

    /// Stage A for an input already built, `truncated` telling whether building it hit a cap.
    /// [`Runner::extract`] is this over [`Runner::input`].
    pub fn extract_input(&self, input: &InsightInput, truncated: bool) -> Extracted {
        let mut outcome = InsightOutcome::Ok;
        let mut items = Vec::new();
        for (extractor, version) in &self.extractors {
            match scope::run_extractor(extractor.as_ref(), version, input) {
                Some(found) => items.extend(merge::per_extractor(found)),
                None => outcome = InsightOutcome::Partial,
            }
        }
        let items = merge::normalise(items);
        let items = merge::merge(items);
        let items = merge::validate(input, items);
        let (items, dropped) = merge::cap(items);

        let mut signals = signals::header_signals(input);
        if items.iter().any(is_structured_fact) {
            signals.insert(Signal::Structured);
        }
        let kinds = items
            .iter()
            .filter(|item| item.confidence >= Confidence::Likely && !item.forwarded)
            .map(|item| item.kind.tag())
            .collect();

        Extracted {
            outcome,
            truncated: truncated || dropped,
            from_address: auth::from_address(input).map(|(address, _)| address),
            signals,
            kinds,
            auth: auth::auth_facts(input),
            items: Items { items },
        }
    }
}

fn is_structured_fact(item: &Insight) -> bool {
    !item.forwarded
        && item.confidence == Confidence::Certain
        && matches!(
            item.provenance,
            Provenance::JsonLd | Provenance::Microdata | Provenance::PkPass
        )
}

#[cfg(test)]
mod tests;
