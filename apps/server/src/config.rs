use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use anyhow::{Context, bail};
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_blob_s3::{S3BlobStore, S3Config};
use holtmail_core::TlsRoots;
use holtmail_extract::Extractor;
use holtmail_extract_basic::BasicExtractor;
use holtmail_index::SearchIndex;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::Ingest;
use holtmail_oauth::{Client, Provider};
use holtmail_oauth_http::HttpTokenEndpoint;
use holtmail_secret::SecretBox;
use holtmail_secret_local::LocalSecretBox;
use holtmail_serve_net::{DEFAULT_RELOAD_INTERVAL, TlsCertificates, TrustedProxies};
use holtmail_store::{SettingsStore, Store};
use holtmail_store_sqlite::SqliteStore;
use serde::Deserialize;

use crate::image_proxy::ProxiedFetcher;
use crate::oauth::OAuth;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub web_dir: Option<PathBuf>,
    /// The address people open, e.g. `https://mail.example.com`; used in the printed setup link
    /// and, when https, to always mark the session cookie `Secure`.
    #[serde(default)]
    pub public_url: Option<String>,
    #[serde(default)]
    pub blobs: BlobConfig,
    /// Minutes between syncs of accounts without their own interval; 0 syncs them only on
    /// request. When set, the web app shows it and cannot change it.
    #[serde(default)]
    pub sync_interval_minutes: Option<u64>,
    /// Outbound proxy for remote images, e.g. `socks5h://127.0.0.1:9050`. When set, the web app
    /// shows it and cannot change it.
    #[serde(default)]
    pub image_proxy: Option<String>,
    /// OAuth clients for Gmail and Microsoft sign-in; Thunderbird's public clients by default.
    #[serde(default)]
    pub oauth: OAuthConfig,
    /// A PEM file of CA certificates trusted for outbound TLS besides the built-in roots, for
    /// mail, contacts, OAuth, S3 and image servers with a private CA.
    #[serde(default)]
    pub tls_ca_file: Option<PathBuf>,
    /// An SMTP listener that archives mail sent or journaled to it; off when missing.
    #[serde(default)]
    pub smtp: Option<crate::smtp::SmtpConfig>,
    /// Watching accounts for changes (IMAP IDLE, JMAP push, WebDAV-Push) and how many accounts
    /// sync at once.
    #[serde(default)]
    pub watch: WatchConfig,
    /// Recognising mail forwarded by alias services and listing connected services' aliases.
    #[serde(default)]
    pub aliases: crate::aliases::AliasConfig,
    /// Codes, unsubscribe links and the like read out of archived mail.
    #[serde(default)]
    pub insights: crate::insights::InsightsConfig,
    /// Instance switches for writing to mail, contacts and calendars servers.
    #[serde(default)]
    pub write: WriteConfig,
    /// Serving mail, calendars and contacts to clients: certificates, proxies and who may make
    /// devices.
    #[serde(default)]
    pub serve: ServeConfig,
    #[serde(skip)]
    pub tls_roots: TlsRoots,
}

/// `[watch]`: push from mail, contacts and calendars servers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    /// Watch accounts whose servers can push; `false` leaves only the sync intervals.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Accounts syncing at the same time, mail and contacts runs counted alike.
    #[serde(default = "default_parallel_syncs")]
    pub parallel_syncs: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            parallel_syncs: default_parallel_syncs(),
        }
    }
}

/// `[serve]`: what every protocol listener shares. Every listener stays off until its own
/// section names it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServeConfig {
    /// PEM certificate chain for the protocol listeners; needs `tls_key`. Renewed files are
    /// picked up without a restart.
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    /// PEM private key for `tls_cert`.
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    /// Read PROXY protocol v2 headers on the protocol listeners; needs `trusted_proxies`.
    #[serde(default)]
    pub proxy_protocol: bool,
    /// IP addresses and CIDR ranges of proxies whose PROXY headers and `X-Forwarded-For` are
    /// believed. Nobody by default.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Whether members may make devices for their own archives; `false` limits new devices to
    /// the owner and admins.
    #[serde(default = "yes")]
    pub members: bool,
    /// Keep flag changes a mail client makes that cannot go upstream (the account lacks
    /// `mail.flags`, writing is off, or the message is only in Holtmail) as the user's own
    /// local flags; `false` refuses them instead.
    #[serde(default = "yes")]
    pub local_flags: bool,
    /// Mail domains the operator controls, such as `example.com`. Thunderbird autoconfig is
    /// served only for addresses at these domains, and `holtmail dns` prints their records.
    /// None by default.
    #[serde(default)]
    pub mail_domains: Vec<String>,
    /// SMTP submission for mail clients; off without this section.
    #[serde(default)]
    pub submit: Option<SubmitConfig>,
    /// IMAP for mail clients; off without this section.
    #[serde(default)]
    pub imap: Option<ImapConfig>,
    /// JMAP for mail clients on the main listener; off without this section.
    #[serde(default)]
    pub jmap: Option<JmapConfig>,
    /// CalDAV and CardDAV for calendar and contacts apps on the main listener; off without
    /// this section.
    #[serde(default)]
    pub dav: Option<DavConfig>,
}

