use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use holtmail_collection::{CollectionError, Credentials};
use holtmail_core::TlsRoots;
use reqwest::header::{CONTENT_TYPE, HeaderMap, LOCATION, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use url::{Host, Url};

const ATTEMPTS: u32 = 5;
const LONGEST_WAIT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;
pub(crate) const MAX_XML: usize = 64 * 1024 * 1024;
pub(crate) const MAX_ITEM: usize = 16 * 1024 * 1024;

/// Provider domains whose servers hand out addresses on other hosts of the same domain, such as
/// iCloud's `pNN-contacts.icloud.com` home sets.
const FAMILIES: &[&str] = &["icloud.com"];

pub(crate) fn allowed_scheme(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        _ => false,
    }
}

fn family(host: &str) -> Option<&'static str> {
    let host = host.to_ascii_lowercase();
    FAMILIES
        .iter()
        .copied()
        .find(|f| host == *f || host.ends_with(&format!(".{f}")))
}

fn host(url: &Url) -> String {
    url.host_str().unwrap_or("the server").to_owned()
}

fn wait_for(headers: &HeaderMap, attempt: u32) -> Duration {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or_else(
            || Duration::from_secs(1 << attempt.min(6)),
            Duration::from_secs,
        )
        .min(LONGEST_WAIT)
}

pub(crate) struct Answer {
    pub status: StatusCode,
    pub url: Url,
    pub body: Bytes,
}

pub(crate) struct Http {
    client: reqwest::Client,
    credentials: Credentials,
    origins: Mutex<Vec<url::Origin>>,
    timeout: Duration,
}

async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
    host: &str,
) -> Result<Bytes, CollectionError> {
    let too_large =
        || CollectionError::Protocol(format!("{host} sent an answer that is too large"));
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| CollectionError::Connection(format!("{host} broke off its answer")))?
    {
        if body.len() + chunk.len() > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

fn certificates(roots: &TlsRoots) -> Result<Vec<reqwest::Certificate>, CollectionError> {
    roots
        .iter()
        .map(|der| {
            reqwest::Certificate::from_der(der)
                .map_err(|e| CollectionError::Config(format!("an extra trusted certificate: {e}")))
        })
        .collect()
}

fn client(timeout: Duration, roots: &TlsRoots) -> Result<reqwest::Client, CollectionError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
        .tls_certs_merge(certificates(roots)?)
        .build()
        .map_err(|e| CollectionError::Config(e.to_string()))
}

impl Http {
    pub(crate) fn new(
        credentials: Credentials,
        origins: Vec<url::Origin>,
        timeout: Duration,
    ) -> Result<Self, CollectionError> {
        let client = client(timeout, &TlsRoots::default())?;
        Ok(Self {
            timeout,
            client,
            credentials,
            origins: Mutex::new(origins),
        })
    }

    /// The same client, also trusting `roots` besides the built-in roots.
    pub(crate) fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, CollectionError> {
        self.client = client(self.timeout, roots)?;
        Ok(self)
    }

    /// Whether credentials may go to `url`: https (or http on loopback), to a server the owner
    /// configured or a configured address's well-known lookup led to, or to a host of the same
    /// provider family.
    pub(crate) fn may_sign_in(&self, url: &Url) -> bool {
        if !allowed_scheme(url) {
            return false;
        }
        let origins = self.origins.lock().expect("origins lock");
        if origins.contains(&url.origin()) {
            return true;
        }
        let Some(target) = url.host_str().and_then(family) else {
            return false;
        };
        url.scheme() == "https"
            && origins.iter().any(|o| match o {
                url::Origin::Tuple(scheme, host, _) => {
                    scheme == "https" && family(&host.to_string()) == Some(target)
                }
                url::Origin::Opaque(_) => false,
            })
    }

    fn trust(&self, url: &Url) {
        let mut origins = self.origins.lock().expect("origins lock");
        if !origins.contains(&url.origin()) {
            origins.push(url.origin());
        }
    }

    async fn send_once(
        &self,
        method: &Method,
        url: &Url,
        depth: Option<&str>,
        body: Option<&str>,
        sign_in: bool,
    ) -> Result<reqwest::Response, CollectionError> {
        let host = host(url);
        let mut attempt = 0;
        loop {
            let mut builder = self.client.request(method.clone(), url.clone());
            if let Some(depth) = depth {
                builder = builder.header("Depth", depth);
            }
            if let Some(body) = body {
                builder = builder
                    .header(CONTENT_TYPE, "application/xml; charset=utf-8")
                    .body(body.to_owned());
            }
            if sign_in {
                builder = match &self.credentials {
                    Credentials::Password { username, password } => {
                        builder.basic_auth(username, Some(password))
                    }
                    Credentials::Bearer(token) => builder.bearer_auth(token),
                };
            }
            let response = builder.send().await.map_err(|e| {
                CollectionError::Connection(if e.is_timeout() {
                    format!("{host} did not answer in time")
                } else {
                    format!("{host} could not be reached")
                })
            })?;
            let busy = matches!(
                response.status(),
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            );
            if !busy {
                return Ok(response);
            }
            attempt += 1;
            if attempt >= ATTEMPTS {
                return Err(CollectionError::Connection(format!(
                    "{host} is busy or limiting requests ({}); try again later",
                    response.status().as_u16()
                )));
            }
            let wait = wait_for(response.headers(), attempt);
            tracing::debug!(
                attempt,
                wait_secs = wait.as_secs(),
                "DAV server busy, waiting"
            );
            tokio::time::sleep(wait).await;
        }
    }

