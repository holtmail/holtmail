//! A [`WebhookSender`] on reqwest and rustls.

use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::TlsRoots;
use holtmail_webhook::{WebhookError, WebhookRequest, WebhookSender};
use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use url::Url;

/// Posts webhook requests. Follows no redirects, keeps no cookies, uses no proxy and gives up
/// after its timeout.
#[derive(Clone)]
pub struct HttpWebhookSender {
    client: reqwest::Client,
    timeout: Duration,
}

impl HttpWebhookSender {
    /// A sender that gives up after `timeout` per request.
    pub fn new(timeout: Duration) -> Result<Self, WebhookError> {
        Ok(Self {
            client: client(timeout, &TlsRoots::default())?,
            timeout,
        })
    }

    /// Also trusts `roots` for https, besides the built-in roots. `Invalid` when one of them is
    /// not a usable certificate.
    pub fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, WebhookError> {
        self.client = client(self.timeout, roots)?;
        Ok(self)
    }
}

fn client(timeout: Duration, roots: &TlsRoots) -> Result<reqwest::Client, WebhookError> {
    let certificates = roots
        .iter()
        .map(reqwest::Certificate::from_der)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| WebhookError::Invalid(format!("an extra trusted certificate: {e}")))?;
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
        .tls_certs_merge(certificates)
        .build()
        .map_err(|e| WebhookError::Invalid(e.to_string()))
}

#[async_trait]
impl WebhookSender for HttpWebhookSender {
    async fn send(&self, request: &WebhookRequest) -> Result<u16, WebhookError> {
        let url =
            Url::parse(&request.url).map_err(|_| WebhookError::Invalid("not a URL".into()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(WebhookError::Invalid(
                "only http and https addresses".into(),
            ));
        }
        let mut builder = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .body(request.body.clone());
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| WebhookError::Invalid(format!("header {name}")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| WebhookError::Invalid(format!("header {name}")))?;
            builder = builder.header(name, value);
        }
        match builder.send().await {
            Ok(response) => Ok(response.status().as_u16()),
            Err(error) if error.is_timeout() => Err(WebhookError::TimedOut),
            Err(error) => Err(WebhookError::Unreachable(error.without_url().to_string())),
        }
    }
}

#[cfg(test)]
mod tests;
