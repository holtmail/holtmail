use std::fmt;

use serde::{Deserialize, Serialize};

/// Who sent a message and how. Exactly one per message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Mail a person wrote, including the archive's own mail and out-of-office replies.
    Personal,
    /// A discussion list that members post to.
    List,
    /// Mail sent in bulk: newsletters and marketing.
    Bulk,
    /// Machine-sent mail about one event: receipts, alerts, codes.
    Notification,
}

impl MessageKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Personal, Self::List, Self::Bulk, Self::Notification];

    /// The snake_case name. Stored, so it never changes.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::List => "list",
            Self::Bulk => "bulk",
            Self::Notification => "notification",
        }
    }

    /// The kind with this exact name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.name() == name)
    }
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
