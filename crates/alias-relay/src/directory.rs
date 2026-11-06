use std::time::Duration;

use async_trait::async_trait;
use holtmail_alias::{
    AliasChange, AliasDirectory, AliasError, AliasOptions, Capabilities, NewAlias, RemoteAlias,
};
use holtmail_core::{Timestamp, TlsRoots};
use holtmail_json_http::{HttpError, JsonHttp, Method, utc_millis};
use serde::Deserialize;
use serde_json::{Value, json};

/// The hosted service.
pub const DEFAULT_URL: &str = "https://relay.firefox.com";

/// One Firefox Relay account, reached with the API key from its settings. Lists random masks
/// and, for premium accounts, masks on the account's own subdomain; creates random masks;
/// switches, edits (label, website) and deletes masks.
#[derive(Clone)]
pub struct RelayDirectory {
    http: JsonHttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Random,
    Domain,
}

impl Kind {
    fn path(self) -> &'static str {
        match self {
            Kind::Random => "api/v1/relayaddresses/",
            Kind::Domain => "api/v1/domainaddresses/",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Kind::Random => "r",
            Kind::Domain => "d",
        }
    }
}

fn parse_id(id: &str) -> Result<(Kind, u64), AliasError> {
    let (kind, number) = id.split_once(':').ok_or(AliasError::NotFound)?;
    let kind = match kind {
        "r" => Kind::Random,
        "d" => Kind::Domain,
        _ => return Err(AliasError::NotFound),
    };
    Ok((kind, number.parse().map_err(|_| AliasError::NotFound)?))
}

impl RelayDirectory {
    /// A directory for the account of `api_key` at `base_url` ([`DEFAULT_URL`] or a self-hosted
    /// Relay), trusting `roots` besides the built-in ones.
    pub fn new(base_url: &str, api_key: &str, roots: &TlsRoots) -> Result<Self, AliasError> {
        let token = format!("Token {}", api_key.trim());
        let http = JsonHttp::new(
            base_url,
            &[("Authorization", token.as_str())],
            Duration::from_secs(30),
            roots,
        )
        .map_err(error)?;
        Ok(Self { http })
    }

    async fn list(&self, kind: Kind) -> Result<Vec<RemoteAlias>, AliasError> {
        let masks: Vec<Mask> = self.http.get(kind.path()).await.map_err(error)?;
        Ok(masks.into_iter().map(|m| m.into_alias(kind)).collect())
    }
}

fn text(value: &str) -> Value {
    json!(value.trim())
}

pub(crate) fn error(e: HttpError) -> AliasError {
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
struct Mask {
    id: u64,
    full_address: String,
    enabled: bool,
    description: Option<String>,
    generated_for: Option<String>,
    used_on: Option<String>,
    created_at: Option<String>,
    num_forwarded: Option<u64>,
    num_blocked: Option<u64>,
    num_replied: Option<u64>,
}

impl Mask {
    fn into_alias(self, kind: Kind) -> RemoteAlias {
        let for_domain = self
            .generated_for
            .filter(|g| !g.trim().is_empty())
            .or_else(|| {
                self.used_on.and_then(|u| {
                    u.split(',')
                        .map(str::trim)
                        .find(|s| !s.is_empty())
                        .map(str::to_owned)
                })
            });
        RemoteAlias {
            id: format!("{}:{}", kind.prefix(), self.id),
            address: self.full_address.trim().to_ascii_lowercase(),
            enabled: self.enabled,
            note: self.description.filter(|d| !d.trim().is_empty()),
            name: None,
            for_domain,
            mailboxes: Vec::new(),
            created_at: self
                .created_at
                .as_deref()
                .and_then(utc_millis)
                .map(Timestamp::from_millis),
            forwarded: self.num_forwarded,
            blocked: self.num_blocked,
            replied: self.num_replied,
        }
    }
}

#[async_trait]
impl AliasDirectory for RelayDirectory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            create_random: true,
            create_custom: false,
            note: true,
            name: false,
            for_domain: true,
            switch: true,
            mailboxes: false,
            delete: true,
        }
    }

    async fn aliases(&self) -> Result<Vec<RemoteAlias>, AliasError> {
        let mut all = self.list(Kind::Random).await?;
        match self.list(Kind::Domain).await {
            Ok(domain) => all.extend(domain),
            Err(AliasError::NotFound | AliasError::Auth) => {}
            Err(other) => return Err(other),
        }
        Ok(all)
    }

    async fn options(&self) -> Result<AliasOptions, AliasError> {
        self.http
            .get::<Vec<Value>>(Kind::Random.path())
            .await
            .map_err(error)?;
        Ok(AliasOptions {
            can_create: true,
            ..AliasOptions::default()
        })
    }

    async fn create(&self, new: &NewAlias) -> Result<RemoteAlias, AliasError> {
        if new.prefix.is_some() || new.name.is_some() || !new.mailbox_ids.is_empty() {
            return Err(AliasError::Unsupported);
        }
        let mut body = json!({ "enabled": true });
        if let Some(note) = &new.note {
            body["description"] = text(note);
        }
        if let Some(site) = &new.for_domain {
            body["generated_for"] = text(site);
        }
        let mask: Mask = self
            .http
            .call(Method::POST, Kind::Random.path(), Some(&body))
            .await
            .map_err(error)?;
        Ok(mask.into_alias(Kind::Random))
    }

    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError> {
        if change.name.is_some() || change.mailbox_ids.is_some() {
            return Err(AliasError::Unsupported);
        }
        let (kind, number) = parse_id(id)?;
        let mut body = serde_json::Map::new();
        if let Some(enabled) = change.enabled {
            body.insert("enabled".into(), json!(enabled));
        }
        if let Some(note) = &change.note {
            body.insert("description".into(), text(note));
        }
        if let Some(site) = &change.for_domain {
            let field = if kind == Kind::Random {
                "generated_for"
            } else {
                "used_on"
            };
            body.insert(field.into(), text(site));
        }
        let path = format!("{}{number}/", kind.path());
        let mask: Mask = if body.is_empty() {
            self.http.get(&path).await.map_err(error)?
        } else {
            self.http
                .call(Method::PATCH, &path, Some(&body))
                .await
                .map_err(error)?
        };
        Ok(mask.into_alias(kind))
    }

    async fn delete(&self, id: &str) -> Result<(), AliasError> {
        let (kind, number) = parse_id(id)?;
        self.http
            .call::<Value, _>(
                Method::DELETE,
                &format!("{}{number}/", kind.path()),
                None::<&()>,
            )
            .await
            .map_err(error)?;
        Ok(())
    }
}
