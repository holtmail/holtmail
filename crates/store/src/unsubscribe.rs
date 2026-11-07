use std::fmt;
use std::str::FromStr;

use holtmail_core::{ArchiveId, InvalidId, MessageId, Timestamp, UserId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One recorded one-click unsubscribe request.
///
/// A UUIDv7: ids created later sort after ids created earlier. Displays, parses and serializes as
/// the hyphenated lowercase form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnsubscribeRequestId(Uuid);

impl UnsubscribeRequestId {
    /// A new id, unique and later than every id this process created before.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// The id as a UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for UnsubscribeRequestId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl fmt::Display for UnsubscribeRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for UnsubscribeRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UnsubscribeRequestId({})", self.0.hyphenated())
    }
}

impl FromStr for UnsubscribeRequestId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s).map(Self).map_err(|_| InvalidId)
    }
}

/// How a one-click unsubscribe request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnsubscribeOutcome {
    /// The list answered with a 2xx status.
    Accepted,
    /// The list answered with a redirect, which was not followed.
    Redirected,
    /// The request was not sent: the target was not allowed.
    Refused,
    /// The list answered with another status.
    HttpError,
    /// No answer: connecting, TLS or the timeout failed.
    Network,
}

impl UnsubscribeOutcome {
    /// The stable lowercase name: `accepted`, `redirected`, `refused`, `http_error` or `network`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Redirected => "redirected",
            Self::Refused => "refused",
            Self::HttpError => "http_error",
            Self::Network => "network",
        }
    }

    /// The outcome named by [`UnsubscribeOutcome::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "accepted" => Self::Accepted,
            "redirected" => Self::Redirected,
            "refused" => Self::Refused,
            "http_error" => Self::HttpError,
            "network" => Self::Network,
            _ => return None,
        })
    }
}

/// The audit record of one one-click unsubscribe request, kept after its message is deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubscribeRequest {
    /// Unique among all requests.
    pub id: UnsubscribeRequestId,
    /// The archive the request was made in.
    pub archive: ArchiveId,
    /// The message it was made from; `None` once that message is deleted.
    pub message: Option<MessageId>,
    /// The list it unsubscribes from, compared byte for byte; the caller normalizes it.
    pub list_key: String,
    /// When it was made.
    pub requested_at: Timestamp,
    /// Who made it.
    pub requested_by: UserId,
    /// How it ended.
    pub outcome: UnsubscribeOutcome,
    /// The HTTP status, when the list answered.
    pub status: Option<u16>,
}
