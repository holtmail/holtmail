use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::BytesMut;
use holtmail_core::TlsRoots;
use holtmail_fetch::{DataDownload, Download, FetchError};
use reqwest::header::{ETAG, HeaderValue, IF_NONE_MATCH};
use reqwest::{StatusCode, redirect};
use url::Url;

use crate::describe;

/// The hosts a GitHub release asset is served from: the download URL and the two hosts its
/// signed redirect may point to.
pub const GITHUB_RELEASE_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Settings of an [`HttpDataDownload`].
#[derive(Debug, Clone)]
pub struct DataDownloadOptions {
    /// Certificates trusted for https besides the built-in roots.
    pub roots: TlsRoots,
    /// The only hosts contacted, for the first request and every redirect; compared exactly and
    /// case-insensitively.
    pub pinned_hosts: Vec<String>,
    /// Time for the whole call, redirects and body included.
    pub timeout: Duration,
    /// Redirects followed before the call is refused.
    pub max_redirects: usize,
}

impl Default for DataDownloadOptions {
    fn default() -> Self {
        Self {
            roots: TlsRoots::default(),
            pinned_hosts: GITHUB_RELEASE_HOSTS
                .iter()
                .map(|h| (*h).to_owned())
                .collect(),
            timeout: Duration::from_secs(60),
            max_redirects: 2,
        }
    }
}

/// Downloads data files over HTTPS, directly, from pinned hosts only.
pub struct HttpDataDownload {
    client: reqwest::Client,
    pinned: Arc<[String]>,
    timeout: Duration,
}

impl HttpDataDownload {
    /// A downloader with these settings. Fails when an extra root is not a usable certificate.
    pub fn new(options: DataDownloadOptions) -> Result<Self, FetchError> {
        Self::build(options, &[])
    }

    fn build(options: DataDownloadOptions, loopback: &[String]) -> Result<Self, FetchError> {
        let certificates = options
            .roots
            .iter()
            .map(reqwest::Certificate::from_der)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| FetchError::Unavailable(format!("an extra trusted certificate: {e}")))?;
        let pinned: Arc<[String]> = options
            .pinned_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        let max_redirects = options.max_redirects;
        let policy_pins = pinned.clone();
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > max_redirects {
                return attempt.error("too many redirects");
            }
            match check(attempt.url(), &policy_pins) {
                Ok(()) => attempt.follow(),
                Err(reason) => attempt.error(reason),
            }
        });
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(policy)
            .referer(false)
            .timeout(options.timeout)
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .tls_certs_merge(certificates);
        for host in loopback {
            builder = builder.resolve(host, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
        }
        let client = builder
            .build()
            .map_err(|e| FetchError::Unavailable(e.to_string()))?;
        Ok(Self {
            client,
            pinned,
            timeout: options.timeout,
        })
    }

    async fn send(
        &self,
        url: Url,
        etag: Option<HeaderValue>,
        max_bytes: u64,
    ) -> Result<Download, FetchError> {
        let conditional = etag.is_some();
        let mut request = self.client.get(url);
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let mut response = request.send().await.map_err(failure)?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED && conditional {
            return Ok(Download::NotModified);
        }
        if !status.is_success() {
            return Err(FetchError::Unavailable(format!(
                "status {}",
                status.as_u16()
            )));
        }
        let too_large = || FetchError::Refused("the file is larger than allowed".into());
        if response.content_length().is_some_and(|n| n > max_bytes) {
            return Err(too_large());
        }
        let etag = response
            .headers()
            .get(ETAG)
            .map(HeaderValue::as_bytes)
            .filter(|v| !v.is_empty() && v.iter().all(|b| (0x20..=0x7e).contains(b)))
            .map(|v| String::from_utf8_lossy(v).into_owned());
        let capacity = response.content_length().unwrap_or(0).min(max_bytes);
        let mut bytes = BytesMut::with_capacity(usize::try_from(capacity).unwrap_or(0));
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if (bytes.len() + chunk.len()) as u64 > max_bytes {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Download::Fetched {
            bytes: bytes.freeze(),
            etag,
        })
    }
}

#[async_trait]
impl DataDownload for HttpDataDownload {
    async fn get(
        &self,
        url: &str,
        etag: Option<&str>,
        max_bytes: u64,
    ) -> Result<Download, FetchError> {
        let url = Url::parse(url).map_err(|_| FetchError::Refused("not a URL".into()))?;
        check(&url, &self.pinned).map_err(|reason| FetchError::Refused(reason.into()))?;
        let etag = etag
            .map(HeaderValue::from_str)
            .transpose()
            .map_err(|_| FetchError::Refused("the etag is not a header value".into()))?;
        tokio::time::timeout(self.timeout, self.send(url, etag, max_bytes))
            .await
            .map_err(|_| FetchError::Unavailable("timed out".into()))?
    }
}

