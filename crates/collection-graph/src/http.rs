use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use holtmail_collection::CollectionError;
use holtmail_core::TlsRoots;
use reqwest::header::{ACCEPT, HeaderMap, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode};
use url::{Host, Url};

const ATTEMPTS: u32 = 5;
const LONGEST_WAIT: Duration = Duration::from_secs(120);
const MAX_LISTING: usize = 64 * 1024 * 1024;
const MAX_ITEM: usize = 16 * 1024 * 1024;

pub(crate) fn allowed(url: &Url) -> bool {
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

/// Whether a refusal concerns the account as a whole or one item.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Account,
    Item,
}

/// A body, or the `404`/`410` the caller handles.
pub(crate) enum Answer {
    Found(Bytes),
    Missing,
    Gone,
}

#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
    token: String,
    origin: url::Origin,
    timeout: Duration,
}

fn host(url: &Url) -> String {
    url.host_str().unwrap_or("Microsoft Graph").to_owned()
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
        .map_err(|_| CollectionError::Connection(format!("{host} broke off the answer")))?
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
        token: String,
        base: &Url,
        timeout: Duration,
    ) -> Result<Self, CollectionError> {
        let client = client(timeout, &TlsRoots::default())?;
        Ok(Self {
            timeout,
            client,
            token,
            origin: base.origin(),
        })
    }

    /// The same client, also trusting `roots` besides the built-in roots.
    pub(crate) fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, CollectionError> {
        self.client = client(self.timeout, roots)?;
        Ok(self)
    }

    /// Parses an address and checks it is on the account's Graph host, so the token never goes
    /// anywhere else, even when an answer or a stored link names another host.
    pub(crate) fn url(&self, raw: &str) -> Result<Url, CollectionError> {
        let url = Url::parse(raw).map_err(|_| {
            CollectionError::Protocol("Microsoft Graph named an invalid address".into())
        })?;
        if !allowed(&url) || url.origin() != self.origin {
            return Err(CollectionError::Protocol(
                "Microsoft Graph named an address on another host".into(),
            ));
        }
        Ok(url)
    }

    fn request(&self, url: Url) -> RequestBuilder {
        self.client
            .get(url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/json")
            .header(
                "Prefer",
                "IdType=\"ImmutableId\", outlook.body-content-type=\"text\", odata.maxpagesize=200",
            )
    }

    async fn send(&self, url: Url) -> Result<(reqwest::Response, String), CollectionError> {
        if url.origin() != self.origin {
            return Err(CollectionError::Protocol(
                "Microsoft Graph named an address on another host".into(),
            ));
        }
        let host = host(&url);
        let mut attempt = 0;
        loop {
            let response = self.request(url.clone()).send().await.map_err(|e| {
                CollectionError::Connection(if e.is_timeout() {
                    format!("{host} did not answer in time")
                } else {
                    format!("{host} could not be reached")
                })
            })?;
            let status = response.status();
            if !matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            ) {
                return Ok((response, host));
            }
            attempt += 1;
            if attempt >= ATTEMPTS {
                return Err(CollectionError::Connection(format!(
                    "{host} is busy or limiting requests ({}); try again later",
                    status.as_u16()
                )));
            }
            let wait = wait_for(response.headers(), attempt);
            tracing::debug!(
                status = status.as_u16(),
                wait_secs = wait.as_secs(),
                "Microsoft Graph asked to wait"
            );
            tokio::time::sleep(wait).await;
        }
    }

    fn check(status: StatusCode, host: &str, scope: Scope) -> Result<(), CollectionError> {
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED => Err(CollectionError::Auth(format!(
                "{host} refused the access token"
            ))),
            StatusCode::FORBIDDEN if scope == Scope::Account => Err(CollectionError::Auth(
                format!("{host} refused access to contacts or calendars"),
            )),
            s if s.is_redirection() => Err(CollectionError::Protocol(format!(
                "{host} redirected, which is not followed"
            ))),
            s if s.is_server_error() => Err(CollectionError::Connection(format!(
                "{host} answered {}",
                s.as_u16()
            ))),
            s => Err(CollectionError::Protocol(format!(
                "{host} answered {}",
                s.as_u16()
            ))),
        }
    }

    /// The body of a GET; `Missing` for `404`, `Gone` for `410`.
    pub(crate) async fn get(
        &self,
        url: Url,
        scope: Scope,
        item: bool,
    ) -> Result<Answer, CollectionError> {
        let (response, host) = self.send(url).await?;
        match response.status() {
            StatusCode::NOT_FOUND => return Ok(Answer::Missing),
            StatusCode::GONE => return Ok(Answer::Gone),
            status => Self::check(status, &host, scope)?,
        }
        let limit = if item { MAX_ITEM } else { MAX_LISTING };
        read_limited(response, limit, &host)
            .await
            .map(Answer::Found)
    }
}
