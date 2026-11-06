//! Ids, timestamps and TLS roots shared by Holtmail modules.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod tls;

pub use tls::{InvalidRoots, TlsRoots};

/// A string that is not a valid id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a valid id")]
pub struct InvalidId;

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// A UUIDv7: ids created later sort after ids created earlier. Displays, parses and
        /// serializes as the hyphenated lowercase form.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
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

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0.hyphenated())
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::try_parse(s).map(Self).map_err(|_| InvalidId)
            }
        }
    };
}

id!(
    /// An archive: the unit of access, holding one or more accounts.
    ArchiveId
);
id!(
    /// A connected mail account or an import-only source inside an archive.
    AccountId
);
id!(
    /// A folder (IMAP mailbox, Graph folder, JMAP mailbox) of an account.
    FolderId
);
id!(
    /// A message in an archive. The same bytes in two archives are two messages.
    MessageId
);
id!(
    /// A conversation in an archive.
    ThreadId
);
id!(
    /// A person who signs in to Holtmail.
    UserId
);
id!(
    /// An address book or a calendar in an archive.
    CollectionId
);
id!(
    /// A contact, event or task in a collection, with every version of it.
    ItemId
);
id!(
    /// A split or join of people made by hand in an archive.
    PersonOverrideId
);

/// A person derived from contacts and mail in an archive.
///
/// Not created but computed: the same group of contacts and addresses always gives the same id,
/// so it survives a rebuild of the people. Displays, parses and serializes as a hyphenated
/// lowercase UUID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PersonId(Uuid);

impl PersonId {
    /// The id made from 16 bytes of a hash, as a UUIDv8.
    pub fn from_hash(bytes: [u8; 16]) -> Self {
        Self(Uuid::new_v8(bytes))
    }

    /// The id as a UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for PersonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for PersonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PersonId({})", self.0.hyphenated())
    }
}

impl FromStr for PersonId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s).map(Self).map_err(|_| InvalidId)
    }
}

/// A group of copies of one calendar event in an archive: every event item with the same
/// iCalendar `UID`, across calendars and the events found in mail.
///
/// Not created but computed: the same archive and `UID` always give the same id. Displays,
/// parses and serializes as a hyphenated lowercase UUID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventGroupId(Uuid);

impl EventGroupId {
    /// The id made from 16 bytes of a hash, as a UUIDv8.
    pub fn from_hash(bytes: [u8; 16]) -> Self {
        Self(Uuid::new_v8(bytes))
    }
}

impl fmt::Display for EventGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for EventGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EventGroupId({})", self.0.hyphenated())
    }
}

impl FromStr for EventGroupId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::try_parse(s).map(Self).map_err(|_| InvalidId)
    }
}

/// One mailbox from an address header: `"Ada Lovelace" <ada@example.com>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    /// Display name, decoded.
    pub name: Option<String>,
    /// The address as written, e.g. `ada@example.com`.
    pub email: String,
}

/// A point in time: milliseconds since the Unix epoch, UTC.
///
/// Serializes as a plain integer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The current time.
    pub fn now() -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
    }

    /// The time `millis` milliseconds after the epoch.
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The time `secs` seconds after the epoch.
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(1000))
    }

    /// Milliseconds since the epoch.
    pub const fn as_millis(&self) -> i64 {
        self.0
    }

    /// Whole seconds since the epoch, rounded down.
    pub const fn as_secs(&self) -> i64 {
        self.0.div_euclid(1000)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_by_creation() {
        let first = MessageId::new();
        let second = MessageId::new();
        assert!(first < second);
    }

    #[test]
    fn ids_round_trip_through_text_and_json() {
        let id = ArchiveId::new();
        assert_eq!(id.to_string().parse::<ArchiveId>(), Ok(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<ArchiveId>(&json).unwrap(), id);
        assert_eq!("nope".parse::<ArchiveId>(), Err(InvalidId));
    }

    #[test]
    fn person_ids_are_the_same_for_the_same_hash() {
        let id = PersonId::from_hash([7; 16]);
        assert_eq!(id, PersonId::from_hash([7; 16]));
        assert_ne!(id, PersonId::from_hash([8; 16]));
        assert_eq!(id.to_string().parse::<PersonId>(), Ok(id));
    }

    #[test]
    fn timestamps_convert_between_units() {
        assert_eq!(
            Timestamp::from_secs(1_741_941_000).as_millis(),
            1_741_941_000_000
        );
        assert_eq!(Timestamp::from_millis(-1).as_secs(), -1);
        assert_eq!(
            serde_json::to_string(&Timestamp::from_millis(5)).unwrap(),
            "5"
        );
        assert!(Timestamp::now() > Timestamp::from_secs(1_700_000_000));
    }
}
