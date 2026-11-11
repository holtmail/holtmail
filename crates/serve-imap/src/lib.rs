//! IMAP for the user's own mail clients. A client signs in with a device's app password and
//! sees the one archive the device serves as a mail account: the mailbox tree, served UIDs and
//! `MODSEQ`, flags, bodies and search come from [`MailView`], and every flag change, move,
//! copy, delete and append goes through [`MailActions`], so it meets the same five locks as the
//! web app and reaches upstream servers only as held mail changes.
//!
//! This crate only speaks the protocol: it holds no store, writer or transport, and nothing it
//! does deletes a message from the archive. The design is in SERVE.md, "IMAP".

mod auth;
mod conn;
mod fetch;
mod names;
mod respond;
mod search;
mod selected;
mod session;
mod utf7;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, UserId};
use holtmail_device::DeviceAuth;
use holtmail_mailview::{MailActions, MailView};
use holtmail_serve_net::{
    ConnectionHandler, ConnectionLimiter, ListenerOptions, TlsCertificates, TlsMode, TrustedProxies,
};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

/// The `ID` field that names the instance, so another Holtmail can tell it is talking to this
/// one (the loop guard in `source-imap`).
pub const INSTANCE_ID_FIELD: &str = "holtmail-instance";

/// The largest literal the server reads before sign-in tells it the device's own limit: 50 MiB.
pub const MAX_LITERAL: u32 = 50 << 20;

/// The longest command line, literals aside: 64 KiB.
pub const MAX_LINE: u32 = 64 << 10;

/// How one server behaves.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Sent as `holtmail-instance` in the `ID` response. Opaque; never the instance id itself.
    pub instance_token: Option<String>,
    /// The wait for a client that has not signed in.
    pub sign_in_timeout: Duration,
    /// The wait between commands of a signed-in client.
    pub idle_timeout: Duration,
    /// How long one `IDLE` may last before the server ends the connection.
    pub idle_limit: Duration,
    /// How often an idling client gets `* OK Still here`, so NAT entries stay open.
    pub keepalive: Duration,
    /// How often an idling session looks for changes when no wake-up came.
    pub poll: Duration,
    /// The largest literal accepted at all, also for `APPEND`; a device's own append limit,
    /// when smaller, is enforced once the message arrived.
    pub max_literal: u32,
    /// Bytes of message data all sessions together hold in memory for responses at once; a
    /// larger message waits for the whole budget.
    pub response_bytes: usize,
    /// Messages per batch of a large `FETCH`, between which responses are written out.
    pub fetch_batch: usize,
}

impl Default for Settings {
    /// No instance token, 60 seconds before sign-in, 30 minutes between commands, `IDLE` up
    /// to 31 minutes with a keepalive every 4, a poll every minute, 50 MiB literals, 64 MiB of
    /// response data and batches of 250 messages.
    fn default() -> Self {
        Self {
            instance_token: None,
            sign_in_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(30 * 60),
            idle_limit: Duration::from_secs(31 * 60),
            keepalive: Duration::from_secs(4 * 60),
            poll: Duration::from_secs(60),
            max_literal: MAX_LITERAL,
            response_bytes: 64 << 20,
            fetch_batch: 250,
        }
    }
}

/// Wakes sessions when something in an archive changed.
pub trait ChangeSignal: Send + Sync {
    /// A listener for changes in `archive` that `user` may see.
    fn listen(&self, archive: ArchiveId, user: UserId) -> Box<dyn ChangeListener>;
}

/// One session's wake-ups.
#[async_trait]
pub trait ChangeListener: Send {
    /// Waits until something changed after the last call, and returns the newest change log
    /// `seq` known; `None` once no more wake-ups will come. Safe to cancel.
    async fn changed(&mut self) -> Option<u64>;
}

/// How one listener protects its connections.
#[derive(Clone)]
pub enum PortTls {
    /// TLS from the first byte (port 993).
    Implicit(TlsCertificates),
    /// `STARTTLS` (port 143), with sign-in refused before it except from loopback clients.
    /// `None` only for listeners on loopback addresses: nothing can be encrypted.
    Starttls(Option<TlsCertificates>),
}

pub(crate) struct Shared {
    pub(crate) view: Arc<dyn MailView>,
    pub(crate) actions: Arc<dyn MailActions>,
    pub(crate) devices: Arc<dyn DeviceAuth>,
    pub(crate) signal: Arc<dyn ChangeSignal>,
    pub(crate) settings: Settings,
    pub(crate) bytes: Arc<Semaphore>,
}

/// The IMAP server. Share it between its listeners.
pub struct ImapServer {
    shared: Arc<Shared>,
}

impl ImapServer {
    /// A server over these parts.
    pub fn new(
        view: Arc<dyn MailView>,
        actions: Arc<dyn MailActions>,
        devices: Arc<dyn DeviceAuth>,
        signal: Arc<dyn ChangeSignal>,
        settings: Settings,
    ) -> Arc<Self> {
        let permits = settings.response_bytes.clamp(1, Semaphore::MAX_PERMITS);
        Arc::new(Self {
            shared: Arc::new(Shared {
                view,
                actions,
                devices,
                signal,
                settings,
                bytes: Arc::new(Semaphore::new(permits)),
            }),
        })
    }

    /// Serves one listener until the runtime stops. `limiter` is shared by every listener of
    /// this server; only `trusted_proxies` may send PROXY v2 headers, and only with
    /// `proxy_protocol` on.
    pub async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        tls: PortTls,
        proxy_protocol: bool,
        trusted_proxies: TrustedProxies,
        limiter: ConnectionLimiter,
    ) {
        let mode = match &tls {
            PortTls::Implicit(certificates) => TlsMode::Implicit(certificates.clone()),
            PortTls::Starttls(Some(certificates)) => TlsMode::Starttls(certificates.clone()),
            PortTls::Starttls(None) => TlsMode::Off,
        };
        let mut options = ListenerOptions::new(mode);
        options.proxy_protocol = proxy_protocol;
        options.trusted_proxies = trusted_proxies;
        holtmail_serve_net::serve(listener, options, limiter, self).await;
    }
}

#[async_trait]
impl ConnectionHandler for ImapServer {
    async fn handle(&self, connection: holtmail_serve_net::Connection) {
        session::run(self.shared.clone(), connection).await;
    }
}

#[async_trait]
impl conn::Wire for holtmail_serve_net::Connection {
    fn can_start_tls(&self) -> bool {
        holtmail_serve_net::Connection::can_start_tls(self)
    }

    async fn start_tls(&mut self) -> bool {
        holtmail_serve_net::Connection::start_tls(self)
            .await
            .is_ok()
    }

    fn sign_in_allowed(&self) -> bool {
        self.info().sign_in_allowed()
    }

    fn client(&self) -> IpAddr {
        self.info().client.ip()
    }

    fn authenticated(&mut self, user: &UserId) -> bool {
        holtmail_serve_net::Connection::authenticated(self, user).is_ok()
    }
}

#[cfg(test)]
mod tests;
