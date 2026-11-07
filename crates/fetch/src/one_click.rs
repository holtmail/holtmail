use async_trait::async_trait;

use crate::FetchError;

/// Sends RFC 8058 one-click unsubscribe requests.
///
/// This is the only outbound request tied to message content that the server makes, and only
/// after a user asked for it.
#[async_trait]
pub trait OneClick: Send + Sync {
    /// Sends one `POST` to `url` and reports how the server answered.
    ///
    /// Guarantees:
    ///
    /// - Only `https` URLs on port 443, explicit or default, without user name or password, whose
    ///   host is a name or a public IP address. Local names such as `localhost` are refused.
    /// - Never through a proxy, from configuration or the environment.
    /// - Every A and AAAA answer for the host must be public, or the request is refused before any
    ///   connection; an empty answer is refused too.
    /// - The connection goes only to the addresses that were checked, on port 443, and TLS
    ///   verifies the certificate for the URL's host name.
    /// - Redirects are not followed: a 3xx is reported as [`OneClickOutcome::Redirected`] and its
    ///   target is never requested.
    /// - No cookies, no `Authorization`, no `Referer`.
    /// - The body is exactly one `multipart/form-data` part named `List-Unsubscribe` with the
    ///   value `One-Click`.
    /// - The implementation gives up after its timeout (10 seconds unless configured) with
    ///   [`FetchError::Unavailable`]. It reads at most its body limit (64 KiB unless configured)
    ///   of the response and discards it; a longer body is not an error.
    ///
    /// Refusals are [`FetchError::Refused`]; resolution, connection, TLS and timeout failures are
    /// [`FetchError::Unavailable`]. Each call sends at most one request and never retries.
    async fn post(&self, url: &str) -> Result<OneClickResponse, FetchError>;
}

/// How a server answered a one-click request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneClickResponse {
    /// The class of the status.
    pub outcome: OneClickOutcome,
    /// The HTTP status code as received.
    pub status: u16,
}

/// The class of a one-click response status, stored as the unsubscribe outcome of the same name.
///
/// Together with [`FetchError`] (`Refused` is `refused`, `Unavailable` is `network`) these cover
/// every stored outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OneClickOutcome {
    /// A 2xx status; stored as `accepted`.
    Accepted,
    /// A 3xx status, not followed; stored as `redirected`.
    Redirected,
    /// Any other status; stored as `http_error`.
    HttpError,
}

impl OneClickOutcome {
    /// The outcome for an HTTP status code: 200–299 accepted, 300–399 redirected, anything else
    /// an HTTP error.
    pub fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Accepted,
            300..=399 => Self::Redirected,
            _ => Self::HttpError,
        }
    }
}
