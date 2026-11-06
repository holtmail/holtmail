use std::net::IpAddr;
use std::time::Duration;

use bytes::Bytes;
use holtmail_core::TlsRoots;
use holtmail_source::SourceError;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::{RequestBuilder, StatusCode};
use serde_json::Value;
use url::{Host, Url};

#[derive(Clone)]
pub(crate) enum Auth {
    Basic(String, String),
    Bearer(String),
}

#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
    still: reqwest::Client,
    auth: Auth,
    timeout: Duration,
}

const MAX_REDIRECTS: usize = 5;

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
    url.host_str().unwrap_or("the server").to_owned()
}

fn certificates(roots: &TlsRoots) -> Result<Vec<reqwest::Certificate>, SourceError> {
    roots
        .iter()
        .map(|der| {
            reqwest::Certificate::from_der(der)
                .map_err(|e| SourceError::Config(format!("an extra trusted certificate: {e}")))
        })
        .collect()
}

fn client(
    timeout: Duration,
    roots: &TlsRoots,
    follow: bool,
) -> Result<reqwest::Client, SourceError> {
    let policy = if follow {
        reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS || !allowed(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(timeout)
        .redirect(policy)
        .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
        .tls_certs_merge(certificates(roots)?)
        .build()
        .map_err(|e| SourceError::Config(e.to_string()))
}

impl Http {
    pub(crate) fn new(auth: Auth, timeout: Duration) -> Result<Self, SourceError> {
        let roots = TlsRoots::default();
        Ok(Self {
            timeout,
            client: client(timeout, &roots, true)?,
            still: client(timeout, &roots, false)?,
            auth,
        })
    }

    /// The same client, also trusting `roots` besides the built-in roots.
    pub(crate) fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, SourceError> {
        self.client = client(self.timeout, roots, true)?;
        self.still = client(self.timeout, roots, false)?;
        Ok(self)
    }

    /// Where a well-known address leads, followed through redirects without credentials.
    pub(crate) async fn resolve(&self, start: Url) -> Result<Url, SourceError> {
        let mut url = start;
        for _ in 0..MAX_REDIRECTS {
            let host = host(&url);
            let response = Self::send(
                self.still
                    .get(url.clone())
                    .header(ACCEPT, "application/json"),
                &host,
            )
            .await?;
            if !response.status().is_redirection() {
                return Ok(url);
            }
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .ok_or_else(|| {
                    SourceError::Protocol(format!("{host} redirected without an address"))
                })?;
            if !allowed(&next) || (url.scheme() == "https" && next.scheme() != "https") {
                return Err(SourceError::Protocol(format!(
                    "{host} redirected away from https"
                )));
            }
            url = next;
        }
        Err(SourceError::Protocol(
            "the JMAP address redirects too often".into(),
        ))
    }

    /// The session at `url`, asked with credentials; a redirect is not followed.
    pub(crate) async fn session(&self, url: &Url) -> Result<Value, SourceError> {
        let (builder, host) = self.request_with(&self.still, reqwest::Method::GET, url.as_str())?;
        let response = Self::send(builder.header(ACCEPT, "application/json"), &host).await?;
        if response.status().is_redirection() {
            return Err(SourceError::Protocol(format!(
                "{host} redirected the session address, which is not followed with credentials"
            )));
        }
        Self::check(response.status(), &host)?;
        Self::json(response, &host).await
    }

    fn request(
        &self,
        method: reqwest::Method,
        raw: &str,
    ) -> Result<(RequestBuilder, String), SourceError> {
        self.request_with(&self.client, method, raw)
    }

    fn request_with(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        raw: &str,
    ) -> Result<(RequestBuilder, String), SourceError> {
        let url = Url::parse(raw).map_err(|_| {
            SourceError::Protocol("the JMAP server named an invalid address".into())
        })?;
        if !allowed(&url) {
            return Err(SourceError::Protocol(
                "the JMAP server named an address without https".into(),
            ));
        }
        let host = host(&url);
        let builder = client.request(method, url);
        let builder = match &self.auth {
            Auth::Basic(user, password) => builder.basic_auth(user, Some(password)),
            Auth::Bearer(token) => builder.bearer_auth(token),
        };
        Ok((builder, host))
    }

    async fn send(builder: RequestBuilder, host: &str) -> Result<reqwest::Response, SourceError> {
        builder.send().await.map_err(|e| {
            SourceError::Connection(if e.is_timeout() {
                format!("{host} did not answer in time")
            } else if e.is_redirect() {
                format!("{host} redirected too often or away from https")
            } else {
                format!("{host} could not be reached")
            })
        })
    }

    fn check(status: StatusCode, host: &str) -> Result<(), SourceError> {
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(SourceError::Auth(format!("{host} refused the credentials")))
            }
            StatusCode::NOT_FOUND => Err(SourceError::Config(format!(
                "{host} has no JMAP session at this address"
            ))),
            s if s == StatusCode::TOO_MANY_REQUESTS || s.is_server_error() => Err(
                SourceError::Connection(format!("{host} answered {}", s.as_u16())),
            ),
            s => Err(SourceError::Protocol(format!(
                "{host} answered {}",
                s.as_u16()
            ))),
        }
    }

    async fn json(response: reqwest::Response, host: &str) -> Result<Value, SourceError> {
        let body = read_limited(response, MAX_JSON, host, "the answer").await?;
        serde_json::from_slice(&body)
            .map_err(|_| SourceError::Protocol(format!("{host} did not answer with JMAP")))
    }

    pub(crate) async fn post_json(&self, url: &str, body: &Value) -> Result<Value, SourceError> {
        let (builder, host) = self.request(reqwest::Method::POST, url)?;
        let response = Self::send(
            builder
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string()),
            &host,
        )
        .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(SourceError::Protocol(format!(
                "{host} has no JMAP API at the address its session names"
            )));
        }
        Self::check(response.status(), &host)?;
        Self::json(response, &host).await
    }

    /// An open `text/event-stream` answer from `url`, read as it arrives; the client's overall
    /// timeout does not apply, so the caller watches for silence itself.
    pub(crate) async fn stream(&self, url: &str) -> Result<reqwest::Response, SourceError> {
        let (builder, host) = self.request(reqwest::Method::GET, url)?;
        let response = Self::send(
            builder
                .header(ACCEPT, "text/event-stream")
                .timeout(Duration::from_secs(30 * 24 * 60 * 60)),
            &host,
        )
        .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(SourceError::Protocol(format!(
                "{host} has no event source at the address its session names"
            )));
        }
        Self::check(response.status(), &host)?;
        Ok(response)
    }

    /// The body, or `None` when the server no longer has it.
    pub(crate) async fn get_bytes(&self, url: &str) -> Result<Option<Bytes>, SourceError> {
        let (builder, host) = self.request(reqwest::Method::GET, url)?;
        let response = Self::send(builder, &host).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Self::check(response.status(), &host)?;
        read_limited(response, MAX_DOWNLOAD, &host, "a download")
            .await
            .map(Some)
    }
}
