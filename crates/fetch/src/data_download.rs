use async_trait::async_trait;
use bytes::Bytes;

use crate::FetchError;

/// Downloads Holtmail's own data files, such as the domain category tables, from a fixed set of
/// hosts.
///
/// Unlike [`Fetcher`](crate::Fetcher) and [`OneClick`](crate::OneClick), its URLs come from the
/// server's configuration, never from message content.
#[async_trait]
pub trait DataDownload: Send + Sync {
    /// Downloads the file at `url`, or learns that the copy tagged `etag` is still current.
    ///
    /// Guarantees:
    ///
    /// - Only `https` URLs without user name or password, on any port the URL names.
    /// - The URL's host and the host of every redirect target must be in the implementation's
    ///   pinned host list: an exact, case-insensitive match, never a subdomain of a listed host.
    ///   Otherwise the call fails with [`FetchError::Refused`] and that host is never contacted.
    /// - At most the implementation's redirect limit (2 unless configured) of 301, 302, 303, 307
    ///   and 308 responses is followed; one more is [`FetchError::Refused`] and its target is not
    ///   requested. A redirect to a URL that breaks the rules above is refused the same way.
    /// - Never through a proxy, from configuration or the environment.
    /// - No cookies, no `Authorization`, no `Referer`; a `Set-Cookie` on a redirect is ignored.
    /// - With an `etag`, every request of the call carries `If-None-Match: <etag>` verbatim;
    ///   without one, none does. An `etag` that is not a valid header value is refused.
    /// - A `304` answers [`Download::NotModified`] when an `etag` was sent, and is
    ///   [`FetchError::Unavailable`] otherwise. A 2xx is [`Download::Fetched`]. Any other status,
    ///   a failed connection or TLS handshake, and an unknown host are
    ///   [`FetchError::Unavailable`].
    /// - A body longer than `max_bytes`, whether announced by `Content-Length` or found while
    ///   reading, is [`FetchError::Refused`]; no more than `max_bytes` of it is held in memory.
    /// - The implementation's timeout (60 seconds unless configured) covers the whole call:
    ///   connections, every redirect and the body. Running out is [`FetchError::Unavailable`].
    /// - The returned `etag` is the final response's `ETag` header, verbatim, when it is
    ///   non-empty printable ASCII, and `None` otherwise.
    /// - Each call makes at most one request per hop and never retries.
    async fn get(
        &self,
        url: &str,
        etag: Option<&str>,
        max_bytes: u64,
    ) -> Result<Download, FetchError>;
}

/// The result of a [`DataDownload::get`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Download {
    /// The server confirmed that the copy tagged with the given `etag` is current.
    NotModified,
    /// The file as served.
    Fetched {
        /// The whole body, at most `max_bytes` long.
        bytes: Bytes,
        /// The final response's `ETag`, to send on the next call.
        etag: Option<String>,
    },
}
