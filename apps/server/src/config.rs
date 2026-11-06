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
    #[serde(skip)]
    pub tls_roots: TlsRoots,
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

/// One provider's client. Without `client_id` Thunderbird's client is used.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthClientConfig {
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

    /// The OAuth client for `provider`, from `[oauth.*]` or Thunderbird's.
    pub fn oauth_client(&self, provider: Provider) -> anyhow::Result<Client> {
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
        Ok(client)
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
        clients.push(config.oauth_client(provider)?);
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
        Ok(Self {
            secrets,
            blobs,
            store,
            index,
            extractor: Arc::new(BasicExtractor::new()),
            fetcher,
            oauth,
        })
    }

    /// The ingest pipeline over these services, with attachment text.
    pub fn ingest(&self) -> Ingest {
        Ingest::new(self.blobs.clone(), self.store.clone(), self.index.clone())
            .with_extractor(self.extractor.clone())
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
        let google = config.oauth_client(Provider::Google).unwrap();
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
        let microsoft = config.oauth_client(Provider::Microsoft).unwrap();
        assert!(!microsoft.is_thunderbird());
        assert_eq!(microsoft.client_secret.as_deref(), Some("own-secret"));
        assert_eq!(
            microsoft.redirect_url,
            "https://mail.example.com/api/oauth/callback"
        );
        let google = config.oauth_client(Provider::Google).unwrap();
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
