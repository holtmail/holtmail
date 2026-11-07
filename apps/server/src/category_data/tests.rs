#![cfg_attr(not(feature = "insights-test-data"), allow(dead_code))]

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_category::Topic;
use holtmail_category_data::{Header, MANIFEST_FORMAT, ManifestFile, Row, write};
use holtmail_core::ArchiveId;
use holtmail_store::{ArchiveStore, SettingsStore};
use holtmail_store_sqlite::SqliteStore;

use super::*;
use crate::insights::{CategoriesConfig, UpdateTarget};

const BASE: &str = "https://data.example.com/categories/";
const WIKIDATA: &str = "wikidata-categories.tsv.gz";
const UT1: &str = "ut1-categories.tsv.gz";
const DAY: i64 = 24 * 60 * 60;
const NEWER: &str = "2027010101";
const NEWEST: &str = "2027020101";
const OLDER: &str = "2026010101";

#[derive(Default)]
struct FakeDownload {
    served: StdMutex<HashMap<String, (Bytes, Option<String>)>>,
    requests: StdMutex<Vec<(String, Option<String>)>>,
}

impl FakeDownload {
    fn serve(&self, name: &str, bytes: impl Into<Bytes>, etag: Option<&str>) {
        self.served.lock().unwrap().insert(
            format!("{BASE}{name}"),
            (bytes.into(), etag.map(Into::into)),
        );
    }

    fn requests(&self) -> Vec<(String, Option<String>)> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }
}

#[async_trait]
impl DataDownload for FakeDownload {
    async fn get(
        &self,
        url: &str,
        etag: Option<&str>,
        max_bytes: u64,
    ) -> Result<Download, FetchError> {
        self.requests
            .lock()
            .unwrap()
            .push((url.to_owned(), etag.map(Into::into)));
        let Some((bytes, served)) = self.served.lock().unwrap().get(url).cloned() else {
            return Err(FetchError::Unavailable("status 404".into()));
        };
        if etag.is_some() && etag == served.as_deref() {
            return Ok(Download::NotModified);
        }
        if bytes.len() as u64 > max_bytes {
            return Err(FetchError::Refused("too large".into()));
        }
        Ok(Download::Fetched {
            bytes,
            etag: served,
        })
    }
}

fn table_file(name: &str, version: &str, domains: &[&str]) -> Vec<u8> {
    let rows: Vec<Row> = domains
        .iter()
        .map(|domain| Row {
            domain: (*domain).into(),
            topics: [Topic::Travel].into_iter().collect(),
            source: "test".into(),
        })
        .collect();
    write(&Header::new(name, version), &rows).unwrap()
}

fn wikidata(version: &str) -> Vec<u8> {
    table_file(
        "wikidata",
        version,
        &["air.example.com", "shop.example.com", "trips.example.net"],
    )
}

fn ut1(version: &str) -> Vec<u8> {
    table_file("ut1", version, &["jobs.example.org"])
}

fn entry(name: &str, bytes: &[u8], rows: u64) -> ManifestFile {
    ManifestFile {
        name: name.into(),
        sha256: holtmail_category_domains::sha256_hex(bytes),
        size: bytes.len() as u64,
        license: data_file(name).unwrap().license.into(),
        rows,
    }
}

struct Release {
    manifest: Vec<u8>,
    signature: Vec<u8>,
    wikidata: Vec<u8>,
    ut1: Vec<u8>,
}

fn seed() -> [u8; 32] {
    let hex =
        include_str!("../../../../crates/category-domains/tests/fixtures/test-signing-key.hex")
            .trim();
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    seed
}

fn sign(bytes: &[u8]) -> Vec<u8> {
    use ed25519_dalek::{Signer, SigningKey};
    SigningKey::from_bytes(&seed())
        .sign(bytes)
        .to_bytes()
        .to_vec()
}

fn manifest_json(version: &str, not_after: i64, files: Vec<ManifestFile>) -> Vec<u8> {
    Manifest {
        format: MANIFEST_FORMAT,
        version: version.into(),
        key_id: "test".into(),
        not_after,
        files,
    }
    .to_json()
}

