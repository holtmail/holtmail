use std::time::Duration;

use async_trait::async_trait;
use holtmail_alias::{
    AliasChange, AliasDirectory, AliasDomain, AliasError, AliasOptions, Capabilities, Mailbox,
    NewAlias, RemoteAlias,
};
use holtmail_core::{Timestamp, TlsRoots};
use holtmail_json_http::{HttpError, JsonHttp, Method, utc_millis};
use serde::Deserialize;
use serde_json::{Value, json};

/// The hosted service.
pub const DEFAULT_URL: &str = "https://app.addy.io";

const MAX_PAGES: u32 = 5000;

/// One addy.io account, reached with an API key from its settings. Lists, creates (random or
/// with a chosen local part), switches, edits (description, display name, recipients) and
/// deletes aliases.
#[derive(Clone)]
pub struct AddyDirectory {
    http: JsonHttp,
}

impl AddyDirectory {
    /// A directory for the account of `api_key` at `base_url` ([`DEFAULT_URL`] or a self-hosted
    /// instance), trusting `roots` besides the built-in ones.
    pub fn new(base_url: &str, api_key: &str, roots: &TlsRoots) -> Result<Self, AliasError> {
        let bearer = format!("Bearer {}", api_key.trim());
        let http = JsonHttp::new(
            base_url,
            &[
                ("Authorization", bearer.as_str()),
                ("X-Requested-With", "XMLHttpRequest"),
            ],
            Duration::from_secs(30),
            roots,
        )
        .map_err(error)?;
        Ok(Self { http })
    }

    async fn alias(&self, id: &str) -> Result<RemoteAlias, AliasError> {
        let found: Data<AddyAlias> = self
            .http
            .get(&format!("api/v1/aliases/{}", segment(id)?))
            .await
            .map_err(error)?;
        Ok(found.data.into())
    }

    async fn domain_options(&self) -> Result<DomainOptions, AliasError> {
        self.http.get("api/v1/domain-options").await.map_err(error)
    }
}

fn segment(id: &str) -> Result<&str, AliasError> {
    let id = id.trim();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(AliasError::NotFound);
    }
    Ok(id)
}

fn text(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        json!(value.trim())
    }
}

