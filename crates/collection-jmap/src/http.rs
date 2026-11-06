use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use holtmail_collection::{CollectionError, Credentials};
use holtmail_core::TlsRoots;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, LOCATION, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode};
use url::{Host, Url};

const ATTEMPTS: u32 = 5;
const LONGEST_WAIT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 5;
pub(crate) const MAX_SESSION: usize = 1024 * 1024;
pub(crate) const MAX_ANSWER: usize = 64 * 1024 * 1024;

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

#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
    credentials: Credentials,
    timeout: Duration,
}

fn host(url: &Url) -> String {
    url.host_str().unwrap_or("the JMAP server").to_owned()
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

fn connection(host: &str, e: &reqwest::Error) -> CollectionError {
    CollectionError::Connection(if e.is_timeout() {
        format!("{host} did not answer in time")
    } else {
        format!("{host} could not be reached")
    })
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
        timeout: Duration,
    ) -> Result<Self, CollectionError> {
        let client = client(timeout, &TlsRoots::default())?;
        Ok(Self {
            timeout,
            client,
            credentials,
        })
    }

    /// The same client, also trusting `roots` besides the built-in roots.
    pub(crate) fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, CollectionError> {
        self.client = client(self.timeout, roots)?;
        Ok(self)
    }

    /// Where a well-known address leads, followed through redirects without credentials.
    pub(crate) async fn resolve(&self, start: Url) -> Result<Url, CollectionError> {
        let mut url = start;
        for _ in 0..MAX_REDIRECTS {
            let h = host(&url);
            let response = self
                .client
                .get(url.clone())
                .header(ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| connection(&h, &e))?;
            if !response.status().is_redirection() {
                return Ok(url);
            }
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .ok_or_else(|| {
                    CollectionError::Protocol(format!("{h} redirected without an address"))
                })?;
            if !allowed(&next) || (url.scheme() == "https" && next.scheme() != "https") {
                return Err(CollectionError::Protocol(format!(
                    "{h} redirected away from https"
                )));
            }
            url = next;
        }
        Err(CollectionError::Protocol(
            "the JMAP address redirects too often".into(),
        ))
    }

    fn authorized(&self, builder: RequestBuilder) -> RequestBuilder {
        match &self.credentials {
            Credentials::Password { username, password } => {
                builder.basic_auth(username, Some(password))
            }
            Credentials::Bearer(token) => builder.bearer_auth(token),
        }
    }

    async fn send(
        &self,
        url: &Url,
        body: Option<&[u8]>,
    ) -> Result<reqwest::Response, CollectionError> {
        let h = host(url);
        let mut attempt = 0;
        loop {
            let builder = match body {
                Some(body) => self
                    .client
                    .post(url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.to_vec()),
                None => self.client.get(url.clone()),
            };
            let response = self
                .authorized(builder.header(ACCEPT, "application/json"))
                .send()
                .await
                .map_err(|e| connection(&h, &e))?;
            let status = response.status();
            if !matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            ) {
                return Ok(response);
            }
            attempt += 1;
            if attempt >= ATTEMPTS {
                return Err(CollectionError::Connection(format!(
                    "{h} is busy or limiting requests ({}); try again later",
                    status.as_u16()
                )));
            }
            let wait = wait_for(response.headers(), attempt);
            tracing::debug!(
                status = status.as_u16(),
                wait_secs = wait.as_secs(),
                "the JMAP server asked to wait"
            );
            tokio::time::sleep(wait).await;
        }
    }

    /// A GET (`body` `None`) or POST with the credentials, to an address the caller checked.
    pub(crate) async fn request(
        &self,
        url: &Url,
        body: Option<&[u8]>,
        limit: usize,
    ) -> Result<Bytes, CollectionError> {
        let h = host(url);
        let response = self.send(url, body).await?;
        match response.status() {
            s if s.is_success() => read_limited(response, limit, &h).await,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(CollectionError::Auth(
                format!("{h} refused the credentials"),
            )),
            StatusCode::NOT_FOUND => Err(CollectionError::Config(format!(
                "{h} has no JMAP session at this address"
            ))),
            s if s.is_redirection() => Err(CollectionError::Protocol(format!(
                "{h} redirected, which is not followed with credentials"
            ))),
            s if s.is_server_error() => Err(CollectionError::Connection(format!(
                "{h} answered {}",
                s.as_u16()
            ))),
            s => Err(CollectionError::Protocol(format!(
                "{h} answered {}",
                s.as_u16()
            ))),
        }
    }
}
