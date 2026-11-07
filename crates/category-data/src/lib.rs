//! The category data file format (`docs/INSIGHTS.md` §8): a sorted, gzip-compressed TSV of
//! registrable domains and their topics, with bounded loading into a [`DomainTable`], and the
//! release manifest that lists the published files ([`Manifest`]).
//!
//! Plain library: no I/O beyond the byte slices it is given, no network, no logging. Every
//! function is deterministic, takes any input and never panics.

#![forbid(unsafe_code)]

mod error;
mod format;
mod manifest;
mod table;

pub use error::DataError;
pub use format::{parse, parse_with, read, write};
pub use manifest::{
    DATA_FILES, DataFile, FILE_NAMES, MANIFEST_FORMAT, MANIFEST_NAME, MAX_FILE_BYTES,
    MAX_MANIFEST_BYTES, Manifest, ManifestError, ManifestFile, SIGNATURE_NAME, data_file,
    data_file_for_table, is_newer, parse_version,
};
pub use table::DomainTable;

use holtmail_category::TopicSet;

/// The format number in the header line. A file with another number is refused.
pub const FORMAT: u32 = 1;

/// The largest decompressed file loading accepts, in bytes (32 MiB).
pub const MAX_DECOMPRESSED_BYTES: u64 = 32 * 1024 * 1024;

/// The most data rows loading accepts, rows dropped for unknown topics included.
pub const MAX_ROWS: usize = 500_000;

/// The longest `source` column, in bytes.
pub const MAX_SOURCE_BYTES: usize = 64;

/// The first line of a file: `#holtmail-categories 1 <name> <version>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Header {
    /// The table's name, `[a-z0-9-]{1,32}`, such as `wikidata`.
    pub name: String,
    /// The table's version, `[A-Za-z0-9.-]{1,32}`.
    pub version: String,
}

impl Header {
    /// A header with this name and version. It is checked only when written or parsed.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// One data row: `domain\ttopic[,topic]\tsource`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Row {
    /// The key, a lowercase A-label domain accepted by [`is_valid_domain`].
    pub domain: String,
    /// The topics; written by name in bit order. Never empty in a valid file.
    pub topics: TopicSet,
    /// Where the row comes from, such as `wikidata:Q42`: non-empty, at most
    /// [`MAX_SOURCE_BYTES`] bytes, no control characters.
    pub source: String,
}

/// The caps applied while loading. [`Limits::default`] is [`MAX_DECOMPRESSED_BYTES`] and
/// [`MAX_ROWS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest decompressed size accepted, in bytes. One byte more is refused as
    /// [`DataError::TooLarge`] without decompressing further.
    pub max_bytes: u64,
    /// The most data rows accepted; one more is refused as [`DataError::TooManyRows`].
    pub max_rows: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_DECOMPRESSED_BYTES,
            max_rows: MAX_ROWS,
        }
    }
}

/// Whether `domain` is a valid key: a lowercase ASCII A-label host of at least two labels,
/// each label 1 to 63 bytes of `[a-z0-9-]` without a leading or trailing hyphen, at most 253
/// bytes, no trailing dot, and a last label that is not all digits (so no IPv4 literal).
pub fn is_valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    let mut labels = 0usize;
    let mut last = "";
    for label in domain.split('.') {
        let bytes = label.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 63
            || bytes[0] == b'-'
            || bytes[bytes.len() - 1] == b'-'
            || !bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        {
            return false;
        }
        labels += 1;
        last = label;
    }
    labels >= 2 && !last.bytes().all(|b| b.is_ascii_digit())
}

fn is_valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_valid_version(version: &str) -> bool {
    (1..=32).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn is_valid_source(source: &str) -> bool {
    (1..=MAX_SOURCE_BYTES).contains(&source.len()) && !source.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains() {
        for ok in [
            "example.com",
            "shop.example.org",
            "a-b.example.com",
            "x1.example.com",
            "example.co",
        ] {
            assert!(is_valid_domain(ok), "{ok}");
        }
        let long_label = format!("{}.example.com", "a".repeat(64));
        let too_long = format!("{}example.com", "abcdefghi.".repeat(25));
        for bad in [
            "",
            "com",
            "Example.com",
            "example.com.",
            ".example.com",
            "ex..ample.com",
            "-shop.example.com",
            "shop-.example.com",
            "shop_x.example.com",
            "bücher.example.com",
            "192.0.2.1",
            "shop example.com",
            long_label.as_str(),
            too_long.as_str(),
        ] {
            assert!(!is_valid_domain(bad), "{bad}");
        }
        assert!(is_valid_domain(&format!("{}.example.com", "a".repeat(63))));
    }

    #[test]
    fn header_fields() {
        assert!(is_valid_name("wikidata"));
        assert!(is_valid_name("ut1-x"));
        assert!(!is_valid_name("Wikidata"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name(&"a".repeat(33)));
        assert!(is_valid_version("2026.11.07-1"));
        assert!(!is_valid_version("1 2"));
        assert!(!is_valid_version(""));
    }

    #[test]
    fn sources() {
        assert!(is_valid_source("wikidata:Q42"));
        assert!(is_valid_source(&"q".repeat(MAX_SOURCE_BYTES)));
        assert!(!is_valid_source(&"q".repeat(MAX_SOURCE_BYTES + 1)));
        assert!(!is_valid_source(""));
        assert!(!is_valid_source("a\tb"));
        assert!(!is_valid_source("a\nb"));
    }
}
