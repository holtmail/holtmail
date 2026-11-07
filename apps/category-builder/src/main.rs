//! `holtmail-category-builder`: builds the category files (`wikidata-categories.tsv.gz` from
//! Wikidata, `ut1-categories.tsv.gz` from a UT1 checkout), runs the release sanity gate and
//! signs the release manifest.
//!
//! A tool run by hand and in CI, never by the server. It is the only code that reaches
//! Wikidata or reads UT1. See `docs/README.md`.

mod build;
mod gate;
mod golden;
mod industries;
mod release;
mod report;
mod sparql;
mod ut1;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime};

use clap::{Args, Parser, Subcommand};
use holtmail_category::TopicSet;
use holtmail_category_data::{DataError, Header, Limits, Row, read};

use crate::build::{MAX_GZIP_BYTES, NAME};
use crate::gate::MAX_UNREVIEWED;
use crate::industries::Industries;
use crate::report::Report;
use crate::sparql::{HttpSparql, Sparql, WIKIDATA_ENDPOINT};

/// Why a build or check failed. Messages carry QIDs, domains, counts and paths only.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A data file, an argument or a root on Wikidata is wrong.
    #[error("{0}")]
    Config(String),
    /// The endpoint gave no complete answer.
    #[error("{0}")]
    Sparql(String),
    /// The result broke a gate: unreviewed sensitive rows, golden domains, size.
    #[error("{0}")]
    Gate(String),
    /// A category file could not be read or written.
    #[error("{0}")]
    Data(#[from] DataError),
    /// A local file could not be read or written.
    #[error("{0}")]
    Io(String),
}

#[derive(Parser)]
#[command(
    name = "holtmail-category-builder",
    about = "Builds and checks the Holtmail category data files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build wikidata-categories.tsv.gz from the Wikidata Query Service.
    Wikidata(WikidataArgs),
    /// Check an existing file offline: golden domains, the sensitive gate and the version.
    Check(CheckArgs),
    /// Build ut1-categories.tsv.gz from a local olbat/ut1-blacklists checkout.
    Ut1(Ut1Args),
    /// Run the release sanity gate on a directory of data files.
    Gate(GateArgs),
    /// Re-run the gate, then write manifest.json (unsigned) into the directory.
    Manifest(ManifestArgs),
    /// Re-run the gate, then write manifest.json and manifest.json.sig into the directory.
    Sign(SignArgs),
    /// Write a fresh signing seed and print its public key.
    Keygen(KeygenArgs),
}

