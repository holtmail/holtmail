//! A scripted [`TokenEndpoint`] for tests, and the conformance suite every implementation runs.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{OAuthError, TokenAnswer, TokenEndpoint};

/// A request the fake received: the URL and the form.
pub type Request = (String, Vec<(String, String)>);

/// A token endpoint in memory: answers from a queue and remembers every request.
#[derive(Default)]
pub struct FakeTokenEndpoint {
    answers: Mutex<VecDeque<Result<TokenAnswer, OAuthError>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeTokenEndpoint {
    /// Queues an answer with `status` and a JSON `body`.
    pub fn answer(&self, status: u16, body: serde_json::Value) {
        self.answers
            .lock()
            .expect("answers lock")
            .push_back(Ok(TokenAnswer {
                status,
                body: body.to_string().into_bytes(),
            }));
    }

    /// Queues a successful token answer.
    pub fn tokens(&self, access_token: &str, refresh_token: Option<&str>, expires_in: u64) {
        let mut body = serde_json::json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": expires_in,
        });
        if let Some(refresh) = refresh_token {
            body["refresh_token"] = refresh.into();
        }
        self.answer(200, body);
    }

    /// Queues an OAuth error answer such as `invalid_grant`.
    pub fn error(&self, error: &str) {
        self.answer(
            400,
            serde_json::json!({ "error": error, "error_description": "Refused in a test." }),
        );
    }

    /// Queues an endpoint that cannot be reached.
    pub fn unreachable(&self) {
        self.answers
            .lock()
            .expect("answers lock")
            .push_back(Err(OAuthError::Connection("unreachable in a test".into())));
    }

    /// Every request so far: the URL and the form.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests lock").clone()
    }

    /// The value of `key` in the last request's form.
    pub fn last(&self, key: &str) -> Option<String> {
        self.requests
            .lock()
            .expect("requests lock")
            .last()?
            .1
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

#[async_trait]
impl TokenEndpoint for FakeTokenEndpoint {
    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<TokenAnswer, OAuthError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push((url.to_owned(), form.to_vec()));
        self.answers
            .lock()
            .expect("answers lock")
            .pop_front()
            .unwrap_or_else(|| Err(OAuthError::Connection("no answer queued".into())))
    }
}

/// Checks that `endpoint` keeps the [`TokenEndpoint`] contract without leaving the machine: a
/// closed loopback port and a plain `http` address elsewhere both fail with
/// [`OAuthError::Connection`], and no error repeats the form. Panics on the first violation.
pub async fn conformance(endpoint: &dyn TokenEndpoint) {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free loopback port")
        .port();
    let form = vec![
        (
            "refresh_token".to_owned(),
            "secret-refresh-token".to_owned(),
        ),
        ("client_secret".to_owned(), "secret-client".to_owned()),
    ];
    for url in [
        format!("http://127.0.0.1:{closed}/token"),
        "http://example.com/token".to_owned(),
        "ftp://example.com/token".to_owned(),
        "not a url".to_owned(),
    ] {
        match endpoint.post_form(&url, &form).await {
            Err(OAuthError::Connection(reason)) => assert!(
                !reason.contains("secret"),
                "{url}: the error repeats the form: {reason}"
            ),
            other => panic!("{url} did not fail with Connection: {other:?}"),
        }
    }
}
