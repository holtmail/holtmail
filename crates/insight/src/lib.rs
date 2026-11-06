//! Contract for message insight extractors: codes, links, reservations and unsubscribe options read from a message.
//!
//! An [`InsightExtractor`] reads one [`InsightInput`] and returns [`Insight`] items. This crate
//! holds only the contract, the types in its signatures, the persisted bit sets
//! ([`Signals`], [`KindTags`]) and the stored JSON shape of a row's items ([`Items`]). No parser,
//! no I/O and no merge logic live here.

mod bits;
mod error;
mod explain;
mod input;
mod item;
mod items;
pub mod limits;
mod url;

#[cfg(feature = "testing")]
pub mod testing;

pub use bits::{KindTag, KindTags, Signal, Signals};
pub use error::InsightError;
pub use explain::{Explanation, Skeleton};
pub use input::{
    Anchor, AttachmentMeta, HeaderField, HtmlView, InsightInput, MicrodataItem, MicrodataProperty,
    MicrodataValue, Segment,
};
pub use item::{
    BoardingLeg, BoardingPass, BusReservation, CarrierId, Confidence, Currency, Date,
    EventReservation, FlightReservation, FoodReservation, Insight, InsightKind, LinkAction,
    LocalOrZoned, LodgingReservation, Money, PartRef, PassDate, PassStyle, Provenance,
    RentalCarReservation, Reservation, TrainReservation, fits_identifier, truncate_display,
};
pub use items::{ITEMS_VERSION, Items, ItemsRead};
pub use url::{Url, UrlScheme};

/// Reads insights out of one message.
///
/// Every implementation guarantees:
///
/// - **Deterministic.** Equal input gives equal output, item for item and in the same order. No
///   clock, randomness, locale or hash-map order reaches the output. The "now" for resolving dates
///   is [`InsightInput::sent_at`].
/// - **Pure.** No I/O, no shared mutable state, and no dependence on another extractor. Rules that
///   compare items of different extractors belong to the runner.
/// - **Never fails, never panics.** Input it cannot read gives fewer items or none. The runner still
///   catches panics and drops that extractor's output.
/// - **Quiet on personal mail.** A plain message one person wrote to another gives no items.
/// - **Bounded.** At most [`limits::MAX_ITEMS_PER_EXTRACTOR`] items. Identifiers obey
///   [`fits_identifier`] and are never cut; display text obeys [`truncate_display`]; no identifier
///   or URL is a prefix of a longer value in the input.
pub trait InsightExtractor: Send + Sync {
    /// A short stable name, such as `code` or `unsubscribe`. Never changes for an implementation.
    fn id(&self) -> &str;

    /// The behaviour version. Equal for the life of the process, and changes whenever the output
    /// for some input may change, including the hash of every embedded data table.
    fn version(&self) -> String;

    /// The items found in `input`, most relevant first.
    fn extract(&self, input: &InsightInput) -> Vec<Insight>;

    /// Why [`extract`](Self::extract) gives what it gives for `input`, step by step, for an
    /// operator's terminal.
    ///
    /// Never contains a value, host, URL or free text from the message, only what an
    /// [`Explanation`] can hold. Deterministic and never panics, like `extract`. The default
    /// explains nothing.
    fn explain(&self, _input: &InsightInput) -> Vec<Explanation> {
        Vec::new()
    }
}
