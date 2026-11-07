use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use holtmail_blob::BlobStore;
use holtmail_category::Categorizer;
use holtmail_category_domains::DomainsCategorizer;
use holtmail_category_signals::SignalsCategorizer;
use holtmail_core::{MessageId, TlsRoots};
use holtmail_fetch::OneClick;
use holtmail_fetch_http::{HttpOneClick, OneClickOptions};
use holtmail_insight::{InsightExtractor, KindTag};
use holtmail_insight_code::CodeExtractor;
use holtmail_insight_links::LinksExtractor;
use holtmail_insight_parcel::ParcelExtractor;
use holtmail_insight_pass::PassExtractor;
use holtmail_insight_schemaorg::SchemaOrgExtractor;
use holtmail_insight_unsubscribe::UnsubscribeExtractor;
use holtmail_insights::{DeriveConfig, Deriver, Runner};
use holtmail_store::{CategoryPolicy, Store};
use serde::Deserialize;

use crate::walk::WalkWake;

/// The extractors the server knows, in the order they always run: the order is part of the
/// fingerprint, so it never follows the config.
const EXTRACTORS: &[&str] = &[
    "unsubscribe",
    "schemaorg",
    "pass",
    "parcel",
    "links",
    "code",
];

/// The categorizers the server knows, in the order they always vote: the order is part of the
/// fingerprint. `signals` always votes.
const SOURCES: &[&str] = &[holtmail_category_signals::ID, holtmail_category_domains::ID];

const MAX_AUTHSERV_IDS: usize = 16;
const MAX_AUTHSERV_ID_BYTES: usize = 255;

/// `[insights]`: reading codes, unsubscribe links and the like out of archived mail.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsightsConfig {
    /// `false` computes, stores and serves no insights.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Per kind, the lowest score at which a `Possible` item is shown too; none by default.
    #[serde(default)]
    pub show_possible: BTreeMap<String, u16>,
    /// The extractors to run, by name; they always run in the order of [`EXTRACTORS`].
    #[serde(default = "default_extractors")]
    pub extractors: Vec<String>,
    /// The authserv-ids whose `Authentication-Results` are trusted for mail of import accounts
    /// other than the SMTP one; none by default, so imported mail has trust `unknown`.
    #[serde(default)]
    pub import_authserv_ids: Vec<String>,
    /// `[insights.categories]`: the kind and topics of messages.
    #[serde(default)]
    pub categories: CategoriesConfig,
    /// `false` sends no one-click unsubscribe requests: the kill switch. On by default.
    #[serde(default = "yes")]
    pub one_click: bool,
    /// Networks in CIDR form that one-click requests may reach although they are not public,
    /// for the integration tests. The key exists only in builds with the `insights-test-data`
    /// feature; elsewhere it is an unknown key.
    #[cfg(feature = "insights-test-data")]
    #[serde(default)]
    pub one_click_test_networks: Vec<String>,
}

impl Default for InsightsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_possible: BTreeMap::new(),
            extractors: default_extractors(),
            import_authserv_ids: Vec::new(),
            categories: CategoriesConfig::default(),
            one_click: true,
            #[cfg(feature = "insights-test-data")]
            one_click_test_networks: Vec::new(),
        }
    }
}

/// `[insights.categories]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoriesConfig {
    /// `false` derives, applies and serves no `health` topic; stored override bits are kept.
    #[serde(default = "yes")]
    pub health: bool,
    /// The categorizers that vote, by name; `signals` votes whether listed or not. By default
    /// every one in [`SOURCES`].
    #[serde(default = "default_sources")]
    pub sources: Vec<String>,
    /// `true` checks for signed category data releases on a schedule and loads newer ones. Off by
    /// default; needs `domains` among the sources.
    #[serde(default)]
    pub update: bool,
    /// The downloaded tables to use, by table name among `wikidata` and `ut1`; `["wikidata"]` by
    /// default. The bundled `wikidata` table loads whatever this says.
    #[serde(default = "default_data_sources")]
    pub data_sources: Vec<String>,
    /// The address the release files are fetched from, ending in `/`; only with the
    /// `insights-test-data` feature.
    #[cfg(feature = "insights-test-data")]
    #[serde(default = "default_update_base_url")]
    pub update_base_url: String,
    /// The hosts downloads and their redirects may reach; only with the `insights-test-data`
    /// feature.
    #[cfg(feature = "insights-test-data")]
    #[serde(default = "default_update_hosts")]
    pub update_hosts: Vec<String>,
    /// Seconds between checks; only with the `insights-test-data` feature.
    #[cfg(feature = "insights-test-data")]
    #[serde(default = "default_update_interval_secs")]
    pub update_interval_secs: u64,
    /// The fewest seconds between start and the first check; only with the `insights-test-data`
    /// feature.
    #[cfg(feature = "insights-test-data")]
    #[serde(default = "default_update_delay_secs")]
    pub update_delay_secs: u64,
}

