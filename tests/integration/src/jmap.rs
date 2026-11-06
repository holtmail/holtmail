//! Seeding JMAP servers: one request with method calls, such as `Email/set` or `Mailbox/set`.

use serde_json::{Value, json};

/// How a [`Jmap`] client signs in.
#[derive(Clone)]
pub enum Auth {
    /// HTTP Basic with a user name and a password.
    Basic(String, String),
    /// A bearer token.
    Bearer(String),
}

/// A JMAP client signed in with HTTP Basic or a bearer token, trusting the stack's CA.
#[derive(Clone)]
pub struct Jmap {
    client: reqwest::Client,
    session: Value,
    auth: Auth,
}

impl Jmap {
    /// Reads the session at `session_url` with HTTP Basic and keeps it.
    pub async fn connect(session_url: &str, user: &str, password: &str) -> Self {
        Self::connect_with(
            session_url,
            Auth::Basic(user.to_owned(), password.to_owned()),
        )
        .await
    }

    /// Reads the session at `session_url` with `auth` and keeps it.
    pub async fn connect_with(session_url: &str, auth: Auth) -> Self {
        let client = crate::services::http_client();
        let session: Value = authorize(client.get(session_url), &auth)
            .send()
            .await
            .unwrap_or_else(|e| panic!("JMAP session at {session_url}: {e}"))
            .error_for_status()
            .unwrap_or_else(|e| panic!("JMAP session at {session_url}: {e}"))
            .json()
            .await
            .expect("a JMAP session");
        assert!(
            session["apiUrl"].is_string(),
            "the session names apiUrl: {session}"
        );
        Self {
            client,
            session,
            auth,
        }
    }

    /// The session resource as the server sent it.
    pub fn session(&self) -> &Value {
        &self.session
    }

    /// The primary account for `capability`, such as `urn:ietf:params:jmap:mail`.
    pub fn account(&self, capability: &str) -> String {
        self.session["primaryAccounts"][capability]
            .as_str()
            .unwrap_or_else(|| panic!("no primary account for {capability}"))
            .to_owned()
    }

    /// Sends `method_calls` with the capabilities in `using` and returns `methodResponses`.
    pub async fn call(&self, using: &[&str], method_calls: Value) -> Vec<Value> {
        let api_url = self.session["apiUrl"].as_str().expect("apiUrl");
        let answer: Value = authorize(self.client.post(api_url), &self.auth)
            .json(&json!({ "using": using, "methodCalls": method_calls }))
            .send()
            .await
            .expect("JMAP answers")
            .json()
            .await
            .expect("a JMAP answer");
        answer["methodResponses"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("no methodResponses in {answer}"))
    }

    /// Like [`Jmap::call`] with one method; fails the test on an `error` answer, a
    /// `notCreated`, `notUpdated` or `notDestroyed` entry, and returns the answer's arguments.
    pub async fn ok(&self, using: &[&str], method: &str, arguments: Value) -> Value {
        let answers = self.call(using, json!([[method, arguments, "0"]])).await;
        let answer = answers.first().cloned().unwrap_or_default();
        assert_eq!(answer[0], method, "{method}: {answer}");
        for refused in ["notCreated", "notUpdated", "notDestroyed"] {
            assert!(
                answer[1][refused].as_object().is_none_or(|m| m.is_empty()),
                "{method}: {answer}"
            );
        }
        answer[1].clone()
    }

    /// Uploads `bytes` to the primary mail account and returns the blob id.
    pub async fn upload(&self, bytes: &[u8]) -> String {
        let account = self.account("urn:ietf:params:jmap:mail");
        let url = self.session["uploadUrl"]
            .as_str()
            .expect("uploadUrl")
            .replace("{accountId}", &account);
        let answer: Value = authorize(self.client.post(url), &self.auth)
            .header(reqwest::header::CONTENT_TYPE, "message/rfc822")
            .body(bytes.to_vec())
            .send()
            .await
            .expect("the upload answers")
            .error_for_status()
            .expect("the upload is accepted")
            .json()
            .await
            .expect("an upload answer");
        answer["blobId"].as_str().expect("a blob id").to_owned()
    }
}

fn authorize(request: reqwest::RequestBuilder, auth: &Auth) -> reqwest::RequestBuilder {
    match auth {
        Auth::Basic(user, password) => request.basic_auth(user, Some(password)),
        Auth::Bearer(token) => request.bearer_auth(token),
    }
}
