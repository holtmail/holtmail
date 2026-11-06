//! Fastmail Masked Email: [`FastmailDirectory`] manages an account's masked addresses through
//! JMAP (`https://www.fastmail.com/dev/maskedemail`).
//!
//! Fastmail delivers mail to a masked address without rewriting it, so there is no detector:
//! messages are matched to masked addresses by their recipients once the directory has listed
//! them.

use std::time::Duration;

use async_trait::async_trait;
use holtmail_alias::{
    AliasChange, AliasDirectory, AliasDomain, AliasError, AliasOptions, Capabilities, NewAlias,
    RemoteAlias,
};
use holtmail_core::{Timestamp, TlsRoots};
use holtmail_json_http::{HttpError, JsonHttp, Method, allowed, utc_millis};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use url::Url;

/// The provider id.
pub const PROVIDER: &str = "fastmail";

/// Fastmail's JMAP session.
pub const DEFAULT_URL: &str = "https://api.fastmail.com/jmap/session";

const CAPABILITY: &str = "https://www.fastmail.com/dev/maskedemail";

/// One Fastmail account, reached with an API token that has the Masked Email scope. Lists,
/// creates (random or with a chosen prefix), switches, edits (description, website) and deletes
/// masked addresses. Deleted ones bounce mail and are not listed.
pub struct FastmailDirectory {
    session_url: String,
    token: String,
    roots: TlsRoots,
    session: OnceCell<Session>,
}

struct Session {
    api: JsonHttp,
    account: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionAnswer {
    api_url: String,
    #[serde(default)]
    primary_accounts: std::collections::HashMap<String, String>,
}

fn error(e: HttpError) -> AliasError {
    match e {
        HttpError::Invalid(m) => AliasError::Invalid(m),
        HttpError::Status {
            status: 401 | 403, ..
        } => AliasError::Auth,
        HttpError::Status { status: 404, .. } => AliasError::NotFound,
        HttpError::Status { status: 429, .. } => {
            AliasError::Unavailable("too many requests, try again later".into())
        }
        HttpError::Status { status, message } if (400..500).contains(&status) => {
            AliasError::Invalid(message.unwrap_or_else(|| format!("refused ({status})")))
        }
        HttpError::Status { status, .. } => AliasError::Unavailable(format!("status {status}")),
        HttpError::Unreachable(m) => AliasError::Unavailable(m),
        HttpError::Unexpected(m) => AliasError::Protocol(m),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Masked {
    id: String,
    email: String,
    state: String,
    for_domain: Option<String>,
    description: Option<String>,
    created_at: Option<String>,
}

impl From<Masked> for RemoteAlias {
    fn from(m: Masked) -> Self {
        Self {
            id: m.id,
            address: m.email.trim().to_ascii_lowercase(),
            enabled: m.state != "disabled",
            note: m.description.filter(|d| !d.trim().is_empty()),
            name: None,
            for_domain: m.for_domain.filter(|d| !d.trim().is_empty()),
            mailboxes: Vec::new(),
            created_at: m
                .created_at
                .as_deref()
                .and_then(utc_millis)
                .map(Timestamp::from_millis),
            forwarded: None,
            blocked: None,
            replied: None,
        }
    }
}

fn set_error(value: &Value) -> AliasError {
    let kind = value["type"].as_str().unwrap_or_default();
    let text = value["description"].as_str().map(str::to_owned);
    match kind {
        "notFound" => AliasError::NotFound,
        "rateLimit" => AliasError::Unavailable(
            text.unwrap_or_else(|| "too many masked addresses made, try later".into()),
        ),
        "forbidden" => AliasError::Auth,
        _ => AliasError::Invalid(text.unwrap_or_else(|| format!("refused ({kind})"))),
    }
}

impl FastmailDirectory {
    /// A directory for the account of `token` behind the JMAP session at `session_url`
    /// ([`DEFAULT_URL`] for Fastmail), trusting `roots` besides the built-in ones. The session
    /// is opened on first use.
    pub fn new(session_url: &str, token: &str, roots: &TlsRoots) -> Result<Self, AliasError> {
        let url = Url::parse(session_url.trim())
            .map_err(|_| AliasError::Invalid("the session address is not a URL".into()))?;
        if !allowed(&url) {
            return Err(AliasError::Invalid(
                "the session address must use https".into(),
            ));
        }
        Ok(Self {
            session_url: session_url.trim().to_owned(),
            token: token.trim().to_owned(),
            roots: roots.clone(),
            session: OnceCell::new(),
        })
    }

    fn client(&self, base: &str) -> Result<JsonHttp, AliasError> {
        let bearer = format!("Bearer {}", self.token);
        JsonHttp::new(
            base,
            &[("Authorization", bearer.as_str())],
            Duration::from_secs(30),
            &self.roots,
        )
        .map_err(error)
    }

    async fn session(&self) -> Result<&Session, AliasError> {
        self.session
            .get_or_try_init(|| async {
                let http = self.client(&self.session_url)?;
                let answer: SessionAnswer = http.get("").await.map_err(error)?;
                let account = answer
                    .primary_accounts
                    .get(CAPABILITY)
                    .cloned()
                    .ok_or_else(|| {
                        AliasError::Invalid(
                            "the token has no access to Masked Email; give it that scope".into(),
                        )
                    })?;
                let api_url = Url::parse(&self.session_url)
                    .and_then(|base| base.join(&answer.api_url))
                    .map_err(|_| AliasError::Protocol("the session has no API address".into()))?;
                if !allowed(&api_url) {
                    return Err(AliasError::Protocol(
                        "the session names an API address that is not https".into(),
                    ));
                }
                Ok(Session {
                    api: self.client(api_url.as_str())?,
                    account,
                })
            })
            .await
    }

    async fn call(&self, method: &str, arguments: Value) -> Result<Value, AliasError> {
        let session = self.session().await?;
        let mut arguments = arguments;
        arguments["accountId"] = json!(session.account);
        let request = json!({
            "using": ["urn:ietf:params:jmap:core", CAPABILITY],
            "methodCalls": [[method, arguments, "0"]],
        });
        let answer: Value = session
            .api
            .call(Method::POST, "", Some(&request))
            .await
            .map_err(error)?;
        let response = answer["methodResponses"]
            .get(0)
            .ok_or_else(|| AliasError::Protocol("no method response".into()))?;
        match response[0].as_str() {
            Some("error") => {
                let kind = response[1]["type"].as_str().unwrap_or_default();
                Err(match kind {
                    "forbidden" | "accountNotFound" => AliasError::Auth,
                    "unknownMethod" | "unknownCapability" => AliasError::Invalid(
                        "the token has no access to Masked Email; give it that scope".into(),
                    ),
                    _ => AliasError::Protocol(format!("{method} failed: {kind}")),
                })
            }
            Some(_) => Ok(response[1].clone()),
            None => Err(AliasError::Protocol(
                "a method response not understood".into(),
            )),
        }
    }

    async fn get(&self, ids: Option<Vec<String>>) -> Result<Vec<Masked>, AliasError> {
        let answer = self.call("MaskedEmail/get", json!({ "ids": ids })).await?;
        serde_json::from_value(answer["list"].clone())
            .map_err(|e| AliasError::Protocol(format!("MaskedEmail/get: {e}")))
    }
}

#[async_trait]
impl AliasDirectory for FastmailDirectory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            create_random: true,
            create_custom: true,
            note: true,
            name: false,
            for_domain: true,
            switch: true,
            mailboxes: false,
            delete: true,
        }
    }