/// `[serve.dav]`: CalDAV and CardDAV at `/dav`, `/.well-known/caldav` and
/// `/.well-known/carddav`. Needs `public_url`, because redirects and hrefs are absolute.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DavConfig {
    /// Mounts the routes; `true` by default, so an empty section turns DAV on.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Whether clients may write; `false` serves every collection read-only.
    #[serde(default = "yes")]
    pub edits: bool,
    /// Whether calendar-auto-schedule may be offered: Holtmail then delivers the invitations
    /// and answers calendar apps write, for devices with `write:pim` and `send:mail`.
    #[serde(default = "yes")]
    pub scheduling: bool,
}

/// `[serve.jmap]`: JMAP for mail clients at `/jmap` and `/.well-known/jmap`. Needs
/// `public_url`, because the session resource carries absolute URLs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JmapConfig {
    /// Mounts the routes; `true` by default, so an empty section turns JMAP on.
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// `[serve.imap]`: IMAP for mail clients.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapConfig {
    /// Addresses with TLS from the first byte, such as `0.0.0.0:993`.
    #[serde(default)]
    pub listen_tls: Vec<SocketAddr>,
    /// Addresses with `STARTTLS`, such as `0.0.0.0:143`.
    #[serde(default)]
    pub listen_starttls: Vec<SocketAddr>,
    /// The host clients are told to use; the host of `public_url` by default.
    #[serde(default)]
    pub hostname: Option<String>,
}

/// `[serve.submit]`: SMTP submission for mail clients.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitConfig {
    /// Addresses with TLS from the first byte, such as `0.0.0.0:465`.
    #[serde(default)]
    pub listen_tls: Vec<SocketAddr>,
    /// Addresses with `STARTTLS`, such as `0.0.0.0:587`.
    #[serde(default)]
    pub listen_starttls: Vec<SocketAddr>,
    /// How long a submitted message can be undone before it leaves; 0 by default, because the
    /// mail client already had its own chance.
    #[serde(default)]
    pub undo_seconds: u64,
    /// Messages per user in any hour, across their devices.
    #[serde(default = "default_messages_per_hour")]
    pub messages_per_hour: u32,
    /// Envelope recipients per user in any day, across their devices.
    #[serde(default = "default_recipients_per_day")]
    pub recipients_per_day: u32,
    /// The name in the greeting; the host of `public_url` by default.
    #[serde(default)]
    pub hostname: Option<String>,
}

fn default_messages_per_hour() -> u32 {
    200
}

fn default_recipients_per_day() -> u32 {
    2000
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            tls_cert: None,
            tls_key: None,
            proxy_protocol: false,
            trusted_proxies: Vec::new(),
            members: true,
            local_flags: true,
            mail_domains: Vec::new(),
            submit: None,
            imap: None,
            jmap: None,
            dav: None,
        }
    }
}

impl ServeConfig {
    /// Refuses a section that cannot work, and reads the proxy list.
    pub fn validate(&self) -> anyhow::Result<TrustedProxies> {
        anyhow::ensure!(
            self.tls_cert.is_some() == self.tls_key.is_some(),
            "[serve] tls_cert and tls_key go together"
        );
        anyhow::ensure!(
            !self.proxy_protocol || !self.trusted_proxies.is_empty(),
            "[serve] proxy_protocol needs trusted_proxies"
        );
        for domain in self.mail_domains() {
            anyhow::ensure!(
                !domain.is_empty()
                    && domain.contains('.')
                    && domain
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
                "[serve] mail_domains: {domain:?} is not a domain name"
            );
        }
        if let Some(submit) = &self.submit {
            self.check_listeners("serve.submit", &submit.listen_tls, &submit.listen_starttls)?;
        }
        if let Some(imap) = &self.imap {
            self.check_listeners("serve.imap", &imap.listen_tls, &imap.listen_starttls)?;
        }
        TrustedProxies::parse(&self.trusted_proxies).context("[serve] trusted_proxies")
    }

    fn check_listeners(
        &self,
        section: &str,
        tls: &[SocketAddr],
        starttls: &[SocketAddr],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !tls.is_empty() || !starttls.is_empty(),
            "[{section}] needs listen_tls or listen_starttls"
        );
        let everything_local = tls.is_empty() && starttls.iter().all(|a| a.ip().is_loopback());
        anyhow::ensure!(
            self.tls_cert.is_some() || everything_local,
            "[{section}] needs [serve] tls_cert and tls_key, unless it only listens with \
             STARTTLS on loopback addresses"
        );
        Ok(())
    }

    /// `mail_domains`, trimmed and lowercase, without repeats.
    pub fn mail_domains(&self) -> Vec<String> {
        let mut domains: Vec<String> = Vec::new();
        for domain in &self.mail_domains {
            let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
            if !domains.contains(&domain) {
                domains.push(domain);
            }
        }
        domains
    }

    /// What the API acts on.
    pub fn api_options(&self) -> anyhow::Result<holtmail_api::ServeOptions> {
        Ok(holtmail_api::ServeOptions {
            members: self.members,
            trusted_proxies: self.validate()?,
            local_flags: self.local_flags,
            ..holtmail_api::ServeOptions::default()
        })
    }

    /// Loads the certificate, and keeps it fresh for as long as the returned value or a clone
    /// of it lives. `None` without `tls_cert`.
    pub async fn certificates(&self) -> anyhow::Result<Option<TlsCertificates>> {
        let (Some(cert), Some(key)) = (&self.tls_cert, &self.tls_key) else {
            return Ok(None);
        };
        let certificates = TlsCertificates::load(cert, key)
            .await
            .context("[serve] tls_cert and tls_key")?;
        drop(certificates.watch(DEFAULT_RELOAD_INTERVAL));
        Ok(Some(certificates))
    }
}

