use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use holtmail_alias::{AliasChange, AliasOptions, Capabilities, NewAlias};
use holtmail_core::{AliasServiceId, ArchiveId, MessageId};
use holtmail_store::{AliasService, AliasSummary, KnownAlias, MessageAlias};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::{AddressDto, ErrorDto};
use crate::manage::{archive, refused};
use crate::{ApiError, ApiResult, ApiState, MailboxError};

type AppState = State<Arc<ApiState>>;

/// The longest note, name or website kept for an alias.
const MAX_TEXT: usize = 500;

/// An alias service the server knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasProvider {
    /// The provider id.
    pub id: String,
    /// What people call it.
    pub name: String,
    /// Its address when the owner gives none; `None` when it has no API.
    pub default_url: Option<String>,
    /// What its API can do; `None` when there is no API, so its aliases are only recognised in
    /// mail or added by hand.
    pub capabilities: Option<Capabilities>,
    /// What the key is called at the service, e.g. "API key".
    pub key_label: String,
    /// Where the owner finds the key.
    pub key_help: String,
    /// Mail it forwarded is recognised.
    pub detects: bool,
}

/// A service account to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewService {
    /// A provider id from [`Aliases::providers`] that has an API.
    pub provider: String,
    /// A name; the provider's name when empty.
    pub name: String,
    /// The service's address; the provider's default when `None`.
    pub base_url: Option<String>,
    /// The API key or token.
    pub key: String,
}

/// Changes to a connected service; `None` leaves a field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceChange {
    /// A new name.
    pub name: Option<String>,
    /// A new address.
    pub base_url: Option<String>,
    /// A new key.
    pub key: Option<String>,
}

/// Alias services: connecting accounts at them and changing their aliases there. Implemented by
/// the server, which holds the keys.
#[async_trait]
pub trait Aliases: Send + Sync {
    /// Every provider the server knows, those with an API first.
    fn providers(&self) -> Vec<AliasProvider>;

    /// Checks the key with the service, stores the service with its key sealed and lists its
    /// aliases once. `Invalid` for an unknown provider, a refused key or an unreachable service;
    /// nothing is stored then.
    async fn add_service(
        &self,
        archive: ArchiveId,
        service: NewService,
    ) -> Result<AliasService, MailboxError>;

    /// Changes a service. A new key or address is checked with the service first. `Invalid` for
    /// an unknown service or a refused key.
    async fn update_service(
        &self,
        id: AliasServiceId,
        change: ServiceChange,
    ) -> Result<AliasService, MailboxError>;

    /// Lists the service's aliases now and stores them. A failure is kept on the service as
    /// `sync_error` and also returned as `Invalid`.
    async fn sync_service(&self, id: AliasServiceId) -> Result<AliasService, MailboxError>;

    /// What the service offers for new aliases, with what it can do.
    async fn alias_options(
        &self,
        id: AliasServiceId,
    ) -> Result<(Capabilities, AliasOptions), MailboxError>;

    /// Creates an alias at the service and stores it as known.
    async fn create_alias(
        &self,
        id: AliasServiceId,
        alias: NewAlias,
    ) -> Result<KnownAlias, MailboxError>;

    /// Changes a known alias: at its service when it has one (fields the service cannot keep
    /// are `Invalid`), else only the note of an alias added by hand. `Invalid` for an unknown
    /// alias.
    async fn update_alias(
        &self,
        archive: ArchiveId,
        address: &str,
        change: AliasChange,
    ) -> Result<KnownAlias, MailboxError>;

    /// Forgets a known alias. With `at_service`, deletes it at its service first, so mail to it
    /// is no longer forwarded. Its mail stays archived. `Invalid` for an unknown alias.
    async fn delete_alias(
        &self,
        archive: ArchiveId,
        address: &str,
        at_service: bool,
    ) -> Result<(), MailboxError>;
}

/// What an alias service API can do.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct CapabilitiesDto {
    pub create_random: bool,
    pub create_custom: bool,
    pub note: bool,
    pub name: bool,
    pub for_domain: bool,
    pub switch: bool,
    pub mailboxes: bool,
    pub delete: bool,
}

