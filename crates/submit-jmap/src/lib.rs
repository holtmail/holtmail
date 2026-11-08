//! Handing prebuilt messages to a JMAP server (RFC 8620, RFC 8621).
//!
//! [`JmapSubmitter`] implements [`holtmail_submit::Submitter`]: it uploads the exact bytes as a
//! blob, imports them into the account's Drafts mailbox (or Sent when there is none) and creates
//! an `EmailSubmission` with the explicit envelope in the same request. `onSuccessUpdateEmail`
//! moves the email to Sent and clears `$draft`, so the provider keeps the sent copy itself.

mod http;
mod session;

use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::TlsRoots;
use holtmail_submit::{
    Envelope, Limits, RecipientOutcome, RecipientResult, Refusal, SubmitError, SubmitReport,
    Submitter,
};
use serde_json::{Map, Value, json};
use url::Url;

use http::{Http, Stage};
use session::{CORE, MAIL, SUBMISSION, Session, pointer_key};

/// What the client signs in with.
#[derive(Clone, PartialEq, Eq)]
pub enum JmapSecret {
    /// HTTP Basic with [`JmapSubmitConfig::username`] and this password or app password.
    Password(String),
    /// A bearer token (an API token or an OAuth access token); the user name is not sent.
    Token(String),
}

impl fmt::Debug for JmapSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => f.write_str("Password(..)"),
            Self::Token(_) => f.write_str("Token(..)"),
        }
    }
}

/// Where and how to submit.
#[derive(Debug, Clone)]
pub struct JmapSubmitConfig {
    /// The session resource, or a server's address where `/.well-known/jmap` is asked; see
    /// [`session_url`].
    pub url: String,
    /// The user name for [`JmapSecret::Password`]; ignored for a token.
    pub username: String,
    /// The password or token.
    pub secret: JmapSecret,
    /// The address whose JMAP identity submits. `None` uses the envelope's return path, and the
    /// account's first identity for the null return path.
    pub address: Option<String>,
    /// The limit for every HTTP request.
    pub timeout: Duration,
}

impl JmapSubmitConfig {
    /// A configuration with no fixed identity address and a two-minute timeout.
    pub fn new(url: impl Into<String>, username: impl Into<String>, secret: JmapSecret) -> Self {
        Self {
            url: url.into(),
            username: username.into(),
            secret,
            address: None,
            timeout: Duration::from_secs(120),
        }
    }
}

/// The session resource for what someone typed: `https://` is added when no scheme is given, and
/// an address without a path gets `/.well-known/jmap`. Only `https` is accepted, except plain
/// `http` to a loopback address. [`SubmitError::Invalid`] for anything else.
pub fn session_url(input: &str) -> Result<String, SubmitError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SubmitError::Invalid("the JMAP address is empty".into()));
    }
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|_| SubmitError::Invalid("the JMAP address is not a web address".into()))?;
    if !http::allowed(&url) {
        return Err(SubmitError::Invalid(
            "the JMAP address must use https".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SubmitError::Invalid(
            "put the user name and password in their own fields, not in the address".into(),
        ));
    }
    url.set_fragment(None);
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/.well-known/jmap");
    }
    Ok(url.to_string())
}

/// A JMAP submission client. Every call asks for the session afresh; calls share nothing but the
/// [`Submitter::limits`] last seen.
pub struct JmapSubmitter {
    config: JmapSubmitConfig,
    http: Http,
    limits: Mutex<Limits>,
}

impl fmt::Debug for JmapSubmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JmapSubmitter")
            .field("url", &self.config.url)
            .finish_non_exhaustive()
    }
}

struct Account {
    identities: Vec<(String, String)>,
    drafts: Option<String>,
    sent: Option<String>,
}

enum Outcome {
    Created(Value),
    Failed(Value),
    MethodError(String),
    Missing,
}

