//! The stack's category data host: the `images` nginx serves `.state/data` over HTTPS on 443
//! with the stack's certificate as `data.it.test` and `mirror.it.test` (both on the
//! `unsubscribe` network at `10.88.79.10`). `/hop/<path>` answers a `302` to
//! `https://mirror.it.test/<path>` and `/away/<path>` a `302` to `https://outside.it.test/<path>`,
//! which the `unsubscribe` server block answers. Static files carry nginx's default `ETag`, so a
//! matching `If-None-Match` gets `304`. nginx logs each request to these names as one line
//! ([`fetches`]).
//!
//! Releases are built and signed here with the test key whose seed is
//! `crates/category-domains/tests/fixtures/test-signing-key.hex` (key id `test`), which servers
//! built with `insights-test-data` trust.

use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use holtmail_category::Topic;
use holtmail_category_data::{
    Header, MANIFEST_FORMAT, MANIFEST_NAME, Manifest, ManifestFile, Row, SIGNATURE_NAME, data_file,
    write,
};
use sha2::{Digest, Sha256};

/// The data host's name; downloads start here.
pub const DATA_NAME: &str = "data.it.test";
/// The same server under a second pinned name, the target of `/hop/`.
pub const MIRROR_NAME: &str = "mirror.it.test";
/// The Docker network on which both names resolve.
pub const DATA_NETWORK: &str = crate::unsubscribe::ALLOWED_NETWORK;
/// The key id of the test key.
pub const TEST_KEY_ID: &str = "test";

/// One request nginx logged for [`DATA_NAME`] or [`MIRROR_NAME`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetch {
    /// The name asked for.
    pub host: String,
    /// The answer's status.
    pub status: u16,
    /// `GET`, ...
    pub method: String,
    /// The path and query.
    pub uri: String,
    /// The request's `If-None-Match`, empty when absent.
    pub if_none_match: String,
    /// The answer's `ETag`, empty when absent.
    pub etag: String,
}

/// Every request to the data host whose path starts with `prefix`, oldest first.
pub fn fetches(prefix: &str) -> Vec<Fetch> {
    let logs = crate::services::compose(&["logs", "--no-log-prefix", "images"]);
    logs.lines()
        .filter_map(|line| line.strip_prefix("data "))
        .filter_map(parse)
        .filter(|fetch| fetch.uri.starts_with(prefix))
        .collect()
}

fn parse(line: &str) -> Option<Fetch> {
    let mut words = line.splitn(5, ' ');
    let host = words.next()?.to_owned();
    let status = words.next()?.parse().ok()?;
    let method = words.next()?.to_owned();
    let uri = words.next()?.to_owned();
    let mut rest = words.next().unwrap_or_default();
    let mut fetch = Fetch {
        host,
        status,
        method,
        uri,
        ..Fetch::default()
    };
    while let Some((key, after)) = rest.trim_start().split_once("=\"") {
        let (value, tail) = crate::unsubscribe::quoted(after)?;
        match key {
            "inm" => fetch.if_none_match = value,
            "etag" => fetch.etag = value,
            _ => {}
        }
        rest = tail;
    }
    Some(fetch)
}

/// The test key, from the fixture seed.
pub fn test_key() -> SigningKey {
    let path =
        crate::workspace_root().join("crates/category-domains/tests/fixtures/test-signing-key.hex");
    let hex = std::fs::read_to_string(path).expect("the test seed");
    let hex = hex.trim();
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex");
    }
    SigningKey::from_bytes(&seed)
}

/// A key no server trusts, under any id.
pub fn stranger_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A data file in format 1 for `table` at `version` with `rows` of `(domain, topic)`, sorted
/// here; each row's source is the table name.
pub fn table(table: &str, version: &str, rows: &[(&str, Topic)]) -> Vec<u8> {
    let mut rows: Vec<Row> = rows
        .iter()
        .map(|(domain, topic)| Row {
            domain: (*domain).to_owned(),
            topics: [*topic].into_iter().collect(),
            source: table.to_owned(),
        })
        .collect();
    rows.sort_by(|a, b| a.domain.cmp(&b.domain));
    write(&Header::new(table, version), &rows).expect("a valid data file")
}

