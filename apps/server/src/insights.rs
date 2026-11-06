use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, bail};
use holtmail_blob::BlobStore;
use holtmail_core::MessageId;
use holtmail_insight::{InsightExtractor, KindTag};
use holtmail_insight_code::CodeExtractor;
use holtmail_insight_unsubscribe::UnsubscribeExtractor;
use holtmail_insights::Runner;
use holtmail_store::Store;
use serde::Deserialize;

/// The extractors the server knows, in the order they always run: the order is part of the
/// fingerprint, so it never follows the config.
const EXTRACTORS: &[&str] = &["unsubscribe", "code"];

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
}

impl Default for InsightsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_possible: BTreeMap::new(),
            extractors: default_extractors(),
        }
    }
}

fn yes() -> bool {
    true
}

fn default_extractors() -> Vec<String> {
    EXTRACTORS.iter().map(|&name| name.to_owned()).collect()
}

impl InsightsConfig {
    /// Fails on an unknown or repeated extractor or kind name.
    pub fn check(&self) -> anyhow::Result<()> {
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
        Ok(())
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
                    "code" => Arc::new(CodeExtractor::new()),
                    _ => unreachable!("every name in EXTRACTORS is built"),
                }
            })
            .collect();
        Runner::new(extractors)
    }
}

/// The insights the server computes: the runner and what the API needs to serve them.
pub struct Insights {
    pub runner: Arc<Runner>,
    pub api: holtmail_api::InsightsConfig,
}

impl Insights {
    /// The runner and API config for `config`; `None` when insights are off. Installs the panic
    /// hook that keeps message content out of panic reports.
    pub fn open(config: &InsightsConfig) -> anyhow::Result<Option<Self>> {
        config.check()?;
        if !config.enabled {
            return Ok(None);
        }
        holtmail_insights::install_panic_hook();
        let runner = Arc::new(config.runner());
        let mut api = holtmail_api::InsightsConfig::new(runner.extract_fp());
        for (kind, score) in &config.show_possible {
            let kind = KindTag::from_name(kind).context("checked above")?;
            api.show_possible.insert(kind, *score);
        }
        Ok(Some(Self { runner, api }))
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
        assert_eq!(config.insights.extractors, ["unsubscribe", "code"]);
        assert!(config.insights.show_possible.is_empty());

        let config: crate::config::Config = toml::from_str(
            "[insights]\nextractors = [\"code\"]\nshow_possible.code = 400\nenabled = false",
        )
        .unwrap();
        assert!(!config.insights.enabled);
        assert_eq!(config.insights.show_possible["code"], 400);
        assert!(Insights::open(&config.insights).unwrap().is_none());

        assert!(toml::from_str::<crate::config::Config>("[insights]\nenable = true").is_err());
        for bad in [
            "extractors = [\"links\"]",
            "extractors = [\"code\", \"code\"]",
            "show_possible.codes = 1",
        ] {
            let config: crate::config::Config =
                toml::from_str(&format!("[insights]\n{bad}")).unwrap();
            assert!(config.insights.check().is_err(), "{bad}");
        }
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
            both(&["code", "unsubscribe"]),
            both(&["unsubscribe", "code"])
        );
        assert_ne!(both(&["code"]), both(&["unsubscribe", "code"]));

        let mut config = InsightsConfig::default();
        config.show_possible.insert("code".into(), 300);
        let insights = Insights::open(&config).unwrap().unwrap();
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

    #[tokio::test]
    async fn explains_without_values_or_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let index = Arc::new(TantivyIndex::open(dir.path().join("index")).unwrap());
        let archive = store.create_archive("Mail").await.unwrap().id;
        let id = Ingest::new(blobs.clone(), store.clone(), index)
            .ingest(archive, Bytes::from(MAIL), &Placement::default())
            .await
            .unwrap()
            .message_id;
        let runner = InsightsConfig::default().runner();
        let extracted = runner.extract(&holtmail_mime::parse(MAIL.as_bytes()).unwrap());
        assert!(
            extracted.items.to_json().contains("482913"),
            "the fixture has no code to hide"
        );

        let report = explain(&*store, &*blobs, &runner, id).await.unwrap();
        assert!(!report.is_empty());
        for secret in [
            "482913",
            "unsub.example.net",
            "lists.example.net",
            "shop.example.com",
            "q7k2",
            "ada@example.org",
            "verification code is",
        ] {
            assert!(!report.contains(secret), "{secret} leaked:\n{report}");
        }
        assert!(
            explain(&*store, &*blobs, &runner, MessageId::new())
                .await
                .is_err()
        );
    }
}
