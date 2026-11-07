use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::MAX_ROWS;

/// The manifest format number. A manifest with another number is refused.
pub const MANIFEST_FORMAT: u32 = 1;

/// The largest manifest accepted, in bytes (64 KiB).
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// The largest data file a manifest may list, in compressed bytes (16 MiB).
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The published name of the manifest.
pub const MANIFEST_NAME: &str = "manifest.json";

/// The published name of the detached signature: the 64 raw bytes of an ed25519 signature over
/// the exact bytes of [`MANIFEST_NAME`].
pub const SIGNATURE_NAME: &str = "manifest.json.sig";

/// The only data file names a manifest may list, in [`DATA_FILES`] order.
pub const FILE_NAMES: [&str; 2] = ["wikidata-categories.tsv.gz", "ut1-categories.tsv.gz"];

/// One published data file: its fixed file name, the table name in its header and its licence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataFile {
    /// The file name, one of [`FILE_NAMES`].
    pub file: &'static str,
    /// The table name its header must carry.
    pub table: &'static str,
    /// The SPDX identifier its manifest entry must carry.
    pub license: &'static str,
}

/// Every published data file. The set is closed: nothing else is ever accepted.
pub const DATA_FILES: [DataFile; 2] = [
    DataFile {
        file: FILE_NAMES[0],
        table: "wikidata",
        license: "CC0-1.0",
    },
    DataFile {
        file: FILE_NAMES[1],
        table: "ut1",
        license: "CC-BY-SA-4.0",
    },
];

/// The entry of [`DATA_FILES`] with this file name, compared exactly.
pub fn data_file(file: &str) -> Option<DataFile> {
    DATA_FILES.into_iter().find(|entry| entry.file == file)
}

/// The entry of [`DATA_FILES`] for this table name, compared exactly.
pub fn data_file_for_table(table: &str) -> Option<DataFile> {
    DATA_FILES.into_iter().find(|entry| entry.table == table)
}

/// A data version as a number: `Some` for 1 to 16 ASCII digits (leading zeros allowed),
/// `None` for anything else.
pub fn parse_version(version: &str) -> Option<u64> {
    ((1..=16).contains(&version.len()) && version.bytes().all(|b| b.is_ascii_digit()))
        .then(|| version.parse().ok())
        .flatten()
}

/// Whether `new` is strictly greater than `old` as numbers. False when either is not a
/// version accepted by [`parse_version`], so an unreadable version never counts as newer.
pub fn is_newer(new: &str, old: &str) -> bool {
    matches!((parse_version(new), parse_version(old)), (Some(n), Some(o)) if n > o)
}

/// Why a manifest was refused. The message names the rule only, never a field's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// The manifest is larger than [`MAX_MANIFEST_BYTES`].
    #[error("manifest is larger than the limit")]
    TooLarge,
    /// The bytes are not JSON of the manifest's shape: a field missing, of the wrong type or
    /// not known.
    #[error("manifest is not valid JSON of the expected shape")]
    Json,
    /// `format` is not [`MANIFEST_FORMAT`].
    #[error("manifest format is not supported")]
    Format,
    /// `version` is not 1 to 16 digits.
    #[error("manifest version is not 1 to 16 digits")]
    Version,
    /// `key_id` is not `[a-z0-9-]{1,32}`.
    #[error("manifest key id is malformed")]
    KeyId,
    /// `not_after` is not `YYYY-MM-DDTHH:MM:SSZ` with a four-digit year.
    #[error("manifest not_after is not an RFC 3339 UTC time")]
    NotAfter,
    /// `files` is empty or longer than [`FILE_NAMES`].
    #[error("manifest lists no files or too many")]
    Files,
    /// A file name is not one of [`FILE_NAMES`].
    #[error("manifest lists a file name outside the fixed set")]
    FileName,
    /// A file name is listed twice.
    #[error("manifest lists a file twice")]
    DuplicateFile,
    /// A `sha256` is not 64 lowercase hex characters.
    #[error("manifest file hash is malformed")]
    Sha256,
    /// A `size` is 0 or above [`MAX_FILE_BYTES`].
    #[error("manifest file size is out of range")]
    Size,
    /// A `license` is not the one [`DATA_FILES`] fixes for that file.
    #[error("manifest file licence does not match the file")]
    License,
    /// A `rows` count is above [`MAX_ROWS`].
    #[error("manifest file row count is out of range")]
    Rows,
}

