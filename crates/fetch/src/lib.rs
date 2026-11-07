//! Contract for outbound requests that message content can cause.
//!
//! The reader's image proxy and the archiver fetch images through a [`Fetcher`], and one-click
//! unsubscribe requests go through a [`OneClick`], so one place decides which addresses may be
//! contacted. A message is written by its sender, so every URL in it is untrusted: an
//! implementation must never reach the server's own network. Holtmail's own data files come
//! through a [`DataDownload`], which only ever contacts a pinned list of hosts.

mod data_download;
mod one_click;

#[cfg(feature = "testing")]
pub mod testing;

use async_trait::async_trait;
use bytes::Bytes;

pub use data_download::{DataDownload, Download};
pub use one_click::{OneClick, OneClickOutcome, OneClickResponse};

/// An image downloaded from a remote server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    /// Lowercase `image/*` media type, without parameters.
    pub content_type: String,
    /// The image bytes, as served.
    pub bytes: Bytes,
}

/// Why a request could not be made or completed.
///
/// For [`OneClick::post`] the variants map one to one onto the stored unsubscribe outcomes:
/// `Refused` is `refused` and `Unavailable` is `network`. An answered request is never an error,
/// whatever its status; see [`OneClickOutcome`].
///
/// For [`DataDownload::get`], `Refused` means the URL, a redirect, the `etag` or the body size
/// broke the rules and nothing more was requested; `Unavailable` means the host could not be
/// reached in time or answered with a status other than 2xx or a `304` to a conditional request.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The request will not be made: a URL or address the implementation does not allow, or for
    /// images not an image or too large. Asking again gives the same answer. For one-click, no
    /// connection was opened.
    #[error("refused: {0}")]
    Refused(String),
    /// The remote server could not be reached: the name did not resolve, the connection or TLS
    /// failed, or the time ran out. For images, an error status too. It may work later.
    #[error("unavailable: {0}")]
    Unavailable(String),
}

/// Fetches remote images on behalf of readers and the archiver.
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Downloads the image at `url`.
    ///
    /// Only `http` and `https` URLs are fetched. The fetcher never connects to loopback, private,
    /// link-local, unique-local or otherwise non-public addresses, whether the URL names them
    /// directly, through a host name or through a redirect. It returns only `image/*` content, up
    /// to the implementation's size limit, and gives up after its timeout. It sends no cookies and
    /// no referrer.
    async fn image(&self, url: &str) -> Result<RemoteImage, FetchError>;
}
