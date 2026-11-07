use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{KindTags, MessageKind, ReservationKinds, Signals, TopicSet, TrustState};

/// What a categorizer reads about one message.
///
/// It holds no body. Every field is a stored column, or is computed in stage B from stored
/// columns, the stored alias row, the archive's own addresses and the trust lists, so categories
/// can be reproduced from stored rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryInput {
    /// The stored header signals.
    pub signals: Signals,
    /// The stored item kinds: kinds at `Likely` or higher that are not forwarded.
    pub kinds: KindTags,
    /// The reservation types among the same items as `kinds`.
    pub reservations: ReservationKinds,
    /// A non-forwarded `Certain` item from JSON-LD, microdata or a pkpass is present.
    pub structured: bool,
    /// The message is an inline forward.
    pub forwarded: bool,
    /// The From address is one of the archive's own addresses.
    pub own: bool,
    /// The message's trust.
    pub trust: TrustState,
    /// The sender used for lookups: the alias's claimed real sender on an alias hit, else the
    /// From address. Lowercase with an A-label domain.
    pub sender_address: Option<String>,
    /// The registrable domain of `sender_address`.
    pub sender_domain: Option<String>,
}

/// The layer a vote, or one axis of it, comes from. Precedence is applied per axis by
/// [`CategorySource::rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySource {
    /// The user's override.
    Override,
    /// The message's content: its insight items.
    Content,
    /// A domain table.
    Domain,
    /// The header signals.
    Headers,
}

impl CategorySource {
    /// Every source, highest rank first.
    pub const ALL: &'static [Self] = &[Self::Override, Self::Content, Self::Domain, Self::Headers];

    /// The snake_case name. Stored, so it never changes.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Content => "content",
            Self::Domain => "domain",
            Self::Headers => "headers",
        }
    }

    /// The source with this exact name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.name() == name)
    }

    /// The precedence of `docs/INSIGHTS.md` §4.2: a higher rank wins. Override 3, content 2,
    /// domain 1, headers 0.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Override => 3,
            Self::Content => 2,
            Self::Domain => 1,
            Self::Headers => 0,
        }
    }
}

impl fmt::Display for CategorySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One categorizer's answer for one message.
///
/// One categorizer may speak for two layers, so the kind and the topics each carry their source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CategoryVote {
    /// The kind, or `None` when the categorizer has no opinion on it.
    pub kind: Option<MessageKind>,
    /// The layer the kind comes from. Meaningless when `kind` is `None`.
    pub kind_source: CategorySource,
    /// The topics, before the trust gate.
    pub topics: TopicSet,
    /// The layer the topics come from.
    pub source: CategorySource,
}
