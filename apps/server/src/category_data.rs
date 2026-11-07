use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use holtmail_api::{
    DataOrigin, DataOutcome, DataSnapshot, DataStatus, DataUpdates, LoadedDataFile,
};
use holtmail_category_data::{
    DomainTable, MANIFEST_NAME, MAX_MANIFEST_BYTES, Manifest, SIGNATURE_NAME, data_file,
    data_file_for_table, is_newer,
};
use holtmail_category_domains::{DomainsCategorizer, UpdateError, check_file, verify_stored};
use holtmail_core::{Timestamp, TlsRoots};
use holtmail_fetch::{DataDownload, Download, FetchError};
use holtmail_fetch_http::{DataDownloadOptions, HttpDataDownload};
use holtmail_store::Store;
use tokio::sync::Mutex;

use crate::insights::{Insights, InsightsConfig};
use crate::walk::WalkWake;

/// The setting holding the `ETag` of the last accepted manifest.
pub const ETAG_SETTING: &str = "categories_data_etag";
/// The setting holding the version of the last accepted release.
pub const VERSION_SETTING: &str = "categories_data_version";
/// The setting holding when the last check finished, in Unix milliseconds.
pub const CHECKED_SETTING: &str = "categories_data_checked_at";
/// The setting holding what the last check found.
pub const OUTCOME_SETTING: &str = "categories_data_outcome";

/// The folder under the data directory that holds the accepted release.
pub const DIR: &str = "categories";

const MAX_SIGNATURE_DOWNLOAD: u64 = 1024;

const UT1_ATTRIBUTION: &str = "Contains data from UT1 (Université Toulouse Capitole), CC BY-SA 4.0";

/// Loads the category data release stored in the data directory and keeps it current.
///
/// At start it verifies the stored manifest and each selected file and loads what passes into
/// the domain categorizer; anything that fails is ignored with a warning and the bundled table
/// stays. [`Updater::check`] fetches, verifies, stores and loads a newer release. Checks never
/// run concurrently; a refused or failed check changes neither the files nor the loaded tables.
pub struct Updater {
    dir: PathBuf,
    base_url: String,
    data_sources: Vec<String>,
    target: Option<crate::insights::UpdateTarget>,
    download: Option<Arc<dyn DataDownload>>,
    store: Arc<dyn Store>,
    domains: Arc<DomainsCategorizer>,
    wake: Arc<WalkWake>,
    status: Arc<DataStatus>,
    state: Mutex<State>,
}

struct State {
    bundled: DomainTable,
    release: Option<String>,
    origins: Vec<(String, DataOrigin)>,
    checked_at: Option<Timestamp>,
    outcome: Option<DataOutcome>,
}

/// What [`Updater::open_with`] needs besides the insights.
pub(crate) struct Parts {
    pub dir: PathBuf,
    pub data_sources: Vec<String>,
    pub target: Option<crate::insights::UpdateTarget>,
    pub download: Option<Arc<dyn DataDownload>>,
    pub store: Arc<dyn Store>,
}

enum Checked {
    Updated { version: String, files: usize },
    NotModified,
}

enum CheckError {
    Refused(&'static str, String),
    Failed(String),
}

impl From<FetchError> for CheckError {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Refused(_) => Self::Refused("download_refused", error.to_string()),
            FetchError::Unavailable(_) => Self::Failed(error.to_string()),
        }
    }
}

impl From<UpdateError> for CheckError {
    fn from(error: UpdateError) -> Self {
        Self::Refused(reason(error), error.to_string())
    }
}

