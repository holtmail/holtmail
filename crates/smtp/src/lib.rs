//! An SMTP receiver (RFC 5321) for mail servers that forward or journal into Holtmail.
//!
//! It speaks just enough SMTP to take mail in: `EHLO`/`HELO`, `STARTTLS`, `AUTH PLAIN` and
//! `LOGIN`, `MAIL`, `RCPT`, `DATA`, `RSET`, `NOOP`, `VRFY` (always `252`), `HELP` and `QUIT`.
//! It never relays: every recipient is asked of a [`Handler`], and accepted mail is handed to it
//! with the envelope. The data is passed on as received, after dot-unstuffing, without a
//! `Received:` line. Only `<CRLF>.<CRLF>` ends the data, so a bare `LF` cannot smuggle a second
//! message in.

mod session;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

/// Who sent a message and to whom, as the client said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The client's address.
    pub peer: IpAddr,
    /// The name the client gave in `EHLO` or `HELO`.
    pub helo: String,
    /// The reverse path, without angle brackets; empty for bounces.
    pub mail_from: String,
    /// Accepted recipients, in the order given, without angle brackets.
    pub recipients: Vec<String>,
    /// The session was encrypted with `STARTTLS`.
    pub tls: bool,
    /// The client signed in with `AUTH`.
    pub authenticated: bool,
}

/// What the handler says to a recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// Take mail for it.
    Accept,
    /// Refuse it permanently (`550`) with this text.
    Reject(String),
}

/// Why delivered mail was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Try again later (`451`).
    #[error("{0}")]
    Temporary(String),
    /// Do not send it again (`554`).
    #[error("{0}")]
    Permanent(String),
}

/// Decides about recipients and takes accepted mail.
#[async_trait]
pub trait Handler: Send + Sync {
    /// Whether to take mail for `address` (as written in `RCPT TO`, without angle brackets).
    async fn recipient(&self, address: &str) -> Recipient;

    /// Takes one message. `Ok` is answered `250` and means the mail is stored; the client then
    /// forgets it.
    async fn deliver(&self, envelope: &Envelope, data: Bytes) -> Result<(), Refusal>;
}

/// A user name and password clients sign in with.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The user name.
    pub username: String,
    /// The password.
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// How the receiver behaves.
#[derive(Clone)]
pub struct Options {
    /// The name in the greeting and in `EHLO` answers.
    pub hostname: String,
    /// Offers `STARTTLS` with this certificate.
    pub tls: Option<TlsAcceptor>,
    /// Refuses `MAIL` before `STARTTLS` (needs `tls`).
    pub require_tls: bool,
    /// Requires `AUTH` before `MAIL`. Signing in is offered only over TLS, or to loopback
    /// clients.
    pub credentials: Option<Credentials>,
    /// The largest message in bytes; advertised with `SIZE` and enforced while reading.
    pub max_size: usize,
    /// Recipients per message.
    pub max_recipients: usize,
    /// How long a command or a stretch of data may take to arrive.
    pub idle_timeout: Duration,
    /// Connections served at once; more wait for a free place.
    pub max_connections: usize,
}

impl Options {
    /// Plain SMTP under `hostname` with 50 MiB messages, 100 recipients, five-minute waits and
    /// 32 connections.
    pub fn new(hostname: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            tls: None,
            require_tls: false,
            credentials: None,
            max_size: 50 << 20,
            max_recipients: 100,
            idle_timeout: Duration::from_secs(300),
            max_connections: 32,
        }
    }
}

/// Serves SMTP on `listener` until the runtime stops. Clients for which `allowed` is `false`
/// get `554` at once and are disconnected. Logs carry addresses of clients and counts, never
/// envelopes or content.
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
            session::run(stream, peer, &options, &*handler, allowed).await;
        });
    }
}

/// The address a client connected from, for tests and logs.
pub fn peer_ip(peer: SocketAddr) -> IpAddr {
    match peer.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests;
