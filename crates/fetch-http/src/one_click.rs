use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::TlsRoots;
use holtmail_fetch::{FetchError, OneClick, OneClickOutcome, OneClickResponse};
use reqwest::dns::{Name, Resolve, Resolving};
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect;
use url::{Host, Url};

use crate::{describe, is_local_name, is_public};

const PORT: u16 = 443;
const BOUNDARY: &str = "holtmail-one-click-5f0c9e2a7b3d4816";

/// Limits of an [`HttpOneClick`].
#[derive(Debug, Clone)]
pub struct OneClickOptions {
    /// Time for the whole call, name resolution and response body included.
    pub timeout: Duration,
    /// Most response body bytes read; the rest is discarded unread.
    pub max_body: usize,
    /// Certificates trusted for https besides the built-in roots.
    pub roots: TlsRoots,
    /// Networks, as address and prefix length, whose addresses count as public for one-click
    /// requests only, never for image fetches. Only in builds with the `test-networks` feature,
    /// for test setups whose servers sit on private addresses. Empty by default.
    #[cfg(feature = "test-networks")]
    pub allowed_test_networks: Vec<(IpAddr, u8)>,
}

impl Default for OneClickOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_body: 64 * 1024,
            roots: TlsRoots::default(),
            #[cfg(feature = "test-networks")]
            allowed_test_networks: Vec::new(),
        }
    }
}

/// Reads a network such as `11.88.77.0/24` or `fd00:1::/64`; a bare address is the network of
/// that one address. The prefix length must fit the address family.
#[cfg(feature = "test-networks")]
pub fn parse_cidr(text: &str) -> Result<(IpAddr, u8), String> {
    let text = text.trim();
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (text, None),
    };
    let address: IpAddr = address
        .parse()
        .map_err(|_| format!("{text} is not an IP network"))?;
    let max = if address.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(prefix) => prefix
            .parse::<u8>()
            .ok()
            .filter(|p| *p <= max)
            .ok_or_else(|| format!("{text} has a bad prefix length"))?,
        None => max,
    };
    Ok((address, prefix))
}

#[cfg(feature = "test-networks")]
fn in_network(ip: IpAddr, (network, prefix): (IpAddr, u8)) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            u32::from(ip) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            u128::from(ip) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

/// Sends one-click unsubscribe requests over HTTPS, directly and to public addresses only.
///
/// It never uses a proxy: a proxy would resolve the name itself, and the addresses actually
/// contacted could not be checked. Every request gets its own client pinned to the addresses that
/// were checked, so the connection cannot go anywhere else while TLS still verifies the URL's
/// host name.
pub struct HttpOneClick {
    timeout: Duration,
    max_body: usize,
    certificates: Vec<reqwest::Certificate>,
    #[cfg(feature = "test-networks")]
    allowed_test_networks: Vec<(IpAddr, u8)>,
    #[cfg(test)]
    hooks: Option<TestHooks>,
}

#[cfg(test)]
struct TestHooks {
    answers: std::collections::HashMap<String, Vec<IpAddr>>,
    unanswered: Vec<String>,
    public: Vec<IpAddr>,
    port: u16,
}