fn release_until(version: &str, not_after: i64) -> Release {
    let wikidata = wikidata(version);
    let ut1 = ut1(version);
    let manifest = manifest_json(
        version,
        not_after,
        vec![entry(WIKIDATA, &wikidata, 3), entry(UT1, &ut1, 1)],
    );
    Release {
        signature: sign(&manifest),
        manifest,
        wikidata,
        ut1,
    }
}

fn release(version: &str) -> Release {
    release_until(version, Timestamp::now().as_secs() + 30 * DAY)
}

fn serve(download: &FakeDownload, release: &Release, etag: &str) {
    download.serve(MANIFEST_NAME, release.manifest.clone(), Some(etag));
    download.serve(SIGNATURE_NAME, release.signature.clone(), None);
    download.serve(WIKIDATA, release.wikidata.clone(), None);
    download.serve(UT1, release.ut1.clone(), None);
}

fn store_release(dir: &Path, release: &Release) {
    write_release(
        dir,
        &[
            (WIKIDATA, Bytes::from(release.wikidata.clone())),
            (UT1, Bytes::from(release.ut1.clone())),
            (MANIFEST_NAME, Bytes::from(release.manifest.clone())),
            (SIGNATURE_NAME, Bytes::from(release.signature.clone())),
        ],
    )
    .unwrap();
}

struct Harness {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    store: Arc<SqliteStore>,
    archive: ArchiveId,
    insights: Insights,
    download: Arc<FakeDownload>,
}

async fn harness() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(tmp.path().join("db")).await.unwrap());
    let archive = store.create_archive("Mail").await.unwrap().id;
    Harness {
        dir: tmp.path().join(DIR),
        _tmp: tmp,
        store,
        archive,
        insights: Insights::open(&InsightsConfig::default(), &[], &TlsRoots::default())
            .unwrap()
            .unwrap(),
        download: Arc::new(FakeDownload::default()),
    }
}