impl From<Capabilities> for CapabilitiesDto {
    fn from(c: Capabilities) -> Self {
        Self {
            create_random: c.create_random,
            create_custom: c.create_custom,
            note: c.note,
            name: c.name,
            for_domain: c.for_domain,
            switch: c.switch,
            mailboxes: c.mailboxes,
            delete: c.delete,
        }
    }
}

/// An alias service the server knows.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasProviderDto {
    pub id: String,
    pub name: String,
    pub default_url: Option<String>,
    /// What its API can do; `null` when it has none.
    pub capabilities: Option<CapabilitiesDto>,
    pub key_label: String,
    pub key_help: String,
    /// Mail it forwarded is recognised.
    pub detects: bool,
}

/// An account at an alias service.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasServiceDto {
    pub id: String,
    pub archive_id: String,
    pub provider: String,
    pub name: String,
    pub base_url: String,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds of the last listing.
    pub synced_at: Option<i64>,
    /// Why the last listing failed.
    pub sync_error: Option<String>,
    /// What its API can do.
    pub capabilities: Option<CapabilitiesDto>,
}

/// An account receiving an alias's mail.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasAccountDto {
    pub account_id: String,
    pub name: String,
    pub address: Option<String>,
    pub messages: u64,
}

/// One alias of an archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasDto {
    pub address: String,
    pub provider: String,
    /// `service`: listed by a connected service; `manual`: added by hand; `mail`: only seen in
    /// mail.
    pub source: String,
    pub service_id: Option<String>,
    /// Forwarding is on, when known.
    pub enabled: Option<bool>,
    pub note: Option<String>,
    pub name: Option<String>,
    pub for_domain: Option<String>,
    /// Addresses the service forwards it to.
    pub mailboxes: Vec<String>,
    /// Unix milliseconds of when the service made it.
    pub created_at: Option<i64>,
    /// Counts from the service.
    pub forwarded: Option<u64>,
    pub blocked: Option<u64>,
    pub replied: Option<u64>,
    /// Archived messages forwarded or addressed to it.
    pub messages: u64,
    pub senders: u64,
    /// Forwarded messages with the provider's passing signature.
    pub signed: u64,
    pub first_at: Option<i64>,
    pub last_at: Option<i64>,
    /// Accounts its mail was archived from, most first.
    pub accounts: Vec<AliasAccountDto>,
}

/// The aliases and alias services of an archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasListDto {
    pub aliases: Vec<AliasDto>,
    pub services: Vec<AliasServiceDto>,
}

/// The alias a message was forwarded to.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessageAliasDto {
    pub provider: String,
    pub alias: String,
    /// Who really sent it, when the service hid them.
    pub sender: Option<AddressDto>,
    /// The address that reaches the sender through the service.
    pub reverse_alias: Option<String>,
    /// The provider's signature passed; without it the forwarding fields may be forged.
    pub signed: bool,
    /// The alias is known: listed by a service or added by hand.
    pub known: bool,
    pub note: Option<String>,
}

/// Connects an account at an alias service.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewAliasServiceRequest {
    pub provider: String,
    pub name: Option<String>,
    /// Another address, for a self-hosted service.
    pub base_url: Option<String>,
    pub key: String,
}

/// Changes a service; missing fields stay.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AliasServiceUpdateRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub key: Option<String>,
}

/// Adds an alias by hand, e.g. an Apple Hide My Email address.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ManualAliasRequest {
    pub address: String,
    /// A provider id, or any lowercase name.
    pub provider: String,
    pub note: Option<String>,
    pub for_domain: Option<String>,
}

/// Creates an alias at a service. Without `prefix` the service picks it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAliasRequest {
    pub prefix: Option<String>,
    /// A domain id from the options.
    pub domain: Option<String>,
    pub note: Option<String>,
    pub name: Option<String>,
    pub for_domain: Option<String>,
    #[serde(default)]
    pub mailbox_ids: Vec<String>,
}

