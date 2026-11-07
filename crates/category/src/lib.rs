//! The contract for message categorizers.
//!
//! A [`Categorizer`] reads a message's stored stage-A columns ([`CategoryInput`]) and votes on its
//! [`MessageKind`] and its [`Topic`]s. The runner combines the votes of every categorizer by
//! [`CategorySource::rank`] and applies the trust gate ([`TopicSet::gated`]). Nothing here reads a
//! body, does I/O or reads a clock. The design is `docs/INSIGHTS.md` §4.2.

#![forbid(unsafe_code)]

mod error;
mod kind;
mod reservation;
mod topic;
mod trust;
mod vote;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::sync::Arc;

pub use error::CategoryError;
pub use holtmail_insight::{KindTag, KindTags, Signal, Signals};
pub use kind::MessageKind;
pub use reservation::{ReservationKind, ReservationKinds};
pub use topic::{Topic, TopicSet};
pub use trust::TrustState;
pub use vote::{CategoryInput, CategorySource, CategoryVote};

/// The version of the taxonomy: the [`MessageKind`] and [`Topic`] sets and their meaning. It
/// changes whenever a kind or topic is added or its meaning changes, and joins `derive_fp`.
pub const TAXONOMY_VERSION: &str = "1";

/// A source of category votes, such as the header rules or a domain table.
///
/// Object safe, `Send + Sync`. A categorizer that reloads its data does so behind
/// [`Categorizer::snapshot`]: each snapshot is a consistent view of one set of data.
pub trait Categorizer: Send + Sync {
    /// A stable, non-empty lowercase name (`signals`, `domains`). It never changes for the life of
    /// the value.
    fn id(&self) -> &str;

    /// A consistent view of the categorizer's current data. Two calls with no reload in between
    /// give snapshots with equal versions and equal answers. A snapshot taken before a reload
    /// keeps the old data and version.
    fn snapshot(&self) -> Arc<dyn CategorySnapshot>;
}

/// One consistent view of a [`Categorizer`]: its rules, its tables and their version.
///
/// Its version and answers never change for its whole life.
pub trait CategorySnapshot: Send + Sync {
    /// A non-empty version that changes whenever any answer could change: the rule version and
    /// the hash of every loaded table. Equal for every call on one snapshot.
    fn version(&self) -> String;

    /// The vote for one message, or `None` when this categorizer has nothing to say about it.
    ///
    /// Deterministic for equal input within one snapshot. Does no I/O, reads no clock and never
    /// panics, whatever the input holds (unknown bits, long or non-ASCII senders). The vote's
    /// topics hold known [`Topic`]s only, and the trust gate is not applied: the runner applies
    /// it.
    fn categorize(&self, input: &CategoryInput) -> Option<CategoryVote>;
}

#[cfg(test)]
mod tests;
