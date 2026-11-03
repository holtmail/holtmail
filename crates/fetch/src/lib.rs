//! Contract for fetching remote images linked from messages.
//!
//! The reader's image proxy and the archiver both fetch through a [`Fetcher`], so one place decides
//! which addresses may be contacted. A message is written by its sender, so every URL in it is
//! untrusted: a fetcher must never reach the server's own network.

use async_trait::async_trait;
use bytes::Bytes;

/// An image downloaded from a remote server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    /// Lowercase `image/*` media type, without parameters.
    pub content_type: String,
    /// The image bytes, as served.
    pub bytes: Bytes,
}

/// Why an image could not be fetched.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The fetcher will not fetch this: not an `http(s)` URL, an address inside a private network,
    /// not an image or too large. Asking again gives the same answer.
    #[error("refused: {0}")]
    Refused(String),
    /// The remote server could not be reached or answered with an error. It may work later.
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

/// Conformance suite every [`Fetcher`] implementation runs in its own tests.
#[cfg(feature = "testing")]
pub mod testing {
    use super::*;

    /// URLs a fetcher must refuse without contacting anything outside the machine.
    pub const REFUSED: &[&str] = &[
        "not a url",
        "ftp://example.com/logo.png",
        "file:///etc/passwd",
        "data:image/png;base64,AAAA",
        "http://127.0.0.1/logo.png",
        "http://127.1/logo.png",
        "http://2130706433/logo.png",
        "http://0.0.0.0/logo.png",
        "http://localhost/logo.png",
        "http://localhost:8080/api/archives",
        "http://[::1]/logo.png",
        "http://[::ffff:127.0.0.1]/logo.png",
        "http://10.1.2.3/logo.png",
        "http://172.16.0.1/logo.png",
        "http://192.168.1.1/logo.png",
        "http://100.64.0.1/logo.png",
        "http://169.254.169.254/latest/meta-data/",
        "http://[fd00::1]/logo.png",
        "http://[fe80::1]/logo.png",
    ];

    /// Checks that `fetcher` refuses every URL in [`REFUSED`] with [`FetchError::Refused`].
    /// Panics on the first violation. Needs no network.
    pub async fn conformance(fetcher: &dyn Fetcher) {
        for url in REFUSED {
            match fetcher.image(url).await {
                Err(FetchError::Refused(_)) => {}
                other => panic!("{url} was not refused: {other:?}"),
            }
        }
    }
}
