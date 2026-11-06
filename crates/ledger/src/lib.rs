//! The archive ledger: one entry for every message archived, every message deleted and every
//! archive deleted, each sealed with a BLAKE3 hash over its fields and the hash of the entry
//! before it.
//!
//! Changing, removing or reordering any entry changes every hash after it, so a head hash
//! written down elsewhere ([`LedgerEntry::hash`] of the newest entry) proves that the history up
//! to it is unchanged. Entries carry ids and hashes, never mail content or addresses.

use std::fmt;
use std::str::FromStr;

use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, MessageId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

const DOMAIN: &[u8] = b"holtmail ledger v1\n";

/// A BLAKE3 hash in the ledger; [`LedgerHash::ZERO`] stands before the first entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LedgerHash([u8; 32]);

impl LedgerHash {
    /// What the first entry links to.
    pub const ZERO: Self = Self([0; 32]);

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for LedgerHash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for LedgerHash {
    /// Lowercase hex, 64 characters.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for LedgerHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LedgerHash({self})")
    }
}

/// A hash that is not 64 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a ledger hash is 64 hex digits")]
pub struct BadHash;

impl FromStr for LedgerHash {
    type Err = BadHash;

    /// Reads 64 hex digits in either case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() != 64 || !s.is_ascii() {
            return Err(BadHash);
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| BadHash)?;
        }
        Ok(Self(out))
    }
}

impl Serialize for LedgerHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LedgerHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// What an entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerEvent {
    /// A message was archived, or was already archived when the ledger began.
    Archived,
    /// A message was deleted on purpose.
    Deleted,
    /// An archive was deleted with every message in it.
    ArchiveDeleted,
}

impl LedgerEvent {
    /// The stable name of the event.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Archived => "archived",
            Self::Deleted => "deleted",
            Self::ArchiveDeleted => "archive_deleted",
        }
    }

    /// The event named `name`, as [`LedgerEvent::as_str`] writes it.
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Archived, Self::Deleted, Self::ArchiveDeleted]
            .into_iter()
            .find(|e| e.as_str() == name)
    }

    fn tag(self) -> u8 {
        match self {
            Self::Archived => 1,
            Self::Deleted => 2,
            Self::ArchiveDeleted => 3,
        }
    }
}

/// An entry before it has its place in the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    /// When it happened.
    pub at: Timestamp,
    /// What happened.
    pub event: LedgerEvent,
    /// The archive it happened in.
    pub archive_id: ArchiveId,
    /// The message; `None` only for [`LedgerEvent::ArchiveDeleted`].
    pub message_id: Option<MessageId>,
    /// The message's raw bytes; `None` only for [`LedgerEvent::ArchiveDeleted`].
    pub blob_id: Option<BlobId>,
    /// Who did it, for deletions done by a person.
    pub actor: Option<UserId>,
}

impl NewEntry {
    /// The entry at position `seq` after the entry hashed `prev`, sealed with its hash.
    pub fn seal(self, seq: u64, prev: LedgerHash) -> LedgerEntry {
        let mut entry = LedgerEntry {
            seq,
            at: self.at,
            event: self.event,
            archive_id: self.archive_id,
            message_id: self.message_id,
            blob_id: self.blob_id,
            actor: self.actor,
            prev,
            hash: LedgerHash::ZERO,
        };
        entry.hash = entry.expected_hash();
        entry
    }
}

/// One entry of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Its position: 1 for the first entry, one more for each after it.
    pub seq: u64,
    /// When it happened.
    pub at: Timestamp,
    /// What happened.
    pub event: LedgerEvent,
    /// The archive it happened in.
    pub archive_id: ArchiveId,
    /// The message, for message events.
    pub message_id: Option<MessageId>,
    /// The message's raw bytes, for message events.
    pub blob_id: Option<BlobId>,
    /// Who did it, for deletions done by a person.
    pub actor: Option<UserId>,
    /// The hash of the entry before; [`LedgerHash::ZERO`] for the first.
    pub prev: LedgerHash,
    /// BLAKE3 over a fixed encoding of every field above.
    pub hash: LedgerHash,
}