impl Default for CategoriesConfig {
    fn default() -> Self {
        Self {
            health: true,
            sources: default_sources(),
            update: false,
            data_sources: default_data_sources(),
            #[cfg(feature = "insights-test-data")]
            update_base_url: default_update_base_url(),
            #[cfg(feature = "insights-test-data")]
            update_hosts: default_update_hosts(),
            #[cfg(feature = "insights-test-data")]
            update_interval_secs: default_update_interval_secs(),
            #[cfg(feature = "insights-test-data")]
            update_delay_secs: default_update_delay_secs(),
        }
    }
}

/// Where and how often the server looks for category data releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateTarget {
    /// The address the release files are fetched from, ending in `/`.
    pub base_url: String,
    /// The hosts downloads and their redirects may reach.
    pub hosts: Vec<String>,
    /// Time between checks.
    pub interval: std::time::Duration,
    /// The least time between start and the first check.
    pub delay: std::time::Duration,
}

impl CategoriesConfig {
    /// The release address, pinned hosts and schedule: the defaults in a release build, the
    /// configured values with the `insights-test-data` feature.
    pub fn update_target(&self) -> UpdateTarget {
        #[cfg(feature = "insights-test-data")]
        return UpdateTarget {
            base_url: self.update_base_url.clone(),
            hosts: self.update_hosts.clone(),
            interval: std::time::Duration::from_secs(self.update_interval_secs),
            delay: std::time::Duration::from_secs(self.update_delay_secs),
        };
        #[cfg(not(feature = "insights-test-data"))]
        UpdateTarget {
            base_url: default_update_base_url(),
            hosts: default_update_hosts(),
            interval: std::time::Duration::from_secs(default_update_interval_secs()),
            delay: std::time::Duration::from_secs(default_update_delay_secs()),
        }
    }
}

fn default_data_sources() -> Vec<String> {
    vec![holtmail_category_data::DATA_FILES[0].table.to_owned()]
}

fn default_update_base_url() -> String {
    "https://github.com/holtmail/holtmail/releases/download/categories-latest/".to_owned()
}

fn default_update_hosts() -> Vec<String> {
    holtmail_fetch_http::GITHUB_RELEASE_HOSTS
        .iter()
        .map(|&host| host.to_owned())
        .collect()
}

fn default_update_interval_secs() -> u64 {
    168 * 60 * 60
}

fn default_update_delay_secs() -> u64 {
    5 * 60
}

fn yes() -> bool {
    true
}

fn default_sources() -> Vec<String> {
    SOURCES.iter().map(|&name| name.to_owned()).collect()
}

/// Fails unless `ids` are at most 16 authserv-ids of 1 to 255 printable ASCII characters without
/// spaces, none repeated ignoring case. `key` names the setting in the error.
pub fn check_authserv_ids(key: &str, ids: &[String]) -> anyhow::Result<()> {
    if ids.len() > MAX_AUTHSERV_IDS {
        bail!("{key} lists more than {MAX_AUTHSERV_IDS} authserv-ids");
    }
    for (n, id) in ids.iter().enumerate() {
        if id.is_empty()
            || id.len() > MAX_AUTHSERV_ID_BYTES
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            bail!(
                "{key}: {id:?} is not an authserv-id (1 to 255 printable ASCII characters without spaces)"
            );
        }
        if ids[..n].iter().any(|seen| seen.eq_ignore_ascii_case(id)) {
            bail!("{key} names {id:?} twice");
        }
    }
    Ok(())
}

fn default_extractors() -> Vec<String> {
    EXTRACTORS.iter().map(|&name| name.to_owned()).collect()
}

