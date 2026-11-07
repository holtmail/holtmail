//! A [`Categorizer`] that looks up the sender's domain in category tables: the bundled CC0
//! Wikidata table and tables handed to it later through [`DomainsCategorizer::reload`].
//!
//! It gives topics only, never a kind, and does not apply the trust gate: the runner does, so a
//! sensitive topic from a table counts only with aligned trust. It never votes on the archive's
//! own mail or on an inline forward, whose sender is a person. The design is
//! `docs/INSIGHTS.md` §4.2 and §8.
//!
//! It also checks downloaded data releases ([`verify_manifest`], [`check_file`]): the signed
//! manifest against the compiled-in keys, then each file against the manifest.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, RwLock};

use holtmail_category::{
    Categorizer, CategoryError, CategoryInput, CategorySnapshot, CategorySource, CategoryVote,
    TopicSet,
};
use holtmail_category_data::{DataError, DomainTable};

mod manifest;

#[cfg(feature = "test-key")]
pub use manifest::TEST_KEY;
pub use manifest::{
    CURRENT_KEY, MAX_VALIDITY_SECONDS, NEXT_KEY, ReleaseKey, SIGNATURE_BYTES, UpdateError,
    check_file, keys, sha256_hex, verify_manifest, verify_stored,
};

/// The categorizer id.
pub const ID: &str = "domains";

/// The version of the lookup rule. It is bumped whenever the way a table is searched changes,
/// and is part of every snapshot version.
pub const VERSION: &str = "2";

/// The largest bundled file, in compressed bytes (2 MiB). A larger file fails the build.
pub const BUNDLED_MAX_BYTES: usize = 2 * 1024 * 1024;

const BUNDLED: &[u8] = include_bytes!("../data/wikidata-categories.tsv.gz");

const _: () = assert!(BUNDLED.len() <= BUNDLED_MAX_BYTES);

/// The bundled CC0 table, `wikidata`, parsed from the file compiled into the binary.
///
/// Deterministic: every call gives an equal table. Does no I/O. A file that does not parse is
/// [`CategoryError::Data`] naming the broken rule.
pub fn bundled() -> Result<DomainTable, CategoryError> {
    holtmail_category_data::parse(BUNDLED).map_err(data_error)
}

fn data_error(error: DataError) -> CategoryError {
    CategoryError::Data(match error {
        DataError::Gzip => "gzip",
        DataError::TooLarge => "too large",
        DataError::TooManyRows => "too many rows",
        DataError::Header(rule) | DataError::Line { rule, .. } => rule,
        _ => "unreadable table",
    })
}

/// What one loaded table is, for display: no rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableInfo {
    /// The table's name from its header, such as `wikidata`.
    pub name: String,
    /// The table's version from its header.
    pub version: String,
    /// The rows kept, each with at least one known topic.
    pub rows: usize,
    /// The lowercase hex BLAKE3 hash of the decompressed file.
    pub hash: String,
}

/// The domain categorizer (id [`ID`]).
///
/// It votes the topics the tables give the sender's domain, and never votes when the message is
/// the archive's own mail or an inline forward.
///
/// It holds one set of tables with distinct names. [`Categorizer::snapshot`] gives a view of the
/// current set; [`DomainsCategorizer::reload`] replaces the set atomically, and snapshots taken
/// before it keep the old set. Safe to share between threads; a panic in another thread never
/// leaves it unusable.
pub struct DomainsCategorizer {
    current: RwLock<Arc<DomainsSnapshot>>,
}

impl DomainsCategorizer {
    /// A categorizer over `tables`, in any order. Refuses two tables with the same name as
    /// [`CategoryError::Data`]. An empty set is allowed and never votes.
    pub fn new(tables: Vec<DomainTable>) -> Result<Self, CategoryError> {
        Ok(Self {
            current: RwLock::new(Arc::new(DomainsSnapshot::new(tables)?)),
        })
    }

    /// A categorizer over the [`bundled`] table alone.
    pub fn with_bundled() -> Result<Self, CategoryError> {
        Self::new(vec![bundled()?])
    }

    /// Replaces the whole set of tables at once. Snapshots taken afterwards see only `tables`;
    /// snapshots taken before keep the old set. On error (two tables with the same name) the old
    /// set stays. Only the composition root calls it.
    pub fn reload(&self, tables: Vec<DomainTable>) -> Result<(), CategoryError> {
        let next = Arc::new(DomainsSnapshot::new(tables)?);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    /// The loaded tables, sorted by name.
    pub fn tables(&self) -> Vec<TableInfo> {
        self.current()
            .tables
            .iter()
            .map(|table| TableInfo {
                name: table.name().to_owned(),
                version: table.version().to_owned(),
                rows: table.len(),
                hash: table.hash().to_owned(),
            })
            .collect()
    }

    fn current(&self) -> Arc<DomainsSnapshot> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl fmt::Debug for DomainsCategorizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DomainsCategorizer")
            .field("version", &self.current().version)
            .finish()
    }
}

impl Categorizer for DomainsCategorizer {
    fn id(&self) -> &str {
        ID
    }

    fn snapshot(&self) -> Arc<dyn CategorySnapshot> {
        self.current()
    }
}

struct DomainsSnapshot {
    version: String,
    tables: Vec<DomainTable>,
}

impl DomainsSnapshot {
    fn new(mut tables: Vec<DomainTable>) -> Result<Self, CategoryError> {
        tables.sort_by(|a, b| a.name().cmp(b.name()));
        let names: BTreeSet<&str> = tables.iter().map(DomainTable::name).collect();
        if names.len() != tables.len() {
            return Err(CategoryError::Data("duplicate table name"));
        }
        let mut version = format!("{ID}-{VERSION}");
        if tables.is_empty() {
            version.push_str(":none");
        }
        for table in &tables {
            version.push_str(&format!(
                ":{}@{}#{}",
                table.name(),
                table.version(),
                table.hash()
            ));
        }
        Ok(Self { version, tables })
    }
}

impl CategorySnapshot for DomainsSnapshot {
    fn version(&self) -> String {
        self.version.clone()
    }

    fn categorize(&self, input: &CategoryInput) -> Option<CategoryVote> {
        if input.own || input.forwarded {
            return None;
        }
        let host = input
            .sender_address
            .as_deref()
            .and_then(|address| address.rsplit_once('@'))
            .map(|(_, host)| host);
        let registrable = input.sender_domain.as_deref();
        let topics = self
            .tables
            .iter()
            .filter_map(|table| table.lookup(host, registrable))
            .fold(TopicSet::EMPTY, |all, topics| all | topics)
            .known();
        (!topics.is_empty()).then_some(CategoryVote {
            kind: None,
            kind_source: CategorySource::Domain,
            topics,
            source: CategorySource::Domain,
        })
    }
}

#[cfg(test)]
mod tests;
