//! SMTP submission for the user's own mail clients. A client signs in with a device's app
//! password; what it submits goes through [`Submission`], so it meets the same locks, identities
//! and limits as a send from the web app, and leaves through a configured transport, never
//! directly to a recipient's server.
//!
//! This crate only speaks the protocol: it holds no writer, transport or store handle.

mod fixup;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::ArchiveId;
use holtmail_device::{DeviceAuth, DeviceAuthError, Principal as Device};
use holtmail_serve_net::{
    ClientSource, ConnectionHandler, ConnectionLimiter, LimitPlace, ListenerOptions,
    TlsCertificates, TlsMode, TrustedProxies,
};
use holtmail_smtp::server::{
    AuthError, Connection, Ended, Envelope, Handler, Limits, Options, Principal, Refused, Session,
    SignedIn, SmtpAuth,
};
use holtmail_submission::{Actor, Submission, SubmissionError, SubmissionState, Submit};
use tokio::net::TcpListener;

/// What a client may send at most, whatever its transports take: 50 MiB.
pub const MAX_SIZE: usize = 50 << 20;

const LEAK_TEXT: &str =
    "A recipient would see the real mailbox behind the alias this thread arrived through";

/// How one server behaves.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The name in the greeting and in `EHLO` answers.
    pub hostname: String,
    /// Sent in the greeting so this instance can recognise itself as a mail server.
    pub instance_token: Option<String>,
    /// How long a submitted message waits before it may leave; zero lets it leave at once.
    pub undo_window: Duration,
    /// The wait for a client that has not signed in.
    pub sign_in_timeout: Duration,
    /// The wait for a client that has.
    pub idle_timeout: Duration,
}

impl Settings {
    /// No undo window, 60 seconds before sign-in and 30 minutes after, as `hostname`.
    pub fn new(hostname: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            instance_token: None,
            undo_window: Duration::ZERO,
            sign_in_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(30 * 60),
        }
    }
}

/// How one listener protects its connections.
#[derive(Clone)]
pub enum PortTls {
    /// TLS from the first byte (port 465).
    Implicit(TlsCertificates),
    /// `STARTTLS` (port 587), with `MAIL` refused before it except from loopback clients.
    /// `None` is only for listeners on loopback addresses: nothing can be encrypted.
    Starttls(Option<TlsCertificates>),
}

/// Reads a device from the session.
fn device(session: &Session) -> Result<&Device, Refused> {
    session
        .principal
        .as_ref()
        .and_then(|principal| principal.get::<Device>())
        .ok_or_else(|| Refused::new(530, "5.7.0", "Authentication required"))
}

fn archive_of(device: &Device) -> Result<ArchiveId, Refused> {
    device
        .mail_archive
        .ok_or_else(|| Refused::new(553, "5.7.1", "This device has no mail archive"))
}

fn actor_of(device: &Device) -> Actor {
    Actor {
        user: device.user,
        device: Some(device.device),
        scopes: device.scopes.clone(),
    }
}

/// Maps a refusal to the reply a client gets. Reasons name rules, never addresses or content.
fn refusal(error: SubmissionError) -> Refused {
    match &error {
        SubmissionError::Forbidden(reason) => Refused::new(553, "5.7.1", reason.clone()),
        SubmissionError::FromMismatch => Refused::new(
            554,
            "5.7.1",
            "The From header must be the one address of the sending identity",
        ),
        SubmissionError::Leak => Refused::new(550, "5.7.1", LEAK_TEXT),
        SubmissionError::TooLarge { limit } => Refused::new(
            552,
            "5.3.4",
            format!("The message is larger than the {limit} bytes allowed"),
        ),
        SubmissionError::RateLimited { .. } => Refused::new(451, "4.7.1", error.to_string()),
        SubmissionError::Invalid(reason) => Refused::new(554, "5.6.0", reason.clone()),
        SubmissionError::Unavailable(_)
        | SubmissionError::NotFound(_)
        | SubmissionError::TooLate(_) => {
            tracing::warn!(%error, "submission failed");
            Refused::new(451, "4.3.0", "Temporary failure, try again later")
        }
    }
}

/// Rules on each transaction from a signed-in device.
struct Submitting {
    submission: Arc<dyn Submission>,
    undo_window: Duration,
}

#[async_trait]
impl Handler for Submitting {
    async fn limits(&self, session: &Session) -> Option<Limits> {
        let Ok(device) = device(session) else {
            return Some(Limits {
                max_size: MAX_SIZE,
                smtputf8: false,
            });
        };
        let limits = match device.mail_archive {
            Some(archive) => self.submission.limits(&actor_of(device), archive).await,
            None => return None,
        };
        match limits {
            Ok(limits) => Some(Limits {
                max_size: usize::try_from(limits.max_size).unwrap_or(MAX_SIZE),
                smtputf8: limits.smtputf8,
            }),
            Err(error) => {
                tracing::warn!(%error, "submission limits not known");
                None
            }
        }
    }

    async fn mail_from(&self, session: &Session, address: &str) -> Result<(), Refused> {
        let device = device(session)?;
        self.submission
            .sender(&actor_of(device), archive_of(device)?, address)
            .await
            .map_err(refusal)?;
        Ok(())
    }

