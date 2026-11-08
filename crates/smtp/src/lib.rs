//! An SMTP server (RFC 5321) for mail that comes to Holtmail: inbound from mail servers that
//! forward or journal into it, and submission from mail clients.
//!
//! It speaks `EHLO`/`HELO`, `STARTTLS`, implicit TLS, `AUTH PLAIN` and `LOGIN`, `MAIL`, `RCPT`,
//! `DATA`, `RSET`, `NOOP`, `VRFY` (always `252`), `HELP` and `QUIT`. It never relays: the reverse
//! path and every recipient are ruled on by a handler, and accepted mail is handed to it with
//! the envelope. The data is passed on as received, after dot-unstuffing, without a `Received:`
//! line. Only `<CRLF>.<CRLF>` ends the data, so a bare `LF` cannot smuggle a second message in.
//!
//! [`server`] is the full API. The crate root is a simpler one for receiving: one fixed sign-in,
//! every reverse path taken, and handlers that do not need the session.

pub mod server;
mod session;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::server::{AuthError, Principal, Refused, Session, SignedIn, SmtpAuth};

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

#[async_trait]
impl SmtpAuth for Credentials {
    async fn verify(
        &self,
        login: &str,
        secret: &str,
        _peer: IpAddr,
    ) -> Result<SignedIn, AuthError> {
        if equal(login.as_bytes(), self.username.as_bytes())
            & equal(secret.as_bytes(), self.password.as_bytes())
        {
            Ok(SignedIn::new(Principal::new(())))
        } else {
            Err(AuthError::Invalid)
        }
    }
}

fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= usize::from(x ^ y);
    }
    diff == 0
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

    fn server(&self) -> server::Options {
        server::Options {
            tls: self.tls.clone(),
            require_tls: self.require_tls,
            auth: self
                .credentials
                .clone()
                .map(|c| Arc::new(c) as Arc<dyn SmtpAuth>),
            max_size: self.max_size,
            max_recipients: self.max_recipients,
            idle_timeout: self.idle_timeout,
            max_connections: self.max_connections,
            ..server::Options::new(self.hostname.clone())
        }
    }
}

struct Receiver(Arc<dyn Handler>);

#[async_trait]
impl server::Handler for Receiver {
    async fn mail_from(&self, _session: &Session, _address: &str) -> Result<(), Refused> {
        Ok(())
    }

    async fn recipient(&self, _session: &Session, address: &str) -> Result<(), Refused> {
        match self.0.recipient(address).await {
            Recipient::Accept => Ok(()),
            Recipient::Reject(text) => Err(Refused::new(550, "5.1.1", text)),
        }
    }

    async fn deliver(
        &self,
        _session: &Session,
        envelope: &server::Envelope,
        data: Bytes,
    ) -> Result<(), Refused> {
        let envelope = Envelope {
            peer: envelope.peer,
            helo: envelope.helo.clone(),
            mail_from: envelope.mail_from.clone(),
            recipients: envelope.recipients.clone(),
            tls: envelope.tls,
            authenticated: envelope.principal.is_some(),
        };
        self.0.deliver(&envelope, data).await.map_err(|r| match r {
            Refusal::Temporary(text) => Refused::new(451, "4.3.0", text),
            Refusal::Permanent(text) => Refused::new(554, "5.6.0", text),
        })
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
    server::serve(
        listener,
        Arc::new(options.server()),
        Arc::new(Receiver(handler)),
        allowed,
    )
    .await;
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