/// `[write]`: instance switches over what accounts allow. `false` turns a capability off for
/// every account, whatever the account allows; all are on by default, and accounts allow nothing
/// until a manager turns their capabilities on.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteConfig {
    /// Any write to a mail, contacts or calendars server.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Sending mail.
    #[serde(default = "yes")]
    pub send: bool,
    /// Running rule flows.
    #[serde(default = "yes")]
    pub rules: bool,
    /// Seconds a queued message waits before it leaves, so it can be undone.
    #[serde(default = "default_undo_seconds")]
    pub undo_seconds: u64,
}

impl Default for WriteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            send: true,
            rules: true,
            undo_seconds: default_undo_seconds(),
        }
    }
}

fn default_undo_seconds() -> u64 {
    10
}

impl From<WriteConfig> for holtmail_api::WriteSwitches {
    fn from(config: WriteConfig) -> Self {
        Self {
            enabled: config.enabled,
            send: config.send,
            rules: config.rules,
        }
    }
}

fn yes() -> bool {
    true
}

fn default_parallel_syncs() -> usize {
    4
}

static TLS_ROOTS: LazyLock<RwLock<TlsRoots>> = LazyLock::new(Default::default);

/// The extra TLS roots of the config this process loaded last; none before any was loaded.
pub fn tls_roots() -> TlsRoots {
    TLS_ROOTS.read().expect("TLS roots lock").clone()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    #[serde(default)]
    pub google: OAuthClientConfig,
    #[serde(default)]
    pub microsoft: OAuthClientConfig,
}

/// One provider's client. Without `client_id` Thunderbird's client is used, unless
/// `public_client` is `false`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthClientConfig {
    /// `false` turns Thunderbird's client off: without `client_id` the provider is then not
    /// offered. On by default.
    #[serde(default)]
    pub public_client: Option<bool>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Where the provider sends the browser back: `<public_url>/api/oauth/callback` for an own
    /// client, `http://127.0.0.1` for Thunderbird's.
    #[serde(default)]
    pub redirect_url: Option<String>,
    /// The provider's sign-in page, for a national cloud or a single-tenant authority such as
    /// `https://login.microsoftonline.com/<tenant>/oauth2/v2.0/authorize`.
    #[serde(default)]
    pub authorize_url: Option<String>,
    /// The provider's token endpoint, to go with `authorize_url`.
    #[serde(default)]
    pub token_url: Option<String>,
    /// Microsoft only: the Graph address new Graph accounts use, for a national cloud such as
    /// `https://graph.microsoft.us/v1.0`.
    #[serde(default)]
    pub graph_url: Option<String>,
    /// The IMAP server new OAuth accounts of this provider connect to over TLS, for a national
    /// cloud such as `outlook.office365.us`; the provider's own server by default.
    #[serde(default)]
    pub imap_host: Option<String>,
    /// Its port, 993 by default.
    #[serde(default)]
    pub imap_port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BlobConfig {
    #[default]
    Fs,
    FsAt {
        path: PathBuf,
    },
    S3 {
        bucket: String,
        #[serde(default = "default_region")]
        region: String,
        endpoint: Option<String>,
        prefix: Option<String>,
        #[serde(default)]
        path_style: bool,
        #[serde(default)]
        allow_http: bool,
    },
}

