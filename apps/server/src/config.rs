use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_blob_s3::{S3BlobStore, S3Config};
use holtmail_index::SearchIndex;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_secret::SecretBox;
use holtmail_secret_local::LocalSecretBox;
use holtmail_store::Store;
use holtmail_store_sqlite::SqliteStore;
use serde::Deserialize;

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
    /// Minutes between scheduled syncs in `serve`; 0 turns scheduling off.
    #[serde(default = "default_sync_minutes")]
    pub sync_interval_minutes: u64,
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

fn default_sync_minutes() -> u64 {
    15
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
}

impl Services {
    pub async fn open(config: &Config) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating {}", config.data_dir.display()))?;
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
                    bail!(
                        "S3 blobs need HOLTMAIL_S3_ACCESS_KEY_ID and HOLTMAIL_S3_SECRET_ACCESS_KEY"
                    );
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
                })?)
            }
        };
        let secrets = Arc::new(LocalSecretBox::open_or_create(
            config.data_dir.join("secret.key"),
        )?);
        let store = Arc::new(SqliteStore::open(config.data_dir.join("holtmail.db")).await?);
        let index = Arc::new(TantivyIndex::open(config.data_dir.join("index"))?);
        Ok(Self {
            secrets,
            blobs,
            store,
            index,
        })
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
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("lissen = \"x\"").is_err());
    }
}
