use std::collections::BTreeMap;
use std::fmt;
use std::io::Read as _;
use std::path::Path;

use ed25519_dalek::{Signer as _, SigningKey};
use holtmail_category::TopicSet;
use holtmail_category_data::{
    DATA_FILES, FILE_NAMES, Header, Limits, MANIFEST_FORMAT, MANIFEST_NAME, MAX_FILE_BYTES,
    Manifest, ManifestFile, Row, data_file, is_newer, parse, read,
};
use sha2::{Digest as _, Sha256};

use crate::Error;
use crate::gate::{MAX_UNREVIEWED, audit};
use crate::golden::{Golden, check as golden_check};

/// The largest share of rows a file may gain or lose against the previous release.
pub(crate) const MAX_DRIFT_PERCENT: u64 = 20;

/// The longest validity of a signed manifest, in days.
pub(crate) const MAX_VALIDITY_DAYS: u32 = 60;

/// The environment variable holding the signing seed as 64 hex characters.
pub(crate) const KEY_ENV: &str = "CATEGORIES_SIGNING_KEY";

/// One data file read from a release directory.
#[derive(Debug, Clone)]
pub(crate) struct Loaded {
    pub header: Header,
    pub rows: Vec<Row>,
    /// Data lines in the file, rows dropped for unknown topics included.
    pub count: u64,
    pub bytes: Vec<u8>,
}

/// The data files of one release directory, by fixed file name, and every other
/// `*.tsv.gz` name found there.
#[derive(Debug, Clone, Default)]
pub(crate) struct Release {
    pub files: BTreeMap<&'static str, Loaded>,
    pub unknown: Vec<String>,
    pub manifest: Option<Manifest>,
}

fn read_capped(path: &Path) -> Result<Vec<u8>, Error> {
    let io = |e: std::io::Error| Error::Io(format!("reading {}: {e}", path.display()));
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(io)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::Gate(format!(
            "{} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn load_file(path: &Path) -> Result<Loaded, Error> {
    let bytes = read_capped(path)?;
    let table = parse(&bytes)?;
    let (header, rows) = read(&bytes, Limits::default())?;
    Ok(Loaded {
        header,
        rows,
        count: (table.len() + table.skipped()) as u64,
        bytes,
    })
}

impl Release {
    /// Reads every fixed-name data file in `dir` and `manifest.json` when present. Other
    /// `*.tsv.gz` names are recorded as unknown; any other file is ignored. A file that does
    /// not parse fails the whole read.
    pub(crate) fn load(dir: &Path) -> Result<Self, Error> {
        let io = |e: std::io::Error| Error::Io(format!("reading {}: {e}", dir.display()));
        let mut release = Self::default();
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map_err(io)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()
            .map_err(io)?;
        names.sort();
        for name in names {
            if name == MANIFEST_NAME {
                let bytes = read_capped(&dir.join(&name))?;
                release.manifest =
                    Some(Manifest::parse(&bytes).map_err(|e| {
                        Error::Config(format!("{}: {e}", dir.join(&name).display()))
                    })?);
            } else if let Some(entry) = data_file(&name) {
                release
                    .files
                    .insert(entry.file, load_file(&dir.join(&name))?);
            } else if name.ends_with(".tsv.gz") {
                release.unknown.push(name);
            }
        }
        Ok(release)
    }

    /// A release directory that may not exist yet (the first release): empty when missing.
    pub(crate) fn load_optional(dir: Option<&Path>) -> Result<Self, Error> {
        match dir {
            Some(dir) if dir.exists() => Self::load(dir),
            _ => Ok(Self::default()),
        }
    }

    fn version(&self) -> Option<&str> {
        self.files
            .values()
            .next()
            .map(|f| f.header.version.as_str())
    }
}

/// The lists the gate checks against: golden domains, never-written domains and the
/// sensitive review.
#[derive(Debug, Clone, Default)]
pub(crate) struct Lists {
    /// Golden domains with `absent` already extended by `skip_domains` and `mailbox_domains`.
    pub golden: Golden,
    pub reviewed: BTreeMap<String, TopicSet>,
}

/// One broken rule of the sanity gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    NoFiles,
    UnknownFile(String),
    TableName {
        file: &'static str,
        found: String,
    },
    MixedVersions,
    NotNewer {
        version: String,
        previous: String,
    },
    Drift {
        file: &'static str,
        previous: u64,
        now: u64,
    },
    Golden {
        file: &'static str,
        misses: usize,
    },
    Unreviewed {
        file: &'static str,
        rows: usize,
    },
    SensitiveUt1 {
        rows: usize,
    },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFiles => write!(f, "no data file"),
            Self::UnknownFile(name) => write!(f, "{name} is not one of the fixed file names"),
            Self::TableName { file, found } => {
                write!(f, "{file} carries the table name {found}")
            }
            Self::MixedVersions => write!(f, "the files carry different versions"),
            Self::NotNewer { version, previous } => write!(
                f,
                "version {version} is not greater than the previous {previous}"
            ),
            Self::Drift {
                file,
                previous,
                now,
            } => write!(
                f,
                "{file} changed from {previous} to {now} rows, more than {MAX_DRIFT_PERCENT}%"
            ),
            Self::Golden { file, misses } => write!(
                f,
                "{file}: {misses} golden domains are missing, changed topics or must have no row"
            ),
            Self::Unreviewed { file, rows } => write!(
                f,
                "{file}: {rows} rows carry unreviewed sensitive topics, more than {MAX_UNREVIEWED}"
            ),
            Self::SensitiveUt1 { rows } => {
                write!(
                    f,
                    "ut1-categories.tsv.gz: {rows} rows carry a sensitive topic"
                )
            }
        }
    }
}

