use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use holtmail_core::WebhookId;
use holtmail_store::{Webhook, WebhookDelivery};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::manage::refused;
use crate::session::no_store;
use crate::{ApiError, ApiResult, ApiState};

type AppState = State<Arc<ApiState>>;

const DELIVERIES: u32 = 50;

/// An address the server tells about events.
#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookDto {
    pub id: String,
    pub url: String,
    /// Event names it gets, besides `webhook.test`.
    pub events: Vec<String>,
    pub enabled: bool,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Its newest delivery.
    pub last_delivery: Option<DeliveryDto>,
}

/// One delivery of an event to a webhook.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeliveryDto {
    pub id: u64,
    pub event: String,
    /// The `id` in the body and the `Holtmail-Delivery` header.
    pub event_id: String,
    /// Unix milliseconds.
    pub created_at: i64,
    pub attempts: u32,
    /// Unix milliseconds of the next attempt; `null` once delivered or given up.
    pub next_at: Option<i64>,
    pub last_status: Option<u16>,
    pub last_error: Option<String>,
    /// Unix milliseconds.
    pub delivered_at: Option<i64>,
}

impl From<WebhookDelivery> for DeliveryDto {
    fn from(d: WebhookDelivery) -> Self {
        Self {
            id: d.id,
            event: d.event,
            event_id: d.event_id,
            created_at: d.created_at.as_millis(),
            attempts: d.attempts,
            next_at: d.next_at.map(|t| t.as_millis()),
            last_status: d.last_status,
            last_error: d.last_error,
            delivered_at: d.delivered_at.map(|t| t.as_millis()),
        }
    }
}

fn dto(hook: Webhook, last: Option<WebhookDelivery>) -> WebhookDto {
    WebhookDto {
        id: hook.id.to_string(),
        url: hook.url,
        events: hook.events,
        enabled: hook.enabled,
        created_at: hook.created_at.as_millis(),
        last_delivery: last.map(Into::into),
    }
}

/// The webhooks and the events they can get.
#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookListDto {
    /// Every event name a webhook can subscribe to.
    pub events: Vec<String>,
    pub webhooks: Vec<WebhookDto>,
}

/// Adds a webhook.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewWebhookRequest {
    /// An `http://` or `https://` address without a user name or password.
    pub url: String,
    pub events: Vec<String>,
}

/// A webhook with its signing secret, shown only when it is made.
#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookCreatedDto {
    pub webhook: WebhookDto,
    /// HMAC-SHA256 key for `Holtmail-Signature`; keep it now, it is not shown again.
    pub secret: String,
}

/// Changes a webhook; missing fields stay.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WebhookUpdateRequest {
    pub url: Option<String>,
    pub events: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

/// A new signing secret.
#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookSecretDto {
    pub secret: String,
}

async fn find(state: &ApiState, raw: &str) -> ApiResult<Webhook> {
    let id: WebhookId = raw.parse().map_err(|_| ApiError::not_found("webhook"))?;
    state
        .store
        .get_webhook(id)
        .await?
        .ok_or_else(|| ApiError::not_found("webhook"))
}

#[utoipa::path(
    get,
    path = "/api/webhooks",
    responses((status = 200, body = WebhookListDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list(State(state): AppState) -> ApiResult<Json<WebhookListDto>> {
    let mut webhooks = Vec::new();
    for hook in state.store.list_webhooks().await? {
        let last = state.store.recent_deliveries(hook.id, 1).await?.pop();
        webhooks.push(dto(hook, last));
    }
    Ok(Json(WebhookListDto {
        events: holtmail_hooks::EVENTS.iter().map(|e| (*e).into()).collect(),
        webhooks,
    }))
}

#[utoipa::path(
    post,
    path = "/api/webhooks",
    request_body = NewWebhookRequest,
    responses((status = 201, body = WebhookCreatedDto), (status = 400, description = "A bad address or an unknown event", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn create(
    State(state): AppState,
    Json(request): Json<NewWebhookRequest>,
) -> ApiResult<Response> {
    let (hook, secret) = state
        .operations
        .create_webhook(&request.url, &request.events)
        .await
        .map_err(refused)?;
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(WebhookCreatedDto {
                webhook: dto(hook, None),
                secret,
            }),
        )
            .into_response(),
    ))
}

#[utoipa::path(
    patch,
    path = "/api/webhooks/{id}",
    params(("id" = String, Path, description = "Webhook id")),
    request_body = WebhookUpdateRequest,
    responses((status = 200, body = WebhookDto), (status = 400, description = "A bad address or an unknown event", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such webhook", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<WebhookUpdateRequest>,
) -> ApiResult<Json<WebhookDto>> {
    let mut hook = find(&state, &raw).await?;
    let (url, events) = holtmail_hooks::check(
        request.url.as_deref().unwrap_or(&hook.url),
        request.events.as_deref().unwrap_or(&hook.events),
    )
    .map_err(ApiError::bad_request)?;
    hook.url = url;
    hook.events = events;
    if let Some(enabled) = request.enabled {
        hook.enabled = enabled;
    }
    state.store.update_webhook(&hook).await?;
    let last = state.store.recent_deliveries(hook.id, 1).await?.pop();
    Ok(Json(dto(hook, last)))
}

#[utoipa::path(
    delete,
    path = "/api/webhooks/{id}",
    params(("id" = String, Path, description = "Webhook id")),
    responses((status = 204, description = "Removed with its deliveries"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such webhook", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn remove(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let hook = find(&state, &raw).await?;
    state.store.delete_webhook(hook.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/webhooks/{id}/secret",
    params(("id" = String, Path, description = "Webhook id")),
    responses((status = 200, body = WebhookSecretDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such webhook", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn rotate(State(state): AppState, Path(raw): Path<String>) -> ApiResult<Response> {
    let hook = find(&state, &raw).await?;
    let secret = state
        .operations
        .rotate_webhook_secret(hook.id)
        .await
        .map_err(refused)?;
    Ok(no_store(Json(WebhookSecretDto { secret }).into_response()))
}

#[utoipa::path(
    post,
    path = "/api/webhooks/{id}/test",
    params(("id" = String, Path, description = "Webhook id")),
    responses((status = 202, description = "A webhook.test delivery is queued"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such webhook", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn test(State(state): AppState, Path(raw): Path<String>) -> ApiResult<StatusCode> {
    let hook = find(&state, &raw).await?;
    state
        .operations
        .test_webhook(hook.id)
        .await
        .map_err(refused)?;
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(
    get,
    path = "/api/webhooks/{id}/deliveries",
    params(("id" = String, Path, description = "Webhook id")),
    responses((status = 200, description = "The newest 50 deliveries first", body = [DeliveryDto]), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such webhook", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn deliveries(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<DeliveryDto>>> {
    let hook = find(&state, &raw).await?;
    Ok(Json(
        state
            .store
            .recent_deliveries(hook.id, DELIVERIES)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}
