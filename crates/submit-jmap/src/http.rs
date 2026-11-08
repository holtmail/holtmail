use std::net::IpAddr;
use std::time::Duration;

use holtmail_core::TlsRoots;
use holtmail_submit::SubmitError;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::{RequestBuilder, StatusCode};
use serde_json::Value;
use url::{Host, Url};

#[derive(Clone)]
pub(crate) enum Auth {
    Basic(String, String),
    Bearer(String),
}

/// Whether a failure can still mean the server acted on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Before,
    Handover,
}

pub(crate) struct Http {
    client: reqwest::Client,
    auth: Auth,
    timeout: Duration,
}

const MAX_REDIRECTS: usize = 5;
const MAX_JSON: usize = 16 * 1024 * 1024;

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

fn host(url: &Url) -> String {
    url.host_str().unwrap_or("the server").to_owned()
}

fn client(timeout: Duration, roots: &TlsRoots) -> Result<reqwest::Client, SubmitError> {
    let certificates = roots
        .iter()
        .map(|der| {
            reqwest::Certificate::from_der(der)
                .map_err(|e| SubmitError::Invalid(format!("an extra trusted certificate: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
        .tls_certs_merge(certificates)
        .build()
        .map_err(|e| SubmitError::Invalid(e.to_string()))
}

impl Http {
    pub(crate) fn new(auth: Auth, timeout: Duration) -> Result<Self, SubmitError> {
        Ok(Self {
            client: client(timeout, &TlsRoots::default())?,
            auth,
            timeout,
        })
    }

    pub(crate) fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, SubmitError> {
        self.client = client(self.timeout, roots)?;
        Ok(self)
    }

    fn request(
        &self,
        method: reqwest::Method,
        raw: &str,
    ) -> Result<(RequestBuilder, String), SubmitError> {
        let url = Url::parse(raw).map_err(|_| {
            SubmitError::Unavailable("the JMAP server named an invalid address".into())
        })?;
        if !allowed(&url) {
            return Err(SubmitError::Invalid(
                "the JMAP server named an address without https".into(),
            ));
        }
        let host = host(&url);
        let builder = self.client.request(method, url);
        let builder = match &self.auth {
            Auth::Basic(user, password) => builder.basic_auth(user, Some(password)),
            Auth::Bearer(token) => builder.bearer_auth(token),
        };
        Ok((builder, host))
    }

    /// Where a well-known address leads, followed through redirects without credentials.
    pub(crate) async fn resolve(&self, start: Url) -> Result<Url, SubmitError> {
        let mut url = start;
        for _ in 0..MAX_REDIRECTS {
            let host = host(&url);
            let response = send(
                self.client
                    .get(url.clone())
                    .header(ACCEPT, "application/json"),
                &host,
                Stage::Before,
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
                    SubmitError::Unavailable(format!("{host} redirected without an address"))
                })?;
            if !allowed(&next) || (url.scheme() == "https" && next.scheme() != "https") {
                return Err(SubmitError::Invalid(format!(
                    "{host} redirected away from https"
                )));
            }
            url = next;
        }
        Err(SubmitError::Invalid(
            "the JMAP address redirects too often".into(),
        ))
    }

    /// The session at `url`, asked with credentials; a redirect is not followed.
    pub(crate) async fn session(&self, url: &Url) -> Result<Value, SubmitError> {
        let (builder, host) = self.request(reqwest::Method::GET, url.as_str())?;
        let response = send(
            builder.header(ACCEPT, "application/json"),
            &host,
            Stage::Before,
        )
        .await?;
        if response.status().is_redirection() {
            return Err(SubmitError::Invalid(format!(
                "{host} redirected the session address, which is not followed with credentials"
            )));
        }
        if response.status() == StatusCode::NOT_FOUND {
            return Err(SubmitError::Invalid(format!(
                "{host} has no JMAP session at this address"
            )));
        }
        status(response.status(), &host, Stage::Before)?;
        json(response, &host, Stage::Before).await
    }

    /// A JMAP API request. At [`Stage::Handover`] every failure after the request may have reached
    /// the server is [`SubmitError::Uncertain`].
    pub(crate) async fn post_json(
        &self,
        url: &str,
        body: &Value,
        stage: Stage,
    ) -> Result<Value, SubmitError> {
        let (builder, host) = self.request(reqwest::Method::POST, url)?;
        let response = send(
            builder
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string()),
            &host,
            stage,
        )
        .await?;
        status(response.status(), &host, stage)?;
        json(response, &host, stage).await
    }

    /// Uploads `bytes` as a `message/rfc822` blob and returns the server's answer.
    pub(crate) async fn upload(
        &self,
        url: &str,
        bytes: &[u8],
        limit: Option<u64>,
    ) -> Result<Value, SubmitError> {
        let (builder, host) = self.request(reqwest::Method::POST, url)?;
        let response = send(
            builder
                .header(ACCEPT, "application/json")
                .header(CONTENT_TYPE, "message/rfc822")
                .body(bytes.to_vec()),
            &host,
            Stage::Before,
        )
        .await?;
        if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(SubmitError::TooLarge { limit });
        }
        status(response.status(), &host, Stage::Before)?;
        json(response, &host, Stage::Before).await
    }
}

async fn send(
    builder: RequestBuilder,
    host: &str,
    stage: Stage,
) -> Result<reqwest::Response, SubmitError> {
    builder.send().await.map_err(|e| {
        if e.is_connect() || e.is_builder() {
            SubmitError::Unavailable(format!("{host} could not be reached"))
        } else if stage == Stage::Handover {
            SubmitError::Uncertain(format!("the connection to {host} broke after sending"))
        } else if e.is_timeout() {
            SubmitError::Unavailable(format!("{host} did not answer in time"))
        } else {
            SubmitError::Unavailable(format!("the connection to {host} broke"))
        }
    })
}

fn status(status: StatusCode, host: &str, stage: Stage) -> Result<(), SubmitError> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(SubmitError::Auth(format!("{host} refused the credentials")))
        }
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => Err(
            SubmitError::Temporary(format!("{host} answered {}", status.as_u16())),
        ),
        s if s.is_server_error() && stage == Stage::Handover => Err(SubmitError::Uncertain(
            format!("{host} answered {}", s.as_u16()),
        )),
        s if s.is_server_error() => Err(SubmitError::Unavailable(format!(
            "{host} answered {}",
            s.as_u16()
        ))),
        s => Err(SubmitError::Invalid(format!(
            "{host} refused the request with {}",
            s.as_u16()
        ))),
    }
}

async fn json(
    mut response: reqwest::Response,
    host: &str,
    stage: Stage,
) -> Result<Value, SubmitError> {
    let broke = |what: &str| match stage {
        Stage::Handover => SubmitError::Uncertain(format!("{host} {what}")),
        Stage::Before => SubmitError::Unavailable(format!("{host} {what}")),
    };
    if response
        .content_length()
        .is_some_and(|n| n > MAX_JSON as u64)
    {
        return Err(broke("sent an answer that is too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| broke("broke off its answer"))?
    {
        if body.len() + chunk.len() > MAX_JSON {
            return Err(broke("sent an answer that is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| broke("did not answer with JMAP"))
}