fn drifted(previous: u64, now: u64) -> bool {
    previous > 0 && previous.abs_diff(now) * 100 > previous * MAX_DRIFT_PERCENT
}

fn topics(rows: &[Row]) -> BTreeMap<String, TopicSet> {
    rows.iter().map(|r| (r.domain.clone(), r.topics)).collect()
}

/// The sanity gate over a new release against the previous one, which is empty on the first
/// release. Every broken rule is returned, in this order:
///
/// - no data file, or a `*.tsv.gz` outside the fixed names;
/// - a header name that is not the one fixed for the file, or files of different versions;
/// - a version not greater than the previous manifest's or any previous file's;
/// - a row count more than 20% away from the previous manifest's count for that file (or the
///   previous file's when no manifest lists it);
/// - for the Wikidata file, a golden domain missing or with other topics, or a row on or under
///   an absent domain, and more than 50 rows with sensitive topics neither the previous file
///   nor the review gives them;
/// - for the UT1 file, a row on or under an absent domain, and any sensitive topic at all.
pub(crate) fn gate(new: &Release, previous: &Release, lists: &Lists) -> Vec<Failure> {
    let mut failures = Vec::new();
    if new.files.is_empty() {
        failures.push(Failure::NoFiles);
    }
    failures.extend(new.unknown.iter().cloned().map(Failure::UnknownFile));
    for (file, loaded) in &new.files {
        let expected = data_file(file).map(|d| d.table).unwrap_or_default();
        if loaded.header.name != expected {
            failures.push(Failure::TableName {
                file,
                found: loaded.header.name.clone(),
            });
        }
    }
    let version = new.version().unwrap_or_default();
    if new.files.values().any(|f| f.header.version != version) {
        failures.push(Failure::MixedVersions);
    }
    let previous_versions = previous
        .manifest
        .iter()
        .map(|m| m.version.as_str())
        .chain(previous.files.values().map(|f| f.header.version.as_str()));
    for old in previous_versions {
        if !version.is_empty() && !is_newer(version, old) {
            failures.push(Failure::NotNewer {
                version: version.to_owned(),
                previous: old.to_owned(),
            });
            break;
        }
    }
    for (file, loaded) in &new.files {
        let old = previous
            .manifest
            .as_ref()
            .and_then(|m| m.file(file))
            .map(|f| f.rows)
            .or_else(|| previous.files.get(file).map(|f| f.count));
        if let Some(old) = old
            && drifted(old, loaded.count)
        {
            failures.push(Failure::Drift {
                file,
                previous: old,
                now: loaded.count,
            });
        }
    }
    if let Some(loaded) = new.files.get(FILE_NAMES[0]) {
        let misses = golden_check(&loaded.rows, &lists.golden).len();
        if misses > 0 {
            failures.push(Failure::Golden {
                file: FILE_NAMES[0],
                misses,
            });
        }
        let before = previous
            .files
            .get(FILE_NAMES[0])
            .map(|f| topics(&f.rows))
            .unwrap_or_default();
        let rows = audit(&loaded.rows, &before, &lists.reviewed).len();
        if rows > MAX_UNREVIEWED {
            failures.push(Failure::Unreviewed {
                file: FILE_NAMES[0],
                rows,
            });
        }
    }
    if let Some(loaded) = new.files.get(FILE_NAMES[1]) {
        let absent = Golden {
            present: BTreeMap::new(),
            absent: lists.golden.absent.clone(),
        };
        let misses = golden_check(&loaded.rows, &absent).len();
        if misses > 0 {
            failures.push(Failure::Golden {
                file: FILE_NAMES[1],
                misses,
            });
        }
        let rows = loaded
            .rows
            .iter()
            .filter(|r| r.topics.iter().any(|t| t.is_sensitive()))
            .count();
        if rows > 0 {
            failures.push(Failure::SensitiveUt1 { rows });
        }
    }
    failures
}