    async fn aliases(&self) -> Result<Vec<RemoteAlias>, AliasError> {
        Ok(self
            .get(None)
            .await?
            .into_iter()
            .filter(|m| m.state != "deleted")
            .map(RemoteAlias::from)
            .collect())
    }

    async fn options(&self) -> Result<AliasOptions, AliasError> {
        self.get(Some(Vec::new())).await?;
        Ok(AliasOptions {
            domains: vec![AliasDomain {
                id: String::new(),
                label: "Fastmail's masked domain".into(),
            }],
            can_create: true,
            ..AliasOptions::default()
        })
    }

    async fn create(&self, new: &NewAlias) -> Result<RemoteAlias, AliasError> {
        if new.name.is_some() || !new.mailbox_ids.is_empty() {
            return Err(AliasError::Unsupported);
        }
        let mut object = json!({
            "state": "enabled",
            "description": new.note.as_deref().map(str::trim).unwrap_or_default(),
        });
        if let Some(site) = new
            .for_domain
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            object["forDomain"] = json!(site);
        }
        if let Some(prefix) = &new.prefix {
            let prefix = prefix.trim().to_ascii_lowercase();
            if prefix.is_empty()
                || prefix.len() > 64
                || !prefix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(AliasError::Invalid(
                    "a prefix has at most 64 letters, digits and underscores".into(),
                ));
            }
            object["emailPrefix"] = json!(prefix);
        }
        let answer = self
            .call("MaskedEmail/set", json!({ "create": { "new": object } }))
            .await?;
        if let Some(refused) = answer["notCreated"].get("new") {
            return Err(set_error(refused));
        }
        let id = answer["created"]["new"]["id"]
            .as_str()
            .ok_or_else(|| AliasError::Protocol("MaskedEmail/set made nothing".into()))?
            .to_owned();
        self.get(Some(vec![id]))
            .await?
            .into_iter()
            .next()
            .map(RemoteAlias::from)
            .ok_or_else(|| AliasError::Protocol("the new masked address is missing".into()))
    }

    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError> {
        if change.name.is_some() || change.mailbox_ids.is_some() {
            return Err(AliasError::Unsupported);
        }
        let mut patch = serde_json::Map::new();
        if let Some(enabled) = change.enabled {
            patch.insert(
                "state".into(),
                json!(if enabled { "enabled" } else { "disabled" }),
            );
        }
        if let Some(note) = &change.note {
            patch.insert("description".into(), json!(note.trim()));
        }
        if let Some(site) = &change.for_domain {
            patch.insert("forDomain".into(), json!(site.trim()));
        }
        let current = self
            .get(Some(vec![id.to_owned()]))
            .await?
            .into_iter()
            .find(|m| m.state != "deleted")
            .ok_or(AliasError::NotFound)?;
        if !patch.is_empty() {
            let answer = self
                .call("MaskedEmail/set", json!({ "update": { id: patch } }))
                .await?;
            if let Some(refused) = answer["notUpdated"].get(id) {
                return Err(set_error(refused));
            }
        } else {
            return Ok(current.into());
        }
        self.get(Some(vec![id.to_owned()]))
            .await?
            .into_iter()
            .next()
            .map(RemoteAlias::from)
            .ok_or(AliasError::NotFound)
    }

    async fn delete(&self, id: &str) -> Result<(), AliasError> {
        let current = self.get(Some(vec![id.to_owned()])).await?;
        if !current.iter().any(|m| m.state != "deleted") {
            return Err(AliasError::NotFound);
        }
        let answer = self
            .call(
                "MaskedEmail/set",
                json!({ "update": { id: { "state": "deleted" } } }),
            )
            .await?;
        if let Some(refused) = answer["notUpdated"].get(id) {
            return Err(set_error(refused));
        }
        Ok(())
    }
}
