//! The SMTP server itself, for both inbound mail and submission.
//!
//! Everything a caller decides goes through two traits: [`SmtpAuth`] turns a login into an
//! opaque [`Principal`], and [`Handler`] rules on the reverse path and each recipient and takes
//! the message. The crate root keeps the simpler receiver API on top of this one.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

use crate::session;

/// Who signed in, as the [`SmtpAuth`] that verified them describes it. This crate only carries
/// it from sign-in to the [`Handler`]; cloning shares the same value.
#[derive(Clone)]
pub struct Principal(Arc<dyn Any + Send + Sync>);

impl Principal {
    /// Wraps `value`.
    pub fn new<T: Any + Send + Sync>(value: T) -> Self {
        Self(Arc::new(value))
    }

    /// The wrapped value, when it is a `T`.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

impl fmt::Debug for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Principal(..)")
    }
}

/// A future that finishes when a signed-in session must end.
pub type Ended = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A successful sign-in.
pub struct SignedIn {
    /// Who signed in.
    pub principal: Principal,
    /// When set and finished, the session gets `421` and is closed the next time it waits for
    /// the client, or at once if it is waiting.
    pub ended: Option<Ended>,
}

impl SignedIn {
    /// A sign-in that lasts as long as the connection.
    pub fn new(principal: Principal) -> Self {
        Self {
            principal,
            ended: None,
        }
    }
}

/// Why a sign-in failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// Wrong login or secret (`535`). Three of these on one connection close it.
    #[error("wrong login or secret")]
    Invalid,
    /// This client has failed too often (`421`, and the connection is closed).
    #[error("too many failed sign-ins")]
    TooMany,
    /// The check could not be made (`454`). Does not count as a failure.
    #[error("sign-in is unavailable")]
    Unavailable,
}

/// Verifies what a client signs in with. Safe to share between tasks.
#[async_trait]
pub trait SmtpAuth: Send + Sync {
    /// Verifies `login` and `secret` from the client at `peer`. Never asked for a `PLAIN` sign-in
    /// whose authorization identity is neither empty nor `login`: those are refused before this.
    async fn verify(&self, login: &str, secret: &str, peer: IpAddr) -> Result<SignedIn, AuthError>;
}

/// What is known about the connection when a [`Handler`] is asked. Fields are public so handlers
/// can be tested without a connection.
#[derive(Debug, Clone)]
pub struct Session {
    /// The client's address.
    pub peer: IpAddr,
    /// The name the client gave in `EHLO` or `HELO`; `None` before that and after `STARTTLS`.
    pub helo: Option<String>,
    /// The session is encrypted: implicit TLS, `STARTTLS`, or a trusted proxy that terminated it.
    pub tls: bool,
    /// Who signed in with `AUTH`, if anyone.
    pub principal: Option<Principal>,
}

/// Who sent a message and to whom, as the client said.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// The client's address.
    pub peer: IpAddr,
    /// The name the client gave in `EHLO` or `HELO`.
    pub helo: String,
    /// The reverse path, without angle brackets; empty for a null reverse path.
    pub mail_from: String,
    /// Accepted recipients, in the order given, without angle brackets.
    pub recipients: Vec<String>,
    /// The session was encrypted.
    pub tls: bool,
    /// Who signed in with `AUTH`, if anyone.
    pub principal: Option<Principal>,
}

/// A refusal, sent to the client as `<code> <enhanced> <text>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code} {enhanced} {text}")]
pub struct Refused {
    code: u16,
    enhanced: &'static str,
    text: String,
}

impl Refused {
    /// A refusal with this reply code, enhanced status code (RFC 3463) and text. A `code`
    /// outside 400 to 599 becomes `451 4.3.0`, so a refusal can never read as a success. Control
    /// characters in `text` are replaced and it is cut to 200 characters.
    pub fn new(code: u16, enhanced: &'static str, text: impl Into<String>) -> Self {
        let (code, enhanced) = if (400..600).contains(&code) {
            (code, enhanced)
        } else {
            (451, "4.3.0")
        };
        Self {
            code,
            enhanced,
            text: text.into(),
        }
    }

    pub(crate) fn line(&self) -> String {
        let text: String = self
            .text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(200)
            .collect();
        format!("{} {} {text}", self.code, self.enhanced)
    }
}

/// What one client may send, when it differs from [`Options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest message in bytes; advertised with `SIZE`. [`Options::max_size`] still caps it.
    pub max_size: usize,
    /// Offers `SMTPUTF8`. When false, `MAIL` with `SMTPUTF8` gets `555` and addresses that are
    /// not ASCII get `553 5.6.7`.
    pub smtputf8: bool,
}

/// Rules on each step of a transaction and takes accepted mail. Safe to share between tasks.
#[async_trait]
pub trait Handler: Send + Sync {
    /// What this client may send; asked at each `EHLO` and `MAIL`. `None` (the default) means
    /// [`Options::max_size`], with `SMTPUTF8` offered.
    async fn limits(&self, _session: &Session) -> Option<Limits> {
        None
    }