/// Changes an alias; missing fields stay, an empty string clears.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AliasUpdateRequest {
    pub enabled: Option<bool>,
    pub note: Option<String>,
    pub name: Option<String>,
    pub for_domain: Option<String>,
    pub mailbox_ids: Option<Vec<String>>,
}

/// A domain or suffix for new aliases.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasDomainDto {
    pub id: String,
    pub label: String,
}

/// A mailbox an alias can forward to.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasMailboxDto {
    pub id: String,
    pub email: String,
}

/// Choices for a new alias at a service.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasOptionsDto {
    pub capabilities: CapabilitiesDto,
    pub domains: Vec<AliasDomainDto>,
    pub mailboxes: Vec<AliasMailboxDto>,
    pub prefix_hint: Option<String>,
    pub can_create: bool,
}

/// Whether a service's aliases stay when it is removed.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RemoveServiceQuery {
    /// Keep its aliases as aliases added by hand.
    #[serde(default)]
    pub keep_aliases: bool,
}

/// Whether an alias is deleted at its service too.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RemoveAliasQuery {
    /// Delete it at the service, so its mail is no longer forwarded.
    #[serde(default)]
    pub at_service: bool,
}

fn millis(t: Option<holtmail_core::Timestamp>) -> Option<i64> {
    t.map(|t| t.as_millis())
}

fn capabilities_of(state: &ApiState, provider: &str) -> Option<CapabilitiesDto> {
    state
        .aliases
        .providers()
        .into_iter()
        .find(|p| p.id == provider)
        .and_then(|p| p.capabilities)
        .map(Into::into)
}

pub(crate) fn service_dto(state: &ApiState, s: AliasService) -> AliasServiceDto {
    AliasServiceDto {
        capabilities: capabilities_of(state, &s.provider),
        id: s.id.to_string(),
        archive_id: s.archive_id.to_string(),
        provider: s.provider,
        name: s.name,
        base_url: s.base_url,
        created_at: s.created_at.as_millis(),
        synced_at: millis(s.synced_at),
        sync_error: s.sync_error,
    }
}

async fn alias_dto(state: &ApiState, summary: AliasSummary) -> ApiResult<AliasDto> {
    let known = summary.known.clone();
    let source = match &known {
        Some(k) if k.service_id.is_some() => "service",
        Some(_) => "manual",
        None => "mail",
    };
    let mut accounts = Vec::new();
    for (account, messages) in summary.mail.accounts {
        if let Some(a) = state.store.get_account(account).await? {
            accounts.push(AliasAccountDto {
                account_id: a.id.to_string(),
                name: a.name,
                address: a.address,
                messages,
            });
        }
    }
    Ok(AliasDto {
        address: summary.address,
        provider: summary.provider,
        source: source.into(),
        service_id: known
            .as_ref()
            .and_then(|k| k.service_id)
            .map(|id| id.to_string()),
        enabled: known.as_ref().and_then(|k| k.enabled),
        note: known.as_ref().and_then(|k| k.note.clone()),
        name: known.as_ref().and_then(|k| k.name.clone()),
        for_domain: known.as_ref().and_then(|k| k.for_domain.clone()),
        mailboxes: known
            .as_ref()
            .map(|k| k.mailboxes.clone())
            .unwrap_or_default(),
        created_at: millis(known.as_ref().and_then(|k| k.created_at)),
        forwarded: known.as_ref().and_then(|k| k.forwarded),
        blocked: known.as_ref().and_then(|k| k.blocked),
        replied: known.as_ref().and_then(|k| k.replied),
        messages: summary.mail.messages,
        senders: summary.mail.senders,
        signed: summary.mail.signed,
        first_at: millis(summary.mail.first_at),
        last_at: millis(summary.mail.last_at),
        accounts,
    })
}