/// Runs the gate and turns its failures into one error, after printing each to stderr.
pub(crate) fn require(new: &Release, previous: &Release, lists: &Lists) -> Result<(), Error> {
    let failures = gate(new, previous, lists);
    if failures.is_empty() {
        return Ok(());
    }
    for failure in &failures {
        eprintln!("gate: {failure}");
    }
    Err(Error::Gate(format!(
        "the sanity gate failed on {} rules",
        failures.len()
    )))
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest for a release that passed the gate: files in fixed order, each with its
/// SHA-256, size, licence and row count. Checked before it is returned.
pub(crate) fn manifest(new: &Release, key_id: &str, not_after: i64) -> Result<Manifest, Error> {
    let files = DATA_FILES
        .iter()
        .filter_map(|entry| {
            new.files.get(entry.file).map(|loaded| ManifestFile {
                name: entry.file.into(),
                sha256: hex(&Sha256::digest(&loaded.bytes)),
                size: loaded.bytes.len() as u64,
                license: entry.license.into(),
                rows: loaded.count,
            })
        })
        .collect();
    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        version: new.version().unwrap_or_default().to_owned(),
        key_id: key_id.to_owned(),
        not_after,
        files,
    };
    manifest
        .check()
        .map_err(|e| Error::Config(format!("the manifest would be invalid: {e}")))?;
    Ok(manifest)
}

/// The signing key from the value of [`KEY_ENV`]: exactly 64 hex characters, surrounding
/// whitespace ignored. Errors never contain the value.
pub(crate) fn signing_key(value: Option<&std::ffi::OsStr>) -> Result<SigningKey, Error> {
    let value = value
        .ok_or_else(|| Error::Config(format!("{KEY_ENV} is not set")))?
        .to_str()
        .ok_or_else(|| Error::Config(format!("{KEY_ENV} is not 64 hex characters")))?
        .trim();
    let mut seed = [0u8; 32];
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Config(format!("{KEY_ENV} is not 64 hex characters")));
    }
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap_or_default();
    }
    let key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    Ok(key)
}

/// What [`sign`] wrote.
#[derive(Debug, Clone)]
pub(crate) struct Signed {
    pub manifest: Manifest,
    pub bytes: Vec<u8>,
    pub signature: [u8; 64],
}

/// Re-runs the gate on `dir` against `previous` and builds the manifest for it, with
/// `not_after` at `now` plus `days`, 1 to 60. Returns the manifest and its exact bytes from
/// [`Manifest::to_json`]; writes nothing.
fn prepare(
    dir: &Path,
    previous: &Release,
    lists: &Lists,
    key_id: &str,
    days: u32,
    now: i64,
) -> Result<(Manifest, Vec<u8>), Error> {
    if !(1..=MAX_VALIDITY_DAYS).contains(&days) {
        return Err(Error::Config(format!(
            "--not-after-days must be 1 to {MAX_VALIDITY_DAYS}"
        )));
    }
    let new = Release::load(dir)?;
    require(&new, previous, lists)?;
    let manifest = manifest(&new, key_id, now + i64::from(days) * 86_400)?;
    let bytes = manifest.to_json();
    Ok((manifest, bytes))
}