impl JmapSubmitter {
    /// A client trusting the public web PKI roots. [`SubmitError::Invalid`] when the address is
    /// not usable or a password comes without a user name.
    pub fn new(config: JmapSubmitConfig) -> Result<Self, SubmitError> {
        session_url(&config.url)?;
        let auth = match &config.secret {
            JmapSecret::Password(password) => {
                if config.username.trim().is_empty() {
                    return Err(SubmitError::Invalid(
                        "signing in with a password needs a user name".into(),
                    ));
                }
                http::Auth::Basic(config.username.trim().to_owned(), password.clone())
            }
            JmapSecret::Token(token) => http::Auth::Bearer(token.clone()),
        };
        Ok(Self {
            http: Http::new(auth, config.timeout)?,
            config,
            limits: Mutex::new(Limits::default()),
        })
    }

    /// Also trusts `roots`, for servers with a private CA.
    pub fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, SubmitError> {
        self.http = self.http.with_roots(roots)?;
        Ok(self)
    }

    async fn open(&self) -> Result<Session, SubmitError> {
        let url = session_url(&self.config.url)?;
        let mut base = Url::parse(&url).map_err(|e| SubmitError::Invalid(e.to_string()))?;
        if base.path().starts_with("/.well-known/") {
            base = self.http.resolve(base).await?;
        }
        let value = self.http.session(&base).await?;
        let session = Session::parse(&base, &value)?;
        *self.limits.lock().expect("limits") = Limits {
            max_size: session.max_size,
            smtputf8: false,
        };
        Ok(session)
    }

    async fn call(
        &self,
        session: &Session,
        calls: Vec<(&str, Value)>,
        stage: Stage,
    ) -> Result<Vec<(String, Value, String)>, SubmitError> {
        let body = json!({
            "using": [CORE, MAIL, SUBMISSION],
            "methodCalls": calls
                .into_iter()
                .enumerate()
                .map(|(i, (name, args))| json!([name, args, i.to_string()]))
                .collect::<Vec<_>>(),
        });
        let answer = self.http.post_json(&session.api_url, &body, stage).await?;
        let malformed = || match stage {
            Stage::Handover => SubmitError::Uncertain("the JMAP answer is malformed".into()),
            Stage::Before => SubmitError::Unavailable("the JMAP answer is malformed".into()),
        };
        answer
            .get("methodResponses")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?
            .iter()
            .map(|response| match response.as_array().map(Vec::as_slice) {
                Some([Value::String(name), args, Value::String(id)]) => {
                    Ok((name.clone(), args.clone(), id.clone()))
                }
                _ => Err(malformed()),
            })
            .collect()
    }

