//! Handing prebuilt messages to an SMTP submission server (RFC 6409).
//!
//! [`SmtpSubmitter`] implements [`holtmail_submit::Submitter`] over SMTP with implicit TLS
//! (port 465) or `STARTTLS` (port 587), signing in with `AUTH PLAIN`, `LOGIN` or `XOAUTH2`.

use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::tls::TlsRoots;
use holtmail_submit::{Envelope, Limits, SubmitError, SubmitReport, Submitter};
use tokio_rustls::rustls::ClientConfig;

mod session;

/// How the connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte (port 465).
    Tls,
    /// Plain connection upgraded with `STARTTLS` before signing in (port 587). A server that does
    /// not offer `STARTTLS` is refused.
    Starttls,
    /// No TLS. Accepted only for `localhost` and loopback addresses, for tests and local relays.
    None,
}

/// What the client signs in with.
#[derive(Clone, PartialEq, Eq)]
pub enum Secret {
    /// A password, sent with `AUTH PLAIN` (or `LOGIN` when `PLAIN` is not offered).
    Password(String),
    /// An OAuth access token, sent with `AUTH XOAUTH2`.
    OAuthToken(String),
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => f.write_str("Password(..)"),
            Self::OAuthToken(_) => f.write_str("OAuthToken(..)"),
        }
    }
}

/// Where and how to submit.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    /// The server's host name; also the name its certificate must carry.
    pub host: String,
    /// The port, usually 465 for [`Security::Tls`] and 587 for [`Security::Starttls`].
    pub port: u16,
    /// How the connection is protected.
    pub security: Security,
    /// The user name to sign in with; no sign-in when `None`.
    pub username: Option<String>,
    /// The password or token for `username`.
    pub secret: Option<Secret>,
    /// The name given in `EHLO`.
    pub helo: String,
    /// The limit for connecting, the TLS handshake and every command reply.
    pub timeout: Duration,
    /// The limit for the server's reply once the whole message is sent (RFC 5321 suggests ten
    /// minutes).
    pub data_timeout: Duration,
}

impl SmtpConfig {
    /// `STARTTLS` on port 587 without sign-in, `EHLO localhost`, one-minute command and
    /// ten-minute data timeouts.
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 587,
            security: Security::Starttls,
            username: None,
            secret: None,
            helo: "localhost".into(),
            timeout: Duration::from_secs(60),
            data_timeout: Duration::from_secs(600),
        }
    }
}

/// An SMTP submission client. Every call opens its own connection and closes it afterwards, so
/// calls never share state except the [`Submitter::limits`] last seen.
pub struct SmtpSubmitter {
    config: SmtpConfig,
    tls: Arc<ClientConfig>,
    limits: Mutex<Limits>,
    own_instance: Option<String>,
}

impl fmt::Debug for SmtpSubmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpSubmitter")
            .field("host", &self.config.host)
            .field("port", &self.config.port)
            .finish_non_exhaustive()
    }
}

impl SmtpSubmitter {
    /// A client trusting the public web PKI roots. Fails with [`SubmitError::Invalid`] for
    /// [`Security::None`] towards a host that is not `localhost` or a loopback address, for a
    /// user name without a secret or the other way round, and for an empty host or `EHLO` name.
    pub fn new(config: SmtpConfig) -> Result<Self, SubmitError> {
        if config.host.is_empty() {
            return Err(SubmitError::Invalid("the host is empty".into()));
        }
        if config.helo.is_empty()
            || config
                .helo
                .contains(|c: char| c.is_whitespace() || c.is_control())
        {
            return Err(SubmitError::Invalid("the EHLO name is not a name".into()));
        }
        if config.security == Security::None && !is_loopback(&config.host) {
            return Err(SubmitError::Invalid(
                "a connection without TLS is allowed only to localhost".into(),
            ));
        }
        if config.username.is_some() != config.secret.is_some() {
            return Err(SubmitError::Invalid(
                "a user name and a secret go together".into(),
            ));
        }
        Ok(Self {
            config,
            tls: Arc::new(tls_config(&TlsRoots::default())?),
            limits: Mutex::new(Limits::default()),
            own_instance: None,
        })
    }

    /// Also trusts `roots`, for servers with a private CA.
    pub fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, SubmitError> {
        self.tls = Arc::new(tls_config(roots)?);
        Ok(self)
    }

    /// Refuses a server whose greeting carries `holtmail-instance=<token>`, with
    /// [`SubmitError::Invalid`]: that server is this very instance, and submitting to it would
    /// send the message around in a circle. Nothing is sent to such a server after its greeting.
    pub fn refusing_instance(mut self, token: impl Into<String>) -> Self {
        self.own_instance = Some(token.into());
        self
    }

    fn remember(&self, limits: &Limits) {
        *self.limits.lock().expect("limits") = limits.clone();
    }
}

#[async_trait]
impl Submitter for SmtpSubmitter {
    async fn check(&self) -> Result<Limits, SubmitError> {
        let session =
            session::Session::open(&self.config, self.tls.clone(), self.own_instance.as_deref())
                .await?;
        let limits = session.limits();
        self.remember(&limits);
        session.quit().await;
        Ok(limits)
    }

    async fn submit(
        &self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError> {
        envelope.validate(message)?;
        let mut session =
            session::Session::open(&self.config, self.tls.clone(), self.own_instance.as_deref())
                .await?;
        self.remember(&session.limits());
        let result = session.send(envelope, message).await;
        if !matches!(result, Err(SubmitError::Uncertain(_))) {
            session.quit().await;
        }
        result
    }

    fn limits(&self) -> Limits {
        self.limits.lock().expect("limits").clone()
    }
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn tls_config(extra: &TlsRoots) -> Result<ClientConfig, SubmitError> {
    let mut roots = tokio_rustls::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    for der in extra.iter() {
        roots
            .add(der.to_vec().into())
            .map_err(|e| SubmitError::Invalid(format!("an extra trusted certificate: {e}")))?;
    }
    Ok(ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth())
}

#[cfg(test)]
mod tests;