/// The alias of a message: what detection found, else a known alias among its recipients.
pub(crate) async fn message_alias(
    state: &ApiState,
    archive: ArchiveId,
    message: MessageId,
    recipients: &[String],
) -> ApiResult<Option<MessageAliasDto>> {
    let found: Option<MessageAlias> = state.store.message_alias(message).await?;
    let wanted: Vec<String> = match &found {
        Some(hit) => vec![hit.alias.clone()],
        None => recipients.to_vec(),
    };
    let known = state
        .store
        .known_aliases_among(archive, &wanted)
        .await?
        .into_iter()
        .next();
    Ok(match (found, known) {
        (Some(hit), known) => Some(MessageAliasDto {
            provider: hit.provider,
            alias: hit.alias,
            sender: hit.sender.map(|s| AddressDto {
                name: s.name,
                email: s.email,
            }),
            reverse_alias: hit.reverse_alias,
            signed: hit.signed,
            note: known.as_ref().and_then(|k| k.note.clone()),
            known: known.is_some(),
        }),
        (None, Some(known)) => Some(MessageAliasDto {
            provider: known.provider,
            alias: known.address,
            sender: None,
            reverse_alias: None,
            signed: false,
            known: true,
            note: known.note,
        }),
        (None, None) => None,
    })
}

fn people_later(state: &ApiState, archive: ArchiveId) {
    let mailboxes = state.mailboxes.clone();
    tokio::spawn(async move {
        let _ = mailboxes.rebuild_people(archive).await;
    });
}

fn service_id(raw: &str) -> ApiResult<AliasServiceId> {
    raw.parse()
        .map_err(|_| ApiError::not_found("alias service"))
}

async fn find_service(state: &ApiState, raw: &str) -> ApiResult<AliasService> {
    state
        .store
        .get_alias_service(service_id(raw)?)
        .await?
        .ok_or_else(|| ApiError::not_found("alias service"))
}

fn text(value: Option<String>, what: &str) -> ApiResult<Option<String>> {
    match value {
        Some(v) if v.chars().count() > MAX_TEXT => Err(ApiError::bad_request(format!(
            "the {what} is longer than {MAX_TEXT} characters"
        ))),
        Some(v) => Ok(Some(v.trim().to_owned())),
        None => Ok(None),
    }
}

fn address(raw: &str) -> ApiResult<String> {
    let address = raw.trim().to_ascii_lowercase();
    match address.rsplit_once('@') {
        Some((local, domain))
            if !local.is_empty()
                && domain.contains('.')
                && !address
                    .chars()
                    .any(|c| c.is_whitespace() || c == '<' || c == '>') =>
        {
            Ok(address)
        }
        _ => Err(ApiError::bad_request(format!("{raw:?} is not an address"))),
    }
}