/// Re-runs the gate on `dir` against `previous`, then writes only `manifest.json` into `dir`
/// and removes a `manifest.json.sig` left there, so no stale signature sits beside it. Needs
/// no key; the bytes written are exactly the ones [`sign`] would sign. Nothing is written
/// when any check fails.
pub(crate) fn write_manifest(
    dir: &Path,
    previous: &Release,
    lists: &Lists,
    key_id: &str,
    days: u32,
    now: i64,
) -> Result<(Manifest, Vec<u8>), Error> {
    let (manifest, bytes) = prepare(dir, previous, lists, key_id, days, now)?;
    let signature = dir.join(holtmail_category_data::SIGNATURE_NAME);
    match std::fs::remove_file(&signature) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error::Io(format!("removing {}: {e}", signature.display())));
        }
    }
    crate::write_atomic(&dir.join(MANIFEST_NAME), &bytes)?;
    Ok((manifest, bytes))
}

/// Re-runs the gate on `dir` against `previous`, then writes `manifest.json` (from
/// [`Manifest::to_json`]) and `manifest.json.sig` (the 64-byte ed25519 signature over exactly
/// those bytes) into `dir`. `not_after` is `now` plus `days`, 1 to 60. Nothing is written
/// when any check fails.
pub(crate) fn sign(
    dir: &Path,
    previous: &Release,
    lists: &Lists,
    key_id: &str,
    days: u32,
    now: i64,
    key: &SigningKey,
) -> Result<Signed, Error> {
    let (manifest, bytes) = prepare(dir, previous, lists, key_id, days, now)?;
    let signature = key.sign(&bytes).to_bytes();
    key.verifying_key()
        .verify_strict(&bytes, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| Error::Config("the signature does not verify".into()))?;
    crate::write_atomic(&dir.join(MANIFEST_NAME), &bytes)?;
    crate::write_atomic(
        &dir.join(holtmail_category_data::SIGNATURE_NAME),
        &signature,
    )?;
    Ok(Signed {
        manifest,
        bytes,
        signature,
    })
}

