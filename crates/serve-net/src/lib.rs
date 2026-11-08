//! The TCP listener every protocol front end stands on: PROXY v2 from trusted proxies,
//! connection limits, implicit TLS and STARTTLS, and certificates that reload when their files
//! change. It knows no protocol; a front end gets a finished [`Connection`].

mod connection;
mod limits;
mod listener;
mod tls;
mod trusted;

pub use connection::{ClientSource, Connection, ConnectionInfo, Transport};
pub use limits::{ConnectionLimiter, LimitExceeded, LimitPlace, Limits};
pub use listener::{ConnectionHandler, ListenerOptions, TlsMode, serve};
pub use tls::{DEFAULT_RELOAD_INTERVAL, TlsCertificates};
pub use trusted::TrustedProxies;

/// Why setting up or upgrading a connection failed.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// A `trusted_proxies` entry is neither an IP address nor a CIDR range.
    #[error("not an IP address or CIDR range: {0}")]
    InvalidProxy(String),
    /// The certificate or key file could not be read or used.
    #[error("TLS certificate: {0}")]
    Certificate(String),
    /// `STARTTLS` is not available on this connection: the listener has no certificate, or the
    /// connection is already encrypted.
    #[error("STARTTLS is not available on this connection")]
    TlsUnavailable,
    /// The TLS handshake failed or ran out of time. The connection cannot be used any more.
    #[error("TLS handshake failed")]
    Handshake,
    /// A connection limit is reached.
    #[error(transparent)]
    Limit(#[from] LimitExceeded),
}
