//! Insight extractor for schema.org markup in mail.
//!
//! [`SchemaOrgExtractor`] reads the JSON-LD blocks and microdata items of a message's HTML body
//! and maps the seven reservation types, `Order`, `Invoice`, `ParcelDelivery` and the
//! `ConfirmAction`, `ViewAction`, `TrackAction` and `CheckInAction` actions to insight items.
//! ISO 4217 exponents (`data/currencies.toml`) and parcel carrier names (`data/carriers.toml`)
//! are embedded, and their hash is part of the version.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod data;
mod graph;
mod map;
mod money;
mod time;

use std::sync::Arc;

use holtmail_insight::{Explanation, Insight, InsightExtractor, InsightInput};

pub use data::ALGORITHM;
pub use graph::{MAX_JSON_DEPTH, MAX_MICRODATA_DEPTH, MAX_NODES};

/// The [`InsightExtractor::id`] of [`SchemaOrgExtractor`].
pub const ID: &str = "schemaorg";

/// Reads schema.org JSON-LD and microdata.
///
/// Besides the [`InsightExtractor`] guarantees:
///
/// - **Only declared markup.** Items come from [`HtmlView::ld_json`] and
///   [`HtmlView::microdata`], never from visible text, so a message without HTML gives nothing.
///   Forged markup is extracted like any other; judging the sender is the runner's job.
/// - **Known types only.** `@type` may be a bare name, `schema:X` or `http(s)://schema.org/X`;
///   in a type array only the known members count, and the first one decides. Other types give
///   nothing.
/// - **Required fields.** An item missing a required field of its type, or whose end lies before
///   its start, is dropped whole. Every kept item is `Certain` with score 1000, origin `Html`,
///   provenance `JsonLd` or `Microdata`.
/// - **Merged by key.** Items with equal merge keys (for a flight: airline, flight number and
///   local departure date) become one item, keeping the first item's place and fields, filling
///   missing ones from later items and joining passengers, seats, guests and tickets (at most 9
///   each). A merged item is `JsonLd` when any part was, and forwarded only when every part was.
/// - **Forwards.** An item is `forwarded` when its JSON-LD block is at or after
///   [`HtmlView::ld_json_forward_at`] or its microdata item is `below_forward`.
/// - **Bounded.** One JSON-LD block parses within serde_json's recursion limit (128), or is
///   skipped. The walk is iterative, follows at most [`MAX_JSON_DEPTH`] levels of JSON-LD and
///   [`MAX_MICRODATA_DEPTH`] of microdata, and expands at most [`MAX_NODES`] nodes per message;
///   past the budget nothing more is read. `@id` references resolve one level deep against the
///   block's top-level nodes, each id at most once per top-level node; microdata items expand at
///   most once per top-level item, so cycles and out-of-range indexes end the walk there.
/// - **Exact values.** Times keep their written offset and are never converted; money parses as
///   a checked decimal in a currency of the embedded table, or is left out. URLs with refused
///   schemes are left out. Identifiers over 64 chars are left out, never cut.
/// - **Document order.** JSON-LD blocks, then microdata items, each in document order; at most 16
///   items.
///
/// [`HtmlView::ld_json`]: holtmail_insight::HtmlView::ld_json
/// [`HtmlView::microdata`]: holtmail_insight::HtmlView::microdata
/// [`HtmlView::ld_json_forward_at`]: holtmail_insight::HtmlView::ld_json_forward_at
#[derive(Clone)]
pub struct SchemaOrgExtractor {
    data: Option<Arc<data::Data>>,
}

impl std::fmt::Debug for SchemaOrgExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaOrgExtractor")
            .field("version", &self.version())
            .finish()
    }
}

impl SchemaOrgExtractor {
    /// The extractor with the embedded data tables.
    pub fn new() -> Self {
        Self {
            data: data::DATA.clone(),
        }
    }
}

impl Default for SchemaOrgExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl InsightExtractor for SchemaOrgExtractor {
    fn id(&self) -> &str {
        ID
    }

    /// `ALGORITHM + "+" + blake3(currencies.toml, carriers.toml)[..12]`, or
    /// `ALGORITHM + "+unreadable"` when the embedded tables do not parse.
    fn version(&self) -> String {
        match &self.data {
            Some(data) => data.version.clone(),
            None => format!("{ALGORITHM}+unreadable"),
        }
    }

    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        match &self.data {
            Some(data) => map::extract(data, input, &mut map::Log::off()),
            None => Vec::new(),
        }
    }

    /// The walk's cuts (`ld_json_unparsable`, `depth_cut`, `reference_cut`, `node_budget` with
    /// counts), then per top-level node its position and `root` type name or `unknown_type`, the
    /// `missing_required`, `end_before_start`, `money_dropped`, `url_refused` and `merged`
    /// steps, the `item_cap`, and each `chosen` item with its kind.
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
        let Some(data) = &self.data else {
            return vec![Explanation::new("data_unreadable")];
        };
        let mut log = map::Log::on();
        map::extract(data, input, &mut log);
        log.into_steps()
    }
}