impl HttpOneClick {
    /// A sender with these limits. Fails when an extra root is not a usable certificate.
    pub fn new(options: OneClickOptions) -> Result<Self, FetchError> {
        let certificates = options
            .roots
            .iter()
            .map(reqwest::Certificate::from_der)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| FetchError::Unavailable(format!("an extra trusted certificate: {e}")))?;
        let one_click = Self {
            timeout: options.timeout,
            max_body: options.max_body,
            certificates,
            #[cfg(feature = "test-networks")]
            allowed_test_networks: options.allowed_test_networks,
            #[cfg(test)]
            hooks: None,
        };
        one_click.client("example.com", &[])?;
        Ok(one_click)
    }

    #[cfg(test)]
    fn with_hooks(options: OneClickOptions, hooks: TestHooks) -> Self {
        let mut one_click = Self::new(options).expect("one-click");
        one_click.hooks = Some(hooks);
        one_click
    }

    fn port(&self) -> u16 {
        #[cfg(test)]
        if let Some(hooks) = &self.hooks {
            return hooks.port;
        }
        PORT
    }

    fn counts_as_public(&self, ip: IpAddr) -> bool {
        #[cfg(feature = "test-networks")]
        if self
            .allowed_test_networks
            .iter()
            .any(|network| in_network(ip, *network))
        {
            return true;
        }
        #[cfg(test)]
        if let Some(hooks) = &self.hooks
            && hooks.public.contains(&ip)
        {
            return true;
        }
        is_public(ip)
    }

    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, FetchError> {
        #[cfg(test)]
        if let Some(hooks) = &self.hooks {
            if hooks.unanswered.iter().any(|h| h == host) {
                return std::future::pending().await;
            }
            return hooks
                .answers
                .get(host)
                .cloned()
                .ok_or_else(|| FetchError::Unavailable("host not found".into()));
        }
        let found = tokio::net::lookup_host((host, PORT))
            .await
            .map_err(|_| FetchError::Unavailable("host not found".into()))?;
        Ok(found.map(|addr| addr.ip()).collect())
    }

    fn check(&self, url: &str) -> Result<Url, FetchError> {
        let refuse = |reason: &str| FetchError::Refused(reason.into());
        let url = Url::parse(url).map_err(|_| refuse("not a URL"))?;
        if url.scheme() != "https" {
            return Err(refuse("only https"));
        }
        if url.port_or_known_default() != Some(PORT) {
            return Err(refuse("only port 443"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(refuse("credentials in the URL"));
        }
        match url.host() {
            Some(Host::Domain(name)) if !name.is_empty() && !is_local_name(name) => {}
            Some(Host::Ipv4(ip)) if self.counts_as_public(IpAddr::V4(ip)) => {}
            Some(Host::Ipv6(ip)) if self.counts_as_public(IpAddr::V6(ip)) => {}
            None => return Err(refuse("no host")),
            Some(_) => return Err(refuse("not a public address")),
        }
        Ok(url)
    }

    fn client(&self, host: &str, addrs: &[SocketAddr]) -> Result<reqwest::Client, FetchError> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(redirect::Policy::none())
            .referer(false)
            .timeout(self.timeout)
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .tls_certs_merge(self.certificates.clone())
            .dns_resolver(Arc::new(NoResolver));
        if !addrs.is_empty() {
            builder = builder.resolve_to_addrs(host, addrs);
        }
        builder
            .build()
            .map_err(|e| FetchError::Unavailable(e.to_string()))
    }
}

#[async_trait]
impl OneClick for HttpOneClick {
    async fn post(&self, url: &str) -> Result<OneClickResponse, FetchError> {
        let url = self.check(url)?;
        tokio::time::timeout(self.timeout, self.send(url))
            .await
            .map_err(|_| FetchError::Unavailable("timed out".into()))?
    }
}

impl HttpOneClick {
    async fn send(&self, url: Url) -> Result<OneClickResponse, FetchError> {
        let mut pinned = Vec::new();
        if let Some(Host::Domain(host)) = url.host() {
            let mut found = self.lookup(host).await?;
            found.sort();
            found.dedup();
            if found.is_empty() || !found.iter().all(|ip| self.counts_as_public(*ip)) {
                return Err(FetchError::Refused("not a public address".into()));
            }
            pinned = found
                .into_iter()
                .map(|ip| SocketAddr::new(ip, self.port()))
                .collect();
        }
        let client = self.client(url.host_str().unwrap_or_default(), &pinned)?;
        let mut response = client
            .post(url)
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"List-Unsubscribe\"\r\n\r\nOne-Click\r\n--{BOUNDARY}--\r\n"
            ))
            .send()
            .await
            .map_err(|e| FetchError::Unavailable(describe(&e)))?;
        let status = response.status().as_u16();
        let mut read = 0;
        while read < self.max_body {
            match response.chunk().await {
                Ok(Some(chunk)) => read += chunk.len(),
                _ => break,
            }
        }
        Ok(OneClickResponse {
            outcome: OneClickOutcome::from_status(status),
            status,
        })
    }
}

struct NoResolver;