/// The signed description of one data release.
///
/// [`Manifest::parse`] accepts only a manifest that passes [`Manifest::check`], and
/// `Manifest::parse(&m.to_json())` gives back `m` for every `m` that passes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Manifest {
    /// The manifest format, [`MANIFEST_FORMAT`].
    pub format: u32,
    /// The data version, 1 to 16 digits, compared as a number ([`is_newer`]). Every listed
    /// file's header carries the same version.
    pub version: String,
    /// The id of the key that signed it, `[a-z0-9-]{1,32}`.
    pub key_id: String,
    /// The last moment the manifest is valid for a fresh download, in Unix seconds, whole
    /// seconds in UTC. In JSON it is an RFC 3339 string `YYYY-MM-DDTHH:MM:SSZ`.
    pub not_after: i64,
    /// The listed files, each name at most once, in the order written.
    pub files: Vec<ManifestFile>,
}

/// One file listed in a [`Manifest`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    /// One of [`FILE_NAMES`].
    pub name: String,
    /// The SHA-256 of the compressed file, 64 lowercase hex characters.
    pub sha256: String,
    /// The size of the compressed file in bytes, 1 to [`MAX_FILE_BYTES`].
    pub size: u64,
    /// The SPDX identifier of the data licence, as [`DATA_FILES`] fixes it for the name.
    pub license: String,
    /// The number of data lines in the file (the header not counted), at most [`MAX_ROWS`].
    pub rows: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Json {
    format: u32,
    version: String,
    key_id: String,
    not_after: String,
    files: Vec<ManifestFile>,
}

#[derive(Deserialize)]
struct KeyIdOnly {
    key_id: String,
}

impl Manifest {
    /// Parses and checks a manifest.
    ///
    /// Refuses more than [`MAX_MANIFEST_BYTES`] bytes before reading them, JSON with a missing,
    /// mistyped or unknown field, and anything [`Manifest::check`] refuses. Never panics.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let json: Json = serde_json::from_slice(bytes).map_err(|_| ManifestError::Json)?;
        let manifest = Manifest {
            format: json.format,
            version: json.version,
            key_id: json.key_id,
            not_after: parse_time(&json.not_after).ok_or(ManifestError::NotAfter)?,
            files: json.files,
        };
        manifest.check()?;
        Ok(manifest)
    }

    /// Reads only `key_id`, to choose the verifying key before the signature is checked.
    ///
    /// Applies the size cap and the `key_id` rule; every other field is left unchecked, so the
    /// result says nothing about the rest of the manifest.
    pub fn peek_key_id(bytes: &[u8]) -> Result<String, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let only: KeyIdOnly = serde_json::from_slice(bytes).map_err(|_| ManifestError::Json)?;
        if !is_valid_key_id(&only.key_id) {
            return Err(ManifestError::KeyId);
        }
        Ok(only.key_id)
    }

    /// Checks every field rule listed on [`ManifestError`], in field order, and reports the
    /// first broken one.
    pub fn check(&self) -> Result<(), ManifestError> {
        if self.format != MANIFEST_FORMAT {
            return Err(ManifestError::Format);
        }
        if parse_version(&self.version).is_none() {
            return Err(ManifestError::Version);
        }
        if !is_valid_key_id(&self.key_id) {
            return Err(ManifestError::KeyId);
        }
        if parse_time(&format_time(self.not_after)) != Some(self.not_after) {
            return Err(ManifestError::NotAfter);
        }
        if self.files.is_empty() || self.files.len() > FILE_NAMES.len() {
            return Err(ManifestError::Files);
        }
        let mut seen = BTreeSet::new();
        for file in &self.files {
            let entry = data_file(&file.name).ok_or(ManifestError::FileName)?;
            if !seen.insert(entry.file) {
                return Err(ManifestError::DuplicateFile);
            }
            if file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(ManifestError::Sha256);
            }
            if !(1..=MAX_FILE_BYTES).contains(&file.size) {
                return Err(ManifestError::Size);
            }
            if file.license != entry.license {
                return Err(ManifestError::License);
            }
            if file.rows > MAX_ROWS as u64 {
                return Err(ManifestError::Rows);
            }
        }
        Ok(())
    }

    /// The listed entry with this file name, if any.
    pub fn file(&self, name: &str) -> Option<&ManifestFile> {
        self.files.iter().find(|file| file.name == name)
    }

    /// The manifest as JSON: two-space indented, fields in declaration order, files in list
    /// order, `not_after` as `YYYY-MM-DDTHH:MM:SSZ`, ending in one newline.
    ///
    /// Deterministic: equal manifests give equal bytes, so the signature can be made over
    /// exactly these bytes. It does not check the manifest; call [`Manifest::check`] first.
    pub fn to_json(&self) -> Vec<u8> {
        let json = Json {
            format: self.format,
            version: self.version.clone(),
            key_id: self.key_id.clone(),
            not_after: format_time(self.not_after),
            files: self.files.clone(),
        };
        let mut out = serde_json::to_vec_pretty(&json).unwrap_or_default();
        out.push(b'\n');
        out
    }
}