impl Updater {
    /// The updater for `insights` when `domains` is among the sources, else `None`. It loads the
    /// stored release from `{data_dir}/categories/` into the categorizer and publishes what is
    /// loaded to the API. With `update` on it builds a downloader that trusts the built-in roots
    /// and `roots`, connects directly whatever `image_proxy` says and reaches only the pinned
    /// hosts.
    pub async fn open(
        config: &InsightsConfig,
        insights: &Insights,
        data_dir: &Path,
        roots: &TlsRoots,
        store: Arc<dyn Store>,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        let categories = &config.categories;
        let target = categories.update.then(|| categories.update_target());
        let download = match &target {
            Some(target) => {
                let download = HttpDataDownload::new(DataDownloadOptions {
                    roots: roots.clone(),
                    pinned_hosts: target.hosts.clone(),
                    ..DataDownloadOptions::default()
                })
                .map_err(|error| anyhow::anyhow!("insights.categories.update: {error}"))?;
                Some(Arc::new(download) as Arc<dyn DataDownload>)
            }
            None => None,
        };
        Self::open_with(
            insights,
            Parts {
                dir: data_dir.join(DIR),
                data_sources: categories.data_sources.clone(),
                target,
                download,
                store,
            },
        )
        .await
    }

    pub(crate) async fn open_with(
        insights: &Insights,
        parts: Parts,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        let Some(domains) = insights.domains.clone() else {
            return Ok(None);
        };
        let bundled = holtmail_category_domains::bundled()
            .map_err(|error| anyhow::anyhow!("the bundled domain table does not load: {error}"))?;
        let stored = read_stored(&parts.dir, &parts.data_sources);
        let (tables, origins) = select(&bundled, stored.tables);
        domains
            .reload(tables)
            .map_err(|error| anyhow::anyhow!("domain tables: {error}"))?;
        let checked_at = parts
            .store
            .setting(CHECKED_SETTING)
            .await?
            .and_then(|text| text.parse().ok())
            .map(Timestamp::from_millis);
        let outcome = parts
            .store
            .setting(OUTCOME_SETTING)
            .await?
            .as_deref()
            .and_then(outcome_from_name);
        let updater = Arc::new(Self {
            dir: parts.dir,
            base_url: parts
                .target
                .as_ref()
                .map(|target| target.base_url.clone())
                .unwrap_or_default(),
            data_sources: parts.data_sources,
            target: parts.target,
            download: parts.download,
            store: parts.store,
            domains,
            wake: insights.wake.clone(),
            status: insights.api.data.clone(),
            state: Mutex::new(State {
                bundled,
                release: stored.release,
                origins,
                checked_at,
                outcome,
            }),
        });
        updater.publish(&*updater.state.lock().await);
        Ok(Some(updater))
    }

    /// Whether the server checks for releases on a schedule.
    pub fn updates(&self) -> bool {
        self.download.is_some()
    }

    fn publish(&self, state: &State) {
        let files = self
            .domains
            .tables()
            .into_iter()
            .map(|table| {
                let entry = data_file_for_table(&table.name);
                let origin = state
                    .origins
                    .iter()
                    .find(|(name, _)| *name == table.name)
                    .map_or(DataOrigin::Bundled, |(_, origin)| *origin);
                LoadedDataFile {
                    file: entry.map(|e| e.file.to_owned()).unwrap_or_default(),
                    license: entry.map(|e| e.license.to_owned()).unwrap_or_default(),
                    attribution: (table.name == "ut1").then(|| UT1_ATTRIBUTION.to_owned()),
                    name: table.name,
                    version: table.version,
                    rows: table.rows as u64,
                    origin,
                }
            })
            .collect();
        self.status.set(DataSnapshot {
            files,
            updates: DataUpdates {
                enabled: self.updates(),
                data_sources: self.data_sources.clone(),
                last_checked_at: state.checked_at,
                last_outcome: state.outcome,
            },
        });
    }

    /// How long to wait before the first check: the configured delay, or longer when the last
    /// check recorded in settings was less than one interval ago.
    pub fn first_wait(&self, now: Timestamp, checked_at: Option<Timestamp>) -> Duration {
        let Some(target) = &self.target else {
            return Duration::ZERO;
        };
        let due = checked_at.map_or(0, |at| {
            at.as_millis()
                .saturating_add(target.interval.as_millis() as i64)
                .saturating_sub(now.as_millis())
                .max(0) as u64
        });
        target.delay.max(Duration::from_millis(due))
    }

