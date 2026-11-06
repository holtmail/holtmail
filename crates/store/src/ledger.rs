use async_trait::async_trait;
use holtmail_core::MessageId;
pub use holtmail_ledger::{LedgerEntry, LedgerEvent, LedgerHash, NewEntry};

use crate::StoreError;

/// Where the live messages and the ledger disagree, as found by [`LedgerStore::ledger_gaps`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerGaps {
    /// Live messages without an `archived` entry, the first ones by id.
    pub unentered: Vec<MessageId>,
    /// How many live messages have no `archived` entry.
    pub unentered_count: u64,
    /// Live messages whose `archived` entry names other bytes or another archive.
    pub changed: Vec<MessageId>,
    /// How many there are.
    pub changed_count: u64,
    /// Messages entered as archived that are gone without a `deleted` entry for them or a later
    /// `archive_deleted` entry for their archive.
    pub vanished: Vec<MessageId>,
    /// How many there are.
    pub vanished_count: u64,
}

impl LedgerGaps {
    /// Nothing disagrees.
    pub fn is_empty(&self) -> bool {
        self.unentered_count == 0 && self.changed_count == 0 && self.vanished_count == 0
    }
}

/// The archive ledger: a hash chain of every message archived and deleted, written in the same
/// transaction as the change it records, so the chain and the messages never disagree unless
/// someone changed the database by hand.
///
/// Every new message ([`crate::MessageStore::insert_message`] returning `created`) gets an
/// `archived` entry, every deleted archive an `archive_deleted` entry and every deleted message
/// a `deleted` entry. Entries are numbered from 1 without gaps and each links to the hash of the
/// one before; entries are never changed or removed, not even when their archive is deleted.
#[async_trait]
pub trait LedgerStore: Send + Sync {
    /// The newest entry; `None` while the ledger is empty.
    async fn ledger_head(&self) -> Result<Option<LedgerEntry>, StoreError>;

    /// Up to `limit` entries after position `after` (0 for the first), in order, exactly as
    /// stored, so a changed entry reads back changed.
    async fn ledger_entries(&self, after: u64, limit: u32) -> Result<Vec<LedgerEntry>, StoreError>;

    /// Enters up to `limit` live messages that have no `archived` entry, such as those archived
    /// before the ledger existed, oldest archived first, each as an `archived` entry made now.
    /// Returns how many were entered; 0 when none are left.
    async fn enter_unentered(&self, limit: u32) -> Result<u64, StoreError>;

    /// Compares the live messages with the entries, listing at most `limit` messages of each
    /// kind.
    async fn ledger_gaps(&self, limit: u32) -> Result<LedgerGaps, StoreError>;
}
