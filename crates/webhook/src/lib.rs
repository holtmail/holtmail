//! Contract for delivering webhook requests: one `POST` to an address the instance's owner
//! configured, with a body and headers made by the caller.
//!
//! What to send, how it is signed and when to try again belong to the caller; a sender only
//! carries one request and reports the answer.

use async_trait::async_trait;
use bytes::Bytes;

/// One request to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRequest {
    /// An `http` or `https` address.
    pub url: String,
    /// Extra headers, sent as given; `Content-Type: application/json` is always sent.
    pub headers: Vec<(String, String)>,
    /// The body, sent byte for byte.
    pub body: Bytes,
}

/// Why a request was not answered.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The address cannot be used: not `http` or `https`, or not a valid URL. Sending again gives
    /// the same answer.
    #[error("invalid address: {0}")]
    Invalid(String),
    /// The server could not be reached, refused the connection or TLS failed. It may work later.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// No answer within the sender's timeout.
    #[error("no answer in time")]
    TimedOut,
}

/// Delivers webhook requests.
#[async_trait]
pub trait WebhookSender: Send + Sync {
    /// Sends `request` as one `POST` and returns the answer's status code, whatever it is: a
    /// `4xx` or `5xx` is an answer, not an error. Redirects are not followed; a `3xx` is
    /// returned as it is. The answer's body is not kept. No cookies are sent or kept.
    async fn send(&self, request: &WebhookRequest) -> Result<u16, WebhookError>;
}

/// Conformance suite every [`WebhookSender`] runs in its own tests, against a local receiver
/// the implementation's test provides.
#[cfg(feature = "testing")]
pub mod testing {
    use super::*;

    /// A request as the receiver got it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Received {
        /// The request path.
        pub path: String,
        /// Header names in lowercase, with their values.
        pub headers: Vec<(String, String)>,
        /// The body.
        pub body: Bytes,
    }

    /// A local HTTP server for the suite.
    #[async_trait]
    pub trait Receiver: Send + Sync {
        /// An address under which every `POST` is recorded and answered with `status`; `path`
        /// tells requests apart.
        fn url(&self, path: &str, status: u16) -> String;
        /// An address that answers `307` with a `Location` pointing at `url(target, 200)`.
        fn redirect(&self, path: &str, target: &str) -> String;
        /// An address nothing listens on.
        fn closed(&self) -> String;
        /// Every recorded request, oldest first.
        async fn received(&self) -> Vec<Received>;
    }

    /// Checks `sender` against `receiver`. Panics on the first violation.
    pub async fn conformance(sender: &dyn WebhookSender, receiver: &dyn Receiver) {
        let body = Bytes::from_static(b"{\"event\":\"webhook.test\",\"data\":{\"n\":1}}");
        for (path, status) in [
            ("ok", 200u16),
            ("empty", 204),
            ("gone", 410),
            ("broken", 500),
        ] {
            let answer = sender
                .send(&WebhookRequest {
                    url: receiver.url(path, status),
                    headers: vec![
                        ("Holtmail-Event".into(), "webhook.test".into()),
                        ("Holtmail-Signature".into(), format!("t=1,v1={path}")),
                    ],
                    body: body.clone(),
                })
                .await;
            assert_eq!(answer.ok(), Some(status), "{path}");
        }
        let received = receiver.received().await;
        assert_eq!(received.len(), 4);
        for request in &received {
            assert_eq!(request.body, body, "the body changed");
            let header = |name: &str| {
                request
                    .headers
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.as_str())
            };
            assert_eq!(header("holtmail-event"), Some("webhook.test"));
            assert_eq!(
                header("holtmail-signature"),
                Some(format!("t=1,v1={}", request.path.trim_start_matches('/')).as_str())
            );
            assert!(
                header("content-type").is_some_and(|v| v.starts_with("application/json")),
                "no JSON content type"
            );
            assert!(header("cookie").is_none());
        }

        let moved = sender
            .send(&WebhookRequest {
                url: receiver.redirect("moved", "target"),
                headers: Vec::new(),
                body: body.clone(),
            })
            .await;
        assert_eq!(moved.ok(), Some(307), "a redirect was followed");
        assert_eq!(
            receiver.received().await.len(),
            4,
            "the redirect's target was asked"
        );

        assert!(matches!(
            sender
                .send(&WebhookRequest {
                    url: receiver.closed(),
                    headers: Vec::new(),
                    body: body.clone(),
                })
                .await,
            Err(WebhookError::Unreachable(_))
        ));
        for url in ["not a url", "ftp://example.com/hook", "file:///etc/passwd"] {
            assert!(
                matches!(
                    sender
                        .send(&WebhookRequest {
                            url: url.into(),
                            headers: Vec::new(),
                            body: body.clone(),
                        })
                        .await,
                    Err(WebhookError::Invalid(_))
                ),
                "{url}"
            );
        }
    }
}