    /// Checks once for a newer release and returns what it found. An accepted release is
    /// written, loaded and wakes the walk; every outcome is recorded in settings and published
    /// to the API. Logs versions, file names, sizes, row counts, reasons and timings only.
    pub async fn check(&self) -> DataOutcome {
        let started = Instant::now();
        let mut state = self.state.lock().await;
        let outcome = match self.try_check(&mut state).await {
            Ok(Checked::Updated { version, files }) => {
                tracing::info!(
                    %version,
                    files,
                    millis = started.elapsed().as_millis() as u64,
                    "category data updated"
                );
                DataOutcome::Updated
            }
            Ok(Checked::NotModified) => {
                tracing::info!(
                    millis = started.elapsed().as_millis() as u64,
                    "category data not modified"
                );
                DataOutcome::NotModified
            }
            Err(CheckError::Refused(reason, error)) => {
                tracing::warn!(reason, %error, "category data refused");
                DataOutcome::Refused
            }
            Err(CheckError::Failed(error)) => {
                tracing::warn!(%error, "category data check failed");
                DataOutcome::Failed
            }
        };
        let now = Timestamp::now();
        state.checked_at = Some(now);
        state.outcome = Some(outcome);
        for (key, value) in [
            (CHECKED_SETTING, now.as_millis().to_string()),
            (OUTCOME_SETTING, outcome_name(outcome).to_owned()),
        ] {
            if let Err(error) = self.store.set_setting(key, Some(&value)).await {
                tracing::warn!(%error, key, "category data check not recorded");
            }
        }
        self.publish(&state);
        outcome
    }