    async fn account(&self, session: &Session) -> Result<Account, SubmitError> {
        let responses = self
            .call(
                session,
                vec![
                    (
                        "Mailbox/get",
                        json!({ "accountId": session.account, "ids": null, "properties": ["id", "role"] }),
                    ),
                    (
                        "Identity/get",
                        json!({ "accountId": session.account, "ids": null }),
                    ),
                ],
                Stage::Before,
            )
            .await?;
        let list = |id: &str| -> Result<Vec<Value>, SubmitError> {
            match responses.iter().find(|(_, _, i)| i == id) {
                Some((name, args, _)) if name == "error" => {
                    let kind = args
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    Err(match kind {
                        "serverUnavailable" | "serverFail" => SubmitError::Unavailable(format!(
                            "the JMAP server failed a request: {kind}"
                        )),
                        "forbidden" | "accountNotFound" => SubmitError::Auth(format!(
                            "the JMAP server refused the account: {kind}"
                        )),
                        _ => SubmitError::Invalid(format!(
                            "the JMAP server refused a request: {kind}"
                        )),
                    })
                }
                Some((_, args, _)) => Ok(args
                    .get("list")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()),
                None => Err(SubmitError::Unavailable(
                    "the JMAP server left a call unanswered".into(),
                )),
            }
        };
        let mailboxes = list("0")?;
        let role = |role: &str| {
            mailboxes
                .iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some(role))
                .and_then(|m| m.get("id").and_then(Value::as_str))
                .map(str::to_owned)
        };
        let identities = list("1")?
            .iter()
            .filter_map(|i| {
                Some((
                    i.get("id")?.as_str()?.to_owned(),
                    i.get("email")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        let sent = role("sent");
        let drafts = role("drafts").filter(|d| Some(d) != sent.as_ref());
        Ok(Account {
            identities,
            drafts,
            sent,
        })
    }

    async fn destroy(&self, session: &Session, email: &str) {
        let _ = self
            .call(
                session,
                vec![(
                    "Email/set",
                    json!({ "accountId": session.account, "destroy": [email] }),
                )],
                Stage::Before,
            )
            .await;
    }
}

/// The identity whose email is `address`, or a wildcard identity `*@domain` for its domain;
/// without an address, the first identity.
fn identity<'a>(identities: &'a [(String, String)], address: Option<&str>) -> Option<&'a str> {
    let Some(address) = address else {
        return identities.first().map(|(id, _)| id.as_str());
    };
    let domain = address.rsplit_once('@').map(|(_, d)| d);
    identities
        .iter()
        .find(|(_, email)| email.eq_ignore_ascii_case(address))
        .or_else(|| {
            identities.iter().find(|(_, email)| {
                email
                    .strip_prefix("*@")
                    .is_some_and(|d| domain.is_some_and(|domain| d.eq_ignore_ascii_case(domain)))
            })
        })
        .map(|(id, _)| id.as_str())
}

fn outcome(responses: &[(String, Value, String)], call: &str, creation: &str) -> Outcome {
    let Some((name, args, _)) = responses.iter().find(|(_, _, id)| id == call) else {
        return Outcome::Missing;
    };
    if name == "error" {
        return Outcome::MethodError(
            args.get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        );
    }
    if let Some(created) = args.pointer(&format!("/created/{creation}")) {
        return Outcome::Created(created.clone());
    }
    match args.pointer(&format!("/notCreated/{creation}")) {
        Some(error) => Outcome::Failed(error.clone()),
        None => Outcome::Missing,
    }
}

fn error_text(error: &Value) -> (String, String) {
    let kind = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let text = match error.get("description").and_then(Value::as_str) {
        Some(description) => format!("{kind}: {description}"),
        None => kind.clone(),
    };
    (kind, text)
}

fn refused(code: u16, text: String) -> SubmitError {
    SubmitError::Refused {
        permanent: true,
        code,
        text,
    }
}

fn set_error(error: &Value, limit: Option<u64>) -> SubmitError {
    let (kind, text) = error_text(error);
    match kind.as_str() {
        "forbiddenMailFrom" | "forbiddenFrom" | "forbiddenToSend" | "forbidden" => {
            refused(550, text)
        }
        "tooLarge" => SubmitError::TooLarge {
            limit: error.get("maxSize").and_then(Value::as_u64).or(limit),
        },
        "tooManyRecipients" | "overQuota" => refused(552, text),
        "rateLimit" => SubmitError::Temporary(text),
        "noRecipients" | "invalidProperties" => SubmitError::Invalid(text),
        _ => refused(554, text),
    }
}

fn method_error(kind: &str) -> SubmitError {
    let text = format!("the JMAP server refused the submission: {kind}");
    match kind {
        "serverFail" => SubmitError::Uncertain(text),
        "serverUnavailable" => SubmitError::Temporary(text),
        "forbidden" | "accountReadOnly" => refused(550, text),
        _ => SubmitError::Invalid(text),
    }
}

fn refusal_from_reply(reply: &str) -> Option<Refusal> {
    let code: u16 = reply.get(..3)?.parse().ok()?;
    (400..600).contains(&code).then(|| Refusal {
        permanent: code >= 500,
        code,
        text: reply.to_owned(),
    })
}

fn report(
    envelope: &Envelope,
    invalid: &[(String, Refusal)],
    created: &Value,
) -> Result<SubmitReport, SubmitError> {
    if created.get("undoStatus").and_then(Value::as_str) == Some("canceled") {
        return Err(refused(554, "the server canceled the submission".into()));
    }
    let status = created.get("deliveryStatus").and_then(Value::as_object);
    let recipients: Vec<RecipientResult> = envelope
        .recipients
        .iter()
        .map(|address| {
            let refusal = invalid
                .iter()
                .find(|(a, _)| a.eq_ignore_ascii_case(address))
                .map(|(_, r)| r.clone())
                .or_else(|| {
                    let entry = status?
                        .iter()
                        .find(|(a, _)| a.eq_ignore_ascii_case(address))?
                        .1;
                    if entry.get("delivered").and_then(Value::as_str) != Some("no") {
                        return None;
                    }
                    refusal_from_reply(entry.get("smtpReply")?.as_str()?)
                });
            RecipientResult {
                address: address.clone(),
                outcome: refusal.map_or(RecipientOutcome::Accepted, RecipientOutcome::Refused),
            }
        })
        .collect();
    if recipients
        .iter()
        .all(|r| matches!(r.outcome, RecipientOutcome::Refused(_)))
    {
        return Err(SubmitError::RecipientsRefused(recipients));
    }
    Ok(SubmitReport { recipients })
}

fn refused_all(envelope: &Envelope, invalid: &[(String, Refusal)]) -> SubmitError {
    SubmitError::RecipientsRefused(
        envelope
            .recipients
            .iter()
            .map(|address| RecipientResult {
                address: address.clone(),
                outcome: RecipientOutcome::Refused(
                    invalid
                        .iter()
                        .find(|(a, _)| a.eq_ignore_ascii_case(address))
                        .map(|(_, r)| r.clone())
                        .unwrap_or_else(invalid_recipient),
                ),
            })
            .collect(),
    )
}

fn invalid_recipient() -> Refusal {
    Refusal {
        permanent: true,
        code: 550,
        text: "invalidRecipients".into(),
    }
}

#[async_trait]
impl Submitter for JmapSubmitter {
    /// Fetches the session and checks that it offers mail submission for the account, that the
    /// account has a Sent mailbox and, when [`JmapSubmitConfig::address`] is set, an identity
    /// for it. Sends nothing and changes nothing.
    async fn check(&self) -> Result<Limits, SubmitError> {
        let session = self.open().await?;
        let account = self.account(&session).await?;
        if account.sent.is_none() {
            return Err(SubmitError::Invalid(
                "the JMAP account has no Sent mailbox".into(),
            ));
        }
        if let Some(address) = &self.config.address
            && identity(&account.identities, Some(address)).is_none()
        {
            return Err(SubmitError::Invalid(
                "the JMAP account has no identity for this address".into(),
            ));
        }
        Ok(self.limits())
    }