/// A release: data files by their fixed names and a manifest signed over its exact bytes.
#[derive(Debug, Clone)]
pub struct Release {
    /// The manifest's version.
    pub version: String,
    /// `(file name, bytes)`.
    pub files: Vec<(String, Vec<u8>)>,
    /// `manifest.json`.
    pub manifest: Vec<u8>,
    /// `manifest.json.sig`, the raw 64-byte signature.
    pub signature: Vec<u8>,
}

impl Release {
    /// Lists `files` (with their row counts) in a manifest at `version`, valid until
    /// `not_after` (Unix seconds), under `key_id`, signed with `key`.
    pub fn new(
        version: &str,
        files: Vec<(&str, Vec<u8>, u64)>,
        not_after: i64,
        key_id: &str,
        key: &SigningKey,
    ) -> Self {
        let manifest = Manifest {
            format: MANIFEST_FORMAT,
            version: version.to_owned(),
            key_id: key_id.to_owned(),
            not_after,
            files: files
                .iter()
                .map(|(name, bytes, rows)| ManifestFile {
                    name: (*name).to_owned(),
                    sha256: Sha256::digest(bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    size: bytes.len() as u64,
                    license: data_file(name)
                        .expect("a fixed file name")
                        .license
                        .to_owned(),
                    rows: *rows,
                })
                .collect(),
        }
        .to_json();
        let signature = key.sign(&manifest).to_bytes().to_vec();
        Self {
            version: version.to_owned(),
            files: files
                .into_iter()
                .map(|(name, bytes, _)| (name.to_owned(), bytes))
                .collect(),
            manifest,
            signature,
        }
    }

    /// Writes the release into `dir` (created if needed), each file through a rename: the
    /// data files, then the signature, then the manifest.
    pub fn publish(&self, dir: &Path) {
        std::fs::create_dir_all(dir).expect("the release folder");
        for (name, bytes) in self
            .files
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .chain([
                (SIGNATURE_NAME, self.signature.as_slice()),
                (MANIFEST_NAME, self.manifest.as_slice()),
            ])
        {
            let staged = dir.join(format!(".{name}.part"));
            std::fs::write(&staged, bytes).expect("a release file written");
            std::fs::rename(&staged, dir.join(name)).expect("a release file renamed");
        }
    }

    /// Writes only the signature and then the manifest into `dir`, leaving the data files
    /// already there untouched.
    pub fn publish_manifest(&self, dir: &Path) {
        Self {
            files: Vec::new(),
            ..self.clone()
        }
        .publish(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nginx_data_lines() {
        let line = r#"data.it.test 304 GET /r-1/manifest.json inm="\"6745a1b2-2c1\"" etag="\"6745a1b2-2c1\"""#;
        let fetch = parse(line).expect("a fetch");
        assert_eq!(fetch.host, DATA_NAME);
        assert_eq!(fetch.status, 304);
        assert_eq!(fetch.uri, "/r-1/manifest.json");
        assert_eq!(fetch.if_none_match, "\"6745a1b2-2c1\"");
        assert_eq!(fetch.etag, fetch.if_none_match);
        let first = parse(r#"mirror.it.test 200 GET /r-1/x inm="" etag="""#).expect("a fetch");
        assert!(first.if_none_match.is_empty() && first.etag.is_empty());
    }

    #[test]
    fn signs_with_the_test_key() {
        use ed25519_dalek::Verifier;
        let file = table(
            "wikidata",
            "2099010100",
            &[("shop.example.com", Topic::Purchases)],
        );
        let release = Release::new(
            "2099010100",
            vec![("wikidata-categories.tsv.gz", file, 1)],
            4_102_444_800,
            TEST_KEY_ID,
            &test_key(),
        );
        let parsed = Manifest::parse(&release.manifest).expect("a valid manifest");
        assert_eq!(parsed.key_id, TEST_KEY_ID);
        let signature: [u8; 64] = release.signature.clone().try_into().expect("64 bytes");
        test_key()
            .verifying_key()
            .verify(
                &release.manifest,
                &ed25519_dalek::Signature::from_bytes(&signature),
            )
            .expect("verifies");
        assert_eq!(
            test_key().verifying_key().to_bytes()[..4],
            [199, 67, 215, 157],
            "the key the test-key feature compiles in"
        );
    }
}