#[derive(Args, Debug, Clone)]
struct Ut1Args {
    /// The checkout or extracted tarball of olbat/ut1-blacklists (or its blacklists directory).
    #[arg(long)]
    dir: PathBuf,
    /// Where to write the file; replaced atomically. LICENSE-DATA is written next to it.
    #[arg(long)]
    out: PathBuf,
    /// The previous release's UT1 file, for the version and the 20% drift check.
    #[arg(long)]
    previous: Option<PathBuf>,
    /// Where to write the report; stdout when absent.
    #[arg(long)]
    report: Option<PathBuf>,
    /// The version, digits only; today's UTC date as YYYYMMDD by default.
    #[arg(long)]
    version: Option<String>,
    /// The upstream commit the checkout is at; the one pinned in data/ut1.toml by default.
    #[arg(long)]
    commit: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct GateArgs {
    /// The directory holding the new data files.
    #[arg(long)]
    dir: PathBuf,
    /// The previous release: its manifest.json and data files. Missing or empty on the first
    /// release.
    #[arg(long)]
    previous: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct SignArgs {
    /// The directory holding the new data files; manifest.json and its signature go here.
    #[arg(long)]
    dir: PathBuf,
    /// The previous release: its manifest.json and data files. Missing or empty on the first
    /// release.
    #[arg(long)]
    previous: Option<PathBuf>,
    /// The id of the signing key, as compiled into holtmail-category-domains.
    #[arg(long)]
    key_id: String,
    /// Days until the manifest expires, 1 to 60.
    #[arg(long)]
    not_after_days: u32,
}

#[derive(Args, Debug, Clone)]
struct ManifestArgs {
    /// The directory holding the new data files; manifest.json goes here.
    #[arg(long)]
    dir: PathBuf,
    /// The previous release: its manifest.json and data files. Missing or empty on the first
    /// release.
    #[arg(long)]
    previous: Option<PathBuf>,
    /// The id of the key that will sign it, as compiled into holtmail-category-domains.
    #[arg(long)]
    key_id: String,
    /// Days until the manifest expires, 1 to 60.
    #[arg(long)]
    not_after_days: u32,
}

#[derive(Args, Debug, Clone)]
struct KeygenArgs {
    /// Where to write the seed (64 hex characters, mode 0600); never replaced.
    #[arg(long)]
    out: PathBuf,
    /// The id the key will carry in manifests, `[a-z0-9-]{1,32}`.
    #[arg(long)]
    key_id: String,
}

#[derive(Args, Debug, Clone)]
struct WikidataArgs {
    /// Where to write the file; replaced atomically.
    #[arg(long)]
    out: PathBuf,
    /// The previous release, read fully before anything is written (may equal --out).
    #[arg(long)]
    previous: Option<PathBuf>,
    /// Where to write the report; stdout when absent.
    #[arg(long)]
    report: Option<PathBuf>,
    /// The version, digits only; today's UTC date as YYYYMMDD by default.
    #[arg(long)]
    version: Option<String>,
    /// The SPARQL endpoint.
    #[arg(long, default_value = WIKIDATA_ENDPOINT)]
    endpoint: String,
    /// Strip every unreviewed sensitive topic and do not fail on their count. Only without
    /// --previous.
    #[arg(long, conflicts_with = "previous")]
    first_release: bool,
    #[arg(skip = MAX_GZIP_BYTES)]
    max_bytes: usize,
}

#[derive(Args, Debug, Clone)]
struct CheckArgs {
    /// The file to check.
    file: PathBuf,
    /// The previous release: its sensitive topics count as reviewed and its version must be
    /// lower.
    #[arg(long)]
    previous: Option<PathBuf>,
}

/// The checked-in data files.
struct Data<'a> {
    industries: &'a str,
    golden: &'a str,
    reviewed: &'a str,
    ut1: &'a str,
    license: &'a str,
}

const DATA: Data<'static> = Data {
    industries: include_str!("../data/industries.toml"),
    golden: include_str!("../data/golden.toml"),
    reviewed: include_str!("../data/sensitive-reviewed.toml"),
    ut1: include_str!("../data/ut1.toml"),
    license: include_str!("../data/LICENSE-DATA"),
};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Wikidata(args) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Io(format!("runtime: {e}")))
            .and_then(|rt| {
                rt.block_on(async {
                    let sparql = HttpSparql::new(&args.endpoint)?;
                    wikidata(&args, &sparql, &DATA).await
                })
            }),
        Command::Check(args) => check(&args, &DATA),
        Command::Ut1(args) => ut1(&args, &DATA),
        Command::Gate(args) => gate(&args, &DATA),
        Command::Manifest(args) => write_manifest(&args, &DATA, SystemTime::now()),
        Command::Sign(args) => sign(
            &args,
            &DATA,
            std::env::var_os(release::KEY_ENV).as_deref(),
            SystemTime::now(),
        ),
        Command::Keygen(args) => keygen(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load(path: &Path) -> Result<(Header, Vec<Row>), Error> {
    load_named(path, NAME)
}

fn load_named(path: &Path, name: &str) -> Result<(Header, Vec<Row>), Error> {
    let bytes =
        std::fs::read(path).map_err(|e| Error::Io(format!("reading {}: {e}", path.display())))?;
    let (header, rows) = read(&bytes, Limits::default())?;
    if header.name != name {
        return Err(Error::Config(format!(
            "{} is the {} file, not {name}",
            path.display(),
            header.name
        )));
    }
    Ok((header, rows))
}

fn lists(data: &Data<'_>) -> Result<(Industries, release::Lists), Error> {
    let industries = Industries::parse(data.industries)?;
    let mut golden = golden::golden_list(data.golden)?;
    golden.forbid(&industries.skip_domains);
    golden.forbid(&industries.mailbox_domains);
    let reviewed = gate::reviewed_list(data.reviewed)?;
    Ok((industries, release::Lists { golden, reviewed }))
}

fn topics_by_domain(rows: &[Row]) -> BTreeMap<String, TopicSet> {
    rows.iter().map(|r| (r.domain.clone(), r.topics)).collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let io = |e: std::io::Error| Error::Io(format!("writing {}: {e}", path.display()));
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    file.write_all(bytes).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .map_err(io)?;
    }
    file.as_file().sync_all().map_err(io)?;
    file.persist(path).map_err(|e| io(e.error))?;
    Ok(())
}

fn emit(report: &Report, path: Option<&Path>) -> Result<(), Error> {
    let text = report.render();
    match path {
        Some(path) => {
            if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)
                    .map_err(|e| Error::Io(format!("creating {}: {e}", dir.display())))?;
            }
            std::fs::write(path, text)
                .map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))
        }
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