impl Harness {
    async fn updater(&self, sources: &[&str], update: bool) -> Arc<Updater> {
        Updater::open_with(
            &self.insights,
            Parts {
                dir: self.dir.clone(),
                data_sources: sources.iter().map(|s| s.to_string()).collect(),
                target: update.then(|| UpdateTarget {
                    base_url: BASE.into(),
                    hosts: vec!["data.example.com".into()],
                    interval: Duration::from_secs(3600),
                    delay: Duration::from_secs(60),
                }),
                download: update.then(|| self.download.clone() as Arc<dyn DataDownload>),
                store: self.store.clone(),
            },
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn loaded(&self) -> Vec<(String, String)> {
        self.insights
            .domains
            .as_ref()
            .unwrap()
            .tables()
            .into_iter()
            .map(|t| (t.name, t.version))
            .collect()
    }

    async fn derive_fp(&self) -> String {
        self.insights
            .deriver
            .context(&*self.store, self.archive)
            .await
            .unwrap()
            .derive_fp()
            .to_owned()
    }

    fn files(&self) -> Vec<(String, Vec<u8>)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    async fn setting(&self, key: &str) -> Option<String> {
        self.store.setting(key).await.unwrap()
    }

    async fn woken(&self) -> bool {
        tokio::time::timeout(Duration::from_millis(50), self.insights.wake.wait())
            .await
            .is_ok()
    }
}

fn bundled_version() -> String {
    holtmail_category_domains::bundled()
        .unwrap()
        .version()
        .to_owned()
}

#[test]
fn reads_the_update_settings() {
    let config: crate::config::Config = toml::from_str("").unwrap();
    let categories = &config.insights.categories;
    assert!(!categories.update);
    assert_eq!(categories.data_sources, ["wikidata"]);
    let target = categories.update_target();
    assert_eq!(
        target.base_url,
        "https://github.com/holtmail/holtmail/releases/download/categories-latest/"
    );
    assert_eq!(
        target.hosts,
        [
            "github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com"
        ]
    );
    assert_eq!(target.interval, Duration::from_secs(168 * 3600));
    assert_eq!(target.delay, Duration::from_secs(300));

    let config: crate::config::Config = toml::from_str(
        "[insights.categories]\nupdate = true\ndata_sources = [\"ut1\", \"wikidata\"]",
    )
    .unwrap();
    assert!(config.insights.check().is_ok());
    assert_eq!(config.insights.categories.data_sources, ["ut1", "wikidata"]);

    for bad in [
        "data_sources = [\"dmoz\"]",
        "data_sources = [\"sources\"]",
        "data_sources = [\"wikidata\", \"wikidata\"]",
        "update = true\nsources = [\"signals\"]",
        "update = true\nsources = []",
        "update = true\ndata_sources = []",
    ] {
        let config: crate::config::Config =
            toml::from_str(&format!("[insights.categories]\n{bad}")).unwrap();
        assert!(config.insights.check().is_err(), "{bad}");
        assert!(
            Insights::open(&config.insights, &[], &TlsRoots::default()).is_err(),
            "{bad}"
        );
    }
    for fine in [
        "data_sources = []",
        "sources = [\"signals\"]\ndata_sources = [\"ut1\"]",
    ] {
        let config: crate::config::Config =
            toml::from_str(&format!("[insights.categories]\n{fine}")).unwrap();
        assert!(config.insights.check().is_ok(), "{fine}");
    }
}

#[cfg(not(feature = "insights-test-data"))]
#[test]
fn has_no_update_target_keys_without_the_feature() {
    for key in [
        "update_base_url = \"https://data.example.com/\"",
        "update_hosts = [\"data.example.com\"]",
        "update_interval_secs = 60",
        "update_delay_secs = 1",
    ] {
        assert!(
            toml::from_str::<crate::config::Config>(&format!("[insights.categories]\n{key}"))
                .is_err(),
            "{key}"
        );
    }
}

#[cfg(feature = "insights-test-data")]
#[test]
fn reads_update_target_keys_with_the_feature() {
    let config: crate::config::Config = toml::from_str(
        "[insights.categories]\nupdate = true\n\
         update_base_url = \"https://data.example.com/categories/\"\n\
         update_hosts = [\"data.example.com\"]\n\
         update_interval_secs = 60\nupdate_delay_secs = 1",
    )
    .unwrap();
    assert!(config.insights.check().is_ok());
    assert_eq!(
        config.insights.categories.update_target(),
        UpdateTarget {
            base_url: "https://data.example.com/categories/".into(),
            hosts: vec!["data.example.com".into()],
            interval: Duration::from_secs(60),
            delay: Duration::from_secs(1),
        }
    );
    assert_eq!(
        CategoriesConfig::default().update_target(),
        toml::from_str::<crate::config::Config>("")
            .unwrap()
            .insights
            .categories
            .update_target()
    );
    for bad in [
        "update_base_url = \"http://data.example.com/\"",
        "update_base_url = \"https://data.example.com/categories\"",
        "update_base_url = \"not a url/\"",
        "update_hosts = []",
        "update_interval_secs = 0",
    ] {
        let config: crate::config::Config =
            toml::from_str(&format!("[insights.categories]\n{bad}")).unwrap();
        assert!(config.insights.check().is_err(), "{bad}");
    }
}

#[tokio::test]
async fn starts_with_the_bundled_table_and_publishes_it() {
    let h = harness().await;
    let updater = h.updater(&["wikidata"], false).await;
    assert!(!updater.updates());
    assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
    let status = h.insights.api.data.get();
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].file, WIKIDATA);
    assert_eq!(status.files[0].license, "CC0-1.0");
    assert_eq!(status.files[0].origin, DataOrigin::Bundled);
    assert_eq!(status.files[0].attribution, None);
    assert!(status.files[0].rows > 1000);
    assert!(!status.updates.enabled);
    assert_eq!(status.updates.data_sources, ["wikidata"]);
    assert_eq!(status.updates.last_checked_at, None);