fn default_listen() -> SocketAddr {
    "127.0.0.1:8080".parse().expect("valid address")
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_region() -> String {
    "us-east-1".into()
}

impl Config {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut config: Config = match path {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
            }
            None => toml::from_str("")?,
        };
        if let Ok(listen) = std::env::var("HOLTMAIL_LISTEN") {
            config.listen = listen.parse().context("HOLTMAIL_LISTEN")?;
        }
        if let Ok(dir) = std::env::var("HOLTMAIL_DATA_DIR") {
            config.data_dir = dir.into();
        }
        if let Ok(dir) = std::env::var("HOLTMAIL_WEB_DIR") {
            config.web_dir = Some(dir.into());
        }
        if let Ok(url) = std::env::var("HOLTMAIL_PUBLIC_URL") {
            config.public_url = Some(url);
        }
        if let Ok(minutes) = std::env::var("HOLTMAIL_SYNC_INTERVAL") {
            config.sync_interval_minutes = Some(minutes.parse().context("HOLTMAIL_SYNC_INTERVAL")?);
        }
        if let Some(smtp) = &mut config.smtp
            && let Ok(password) = std::env::var("HOLTMAIL_SMTP_PASSWORD")
        {
            smtp.password = Some(password).filter(|p| !p.is_empty());
        }
        if let Ok(on) = std::env::var("HOLTMAIL_WATCH") {
            config.watch.enabled = on
                .trim()
                .parse()
                .context("HOLTMAIL_WATCH is true or false")?;
        }
        if let Ok(on) = std::env::var("HOLTMAIL_INSIGHTS") {
            config.insights.enabled = on
                .trim()
                .parse()
                .context("HOLTMAIL_INSIGHTS is true or false")?;
        }
        config.insights.check()?;
        if let Some(smtp) = &config.smtp {
            crate::insights::check_authserv_ids(
                "smtp.trusted_authserv_ids",
                &smtp.trusted_authserv_ids,
            )?;
        }
        anyhow::ensure!(
            (1..=64).contains(&config.watch.parallel_syncs),
            "watch.parallel_syncs is between 1 and 64"
        );
        if let Ok(proxy) = std::env::var("HOLTMAIL_IMAGE_PROXY") {
            config.image_proxy = Some(proxy).filter(|p| !p.trim().is_empty());
        }
        for (name, client) in [
            ("GOOGLE", &mut config.oauth.google),
            ("MICROSOFT", &mut config.oauth.microsoft),
        ] {
            let read = |key: &str| {
                std::env::var(format!("HOLTMAIL_OAUTH_{name}_{key}"))
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            };
            if let Some(id) = read("CLIENT_ID") {
                client.client_id = Some(id);
            }
            if let Some(on) = read("PUBLIC_CLIENT") {
                client.public_client = Some(
                    on.parse()
                        .with_context(|| format!("HOLTMAIL_OAUTH_{name}_PUBLIC_CLIENT"))?,
                );
            }
            if let Some(secret) = read("CLIENT_SECRET") {
                client.client_secret = Some(secret);
            }
            if let Some(url) = read("REDIRECT_URL") {
                client.redirect_url = Some(url);
            }
            if let Some(url) = read("AUTHORIZE_URL") {
                client.authorize_url = Some(url);
            }
            if let Some(url) = read("TOKEN_URL") {
                client.token_url = Some(url);
            }
            if let Some(url) = read("GRAPH_URL") {
                client.graph_url = Some(url);
            }
            if let Some(host) = read("IMAP_HOST") {
                client.imap_host = Some(host);
            }
            if let Some(port) = read("IMAP_PORT") {
                client.imap_port = Some(
                    port.trim()
                        .parse()
                        .with_context(|| format!("HOLTMAIL_OAUTH_{name}_IMAP_PORT"))?,
                );
            }
        }
        if let Some(path) = std::env::var_os("HOLTMAIL_TLS_CA_FILE").filter(|p| !p.is_empty()) {
            config.tls_ca_file = Some(path.into());
        }
        if let Some(path) = &config.tls_ca_file {
            config.tls_roots = read_roots(path)?;
        }
        *TLS_ROOTS.write().expect("TLS roots lock") = config.tls_roots.clone();
        config.serve.validate()?;
        anyhow::ensure!(
            config.public_url.is_some() || !config.serve.jmap.as_ref().is_some_and(|j| j.enabled),
            "[serve.jmap] needs public_url"
        );
        anyhow::ensure!(
            config.public_url.is_some() || !config.serve.dav.as_ref().is_some_and(|d| d.enabled),
            "[serve.dav] needs public_url"
        );
        if let Some(url) = &config.public_url {
            anyhow::ensure!(
                url.starts_with("http://") || url.starts_with("https://"),
                "public_url must start with http:// or https://"
            );
        }
        Ok(config)
    }

    /// The base URL people open, without a trailing slash.
    pub fn base_url(&self) -> String {
        match &self.public_url {
            Some(url) => url.trim_end_matches('/').to_owned(),
            None if self.listen.ip().is_unspecified() => {
                format!("http://localhost:{}", self.listen.port())
            }
            None => format!("http://{}", self.listen),
        }
    }

    /// The OAuth client for `provider`, from `[oauth.*]` or Thunderbird's; `None` when the
    /// provider has no own client and Thunderbird's is turned off.
    pub fn oauth_client(&self, provider: Provider) -> anyhow::Result<Option<Client>> {
        let own = match provider {
            Provider::Google => &self.oauth.google,
            Provider::Microsoft => &self.oauth.microsoft,
        };
        let mut client = match &own.client_id {
            Some(id) => Client::own(
                provider,
                id.trim(),
                own.client_secret.clone().filter(|s| !s.is_empty()),
                format!("{}{}", self.base_url(), crate::oauth::CALLBACK_PATH),
            ),
            None => {
                anyhow::ensure!(
                    own.client_secret.is_none(),
                    "oauth.{}.client_secret is set without client_id",
                    provider.as_str()
                );
                if own.public_client == Some(false) {
                    return Ok(None);
                }
                Client::thunderbird(provider)
            }
        };
        if let Some(url) = &own.redirect_url {
            let parsed = url::Url::parse(url)
                .with_context(|| format!("oauth.{}.redirect_url", provider.as_str()))?;
            anyhow::ensure!(
                matches!(parsed.scheme(), "http" | "https"),
                "oauth.{}.redirect_url must start with http:// or https://",
                provider.as_str()
            );
            client.redirect_url = url.clone();
        }
        if let Some(url) = &own.authorize_url {
            client.authorize_url = secure_url(url, provider, "authorize_url")?;
        }
        if let Some(url) = &own.token_url {
            client.token_url = secure_url(url, provider, "token_url")?;
        }
        Ok(Some(client))
    }

    /// The IMAP server and port new OAuth accounts of `provider` connect to over TLS.
    pub fn oauth_imap(&self, provider: Provider) -> anyhow::Result<(String, u16)> {
        let own = match provider {
            Provider::Google => &self.oauth.google,
            Provider::Microsoft => &self.oauth.microsoft,
        };
        let (default_host, default_port) = provider.imap();
        let host = match own.imap_host.as_deref().map(str::trim) {
            Some(host) => {
                anyhow::ensure!(
                    !host.is_empty()
                        && !host.contains(['/', ':', '@', ' '])
                        && url::Host::parse(host).is_ok(),
                    "oauth.{}.imap_host is a host name, like outlook.office365.us",
                    provider.as_str()
                );
                host.to_owned()
            }
            None => default_host.to_owned(),
        };
        let port = own.imap_port.unwrap_or(default_port);
        anyhow::ensure!(port != 0, "oauth.{}.imap_port is 0", provider.as_str());
        Ok((host, port))
    }

    /// The Graph address new Microsoft Graph accounts are given, when the config names one.
    pub fn graph_url(&self) -> anyhow::Result<Option<String>> {
        self.oauth
            .microsoft
            .graph_url
            .as_deref()
            .map(|url| secure_url(url, Provider::Microsoft, "graph_url"))
            .transpose()
    }

    /// Whether the instance is served over https.
    pub fn https(&self) -> bool {
        self.public_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://"))
    }
}