    fn redirect_target(response: &reqwest::Response, from: &Url) -> Option<Url> {
        if !matches!(response.status().as_u16(), 301 | 302 | 307 | 308) {
            return None;
        }
        let location = response.headers().get(LOCATION)?.to_str().ok()?;
        from.join(location).ok()
    }

    /// A request with credentials. Redirects are followed, with the same method and body, only
    /// to addresses [`Http::may_sign_in`] allows.
    pub(crate) async fn dav(
        &self,
        method: &str,
        url: &Url,
        depth: Option<&str>,
        body: Option<&str>,
        limit: usize,
    ) -> Result<Answer, CollectionError> {
        let method = Method::from_bytes(method.as_bytes()).expect("a valid method");
        let mut url = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            if !self.may_sign_in(&url) {
                return Err(CollectionError::Protocol(format!(
                    "the server pointed to {}, where the sign-in is not sent; enter that address \
                     as the contacts and calendars address to use it",
                    host(&url)
                )));
            }
            let response = self.send_once(&method, &url, depth, body, true).await?;
            if let Some(next) = Self::redirect_target(&response, &url) {
                url = next;
                continue;
            }
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                return Err(CollectionError::Auth(format!(
                    "{} refused the credentials",
                    host(&url)
                )));
            }
            let body = read_limited(response, limit, &host(&url)).await?;
            return Ok(Answer { status, url, body });
        }
        Err(CollectionError::Protocol(format!(
            "{} redirected too often",
            host(&url)
        )))
    }

    /// Looks up a well-known address without credentials, following redirects to any https
    /// address (never from https to http), and trusts where it leads: the owner's domain names its own DAV server there.
    /// `None` when the lookup leads nowhere useful.
    pub(crate) async fn well_known(&self, url: &Url) -> Result<Option<Url>, CollectionError> {
        let method = Method::from_bytes(b"PROPFIND").expect("a valid method");
        let mut url = url.clone();
        let mut moved = false;
        let secure = url.scheme() == "https";
        for _ in 0..=MAX_REDIRECTS {
            if !allowed_scheme(&url) || (secure && url.scheme() != "https") {
                return Ok(None);
            }
            let response = match self.send_once(&method, &url, Some("0"), None, false).await {
                Ok(response) => response,
                Err(CollectionError::Connection(_)) if moved => return Ok(None),
                Err(error) => return Err(error),
            };
            if let Some(next) = Self::redirect_target(&response, &url) {
                url = next;
                moved = true;
                continue;
            }
            let status = response.status();
            if moved
                || status.is_success()
                || status == StatusCode::UNAUTHORIZED
                || status.as_u16() == 207
            {
                self.trust(&url);
                return Ok(Some(url));
            }
            return Ok(None);
        }
        Ok(None)
    }

    /// A GET with credentials, for servers without multiget. `None` when the item is gone.
    pub(crate) async fn get(&self, url: &Url) -> Result<Option<Answer>, CollectionError> {
        let answer = self.dav("GET", url, None, None, MAX_ITEM).await?;
        match answer.status {
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(None),
            s if s.is_success() => Ok(Some(answer)),
            s => Err(status_error(s, &answer.url)),
        }
    }
}

/// The error for an answer that is neither success nor something the caller handles.
pub(crate) fn status_error(status: StatusCode, url: &Url) -> CollectionError {
    let host = host(url);
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            CollectionError::Auth(format!("{host} refused the credentials"))
        }
        StatusCode::NOT_FOUND | StatusCode::GONE => CollectionError::Gone,
        s if s.is_server_error() => {
            CollectionError::Connection(format!("{host} answered {}", s.as_u16()))
        }
        s => CollectionError::Protocol(format!("{host} answered {}", s.as_u16())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(origins: &[&str]) -> Http {
        Http::new(
            Credentials::Bearer("t".into()),
            origins
                .iter()
                .map(|o| Url::parse(o).unwrap().origin())
                .collect(),
            Duration::from_secs(5),
        )
        .unwrap()
    }

    #[test]
    fn credentials_stay_with_the_configured_servers() {
        let http = http(&["https://dav.example.com/", "https://contacts.icloud.com/"]);
        let ok = |u: &str| http.may_sign_in(&Url::parse(u).unwrap());
        assert!(ok("https://dav.example.com/remote.php/dav/"));
        assert!(!ok("https://dav.example.com:8443/"), "another port");
        assert!(!ok("http://dav.example.com/"), "plain http");
        assert!(!ok("https://evil.example.com/"), "a sibling host");
        assert!(!ok("https://example.com/"), "the parent domain");
        assert!(
            ok("https://p42-contacts.icloud.com/123/carddav/"),
            "iCloud partitions"
        );
        assert!(!ok("https://icloud.com.example.org/"));
        assert!(!ok("http://p42-contacts.icloud.com/"));
        let local = super::tests::http(&["http://127.0.0.1:8080/"]);
        assert!(local.may_sign_in(&Url::parse("http://127.0.0.1:8080/dav/").unwrap()));
        assert!(!local.may_sign_in(&Url::parse("http://127.0.0.1:9090/dav/").unwrap()));
    }

    #[test]
    fn waits_are_bounded() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "86400".parse().unwrap());
        assert_eq!(wait_for(&headers, 1), LONGEST_WAIT);
        headers.insert(RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(wait_for(&headers, 1), Duration::from_secs(3));
        assert_eq!(wait_for(&HeaderMap::new(), 2), Duration::from_secs(4));
    }
}
