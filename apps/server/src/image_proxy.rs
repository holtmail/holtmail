use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use holtmail_api::{ImageProxy, MailboxError};
use holtmail_fetch::{FetchError, Fetcher, RemoteImage};
use holtmail_fetch_http::{HttpFetcher, Options, proxy_address};
use holtmail_secret::SecretBox;
use holtmail_store::Store;

const SETTING: &str = "image_proxy";

/// A [`Fetcher`] whose outbound proxy can change while the server runs.
pub struct ProxiedFetcher {
    inner: RwLock<(Arc<HttpFetcher>, ImageProxy)>,
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
}

fn fetcher(proxy: Option<&str>) -> Result<HttpFetcher, FetchError> {
    HttpFetcher::new(Options {
        proxy: proxy.map(str::to_owned),
        ..Options::default()
    })
}

fn status(proxy: Option<&str>, from_config: bool) -> ImageProxy {
    ImageProxy {
        address: proxy
            .and_then(|p| proxy_address(p).ok())
            .map(|address| address.to_string()),
        from_config,
    }
}

impl ProxiedFetcher {
    /// Uses the proxy from the config when given, otherwise the one stored in the settings.
    pub async fn open(
        config_proxy: Option<&str>,
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretBox>,
    ) -> anyhow::Result<Self> {
        let (proxy, from_config) = match config_proxy {
            Some(proxy) => (Some(proxy.to_owned()), true),
            None => (stored(&*store, &*secrets).await?, false),
        };
        let http = fetcher(proxy.as_deref()).map_err(|e| anyhow::anyhow!("image proxy: {e}"))?;
        Ok(Self {
            inner: RwLock::new((Arc::new(http), status(proxy.as_deref(), from_config))),
            store,
            secrets,
        })
    }

    pub fn status(&self) -> ImageProxy {
        self.inner.read().expect("proxy lock").1.clone()
    }

    pub async fn set(&self, proxy: Option<String>) -> Result<ImageProxy, MailboxError> {
        if self.status().from_config {
            return Err(MailboxError::Invalid(
                "The image proxy is set in the server's config; change it there.".into(),
            ));
        }
        if let Some(proxy) = &proxy {
            let address = proxy_address(proxy).map_err(MailboxError::Invalid)?;
            let reachable = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect((address.host.as_str(), address.port)),
            )
            .await;
            if !matches!(reachable, Ok(Ok(_))) {
                return Err(MailboxError::Invalid(format!(
                    "Could not connect to the proxy at {address}."
                )));
            }
        }
        let http = fetcher(proxy.as_deref()).map_err(|e| MailboxError::Invalid(e.to_string()))?;
        let sealed = proxy
            .as_deref()
            .map(|proxy| self.secrets.seal(SETTING, proxy.as_bytes()))
            .transpose()
            .map_err(|e| MailboxError::Backend(e.to_string()))?
            .map(|sealed| STANDARD.encode(sealed));
        self.store
            .set_setting(SETTING, sealed.as_deref())
            .await
            .map_err(|e| MailboxError::Backend(e.to_string()))?;
        let status = status(proxy.as_deref(), false);
        *self.inner.write().expect("proxy lock") = (Arc::new(http), status.clone());
        Ok(status)
    }

    fn current(&self) -> Arc<HttpFetcher> {
        self.inner.read().expect("proxy lock").0.clone()
    }
}

async fn stored(store: &dyn Store, secrets: &dyn SecretBox) -> anyhow::Result<Option<String>> {
    let Some(sealed) = store.setting(SETTING).await? else {
        return Ok(None);
    };
    let sealed = STANDARD.decode(sealed)?;
    let proxy = secrets
        .open(SETTING, &sealed)
        .map_err(|e| anyhow::anyhow!("the stored image proxy: {e}"))?;
    Ok(Some(String::from_utf8(proxy)?))
}

#[async_trait]
impl Fetcher for ProxiedFetcher {
    async fn image(&self, url: &str) -> Result<RemoteImage, FetchError> {
        self.current().image(url).await
    }
}

#[cfg(test)]
mod tests {
    use holtmail_secret_local::LocalSecretBox;
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    #[tokio::test]
    async fn stores_the_proxy_sealed_and_reopens_it() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let secrets: Arc<dyn SecretBox> =
            Arc::new(LocalSecretBox::open_or_create(dir.path().join("key")).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let fetcher = ProxiedFetcher::open(None, store.clone(), secrets.clone())
            .await
            .unwrap();
        assert_eq!(fetcher.status(), ImageProxy::default());
        let proxy = format!("socks5h://ada:hunter2@127.0.0.1:{port}");
        let status = fetcher.set(Some(proxy)).await.unwrap();
        assert_eq!(
            status.address.as_deref(),
            Some(format!("socks5h://127.0.0.1:{port}").as_str())
        );
        let stored = store.setting(SETTING).await.unwrap().unwrap();
        assert!(
            !stored.contains("hunter2"),
            "the proxy password was stored in the clear"
        );

        let reopened = ProxiedFetcher::open(None, store.clone(), secrets.clone())
            .await
            .unwrap();
        assert_eq!(reopened.status(), status);
        holtmail_fetch::testing::conformance(&reopened).await;

        drop(listener);
        assert!(matches!(
            fetcher.set(Some(format!("http://127.0.0.1:{port}"))).await,
            Err(MailboxError::Invalid(_))
        ));
        assert!(matches!(
            fetcher.set(Some("ftp://127.0.0.1".into())).await,
            Err(MailboxError::Invalid(_))
        ));
        fetcher.set(None).await.unwrap();
        assert_eq!(store.setting(SETTING).await.unwrap(), None);

        let configured = ProxiedFetcher::open(Some("socks5h://127.0.0.1:9050"), store, secrets)
            .await
            .unwrap();
        assert!(configured.status().from_config);
        assert!(configured.set(None).await.is_err());
    }
}