pub struct Services {
    pub secrets: Arc<dyn SecretBox>,
    pub blobs: Arc<dyn BlobStore>,
    pub store: Arc<dyn Store>,
    pub index: Arc<dyn SearchIndex>,
    pub extractor: Arc<dyn Extractor>,
    pub fetcher: Arc<ProxiedFetcher>,
    pub oauth: Arc<OAuth>,
    pub hooks: Arc<crate::webhooks::Hooks>,
    pub smtp: std::sync::OnceLock<Arc<crate::smtp::Listener>>,
    /// Sends held contact and calendar edits upstream, once `serve` started it.
    pub pim_writeback: std::sync::OnceLock<Arc<holtmail_pim_writeback::Writeback>>,
    /// The writers `pim_writeback` sends through, which also tell how a collection's server
    /// schedules; set with it.
    pub pim_writers: std::sync::OnceLock<Arc<crate::pim_writeback::PimWriters>>,
    pub watch: WatchConfig,
    /// Where contacts and calendars servers post WebDAV-Push messages: the https `public_url`
    /// without a trailing slash; `None` when the instance has no public https address.
    pub push_base: Option<String>,
    /// Recognise mail forwarded by alias services, in this order.
    pub detectors: Vec<Arc<dyn holtmail_alias::AliasDetector>>,
    /// Connected alias services.
    pub aliases: holtmail_aliases::AliasManager,
    /// Minutes between listings of the services' aliases; 0 for only on request.
    pub alias_sync_minutes: u64,
    /// The insights runner and what the API serves of it; `None` when insights are off.
    pub insights: Option<crate::insights::Insights>,
    /// The category data release loader and updater; `None` when insights are off or `domains`
    /// is not among the sources.
    pub category_data: Option<Arc<crate::category_data::Updater>>,
    /// Matches what sync adds against the outbox once the server's outbox is open.
    pub sent_mail: Arc<crate::send::SentMail>,
    /// Records attendees' iMIP replies on the user's own events once `serve` started it.
    pub replies: Arc<crate::replies::Replies>,
    /// Hands live mail to the rules executor once `serve` started it.
    pub rules: Arc<crate::rules::RulesObserver>,
}

/// Opens only the metadata store, so user commands work while `serve` holds the index.
pub async fn open_store(config: &Config) -> anyhow::Result<Arc<dyn Store>> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating {}", config.data_dir.display()))?;
    Ok(Arc::new(
        SqliteStore::open(config.data_dir.join("holtmail.db")).await?,
    ))
}