    async fn try_check(&self, state: &mut State) -> Result<Checked, CheckError> {
        let download = self
            .download
            .as_ref()
            .ok_or_else(|| CheckError::Failed("updates are off".into()))?;
        let etag = match &state.release {
            Some(_) => self.store.setting(ETAG_SETTING).await.map_err(failed)?,
            None => None,
        };
        let (manifest_bytes, etag) = match download
            .get(
                &self.url(MANIFEST_NAME),
                etag.as_deref(),
                MAX_MANIFEST_BYTES as u64,
            )
            .await?
        {
            Download::NotModified => return Ok(Checked::NotModified),
            Download::Fetched { bytes, etag } => (bytes, etag),
        };
        let signature = match download
            .get(&self.url(SIGNATURE_NAME), None, MAX_SIGNATURE_DOWNLOAD)
            .await?
        {
            Download::Fetched { bytes, .. } => bytes,
            Download::NotModified => {
                return Err(CheckError::Failed("signature answered not modified".into()));
            }
        };
        let manifest = holtmail_category_domains::verify_manifest(
            &manifest_bytes,
            &signature,
            Timestamp::now().as_secs(),
            None,
        )?;
        match &state.release {
            Some(loaded) if manifest.version == *loaded => {
                self.set(ETAG_SETTING, etag.as_deref()).await?;
                return Ok(Checked::NotModified);
            }
            Some(loaded) if !is_newer(&manifest.version, loaded) => {
                return Err(UpdateError::NotNewer.into());
            }
            _ => {}
        }
        if let Some(accepted) = self.store.setting(VERSION_SETTING).await.map_err(failed)?
            && is_newer(&accepted, &manifest.version)
        {
            return Err(UpdateError::NotNewer.into());
        }

        let mut files = Vec::new();
        for source in &self.data_sources {
            let Some(entry) = data_file_for_table(source) else {
                continue;
            };
            let Some(listed) = manifest.file(entry.file) else {
                continue;
            };
            let bytes = match download
                .get(&self.url(entry.file), None, listed.size)
                .await?
            {
                Download::Fetched { bytes, .. } => bytes,
                Download::NotModified => {
                    return Err(CheckError::Failed("data file answered not modified".into()));
                }
            };
            let table = check_file(&manifest, entry.file, &bytes)?;
            tracing::info!(
                file = entry.file,
                version = %table.version(),
                bytes = bytes.len(),
                rows = table.len(),
                "category data file verified"
            );
            files.push((entry.file, bytes, table));
        }

        let dir = self.dir.clone();
        let writes: Vec<(&'static str, bytes::Bytes)> = files
            .iter()
            .map(|(name, bytes, _)| (*name, bytes.clone()))
            .chain([
                (MANIFEST_NAME, manifest_bytes.clone()),
                (SIGNATURE_NAME, signature.clone()),
            ])
            .collect();
        tokio::task::spawn_blocking(move || write_release(&dir, &writes))
            .await
            .map_err(failed)?
            .map_err(|error| CheckError::Failed(format!("{error:#}")))?;
        self.set(VERSION_SETTING, Some(&manifest.version)).await?;
        self.set(ETAG_SETTING, etag.as_deref()).await?;

        let count = files.len();
        let (tables, origins) = select(
            &state.bundled,
            files.into_iter().map(|(_, _, table)| table).collect(),
        );
        self.domains
            .reload(tables)
            .map_err(|error| CheckError::Failed(error.to_string()))?;
        state.release = Some(manifest.version.clone());
        state.origins = origins;
        self.wake.signal();
        Ok(Checked::Updated {
            version: manifest.version,
            files: count,
        })
    }

    async fn set(&self, key: &str, value: Option<&str>) -> Result<(), CheckError> {
        self.store.set_setting(key, value).await.map_err(failed)
    }

    fn url(&self, name: &str) -> String {
        format!("{}{name}", self.base_url)
    }

    /// The last check recorded, for [`Updater::first_wait`].
    pub async fn checked_at(&self) -> Option<Timestamp> {
        self.state.lock().await.checked_at
    }

    fn interval(&self) -> Duration {
        self.target
            .as_ref()
            .map_or(Duration::from_secs(3600), |target| target.interval)
    }
}

/// Checks for new category data after [`Updater::first_wait`], then every interval, forever.
/// Returns at once when updates are off.
pub async fn run(updater: Arc<Updater>) {
    if !updater.updates() {
        return;
    }
    let wait = updater.first_wait(Timestamp::now(), updater.checked_at().await);
    tokio::time::sleep(wait).await;
    loop {
        updater.check().await;
        tokio::time::sleep(updater.interval()).await;
    }
}

fn failed(error: impl std::fmt::Display) -> CheckError {
    CheckError::Failed(error.to_string())
}

struct Stored {
    tables: Vec<DomainTable>,
    release: Option<String>,
}

fn read_stored(dir: &Path, data_sources: &[String]) -> Stored {
    let none = Stored {
        tables: Vec::new(),
        release: None,
    };
    let manifest_path = dir.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return none;
    }
    let manifest = match read_capped(&manifest_path, MAX_MANIFEST_BYTES as u64).and_then(|m| {
        let signature = read_capped(
            &dir.join(SIGNATURE_NAME),
            holtmail_category_domains::SIGNATURE_BYTES as u64,
        )?;
        Ok((m, signature))
    }) {
        Ok((manifest, signature)) => match verify_stored(&manifest, &signature) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!(
                    file = MANIFEST_NAME,
                    reason = reason(error),
                    %error,
                    "stored category data ignored"
                );
                return none;
            }
        },
        Err(error) => {
            tracing::warn!(file = MANIFEST_NAME, error = %format!("{error:#}"), "stored category data ignored");
            return none;
        }
    };
    let mut tables = Vec::new();
    let mut consistent = true;
    for source in data_sources {
        let Some(entry) = data_file_for_table(source) else {
            continue;
        };
        let Some(listed) = manifest.file(entry.file) else {
            continue;
        };
        match load_file(dir, &manifest, entry.file, listed.size) {
            Ok(table) => {
                tracing::info!(
                    file = entry.file,
                    version = %table.version(),
                    rows = table.len(),
                    "stored category data file loaded"
                );
                tables.push(table);
            }
            Err(error) => {
                consistent = false;
                tracing::warn!(
                    version = %manifest.version,
                    file = entry.file,
                    error = %format!("{error:#}"),
                    "stored category data file ignored"
                );
            }
        }
    }
    Stored {
        tables,
        release: consistent.then_some(manifest.version),
    }
}

