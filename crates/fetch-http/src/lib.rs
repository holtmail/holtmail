//! A [`Fetcher`] over HTTP that only ever connects to public addresses.
//!
//! Host names go through a resolver that drops every non-public address, so a name that points
//! into a private network, or changes to one between checks, fails to connect. Addresses written
//! in the URL and in redirects are checked before any connection. Requests can go through an
//! outbound proxy, so senders see the proxy's address instead of the server's.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use holtmail_core::TlsRoots;
use holtmail_fetch::{FetchError, Fetcher, RemoteImage};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Proxy, redirect};
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
    /// Outbound proxy for every request: `http://`, `https://`, `socks5://` or `socks5h://`, with
    /// optional `user:password@`. With `socks5h` host names are resolved by the proxy and never
    /// looked up locally.
    pub proxy: Option<String>,
    /// Certificates trusted for https besides the built-in roots.
    pub roots: TlsRoots,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1024 * 1024,
            timeout: Duration::from_secs(15),
            max_redirects: 5,
            proxy: None,
            roots: TlsRoots::default(),
        }
    }
}

/// Fetches remote images over HTTP and HTTPS from public addresses only.
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    max_bytes: usize,
    remote_dns: bool,
}

/// Where an outbound proxy is, without its credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyAddress {
    /// `http`, `https`, `socks5` or `socks5h`.
    pub scheme: String,
    /// Host name or address.
    pub host: String,
    /// Port, the scheme's default when the URL has none (1080 for SOCKS).
    pub port: u16,
}

impl std::fmt::Display for ProxyAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

/// Checks a proxy URL and returns where it points, or a reason it cannot be used.
pub fn proxy_address(proxy: &str) -> Result<ProxyAddress, String> {
    let url = Url::parse(proxy.trim()).map_err(|_| "The proxy is not a URL.".to_owned())?;
    let scheme = url.scheme().to_owned();
    let port = match scheme.as_str() {
        "http" => url.port().unwrap_or(80),
        "https" => url.port().unwrap_or(443),
        "socks5" | "socks5h" => url.port().unwrap_or(1080),
        _ => return Err("Use an http://, https://, socks5:// or socks5h:// proxy.".into()),
    };
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or("The proxy has no host.")?
        .trim_matches(['[', ']'])
        .to_owned();
    if url.path() != "/" && !url.path().is_empty() || url.query().is_some() {
        return Err(
            "A proxy URL has only a scheme, host, port and optional user and password.".into(),
        );
    }
    Ok(ProxyAddress { scheme, host, port })
}

impl HttpFetcher {
    /// A fetcher with these limits.
    pub fn new(options: Options) -> Result<Self, FetchError> {
        let max_redirects = options.max_redirects;
        let proxy = options
            .proxy
            .as_deref()
            .map(|proxy| {
                let address = proxy_address(proxy).map_err(FetchError::Refused)?;
                let proxy =
                    Proxy::all(proxy.trim()).map_err(|e| FetchError::Refused(e.to_string()))?;
                Ok::<_, FetchError>((address, proxy))
            })
            .transpose()?;
        let remote_dns = proxy
            .as_ref()
            .is_some_and(|(address, _)| address.scheme == "socks5h");
        let certificates = options
            .roots
            .iter()
            .map(reqwest::Certificate::from_der)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| FetchError::Unavailable(format!("an extra trusted certificate: {e}")))?;
        let mut builder = reqwest::Client::builder().no_proxy();
        if let Some((_, proxy)) = proxy {
            builder = builder.proxy(proxy);
        }
        let client = builder
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .timeout(options.timeout)
            .connect_timeout(options.timeout.min(Duration::from_secs(5)))
            .referer(false)
            .tls_certs_merge(certificates)
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
            remote_dns,
        })
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn image(&self, url: &str) -> Result<RemoteImage, FetchError> {
        let url = Url::parse(url).map_err(|_| FetchError::Refused("not a URL".into()))?;
        check_url(&url).map_err(|reason| FetchError::Refused(reason.into()))?;
        if let Some(Host::Domain(host)) = url.host().filter(|_| !self.remote_dns) {
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

    #[tokio::test]
    async fn keeps_the_contract_through_a_proxy() {
        for proxy in [
            "socks5h://127.0.0.1:9050",
            "http://user:secret@proxy.example.com:3128",
        ] {
            let fetcher = HttpFetcher::new(Options {
                proxy: Some(proxy.into()),
                ..Options::default()
            })
            .unwrap();
            holtmail_fetch::testing::conformance(&fetcher).await;
        }
    }

    async fn socks5_to(target: SocketAddr) -> SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut head = [0u8; 2];
                    client.read_exact(&mut head).await.ok()?;
                    let mut methods = vec![0u8; head[1] as usize];
                    client.read_exact(&mut methods).await.ok()?;
                    client.write_all(&[5, 0]).await.ok()?;
                    let mut request = [0u8; 4];
                    client.read_exact(&mut request).await.ok()?;
                    let skip = match request[3] {
                        1 => 4,
                        4 => 16,
                        _ => client.read_u8().await.ok()? as usize,
                    };
                    let mut rest = vec![0u8; skip + 2];
                    client.read_exact(&mut rest).await.ok()?;
                    client
                        .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                        .await
                        .ok()?;
                    let mut server = tokio::net::TcpStream::connect(target).await.ok()?;
                    tokio::io::copy_bidirectional(&mut client, &mut server)
                        .await
                        .ok()
                });
            }
        });
        address
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trusts_extra_roots_through_a_proxy() {
        let ca = holtmail_core::tls::testing::TestCa::new(&["images.example.com"]);
        let server = ca.serve_https(200, "{}").await;
        let proxy = format!("socks5h://{}", socks5_to(server).await);
        let fetcher = |roots: TlsRoots| {
            HttpFetcher::new(Options {
                proxy: Some(proxy.clone()),
                roots,
                ..Options::default()
            })
            .unwrap()
        };
        let url = "https://images.example.com/a.png";
        assert!(matches!(
            fetcher(ca.roots()).image(url).await,
            Err(FetchError::Refused(reason)) if reason == "not an image"
        ));
        assert!(matches!(
            fetcher(TlsRoots::default()).image(url).await,
            Err(FetchError::Unavailable(_))
        ));
        assert!(
            HttpFetcher::new(Options {
                roots: TlsRoots::from_der(vec![vec![1, 2, 3]]),
                ..Options::default()
            })
            .is_err()
        );
    }

    #[test]
    fn reads_proxy_addresses() {
        assert_eq!(
            proxy_address("socks5h://127.0.0.1:9050")
                .unwrap()
                .to_string(),
            "socks5h://127.0.0.1:9050"
        );
        assert_eq!(
            proxy_address("http://user:secret@proxy.example.com")
                .unwrap()
                .to_string(),
            "http://proxy.example.com:80",
            "credentials leaked into the address"
        );
        assert_eq!(proxy_address("socks5://[::1]").unwrap().port, 1080);
        for bad in [
            "ftp://proxy.example.com",
            "proxy.example.com:3128",
            "http://proxy.example.com/path",
        ] {
            assert!(proxy_address(bad).is_err(), "{bad}");
        }
        assert!(
            HttpFetcher::new(Options {
                proxy: Some("ftp://proxy.example.com".into()),
                ..Options::default()
            })
            .is_err()
        );
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
