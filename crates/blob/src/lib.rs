//! Contract for content-addressed storage of raw message bytes.
//!
//! Every message Holtmail archives is kept as its original bytes in a [`BlobStore`], addressed by
//! the BLAKE3 hash of those bytes. The same message seen in two folders or two accounts is stored
//! once.

use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use bytes::Bytes;

#[cfg(feature = "testing")]
pub mod testing;

/// The address of a blob: the BLAKE3 hash of its bytes.
///
/// Two blobs have the same id exactly when their bytes are equal. Displays, parses and serializes
/// as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId([u8; 32]);

impl BlobId {
    /// The id of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// The raw 32 hash bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for BlobId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(blake3::Hash::from_bytes(self.0).to_hex().as_str())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobId({self})")
    }
}

impl FromStr for BlobId {
    type Err = InvalidBlobId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 || s.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(InvalidBlobId);
        }
        blake3::Hash::from_hex(s)
            .map(|hash| Self(*hash.as_bytes()))
            .map_err(|_| InvalidBlobId)
    }
}

impl serde::Serialize for BlobId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for BlobId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        hex.parse().map_err(serde::de::Error::custom)
    }
}

/// A string that is not 64 lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a blob id is 64 lowercase hex characters")]
pub struct InvalidBlobId;

/// Why a [`BlobStore`] operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// The stored bytes no longer hash to their id.
    #[error("blob {0} is corrupt")]
    Corrupt(BlobId),
    /// The backend failed; the operation may succeed if retried.
    #[error("blob backend failed: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Storage of immutable blobs addressed by [`BlobId`].
///
/// Implementations are safe to share between tasks and must tolerate concurrent calls, including
/// concurrent `put` of the same bytes.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Stores `bytes` and returns `BlobId::of(&bytes)`.
    ///
    /// Idempotent: storing bytes that are already present succeeds and changes nothing. Once it
    /// returns, the blob survives a process crash.
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError>;

    /// The exact bytes stored under `id`, or `None` when there is no such blob.
    ///
    /// Never returns bytes that do not hash to `id`; it fails with [`BlobError::Corrupt`] instead.
    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError>;

    /// Whether a blob is stored under `id`.
    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError>;

    /// Removes the blob under `id`. Removing a missing blob succeeds.
    async fn delete(&self, id: &BlobId) -> Result<(), BlobError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_round_trips_through_hex() {
        let id = BlobId::of(b"Subject: Hello\r\n\r\nHi\r\n");
        let hex = id.to_string();
        assert_eq!(hex.len(), 64);
        assert_eq!(hex.parse::<BlobId>(), Ok(id));
    }

    #[test]
    fn id_rejects_malformed_hex() {
        assert_eq!("abc".parse::<BlobId>(), Err(InvalidBlobId));
        assert_eq!("Z".repeat(64).parse::<BlobId>(), Err(InvalidBlobId));
        let upper = BlobId::of(b"x").to_string().to_uppercase();
        assert_eq!(upper.parse::<BlobId>(), Err(InvalidBlobId));
    }
}