async fn wikidata(args: &WikidataArgs, sparql: &impl Sparql, data: &Data<'_>) -> Result<(), Error> {
    let started = Instant::now();
    if args.first_release && args.previous.is_some() {
        return Err(Error::Config(
            "--first-release cannot be used with --previous".into(),
        ));
    }
    let industries = Industries::parse(data.industries)?;
    let reviewed = gate::reviewed_list(data.reviewed)?;
    let mut golden = golden::golden_list(data.golden)?;
    golden.forbid(&industries.skip_domains);
    golden.forbid(&industries.mailbox_domains);
    let (previous_version, previous) = match &args.previous {
        Some(path) => {
            let (header, rows) = load(path)?;
            (Some(header.version), topics_by_domain(&rows))
        }
        None => (None, BTreeMap::new()),
    };
    let version = args
        .version
        .clone()
        .unwrap_or_else(|| build::today(SystemTime::now()));
    build::check_version(&version, previous_version.as_deref())?;

    let fetched = industries::fetch(sparql, &industries).await?;
    let assembled = build::assemble(
        &fetched.found,
        &industries.skip_domains,
        &industries.mailbox_domains,
    );
    let gated = gate::apply(assembled.rows(), &previous, &reviewed);
    let misses = golden::check(&gated.rows, &golden);

    let mut report = Report {
        command: "wikidata",
        version: version.clone(),
        previous_version,
        first_release: args.first_release,
        passes: fetched.passes,
        queries: fetched.queries,
        requests: sparql.requests(),
        skipped: assembled.skipped.clone(),
        crowded: assembled.crowded(20),
        total_rows: gated.rows.len(),
        rows: report::per_topic(&gated.rows),
        golden: golden.len(),
        misses,
        dropped: gated.dropped,
        unreviewed: gated.unreviewed,
        ..Report::default()
    };
    let result = if report.unreviewed.len() > MAX_UNREVIEWED && !args.first_release {
        Err(Error::Gate(format!(
            "{} rows carry unreviewed sensitive topics, more than {MAX_UNREVIEWED}: review them in data/sensitive-reviewed.toml",
            report.unreviewed.len()
        )))
    } else if !report.misses.is_empty() {
        Err(Error::Gate(format!(
            "{} golden domains are missing, changed topics or must have no row",
            report.misses.len()
        )))
    } else {
        build::encode(&version, &gated.rows, args.max_bytes).and_then(|bytes| {
            report.bytes = bytes.len();
            write_atomic(&args.out, &bytes)
        })
    };
    report.elapsed = Some(started.elapsed());
    report.outcome = match &result {
        Ok(()) => format!("written to {}", args.out.display()),
        Err(e) => format!("failed, nothing written: {e}"),
    };
    emit(&report, args.report.as_deref())?;
    result
}

fn check(args: &CheckArgs, data: &Data<'_>) -> Result<(), Error> {
    let industries = Industries::parse(data.industries)?;
    let reviewed = gate::reviewed_list(data.reviewed)?;
    let mut golden = golden::golden_list(data.golden)?;
    golden.forbid(&industries.skip_domains);
    golden.forbid(&industries.mailbox_domains);
    let (header, rows) = load(&args.file)?;
    let (previous_version, previous) = match &args.previous {
        Some(path) => {
            let (header, rows) = load(path)?;
            (Some(header.version), topics_by_domain(&rows))
        }
        None => (None, BTreeMap::new()),
    };
    let unreviewed = gate::audit(&rows, &previous, &reviewed);
    let misses = golden::check(&rows, &golden);
    let result = if let Err(e) = build::check_version(&header.version, previous_version.as_deref())
    {
        Err(e)
    } else if !unreviewed.is_empty() {
        Err(Error::Gate(format!(
            "{} rows carry sensitive topics that were neither in the previous release nor reviewed",
            unreviewed.len()
        )))
    } else if !misses.is_empty() {
        Err(Error::Gate(format!(
            "{} golden domains are missing, changed topics or must have no row",
            misses.len()
        )))
    } else {
        Ok(())
    };
    let report = Report {
        command: "check",
        version: header.version,
        previous_version,
        total_rows: rows.len(),
        rows: report::per_topic(&rows),
        golden: golden.len(),
        misses,
        unreviewed,
        outcome: match &result {
            Ok(()) => "passed".into(),
            Err(e) => format!("failed: {e}"),
        },
        ..Report::default()
    };
    emit(&report, None)?;
    result
}

fn ut1(args: &Ut1Args, data: &Data<'_>) -> Result<(), Error> {
    let started = Instant::now();
    let config = ut1::Ut1::parse(data.ut1)?;
    let (industries, lists) = lists(data)?;
    let commit = args.commit.clone().unwrap_or_else(|| config.commit.clone());
    if !ut1::is_commit(&commit) {
        return Err(Error::Config(
            "--commit is not a 40-character lowercase hex id".into(),
        ));
    }
    let previous = match &args.previous {
        Some(path) => Some(load_named(path, ut1::NAME)?),
        None => None,
    };
    let previous_version = previous.as_ref().map(|(h, _)| h.version.clone());
    let version = args
        .version
        .clone()
        .unwrap_or_else(|| build::today(SystemTime::now()));
    build::check_version(&version, previous_version.as_deref())?;

    let assembled = ut1::assemble(
        &args.dir,
        &config,
        &industries.skip_domains,
        &industries.mailbox_domains,
    )?;
    let rows = assembled.rows();
    let absent = golden::Golden {
        absent: lists.golden.absent.clone(),
        ..golden::Golden::default()
    };
    let misses = golden::check(&rows, &absent);
    let mut notes = vec![format!("source: {} at {commit}", config.repository)];
    notes.extend(assembled.read.iter().map(|(category, count)| {
        format!(
            "category {category} -> {}: {count} hosts read",
            config.categories[category]
        )
    }));
    let mut report = Report {
        command: "ut1",
        version: version.clone(),
        previous_version,
        skipped: assembled.skipped.clone(),
        total_rows: rows.len(),
        rows: report::per_topic(&rows),
        golden: absent.len(),
        misses,
        notes,
        ..Report::default()
    };
    let drift = previous
        .as_ref()
        .map(|(_, old)| (old.len(), rows.len()))
        .filter(|(old, new)| {
            *old > 0 && old.abs_diff(*new) * 100 > old * release::MAX_DRIFT_PERCENT as usize
        });
    let result = if rows.len() > holtmail_category_data::MAX_ROWS {
        Err(Error::Gate(format!(
            "{} rows, more than the {} allowed",
            rows.len(),
            holtmail_category_data::MAX_ROWS
        )))
    } else if let Some((old, new)) = drift {
        Err(Error::Gate(format!(
            "the row count changed from {old} to {new}, more than {}%",
            release::MAX_DRIFT_PERCENT
        )))
    } else if !report.misses.is_empty() {
        Err(Error::Gate(format!(
            "{} rows sit on or under a domain that must have no row",
            report.misses.len()
        )))
    } else {
        build::encode_table(
            ut1::NAME,
            &version,
            &rows,
            holtmail_category_data::MAX_FILE_BYTES as usize,
        )
        .and_then(|bytes| {
            report.bytes = bytes.len();
            let notice = ut1::license_notice(data.license, &config.repository, &commit);
            let dir = args
                .out
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            write_atomic(&dir.join("LICENSE-DATA"), notice.as_bytes())?;
            write_atomic(&args.out, &bytes)
        })
    };
    report.elapsed = Some(started.elapsed());
    report.outcome = match &result {
        Ok(()) => format!("written to {}", args.out.display()),
        Err(e) => format!("failed, nothing written: {e}"),
    };
    emit(&report, args.report.as_deref())?;
    result
}