impl LedgerEntry {
    /// The hash the entry's fields give; equal to [`LedgerEntry::hash`] unless it was changed.
    pub fn expected_hash(&self) -> LedgerHash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DOMAIN);
        hasher.update(&self.seq.to_be_bytes());
        hasher.update(&self.at.as_millis().to_be_bytes());
        hasher.update(&[self.event.tag()]);
        hasher.update(self.archive_id.as_uuid().as_bytes());
        optional(
            &mut hasher,
            self.message_id.map(|m| *m.as_uuid().as_bytes()),
        );
        optional(&mut hasher, self.blob_id.map(|b| *b.as_bytes()));
        optional(&mut hasher, self.actor.map(|u| *u.as_uuid().as_bytes()));
        hasher.update(self.prev.as_bytes());
        LedgerHash(*hasher.finalize().as_bytes())
    }
}

fn optional<const N: usize>(hasher: &mut blake3::Hasher, value: Option<[u8; N]>) {
    match value {
        Some(bytes) => {
            hasher.update(&[1]);
            hasher.update(&bytes);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

/// Something wrong in the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChainProblem {
    /// An entry is missing or out of place: `expected` came next, `found` was there.
    Gap {
        /// The position that should have come.
        expected: u64,
        /// The position that came.
        found: u64,
    },
    /// The entry does not link to the hash of the one before it.
    BrokenLink {
        /// The entry.
        seq: u64,
    },
    /// The entry's fields do not give its hash: it was changed after it was written.
    Altered {
        /// The entry.
        seq: u64,
    },
    /// The entry at the anchor's position has another hash than the one written down.
    AnchorMismatch {
        /// The anchor's position.
        seq: u64,
    },
    /// The chain ends before the anchor's position.
    AnchorMissing {
        /// The anchor's position.
        seq: u64,
    },
}

/// Checks entries one after another, in order, keeping only the last hash.
#[derive(Debug, Clone)]
pub struct ChainCheck {
    next: u64,
    prev: LedgerHash,
    anchor: Option<(u64, LedgerHash)>,
    anchor_seen: bool,
    problems: Vec<ChainProblem>,
    problem_count: u64,
    entries: u64,
    head: Option<(u64, LedgerHash)>,
}

/// Problems kept by a [`ChainCheck`]; any after these are only counted.
pub const MAX_PROBLEMS: usize = 100;

/// What a [`ChainCheck`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainReport {
    /// Entries looked at.
    pub entries: u64,
    /// The last entry's position and hash.
    pub head: Option<(u64, LedgerHash)>,
    /// The first [`MAX_PROBLEMS`] problems, in chain order.
    pub problems: Vec<ChainProblem>,
    /// Every problem, also those not kept.
    pub problem_count: u64,
}

impl Default for ChainCheck {
    fn default() -> Self {
        Self::new()
    }
}

impl ChainCheck {
    /// A check of a chain from its first entry.
    pub fn new() -> Self {
        Self {
            next: 1,
            prev: LedgerHash::ZERO,
            anchor: None,
            anchor_seen: false,
            problems: Vec::new(),
            problem_count: 0,
            entries: 0,
            head: None,
        }
    }

    /// The same, also checking that the entry at `seq` has `hash`, as written down earlier.
    pub fn with_anchor(mut self, seq: u64, hash: LedgerHash) -> Self {
        self.anchor = Some((seq, hash));
        self
    }

    fn problem(&mut self, problem: ChainProblem) {
        self.problem_count += 1;
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(problem);
        }
    }

    /// Takes the next entry. Entries must come in the order they are stored.
    pub fn push(&mut self, entry: &LedgerEntry) {
        if entry.seq != self.next {
            self.problem(ChainProblem::Gap {
                expected: self.next,
                found: entry.seq,
            });
        }
        if entry.prev != self.prev {
            self.problem(ChainProblem::BrokenLink { seq: entry.seq });
        }
        if entry.expected_hash() != entry.hash {
            self.problem(ChainProblem::Altered { seq: entry.seq });
        }
        if let Some((seq, hash)) = self.anchor
            && seq == entry.seq
        {
            self.anchor_seen = true;
            if hash != entry.hash {
                self.problem(ChainProblem::AnchorMismatch { seq });
            }
        }
        self.entries += 1;
        self.next = entry.seq + 1;
        self.prev = entry.hash;
        self.head = Some((entry.seq, entry.hash));
    }

    /// What was found, after the last entry.
    pub fn finish(mut self) -> ChainReport {
        if let Some((seq, _)) = self.anchor
            && !self.anchor_seen
        {
            self.problem(ChainProblem::AnchorMissing { seq });
        }
        ChainReport {
            entries: self.entries,
            head: self.head,
            problem_count: self.problem_count,
            problems: self.problems,
        }
    }
}

#[cfg(test)]
mod tests;