impl InsightsConfig {
    /// Fails on an unknown or repeated extractor, kind or source name, a malformed
    /// `import_authserv_ids` or, with the `insights-test-data` feature, a malformed
    /// `one_click_test_networks` entry.
    pub fn check(&self) -> anyhow::Result<()> {
        #[cfg(feature = "insights-test-data")]
        self.test_networks()?;
        for (n, name) in self.extractors.iter().enumerate() {
            if !EXTRACTORS.contains(&name.as_str()) {
                bail!(
                    "insights.extractors: unknown extractor {name:?}; known: {}",
                    EXTRACTORS.join(", ")
                );
            }
            if self.extractors[..n].contains(name) {
                bail!("insights.extractors names {name:?} twice");
            }
        }
        for kind in self.show_possible.keys() {
            KindTag::from_name(kind)
                .with_context(|| format!("insights.show_possible: unknown kind {kind:?}"))?;
        }
        for (n, name) in self.categories.sources.iter().enumerate() {
            if !SOURCES.contains(&name.as_str()) {
                bail!(
                    "insights.categories.sources: unknown source {name:?}; known: {}",
                    SOURCES.join(", ")
                );
            }
            if self.categories.sources[..n].contains(name) {
                bail!("insights.categories.sources names {name:?} twice");
            }
        }
        self.check_updates()?;
        check_authserv_ids("insights.import_authserv_ids", &self.import_authserv_ids)
    }