/// Opens the blob store the config names, without the rest of the services.
pub async fn open_blobs(config: &Config) -> anyhow::Result<Arc<dyn BlobStore>> {
    let blobs: Arc<dyn BlobStore> = match &config.blobs {
        BlobConfig::Fs => Arc::new(FsBlobStore::open(config.data_dir.join("blobs")).await?),
        BlobConfig::FsAt { path } => Arc::new(FsBlobStore::open(path).await?),
        BlobConfig::S3 {
            bucket,
            region,
            endpoint,
            prefix,
            path_style,
            allow_http,
        } => {
            let (Ok(access_key_id), Ok(secret_access_key)) = (
                std::env::var("HOLTMAIL_S3_ACCESS_KEY_ID"),
                std::env::var("HOLTMAIL_S3_SECRET_ACCESS_KEY"),
            ) else {
                bail!("S3 blobs need HOLTMAIL_S3_ACCESS_KEY_ID and HOLTMAIL_S3_SECRET_ACCESS_KEY");
            };
            Arc::new(S3BlobStore::new(S3Config {
                bucket: bucket.clone(),
                region: region.clone(),
                endpoint: endpoint.clone(),
                access_key_id,
                secret_access_key,
                prefix: prefix.clone(),
                path_style: *path_style,
                allow_http: *allow_http,
                roots: config.tls_roots.clone(),
            })?)
        }
    };
    Ok(blobs)
}

/// Opens the secret box of the data directory, creating its key on first use.
pub fn open_secrets(config: &Config) -> anyhow::Result<Arc<LocalSecretBox>> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating {}", config.data_dir.display()))?;
    Ok(Arc::new(LocalSecretBox::open_or_create(
        config.data_dir.join("secret.key"),
    )?))
}

fn read_roots(path: &Path) -> anyhow::Result<TlsRoots> {
    let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    TlsRoots::from_pem(&pem).with_context(|| format!("tls_ca_file {}", path.display()))
}

/// An address the config gives for a provider: https, or plain http to a loopback address.
fn secure_url(raw: &str, provider: Provider, key: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(raw.trim())
        .with_context(|| format!("oauth.{}.{key}", provider.as_str()))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "oauth.{}.{key} must use https",
        provider.as_str()
    );
    Ok(raw.trim().to_owned())
}

/// The OAuth clients the config names, with the provider's token endpoint over HTTPS.
pub fn open_oauth(config: &Config) -> anyhow::Result<Arc<OAuth>> {
    let mut clients = Vec::new();
    for provider in Provider::ALL {
        clients.extend(config.oauth_client(provider)?);
    }
    let mut oauth = OAuth::new(
        clients,
        Arc::new(
            HttpTokenEndpoint::new(std::time::Duration::from_secs(30))?
                .with_roots(&config.tls_roots)?,
        ),
    )
    .with_graph_url(config.graph_url()?);
    for provider in Provider::ALL {
        let (host, port) = config.oauth_imap(provider)?;
        oauth = oauth.with_imap(provider, host, port);
    }
    Ok(Arc::new(oauth))
}

