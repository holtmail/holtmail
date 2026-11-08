use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::crypto::{CryptoProvider, ring};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;
use tokio_rustls::rustls::{ServerConfig, version};

use crate::NetError;

/// How often [`TlsCertificates::watch`] should look at the files, when nothing says otherwise.
pub const DEFAULT_RELOAD_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Current(RwLock<Arc<CertifiedKey>>);

impl ResolvesServerCert for Current {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.read().unwrap_or_else(|e| e.into_inner()).clone())
    }
}

type Fingerprint = (blake3::Hash, blake3::Hash);

#[derive(Debug)]
struct Shared {
    cert: PathBuf,
    key: PathBuf,
    provider: Arc<CryptoProvider>,
    current: Arc<Current>,
    seen: Mutex<Fingerprint>,
}

/// A certificate and key from two PEM files, served with TLS 1.2 and 1.3 only. Cloning shares
/// them, so every listener given a clone presents the same certificate.
///
/// Connections that have finished their handshake keep the certificate they got; a new
/// handshake uses the newest one that loaded.
#[derive(Debug, Clone)]
pub struct TlsCertificates {
    shared: Arc<Shared>,
    config: Arc<ServerConfig>,
}

impl TlsCertificates {
    /// Reads the two files. [`NetError::Certificate`] when one cannot be read, holds no
    /// certificate or key, or the key does not belong to the certificate.
    pub async fn load(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Result<Self, NetError> {
        let (cert, key) = (cert.into(), key.into());
        let provider = Arc::new(ring::default_provider());
        let (cert_pem, key_pem) = read_files(&cert, &key).await?;
        let certified = certified_key(&cert_pem, &key_pem, &provider)?;
        let current = Arc::new(Current(RwLock::new(Arc::new(certified))));
        let config = ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&version::TLS13, &version::TLS12])
            .map_err(|e| NetError::Certificate(e.to_string()))?
            .with_no_client_auth()
            .with_cert_resolver(current.clone());
        Ok(Self {
            shared: Arc::new(Shared {
                cert,
                key,
                provider,
                current,
                seen: Mutex::new(fingerprint(&cert_pem, &key_pem)),
            }),
            config: Arc::new(config),
        })
    }

    /// Reads the files again and starts using them if they changed. `Ok(true)` when the
    /// certificate was replaced, `Ok(false)` when the files are as they were. On an error the
    /// certificate in use stays, and the next call tries again.
    pub async fn reload(&self) -> Result<bool, NetError> {
        reload(&self.shared).await
    }

    /// Calls [`reload`](Self::reload) every `interval` until every clone of this value is
    /// gone. A file caught half written is retried at the next tick. Failures are logged once
    /// per distinct failure, without file contents.
    pub fn watch(&self, interval: Duration) -> JoinHandle<()> {
        let shared: Weak<Shared> = Arc::downgrade(&self.shared);
        tokio::spawn(async move {
            let mut last_failure = None;
            loop {
                tokio::time::sleep(interval).await;
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                match reload(&shared).await {
                    Ok(true) => {
                        last_failure = None;
                        tracing::info!("TLS certificate reloaded");
                    }
                    Ok(false) => last_failure = None,
                    Err(error) => {
                        let text = error.to_string();
                        if last_failure.as_ref() != Some(&text) {
                            tracing::warn!(error = %text, "TLS certificate reload failed");
                            last_failure = Some(text);
                        }
                    }
                }
            }
        })
    }

    /// A TLS acceptor with the certificate in force now, for a protocol that runs its own
    /// `STARTTLS`.
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(self.config.clone())
    }
}

async fn reload(shared: &Shared) -> Result<bool, NetError> {
    let mut seen = shared.seen.lock().await;
    let (cert, key) = read_files(&shared.cert, &shared.key).await?;
    let now = fingerprint(&cert, &key);
    if *seen == now {
        return Ok(false);
    }
    let certified = certified_key(&cert, &key, &shared.provider)?;
    *shared.current.0.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(certified);
    *seen = now;
    Ok(true)
}

fn fingerprint(cert: &[u8], key: &[u8]) -> Fingerprint {
    (blake3::hash(cert), blake3::hash(key))
}

async fn read_files(cert: &Path, key: &Path) -> Result<(Vec<u8>, Vec<u8>), NetError> {
    let read = |path: PathBuf| async move {
        tokio::fs::read(&path)
            .await
            .map_err(|e| NetError::Certificate(format!("reading {}: {e}", path.display())))
    };
    Ok((read(cert.to_owned()).await?, read(key.to_owned()).await?))
}

fn certified_key(
    cert: &[u8],
    key: &[u8],
    provider: &CryptoProvider,
) -> Result<CertifiedKey, NetError> {
    let chain = CertificateDer::pem_slice_iter(cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| NetError::Certificate(format!("not a PEM certificate file: {e}")))?;
    if chain.is_empty() {
        return Err(NetError::Certificate(
            "no certificate in the PEM file".into(),
        ));
    }
    let key = PrivateKeyDer::from_pem_slice(key)
        .map_err(|_| NetError::Certificate("no usable private key in the PEM file".into()))?;
    CertifiedKey::from_der(chain, key, provider)
        .map_err(|e| NetError::Certificate(format!("the key does not fit the certificate: {e}")))
}
