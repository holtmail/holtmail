use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, FolderId, Timestamp};
use holtmail_ingest::{Ingest, IngestError, Placement};
use holtmail_smtp::{Credentials, Envelope, Handler, Options, Recipient, Refusal};
use holtmail_store::{AccountKind, NewAccount, Store};
use serde::Deserialize;

pub const ACCOUNT_NAME: &str = "Received by SMTP";
const COMMIT_EVERY: Duration = Duration::from_secs(2);

/// `[smtp]` in the config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    /// Where to listen, e.g. `0.0.0.0:2525`.
    pub listen: SocketAddr,
    /// The name in the greeting.
    #[serde(default = "default_hostname")]
    pub hostname: String,
    /// Client networks allowed to connect, as addresses or CIDR ranges; loopback only by default.
    #[serde(default = "default_allow")]
    pub allow: Vec<String>,
    /// The largest message in MiB, 50 by default.
    #[serde(default)]
    pub max_size_mb: Option<u64>,
    /// A PEM certificate chain, which turns `STARTTLS` on.
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    /// Its PEM private key.
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    /// Refuse mail before `STARTTLS`.
    #[serde(default)]
    pub require_tls: bool,
    /// With `password`, clients must sign in with `AUTH` as this user.
    #[serde(default)]
    pub username: Option<String>,
    /// Better given as `HOLTMAIL_SMTP_PASSWORD`.
    #[serde(default)]
    pub password: Option<String>,
    /// Which recipients are taken and where their mail goes.
    #[serde(default, rename = "route")]
    pub routes: Vec<RouteConfig>,
}

/// One `[[smtp.route]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    /// An address, or `*@domain` for every address of a domain.
    pub recipient: String,
    /// The archive's name or id; a name that does not exist yet is created.
    pub archive: String,
    /// The folder of the archive's "Received by SMTP" account; the recipient by default.
    #[serde(default)]
    pub folder: Option<String>,
    /// `exchange` stores the original message of an Exchange journal report instead of the
    /// report; `none` stores what arrives.
    #[serde(default)]
    pub journal: Journal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Journal {
    #[default]
    None,
    Exchange,
}

fn default_hostname() -> String {
    "holtmail".into()
}

fn default_allow() -> Vec<String> {
    vec!["127.0.0.0/8".into(), "::1/128".into()]
}

/// A network a client may connect from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    base: IpAddr,
    prefix: u8,
}

