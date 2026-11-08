use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_alias::{
    AliasChange, AliasDetector, AliasDirectory, AliasError, AliasOptions, Capabilities, NewAlias,
};
use holtmail_aliases::{Connect, ManageError, ProviderInfo};
use holtmail_api::{AliasProvider, Aliases, MailboxError, NewService, ServiceChange};
use holtmail_core::{AliasServiceId, ArchiveId, TlsRoots};
use holtmail_store::{AliasService, KnownAlias, Store};
use serde::Deserialize;

use crate::config::Services;
use crate::mailboxes::Runner;

/// `[aliases]`: recognising mail forwarded by alias services, and listing the aliases of
/// connected services.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasConfig {
    /// Recognise forwarded mail; `false` records no aliases for new mail.
    #[serde(default = "yes")]
    pub detect: bool,
    /// Minutes between listings of every connected service's aliases; 0 lists them only on
    /// request.
    #[serde(default = "default_interval")]
    pub sync_interval_minutes: u64,
    /// Extra DKIM domains trusted for SimpleLogin, for a self-hosted instance.
    #[serde(default)]
    pub simplelogin_signing_domains: Vec<String>,
    /// Extra DKIM domains trusted for addy.io, for a self-hosted instance or custom domains.
    #[serde(default)]
    pub addy_signing_domains: Vec<String>,
    /// Extra mask domains for Firefox Relay, for a self-hosted instance.
    #[serde(default)]
    pub relay_domains: Vec<String>,
    /// Own rules, for catch-all domains or other services.
    #[serde(default)]
    pub rules: Vec<holtmail_alias_rules::Rule>,
}