    let unlisted = Insights::open(
        &InsightsConfig {
            categories: CategoriesConfig {
                sources: vec!["signals".into()],
                ..Default::default()
            },
            ..Default::default()
        },
        &[],
        &TlsRoots::default(),
    )
    .unwrap()
    .unwrap();
    let parts = Parts {
        dir: h.dir.clone(),
        data_sources: vec!["wikidata".into()],
        target: None,
        download: None,
        store: h.store.clone(),
    };
    assert!(
        Updater::open_with(&unlisted, parts)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn waits_for_the_delay_or_the_rest_of_the_interval() {
    let h = harness().await;
    let updater = h.updater(&["wikidata"], true).await;
    let now = Timestamp::from_secs(1_800_000_000);
    assert_eq!(updater.first_wait(now, None), Duration::from_secs(60));
    assert_eq!(
        updater.first_wait(now, Some(Timestamp::from_secs(1_800_000_000 - 600))),
        Duration::from_secs(3000)
    );
    assert_eq!(
        updater.first_wait(now, Some(Timestamp::from_secs(1_800_000_000 - 3590))),
        Duration::from_secs(60)
    );
    assert_eq!(
        updater.first_wait(now, Some(Timestamp::from_secs(1_700_000_000))),
        Duration::from_secs(60)
    );
}

#[cfg(not(feature = "insights-test-data"))]
#[tokio::test]
async fn refuses_every_release_without_a_compiled_key() {
    let h = harness().await;
    let updater = h.updater(&["wikidata"], true).await;
    serve(&h.download, &release(NEWER), "\"v1\"");
    assert_eq!(updater.check().await, DataOutcome::Refused);
    assert!(h.files().is_empty());
    assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
    assert_eq!(h.setting(OUTCOME_SETTING).await.as_deref(), Some("refused"));
    assert!(h.setting(CHECKED_SETTING).await.is_some());
    assert_eq!(h.setting(ETAG_SETTING).await, None);
    let status = h.insights.api.data.get();
    assert_eq!(status.updates.last_outcome, Some(DataOutcome::Refused));
    assert!(status.updates.enabled);

    store_release(&h.dir, &release(NEWER));
    h.updater(&["wikidata", "ut1"], false).await;
    assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
}

#[tokio::test]
async fn records_a_failed_download() {
    let h = harness().await;
    let updater = h.updater(&["wikidata"], true).await;
    assert_eq!(updater.check().await, DataOutcome::Failed);
    assert_eq!(h.setting(OUTCOME_SETTING).await.as_deref(), Some("failed"));
    assert!(h.files().is_empty());
    let reopened = h.updater(&["wikidata"], true).await;
    assert_eq!(
        h.insights.api.data.get().updates.last_outcome,
        Some(DataOutcome::Failed)
    );
    assert!(reopened.checked_at().await.is_some());
}

#[cfg(feature = "insights-test-data")]
mod signed {
    use super::*;

    #[tokio::test]
    async fn loads_a_fresh_release_and_wakes_the_walk() {
        let h = harness().await;
        let updater = h.updater(&["wikidata"], true).await;
        let before = h.derive_fp().await;
        assert!(!h.woken().await);

        let fresh = release(NEWER);
        serve(&h.download, &fresh, "\"v1\"");
        assert_eq!(updater.check().await, DataOutcome::Updated);
        assert_eq!(
            h.download.requests(),
            [
                (format!("{BASE}{MANIFEST_NAME}"), None),
                (format!("{BASE}{SIGNATURE_NAME}"), None),
                (format!("{BASE}{WIKIDATA}"), None),
            ]
        );
        assert_eq!(h.loaded(), [("wikidata".to_owned(), NEWER.to_owned())]);
        assert_ne!(h.derive_fp().await, before);
        assert!(h.woken().await);
        assert_eq!(
            h.files(),
            [
                (MANIFEST_NAME.to_owned(), fresh.manifest.clone()),
                (SIGNATURE_NAME.to_owned(), fresh.signature.clone()),
                (WIKIDATA.to_owned(), fresh.wikidata.clone()),
            ]
        );
        assert_eq!(h.setting(ETAG_SETTING).await.as_deref(), Some("\"v1\""));
        assert_eq!(h.setting(VERSION_SETTING).await.as_deref(), Some(NEWER));
        assert_eq!(h.setting(OUTCOME_SETTING).await.as_deref(), Some("updated"));
        let status = h.insights.api.data.get();
        assert_eq!(status.files.len(), 1);
        assert_eq!(status.files[0].version, NEWER);
        assert_eq!(status.files[0].rows, 3);
        assert_eq!(status.files[0].origin, DataOrigin::Downloaded);
        assert_eq!(status.updates.last_outcome, Some(DataOutcome::Updated));
        assert!(status.updates.last_checked_at.is_some());

        let after = h.derive_fp().await;
        assert_eq!(updater.check().await, DataOutcome::NotModified);
        assert_eq!(
            h.download.requests(),
            [(format!("{BASE}{MANIFEST_NAME}"), Some("\"v1\"".to_owned()))]
        );
        assert_eq!(h.derive_fp().await, after);
        assert!(!h.woken().await);
        assert_eq!(h.files()[0].1, fresh.manifest);
        assert_eq!(
            h.setting(OUTCOME_SETTING).await.as_deref(),
            Some("not_modified")
        );
        assert_eq!(
            h.insights.api.data.get().updates.last_outcome,
            Some(DataOutcome::NotModified)
        );

        serve(&h.download, &fresh, "\"v1-copy\"");
        assert_eq!(updater.check().await, DataOutcome::NotModified);
        assert_eq!(
            h.setting(ETAG_SETTING).await.as_deref(),
            Some("\"v1-copy\"")
        );

        let reopened = Harness {
            insights: Insights::open(&InsightsConfig::default(), &[], &TlsRoots::default())
                .unwrap()
                .unwrap(),
            ..h
        };
        reopened.updater(&["wikidata"], false).await;
        assert_eq!(
            reopened.loaded(),
            [("wikidata".to_owned(), NEWER.to_owned())]
        );
        assert_eq!(reopened.derive_fp().await, after);
    }

    #[tokio::test]
    async fn refuses_bad_releases_and_changes_nothing() {
        let h = harness().await;
        let updater = h.updater(&["wikidata", "ut1"], true).await;
        serve(&h.download, &release(NEWER), "\"v1\"");
        assert_eq!(updater.check().await, DataOutcome::Updated);
        let files = h.files();
        assert_eq!(files.len(), 4);
        let loaded = h.loaded();
        let fp = h.derive_fp().await;
        assert!(h.woken().await);
        let now = Timestamp::now().as_secs();

        let older = release(OLDER);
        let expired = release_until(NEWEST, now - 1);
        let mut bad_signature = release(NEWEST);
        bad_signature.signature[0] ^= 1;
        let mut sha = release(NEWEST);
        let last = sha.wikidata.len() - 1;
        sha.wikidata[last] ^= 1;
        let mut foreign = release(NEWEST);
        let other = table_file("wikidata", NEWEST, &["example.org"]);
        foreign.manifest = manifest_json(
            NEWEST,
            now + 30 * DAY,
            vec![
                entry(WIKIDATA, &foreign.wikidata, 3),
                ManifestFile {
                    name: "other-categories.tsv.gz".into(),
                    ..entry(WIKIDATA, &other, 1)
                },
            ],
        );
        foreign.signature = sign(&foreign.manifest);
        let mut short = release(NEWEST);
        short.ut1.push(0);

        for (case, bad) in [
            ("rollback", older),
            ("expired", expired),
            ("signature", bad_signature),
            ("sha256", sha),
            ("file name", foreign),
            ("size", short),
        ] {
            serve(&h.download, &bad, &format!("\"{case}\""));
            assert_eq!(updater.check().await, DataOutcome::Refused, "{case}");
            assert_eq!(h.files(), files, "{case}");
            assert_eq!(h.loaded(), loaded, "{case}");
            assert_eq!(h.derive_fp().await, fp, "{case}");
            assert!(!h.woken().await, "{case}");
            assert_eq!(h.setting(ETAG_SETTING).await.as_deref(), Some("\"v1\""));
            assert_eq!(h.setting(VERSION_SETTING).await.as_deref(), Some(NEWER));
            assert_eq!(
                h.setting(OUTCOME_SETTING).await.as_deref(),
                Some("refused"),
                "{case}"
            );
            assert_eq!(
                h.insights.api.data.get().updates.last_outcome,
                Some(DataOutcome::Refused)
            );
            h.download.requests();
        }

        serve(&h.download, &release(NEWEST), "\"v2\"");
        assert_eq!(updater.check().await, DataOutcome::Updated);
        assert_eq!(
            h.loaded(),
            [
                ("ut1".to_owned(), NEWEST.to_owned()),
                ("wikidata".to_owned(), NEWEST.to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn prefers_the_newer_of_bundled_and_downloaded() {
        let h = harness().await;
        store_release(&h.dir, &release(OLDER));
        h.updater(&["wikidata"], false).await;
        assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
        assert_eq!(
            h.insights.api.data.get().files[0].origin,
            DataOrigin::Bundled
        );

        store_release(&h.dir, &release(NEWER));
        h.updater(&["wikidata"], false).await;
        assert_eq!(h.loaded(), [("wikidata".to_owned(), NEWER.to_owned())]);
        assert_eq!(
            h.insights.api.data.get().files[0].origin,
            DataOrigin::Downloaded
        );

        h.updater(&[], false).await;
        assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
    }

    #[tokio::test]
    async fn uses_ut1_only_when_listed() {
        let h = harness().await;
        store_release(&h.dir, &release(OLDER));
        h.updater(&["wikidata"], false).await;
        assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);

        h.updater(&["wikidata", "ut1"], false).await;
        assert_eq!(
            h.loaded(),
            [
                ("ut1".to_owned(), OLDER.to_owned()),
                ("wikidata".to_owned(), bundled_version())
            ]
        );
        let status = h.insights.api.data.get();
        let ut1 = &status.files[0];
        assert_eq!(ut1.file, UT1);
        assert_eq!(ut1.license, "CC-BY-SA-4.0");
        assert_eq!(ut1.origin, DataOrigin::Downloaded);
        assert_eq!(
            ut1.attribution.as_deref(),
            Some("Contains data from UT1 (Université Toulouse Capitole), CC BY-SA 4.0")
        );
        assert_eq!(status.updates.data_sources, ["wikidata", "ut1"]);

        let h = harness().await;
        let updater = h.updater(&["wikidata"], true).await;
        serve(&h.download, &release(NEWER), "\"v1\"");
        assert_eq!(updater.check().await, DataOutcome::Updated);
        assert!(h.files().iter().all(|(name, _)| name != UT1));
        assert!(
            h.download
                .requests()
                .iter()
                .all(|(url, _)| !url.ends_with(UT1))
        );
    }

    #[tokio::test]
    async fn ignores_files_a_crash_left_behind_the_manifest() {
        let h = harness().await;
        let first = release(NEWER);
        store_release(&h.dir, &first);
        let second = release(NEWEST);
        std::fs::write(h.dir.join(WIKIDATA), &second.wikidata).unwrap();

        let updater = h.updater(&["wikidata", "ut1"], true).await;
        assert_eq!(
            h.loaded(),
            [
                ("ut1".to_owned(), NEWER.to_owned()),
                ("wikidata".to_owned(), bundled_version())
            ]
        );

        h.store
            .set_setting(ETAG_SETTING, Some("\"v2\""))
            .await
            .unwrap();
        serve(&h.download, &second, "\"v2\"");
        assert_eq!(updater.check().await, DataOutcome::Updated);
        assert_eq!(h.download.requests()[0].1, None);
        assert_eq!(
            h.loaded(),
            [
                ("ut1".to_owned(), NEWEST.to_owned()),
                ("wikidata".to_owned(), NEWEST.to_owned())
            ]
        );

        std::fs::write(h.dir.join(SIGNATURE_NAME), [0u8; 64]).unwrap();
        h.updater(&["wikidata", "ut1"], false).await;
        assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
    }

    #[tokio::test]
    async fn repairs_a_damaged_release_of_the_same_version() {
        let h = harness().await;
        let updater = h.updater(&["wikidata"], true).await;
        let fresh = release(NEWER);
        serve(&h.download, &fresh, "\"v1\"");
        assert_eq!(updater.check().await, DataOutcome::Updated);

        std::fs::write(h.dir.join(WIKIDATA), b"damaged").unwrap();
        let updater = h.updater(&["wikidata"], true).await;
        assert_eq!(h.loaded(), [("wikidata".to_owned(), bundled_version())]);
        h.download.requests();
        assert_eq!(updater.check().await, DataOutcome::Updated);
        assert_eq!(h.download.requests()[0].1, None);
        assert_eq!(h.loaded(), [("wikidata".to_owned(), NEWER.to_owned())]);

        std::fs::remove_dir_all(&h.dir).unwrap();
        let updater = h.updater(&["wikidata"], true).await;
        serve(&h.download, &release(OLDER), "\"old\"");
        assert_eq!(updater.check().await, DataOutcome::Refused);
        assert!(h.files().is_empty());
    }
}
