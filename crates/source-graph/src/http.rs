use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use holtmail_source::SourceError;
use reqwest::header::{ACCEPT, HeaderMap, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode};
use serde_json::Value;
use url::{Host, Url};

const ATTEMPTS: u32 = 5;
const LONGEST_WAIT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
    token: String,
    origin: url::Origin,
}

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

/// What a refusal means where it happens: on the mailbox as a whole a `403` is a sign-in that
/// lacks access; on one folder or message it is that item's problem.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Mailbox,
    Item,
}

/// The answer to a request whose `404` or `410` the caller handles.
pub(crate) enum Answer<T> {
    Found(T),
    Missing,
}

const MAX_JSON: usize = 64 * 1024 * 1024;
const MAX_DOWNLOAD: usize = 256 * 1024 * 1024;

async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
    host: &str,
    broke: &str,
) -> Result<Bytes, SourceError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(SourceError::Protocol(format!(
            "{host} sent an answer that is too large"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SourceError::Connection(format!("{host} broke off {broke}")))?
    {
        if body.len() + chunk.len() > limit {
            return Err(SourceError::Protocol(format!(
                "{host} sent an answer that is too large"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
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

impl Http {
    pub(crate) fn new(token: String, base: &Url, timeout: Duration) -> Result<Self, SourceError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| SourceError::Config(e.to_string()))?;
        Ok(Self {
            client,
            token,
            origin: base.origin(),
        })
    }

    /// Parses an address and checks that it is on the Graph host the account uses, so the
    /// token never goes anywhere else, even when an answer or a stored link names another host.
    pub(crate) fn url(&self, raw: &str) -> Result<Url, SourceError> {
        let url = Url::parse(raw).map_err(|_| {
            SourceError::Protocol("Microsoft Graph named an invalid address".into())
        })?;
        if !allowed(&url) || url.origin() != self.origin {
            return Err(SourceError::Protocol(
                "Microsoft Graph named an address on another host".into(),
            ));
        }
        Ok(url)
    }

    fn request(&self, url: Url, json: bool) -> RequestBuilder {
        let builder = self.client.get(url).bearer_auth(&self.token);
        let builder = if json {
            builder.header(ACCEPT, "application/json")
        } else {
            builder
        };
        builder.header("Prefer", "IdType=\"ImmutableId\", odata.maxpagesize=500")
    }

    /// Sends a GET, waiting and trying again while Graph throttles (`429`) or is briefly
    /// unavailable (`503`, `504`), as long as `Retry-After` says or with a growing pause.
    async fn send(&self, url: Url, json: bool) -> Result<(reqwest::Response, String), SourceError> {
        let host = host(&url);
        let mut attempt = 0;
        loop {
            let response = self.request(url.clone(), json).send().await.map_err(|e| {
                SourceError::Connection(if e.is_timeout() {
                    format!("{host} did not answer in time")
                } else {
                    format!("{host} could not be reached")
                })
            })?;
            let status = response.status();
            let busy = matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            );
            if !busy {
                return Ok((response, host));
            }
            attempt += 1;
            if attempt >= ATTEMPTS {
                return Err(SourceError::Connection(format!(
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

    fn check(status: StatusCode, host: &str, scope: Scope) -> Result<(), SourceError> {
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED => Err(SourceError::Auth(format!(
                "{host} refused the access token"
            ))),
            StatusCode::FORBIDDEN if scope == Scope::Mailbox => Err(SourceError::Auth(format!(
                "{host} refused access to the mailbox"
            ))),
            StatusCode::FORBIDDEN => Err(SourceError::Protocol(format!(
                "{host} refused access to an item"
            ))),
            s if s.is_server_error() => Err(SourceError::Connection(format!(
                "{host} answered {}",
                s.as_u16()
            ))),
            s => Err(SourceError::Protocol(format!(
                "{host} answered {}",
                s.as_u16()
            ))),
        }
    }

    async fn get(
        &self,
        url: Url,
        scope: Scope,
        json: bool,
    ) -> Result<Answer<reqwest::Response>, SourceError> {
        let (response, host) = self.send(url, json).await?;
        if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Ok(Answer::Missing);
        }
        Self::check(response.status(), &host, scope)?;
        Ok(Answer::Found(response))
    }

    pub(crate) async fn get_json(
        &self,
        url: Url,
        scope: Scope,
    ) -> Result<Answer<Value>, SourceError> {
        let host = host(&url);
        let Answer::Found(response) = self.get(url, scope, true).await? else {
            return Ok(Answer::Missing);
        };
        let body = read_limited(response, MAX_JSON, &host, "the answer").await?;
        serde_json::from_slice(&body)
            .map(Answer::Found)
            .map_err(|_| SourceError::Protocol(format!("{host} did not answer with JSON")))
    }

    pub(crate) async fn get_bytes(&self, url: Url) -> Result<Answer<Bytes>, SourceError> {
        let host = host(&url);
        let Answer::Found(response) = self.get(url, Scope::Item, false).await? else {
            return Ok(Answer::Missing);
        };
        read_limited(response, MAX_DOWNLOAD, &host, "a download")
            .await
            .map(Answer::Found)
    }
}