impl Services {
    pub async fn open(config: &Config) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating {}", config.data_dir.display()))?;
        let blobs = open_blobs(config).await?;
        let secrets = open_secrets(config)?;
        let store = Arc::new(SqliteStore::open(config.data_dir.join("holtmail.db")).await?);
        crate::accounts::refuse_own_instance(crate::submit::instance_token(&*store).await?);
        let index = Arc::new(TantivyIndex::open(config.data_dir.join("index"))?);
        if index.was_emptied() {
            tracing::warn!("the search index was from an older version and is rebuilt");
            store
                .set_setting(crate::REINDEX_SETTING, Some("needed"))
                .await?;
        }
        let fetcher = Arc::new(
            ProxiedFetcher::open(
                config.image_proxy.as_deref(),
                store.clone(),
                secrets.clone(),
            )
            .await?,
        );
        let oauth = open_oauth(config)?;
        let hooks = Arc::new(crate::webhooks::Hooks::new(
            store.clone(),
            secrets.clone(),
            Arc::new(
                holtmail_webhook_http::HttpWebhookSender::new(std::time::Duration::from_secs(10))?
                    .with_roots(&config.tls_roots)?,
            ),
        ));
        oauth.set_hooks(hooks.clone());
        let detectors = crate::aliases::detectors(&config.aliases)?;
        let smtp_authserv_ids = config
            .smtp
            .as_ref()
            .map_or(&[][..], |smtp| &smtp.trusted_authserv_ids[..]);
        let insights = crate::insights::Insights::open(
            &config.insights,
            smtp_authserv_ids,
            &config.tls_roots,
        )?;
        let category_data = match &insights {
            Some(insights) => {
                crate::category_data::Updater::open(
                    &config.insights,
                    insights,
                    &config.data_dir,
                    &config.tls_roots,
                    store.clone(),
                )
                .await?
            }
            None => None,
        };
        let aliases = holtmail_aliases::AliasManager::new(
            store.clone(),
            secrets.clone(),
            crate::aliases::providers(),
            crate::aliases::connect(config.tls_roots.clone()),
        )
        .with_detectors(detectors.clone());
        Ok(Self {
            secrets,
            blobs,
            store,
            index,
            extractor: Arc::new(BasicExtractor::new()),
            fetcher,
            oauth,
            hooks,
            smtp: Default::default(),
            pim_writeback: Default::default(),
            pim_writers: Default::default(),
            watch: config.watch.clone(),
            push_base: config.https().then(|| config.base_url()),
            detectors,
            aliases,
            alias_sync_minutes: config.aliases.sync_interval_minutes,
            insights,
            category_data,
            sent_mail: Arc::default(),
            replies: Arc::default(),
            rules: Arc::default(),
        })
    }

    /// The ingest pipeline over these services, with attachment text, aliases and, when on,
    /// insights and their categories.
    pub fn ingest(&self) -> Ingest {
        let ingest = Ingest::new(self.blobs.clone(), self.store.clone(), self.index.clone())
            .with_observer(self.sent_mail.clone())
            .with_observer(self.replies.clone())
            .with_observer(self.rules.clone())
            .with_extractor(self.extractor.clone())
            .with_alias_detectors(self.detectors.clone());
        match &self.insights {
            Some(insights) => ingest
                .with_insights(insights.runner.clone())
                .with_categories(insights.deriver.clone(), insights.policy()),
            None => ingest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_a_file() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.listen.to_string(), "127.0.0.1:8080");
        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert!(matches!(config.blobs, BlobConfig::Fs));
    }

    #[test]
    fn reads_the_serve_section() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.serve.members && !config.serve.proxy_protocol);
        assert!(config.serve.local_flags);
        assert_eq!(config.serve.validate().unwrap(), TrustedProxies::default());

        let config: Config = toml::from_str(
            "[serve]\ntls_cert = \"c.pem\"\ntls_key = \"k.pem\"\nproxy_protocol = true\ntrusted_proxies = [\"10.0.0.0/8\", \"192.0.2.1\"]\nmembers = false\nlocal_flags = false",
        )
        .unwrap();
        assert!(!config.serve.members && !config.serve.local_flags);
        let trusted = config.serve.validate().unwrap();
        assert!(trusted.contains("10.1.2.3".parse().unwrap()));
        assert!(trusted.contains("192.0.2.1".parse().unwrap()));
        assert!(!trusted.contains("192.0.2.2".parse().unwrap()));
        let api = config.serve.api_options().unwrap();
        assert!(!api.members);
        assert_eq!(api.recent_sign_in.as_secs(), 300);

        for bad in [
            "[serve]\ntls_cert = \"c.pem\"",
            "[serve]\ntls_key = \"k.pem\"",
            "[serve]\nproxy_protocol = true",
            "[serve]\ntrusted_proxies = [\"proxy.example.com\"]",
        ] {
            let config: Config = toml::from_str(bad).unwrap();
            assert!(config.serve.validate().is_err(), "{bad}");
        }
        assert!(toml::from_str::<Config>("[serve]\nlisten = 1").is_err());
        let config: Config = toml::from_str(
            "[serve]\ntls_cert = \"c.pem\"\ntls_key = \"k.pem\"\n[serve.submit]\nlisten_tls = [\"0.0.0.0:465\"]\nlisten_starttls = [\"0.0.0.0:587\"]",
        )
        .unwrap();
        config.serve.validate().unwrap();
        let submit = config.serve.submit.unwrap();
        assert_eq!(
            (
                submit.undo_seconds,
                submit.messages_per_hour,
                submit.recipients_per_day
            ),
            (0, 200, 2000)
        );
        for bad in [
            "[serve.submit]\nundo_seconds = 1",
            "[serve.submit]\nlisten_tls = [\"0.0.0.0:465\"]",
            "[serve.submit]\nlisten_starttls = [\"0.0.0.0:587\"]",
        ] {
            let config: Config = toml::from_str(bad).unwrap();
            assert!(config.serve.validate().is_err(), "{bad}");
        }
        let local: Config =
            toml::from_str("[serve.submit]\nlisten_starttls = [\"127.0.0.1:2587\"]").unwrap();
        local.serve.validate().unwrap();
        for bad in [
            "[serve.imap]\nhostname = \"imap.example.org\"",
            "[serve.imap]\nlisten_tls = [\"0.0.0.0:993\"]",
            "[serve.imap]\nlisten_starttls = [\"0.0.0.0:143\"]",
            "[serve.imap]\nlisten = [\"0.0.0.0:143\"]",
        ] {
            let config = toml::from_str::<Config>(bad);
            assert!(
                config.is_err() || config.unwrap().serve.validate().is_err(),
                "{bad}"
            );
        }
        let imap: Config =
            toml::from_str("[serve.imap]\nlisten_starttls = [\"127.0.0.1:2143\"]").unwrap();
        imap.serve.validate().unwrap();
    }

    #[tokio::test]
    async fn refuses_certificates_that_cannot_be_read() {
        let config = ServeConfig {
            tls_cert: Some("/nonexistent/c.pem".into()),
            tls_key: Some("/nonexistent/k.pem".into()),
            ..ServeConfig::default()
        };
        assert!(config.certificates().await.is_err());
        assert!(
            ServeConfig::default()
                .certificates()
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reads_the_write_switches() {
        let config: Config = toml::from_str("").unwrap();
        let on: holtmail_api::WriteSwitches = config.write.into();
        assert_eq!(on, holtmail_api::WriteSwitches::default());
        let config: Config = toml::from_str("[write]\nsend = false").unwrap();
        assert!(config.write.enabled && !config.write.send && config.write.rules);
        assert_eq!(config.write.undo_seconds, 10);
        let config: Config = toml::from_str("[write]\nundo_seconds = 0").unwrap();
        assert_eq!(config.write.undo_seconds, 0);
        assert!(toml::from_str::<Config>("[write]\nsending = false").is_err());
    }

    #[test]
    fn reads_s3_settings() {
        let config: Config = toml::from_str(
            r#"
            listen = "0.0.0.0:9000"
            [blobs]
            kind = "s3"
            bucket = "holtmail"
            endpoint = "https://s3.example.com"
            path_style = true
            "#,
        )
        .unwrap();
        assert!(
            matches!(config.blobs, BlobConfig::S3 { ref bucket, path_style: true, .. } if bucket == "holtmail")
        );
    }

    #[test]
    fn builds_the_base_url() {
        let config: Config = toml::from_str("listen = \"0.0.0.0:9000\"").unwrap();
        assert_eq!(config.base_url(), "http://localhost:9000");
        assert!(!config.https());
        let config: Config = toml::from_str("public_url = \"https://mail.example.com/\"").unwrap();
        assert_eq!(config.base_url(), "https://mail.example.com");
        assert!(config.https());
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn picks_oauth_clients() {
        let config: Config = toml::from_str("public_url = \"https://mail.example.com/\"").unwrap();
        let google = config.oauth_client(Provider::Google).unwrap().unwrap();
        assert!(google.is_thunderbird());
        assert_eq!(google.redirect_url, "http://127.0.0.1");
        let config: Config = toml::from_str(
            r#"
            public_url = "https://mail.example.com"
            [oauth.microsoft]
            client_id = "own-id"
            client_secret = "own-secret"
            [oauth.google]
            redirect_url = "http://127.0.0.1:8080/api/oauth/callback"
            "#,
        )
        .unwrap();
        let microsoft = config.oauth_client(Provider::Microsoft).unwrap().unwrap();
        assert!(!microsoft.is_thunderbird());
        assert_eq!(microsoft.client_secret.as_deref(), Some("own-secret"));
        assert_eq!(
            microsoft.redirect_url,
            "https://mail.example.com/api/oauth/callback"
        );
        let google = config.oauth_client(Provider::Google).unwrap().unwrap();
        assert!(google.is_thunderbird());
        assert_eq!(
            google.redirect_url,
            "http://127.0.0.1:8080/api/oauth/callback"
        );
        assert_eq!(
            config.oauth_imap(Provider::Google).unwrap(),
            ("imap.gmail.com".to_owned(), 993)
        );
        let config: Config = toml::from_str(
            "[oauth.microsoft]\nimap_host = \"outlook.office365.us\"\n[oauth.google]\nimap_host = \"localhost\"\nimap_port = 41993",
        )
        .unwrap();
        assert_eq!(
            config.oauth_imap(Provider::Microsoft).unwrap(),
            ("outlook.office365.us".to_owned(), 993)
        );
        assert_eq!(
            config.oauth_imap(Provider::Google).unwrap(),
            ("localhost".to_owned(), 41993)
        );
        for bad in [
            "imaps://imap.example.com",
            "imap.example.com:993",
            " ",
            "a b",
        ] {
            let config: Config =
                toml::from_str(&format!("[oauth.google]\nimap_host = {bad:?}")).unwrap();
            assert!(config.oauth_imap(Provider::Google).is_err(), "{bad}");
        }
        let config: Config = toml::from_str("[oauth.google]\nclient_secret = \"x\"").unwrap();
        assert!(config.oauth_client(Provider::Google).is_err());
        assert!(toml::from_str::<Config>("[oauth.yahoo]\nclient_id = \"x\"").is_err());
    }

    #[test]
    fn turns_thunderbirds_client_off() {
        let config: Config = toml::from_str(
            "[oauth.google]\npublic_client = false\n[oauth.microsoft]\npublic_client = false\nclient_id = \"own-id\"",
        )
        .unwrap();
        assert!(config.oauth_client(Provider::Google).unwrap().is_none());
        let microsoft = config.oauth_client(Provider::Microsoft).unwrap().unwrap();
        assert!(!microsoft.is_thunderbird());
    }

    #[test]
    fn reads_extra_tls_roots() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(
            &ca,
            "-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let config: Config =
            toml::from_str(&format!("tls_ca_file = {:?}", ca.display().to_string())).unwrap();
        assert_eq!(config.tls_ca_file.as_deref(), Some(ca.as_path()));
        assert_eq!(read_roots(&ca).unwrap().len(), 1);
        std::fs::write(&ca, "no certificate here").unwrap();
        assert!(read_roots(&ca).is_err());
        assert!(read_roots(&dir.path().join("missing.pem")).is_err());
        assert!(toml::from_str::<Config>("").unwrap().tls_roots.is_empty());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("lissen = \"x\"").is_err());
    }
}
