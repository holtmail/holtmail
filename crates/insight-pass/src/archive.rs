use std::io::{Cursor, Read};

use holtmail_insight::limits::MAX_PKPASS_BYTES;
use zip::{CompressionMethod, ZipArchive};

use crate::{MAX_ENTRIES, MAX_PASS_JSON_BYTES, MAX_UNCOMPRESSED_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    ContentTooLarge,
    NotZip,
    TooManyEntries,
    DuplicateNames,
    UnsafePath,
    TooLarge,
    NoPassJson,
    PassJsonDeclaredTooLarge,
    PassJsonTooLarge,
    Encrypted,
    Compression,
    ReadFailed,
}

impl Refusal {
    pub(crate) fn rule(self) -> &'static str {
        match self {
            Self::ContentTooLarge => "refused:content_too_large",
            Self::NotZip => "refused:not_zip",
            Self::TooManyEntries => "refused:too_many_entries",
            Self::DuplicateNames => "refused:duplicate_names",
            Self::UnsafePath => "refused:unsafe_path",
            Self::TooLarge => "refused:uncompressed_too_large",
            Self::NoPassJson => "refused:no_pass_json",
            Self::PassJsonDeclaredTooLarge => "refused:pass_json_declared_too_large",
            Self::PassJsonTooLarge => "refused:pass_json_too_large",
            Self::Encrypted => "refused:encrypted",
            Self::Compression => "refused:compression",
            Self::ReadFailed => "refused:read_failed",
        }
    }
}

pub(crate) struct PassJson {
    pub entries: usize,
    pub bytes: Vec<u8>,
}

pub(crate) fn read_pass_json(content: &[u8]) -> Result<PassJson, (Refusal, usize)> {
    if content.len() > MAX_PKPASS_BYTES {
        return Err((Refusal::ContentTooLarge, 0));
    }
    let mut archive = ZipArchive::new(Cursor::new(content)).map_err(|_| (Refusal::NotZip, 0))?;
    let entries = archive.len();
    let refuse = |refusal| (refusal, entries);
    if entries > MAX_ENTRIES {
        return Err(refuse(Refusal::TooManyEntries));
    }
    if archive.has_duplicate_names() {
        return Err(refuse(Refusal::DuplicateNames));
    }

    let mut total: u64 = 0;
    let mut pass_json = None;
    for index in 0..entries {
        let entry = archive
            .by_index_data(index)
            .map_err(|_| refuse(Refusal::NotZip))?;
        let name = entry.name_raw();
        if unsafe_name(name) {
            return Err(refuse(Refusal::UnsafePath));
        }
        total = total.saturating_add(entry.size());
        if name == b"pass.json" {
            pass_json = Some((index, entry.size(), entry.encrypted(), entry.compression()));
        }
    }
    if total > MAX_UNCOMPRESSED_BYTES {
        return Err(refuse(Refusal::TooLarge));
    }
    let (index, declared, encrypted, method) = pass_json.ok_or(refuse(Refusal::NoPassJson))?;
    if declared > MAX_PASS_JSON_BYTES {
        return Err(refuse(Refusal::PassJsonDeclaredTooLarge));
    }
    if encrypted {
        return Err(refuse(Refusal::Encrypted));
    }
    if !matches!(
        method,
        CompressionMethod::Stored | CompressionMethod::Deflated
    ) {
        return Err(refuse(Refusal::Compression));
    }

    let file = archive
        .by_index(index)
        .map_err(|_| refuse(Refusal::ReadFailed))?;
    let mut bytes = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
    file.take(MAX_PASS_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refuse(Refusal::ReadFailed))?;
    if bytes.len() as u64 > MAX_PASS_JSON_BYTES {
        return Err(refuse(Refusal::PassJsonTooLarge));
    }
    Ok(PassJson { entries, bytes })
}

fn unsafe_name(name: &[u8]) -> bool {
    matches!(name.first(), Some(b'/' | b'\\'))
        || name
            .split(|b| *b == b'/' || *b == b'\\')
            .any(|segment| segment == b"..")
        || name.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_per_segment() {
        assert!(!unsafe_name(b"pass.json"));
        assert!(!unsafe_name(b"en.lproj/pass.strings"));
        assert!(!unsafe_name(b"a..b/c"));
        assert!(unsafe_name(b"../pass.json"));
        assert!(unsafe_name(b"en.lproj/../../pass.json"));
        assert!(unsafe_name(b"en.lproj\\..\\pass.json"));
        assert!(unsafe_name(b"/etc/pass.json"));
        assert!(unsafe_name(b"\\pass.json"));
        assert!(unsafe_name(b"x/.."));
    }

    #[test]
    fn junk_is_not_a_zip() {
        assert_eq!(
            read_pass_json(b"PK\x03\x04 nope").err(),
            Some((Refusal::NotZip, 0))
        );
        assert_eq!(read_pass_json(b"").err(), Some((Refusal::NotZip, 0)));
    }
}