fn is_valid_key_id(key_id: &str) -> bool {
    (1..=32).contains(&key_id.len())
        && key_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn parse_time(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || bytes[19] != b'Z' {
        return None;
    }
    let time = NaiveDateTime::parse_from_str(&text[..19], "%Y-%m-%dT%H:%M:%S").ok()?;
    let seconds = time.and_utc().timestamp();
    (format_time(seconds) == text).then_some(seconds)
}

fn format_time(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest() -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT,
            version: "2026110701".into(),
            key_id: "release-2026".into(),
            not_after: 1_799_193_600,
            files: vec![
                ManifestFile {
                    name: "wikidata-categories.tsv.gz".into(),
                    sha256: HASH.into(),
                    size: 347_649,
                    license: "CC0-1.0".into(),
                    rows: 28_370,
                },
                ManifestFile {
                    name: "ut1-categories.tsv.gz".into(),
                    sha256: HASH.into(),
                    size: 1_000,
                    license: "CC-BY-SA-4.0".into(),
                    rows: 10,
                },
            ],
        }
    }

    fn refused(edit: impl FnOnce(&mut Manifest)) -> ManifestError {
        let mut m = manifest();
        edit(&mut m);
        Manifest::parse(&m.to_json()).unwrap_err()
    }

    fn refused_json(edit: impl FnOnce(&mut serde_json::Value)) -> ManifestError {
        let mut value: serde_json::Value = serde_json::from_slice(&manifest().to_json()).unwrap();
        edit(&mut value);
        Manifest::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err()
    }

    #[test]
    fn round_trip_and_shape() {
        let m = manifest();
        let json = m.to_json();
        assert_eq!(Manifest::parse(&json).unwrap(), m);
        assert_eq!(m.to_json(), json);
        let text = String::from_utf8(json).unwrap();
        assert!(text.starts_with("{\n  \"format\": 1,\n  \"version\": \"2026110701\",\n"));
        assert!(text.contains("\"not_after\": \"2027-01-06T00:00:00Z\""));
        assert!(text.ends_with("}\n"));
        assert_eq!(
            Manifest::peek_key_id(text.as_bytes()).unwrap(),
            "release-2026"
        );
        assert_eq!(m.file("ut1-categories.tsv.gz").unwrap().rows, 10);
        assert!(m.file("other.tsv.gz").is_none());
    }

    #[test]
    fn compact_input_is_accepted() {
        let value: serde_json::Value = serde_json::from_slice(&manifest().to_json()).unwrap();
        let compact = serde_json::to_vec(&value).unwrap();
        assert_eq!(Manifest::parse(&compact).unwrap(), manifest());
    }

    #[test]
    fn refusals() {
        assert_eq!(refused(|m| m.format = 2), ManifestError::Format);
        assert_eq!(
            refused(|m| m.version = String::new()),
            ManifestError::Version
        );
        assert_eq!(
            refused(|m| m.version = "1".repeat(17)),
            ManifestError::Version
        );
        assert_eq!(
            refused(|m| m.version = "2026-11-07".into()),
            ManifestError::Version
        );
        assert_eq!(
            refused(|m| m.key_id = "Release".into()),
            ManifestError::KeyId
        );
        assert_eq!(refused(|m| m.key_id = String::new()), ManifestError::KeyId);
        assert_eq!(refused(|m| m.files.clear()), ManifestError::Files);
        assert_eq!(
            refused(|m| {
                let extra = m.files[0].clone();
                m.files.push(extra);
            }),
            ManifestError::Files
        );
        assert_eq!(
            refused(|m| m.files[1] = m.files[0].clone()),
            ManifestError::DuplicateFile
        );
        assert_eq!(
            refused(|m| m.files[0].name = "../wikidata-categories.tsv.gz".into()),
            ManifestError::FileName
        );
        assert_eq!(
            refused(|m| m.files[0].name = "Wikidata-categories.tsv.gz".into()),
            ManifestError::FileName
        );
        assert_eq!(
            refused(|m| m.files[0].sha256 = HASH.to_uppercase()),
            ManifestError::Sha256
        );
        assert_eq!(
            refused(|m| m.files[0].sha256 = HASH[..63].into()),
            ManifestError::Sha256
        );
        assert_eq!(refused(|m| m.files[0].size = 0), ManifestError::Size);
        assert_eq!(
            refused(|m| m.files[0].size = MAX_FILE_BYTES + 1),
            ManifestError::Size
        );
        assert_eq!(
            refused(|m| m.files[0].license = "CC-BY-SA-4.0".into()),
            ManifestError::License
        );
        assert_eq!(
            refused(|m| m.files[0].rows = MAX_ROWS as u64 + 1),
            ManifestError::Rows
        );
        let mut ok = manifest();
        ok.files[0].size = MAX_FILE_BYTES;
        ok.files[0].rows = MAX_ROWS as u64;
        ok.files.truncate(1);
        assert_eq!(Manifest::parse(&ok.to_json()).unwrap(), ok);
    }

    #[test]
    fn json_refusals() {
        assert_eq!(
            refused_json(|v| {
                v["extra"] = 1.into();
            }),
            ManifestError::Json
        );
        assert_eq!(
            refused_json(|v| {
                v["files"][0]["extra"] = 1.into();
            }),
            ManifestError::Json
        );
        assert_eq!(
            refused_json(|v| {
                v.as_object_mut().unwrap().remove("key_id");
            }),
            ManifestError::Json
        );
        assert_eq!(
            refused_json(|v| v["version"] = 2026110701.into()),
            ManifestError::Json
        );
        assert_eq!(
            refused_json(|v| v["files"][0]["size"] = (-1).into()),
            ManifestError::Json
        );
        for bad in [
            "2027-01-06T00:00:00+00:00",
            "2027-01-06 00:00:00Z",
            "2027-01-06T00:00:00.5Z",
            "2027-02-30T00:00:00Z",
            "2027-1-6T00:00:00Z",
            "1799193600",
            "",
        ] {
            assert_eq!(
                refused_json(|v| v["not_after"] = bad.into()),
                ManifestError::NotAfter,
                "{bad}"
            );
        }
        assert_eq!(
            refused_json(|v| v["not_after"] = 1_799_193_600.into()),
            ManifestError::Json
        );
        assert_eq!(Manifest::parse(b""), Err(ManifestError::Json));
        assert_eq!(Manifest::parse(b"[]"), Err(ManifestError::Json));
        let mut big = manifest().to_json();
        big.resize(MAX_MANIFEST_BYTES + 1, b' ');
        assert_eq!(Manifest::parse(&big), Err(ManifestError::TooLarge));
        assert_eq!(Manifest::peek_key_id(&big), Err(ManifestError::TooLarge));
        assert_eq!(
            Manifest::peek_key_id(br#"{"key_id":"A"}"#),
            Err(ManifestError::KeyId)
        );
        let mut padded = manifest().to_json();
        padded.resize(MAX_MANIFEST_BYTES, b' ');
        assert_eq!(Manifest::parse(&padded).unwrap(), manifest());
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("0"), Some(0));
        assert_eq!(parse_version("2026110701"), Some(2_026_110_701));
        assert_eq!(parse_version("0042"), Some(42));
        assert_eq!(parse_version(&"9".repeat(16)), Some(9_999_999_999_999_999));
        for bad in ["", "v1", "1.2", "-1", "+1", " 1", "１"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
        assert_eq!(parse_version(&"1".repeat(17)), None);
        assert!(is_newer("20261107", "20261031"));
        assert!(is_newer("2026110701", "20261107"));
        assert!(is_newer("10", "9"));
        assert!(!is_newer("20261107", "20261107"));
        assert!(!is_newer("010", "10"));
        assert!(!is_newer("20261106", "20261107"));
        assert!(!is_newer("20261107", "v1"));
        assert!(!is_newer("v2", "1"));
    }

    #[test]
    fn data_files() {
        assert_eq!(data_file("ut1-categories.tsv.gz").unwrap().table, "ut1");
        assert_eq!(data_file_for_table("wikidata").unwrap().license, "CC0-1.0");
        assert!(data_file("manifest.json").is_none());
        assert!(data_file_for_table("other").is_none());
        for (entry, name) in DATA_FILES.iter().zip(FILE_NAMES) {
            assert_eq!(entry.file, name);
            assert_eq!(entry.file, format!("{}-categories.tsv.gz", entry.table));
        }
    }

    proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = Manifest::parse(&bytes);
            let _ = Manifest::peek_key_id(&bytes);
        }

        #[test]
        fn times_round_trip(seconds in 0i64..4_102_444_800) {
            prop_assert_eq!(parse_time(&format_time(seconds)), Some(seconds));
        }

        #[test]
        fn accepted_manifests_round_trip(
            version in "[0-9]{1,16}",
            key_id in "[a-z0-9-]{1,32}",
            not_after in 0i64..4_102_444_800,
            rows in 0u64..=500_000,
        ) {
            let mut m = manifest();
            m.version = version;
            m.key_id = key_id;
            m.not_after = not_after;
            m.files[0].rows = rows;
            prop_assert_eq!(Manifest::parse(&m.to_json()).unwrap(), m);
        }
    }
}
