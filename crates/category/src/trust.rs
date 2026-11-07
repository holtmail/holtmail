use serde::{Deserialize, Serialize};

/// How far a message's sender is verified, computed in stage B (`docs/INSIGHTS.md` §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TrustState {
    /// A trusted `Authentication-Results` field records a DKIM pass whose registrable domain
    /// equals the From address's, and a stored signature matches it without `l=`.
    Aligned {
        /// The signing domain.
        domain: String,
    },
    /// An alias service relayed the message and signed it: it vouches for delivery, not for the
    /// original sender.
    Relayed {
        /// The alias service.
        provider: String,
    },
    /// The message could be checked and was not aligned, or an unsigned alias relayed it.
    Unverified,
    /// Nothing trusted to read: no trusted authserv-id, or no field from one.
    Unknown,
}

impl TrustState {
    /// The snake_case name of the state. Stored, so it never changes.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Aligned { .. } => "aligned",
            Self::Relayed { .. } => "relayed",
            Self::Unverified => "unverified",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the state is [`TrustState::Aligned`], the only state that allows sensitive topics.
    pub const fn is_aligned(&self) -> bool {
        matches!(self, Self::Aligned { .. })
    }
}