fn gate(args: &GateArgs, data: &Data<'_>) -> Result<(), Error> {
    let (_, lists) = lists(data)?;
    let new = release::Release::load(&args.dir)?;
    let previous = release::Release::load_optional(args.previous.as_deref())?;
    release::require(&new, &previous, &lists)?;
    for (file, loaded) in &new.files {
        println!(
            "{file}: version {}, {} rows, {} bytes",
            loaded.header.version,
            loaded.count,
            loaded.bytes.len()
        );
    }
    println!("gate passed");
    Ok(())
}

fn unix_seconds(now: SystemTime) -> Result<i64, Error> {
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| Error::Config("the clock is before 1970".into()))?
        .as_secs();
    i64::try_from(now).map_err(|_| Error::Config("the clock is out of range".into()))
}

fn write_manifest(args: &ManifestArgs, data: &Data<'_>, now: SystemTime) -> Result<(), Error> {
    let (_, lists) = lists(data)?;
    let previous = release::Release::load_optional(args.previous.as_deref())?;
    let (manifest, bytes) = release::write_manifest(
        &args.dir,
        &previous,
        &lists,
        &args.key_id,
        args.not_after_days,
        unix_seconds(now)?,
    )?;
    println!(
        "manifest for version {} and key {}: {} files, {} bytes, unsigned",
        manifest.version,
        manifest.key_id,
        manifest.files.len(),
        bytes.len()
    );
    Ok(())
}

fn sign(
    args: &SignArgs,
    data: &Data<'_>,
    key: Option<&std::ffi::OsStr>,
    now: SystemTime,
) -> Result<(), Error> {
    let key = release::signing_key(key)?;
    let (_, lists) = lists(data)?;
    let previous = release::Release::load_optional(args.previous.as_deref())?;
    let now = unix_seconds(now)?;
    let signed = release::sign(
        &args.dir,
        &previous,
        &lists,
        &args.key_id,
        args.not_after_days,
        now,
        &key,
    )?;
    println!(
        "signed version {} with key {} (public {}): {} files, manifest {} bytes, signature {}",
        signed.manifest.version,
        signed.manifest.key_id,
        release::hex(&key.verifying_key().to_bytes()),
        signed.manifest.files.len(),
        signed.bytes.len(),
        release::hex(&signed.signature)
    );
    Ok(())
}

