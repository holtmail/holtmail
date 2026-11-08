use std::time::Duration;

use async_trait::async_trait;
use holtmail_alias::{
    AliasChange, AliasDirectory, AliasDomain, AliasError, AliasOptions, Capabilities, Mailbox,
    NewAlias, RemoteAlias, parse_mailbox,
};
use holtmail_core::{Timestamp, TlsRoots};
use holtmail_json_http::{HttpError, JsonHttp, Method};
use serde::Deserialize;
use serde_json::{Value, json};

/// The hosted service.
pub const DEFAULT_URL: &str = "https://app.simplelogin.io";

const PAGE: usize = 20;
const MAX_PAGES: u32 = 5000;

/// One SimpleLogin account, reached with an API key from its settings. Lists, creates (random
/// or with a chosen prefix and suffix), switches, edits (note, display name, mailboxes) and
/// deletes aliases, and makes reverse aliases for new contacts through the contacts API.
#[derive(Clone)]
pub struct SimpleLoginDirectory {
    http: JsonHttp,
}

impl SimpleLoginDirectory {
    /// A directory for the account of `api_key` at `base_url` ([`DEFAULT_URL`] or a self-hosted
    /// instance), trusting `roots` besides the built-in ones. `Invalid` for an address that is
    /// not https (or http to a loopback address).
    pub fn new(base_url: &str, api_key: &str, roots: &TlsRoots) -> Result<Self, AliasError> {
        let http = JsonHttp::new(
            base_url,
            &[("Authentication", api_key.trim())],
            Duration::from_secs(30),
            roots,
        )
        .map_err(error)?;
        Ok(Self { http })
    }

    async fn alias(&self, id: &str) -> Result<RemoteAlias, AliasError> {
        let found: SlAlias = self
            .http
            .get(&format!("api/aliases/{}", number(id)?))
            .await
            .map_err(error)?;
        Ok(found.into())
    }

    async fn mailboxes(&self) -> Result<Vec<Mailbox>, AliasError> {
        let answer: SlMailboxes = self.http.get("api/v2/mailboxes").await.map_err(error)?;
        let mut boxes: Vec<_> = answer
            .mailboxes
            .into_iter()
            .filter(|m| m.verified.unwrap_or(true))
            .collect();
        boxes.sort_by_key(|m| !m.default.unwrap_or(false));
        Ok(boxes
            .into_iter()
            .map(|m| Mailbox {
                id: m.id.to_string(),
                email: m.email,
            })
            .collect())
    }

    async fn patch(&self, id: i64, change: &AliasChange) -> Result<(), AliasError> {
        let mut body = serde_json::Map::new();
        if let Some(note) = &change.note {
            body.insert("note".into(), text_or_null(note));
        }
        if let Some(name) = &change.name {
            body.insert("name".into(), text_or_null(name));
        }
        if let Some(ids) = &change.mailbox_ids {
            if ids.is_empty() {
                return Err(AliasError::Invalid("choose at least one mailbox".into()));
            }
            body.insert("mailbox_ids".into(), json!(numbers(ids)?));
        }
        if body.is_empty() {
            return Ok(());
        }
        self.http
            .call::<Value, _>(Method::PATCH, &format!("api/aliases/{id}"), Some(&body))
            .await
            .map_err(error)?;
        Ok(())
    }
}

fn text_or_null(text: &str) -> Value {
    if text.trim().is_empty() {
        Value::Null
    } else {
        json!(text.trim())
    }
}

fn number(id: &str) -> Result<i64, AliasError> {
    id.trim().parse().map_err(|_| AliasError::NotFound)
}

fn numbers(ids: &[String]) -> Result<Vec<i64>, AliasError> {
    ids.iter()
        .map(|id| {
            id.trim()
                .parse()
                .map_err(|_| AliasError::Invalid(format!("no mailbox {id}")))
        })
        .collect()
}

