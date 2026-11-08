//! Hands prebuilt messages to Microsoft Graph `sendMail`.
//!
//! The message goes to `POST /me/sendMail` (or `/users/{id}/sendMail`) as base64 MIME. Graph
//! keeps the sent copy in Sent Items and takes the recipients from the To, Cc and Bcc fields,
//! so envelope recipients the fields do not name are added in one `Bcc` field to the bytes
//! handed to Graph, and only there.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use holtmail_core::TlsRoots;
use holtmail_submit::{
    Envelope, Limits, RecipientOutcome, RecipientResult, SubmitError, SubmitReport, Submitter,
};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use url::{Host, Url};

mod bcc;
#[cfg(test)]
mod tests;

/// The Graph endpoint used when [`GraphConfig::api_url`] is not changed.
pub const GRAPH_URL: &str = "https://graph.microsoft.com/v1.0/";

/// The largest message `sendMail` takes: Graph refuses requests over 4 MiB, and base64 makes
/// 3 MiB of MIME into 4 MiB.
pub const MAX_SIZE: u64 = 3 * 1024 * 1024;

const MAX_ERROR_BODY: usize = 64 * 1024;

/// Supplies the bearer token for Graph, so refreshing it stays with the caller.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// A current access token with `Mail.Send` for the mailbox. Called once per
    /// [`Submitter::check`] and [`Submitter::submit`], before anything is sent. Its error is
    /// returned unchanged: `Auth` for a revoked sign-in, `Unavailable` for a token endpoint that
    /// is down.
    async fn token(&self) -> Result<String, SubmitError>;
}

/// A token that never changes, for tests and short-lived submitters.
#[derive(Clone)]
pub struct StaticToken(String);

impl StaticToken {
    /// Always hands out `token`.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticToken(..)")
    }
}

#[async_trait]
impl TokenProvider for StaticToken {
    async fn token(&self) -> Result<String, SubmitError> {
        Ok(self.0.clone())
    }
}

/// Where and how to reach Graph.
#[derive(Debug, Clone)]
pub struct GraphConfig {
    /// The Graph endpoint with its version. Only `https`, or plain `http` to a loopback
    /// address.
    pub api_url: String,
    /// The mailbox to send from: `None` for the token's own (`/me`), else a user id or user
    /// principal name (`/users/{id}`).
    pub mailbox: Option<String>,
    /// The largest message to hand over, in bytes, counting an added `Bcc` field; capped at
    /// [`MAX_SIZE`].
    pub max_size: u64,
    /// Connecting, sending the request and reading the answer.
    pub timeout: Duration,
    /// Certificates trusted besides the built-in web PKI roots.
    pub roots: TlsRoots,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            api_url: GRAPH_URL.into(),
            mailbox: None,
            max_size: MAX_SIZE,
            timeout: Duration::from_secs(120),
            roots: TlsRoots::default(),
        }
    }
}

/// A [`Submitter`] over Microsoft Graph `sendMail`.
pub struct GraphSubmitter {
    client: reqwest::Client,
    send_url: Url,
    check_url: Url,
    max_size: u64,
    tokens: Arc<dyn TokenProvider>,
}

impl fmt::Debug for GraphSubmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphSubmitter")
            .field("send_url", &self.send_url.as_str())
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

fn allowed(url: &Url) -> bool {
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

fn invalid(text: &str) -> SubmitError {
    SubmitError::Invalid(text.into())
}

impl GraphSubmitter {
    /// A submitter for `config` that asks `tokens` for a bearer token on every call. `Invalid`
    /// for an address that is not `https` (or `http` to loopback), an empty or malformed
    /// mailbox, a zero size limit or an extra root that is not a certificate.
    pub fn new(config: GraphConfig, tokens: Arc<dyn TokenProvider>) -> Result<Self, SubmitError> {
        let mut base = Url::parse(config.api_url.trim())
            .map_err(|_| invalid("the Graph address is not a web address"))?;
        if !allowed(&base) || base.cannot_be_a_base() {
            return Err(invalid("the Graph address must use https"));
        }
        base.set_query(None);
        base.set_fragment(None);
        let mailbox: Vec<&str> = match config.mailbox.as_deref().map(str::trim) {
            None => vec!["me"],
            Some(id)
                if !id.is_empty() && !id.chars().any(|c| c.is_whitespace() || c.is_control()) =>
            {
                vec!["users", id]
            }
            Some(_) => return Err(invalid("the mailbox is not a user id")),
        };
        if config.max_size == 0 {
            return Err(invalid("the size limit is zero"));
        }
        let endpoint = |tail: &[&str]| {
            let mut url = base.clone();
            url.path_segments_mut()
                .map_err(|_| invalid("the Graph address must use https"))?
                .pop_if_empty()
                .extend(mailbox.iter().chain(tail));
            Ok::<_, SubmitError>(url)
        };
        let send_url = endpoint(&["sendMail"])?;
        let mut check_url = endpoint(&["mailFolders", "sentitems"])?;
        check_url.set_query(Some("$select=id"));
        let certificates = config
            .roots
            .iter()
            .map(reqwest::Certificate::from_der)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid("an extra trusted certificate cannot be read"))?;
        let client = reqwest::Client::builder()
            .connect_timeout(config.timeout.min(Duration::from_secs(30)))
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .tls_certs_merge(certificates)
            .build()
            .map_err(|_| invalid("the HTTP client cannot be built"))?;
        Ok(Self {
            client,
            send_url,
            check_url,
            max_size: config.max_size.min(MAX_SIZE),
            tokens,
        })
    }
}