fn keygen(args: &KeygenArgs) -> Result<(), Error> {
    let probe = holtmail_category_data::Manifest {
        format: holtmail_category_data::MANIFEST_FORMAT,
        version: "1".into(),
        key_id: args.key_id.clone(),
        not_after: 0,
        files: Vec::new(),
    };
    if matches!(
        probe.check(),
        Err(holtmail_category_data::ManifestError::KeyId)
    ) {
        return Err(Error::Config("--key-id is not [a-z0-9-]{1,32}".into()));
    }
    let public = release::keygen(&args.out)?;
    println!("key id: {}", args.key_id);
    println!("public key: {}", release::hex(&public));
    println!("{}", release::rust_key(&args.key_id, &public));
    println!(
        "seed written to {}; store it as the {} secret and delete the file",
        args.out.display(),
        release::KEY_ENV
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use holtmail_category::Topic;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::industries::tests::{Fake, INDUSTRIES};

    const NO_LIST: &str = "";

    fn data<'a>(golden: &'a str, reviewed: &'a str) -> Data<'a> {
        Data {
            industries: INDUSTRIES,
            golden,
            reviewed,
            ut1: crate::ut1::tests::CONFIG,
            license: DATA.license,
        }
    }

    fn args(dir: &Path, name: &str) -> WikidataArgs {
        WikidataArgs {
            out: dir.join(name),
            previous: None,
            report: Some(dir.join(format!("{name}.report"))),
            version: Some("20261107".into()),
            endpoint: WIKIDATA_ENDPOINT.into(),
            first_release: true,
            max_bytes: MAX_GZIP_BYTES,
        }
    }

    fn rows_of(path: &Path) -> Vec<(String, TopicSet, String)> {
        load(path)
            .unwrap()
            .1
            .into_iter()
            .map(|r| (r.domain, r.topics, r.source))
            .collect()
    }

    fn set(topics: &[Topic]) -> TopicSet {
        topics.iter().copied().collect()
    }

    #[tokio::test]
    async fn first_release_from_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let a = args(dir.path(), "a.tsv.gz");
        wikidata(&a, &Fake::standard(), &data(NO_LIST, NO_LIST))
            .await
            .unwrap();
        assert_eq!(
            rows_of(&a.out),
            [
                (
                    "example.com".into(),
                    set(&[Topic::Purchases]),
                    "wikidata:Q9100002".into()
                ),
                (
                    "example.org".into(),
                    set(&[Topic::Travel]),
                    "wikidata:Q9200004".into()
                ),
                (
                    "fly.example".into(),
                    set(&[Topic::Travel]),
                    "wikidata:Q9200001".into()
                ),
                (
                    "shop.example.com".into(),
                    set(&[Topic::Purchases]),
                    "wikidata:Q9100001".into()
                ),
            ]
        );
        let report = std::fs::read_to_string(a.report.as_ref().unwrap()).unwrap();
        assert!(
            report.contains("unreviewed sensitive rows: 2 (2 rows dropped"),
            "{report}"
        );
        assert!(report.contains("  bank.example\tfinance\n"));
        assert!(report.contains("  pay.example.com\tfinance\n"));
        assert!(report.contains("  a listed shared platform: 1\n"));
        assert!(report.contains("  a listed mailbox provider: 1\n"));

        let b = args(dir.path(), "b.tsv.gz");
        wikidata(&b, &Fake::standard(), &data(NO_LIST, NO_LIST))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&a.out).unwrap(),
            std::fs::read(&b.out).unwrap()
        );
    }

    #[tokio::test]
    async fn previous_release_and_reviewed_list() {
        let dir = tempfile::tempdir().unwrap();
        let first = args(dir.path(), "data.tsv.gz");
        wikidata(&first, &Fake::standard(), &data(NO_LIST, NO_LIST))
            .await
            .unwrap();

        let mut next = first.clone();
        next.first_release = false;
        next.previous = Some(first.out.clone());
        next.version = Some("20261114".into());
        let reviewed = "[[domain]]\nname = \"bank.example\"\ntopics = [\"finance\"]\n";
        wikidata(&next, &Fake::standard(), &data(NO_LIST, reviewed))
            .await
            .unwrap();
        let rows = rows_of(&next.out);
        assert_eq!(
            rows[0],
            (
                "bank.example".into(),
                set(&[Topic::Finance]),
                "wikidata:Q9200002".into()
            )
        );
        assert_eq!(rows[1].1, set(&[Topic::Purchases]));
        assert_eq!(load(&next.out).unwrap().0.version, "20261114");

        next.version = Some("20261114".into());
        let err = wikidata(&next, &Fake::standard(), &data(NO_LIST, reviewed))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err}");
    }

    fn many_banks(count: usize) -> &'static [u8] {
        let bindings: Vec<String> = (0..count)
            .map(|n| {
                format!(
                    r#"{{"org":{{"type":"uri","value":"http://www.wikidata.org/entity/Q{}"}},"class":{{"type":"uri","value":"http://www.wikidata.org/entity/Q9000003"}},"site":{{"type":"uri","value":"https://bank{n:03}.example/"}}}}"#,
                    9_400_000 + n
                )
            })
            .collect();
        format!(
            r#"{{"head":{{"vars":["org","class","site"]}},"results":{{"bindings":[{}]}}}}"#,
            bindings.join(",")
        )
        .into_bytes()
        .leak()
    }

    fn bank_fake(count: usize) -> Fake {
        Fake::with(&[
            (
                "rdfs:label",
                include_bytes!("../tests/fixtures/labels.json"),
            ),
            (
                "wd:Q9000001 }",
                include_bytes!("../tests/fixtures/subclasses-retail.json"),
            ),
            (
                "wd:Q9000002 }",
                include_bytes!("../tests/fixtures/subclasses-airline.json"),
            ),
            (
                "wd:Q9000003 }",
                include_bytes!("../tests/fixtures/subclasses-bank.json"),
            ),
            (
                "wdt:P452 ?class",
                include_bytes!("../tests/fixtures/orgs-p452.json"),
            ),
            ("wdt:P31 ?class", many_banks(count)),
        ])
    }

    #[tokio::test]
    async fn unreviewed_sensitive_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = args(dir.path(), "a.tsv.gz");
        a.first_release = false;
        wikidata(&a, &bank_fake(MAX_UNREVIEWED), &data(NO_LIST, NO_LIST))
            .await
            .unwrap();
        assert!(
            rows_of(&a.out)
                .iter()
                .all(|r| !r.1.contains(Topic::Finance))
        );

        let b = WikidataArgs {
            first_release: false,
            ..args(dir.path(), "b.tsv.gz")
        };
        let err = wikidata(&b, &bank_fake(MAX_UNREVIEWED + 1), &data(NO_LIST, NO_LIST))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Gate(_)), "{err}");
        assert!(!b.out.exists());
        let report = std::fs::read_to_string(b.report.as_ref().unwrap()).unwrap();
        assert!(report.contains("unreviewed sensitive rows: 51"));
        assert!(report.contains("  bank050.example\tfinance\n"));

        let c = args(dir.path(), "c.tsv.gz");
        wikidata(&c, &bank_fake(200), &data(NO_LIST, NO_LIST))
            .await
            .unwrap();
        assert_eq!(rows_of(&c.out).len(), 2);

        assert!(
            Cli::try_parse_from([
                "holtmail-category-builder",
                "wikidata",
                "--out",
                "x",
                "--previous",
                "y",
                "--first-release"
            ])
            .is_err()
        );
        let mut d = args(dir.path(), "d.tsv.gz");
        d.previous = Some(c.out.clone());
        assert!(
            wikidata(&d, &Fake::standard(), &data(NO_LIST, NO_LIST))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn golden_domains() {
        let dir = tempfile::tempdir().unwrap();
        let good = "[[domain]]\nname = \"fly.example\"\ntopics = [\"travel\"]\n";
        let a = args(dir.path(), "a.tsv.gz");
        wikidata(&a, &Fake::standard(), &data(good, NO_LIST))
            .await
            .unwrap();
        check(
            &CheckArgs {
                file: a.out.clone(),
                previous: None,
            },
            &data(good, NO_LIST),
        )
        .unwrap();

        let changed = "[[domain]]\nname = \"example.org\"\ntopics = [\"purchases\"]\n";
        let b = args(dir.path(), "b.tsv.gz");
        let err = wikidata(&b, &Fake::standard(), &data(changed, NO_LIST))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Gate(_)));
        assert!(!b.out.exists());
        assert!(
            check(
                &CheckArgs {
                    file: a.out.clone(),
                    previous: None
                },
                &data(changed, NO_LIST)
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = args(dir.path(), "a.tsv.gz");
        a.max_bytes = 64;
        let err = wikidata(&a, &Fake::standard(), &data(NO_LIST, NO_LIST))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Gate(_)), "{err}");
        assert!(!a.out.exists());
    }

    #[test]
    fn check_refuses_unreviewed_sensitive_rows_and_old_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.tsv.gz");
        let write = |version: &str, path: &Path| {
            let rows = [Row {
                domain: "bank.example".into(),
                topics: set(&[Topic::Finance]),
                source: "wikidata:Q9200002".into(),
            }];
            std::fs::write(path, build::encode(version, &rows, MAX_GZIP_BYTES).unwrap()).unwrap();
        };
        write("20261107", &path);
        let plain = CheckArgs {
            file: path.clone(),
            previous: None,
        };
        assert!(check(&plain, &data(NO_LIST, NO_LIST)).is_err());
        let reviewed = "[[domain]]\nname = \"bank.example\"\ntopics = [\"finance\"]\n";
        check(&plain, &data(NO_LIST, reviewed)).unwrap();

        let older = dir.path().join("old.tsv.gz");
        write("20261031", &older);
        check(
            &CheckArgs {
                file: path.clone(),
                previous: Some(older.clone()),
            },
            &data(NO_LIST, NO_LIST),
        )
        .unwrap();
        assert!(
            check(
                &CheckArgs {
                    file: older,
                    previous: Some(path)
                },
                &data(NO_LIST, NO_LIST)
            )
            .is_err()
        );
    }

    #[test]
    fn check_refuses_rows_of_listed_domains() {
        let dir = tempfile::tempdir().unwrap();
        for (domain, refused) in [
            ("shop.example.com", false),
            ("mail.example", true),
            ("news.mail.example", true),
            ("shop.sites.example", true),
        ] {
            let path = dir.path().join(format!("{domain}.tsv.gz"));
            let rows = [Row {
                domain: domain.into(),
                topics: set(&[Topic::Purchases]),
                source: "wikidata:Q9100001".into(),
            }];
            std::fs::write(
                &path,
                build::encode("20261107", &rows, MAX_GZIP_BYTES).unwrap(),
            )
            .unwrap();
            let result = check(
                &CheckArgs {
                    file: path,
                    previous: None,
                },
                &data(NO_LIST, NO_LIST),
            );
            assert_eq!(result.is_err(), refused, "{domain}");
        }
    }

    fn ut1_args(dir: &Path, name: &str, version: &str) -> Ut1Args {
        Ut1Args {
            dir: crate::ut1::tests::fixture(),
            out: dir.join(name),
            previous: None,
            report: Some(dir.join(format!("{name}.report"))),
            version: Some(version.into()),
            commit: None,
        }
    }

    #[test]
    fn ut1_from_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let a = ut1_args(dir.path(), "ut1-categories.tsv.gz", "2026111400");
        ut1(&a, &data(NO_LIST, NO_LIST)).unwrap();
        let (header, rows) = load_named(&a.out, "ut1").unwrap();
        assert_eq!(header.version, "2026111400");
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().all(|r| r.source == "ut1"));
        assert!(
            rows.iter()
                .all(|r| !r.topics.iter().any(|t| t.is_sensitive()))
        );
        assert!(load(&a.out).is_err());
        let notice = std::fs::read_to_string(dir.path().join("LICENSE-DATA")).unwrap();
        assert!(notice.contains("at commit 0123456789abcdef0123456789abcdef01234567"));
        let report = std::fs::read_to_string(a.report.as_ref().unwrap()).unwrap();
        assert!(
            report.contains("category shopping -> purchases: 8 hosts read"),
            "{report}"
        );
        assert!(report.contains("  not a bare host name: 1\n"), "{report}");

        let b = ut1_args(dir.path(), "b.tsv.gz", "2026111400");
        ut1(&b, &data(NO_LIST, NO_LIST)).unwrap();
        assert_eq!(
            std::fs::read(&a.out).unwrap(),
            std::fs::read(&b.out).unwrap()
        );

        let mut c = ut1_args(dir.path(), "c.tsv.gz", "2026111400");
        c.previous = Some(a.out.clone());
        assert!(matches!(
            ut1(&c, &data(NO_LIST, NO_LIST)),
            Err(Error::Config(_))
        ));
        c.version = Some("2026112100".into());
        ut1(&c, &data(NO_LIST, NO_LIST)).unwrap();

        let mut d = ut1_args(dir.path(), "d.tsv.gz", "2026112100");
        let five = dir.path().join("five.tsv.gz");
        let mut fewer = rows.clone();
        fewer.truncate(4);
        std::fs::write(
            &five,
            build::encode_table("ut1", "2026110701", &fewer, MAX_GZIP_BYTES).unwrap(),
        )
        .unwrap();
        d.previous = Some(five);
        assert!(matches!(
            ut1(&d, &data(NO_LIST, NO_LIST)),
            Err(Error::Gate(_))
        ));
        assert!(!d.out.exists());

        let golden = "absent = [\"jobs.example\"]\n";
        let e = ut1_args(dir.path(), "e.tsv.gz", "2026111400");
        assert!(matches!(
            ut1(&e, &data(golden, NO_LIST)),
            Err(Error::Gate(_))
        ));
        assert!(!e.out.exists());

        let mut f = ut1_args(dir.path(), "f.tsv.gz", "2026111400");
        f.commit = Some("main".into());
        assert!(ut1(&f, &data(NO_LIST, NO_LIST)).is_err());
    }

    #[test]
    fn gate_and_sign_commands() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("dist");
        std::fs::create_dir_all(&release).unwrap();
        ut1(
            &ut1_args(&release, "ut1-categories.tsv.gz", "2026111400"),
            &data(NO_LIST, NO_LIST),
        )
        .unwrap();
        let mut w = args(&release, "wikidata-categories.tsv.gz");
        w.version = Some("2026111400".into());
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(wikidata(&w, &Fake::standard(), &data(NO_LIST, NO_LIST)))
            .unwrap();
        for report in [
            "ut1-categories.tsv.gz.report",
            "wikidata-categories.tsv.gz.report",
        ] {
            std::fs::remove_file(release.join(report)).unwrap();
        }
        let gate_args = GateArgs {
            dir: release.clone(),
            previous: Some(dir.path().join("previous")),
        };
        gate(&gate_args, &data(NO_LIST, NO_LIST)).unwrap();
        let changed = "[[domain]]\nname = \"example.org\"\ntopics = [\"purchases\"]\n";
        assert!(matches!(
            gate(&gate_args, &data(changed, NO_LIST)),
            Err(Error::Gate(_))
        ));

        let sign_args = SignArgs {
            dir: release.clone(),
            previous: None,
            key_id: "test".into(),
            not_after_days: 30,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_009_600);
        let err = sign(&sign_args, &data(NO_LIST, NO_LIST), None, now).unwrap_err();
        assert_eq!(err.to_string(), "CATEGORIES_SIGNING_KEY is not set");
        assert!(!release.join("manifest.json").exists());
        let seed =
            include_str!("../../../crates/category-domains/tests/fixtures/test-signing-key.hex");
        sign(
            &sign_args,
            &data(NO_LIST, NO_LIST),
            Some(std::ffi::OsStr::new(seed)),
            now,
        )
        .unwrap();
        let manifest = holtmail_category_data::Manifest::parse(
            &std::fs::read(release.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.not_after, 1_794_009_600 + 30 * 86_400);
        assert_eq!(
            std::fs::read(release.join("manifest.json.sig"))
                .unwrap()
                .len(),
            64
        );

        let signed = std::fs::read(release.join("manifest.json")).unwrap();
        let manifest_args = ManifestArgs {
            dir: release.clone(),
            previous: None,
            key_id: "test".into(),
            not_after_days: 30,
        };
        write_manifest(
            &ManifestArgs {
                not_after_days: 61,
                ..manifest_args.clone()
            },
            &data(NO_LIST, NO_LIST),
            now,
        )
        .unwrap_err();
        assert!(release.join("manifest.json.sig").exists());
        write_manifest(&manifest_args, &data(NO_LIST, NO_LIST), now).unwrap();
        assert_eq!(
            std::fs::read(release.join("manifest.json")).unwrap(),
            signed
        );
        assert!(!release.join("manifest.json.sig").exists());

        let next = dir.path().join("next");
        std::fs::create_dir_all(&next).unwrap();
        std::fs::copy(
            release.join("ut1-categories.tsv.gz"),
            next.join("ut1-categories.tsv.gz"),
        )
        .unwrap();
        assert!(matches!(
            gate(
                &GateArgs {
                    dir: next,
                    previous: Some(release)
                },
                &data(NO_LIST, NO_LIST)
            ),
            Err(Error::Gate(_))
        ));
    }

    #[test]
    fn keygen_command() {
        let dir = tempfile::tempdir().unwrap();
        let bad = KeygenArgs {
            out: dir.path().join("a.hex"),
            key_id: "Release 2026".into(),
        };
        assert!(keygen(&bad).is_err());
        assert!(!bad.out.exists());
        keygen(&KeygenArgs {
            key_id: "release-2026".into(),
            ..bad.clone()
        })
        .unwrap();
        assert!(bad.out.exists());
    }

    #[test]
    fn shipped_data_files_parse() {
        let industries = Industries::parse(DATA.industries).unwrap();
        assert!(industries.roots.len() >= 8);
        for topic in Topic::ALL {
            assert!(
                industries.roots.iter().any(|r| r.topics.contains(*topic)),
                "{topic}"
            );
        }
        assert!(gate::reviewed_list(DATA.reviewed).is_ok());
        assert!(golden::golden_list(DATA.golden).is_ok());
    }

    type Handler = dyn Fn(usize, &str) -> (u16, Vec<(&'static str, String)>, Vec<u8>) + Send + Sync;

    async fn serve(handler: Arc<Handler>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let n = count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(k) => head.extend_from_slice(&buf[..k]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let target = head.split(' ').nth(1).unwrap_or("/");
                    let url = url::Url::parse(&format!("http://fake{target}")).unwrap();
                    let query = url
                        .query_pairs()
                        .find(|(k, _)| k == "query")
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default();
                    assert!(head.contains("user-agent: holtmail-category-builder/"));
                    let (status, headers, body) = handler(n, &query);
                    let mut response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                        body.len()
                    );
                    for (name, value) in headers {
                        response.push_str(&format!("{name}: {value}\r\n"));
                    }
                    response.push_str("\r\n");
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        format!("http://{address}/sparql")
    }

    #[tokio::test]
    async fn truncated_answer_fails_the_build() {
        let endpoint = serve(Arc::new(|_, _| {
            (
                200,
                vec![],
                include_bytes!("../tests/fixtures/truncated.json").to_vec(),
            )
        }))
        .await;
        let sparql = HttpSparql::with_pause(&endpoint, Duration::ZERO).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let a = args(dir.path(), "a.tsv.gz");
        let err = wikidata(&a, &sparql, &data(NO_LIST, NO_LIST))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
        assert!(!a.out.exists());
        assert_eq!(sparql.requests(), 1);
    }

    #[tokio::test]
    async fn retry_after_then_success() {
        let endpoint = serve(Arc::new(|n, query| {
            assert!(query.contains("rdfs:label"));
            if n == 0 {
                (
                    429,
                    vec![("retry-after", "1".to_owned())],
                    b"slow down".to_vec(),
                )
            } else {
                (
                    200,
                    vec![],
                    include_bytes!("../tests/fixtures/labels.json").to_vec(),
                )
            }
        }))
        .await;
        let sparql = HttpSparql::with_pause(&endpoint, Duration::ZERO).unwrap();
        let started = Instant::now();
        let query = industries::label_query(&[9_000_001].into());
        let bindings = sparql.select(&query).await.unwrap();
        assert_eq!(bindings.len(), 3);
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert_eq!(sparql.requests(), 2);
    }

    #[tokio::test]
    async fn gives_up_after_five_attempts() {
        let endpoint = serve(Arc::new(|_, _| {
            (503, vec![("retry-after", "0".to_owned())], Vec::new())
        }))
        .await;
        let sparql = HttpSparql::with_pause(&endpoint, Duration::ZERO).unwrap();
        let err = sparql.select("SELECT ?c WHERE { }").await.unwrap_err();
        assert!(err.to_string().contains("5 attempts"), "{err}");
        assert_eq!(sparql.requests(), 5);

        let endpoint = serve(Arc::new(|_, _| (400, vec![], b"bad query".to_vec()))).await;
        let sparql = HttpSparql::with_pause(&endpoint, Duration::ZERO).unwrap();
        assert!(sparql.select("SELECT").await.is_err());
        assert_eq!(sparql.requests(), 1);
    }
}