#[utoipa::path(
    get,
    path = "/api/alias-providers",
    responses((status = 200, body = [AliasProviderDto]), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn alias_providers(State(state): AppState) -> Json<Vec<AliasProviderDto>> {
    Json(
        state
            .aliases
            .providers()
            .into_iter()
            .map(|p| AliasProviderDto {
                id: p.id,
                name: p.name,
                default_url: p.default_url,
                capabilities: p.capabilities.map(Into::into),
                key_label: p.key_label,
                key_help: p.key_help,
                detects: p.detects,
            })
            .collect(),
    )
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/aliases",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, body = AliasListDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_aliases(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<AliasListDto>> {
    let archive = archive(&state, &raw).await?;
    let mut aliases = Vec::new();
    for summary in state.store.aliases(archive).await? {
        aliases.push(alias_dto(&state, summary).await?);
    }
    let services = state
        .store
        .alias_services(archive)
        .await?
        .into_iter()
        .map(|s| service_dto(&state, s))
        .collect();
    Ok(Json(AliasListDto { aliases, services }))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/alias-services",
    params(("id" = String, Path, description = "Archive id")),
    request_body = NewAliasServiceRequest,
    responses((status = 201, body = AliasServiceDto), (status = 400, description = "An unknown provider, a refused key or an unreachable service", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn add_alias_service(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<NewAliasServiceRequest>,
) -> ApiResult<(StatusCode, Json<AliasServiceDto>)> {
    let archive = archive(&state, &raw).await?;
    if request.key.trim().is_empty() {
        return Err(ApiError::bad_request("the key is empty"));
    }
    let service = state
        .aliases
        .add_service(
            archive,
            NewService {
                provider: request.provider,
                name: text(request.name, "name")?.unwrap_or_default(),
                base_url: request
                    .base_url
                    .map(|u| u.trim().to_owned())
                    .filter(|u| !u.is_empty()),
                key: request.key,
            },
        )
        .await
        .map_err(refused)?;
    Ok((StatusCode::CREATED, Json(service_dto(&state, service))))
}

#[utoipa::path(
    patch,
    path = "/api/alias-services/{id}",
    params(("id" = String, Path, description = "Alias service id")),
    request_body = AliasServiceUpdateRequest,
    responses((status = 200, body = AliasServiceDto), (status = 400, description = "A refused key or an unreachable service", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such alias service", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_alias_service(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<AliasServiceUpdateRequest>,
) -> ApiResult<Json<AliasServiceDto>> {
    let service = find_service(&state, &raw).await?;
    let updated = state
        .aliases
        .update_service(
            service.id,
            ServiceChange {
                name: text(request.name, "name")?.filter(|n| !n.is_empty()),
                base_url: request.base_url.map(|u| u.trim().to_owned()),
                key: request.key.filter(|k| !k.trim().is_empty()),
            },
        )
        .await
        .map_err(refused)?;
    Ok(Json(service_dto(&state, updated)))
}

#[utoipa::path(
    delete,
    path = "/api/alias-services/{id}",
    params(("id" = String, Path, description = "Alias service id"), RemoveServiceQuery),
    responses((status = 204, description = "Removed; nothing changes at the service"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such alias service", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn remove_alias_service(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(query): Query<RemoveServiceQuery>,
) -> ApiResult<StatusCode> {
    let service = find_service(&state, &raw).await?;
    state
        .store
        .delete_alias_service(service.id, query.keep_aliases)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/alias-services/{id}/sync",
    params(("id" = String, Path, description = "Alias service id")),
    responses((status = 200, body = AliasServiceDto), (status = 400, description = "The service refused or could not be reached; kept as sync_error", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such alias service", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn sync_alias_service(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<AliasServiceDto>> {
    let service = find_service(&state, &raw).await?;
    let synced = state
        .aliases
        .sync_service(service.id)
        .await
        .map_err(refused)?;
    Ok(Json(service_dto(&state, synced)))
}

#[utoipa::path(
    get,
    path = "/api/alias-services/{id}/options",
    params(("id" = String, Path, description = "Alias service id")),
    responses((status = 200, body = AliasOptionsDto), (status = 400, description = "The service refused or could not be reached", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such alias service", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn alias_options(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<AliasOptionsDto>> {
    let service = find_service(&state, &raw).await?;
    let (capabilities, options) = state
        .aliases
        .alias_options(service.id)
        .await
        .map_err(refused)?;
    Ok(Json(AliasOptionsDto {
        capabilities: capabilities.into(),
        domains: options
            .domains
            .into_iter()
            .map(|d| AliasDomainDto {
                id: d.id,
                label: d.label,
            })
            .collect(),
        mailboxes: options
            .mailboxes
            .into_iter()
            .map(|m| AliasMailboxDto {
                id: m.id,
                email: m.email,
            })
            .collect(),
        prefix_hint: options.prefix_hint,
        can_create: options.can_create,
    }))
}

async fn summary_of(state: &ApiState, archive: ArchiveId, address: &str) -> ApiResult<AliasDto> {
    let summary = state
        .store
        .aliases(archive)
        .await?
        .into_iter()
        .find(|s| s.address == address)
        .ok_or_else(|| ApiError::not_found("alias"))?;
    alias_dto(state, summary).await
}

#[utoipa::path(
    post,
    path = "/api/alias-services/{id}/aliases",
    params(("id" = String, Path, description = "Alias service id")),
    request_body = CreateAliasRequest,
    responses((status = 201, body = AliasDto), (status = 400, description = "The service refused the alias", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such alias service", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn create_alias(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<CreateAliasRequest>,
) -> ApiResult<(StatusCode, Json<AliasDto>)> {
    let service = find_service(&state, &raw).await?;
    let made = state
        .aliases
        .create_alias(
            service.id,
            NewAlias {
                prefix: request
                    .prefix
                    .map(|p| p.trim().to_owned())
                    .filter(|p| !p.is_empty()),
                domain: request.domain.filter(|d| !d.is_empty()),
                note: text(request.note, "note")?.filter(|n| !n.is_empty()),
                name: text(request.name, "name")?.filter(|n| !n.is_empty()),
                for_domain: text(request.for_domain, "website")?.filter(|n| !n.is_empty()),
                mailbox_ids: request.mailbox_ids,
            },
        )
        .await
        .map_err(refused)?;
    Ok((
        StatusCode::CREATED,
        Json(summary_of(&state, service.archive_id, &made.address).await?),
    ))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/aliases",
    params(("id" = String, Path, description = "Archive id")),
    request_body = ManualAliasRequest,
    responses((status = 201, body = AliasDto), (status = 400, description = "Not an address, or a known alias", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn add_manual_alias(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<ManualAliasRequest>,
) -> ApiResult<(StatusCode, Json<AliasDto>)> {
    let archive = archive(&state, &raw).await?;
    let address = address(&request.address)?;
    let provider = request.provider.trim().to_ascii_lowercase();
    if provider.is_empty()
        || provider.len() > 40
        || !provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ApiError::bad_request(
            "the provider takes lowercase letters, digits and -",
        ));
    }
    if !state
        .store
        .known_aliases_among(archive, std::slice::from_ref(&address))
        .await?
        .is_empty()
    {
        return Err(ApiError::bad_request(format!("{address} is already known")));
    }
    state
        .store
        .put_alias(
            archive,
            &KnownAlias {
                address: address.clone(),
                provider,
                service_id: None,
                remote_id: None,
                enabled: None,
                note: text(request.note, "note")?.filter(|n| !n.is_empty()),
                name: None,
                for_domain: text(request.for_domain, "website")?.filter(|n| !n.is_empty()),
                mailboxes: Vec::new(),
                created_at: None,
                forwarded: None,
                blocked: None,
                replied: None,
            },
        )
        .await?;
    people_later(&state, archive);
    Ok((
        StatusCode::CREATED,
        Json(summary_of(&state, archive, &address).await?),
    ))
}

#[utoipa::path(
    patch,
    path = "/api/archives/{id}/aliases/{address}",
    params(("id" = String, Path, description = "Archive id"), ("address" = String, Path, description = "The alias")),
    request_body = AliasUpdateRequest,
    responses((status = 200, body = AliasDto), (status = 400, description = "The service refused the change or cannot make it", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive or alias", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_alias(
    State(state): AppState,
    Path((raw, alias)): Path<(String, String)>,
    Json(request): Json<AliasUpdateRequest>,
) -> ApiResult<Json<AliasDto>> {
    let archive = archive(&state, &raw).await?;
    let alias = alias.trim().to_ascii_lowercase();
    let change = AliasChange {
        enabled: request.enabled,
        note: text(request.note, "note")?,
        name: text(request.name, "name")?,
        for_domain: text(request.for_domain, "website")?,
        mailbox_ids: request.mailbox_ids,
    };
    if change.is_empty() {
        return Err(ApiError::bad_request("nothing to change"));
    }
    state
        .aliases
        .update_alias(archive, &alias, change)
        .await
        .map_err(refused)?;
    Ok(Json(summary_of(&state, archive, &alias).await?))
}

#[utoipa::path(
    delete,
    path = "/api/archives/{id}/aliases/{address}",
    params(("id" = String, Path, description = "Archive id"), ("address" = String, Path, description = "The alias"), RemoveAliasQuery),
    responses((status = 204, description = "Forgotten; its mail stays"), (status = 400, description = "The service refused to delete it", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn remove_alias(
    State(state): AppState,
    Path((raw, alias)): Path<(String, String)>,
    Query(query): Query<RemoveAliasQuery>,
) -> ApiResult<StatusCode> {
    let archive = archive(&state, &raw).await?;
    state
        .aliases
        .delete_alias(
            archive,
            &alias.trim().to_ascii_lowercase(),
            query.at_service,
        )
        .await
        .map_err(refused)?;
    people_later(&state, archive);
    Ok(StatusCode::NO_CONTENT)
}