fn check(url: &Url, pinned: &[String]) -> Result<(), &'static str> {
    if url.scheme() != "https" {
        return Err("only https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in the URL");
    }
    let host = url.host_str().ok_or("no host")?;
    if pinned.iter().any(|p| p.eq_ignore_ascii_case(host)) {
        Ok(())
    } else {
        Err("host not pinned")
    }
}

fn failure(error: reqwest::Error) -> FetchError {
    if error.is_redirect() {
        FetchError::Refused(describe(&error))
    } else {
        FetchError::Unavailable(describe(&error))
    }
}

#[cfg(test)]
mod tests {
    use holtmail_fetch::testing::{DataDownloadHarness, data_download_cases};

    use super::*;

    fn options(harness: &DataDownloadHarness) -> DataDownloadOptions {
        DataDownloadOptions {
            roots: harness.roots.clone(),
            pinned_hosts: harness.pinned_hosts.clone(),
            timeout: harness.timeout,
            ..DataDownloadOptions::default()
        }
    }

    fn from_harness(harness: DataDownloadHarness) -> Box<dyn DataDownload> {
        Box::new(HttpDataDownload::build(options(&harness), &harness.loopback_hosts).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_data_download_contract() {
        data_download_cases(&from_harness).await;
    }

    #[test]
    fn pins_github_release_hosts_by_default() {
        let options = DataDownloadOptions::default();
        assert_eq!(
            options.pinned_hosts,
            [
                "github.com",
                "objects.githubusercontent.com",
                "release-assets.githubusercontent.com"
            ]
        );
        assert_eq!(options.max_redirects, 2);
        assert_eq!(options.timeout, Duration::from_secs(60));
    }

    #[test]
    fn refuses_unusable_roots() {
        assert!(
            HttpDataDownload::new(DataDownloadOptions {
                roots: TlsRoots::from_der(vec![vec![1, 2, 3]]),
                ..DataDownloadOptions::default()
            })
            .is_err()
        );
    }

    #[test]
    fn checks_scheme_credentials_and_exact_hosts() {
        let pinned = ["github.com".to_owned()];
        let check = |url: &str| check(&Url::parse(url).unwrap(), &pinned);
        assert!(check("https://github.com/o/r/releases/download/t/f").is_ok());
        assert!(check("https://GitHub.com:8443/f").is_ok());
        for url in [
            "http://github.com/f",
            "https://user@github.com/f",
            "https://api.github.com/f",
            "https://github.com.example.org/f",
            "https://example.com/f",
        ] {
            assert!(check(url).is_err(), "{url}");
        }
    }

    struct IgnoresEtag(HttpDataDownload);

    #[async_trait]
    impl DataDownload for IgnoresEtag {
        async fn get(&self, url: &str, _: Option<&str>, max: u64) -> Result<Download, FetchError> {
            self.0.get(url, None, max).await
        }
    }

    struct IgnoresCap(HttpDataDownload);

    #[async_trait]
    impl DataDownload for IgnoresCap {
        async fn get(&self, url: &str, etag: Option<&str>, _: u64) -> Result<Download, FetchError> {
            self.0.get(url, etag, u64::MAX).await
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[should_panic(expected = "v0-old")]
    async fn the_suite_catches_an_ignored_etag() {
        data_download_cases(&|h| {
            Box::new(IgnoresEtag(
                HttpDataDownload::build(options(&h), &h.loopback_hosts).unwrap(),
            ))
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[should_panic(expected = "expected a refusal")]
    async fn the_suite_catches_an_ignored_size_cap() {
        data_download_cases(&|h| {
            Box::new(IgnoresCap(
                HttpDataDownload::build(options(&h), &h.loopback_hosts).unwrap(),
            ))
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[should_panic(expected = "three redirects")]
    async fn the_suite_catches_a_third_redirect() {
        data_download_cases(&|h| {
            let options = DataDownloadOptions {
                max_redirects: 3,
                ..options(&h)
            };
            Box::new(HttpDataDownload::build(options, &h.loopback_hosts).unwrap())
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[should_panic(expected = "an unpinned host")]
    async fn the_suite_catches_an_unpinned_host() {
        data_download_cases(&|h| {
            let mut options = options(&h);
            options.pinned_hosts.push("outside.example.org".into());
            Box::new(HttpDataDownload::build(options, &h.loopback_hosts).unwrap())
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[should_panic(expected = "/slow within the timeout")]
    async fn the_suite_catches_an_ignored_timeout() {
        data_download_cases(&|h| {
            let options = DataDownloadOptions {
                timeout: Duration::from_secs(30),
                ..options(&h)
            };
            Box::new(HttpDataDownload::build(options, &h.loopback_hosts).unwrap())
        })
        .await;
    }
}