pub(crate) fn error(e: HttpError) -> AliasError {
    match e {
        HttpError::Invalid(m) => AliasError::Invalid(m),
        HttpError::Status {
            status: 401 | 403, ..
        } => AliasError::Auth,
        HttpError::Status { status: 404, .. } => AliasError::NotFound,
        HttpError::Status { status: 429, .. } => {
            AliasError::Unavailable("too many requests or aliases this hour, try later".into())
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
struct Data<T> {
    data: T,
}

#[derive(Deserialize)]
struct Meta {
    current_page: u32,
    last_page: u32,
}

#[derive(Deserialize)]
struct Page {
    data: Vec<AddyAlias>,
    meta: Option<Meta>,
}

#[derive(Deserialize)]
struct AddyRecipient {
    id: String,
    email: String,
    email_verified_at: Option<String>,
}

#[derive(Deserialize)]
struct AddyAlias {
    id: String,
    email: String,
    active: bool,
    description: Option<String>,
    from_name: Option<String>,
    emails_forwarded: Option<u64>,
    emails_blocked: Option<u64>,
    emails_replied: Option<u64>,
    created_at: Option<String>,
    deleted_at: Option<String>,
    #[serde(default)]
    recipients: Vec<AddyRecipient>,
}

impl From<AddyAlias> for RemoteAlias {
    fn from(a: AddyAlias) -> Self {
        Self {
            id: a.id,
            address: a.email.trim().to_ascii_lowercase(),
            enabled: a.active,
            note: a.description.filter(|d| !d.trim().is_empty()),
            name: a.from_name.filter(|d| !d.trim().is_empty()),
            for_domain: None,
            mailboxes: a
                .recipients
                .into_iter()
                .map(|r| Mailbox {
                    id: r.id,
                    email: r.email,
                })
                .collect(),
            created_at: a
                .created_at
                .as_deref()
                .and_then(utc_millis)
                .map(Timestamp::from_millis),
            forwarded: a.emails_forwarded,
            blocked: a.emails_blocked,
            replied: a.emails_replied,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DomainOptions {
    data: Vec<String>,
    default_alias_domain: Option<String>,
}

#[async_trait]
impl AliasDirectory for AddyDirectory {
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
        for page in 1..=MAX_PAGES {
            let answer: Page = self
                .http
                .get(&format!(
                    "api/v1/aliases?page%5Bnumber%5D={page}&page%5Bsize%5D=100&with=recipients"
                ))
                .await
                .map_err(error)?;
            let count = answer.data.len();
            for alias in answer.data.into_iter().filter(|a| a.deleted_at.is_none()) {
                let alias = RemoteAlias::from(alias);
                if !out.iter().any(|a| a.id == alias.id) {
                    out.push(alias);
                }
            }
            let last = match answer.meta {
                Some(meta) => meta.current_page >= meta.last_page,
                None => count < 100,
            };
            if last || count == 0 {
                return Ok(out);
            }
        }
        Err(AliasError::Protocol("the alias list does not end".into()))
    }

    async fn options(&self) -> Result<AliasOptions, AliasError> {
        let options = self.domain_options().await?;
        let mut domains = options.data;
        if let Some(default) = &options.default_alias_domain
            && let Some(at) = domains.iter().position(|d| d == default)
        {
            let default = domains.remove(at);
            domains.insert(0, default);
        }
        let recipients: Data<Vec<AddyRecipient>> =
            self.http.get("api/v1/recipients").await.map_err(error)?;
        Ok(AliasOptions {
            domains: domains
                .into_iter()
                .map(|d| AliasDomain {
                    label: format!("@{d}"),
                    id: d,
                })
                .collect(),
            mailboxes: recipients
                .data
                .into_iter()
                .filter(|r| r.email_verified_at.is_some())
                .map(|r| Mailbox {
                    id: r.id,
                    email: r.email,
                })
                .collect(),
            prefix_hint: None,
            can_create: true,
        })
    }

    async fn create(&self, new: &NewAlias) -> Result<RemoteAlias, AliasError> {
        if new.for_domain.is_some() {
            return Err(AliasError::Unsupported);
        }
        let domain = match &new.domain {
            Some(domain) => domain.clone(),
            None => {
                let options = self.domain_options().await?;
                options
                    .default_alias_domain
                    .or_else(|| options.data.into_iter().next())
                    .ok_or_else(|| AliasError::Invalid("no domain to use".into()))?
            }
        };
        let mut body = json!({
            "domain": domain,
            "description": new.note.as_deref().map(text).unwrap_or(Value::Null),
            "format": if new.prefix.is_some() { "custom" } else { "random_characters" },
        });
        if let Some(prefix) = &new.prefix {
            body["local_part"] = json!(prefix.trim());
        }
        if !new.mailbox_ids.is_empty() {
            body["recipient_ids"] = json!(new.mailbox_ids);
        }
        let created: Data<AddyAlias> = self
            .http
            .call(Method::POST, "api/v1/aliases", Some(&body))
            .await
            .map_err(error)?;
        if let Some(name) = &new.name {
            return self
                .update(
                    &created.data.id,
                    &AliasChange {
                        name: Some(name.clone()),
                        ..AliasChange::default()
                    },
                )
                .await;
        }
        Ok(created.data.into())
    }

    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError> {
        if change.for_domain.is_some() {
            return Err(AliasError::Unsupported);
        }
        let id = segment(id)?;
        let current = self.alias(id).await?;
        let mut body = serde_json::Map::new();
        if let Some(note) = &change.note {
            body.insert("description".into(), text(note));
        }
        if let Some(name) = &change.name {
            body.insert("from_name".into(), text(name));
        }
        if !body.is_empty() {
            self.http
                .call::<Value, _>(Method::PATCH, &format!("api/v1/aliases/{id}"), Some(&body))
                .await
                .map_err(error)?;
        }
        if let Some(ids) = &change.mailbox_ids {
            if ids.is_empty() {
                return Err(AliasError::Invalid("choose at least one recipient".into()));
            }
            self.http
                .call::<Value, _>(
                    Method::POST,
                    "api/v1/alias-recipients",
                    Some(&json!({ "alias_id": id, "recipient_ids": ids })),
                )
                .await
                .map_err(error)?;
        }
        match change.enabled {
            Some(true) if !current.enabled => {
                self.http
                    .call::<Value, _>(
                        Method::POST,
                        "api/v1/active-aliases",
                        Some(&json!({ "id": id })),
                    )
                    .await
                    .map_err(error)?;
            }
            Some(false) if current.enabled => {
                self.http
                    .call::<Value, _>(
                        Method::DELETE,
                        &format!("api/v1/active-aliases/{id}"),
                        None::<&()>,
                    )
                    .await
                    .map_err(error)?;
            }
            _ => {}
        }
        self.alias(id).await
    }

    async fn delete(&self, id: &str) -> Result<(), AliasError> {
        self.http
            .call::<Value, _>(
                Method::DELETE,
                &format!("api/v1/aliases/{}", segment(id)?),
                None::<&()>,
            )
            .await
            .map_err(error)?;
        Ok(())
    }
}