    fn check_updates(&self) -> anyhow::Result<()> {
        let categories = &self.categories;
        for (n, name) in categories.data_sources.iter().enumerate() {
            if holtmail_category_data::data_file_for_table(name).is_none() {
                bail!(
                    "insights.categories.data_sources: unknown data source {name:?}; known: {}",
                    holtmail_category_data::DATA_FILES
                        .iter()
                        .map(|file| file.table)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if categories.data_sources[..n].contains(name) {
                bail!("insights.categories.data_sources names {name:?} twice");
            }
        }
        if categories.update {
            if !self.lists(holtmail_category_domains::ID) {
                bail!(
                    "insights.categories.update needs \"domains\" in insights.categories.sources"
                );
            }
            if categories.data_sources.is_empty() {
                bail!("insights.categories.update needs at least one data source");
            }
        }
        #[cfg(feature = "insights-test-data")]
        {
            let url = url::Url::parse(&categories.update_base_url)
                .context("insights.categories.update_base_url")?;
            if url.scheme() != "https" || !categories.update_base_url.ends_with('/') {
                bail!("insights.categories.update_base_url must be an https address ending in /");
            }
            if categories.update_hosts.is_empty() {
                bail!("insights.categories.update_hosts lists no host");
            }
            if categories.update_interval_secs == 0 {
                bail!("insights.categories.update_interval_secs must be above 0");
            }
        }
        Ok(())
    }

    /// The bundled domain categorizer when `domains` is among the sources, else `None`. Every
    /// call loads the table compiled into the binary again and logs its name, version, row count
    /// and load time; [`Insights::open`] calls it once. Fails, naming the table and the broken
    /// rule, when the table does not load.
    pub fn domains(&self) -> anyhow::Result<Option<Arc<DomainsCategorizer>>> {
        if !self.lists(holtmail_category_domains::ID) {
            return Ok(None);
        }
        let started = Instant::now();
        let domains = DomainsCategorizer::with_bundled().map_err(|error| {
            anyhow::anyhow!(
                "insights.categories: the bundled domain table wikidata does not load: {error}"
            )
        })?;
        let millis = started.elapsed().as_millis() as u64;
        for table in domains.tables() {
            tracing::info!(
                table = %table.name,
                version = %table.version,
                rows = table.rows,
                millis,
                "domain table loaded"
            );
        }
        Ok(Some(Arc::new(domains)))
    }

    #[cfg(feature = "insights-test-data")]
    fn test_networks(&self) -> anyhow::Result<Vec<(std::net::IpAddr, u8)>> {
        self.one_click_test_networks
            .iter()
            .map(|text| {
                holtmail_fetch_http::parse_cidr(text).map_err(|error| {
                    anyhow::anyhow!("insights.one_click_test_networks: {text:?}: {error}")
                })
            })
            .collect()
    }

    /// The one-click sender when `one_click` is on, else `None`: a direct connection that
    /// never uses a proxy, whatever `image_proxy` says, trusting the built-in roots and
    /// `roots`.
    pub fn one_click(&self, roots: &TlsRoots) -> anyhow::Result<Option<Arc<dyn OneClick>>> {
        if !self.one_click {
            return Ok(None);
        }
        let options = OneClickOptions {
            roots: roots.clone(),
            #[cfg(feature = "insights-test-data")]
            allowed_test_networks: self.test_networks()?,
            ..OneClickOptions::default()
        };
        let sender = HttpOneClick::new(options)
            .map_err(|error| anyhow::anyhow!("insights.one_click: {error}"))?;
        Ok(Some(Arc::new(sender)))
    }

    fn lists(&self, source: &str) -> bool {
        source == holtmail_insights::ALWAYS_ON_SOURCE
            || self.categories.sources.iter().any(|n| n == source)
    }

    /// The stage-B deriver over the configured sources in the order of [`SOURCES`], with
    /// `smtp_authserv_ids` for the SMTP listener's account. `domains` votes when it is given and
    /// `domains` is among the sources; no table is loaded here. The order of the configured
    /// names never changes the fingerprint.
    pub fn deriver(
        &self,
        smtp_authserv_ids: &[String],
        domains: Option<Arc<DomainsCategorizer>>,
    ) -> Deriver {
        let categorizers = SOURCES
            .iter()
            .filter(|name| self.lists(name))
            .filter_map(|&name| -> Option<Arc<dyn Categorizer>> {
                match name {
                    holtmail_category_signals::ID => Some(Arc::new(SignalsCategorizer::new())),
                    holtmail_category_domains::ID => {
                        domains.clone().map(|d| d as Arc<dyn Categorizer>)
                    }
                    _ => unreachable!("every name in SOURCES is built"),
                }
            })
            .collect();
        Deriver::new(
            categorizers,
            DeriveConfig {
                health: self.categories.health,
                sources: SOURCES
                    .iter()
                    .filter(|name| self.categories.sources.iter().any(|n| n == *name))
                    .map(|&name| name.to_owned())
                    .collect(),
                import_authserv_ids: self.import_authserv_ids.clone(),
                smtp_authserv_ids: smtp_authserv_ids.to_vec(),
                smtp_account_name: crate::smtp::ACCOUNT_NAME.to_owned(),
            },
        )
    }

    /// The runner with the configured extractors in their fixed order, whether or not insights
    /// are enabled.
    pub fn runner(&self) -> Runner {
        let extractors = EXTRACTORS
            .iter()
            .filter(|name| self.extractors.iter().any(|n| n == *name))
            .map(|&name| -> Arc<dyn InsightExtractor> {
                match name {
                    "unsubscribe" => Arc::new(UnsubscribeExtractor::new()),
                    "schemaorg" => Arc::new(SchemaOrgExtractor::new()),
                    "pass" => Arc::new(PassExtractor::new()),
                    "parcel" => Arc::new(ParcelExtractor::new()),
                    "links" => Arc::new(LinksExtractor::new()),
                    "code" => Arc::new(CodeExtractor::new()),
                    _ => unreachable!("every name in EXTRACTORS is built"),
                }
            })
            .collect();
        Runner::new(extractors)
    }
}

/// The insights the server computes: the runner, the stage-B deriver, what wakes the walk and
/// what the API needs to serve them.
pub struct Insights {
    pub runner: Arc<Runner>,
    pub deriver: Arc<Deriver>,
    /// The domain categorizer the deriver votes with, when `domains` is among the sources.
    /// [`crate::category_data`] calls [`DomainsCategorizer::reload`] on it; the deriver sees the
    /// new tables at its next context.
    pub domains: Option<Arc<DomainsCategorizer>>,
    pub wake: Arc<WalkWake>,
    pub api: holtmail_api::InsightsConfig,
}

impl Insights {
    /// The runner, deriver and API config for `config`, with `smtp_authserv_ids` trusted for the
    /// SMTP listener's account and the one-click sender trusting `roots` too; `None` when
    /// insights are off. Installs the panic hook that keeps message content out of panic reports.
    pub fn open(
        config: &InsightsConfig,
        smtp_authserv_ids: &[String],
        roots: &TlsRoots,
    ) -> anyhow::Result<Option<Self>> {
        config.check()?;
        check_authserv_ids("smtp.trusted_authserv_ids", smtp_authserv_ids)?;
        if !config.enabled {
            return Ok(None);
        }
        holtmail_insights::install_panic_hook();
        let runner = Arc::new(config.runner());
        let domains = config.domains()?;
        let deriver = Arc::new(config.deriver(smtp_authserv_ids, domains.clone()));
        let mut api = holtmail_api::InsightsConfig::new(runner.extract_fp());
        api.health = config.categories.health;
        api.deriver = Some(deriver.clone());
        api.one_click = config.one_click(roots)?;
        for (kind, score) in &config.show_possible {
            let kind = KindTag::from_name(kind).context("checked above")?;
            api.show_possible.insert(kind, *score);
        }
        Ok(Some(Self {
            runner,
            deriver,
            domains,
            wake: Arc::new(WalkWake::default()),
            api,
        }))
    }

    /// The policy overrides are applied under.
    pub fn policy(&self) -> CategoryPolicy {
        self.deriver.policy()
    }

    /// Wakes the walk and the stage-B pass after it.
    pub fn changed(&self) {
        self.wake.signal();
    }
}

/// What `runner` finds in the stored message `id`, as rule names, positions, scores and value
/// shapes, never the values, hosts or URLs themselves.
pub async fn explain(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    runner: &Runner,
    id: MessageId,
) -> anyhow::Result<String> {
    let message = store
        .get_message(id)
        .await?
        .with_context(|| format!("no message {id}"))?;
    let raw = blobs
        .get(&message.blob_id)
        .await?
        .with_context(|| format!("the bytes of message {id} are missing"))?;
    let parsed = holtmail_mime::parse(&raw).context("the message does not parse")?;
    Ok(holtmail_insights::with_message(id, || {
        runner.explain(&parsed)
    }))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use holtmail_blob_fs::FsBlobStore;
    use holtmail_index_tantivy::TantivyIndex;
    use holtmail_ingest::{Ingest, Placement};
    use holtmail_store::ArchiveStore;
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    #[test]
    fn reads_the_section() {
        let config: crate::config::Config = toml::from_str("").unwrap();
        assert!(config.insights.enabled);
        assert_eq!(
            config.insights.extractors,
            [
                "unsubscribe",
                "schemaorg",
                "pass",
                "parcel",
                "links",
                "code"
            ]
        );
        assert!(config.insights.show_possible.is_empty());

        let config: crate::config::Config = toml::from_str(
            "[insights]\nextractors = [\"code\"]\nshow_possible.code = 400\nenabled = false",
        )
        .unwrap();
        assert!(!config.insights.enabled);
        assert_eq!(config.insights.show_possible["code"], 400);
        assert!(
            Insights::open(&config.insights, &[], &TlsRoots::default())
                .unwrap()
                .is_none()
        );

        assert!(toml::from_str::<crate::config::Config>("[insights]\nenable = true").is_err());
        for bad in [
            "extractors = [\"calendar\"]",
            "extractors = [\"code\", \"code\"]",
            "show_possible.codes = 1",
        ] {
            let config: crate::config::Config =
                toml::from_str(&format!("[insights]\n{bad}")).unwrap();
            assert!(config.insights.check().is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_trust_and_category_settings() {
        let config: crate::config::Config = toml::from_str("").unwrap();
        assert!(config.insights.import_authserv_ids.is_empty());
        assert!(config.insights.categories.health);
        assert_eq!(config.insights.categories.sources, ["signals", "domains"]);
        let insights = Insights::open(&config.insights, &[], &TlsRoots::default())
            .unwrap()
            .unwrap();
        assert!(insights.api.health);
        assert_eq!(insights.deriver.config().sources, ["signals", "domains"]);
        assert!(insights.domains.is_some());
        assert!(insights.deriver.config().smtp_authserv_ids.is_empty());
        assert_eq!(
            insights.deriver.config().smtp_account_name,
            crate::smtp::ACCOUNT_NAME
        );

        let config: crate::config::Config = toml::from_str(
            "[insights]\nimport_authserv_ids = [\"mx.example.org\"]\n\
             [insights.categories]\nhealth = false\nsources = []\n\
             [smtp]\nlisten = \"127.0.0.1:2525\"\ntrusted_authserv_ids = [\"relay.example.net\"]",
        )
        .unwrap();
        let smtp = config.smtp.as_ref().unwrap();
        assert_eq!(smtp.trusted_authserv_ids, ["relay.example.net"]);
        let insights = Insights::open(
            &config.insights,
            &smtp.trusted_authserv_ids,
            &TlsRoots::default(),
        )
        .unwrap()
        .unwrap();
        assert!(!insights.api.health);
        assert!(!insights.policy().health);
        let derive = insights.deriver.config();
        assert_eq!(derive.import_authserv_ids, ["mx.example.org"]);
        assert_eq!(derive.smtp_authserv_ids, ["relay.example.net"]);
        assert!(derive.sources.is_empty());
        assert!(insights.domains.is_none());

        assert!(
            toml::from_str::<crate::config::Config>("[insights.categories]\nhealthy = true")
                .is_err()
        );
        let long = format!("import_authserv_ids = [\"{}\"]", "a".repeat(256));
        let many = format!(
            "import_authserv_ids = [{}]",
            (0..17)
                .map(|n| format!("\"mx{n}.example.org\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for bad in [
            "categories.sources = [\"wikidata\"]",
            "categories.sources = [\"signals\", \"signals\"]",
            "categories.sources = [\"domains\", \"signals\", \"domains\"]",
            "import_authserv_ids = [\"\"]",
            "import_authserv_ids = [\"mx example.org\"]",
            "import_authserv_ids = [\"mx.exämple.org\"]",
            "import_authserv_ids = [\"MX.example.org\", \"mx.example.org\"]",
            &long,
            &many,
        ] {
            let config: crate::config::Config =
                toml::from_str(&format!("[insights]\n{bad}")).unwrap();
            assert!(config.insights.check().is_err(), "{bad}");
            assert!(
                Insights::open(&config.insights, &[], &TlsRoots::default()).is_err(),
                "{bad}"
            );
        }
        assert!(
            Insights::open(
                &InsightsConfig::default(),
                &["a b".into()],
                &TlsRoots::default()
            )
            .is_err()
        );
        let ok = format!("import_authserv_ids = [\"{}\"]", "a".repeat(255));
        let config: crate::config::Config = toml::from_str(&format!("[insights]\n{ok}")).unwrap();
        assert!(config.insights.check().is_ok());
    }

    #[test]
    fn reads_the_one_click_switch() {
        let config: crate::config::Config = toml::from_str("").unwrap();
        assert!(config.insights.one_click);
        let insights = Insights::open(&config.insights, &[], &TlsRoots::default())
            .unwrap()
            .unwrap();
        assert!(insights.api.one_click.is_some());

        let config: crate::config::Config =
            toml::from_str("[insights]\none_click = false").unwrap();
        assert!(!config.insights.one_click);
        let insights = Insights::open(&config.insights, &[], &TlsRoots::default())
            .unwrap()
            .unwrap();
        assert!(insights.api.one_click.is_none());
        assert!(
            config
                .insights
                .one_click(&TlsRoots::default())
                .unwrap()
                .is_none()
        );
    }

    #[cfg(not(feature = "insights-test-data"))]
    #[test]
    fn has_no_test_networks_without_the_feature() {
        assert!(
            toml::from_str::<crate::config::Config>(
                "[insights]\none_click_test_networks = [\"11.88.77.0/24\"]"
            )
            .is_err()
        );
    }

    #[cfg(feature = "insights-test-data")]
    #[test]
    fn reads_test_networks_with_the_feature() {
        let config: crate::config::Config = toml::from_str(
            "[insights]\none_click_test_networks = [\"11.88.77.0/24\", \"fd00::/8\"]",
        )
        .unwrap();
        assert_eq!(
            config.insights.test_networks().unwrap(),
            [
                ("11.88.77.0".parse().unwrap(), 24),
                ("fd00::".parse().unwrap(), 8)
            ]
        );
        assert!(config.insights.check().is_ok());
        assert!(
            config
                .insights
                .one_click(&TlsRoots::default())
                .unwrap()
                .is_some()
        );
        for bad in ["\"11.88.77.0/33\"", "\"example.org/8\"", "\"fd00::/129\""] {
            let config: crate::config::Config =
                toml::from_str(&format!("[insights]\none_click_test_networks = [{bad}]")).unwrap();
            assert!(config.insights.check().is_err(), "{bad}");
            assert!(
                Insights::open(&config.insights, &[], &TlsRoots::default()).is_err(),
                "{bad}"
            );
        }
    }

    fn sources(list: &[&str]) -> InsightsConfig {
        InsightsConfig {
            categories: CategoriesConfig {
                sources: list.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn domains_vote_only_when_listed_and_change_the_fingerprint() {
        for list in [
            &["domains"][..],
            &["signals", "domains"],
            &["domains", "signals"],
        ] {
            assert!(sources(list).check().is_ok(), "{list:?}");
            assert!(sources(list).domains().unwrap().is_some(), "{list:?}");
        }
        assert!(sources(&["signals"]).domains().unwrap().is_none());
        assert!(sources(&[]).domains().unwrap().is_none());

        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("db")).await.unwrap();
        let archive = store.create_archive("Mail").await.unwrap().id;
        let fp = |config: InsightsConfig| {
            let store = &store;
            async move {
                let domains = config.domains().unwrap();
                let deriver = config.deriver(&[], domains);
                deriver
                    .context(store, archive)
                    .await
                    .unwrap()
                    .derive_fp()
                    .to_owned()
            }
        };
        let with = fp(InsightsConfig::default()).await;
        let without = fp(sources(&["signals"])).await;
        assert_ne!(with, without);
        assert_eq!(with, fp(sources(&["domains", "signals"])).await);
        assert_ne!(without, fp(sources(&[])).await);

        let listed_but_absent = sources(&["signals", "domains"]).deriver(&[], None);
        let ctx = listed_but_absent.context(&store, archive).await.unwrap();
        assert_ne!(ctx.derive_fp(), with);

        let insights = Insights::open(&InsightsConfig::default(), &[], &TlsRoots::default())
            .unwrap()
            .unwrap();
        let tables = insights.domains.as_ref().unwrap().tables();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "wikidata");
        let ctx = insights.deriver.context(&store, archive).await.unwrap();
        assert_eq!(ctx.derive_fp(), with);
    }

    #[test]
    fn times_the_bundled_table() {
        let started = std::time::Instant::now();
        let domains = DomainsCategorizer::with_bundled().unwrap();
        let elapsed = started.elapsed();
        let rows: usize = domains.tables().iter().map(|t| t.rows).sum();
        println!("bundled domain table: {rows} rows in {elapsed:?}");
    }

    #[test]
    fn loading_checks_the_smtp_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("holtmail.toml");
        let smtp = "[smtp]\nlisten = \"127.0.0.1:2525\"\n";
        std::fs::write(
            &path,
            format!("{smtp}trusted_authserv_ids = [\"mx example.org\"]"),
        )
        .unwrap();
        let error = crate::config::Config::load(Some(&path)).unwrap_err();
        assert!(
            error.to_string().contains("smtp.trusted_authserv_ids"),
            "{error}"
        );
        std::fs::write(
            &path,
            format!("{smtp}trusted_authserv_ids = [\"mx.example.org\"]"),
        )
        .unwrap();
        let config = crate::config::Config::load(Some(&path)).unwrap();
        assert_eq!(
            config.smtp.unwrap().trusted_authserv_ids,
            ["mx.example.org"]
        );
    }

    #[test]
    fn runs_extractors_in_a_fixed_order() {
        let both = |list: &[&str]| {
            InsightsConfig {
                extractors: list.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
            .runner()
            .extract_fp()
        };
        assert_eq!(
            both(&[
                "code",
                "links",
                "pass",
                "unsubscribe",
                "parcel",
                "schemaorg"
            ]),
            both(&[
                "unsubscribe",
                "schemaorg",
                "pass",
                "parcel",
                "links",
                "code"
            ])
        );
        assert_eq!(both(&["pass", "schemaorg"]), both(&["schemaorg", "pass"]));
        assert_ne!(both(&["schemaorg"]), both(&["pass"]));
        assert_eq!(both(&["links", "parcel"]), both(&["parcel", "links"]));
        assert_ne!(both(&["code"]), both(&["unsubscribe", "code"]));
        assert_ne!(both(&["links"]), both(&["parcel", "links"]));
        assert_eq!(
            InsightsConfig::default().runner().extract_fp(),
            both(&[
                "unsubscribe",
                "schemaorg",
                "pass",
                "parcel",
                "links",
                "code"
            ])
        );

        let mut config = InsightsConfig::default();
        config.show_possible.insert("code".into(), 300);
        let insights = Insights::open(&config, &[], &TlsRoots::default())
            .unwrap()
            .unwrap();
        assert_eq!(insights.api.fingerprint, insights.runner.extract_fp());
        assert_eq!(
            insights.api.show_possible[&KindTag::from_name("code").unwrap()],
            300
        );
    }

    const MAIL: &str = "From: Example Shop <no-reply@shop.example.com>\r\n\
        To: ada@example.org\r\n\
        Subject: Your verification code\r\n\
        Message-ID: <code1@shop.example.com>\r\n\
        Date: Mon, 10 Mar 2025 09:00:00 +0000\r\n\
        List-Unsubscribe: <https://unsub.example.net/u/q7k2>, <mailto:leave@lists.example.net>\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\r\n\
        Your verification code is 482913. It expires in 10 minutes.\r\n";

    const SHIPPED: &str = "From: Example Shop <orders@shop.example.com>\r\n\
        To: ada@example.org\r\n\
        Subject: Your order has shipped\r\n\
        Message-ID: <ship1@shop.example.com>\r\n\
        Date: Mon, 10 Mar 2025 09:00:00 +0000\r\n\
        Content-Type: text/html; charset=utf-8\r\n\r\n\
        <p>Your order is on its way.</p>\
        <p>Tracking number: 1ZV77Y21K219503089</p>\
        <table><tr><td bgcolor=\"#1a73e8\" style=\"padding:12px 24px\">\
        <a href=\"https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fparcels.example.org%2Ft%2Fz9w4&amp;data=x\">\
        Track your order</a></td></tr></table>\r\n";

    const BOOKED: &str = "From: Example Air <bookings@air.example.com>\r\n\
        To: ada@example.org\r\n\
        Subject: Your flight to Lisbon\r\n\
        Message-ID: <flight1@air.example.com>\r\n\
        Date: Mon, 10 Mar 2025 09:00:00 +0000\r\n\
        Content-Type: text/html; charset=utf-8\r\n\r\n\
        <html><head><script type=\"application/ld+json\">\
        {\"@context\": \"https://schema.org\", \"@type\": \"FlightReservation\", \
        \"reservationNumber\": \"K4XB2Q\", \"underName\": {\"@type\": \"Person\", \"name\": \"Ada Lovelace-Example\"}, \
        \"airplaneSeat\": \"14C\", \"url\": \"https://trips.example.net/k4xb2q\", \
        \"reservationFor\": {\"@type\": \"Flight\", \"flightNumber\": \"482\", \
        \"airline\": {\"@type\": \"Airline\", \"iataCode\": \"XA\"}, \
        \"departureAirport\": {\"@type\": \"Airport\", \"iataCode\": \"FRA\"}, \
        \"arrivalAirport\": {\"@type\": \"Airport\", \"iataCode\": \"LIS\"}, \
        \"departureTime\": \"2025-04-14T09:30:00+01:00\"}, \
        \"potentialAction\": {\"@type\": \"CheckInAction\", \"target\": \"https://checkin.example.net/c/77q\"}}\
        </script></head><body><p>Your booking code is K4XB2Q.</p></body></html>\r\n";

    #[tokio::test]
    async fn explains_without_values_or_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let index = Arc::new(TantivyIndex::open(dir.path().join("index")).unwrap());
        let archive = store.create_archive("Mail").await.unwrap().id;
        let ingest = Ingest::new(blobs.clone(), store.clone(), index);
        let runner = InsightsConfig::default().runner();
        let cases: [(&str, &[&str], &[&str]); 3] = [
            (
                MAIL,
                &["482913"],
                &[
                    "482913",
                    "unsub.example.net",
                    "lists.example.net",
                    "shop.example.com",
                    "q7k2",
                    "ada@example.org",
                    "verification code is",
                ],
            ),
            (
                SHIPPED,
                &["1ZV77Y21K219503089", "parcels.example.org", "outlook.com"],
                &[
                    "1ZV77Y21K219503089",
                    "parcels.example.org",
                    "z9w4",
                    "safelinks",
                    "outlook.com",
                    "shop.example.com",
                    "ada@example.org",
                    "Track your order",
                ],
            ),
            (
                BOOKED,
                &[
                    "K4XB2Q",
                    "Ada Lovelace-Example",
                    "FRA",
                    "checkin.example.net",
                ],
                &[
                    "K4XB2Q",
                    "Lovelace",
                    "14C",
                    "FRA",
                    "LIS",
                    "XA",
                    "trips.example.net",
                    "checkin.example.net",
                    "77q",
                    "air.example.com",
                    "2025-04-14",
                ],
            ),
        ];
        for (mail, found, secrets) in cases {
            let id = ingest
                .ingest(archive, Bytes::from(mail), &Placement::default())
                .await
                .unwrap()
                .message_id;
            let extracted = runner.extract(&holtmail_mime::parse(mail.as_bytes()).unwrap());
            let json = extracted.items.to_json();
            for value in found {
                assert!(json.contains(value), "the fixture has no {value} to hide");
            }

            let report = explain(&*store, &*blobs, &runner, id).await.unwrap();
            assert!(!report.is_empty());
            for secret in secrets {
                assert!(!report.contains(secret), "{secret} leaked:\n{report}");
            }
        }
        assert!(
            explain(&*store, &*blobs, &runner, MessageId::new())
                .await
                .is_err()
        );

        let id = ingest
            .ingest(archive, Bytes::from(MAIL), &Placement::default())
            .await
            .unwrap()
            .message_id;
        let insights = Insights::open(&InsightsConfig::default(), &[], &TlsRoots::default())
            .unwrap()
            .unwrap();
        assert!(insights.domains.is_some());
        let unlisted = sources(&["signals"]).runner();
        assert_eq!(
            explain(&*store, &*blobs, &insights.runner, id)
                .await
                .unwrap(),
            explain(&*store, &*blobs, &unlisted, id).await.unwrap()
        );
    }
}