impl Network {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (text, None),
        };
        let base: IpAddr = address
            .parse()
            .with_context(|| format!("smtp.allow: {text} is not an address or range"))?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .with_context(|| format!("smtp.allow: {text} has a bad prefix"))?,
            None => max,
        };
        Ok(Self { base, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.base, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Address(String),
    Domain(String),
}

impl Pattern {
    fn parse(text: &str) -> anyhow::Result<Self> {
        let text = text.trim().to_ascii_lowercase();
        let Some((local, domain)) = text.rsplit_once('@') else {
            bail!("smtp.route: {text} is not an address or *@domain");
        };
        if domain.is_empty() || local.is_empty() || domain.contains(['*', ' ']) {
            bail!("smtp.route: {text} is not an address or *@domain");
        }
        Ok(if local == "*" {
            Self::Domain(domain.to_owned())
        } else {
            Self::Address(text)
        })
    }

    fn matches(&self, address: &str) -> bool {
        let address = address.trim().to_ascii_lowercase();
        match self {
            Self::Address(wanted) => address == *wanted,
            Self::Domain(domain) => address
                .rsplit_once('@')
                .is_some_and(|(local, d)| !local.is_empty() && d == domain),
        }
    }
}

/// A route ready to take mail.
#[derive(Debug, Clone)]
pub struct Route {
    pattern: Pattern,
    pub recipient: String,
    pub archive: ArchiveId,
    pub folder: FolderId,
    pub folder_name: String,
    pub journal: Journal,
}

/// What the listener did since `serve` started.
#[derive(Debug, Default)]
pub struct Counters {
    pub received: AtomicU64,
    pub deleted: AtomicU64,
    pub refused_recipients: AtomicU64,
    pub failed: AtomicU64,
    pub last_received_at: Mutex<Option<Timestamp>>,
}

/// The running listener, for the status page.
pub struct Listener {
    pub address: SocketAddr,
    pub config: SmtpConfig,
    pub routes: Vec<Route>,
    pub counters: Arc<Counters>,
}

struct Receiver {
    routes: Vec<Route>,
    ingest: Ingest,
    counters: Arc<Counters>,
    dirty: Arc<AtomicBool>,
}

#[async_trait]
impl Handler for Receiver {
    async fn recipient(&self, address: &str) -> Recipient {
        if self.routes.iter().any(|r| r.pattern.matches(address)) {
            Recipient::Accept
        } else {
            self.counters
                .refused_recipients
                .fetch_add(1, Ordering::Relaxed);
            Recipient::Reject("No such mailbox here".into())
        }
    }

    async fn deliver(&self, envelope: &Envelope, data: Bytes) -> Result<(), Refusal> {
        let mut targets = BTreeSet::new();
        for recipient in &envelope.recipients {
            for route in self.routes.iter().filter(|r| r.pattern.matches(recipient)) {
                targets.insert((route.archive, route.folder, route.journal));
            }
        }
        let now = Timestamp::now();
        for (archive, folder, journal) in targets {
            let bytes = match journal {
                Journal::Exchange => holtmail_mime::journal_original(&data).unwrap_or(data.clone()),
                Journal::None => data.clone(),
            };
            let placement = Placement {
                folder: Some(folder),
                uid: None,
                flags: Vec::new(),
                labels: None,
                received_at: Some(now),
            };
            match self.ingest.ingest(archive, bytes, &placement).await {
                Ok(_) => {
                    self.counters.received.fetch_add(1, Ordering::Relaxed);
                }
                Err(IngestError::Deleted) => {
                    self.counters.deleted.fetch_add(1, Ordering::Relaxed);
                }
                Err(IngestError::Mime(_)) => {
                    self.counters.failed.fetch_add(1, Ordering::Relaxed);
                    return Err(Refusal::Permanent("This is not an email message".into()));
                }
                Err(error) => {
                    self.counters.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(%error, %archive, "SMTP mail not stored");
                    return Err(Refusal::Temporary("The message could not be stored".into()));
                }
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
        *self
            .counters
            .last_received_at
            .lock()
            .expect("counters lock") = Some(now);
        tracing::info!(peer = %envelope.peer, recipients = envelope.recipients.len(), "SMTP mail archived");
        Ok(())
    }
}

/// Resolves the routes: archives by name or id (created when a name is new), the archive's
/// "Received by SMTP" account and a folder per route.
pub async fn routes(store: &dyn Store, config: &SmtpConfig) -> anyhow::Result<Vec<Route>> {
    if config.routes.is_empty() {
        bail!("smtp needs at least one [[smtp.route]]");
    }
    let mut out = Vec::new();
    for route in &config.routes {
        let pattern = Pattern::parse(&route.recipient)?;
        let archive = match crate::export::find_archive(store, route.archive.trim()).await {
            Ok(id) => id,
            Err(_) => crate::import::find_or_create_archive(store, route.archive.trim()).await?,
        };
        let account = match store
            .list_accounts(archive)
            .await?
            .into_iter()
            .find(|a| a.kind == AccountKind::Import && a.name == ACCOUNT_NAME)
        {
            Some(account) => account.id,
            None => {
                store
                    .create_account(NewAccount {
                        archive_id: archive,
                        kind: AccountKind::Import,
                        name: ACCOUNT_NAME.into(),
                        address: None,
                        settings: serde_json::json!({ "smtp": true }),
                        sealed_secret: None,
                        enabled: false,
                    })
                    .await?
                    .id
            }
        };
        let folder_name = route
            .folder
            .clone()
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| route.recipient.trim().to_ascii_lowercase());
        let folder = store.upsert_folder(account, &folder_name, None).await?.id;
        out.push(Route {
            pattern,
            recipient: route.recipient.trim().to_owned(),
            archive,
            folder,
            folder_name,
            journal: route.journal,
        });
    }
    Ok(out)
}

fn tls(config: &SmtpConfig) -> anyhow::Result<Option<tokio_rustls::TlsAcceptor>> {
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let (cert, key) = match (&config.tls_cert, &config.tls_key) {
        (None, None) => return Ok(None),
        (Some(cert), Some(key)) => (cert, key),
        _ => bail!("smtp.tls_cert and smtp.tls_key go together"),
    };
    let chain = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("reading {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("reading {}", cert.display()))?;
    let key =
        PrivateKeyDer::from_pem_file(key).with_context(|| format!("reading {}", key.display()))?;
    let server = tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .context("smtp.tls_cert and smtp.tls_key do not fit together")?;
    Ok(Some(tokio_rustls::TlsAcceptor::from(Arc::new(server))))
}

/// Checks the config and builds the receiver's options.
pub fn options(config: &SmtpConfig) -> anyhow::Result<(Options, Vec<Network>)> {
    let networks = config
        .allow
        .iter()
        .map(|n| Network::parse(n))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut options = Options::new(config.hostname.trim());
    options.tls = tls(config)?;
    if config.require_tls && options.tls.is_none() {
        bail!("smtp.require_tls needs smtp.tls_cert and smtp.tls_key");
    }
    options.require_tls = config.require_tls;
    options.max_size = usize::try_from(config.max_size_mb.unwrap_or(50).clamp(1, 1024) << 20)?;
    options.credentials = match (&config.username, &config.password) {
        (None, None) => None,
        (Some(username), Some(password)) if !username.is_empty() && !password.is_empty() => {
            Some(Credentials {
                username: username.clone(),
                password: password.clone(),
            })
        }
        _ => bail!("smtp.username and smtp.password (or HOLTMAIL_SMTP_PASSWORD) go together"),
    };
    Ok((options, networks))
}

/// Starts the listener in the background and returns it.
pub async fn start(
    config: &SmtpConfig,
    store: &dyn Store,
    ingest: Ingest,
) -> anyhow::Result<Arc<Listener>> {
    let (options, networks) = options(config)?;
    let routes = routes(store, config).await?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("smtp.listen {}", config.listen))?;
    let address = listener.local_addr()?;
    let counters = Arc::new(Counters::default());
    let dirty = Arc::new(AtomicBool::new(false));
    let receiver = Arc::new(Receiver {
        routes: routes.clone(),
        ingest: ingest.clone(),
        counters: counters.clone(),
        dirty: dirty.clone(),
    });
    tokio::spawn(holtmail_smtp::serve(
        listener,
        Arc::new(options),
        receiver,
        Arc::new(move |ip| networks.iter().any(|n| n.contains(ip))),
    ));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(COMMIT_EVERY).await;
            if dirty.swap(false, Ordering::Relaxed)
                && let Err(error) = ingest.commit().await
            {
                tracing::warn!(%error, "SMTP mail not made searchable");
                dirty.store(true, Ordering::Relaxed);
            }
        }
    });
    tracing::info!(listen = %config.listen, routes = routes.len(), "SMTP listening");
    Ok(Arc::new(Listener {
        address,
        config: config.clone(),
        routes,
        counters,
    }))
}

#[cfg(test)]
mod tests;