    async fn recipient(&self, _session: &Session, address: &str) -> Result<(), Refused> {
        match address.rsplit_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => Ok(()),
            _ => Err(Refused::new(501, "5.1.3", "Not an address")),
        }
    }

    async fn deliver(
        &self,
        session: &Session,
        envelope: &Envelope,
        data: Bytes,
    ) -> Result<(), Refused> {
        let device = device(session)?;
        let actor = actor_of(device);
        let archive = archive_of(device)?;
        let identity = self
            .submission
            .sender(&actor, archive, &envelope.mail_from)
            .await
            .map_err(refusal)?;
        let domain = identity
            .address
            .rsplit_once('@')
            .map_or("invalid", |(_, domain)| domain);
        let prepared = fixup::prepare(&data, domain);
        let key = fixup::idempotency_key(
            &device.device,
            &envelope.recipients,
            prepared.client_message_id.as_deref(),
            &data,
        );
        let submitted = self
            .submission
            .queue(Submit {
                actor,
                archive,
                identity: identity.id,
                mail_from: envelope.mail_from.clone(),
                recipients: envelope.recipients.clone(),
                message: prepared.bytes,
                send_at: None,
                undo_window: Some(self.undo_window),
                idempotency_key: Some(key),
                allow_leaks: false,
                route: None,
            })
            .await
            .map_err(refusal)?;
        if submitted.state == SubmissionState::Cancelled {
            return Err(Refused::new(
                554,
                "5.7.1",
                "This message was cancelled and will not be sent",
            ));
        }
        tracing::info!(outbox = %submitted.id, device = %device.device,
            recipients = envelope.recipients.len(), "message submitted");
        Ok(())
    }
}

/// Signs one connection in with an app password, and moves it to its user's connection count.
struct DeviceSignIn {
    devices: Arc<dyn DeviceAuth>,
    place: LimitPlace,
}

#[async_trait]
impl SmtpAuth for DeviceSignIn {
    async fn verify(&self, login: &str, secret: &str, peer: IpAddr) -> Result<SignedIn, AuthError> {
        let mut revoked = self.devices.revocations();
        let verified = self
            .devices
            .verify(login, secret, peer, holtmail_core::DeviceProtocol::Smtp)
            .await;
        let device = match verified {
            Ok(device) => device,
            Err(DeviceAuthError::TooMany { .. }) => return Err(AuthError::TooMany),
            Err(DeviceAuthError::Unavailable(error)) => {
                tracing::warn!(%error, "device sign-in unavailable");
                return Err(AuthError::Unavailable);
            }
            Err(DeviceAuthError::Invalid | DeviceAuthError::Revoked) => {
                return Err(AuthError::Invalid);
            }
        };
        if self.place.authenticated(&device.user).is_err() {
            return Err(AuthError::TooMany);
        }
        let id = device.device;
        let ended: Ended = Box::pin(async move { while revoked.next().await != id {} });
        Ok(SignedIn {
            principal: Principal::new(device),
            ended: Some(ended),
        })
    }
}

/// The submission server. Share it between its listeners.
pub struct SubmitServer {
    handler: Submitting,
    devices: Arc<dyn DeviceAuth>,
    settings: Settings,
}

impl SubmitServer {
    /// A server over these parts.
    pub fn new(
        submission: Arc<dyn Submission>,
        devices: Arc<dyn DeviceAuth>,
        settings: Settings,
    ) -> Arc<Self> {
        Arc::new(Self {
            handler: Submitting {
                submission,
                undo_window: settings.undo_window,
            },
            devices,
            settings,
        })
    }

    async fn serve_connection(&self, connection: holtmail_serve_net::Connection, tls: &PortTls) {
        let info = *connection.info();
        let mut options = Options::new(self.settings.hostname.clone());
        options.max_size = MAX_SIZE;
        options.idle_timeout = self.settings.idle_timeout;
        options.sign_in_timeout = Some(self.settings.sign_in_timeout);
        options.instance_token = self.settings.instance_token.clone();
        options.auth = Some(Arc::new(DeviceSignIn {
            devices: self.devices.clone(),
            place: connection.place(),
        }));
        if let PortTls::Starttls(certificates) = tls {
            options.tls = certificates.as_ref().map(TlsCertificates::acceptor);
            options.require_tls = !info.loopback();
        }
        let peer = if info.source == ClientSource::ProxyLocal && info.client.ip().is_loopback() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            info.client.ip()
        };
        let connection = Connection {
            stream: Box::new(connection),
            peer,
            tls_by_proxy: info.encrypted(),
            until_signed_in: None,
        };
        holtmail_smtp::server::handle(connection, &options, &self.handler).await;
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
            PortTls::Starttls(_) => TlsMode::Off,
        };
        let mut options = ListenerOptions::new(mode);
        options.proxy_protocol = proxy_protocol;
        options.trusted_proxies = trusted_proxies;
        let handler = Arc::new(Port { server: self, tls });
        holtmail_serve_net::serve(listener, options, limiter, handler).await;
    }
}

struct Port {
    server: Arc<SubmitServer>,
    tls: PortTls,
}

#[async_trait]
impl ConnectionHandler for Port {
    async fn handle(&self, connection: holtmail_serve_net::Connection) {
        self.server.serve_connection(connection, &self.tls).await;
    }
}

#[cfg(test)]
mod tests;