impl Resolve for NoResolver {
    fn resolve(&self, _: Name) -> Resolving {
        Box::pin(async { Err("names are resolved and checked before the request".into()) })
    }
}

#[cfg(test)]
mod tests {
    use holtmail_fetch::testing::{OneClickHarness, one_click_cases, one_click_refusals};

    use super::*;

    fn from_harness(harness: OneClickHarness) -> Box<dyn OneClick> {
        Box::new(HttpOneClick::with_hooks(
            OneClickOptions {
                timeout: harness.timeout,
                roots: harness.roots,
                ..OneClickOptions::default()
            },
            TestHooks {
                answers: harness.answers.into_iter().collect(),
                unanswered: harness.unanswered,
                public: harness.public,
                port: harness.port,
            },
        ))
    }

    #[tokio::test]
    async fn refuses_without_the_network() {
        let one_click = HttpOneClick::new(OneClickOptions::default()).unwrap();
        one_click_refusals(&one_click).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_one_click_contract() {
        one_click_cases(&from_harness).await;
    }

    #[test]
    fn refuses_unusable_roots() {
        assert!(
            HttpOneClick::new(OneClickOptions {
                roots: TlsRoots::from_der(vec![vec![1, 2, 3]]),
                ..OneClickOptions::default()
            })
            .is_err()
        );
    }

    #[cfg(feature = "test-networks")]
    #[tokio::test(flavor = "multi_thread")]
    async fn reaches_allowed_test_networks_only() {
        use holtmail_core::tls::testing::TestCa;

        let ca = TestCa::new(&["news.example.org"]);
        let server = ca.serve_https(204, "").await;
        let one_click = |networks: &[&str]| {
            HttpOneClick::with_hooks(
                OneClickOptions {
                    roots: ca.roots(),
                    timeout: Duration::from_secs(2),
                    allowed_test_networks: networks
                        .iter()
                        .map(|n| parse_cidr(n).unwrap())
                        .collect(),
                    ..OneClickOptions::default()
                },
                TestHooks {
                    answers: [
                        ("news.example.org".to_owned(), vec![server.ip()]),
                        (
                            "other.example.org".to_owned(),
                            vec!["10.20.30.40".parse().unwrap()],
                        ),
                    ]
                    .into(),
                    unanswered: Vec::new(),
                    public: Vec::new(),
                    port: server.port(),
                },
            )
        };
        let allowed = one_click(&["127.0.0.0/8"]);
        let response = allowed.post("https://news.example.org/u").await.unwrap();
        assert_eq!(response.outcome, OneClickOutcome::Accepted);
        assert_eq!(response.status, 204);
        assert!(matches!(
            allowed.post("https://other.example.org/u").await,
            Err(FetchError::Refused(_))
        ));
        assert!(matches!(
            one_click(&[]).post("https://news.example.org/u").await,
            Err(FetchError::Refused(_))
        ));
        assert!(matches!(
            one_click(&["127.0.0.2/32"])
                .post("https://news.example.org/u")
                .await,
            Err(FetchError::Refused(_))
        ));
        one_click_refusals(&HttpOneClick::new(OneClickOptions::default()).unwrap()).await;
    }

    #[cfg(feature = "test-networks")]
    #[test]
    fn reads_networks() {
        assert_eq!(
            parse_cidr("11.88.77.0/24").unwrap(),
            ("11.88.77.0".parse().unwrap(), 24)
        );
        assert_eq!(
            parse_cidr("fd00:1::5").unwrap(),
            ("fd00:1::5".parse().unwrap(), 128)
        );
        for bad in [
            "11.88.77.0/33",
            "fd00::/129",
            "example.com/24",
            "10.0.0.0/x",
            "",
        ] {
            assert!(parse_cidr(bad).is_err(), "{bad}");
        }
        let network = parse_cidr("11.88.77.0/24").unwrap();
        assert!(in_network("11.88.77.10".parse().unwrap(), network));
        assert!(!in_network("11.88.78.10".parse().unwrap(), network));
        assert!(!in_network("::ffff:11.88.77.10".parse().unwrap(), network));
        assert!(in_network(
            "10.1.2.3".parse().unwrap(),
            parse_cidr("0.0.0.0/0").unwrap()
        ));
    }
}
