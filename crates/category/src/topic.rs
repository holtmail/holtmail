use std::fmt;

use serde::{Deserialize, Serialize};

use crate::TrustState;

/// What a message is about. A message has zero or more. Bit positions are persisted,
/// append-only and never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Orders, receipts and parcels.
    Purchases = 0,
    /// Trips and their bookings.
    Travel = 1,
    /// Banks, invoices and payments. Sensitive.
    Finance = 2,
    /// Health care. Sensitive, and can be turned off.
    Health = 3,
    /// Social networks.
    Social = 4,
    /// Job offers and applications.
    Jobs = 5,
    /// Events and their tickets.
    Events = 6,
    /// Public authorities. Sensitive.
    Government = 7,
}

impl Topic {
    /// Every topic, in bit order.
    pub const ALL: &'static [Self] = &[
        Self::Purchases,
        Self::Travel,
        Self::Finance,
        Self::Health,
        Self::Social,
        Self::Jobs,
        Self::Events,
        Self::Government,
    ];

    /// The persisted bit position. Never changes and is never reused.
    pub const fn bit(self) -> u32 {
        self as u32
    }

    /// The snake_case name. Stored, so it never changes.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Purchases => "purchases",
            Self::Travel => "travel",
            Self::Finance => "finance",
            Self::Health => "health",
            Self::Social => "social",
            Self::Jobs => "jobs",
            Self::Events => "events",
            Self::Government => "government",
        }
    }

    /// The topic with this exact name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|topic| topic.name() == name)
    }

    /// The topic at this bit position.
    pub fn from_bit(bit: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|topic| topic.bit() == bit)
    }

    /// Whether the topic needs aligned trust from every source, overrides included: finance,
    /// health and government.
    pub const fn is_sensitive(self) -> bool {
        matches!(self, Self::Finance | Self::Health | Self::Government)
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of [`Topic`]s (the `derived_topics` and `category_overrides.topics` columns).
///
/// Stored as an integer of its bits. Bits this build does not know are kept as they are, so a
/// value written by a newer build survives a round trip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopicSet(u64);

impl TopicSet {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// The set with exactly these bits, unknown ones included.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The stored bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether the set holds no bit at all.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `topic` is in the set.
    pub const fn contains(self, topic: Topic) -> bool {
        self.0 & (1 << topic.bit()) != 0
    }

    /// Adds `topic`.
    pub fn insert(&mut self, topic: Topic) {
        self.0 |= 1 << topic.bit();
    }

    /// Removes `topic`.
    pub fn remove(&mut self, topic: Topic) {
        self.0 &= !(1 << topic.bit());
    }

    /// The set with `topic` added.
    #[must_use]
    pub const fn with(self, topic: Topic) -> Self {
        Self(self.0 | (1 << topic.bit()))
    }

    /// The known topics in the set, in bit order.
    pub fn iter(self) -> impl Iterator<Item = Topic> {
        Topic::ALL
            .iter()
            .copied()
            .filter(move |topic| self.contains(*topic))
    }

    /// The set without the bits this build does not know.
    #[must_use]
    pub fn known(self) -> Self {
        self.iter().collect()
    }

    /// The topic list rows show: the first member in bit order that is not [`Topic::Health`].
    /// `None` when there is none.
    pub fn list_topic(self) -> Option<Topic> {
        self.iter().find(|topic| *topic != Topic::Health)
    }

    /// The topics that apply under `trust`: sensitive topics are dropped unless `trust` is
    /// aligned, health is dropped when `health` is false, and unknown bits are dropped, since
    /// this build cannot tell whether they are sensitive.
    ///
    /// This is the one gate for every source, overrides included (`docs/INSIGHTS.md` §4.2 and
    /// §4.5). It lives in the contract so the runner and the store apply the same rule.
    #[must_use]
    pub fn gated(self, trust: &TrustState, health: bool) -> Self {
        let aligned = trust.is_aligned();
        self.iter()
            .filter(|topic| aligned || !topic.is_sensitive())
            .filter(|topic| health || *topic != Topic::Health)
            .collect()
    }
}

impl FromIterator<Topic> for TopicSet {
    fn from_iter<I: IntoIterator<Item = Topic>>(iter: I) -> Self {
        iter.into_iter().fold(Self::EMPTY, Self::with)
    }
}

impl From<Topic> for TopicSet {
    fn from(topic: Topic) -> Self {
        Self::EMPTY.with(topic)
    }
}

impl std::ops::BitOr for TopicSet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for TopicSet {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}