    async fn submit(
        &self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError> {
        envelope.validate(message)?;
        let session = self.open().await?;
        let size = message.len() as u64;
        if session.max_size.is_some_and(|max| size > max) {
            return Err(SubmitError::TooLarge {
                limit: session.max_size,
            });
        }
        let account = self.account(&session).await?;
        let sent = account
            .sent
            .clone()
            .ok_or_else(|| SubmitError::Invalid("the JMAP account has no Sent mailbox".into()))?;
        let address = self
            .config
            .address
            .as_deref()
            .or((!envelope.mail_from.is_empty()).then_some(envelope.mail_from.as_str()));
        let identity_id = identity(&account.identities, address)
            .ok_or_else(|| {
                SubmitError::Invalid("the JMAP account has no identity for this address".into())
            })?
            .to_owned();

        let uploaded = self
            .http
            .upload(&session.upload_url, message, session.max_size)
            .await?;
        let blob = uploaded
            .get("blobId")
            .and_then(Value::as_str)
            .ok_or_else(|| SubmitError::Unavailable("the upload gave no blob".into()))?
            .to_owned();

        let target = account.drafts.clone().unwrap_or_else(|| sent.clone());
        let mut patch = Map::new();
        if let Some(drafts) = &account.drafts {
            patch.insert(format!("mailboxIds/{}", pointer_key(drafts)), Value::Null);
            patch.insert(format!("mailboxIds/{}", pointer_key(&sent)), json!(true));
        }
        patch.insert("keywords/$draft".into(), Value::Null);
        let on_success = json!({ "#s": patch });
        let submission = |email: &str, recipients: &[&String]| {
            json!({
                "accountId": session.account,
                "create": { "s": {
                    "identityId": identity_id,
                    "emailId": email,
                    "envelope": {
                        "mailFrom": { "email": envelope.mail_from, "parameters": null },
                        "rcptTo": recipients
                            .iter()
                            .map(|r| json!({ "email": r, "parameters": null }))
                            .collect::<Vec<_>>(),
                    },
                } },
                "onSuccessUpdateEmail": on_success,
            })
        };

        let mut invalid: Vec<(String, Refusal)> = Vec::new();
        let mut email: Option<String> = None;
        loop {
            let remaining: Vec<&String> = envelope
                .recipients
                .iter()
                .filter(|r| !invalid.iter().any(|(a, _)| a.eq_ignore_ascii_case(r)))
                .collect();
            if remaining.is_empty() {
                if let Some(email) = &email {
                    self.destroy(&session, email).await;
                }
                return Err(refused_all(envelope, &invalid));
            }
            let result = match &email {
                None => {
                    let import = json!({
                        "accountId": session.account,
                        "emails": { "m": {
                            "blobId": blob,
                            "mailboxIds": { target.clone(): true },
                            "keywords": { "$draft": true, "$seen": true },
                        } },
                    });
                    let responses = self
                        .call(
                            &session,
                            vec![
                                ("Email/import", import),
                                ("EmailSubmission/set", submission("#m", &remaining)),
                            ],
                            Stage::Handover,
                        )
                        .await?;
                    let submitted = outcome(&responses, "1", "s");
                    if matches!(submitted, Outcome::Failed(_) | Outcome::MethodError(_)) {
                        match outcome(&responses, "0", "m") {
                            Outcome::Created(created) => {
                                email =
                                    created.get("id").and_then(Value::as_str).map(str::to_owned);
                            }
                            Outcome::Failed(error) => {
                                return Err(set_error(&error, session.max_size));
                            }
                            Outcome::MethodError(kind) => {
                                return Err(match kind.as_str() {
                                    "serverFail" | "serverUnavailable" => SubmitError::Temporary(
                                        format!("the JMAP server failed the import: {kind}"),
                                    ),
                                    _ => method_error(&kind),
                                });
                            }
                            Outcome::Missing => {}
                        }
                    }
                    submitted
                }
                Some(id) => {
                    let responses = self
                        .call(
                            &session,
                            vec![("EmailSubmission/set", submission(id, &remaining))],
                            Stage::Handover,
                        )
                        .await?;
                    outcome(&responses, "0", "s")
                }
            };
            let failure = match result {
                Outcome::Created(created) => return report(envelope, &invalid, &created),
                Outcome::Missing => {
                    return Err(SubmitError::Uncertain(
                        "the JMAP server did not say whether it took the submission".into(),
                    ));
                }
                Outcome::MethodError(kind) => method_error(&kind),
                Outcome::Failed(error) => {
                    let named: Vec<String> = error
                        .get("invalidRecipients")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|a| remaining.iter().any(|r| r.eq_ignore_ascii_case(a)))
                        .map(str::to_owned)
                        .collect();
                    if error.get("type").and_then(Value::as_str) == Some("invalidRecipients") {
                        let (_, text) = error_text(&error);
                        if named.is_empty() {
                            remaining.iter().for_each(|r| {
                                invalid.push(((*r).clone(), invalid_recipient()));
                            });
                        }
                        for address in named {
                            invalid.push((
                                address,
                                Refusal {
                                    permanent: true,
                                    code: 550,
                                    text: text.clone(),
                                },
                            ));
                        }
                        continue;
                    }
                    set_error(&error, session.max_size)
                }
            };
            if !failure.may_have_been_sent()
                && let Some(email) = &email
            {
                self.destroy(&session, email).await;
            }
            return Err(failure);
        }
    }

    fn limits(&self) -> Limits {
        self.limits.lock().expect("limits").clone()
    }
}

#[cfg(test)]
mod tests;