/// Writes a fresh random seed to `out` as 64 hex characters and a newline, mode 0600, and
/// returns its public key. Fails without writing when `out` already exists.
pub(crate) fn keygen(out: &Path) -> Result<[u8; 32], Error> {
    use std::io::Write as _;
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| Error::Io(format!("random: {e}")))?;
    let key = SigningKey::from_bytes(&seed);
    let mut text = hex(&seed);
    seed.fill(0);
    text.push('\n');
    let io = |e: std::io::Error| Error::Io(format!("writing {}: {e}", out.display()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(out).map_err(io)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    text.replace_range(.., &"0".repeat(text.len()));
    written.map_err(io)?;
    Ok(key.verifying_key().to_bytes())
}

/// The public key as the Rust item `crates/category-domains` compiles in.
pub(crate) fn rust_key(id: &str, public: &[u8; 32]) -> String {
    let bytes: Vec<String> = public.iter().map(u8::to_string).collect();
    format!(
        "ReleaseKey {{\n    id: \"{id}\",\n    public: [{}],\n}}",
        bytes.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, VerifyingKey};
    use holtmail_category::Topic;

    use super::*;
    use crate::build::encode_table;

    const TEST_SEED: &str =
        include_str!("../../../crates/category-domains/tests/fixtures/test-signing-key.hex");
    const TEST_PUBLIC: [u8; 32] = [
        199, 67, 215, 157, 219, 98, 40, 11, 119, 182, 223, 74, 57, 26, 29, 185, 63, 195, 75, 55,
        168, 222, 59, 83, 122, 207, 223, 209, 219, 13, 138, 56,
    ];
    const NOW: i64 = 1_794_009_600;

    fn row(domain: &str, topics: &[Topic], source: &str) -> Row {
        Row {
            domain: domain.into(),
            topics: topics.iter().copied().collect(),
            source: source.into(),
        }
    }

    fn rows(prefix: &str, count: usize, topic: Topic, source: &str) -> Vec<Row> {
        let mut rows: Vec<Row> = (0..count)
            .map(|n| row(&format!("{prefix}{n:04}.example"), &[topic], source))
            .collect();
        rows.sort_by(|a, b| a.domain.cmp(&b.domain));
        rows
    }

    fn write(dir: &Path, file: &str, table: &str, version: &str, rows: &[Row]) {
        std::fs::create_dir_all(dir).unwrap();
        let bytes = encode_table(table, version, rows, MAX_FILE_BYTES as usize).unwrap();
        std::fs::write(dir.join(file), bytes).unwrap();
    }

    fn wikidata(dir: &Path, version: &str, rows: &[Row]) {
        write(dir, FILE_NAMES[0], "wikidata", version, rows);
    }

    fn ut1(dir: &Path, version: &str, rows: &[Row]) {
        write(dir, FILE_NAMES[1], "ut1", version, rows);
    }

    fn standard(dir: &Path, version: &str, count: usize) {
        wikidata(
            dir,
            version,
            &rows("shop", count, Topic::Purchases, "wikidata:Q9000001"),
        );
        ut1(dir, version, &rows("job", count, Topic::Jobs, "ut1"));
    }

    fn lists() -> Lists {
        let mut golden = Golden::default();
        golden.absent.insert("mail.example".into());
        Lists {
            golden,
            reviewed: BTreeMap::new(),
        }
    }

    fn failures(new: &Path, previous: Option<&Path>, lists: &Lists) -> Vec<Failure> {
        gate(
            &Release::load(new).unwrap(),
            &Release::load_optional(previous).unwrap(),
            lists,
        )
    }

    fn key() -> SigningKey {
        signing_key(Some(std::ffi::OsStr::new(TEST_SEED))).unwrap()
    }

    #[test]
    fn first_and_next_release_pass() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        standard(&old, "2026110701", 100);
        standard(&new, "2026111400", 120);
        assert_eq!(failures(&new, None, &lists()), []);
        assert_eq!(failures(&new, Some(&dir.path().join("none")), &lists()), []);
        assert_eq!(failures(&new, Some(&old), &lists()), []);
        std::fs::write(new.join("LICENSE-DATA"), "notice").unwrap();
        assert_eq!(failures(&new, Some(&old), &lists()), []);
    }

    #[test]
    fn drift() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        standard(&old, "2026110701", 100);
        for (count, fails) in [(80, false), (79, true), (121, true), (120, false)] {
            let new = dir.path().join(format!("new{count}"));
            standard(&new, "2026111400", count);
            let got = failures(&new, Some(&old), &lists());
            assert_eq!(got.len(), if fails { 2 } else { 0 }, "{count}: {got:?}");
            assert!(got.iter().all(|f| matches!(f, Failure::Drift { .. })));
        }

        let only_wikidata = dir.path().join("w");
        wikidata(
            &only_wikidata,
            "2026110701",
            &rows("shop", 100, Topic::Purchases, "wikidata:Q1"),
        );
        let small_ut1 = dir.path().join("u");
        standard(&small_ut1, "2026111400", 100);
        ut1(
            &small_ut1,
            "2026111400",
            &rows("job", 10, Topic::Jobs, "ut1"),
        );
        assert_eq!(failures(&small_ut1, Some(&only_wikidata), &lists()), []);
    }

    #[test]
    fn drift_uses_the_previous_manifest_count() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        standard(&old, "2026110701", 100);
        let mut previous = Release::load(&old).unwrap();
        previous.manifest = Some(manifest(&previous, "test", NOW).unwrap());
        previous.manifest.as_mut().unwrap().files[0].rows = 200;
        let new = dir.path().join("new");
        standard(&new, "2026111400", 100);
        let got = gate(&Release::load(&new).unwrap(), &previous, &lists());
        assert_eq!(
            got,
            [Failure::Drift {
                file: FILE_NAMES[0],
                previous: 200,
                now: 100
            }]
        );
    }

    #[test]
    fn version_must_grow() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        standard(&old, "2026111400", 100);
        for version in ["2026111400", "20261121", "2026110701"] {
            let new = dir.path().join(version);
            standard(&new, version, 100);
            assert!(
                failures(&new, Some(&old), &lists())
                    .iter()
                    .any(|f| matches!(f, Failure::NotNewer { .. })),
                "{version}"
            );
        }
        let previous = Release {
            manifest: Some(manifest(&Release::load(&old).unwrap(), "test", NOW).unwrap()),
            ..Release::default()
        };
        let new = dir.path().join("same");
        standard(&new, "2026111400", 100);
        assert!(matches!(
            gate(&Release::load(&new).unwrap(), &previous, &lists())[..],
            [Failure::NotNewer { .. }]
        ));

        let mixed = dir.path().join("mixed");
        wikidata(
            &mixed,
            "2026112100",
            &rows("shop", 100, Topic::Purchases, "wikidata:Q1"),
        );
        ut1(&mixed, "2026112101", &rows("job", 100, Topic::Jobs, "ut1"));
        assert!(failures(&mixed, None, &lists()).contains(&Failure::MixedVersions));
    }

    #[test]
    fn names() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new");
        standard(&new, "2026111400", 10);
        write(
            &new,
            "other-categories.tsv.gz",
            "other",
            "2026111400",
            &rows("x", 1, Topic::Jobs, "x"),
        );
        assert_eq!(
            failures(&new, None, &lists()),
            [Failure::UnknownFile("other-categories.tsv.gz".into())]
        );

        let swapped = dir.path().join("swapped");
        write(
            &swapped,
            FILE_NAMES[1],
            "wikidata",
            "2026111400",
            &rows("x", 1, Topic::Jobs, "x"),
        );
        assert_eq!(
            failures(&swapped, None, &lists()),
            [Failure::TableName {
                file: FILE_NAMES[1],
                found: "wikidata".into()
            }]
        );

        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(failures(&empty, None, &lists()), [Failure::NoFiles]);
    }

    #[test]
    fn golden_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut lists = lists();
        lists.golden.present.insert(
            "shop0001.example".into(),
            TopicSet::EMPTY.with(Topic::Purchases),
        );
        let new = dir.path().join("new");
        standard(&new, "2026111400", 10);
        assert_eq!(failures(&new, None, &lists), []);

        lists.golden.present.insert(
            "shop0002.example".into(),
            TopicSet::EMPTY.with(Topic::Travel),
        );
        assert_eq!(
            failures(&new, None, &lists),
            [Failure::Golden {
                file: FILE_NAMES[0],
                misses: 1
            }]
        );

        let absent = dir.path().join("absent");
        let mut jobs = rows("job", 10, Topic::Jobs, "ut1");
        jobs.push(row("news.mail.example", &[Topic::Jobs], "ut1"));
        wikidata(
            &absent,
            "2026111400",
            &rows("shop", 10, Topic::Purchases, "wikidata:Q1"),
        );
        ut1(&absent, "2026111400", &jobs);
        assert_eq!(
            failures(&absent, None, &self::lists()),
            [Failure::Golden {
                file: FILE_NAMES[1],
                misses: 1
            }]
        );
    }

    #[test]
    fn sensitive_rows() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok");
        wikidata(
            &ok,
            "2026111400",
            &rows("bank", MAX_UNREVIEWED, Topic::Finance, "wikidata:Q1"),
        );
        assert_eq!(failures(&ok, None, &lists()), []);

        let many = dir.path().join("many");
        let banks = rows("bank", MAX_UNREVIEWED + 1, Topic::Finance, "wikidata:Q1");
        wikidata(&many, "2026111400", &banks);
        assert_eq!(
            failures(&many, None, &lists()),
            [Failure::Unreviewed {
                file: FILE_NAMES[0],
                rows: MAX_UNREVIEWED + 1
            }]
        );
        let mut reviewed = lists();
        reviewed.reviewed.insert(
            "bank0000.example".into(),
            TopicSet::EMPTY.with(Topic::Finance),
        );
        assert_eq!(failures(&many, None, &reviewed), []);
        let old = dir.path().join("old");
        wikidata(&old, "2026110701", &banks);
        assert_eq!(failures(&many, Some(&old), &lists()), []);

        let sensitive_ut1 = dir.path().join("ut1");
        ut1(
            &sensitive_ut1,
            "2026111400",
            &[row("bank.example", &[Topic::Finance], "ut1")],
        );
        assert_eq!(
            failures(&sensitive_ut1, None, &lists()),
            [Failure::SensitiveUt1 { rows: 1 }]
        );
    }

    #[test]
    fn sign_and_verify_with_the_test_seed() {
        let key = key();
        assert_eq!(key.verifying_key().to_bytes(), TEST_PUBLIC);
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new");
        standard(&new, "2026111400", 10);
        let signed = sign(&new, &Release::default(), &lists(), "test", 60, NOW, &key).unwrap();

        let bytes = std::fs::read(new.join(MANIFEST_NAME)).unwrap();
        let signature = std::fs::read(new.join("manifest.json.sig")).unwrap();
        assert_eq!(bytes, signed.bytes);
        assert_eq!(signature.len(), 64);
        let verifying = VerifyingKey::from_bytes(&TEST_PUBLIC).unwrap();
        let signature = Signature::from_slice(&signature).unwrap();
        verifying.verify_strict(&bytes, &signature).unwrap();
        let mut flipped = bytes.clone();
        flipped[10] ^= 1;
        assert!(verifying.verify_strict(&flipped, &signature).is_err());

        let manifest = Manifest::parse(&bytes).unwrap();
        assert_eq!(manifest.version, "2026111400");
        assert_eq!(manifest.key_id, "test");
        assert_eq!(manifest.not_after, NOW + 60 * 86_400);
        assert_eq!(manifest.files.len(), 2);
        for (file, name) in manifest.files.iter().zip(FILE_NAMES) {
            let data = std::fs::read(new.join(name)).unwrap();
            assert_eq!(file.name, name);
            assert_eq!(file.size, data.len() as u64);
            assert_eq!(file.sha256, hex(&Sha256::digest(&data)));
            assert_eq!(file.rows, 10);
        }
        assert_eq!(manifest.files[1].license, "CC-BY-SA-4.0");

        let again = sign(&new, &Release::default(), &lists(), "test", 60, NOW, &key).unwrap();
        assert_eq!(again.bytes, signed.bytes);
        assert_eq!(again.signature, signed.signature);
    }

    #[test]
    fn sign_refuses_and_writes_nothing() {
        let key = key();
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new");
        standard(&new, "2026111400", 10);
        for (key_id, days) in [("test", 0), ("test", 61), ("Test", 30), ("", 30)] {
            assert!(sign(&new, &Release::default(), &lists(), key_id, days, NOW, &key).is_err());
        }
        let old = dir.path().join("old");
        standard(&old, "2026112100", 10);
        let previous = Release::load(&old).unwrap();
        assert!(matches!(
            sign(&new, &previous, &lists(), "test", 30, NOW, &key),
            Err(Error::Gate(_))
        ));
        assert!(!new.join(MANIFEST_NAME).exists());
        assert!(!new.join("manifest.json.sig").exists());
    }

    #[test]
    fn signing_key_errors_never_echo_the_value() {
        assert!(signing_key(None).unwrap_err().to_string().contains(KEY_ENV));
        let secret = "5ec7".repeat(16);
        for bad in [
            secret[..63].to_owned(),
            format!("{secret}00"),
            format!("{}zz", &secret[..62]),
            format!("{}\u{e9}", &secret[..62]),
            String::new(),
        ] {
            let message = signing_key(Some(std::ffi::OsStr::new(&bad)))
                .unwrap_err()
                .to_string();
            assert!(!message.contains("5ec7"), "{message}");
            assert!(message.contains(KEY_ENV), "{message}");
        }
        assert!(
            signing_key(Some(std::ffi::OsStr::new(&format!(
                " {} \n",
                TEST_SEED.trim()
            ))))
            .is_ok()
        );
    }

    #[test]
    fn keygen_writes_a_private_seed() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seed.hex");
        let public = keygen(&out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text.len(), 65);
        let key = signing_key(Some(std::ffi::OsStr::new(&text))).unwrap();
        assert_eq!(key.verifying_key().to_bytes(), public);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(keygen(&out).is_err());
        assert_ne!(keygen(&dir.path().join("other.hex")).unwrap(), public);
        let item = rust_key("release-2026", &TEST_PUBLIC);
        assert!(
            item.starts_with("ReleaseKey {\n    id: \"release-2026\",\n    public: [199, 67, ")
        );
    }
}