pub(crate) fn error(e: HttpError) -> AliasError {
    match e {
        HttpError::Invalid(m) => AliasError::Invalid(m),
        HttpError::Status {
            status: 401 | 403, ..
        } => AliasError::Auth,
        HttpError::Status { status: 404, .. } => AliasError::NotFound,
        HttpError::Status { status: 429, .. } => {
            AliasError::Unavailable("too many requests, try again in a minute".into())
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
struct SlMailbox {
    id: i64,
    email: String,
}

#[derive(Deserialize)]
struct SlMailboxEntry {
    id: i64,
    email: String,
    default: Option<bool>,
    verified: Option<bool>,
}

#[derive(Deserialize)]
struct SlMailboxes {
    mailboxes: Vec<SlMailboxEntry>,
}

#[derive(Deserialize)]
struct SlAlias {
    id: i64,
    email: String,
    name: Option<String>,
    enabled: bool,
    note: Option<String>,
    creation_timestamp: Option<i64>,
    nb_forward: Option<u64>,
    nb_block: Option<u64>,
    nb_reply: Option<u64>,
    #[serde(default)]
    mailboxes: Vec<SlMailbox>,
}

impl From<SlAlias> for RemoteAlias {
    fn from(a: SlAlias) -> Self {
        Self {
            id: a.id.to_string(),
            address: a.email.trim().to_ascii_lowercase(),
            enabled: a.enabled,
            note: a.note.filter(|n| !n.trim().is_empty()),
            name: a.name.filter(|n| !n.trim().is_empty()),
            for_domain: None,
            mailboxes: a
                .mailboxes
                .into_iter()
                .map(|m| Mailbox {
                    id: m.id.to_string(),
                    email: m.email,
                })
                .collect(),
            created_at: a
                .creation_timestamp
                .map(|s| Timestamp::from_millis(s.saturating_mul(1000))),
            forwarded: a.nb_forward,
            blocked: a.nb_block,
            replied: a.nb_reply,
        }
    }
}

#[derive(Deserialize)]
struct SlAliases {
    aliases: Vec<SlAlias>,
}

#[derive(Deserialize)]
struct SlContact {
    reverse_alias: Option<String>,
    reverse_alias_address: Option<String>,
}

#[derive(Deserialize)]
struct SlSuffix {
    suffix: String,
    signed_suffix: String,
}

#[derive(Deserialize)]
struct SlOptions {
    can_create: bool,
    prefix_suggestion: Option<String>,
    #[serde(default)]
    suffixes: Vec<SlSuffix>,
}

#[async_trait]
impl AliasDirectory for SimpleLoginDirectory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            create_random: true,
            create_custom: true,
            note: true,
            name: true,
            for_domain: false,
            switch: true,
            mailboxes: true,
            delete: true,
        }
    }

    async fn aliases(&self) -> Result<Vec<RemoteAlias>, AliasError> {
        let mut out: Vec<RemoteAlias> = Vec::new();
        for page in 0..MAX_PAGES {
            let answer: SlAliases = self
                .http
                .get(&format!("api/v2/aliases?page_id={page}"))
                .await
                .map_err(error)?;
            let count = answer.aliases.len();
            for alias in answer.aliases {
                let alias = RemoteAlias::from(alias);
                if !out.iter().any(|a| a.id == alias.id) {
                    out.push(alias);
                }
            }
            if count < PAGE {
                return Ok(out);
            }
        }
        Err(AliasError::Protocol("the alias list does not end".into()))
    }

    async fn options(&self) -> Result<AliasOptions, AliasError> {
        let options: SlOptions = self.http.get("api/v5/alias/options").await.map_err(error)?;
        Ok(AliasOptions {
            domains: options
                .suffixes
                .into_iter()
                .map(|s| AliasDomain {
                    id: s.signed_suffix,
                    label: s.suffix,
                })
                .collect(),
            mailboxes: self.mailboxes().await?,
            prefix_hint: options.prefix_suggestion.filter(|p| !p.is_empty()),
            can_create: options.can_create,
        })
    }

    async fn create(&self, new: &NewAlias) -> Result<RemoteAlias, AliasError> {
        if new.for_domain.is_some() {
            return Err(AliasError::Unsupported);
        }
        let note = new.note.as_deref().map(text_or_null).unwrap_or(Value::Null);
        let created: SlAlias = match &new.prefix {
            Some(prefix) => {
                let options = if new.domain.is_none() || new.mailbox_ids.is_empty() {
                    Some(self.options().await?)
                } else {
                    None
                };
                let signed_suffix = match (&new.domain, &options) {
                    (Some(domain), _) => domain.clone(),
                    (None, Some(options)) => options
                        .domains
                        .first()
                        .map(|d| d.id.clone())
                        .ok_or_else(|| AliasError::Invalid("no suffix to use".into()))?,
                    (None, None) => unreachable!("options are read without a domain"),
                };
                let mailbox_ids = match (new.mailbox_ids.is_empty(), &options) {
                    (false, _) => numbers(&new.mailbox_ids)?,
                    (true, Some(options)) => numbers(
                        &options
                            .mailboxes
                            .first()
                            .map(|m| vec![m.id.clone()])
                            .ok_or_else(|| AliasError::Invalid("no mailbox to use".into()))?,
                    )?,
                    (true, None) => unreachable!("options are read without mailboxes"),
                };
                let mut body = json!({
                    "alias_prefix": prefix.trim(),
                    "signed_suffix": signed_suffix,
                    "mailbox_ids": mailbox_ids,
                    "note": note,
                });
                if let Some(name) = &new.name {
                    body["name"] = text_or_null(name);
                }
                self.http
                    .call(Method::POST, "api/v3/alias/custom/new", Some(&body))
                    .await
                    .map_err(error)?
            }
            None => {
                let created: SlAlias = self
                    .http
                    .call(
                        Method::POST,
                        "api/alias/random/new",
                        Some(&json!({ "note": note })),
                    )
                    .await
                    .map_err(error)?;
                if new.name.is_some() || !new.mailbox_ids.is_empty() {
                    self.patch(
                        created.id,
                        &AliasChange {
                            name: new.name.clone(),
                            mailbox_ids: (!new.mailbox_ids.is_empty())
                                .then(|| new.mailbox_ids.clone()),
                            ..AliasChange::default()
                        },
                    )
                    .await?;
                    return self.alias(&created.id.to_string()).await;
                }
                created
            }
        };
        Ok(created.into())
    }

    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError> {
        if change.for_domain.is_some() {
            return Err(AliasError::Unsupported);
        }
        let number = number(id)?;
        let current = self.alias(id).await?;
        self.patch(number, change).await?;
        if let Some(enabled) = change.enabled
            && enabled != current.enabled
        {
            self.http
                .call::<Value, _>(
                    Method::POST,
                    &format!("api/aliases/{number}/toggle"),
                    None::<&()>,
                )
                .await
                .map_err(error)?;
        }
        self.alias(id).await
    }

    fn reverse(&self) -> bool {
        true
    }

    async fn reverse_alias(&self, id: &str, contact: &str) -> Result<String, AliasError> {
        let contact = parse_mailbox(contact)
            .map(|a| a.email.trim().to_ascii_lowercase())
            .filter(|e| e.contains('@'))
            .ok_or_else(|| AliasError::Invalid(format!("{} is not an address", contact.trim())))?;
        let answer: SlContact = self
            .http
            .call(
                Method::POST,
                &format!("api/aliases/{}/contacts", number(id)?),
                Some(&json!({ "contact": contact })),
            )
            .await
            .map_err(|e| match e {
                HttpError::Status {
                    status: 403,
                    message: Some(message),
                } if !message.eq_ignore_ascii_case("forbidden") => AliasError::Invalid(message),
                HttpError::Status { status: 403, .. } => AliasError::NotFound,
                other => error(other),
            })?;
        answer
            .reverse_alias_address
            .or_else(|| {
                answer
                    .reverse_alias
                    .as_deref()
                    .and_then(parse_mailbox)
                    .map(|a| a.email)
            })
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| a.contains('@'))
            .ok_or_else(|| AliasError::Protocol("the contact has no reverse alias".into()))
    }

    async fn delete(&self, id: &str) -> Result<(), AliasError> {
        self.http
            .call::<Value, _>(
                Method::DELETE,
                &format!("api/aliases/{}", number(id)?),
                None::<&()>,
            )
            .await
            .map_err(error)?;
        Ok(())
    }
}