fn load_file(
    dir: &Path,
    manifest: &Manifest,
    name: &str,
    size: u64,
) -> anyhow::Result<DomainTable> {
    let bytes = read_capped(&dir.join(name), size)?;
    Ok(check_file(manifest, name, &bytes)?)
}

fn read_capped(path: &Path, max: u64) -> anyhow::Result<Vec<u8>> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let len = std::fs::metadata(path)
        .with_context(|| format!("reading {name}"))?
        .len();
    anyhow::ensure!(len <= max, "{name} is larger than allowed");
    std::fs::read(path).with_context(|| format!("reading {name}"))
}

/// The tables to vote with: the bundled table, replaced by a downloaded table of the same name
/// only when that one's version is newer, and every other downloaded table.
fn select(
    bundled: &DomainTable,
    downloaded: Vec<DomainTable>,
) -> (Vec<DomainTable>, Vec<(String, DataOrigin)>) {
    let mut tables = Vec::new();
    let mut origins = Vec::new();
    let mut bundled_kept = true;
    for table in downloaded {
        if table.name() == bundled.name() {
            if !is_newer(table.version(), bundled.version()) {
                tracing::info!(
                    table = %table.name(),
                    downloaded = %table.version(),
                    bundled = %bundled.version(),
                    "bundled category table is newer than the downloaded one"
                );
                continue;
            }
            bundled_kept = false;
        }
        origins.push((table.name().to_owned(), DataOrigin::Downloaded));
        tables.push(table);
    }
    if bundled_kept {
        origins.push((bundled.name().to_owned(), DataOrigin::Bundled));
        tables.push(bundled.clone());
    }
    (tables, origins)
}

fn write_release(dir: &Path, files: &[(&str, bytes::Bytes)]) -> anyhow::Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(dir).context("creating the category data folder")?;
    for (name, bytes) in files {
        anyhow::ensure!(
            data_file(name).is_some() || *name == MANIFEST_NAME || *name == SIGNATURE_NAME,
            "refusing to write a file outside the fixed set"
        );
        let mut file =
            tempfile::NamedTempFile::new_in(dir).with_context(|| format!("writing {name}"))?;
        file.write_all(bytes)
            .and_then(|()| file.as_file().sync_all())
            .with_context(|| format!("writing {name}"))?;
        file.persist(dir.join(name))
            .map_err(|error| error.error)
            .with_context(|| format!("renaming {name}"))?;
        sync_dir(dir).with_context(|| format!("syncing the folder after {name}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

fn reason(error: UpdateError) -> &'static str {
    match error {
        UpdateError::SignatureLength => "signature_length",
        UpdateError::NoReleaseKey => "no_release_key",
        UpdateError::UnknownKey => "unknown_key",
        UpdateError::Signature => "signature",
        UpdateError::Manifest(_) => "manifest",
        UpdateError::Expired => "expired",
        UpdateError::TooFarAhead => "too_far_ahead",
        UpdateError::NotNewer => "not_newer",
        UpdateError::FileName => "file_name",
        UpdateError::NotListed => "not_listed",
        UpdateError::Size => "size",
        UpdateError::Sha256 => "sha256",
        UpdateError::Data(_) => "data",
        UpdateError::TableName => "table_name",
        UpdateError::TableVersion => "table_version",
        UpdateError::Rows => "rows",
        _ => "other",
    }
}

fn outcome_name(outcome: DataOutcome) -> &'static str {
    match outcome {
        DataOutcome::Updated => "updated",
        DataOutcome::NotModified => "not_modified",
        DataOutcome::Refused => "refused",
        DataOutcome::Failed => "failed",
    }
}

fn outcome_from_name(name: &str) -> Option<DataOutcome> {
    [
        DataOutcome::Updated,
        DataOutcome::NotModified,
        DataOutcome::Refused,
        DataOutcome::Failed,
    ]
    .into_iter()
    .find(|outcome| outcome_name(*outcome) == name)
}

#[cfg(test)]
mod tests;
