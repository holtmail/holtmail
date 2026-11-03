//! A [`Fetcher`] over HTTP that only ever connects to public addresses.
//!
//! Host names go through a resolver that drops every non-public address, so a name that points
//! into a private network, or changes to one between checks, fails to connect. Addresses written
//! in the URL and in redirects are checked before any connection.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use holtmail_fetch::{FetchError, Fetcher, RemoteImage};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::redirect;
use url::{Host, Url};

/// Limits of an [`HttpFetcher`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Largest image accepted, in bytes.
    pub max_bytes: usize,
    /// Time for the whole request, redirects included.
    pub timeout: Duration,
    /// Redirects followed before giving up.
    pub max_redirects: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1024 * 1024,
            timeout: Duration::from_secs(15),
            max_redirects: 5,
        }
    }
}

/// Fetches remote images over HTTP and HTTPS from public addresses only.
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    max_bytes: usize,
}

impl HttpFetcher {
    /// A fetcher with these limits.
    pub fn new(options: Options) -> Result<Self, FetchError> {
        let max_redirects = options.max_redirects;
        let client = reqwest::Client::builder()
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .timeout(options.timeout)
            .connect_timeout(options.timeout.min(Duration::from_secs(5)))
            .referer(false)
            .no_proxy()
            .dns_resolver(Arc::new(PublicResolver))
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= max_redirects {
                    attempt.error("too many redirects")
                } else if let Err(reason) = check_url(attempt.url()) {
                    attempt.error(reason)
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| FetchError::Unavailable(e.to_string()))?;
        Ok(Self {
            client,
            max_bytes: options.max_bytes,
        })
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn image(&self, url: &str) -> Result<RemoteImage, FetchError> {
        let url = Url::parse(url).map_err(|_| FetchError::Refused("not a URL".into()))?;
        check_url(&url).map_err(|reason| FetchError::Refused(reason.into()))?;
        if let Some(Host::Domain(host)) = url.host() {
            let port = url.port_or_known_default().unwrap_or(80);
            if public_addrs(host, port).await?.is_empty() {
                return Err(FetchError::Refused("not a public address".into()));
            }
        }

        let mut response = self
            .client
            .get(url)
            .header(ACCEPT, "image/*")
            .send()
            .await
            .map_err(|e| FetchError::Unavailable(describe(&e)))?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Unavailable(format!("HTTP {}", status.as_u16())));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_bytes as u64)
        {
            return Err(FetchError::Refused("too large".into()));
        }
        let declared = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(media_type);

        let mut body = BytesMut::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| FetchError::Unavailable(describe(&e)))?
        {
            if body.len() + chunk.len() > self.max_bytes {
                return Err(FetchError::Refused("too large".into()));
            }
            body.extend_from_slice(&chunk);
        }
        let bytes: Bytes = body.freeze();
        let content_type = declared
            .filter(|t| t.starts_with("image/"))
            .or_else(|| sniff(&bytes).map(str::to_owned))
            .ok_or_else(|| FetchError::Refused("not an image".into()))?;
        Ok(RemoteImage {
            content_type,
            bytes,
        })
    }
}

fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timed out".into()
    } else if error.is_connect() {
        "could not connect".into()
    } else if error.is_redirect() {
        "bad redirect".into()
    } else {
        "request failed".into()
    }
}

fn media_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn check_url(url: &Url) -> Result<(), &'static str> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in the URL");
    }
    match url.host() {
        None => Err("no host"),
        Some(Host::Ipv4(ip)) if !is_public(IpAddr::V4(ip)) => Err("not a public address"),
        Some(Host::Ipv6(ip)) if !is_public(IpAddr::V6(ip)) => Err("not a public address"),
        Some(Host::Domain(name)) if is_local_name(name) => Err("not a public address"),
        Some(_) => Ok(()),
    }
}

fn is_local_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
        || !name.contains('.')
}

async fn public_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
    let found = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| FetchError::Unavailable("host not found".into()))?;
    Ok(found.filter(|addr| is_public(addr.ip())).collect())
}

/// Whether `ip` is an address on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(ip),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        || first == 0x2001 && ip.segments()[1] == 0x0db8
        || first == 0x0064 && ip.segments()[1] == 0xff9b)
}

struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            if is_local_name(&host) {
                return Err("not a public address".into());
            }
            let addrs = public_addrs(&host, 0).await.map_err(
                |e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() },
            )?;
            if addrs.is_empty() {
                return Err("not a public address".into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keeps_the_fetcher_contract() {
        let fetcher = HttpFetcher::new(Options::default()).unwrap();
        holtmail_fetch::testing::conformance(&fetcher).await;
    }

    #[test]
    fn tells_public_addresses_apart() {
        for public in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
    }

    #[test]
    fn sniffs_images_without_a_type() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(media_type("Image/PNG; charset=binary"), "image/png");
    }
}