    /// Whether to go on with this reverse path (without angle brackets; empty for `<>`). Asked
    /// after every session check passed. On a refusal there is no transaction.
    async fn mail_from(&self, session: &Session, address: &str) -> Result<(), Refused>;

    /// Whether to take mail for `address` (as written in `RCPT TO`, without angle brackets).
    async fn recipient(&self, session: &Session, address: &str) -> Result<(), Refused>;

    /// Takes one message. `Ok` is answered `250` and means the mail is safely held; the client
    /// then forgets it. Called at most once per transaction, and never for a message that was
    /// too large.
    async fn deliver(
        &self,
        session: &Session,
        envelope: &Envelope,
        data: Bytes,
    ) -> Result<(), Refused>;
}

/// How the server behaves.
#[derive(Clone)]
pub struct Options {
    /// The name in the greeting and in `EHLO` answers.
    pub hostname: String,
    /// The certificate for `STARTTLS` and implicit TLS.
    pub tls: Option<TlsAcceptor>,
    /// TLS from the first byte, with no `STARTTLS` (RFC 8314, port 465). Without `tls`, every
    /// connection is closed rather than served in the clear.
    pub implicit_tls: bool,
    /// Refuses `MAIL` before TLS (needs `tls`).
    pub require_tls: bool,
    /// Requires `AUTH` before `MAIL`. Signing in is offered only over TLS, or to loopback
    /// clients.
    pub auth: Option<Arc<dyn SmtpAuth>>,
    /// Sent in the greeting as `holtmail-instance=<token>`, so this instance can recognise
    /// itself as a mail server. Characters other than ASCII letters, digits, `-` and `_` are
    /// left out.
    pub instance_token: Option<String>,
    /// The largest message in bytes, enforced while reading.
    pub max_size: usize,
    /// Recipients per message.
    pub max_recipients: usize,
    /// How long a command or a stretch of data may take to arrive, and how long a reply may take
    /// to be read.
    pub idle_timeout: Duration,
    /// Replaces `idle_timeout` for waits before sign-in when `auth` is set.
    pub sign_in_timeout: Option<Duration>,
    /// Connections served at once by [`serve`]; more wait for a free place.
    pub max_connections: usize,
}

impl Options {
    /// Plain SMTP under `hostname` with 50 MiB messages, 100 recipients, five-minute waits and
    /// 32 connections, without sign-in or an instance token.
    pub fn new(hostname: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            tls: None,
            implicit_tls: false,
            require_tls: false,
            auth: None,
            instance_token: None,
            max_size: 50 << 20,
            max_recipients: 100,
            idle_timeout: Duration::from_secs(300),
            sign_in_timeout: None,
            max_connections: 32,
        }
    }
}

/// Any connection a client can be served on.
pub trait ClientStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ClientStream for T {}

/// An accepted connection for [`handle`], for listeners that do more than [`serve`].
pub struct Connection {
    /// The connection, ready for the first SMTP or TLS byte.
    pub stream: Box<dyn ClientStream>,
    /// The client's real address.
    pub peer: IpAddr,
    /// TLS is already in place, done by the listener or by a trusted proxy: the session counts
    /// as encrypted, and neither implicit TLS nor `STARTTLS` is used.
    pub tls_by_proxy: bool,
    /// Dropped as soon as the client signs in, and otherwise when the connection ends. Lets a
    /// listener count connections that have not signed in.
    pub until_signed_in: Option<Box<dyn Any + Send>>,
}

/// Serves one connection to its end. Logs carry client addresses and counts, never envelopes or
/// content.
pub async fn handle(connection: Connection, options: &Options, handler: &dyn Handler) {
    session::run(connection, options, handler).await;
}

/// Serves SMTP on `listener` until the runtime stops, [`Options::max_connections`] at a time.
/// Clients for which `allowed` is `false` are disconnected without a session: with `554`, or
/// silently under implicit TLS.
pub async fn serve(
    listener: TcpListener,
    options: Arc<Options>,
    handler: Arc<dyn Handler>,
    allowed: Arc<dyn Fn(IpAddr) -> bool + Send + Sync>,
) {
    let places = Arc::new(Semaphore::new(options.max_connections.max(1)));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "SMTP accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(place) = places.clone().acquire_owned().await else {
            return;
        };
        let options = options.clone();
        let handler = handler.clone();
        let allowed = allowed(peer.ip());
        tokio::spawn(async move {
            let _place = place;
            if allowed {
                let connection = Connection {
                    stream: Box::new(stream),
                    peer: peer.ip(),
                    tls_by_proxy: false,
                    until_signed_in: None,
                };
                handle(connection, &options, &*handler).await;
            } else {
                session::refuse(stream, peer.ip(), &options).await;
            }
        });
    }
}

#[cfg(test)]
mod tests;