impl Default for AliasConfig {
    fn default() -> Self {
        Self {
            detect: true,
            sync_interval_minutes: default_interval(),
            simplelogin_signing_domains: Vec::new(),
            addy_signing_domains: Vec::new(),
            relay_domains: Vec::new(),
            rules: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

fn default_interval() -> u64 {
    360
}

/// The detectors the config asks for: the built-in ones, then the rules, in order.
pub fn detectors(config: &AliasConfig) -> anyhow::Result<Vec<Arc<dyn AliasDetector>>> {
    if !config.detect {
        return Ok(Vec::new());
    }
    let mut out: Vec<Arc<dyn AliasDetector>> = vec![
        Arc::new(holtmail_alias_simplelogin::SimpleLoginDetector::new(
            &config.simplelogin_signing_domains,
        )),
        Arc::new(holtmail_alias_addy::AddyDetector::new(
            &config.addy_signing_domains,
        )),
        Arc::new(holtmail_alias_relay::RelayDetector::new(
            &config.relay_domains,
        )),
        Arc::new(holtmail_alias_duck::DuckDetector::default()),
        Arc::new(holtmail_alias_apple::AppleDetector::default()),
    ];
    for rule in &config.rules {
        out.push(Arc::new(holtmail_alias_rules::RuleDetector::new(
            rule.clone(),
        )?));
    }
    Ok(out)
}

/// Changes whenever the detectors may answer differently for stored mail.
pub fn fingerprint(detectors: &[Arc<dyn AliasDetector>]) -> String {
    let joined: Vec<String> = detectors
        .iter()
        .map(|d| format!("{}={}", d.provider(), d.version()))
        .collect();
    blake3::hash(joined.join("\n").as_bytes())
        .to_hex()
        .to_string()
}

fn full() -> Capabilities {
    Capabilities {
        create_random: true,
        create_custom: true,
        note: true,
        name: true,
        for_domain: false,
        switch: true,
        mailboxes: true,
        delete: true,
    }
}

/// Every provider Holtmail knows, those with an API first.
pub fn providers() -> Vec<ProviderInfo> {
    let info = |id: &str,
                name: &str,
                url: Option<&str>,
                caps: Option<Capabilities>,
                label: &str,
                help: &str,
                detects: bool| ProviderInfo {
        id: id.into(),
        name: name.into(),
        default_url: url.map(str::to_owned),
        capabilities: caps,
        key_label: label.into(),
        key_help: help.into(),
        detects,
    };
    vec![
        info(
            holtmail_alias_simplelogin::PROVIDER,
            "SimpleLogin",
            Some(holtmail_alias_simplelogin::DEFAULT_URL),
            Some(full()),
            "API key",
            "SimpleLogin → Settings → API Keys; Proton Pass aliases use the SimpleLogin account linked to Proton",
            true,
        ),
        info(
            holtmail_alias_addy::PROVIDER,
            "addy.io",
            Some(holtmail_alias_addy::DEFAULT_URL),
            Some(full()),
            "API key",
            "addy.io → Settings → API → Create New API Key",
            true,
        ),
        info(
            holtmail_alias_relay::PROVIDER,
            "Firefox Relay",
            Some(holtmail_alias_relay::DEFAULT_URL),
            Some(Capabilities {
                create_random: true,
                note: true,
                for_domain: true,
                switch: true,
                delete: true,
                ..Capabilities::default()
            }),
            "API key",
            "relay.firefox.com → Settings → API Key",
            true,
        ),
        info(
            holtmail_alias_fastmail::PROVIDER,
            "Fastmail Masked Email",
            Some(holtmail_alias_fastmail::DEFAULT_URL),
            Some(Capabilities {
                create_random: true,
                create_custom: true,
                note: true,
                for_domain: true,
                switch: true,
                delete: true,
                ..Capabilities::default()
            }),
            "API token",
            "Fastmail → Settings → Privacy & Security → API tokens, with the Masked Email scope",
            false,
        ),
        info(
            holtmail_alias_duck::PROVIDER,
            "DuckDuckGo Email Protection",
            None,
            None,
            "",
            "No API; add addresses by hand",
            true,
        ),
        info(
            holtmail_alias_apple::PROVIDER,
            "Apple Hide My Email",
            None,
            None,
            "",
            "No API; copy the addresses from iCloud settings → Hide My Email and add them by hand",
            true,
        ),
    ]
}

/// Builds directories for the providers with an API, trusting `roots` besides the built-in ones.
pub fn connect(roots: TlsRoots) -> Connect {
    Arc::new(
        move |provider: &str,
              url: &str,
              key: &str|
              -> Result<Arc<dyn AliasDirectory>, AliasError> {
            Ok(match provider {
                holtmail_alias_simplelogin::PROVIDER => Arc::new(
                    holtmail_alias_simplelogin::SimpleLoginDirectory::new(url, key, &roots)?,
                ),
                holtmail_alias_addy::PROVIDER => {
                    Arc::new(holtmail_alias_addy::AddyDirectory::new(url, key, &roots)?)
                }
                holtmail_alias_relay::PROVIDER => {
                    Arc::new(holtmail_alias_relay::RelayDirectory::new(url, key, &roots)?)
                }
                holtmail_alias_fastmail::PROVIDER => Arc::new(
                    holtmail_alias_fastmail::FastmailDirectory::new(url, key, &roots)?,
                ),
                other => {
                    return Err(AliasError::Invalid(format!(
                        "{other} has no API Holtmail can use"
                    )));
                }
            })
        },
    )
}

fn mailbox_error(error: ManageError) -> MailboxError {
    match error {
        ManageError::Invalid(message) => MailboxError::Invalid(message),
        ManageError::Store(error) => MailboxError::Backend(error.to_string()),
    }
}

#[async_trait]
impl Aliases for Runner {
    fn providers(&self) -> Vec<AliasProvider> {
        self.services()
            .aliases
            .providers()
            .iter()
            .map(|p| AliasProvider {
                id: p.id.clone(),
                name: p.name.clone(),
                default_url: p.default_url.clone(),
                capabilities: p.capabilities,
                key_label: p.key_label.clone(),
                key_help: p.key_help.clone(),
                detects: p.detects,
            })
            .collect()
    }

    async fn add_service(
        &self,
        archive: ArchiveId,
        service: NewService,
    ) -> Result<AliasService, MailboxError> {
        let added = self
            .services()
            .aliases
            .add_service(
                archive,
                &service.provider,
                &service.name,
                service.base_url.as_deref(),
                &service.key,
            )
            .await
            .map_err(mailbox_error)?;
        tracing::info!(service = %added.id, provider = %added.provider, "alias service connected");
        self.refresh_people(archive);
        Ok(added)
    }

    async fn update_service(
        &self,
        id: AliasServiceId,
        change: ServiceChange,
    ) -> Result<AliasService, MailboxError> {
        let updated = self
            .services()
            .aliases
            .update_service(
                id,
                change.name.as_deref(),
                change.base_url.as_deref(),
                change.key.as_deref(),
            )
            .await
            .map_err(mailbox_error)?;
        self.refresh_people(updated.archive_id);
        Ok(updated)
    }

    async fn sync_service(&self, id: AliasServiceId) -> Result<AliasService, MailboxError> {
        let synced = self
            .services()
            .aliases
            .sync_service(id)
            .await
            .map_err(mailbox_error)?;
        self.refresh_people(synced.archive_id);
        Ok(synced)
    }

    async fn alias_options(
        &self,
        id: AliasServiceId,
    ) -> Result<(Capabilities, AliasOptions), MailboxError> {
        self.services()
            .aliases
            .options(id)
            .await
            .map_err(mailbox_error)
    }

    async fn create_alias(
        &self,
        id: AliasServiceId,
        alias: NewAlias,
    ) -> Result<KnownAlias, MailboxError> {
        let made = self
            .services()
            .aliases
            .create_alias(id, &alias)
            .await
            .map_err(mailbox_error)?;
        tracing::info!(service = %id, "alias created at the service");
        Ok(made)
    }

    async fn update_alias(
        &self,
        archive: ArchiveId,
        address: &str,
        change: AliasChange,
    ) -> Result<KnownAlias, MailboxError> {
        self.services()
            .aliases
            .update_alias(archive, address, &change)
            .await
            .map_err(mailbox_error)
    }

    async fn delete_alias(
        &self,
        archive: ArchiveId,
        address: &str,
        at_service: bool,
    ) -> Result<(), MailboxError> {
        self.services()
            .aliases
            .delete_alias(archive, address, at_service)
            .await
            .map_err(mailbox_error)
    }
    fn default_strategy(&self, provider: &str) -> holtmail_alias::ReplyStrategy {
        self.services().aliases.default_strategy(provider)
    }

    async fn can_reverse(
        &self,
        archive: holtmail_core::ArchiveId,
        alias: &str,
    ) -> Result<bool, MailboxError> {
        self.services()
            .aliases
            .can_reverse(archive, alias)
            .await
            .map_err(mailbox_error)
    }

    async fn reverse_alias(
        &self,
        archive: holtmail_core::ArchiveId,
        alias: &str,
        contact: &str,
    ) -> Result<String, MailboxError> {
        self.services()
            .aliases
            .reverse_alias(archive, alias, contact)
            .await
            .map_err(mailbox_error)
    }
}

/// Lists every connected service's aliases every `minutes`, from a minute after start.
pub async fn run_schedule(runner: Runner, minutes: u64) {
    if minutes == 0 {
        return;
    }
    tokio::time::sleep(Duration::from_secs(60)).await;
    loop {
        match runner.services().aliases.sync_all().await {
            Ok((archives, failed)) => {
                if failed > 0 {
                    tracing::warn!(failed, "some alias services could not be listed");
                }
                for archive in archives {
                    runner.refresh_people(archive);
                }
            }
            Err(error) => tracing::warn!(%error, "alias services not listed"),
        }
        tokio::time::sleep(Duration::from_secs(minutes.saturating_mul(60))).await;
    }
}

/// The setting holding the detectors' fingerprint the stored mail was last detected with.
pub const DETECT_SETTING: &str = "alias_detectors";

/// Detects the alias of every message again with the current detectors, then stores their
/// fingerprint. Returns how many messages changed; their archives' people are rebuilt by the
/// caller.
pub async fn detect_all(
    store: &dyn Store,
    ingest: &holtmail_ingest::Ingest,
    fingerprint: &str,
) -> anyhow::Result<(u64, Vec<ArchiveId>)> {
    let mut changed = 0u64;
    let mut archives = Vec::new();
    for archive in store.list_archives().await? {
        let mut after = None;
        let before = changed;
        loop {
            let page = store
                .list_messages(&holtmail_store::MessageQuery {
                    archive_id: archive.id,
                    folder_id: None,
                    label: None,
                    category: None,
                    after,
                    limit: 500,
                })
                .await?;
            for message in &page.items {
                match ingest.refresh_alias(message.id).await {
                    Ok(true) => changed += 1,
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(message = %message.id, %error, "alias not detected")
                    }
                }
            }
            ingest.commit().await?;
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        if changed > before {
            archives.push(archive.id);
        }
    }
    store.set_setting(DETECT_SETTING, Some(fingerprint)).await?;
    Ok((changed, archives))
}

/// Runs [`detect_all`] at start when the detectors changed since the last run.
pub async fn detect_if_needed(runner: Runner) {
    let services: &Arc<Services> = runner.services();
    let fingerprint = fingerprint(&services.detectors);
    match services.store.setting(DETECT_SETTING).await {
        Ok(Some(done)) if done == fingerprint => return,
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "could not tell whether aliases need detecting");
            return;
        }
    }
    tracing::info!("detecting aliases in archived mail");
    let started = std::time::Instant::now();
    match detect_all(&*services.store, &services.ingest(), &fingerprint).await {
        Ok((changed, archives)) => {
            tracing::info!(
                changed,
                seconds = started.elapsed().as_secs(),
                "aliases detected"
            );
            if changed > 0
                && let Some(insights) = &services.insights
            {
                insights.changed();
            }
            for archive in archives {
                runner.refresh_people(archive);
            }
        }
        Err(error) => {
            tracing::warn!(%error, "aliases not detected; it is tried again at the next start")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_follows_the_rules() {
        let plain = detectors(&AliasConfig::default()).unwrap();
        assert_eq!(plain.len(), 5);
        let config: AliasConfig = toml::from_str(
            r#"
            relay_domains = ["relay.example.org"]
            [[rules]]
            provider = "own-domain"
            domains = ["alias.example.org"]
            "#,
        )
        .unwrap();
        let with_rule = detectors(&config).unwrap();
        assert_eq!(with_rule.len(), 6);
        assert_ne!(fingerprint(&plain), fingerprint(&with_rule));
        assert_eq!(
            fingerprint(&plain),
            fingerprint(&detectors(&AliasConfig::default()).unwrap())
        );
        let off = AliasConfig {
            detect: false,
            ..AliasConfig::default()
        };
        assert!(detectors(&off).unwrap().is_empty());
        let bad: AliasConfig =
            toml::from_str("[[rules]]\nprovider = \"Bad Id\"\ndomains = [\"x.example\"]").unwrap();
        assert!(detectors(&bad).is_err());
    }

    #[test]
    fn every_provider_with_an_api_can_be_reached() {
        let connect = connect(TlsRoots::default());
        for provider in providers() {
            let built = connect(
                &provider.id,
                provider
                    .default_url
                    .as_deref()
                    .unwrap_or("https://alias.example.org"),
                "key",
            );
            assert_eq!(
                built.is_ok(),
                provider.capabilities.is_some(),
                "{}",
                provider.id
            );
            if let Ok(directory) = built {
                assert_eq!(
                    Some(directory.capabilities()),
                    provider.capabilities,
                    "{}",
                    provider.id
                );
            }
        }
    }
}
