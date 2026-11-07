use std::borrow::Cow;

use holtmail_category::TopicSet;

use crate::Header;

/// A loaded file: domains in byte order with their known topics, searched by binary search.
///
/// Immutable once built. It keeps no `source` column, so its memory is the keys and one
/// [`TopicSet`] per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainTable {
    header: Header,
    rows: Vec<(Box<str>, TopicSet)>,
    skipped: usize,
    hash: String,
}

impl DomainTable {
    pub(crate) fn new(
        header: Header,
        rows: Vec<(Box<str>, TopicSet)>,
        skipped: usize,
        hash: String,
    ) -> Self {
        Self {
            header,
            rows,
            skipped,
            hash,
        }
    }

    /// The file's header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The table's name from the header, such as `wikidata`.
    pub fn name(&self) -> &str {
        &self.header.name
    }

    /// The table's version from the header.
    pub fn version(&self) -> &str {
        &self.header.version
    }

    /// The rows kept, each with at least one known topic.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether no row was kept. A header-only file gives an empty table.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The rows dropped because none of their topics is known to this build.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// The lowercase hex BLAKE3 hash of the decompressed file. Equal for files with equal
    /// content, whatever their compression, and different for different content.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// The known topics of exactly this key, compared byte for byte. `None` when the key is
    /// not in the table.
    pub fn get(&self, domain: &str) -> Option<TopicSet> {
        self.rows
            .binary_search_by(|(key, _)| key.as_bytes().cmp(domain.as_bytes()))
            .ok()
            .map(|index| self.rows[index].1)
    }

    /// The topics for a sender: the exact `host` first, then its `registrable` domain.
    ///
    /// Both inputs are compared ASCII-case-insensitively; no suffix logic is applied here, so
    /// the caller passes the registrable domain it computed. `None` when neither matches.
    pub fn lookup(&self, host: Option<&str>, registrable: Option<&str>) -> Option<TopicSet> {
        [host, registrable]
            .into_iter()
            .flatten()
            .find_map(|key| self.get(&lowercase(key)))
    }
}

fn lowercase(key: &str) -> Cow<'_, str> {
    if key.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(key.to_ascii_lowercase())
    } else {
        Cow::Borrowed(key)
    }
}