#[async_trait]
impl Submitter for GraphSubmitter {
    async fn check(&self) -> Result<Limits, SubmitError> {
        let token = self.tokens.token().await?;
        let response = self
            .client
            .get(self.check_url.clone())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| SubmitError::Unavailable("Microsoft Graph could not be reached".into()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(self.limits());
        }
        let (code, _) = graph_error(response).await;
        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => SubmitError::Auth(format!(
                "Microsoft Graph refused the sign-in for this mailbox ({status}, {code})"
            )),
            StatusCode::NOT_FOUND => invalid("Microsoft Graph has no such mailbox"),
            s if s == StatusCode::TOO_MANY_REQUESTS || s.is_server_error() => {
                SubmitError::Unavailable(format!("Microsoft Graph answered {status}"))
            }
            _ => SubmitError::Invalid(format!("Microsoft Graph answered {status} ({code})")),
        })
    }

    async fn submit(
        &self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError> {
        envelope.validate(message)?;
        let outgoing = bcc::for_graph(envelope, message)?;
        if outgoing.len() as u64 > self.max_size {
            return Err(SubmitError::TooLarge {
                limit: Some(self.max_size),
            });
        }
        let token = self.tokens.token().await?;
        let body = BASE64.encode(&outgoing);
        let response = match self
            .client
            .post(self.send_url.clone())
            .bearer_auth(token)
            .header(CONTENT_TYPE, "text/plain")
            .body(body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) if e.is_connect() => {
                return Err(SubmitError::Unavailable(
                    "Microsoft Graph could not be reached".into(),
                ));
            }
            Err(_) => {
                return Err(SubmitError::Uncertain(
                    "the connection to Microsoft Graph broke after the message was sent".into(),
                ));
            }
        };
        let status = response.status();
        if status.is_success() {
            return Ok(SubmitReport {
                recipients: envelope
                    .recipients
                    .iter()
                    .map(|address| RecipientResult {
                        address: address.clone(),
                        outcome: RecipientOutcome::Accepted,
                    })
                    .collect(),
            });
        }
        let retry_after = retry_after(response.headers());
        let (code, text) = graph_error(response).await;
        Err(refusal(status, retry_after, &code, &text))
    }

    fn limits(&self) -> Limits {
        Limits {
            max_size: Some(self.max_size),
            smtputf8: false,
        }
    }
}

fn retry_after(headers: &HeaderMap) -> Option<u64> {
    headers.get(RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()
}

async fn graph_error(mut response: reqwest::Response) -> (String, String) {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len() + chunk.len() > MAX_ERROR_BODY {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let field = |name: &str| {
        value
            .pointer(&format!("/error/{name}"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (field("code"), field("message"))
}

fn refusal(status: StatusCode, retry_after: Option<u64>, code: &str, text: &str) -> SubmitError {
    let said = if code.is_empty() {
        format!("{status}")
    } else if text.is_empty() {
        code.to_owned()
    } else {
        format!("{code}: {text}")
    };
    let permanent = |smtp: u16| SubmitError::Refused {
        permanent: true,
        code: smtp,
        text: said.clone(),
    };
    if status == StatusCode::PAYLOAD_TOO_LARGE || code == "ErrorMessageSizeExceeded" {
        return SubmitError::TooLarge { limit: None };
    }
    match status {
        StatusCode::UNAUTHORIZED => {
            SubmitError::Auth(format!("Microsoft Graph refused the access token ({said})"))
        }
        StatusCode::FORBIDDEN => match code {
            "ErrorSendAsDenied" => permanent(550),
            "ErrorMessageSubmissionBlocked" => permanent(554),
            _ => SubmitError::Auth(format!(
                "the sign-in may not send from this mailbox ({said})"
            )),
        },
        StatusCode::NOT_FOUND => invalid("Microsoft Graph has no such mailbox"),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
            SubmitError::Temporary(match retry_after {
                Some(seconds) => {
                    format!("Microsoft Graph answered {status}; retry after {seconds} s")
                }
                None => format!("Microsoft Graph answered {status}"),
            })
        }
        s if s.is_server_error() => SubmitError::Uncertain(format!(
            "Microsoft Graph answered {status} after it had the message"
        )),
        _ if code == "ErrorInvalidRecipients" => permanent(550),
        _ => permanent(554),
    }
}
