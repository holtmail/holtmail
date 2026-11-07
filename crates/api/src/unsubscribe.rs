use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use holtmail_core::{MessageId, Timestamp};
use holtmail_fetch::{FetchError, OneClickOutcome};
use holtmail_insight::{InsightKind, Items};
use holtmail_store::{UnsubscribeOutcome, UnsubscribeRequest, UnsubscribeRequestId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::insights::{
    OneClickReasonDto, RETRY_AFTER_MS, UnsubscribeRequestDto, list_key, one_click_reason,
    recently_accepted, unsubscribe_item,
};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

/// The user's consent to one one-click unsubscribe request. Any URL in the body is ignored: the
/// server only uses the link stored with the message.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UnsubscribeConfirmRequest {
    /// Must be `true`.
    pub confirm: bool,
}

/// What a one-click unsubscribe did.
#[derive(Debug, Serialize, ToSchema)]
pub struct UnsubscribeResultDto {
    /// The request, as recorded.
    pub request: UnsubscribeRequestDto,
    /// Nothing was sent: this is the stored request for the same list, accepted or redirected
    /// less than 24 hours ago.
    pub repeated: bool,
}

/// Why a one-click unsubscribe was not sent.
#[derive(Debug, Serialize, ToSchema)]
pub struct UnsubscribeRefusalDto {
    pub error: String,
    /// Why one-click is not available, as `UnsubscribeDto.one_click.reason` gives it; `null`
    /// when the body did not confirm.
    pub reason: Option<OneClickReasonDto>,
}

static REQUESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn refusal(reason: Option<OneClickReasonDto>) -> Response {
    let error = match reason {
        None => "confirm with {\"confirm\": true}".to_owned(),
        Some(reason) => format!(
            "one-click unsubscribe is not available: {}",
            serde_json::to_value(reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
    };
    (
        StatusCode::BAD_REQUEST,
        Json(UnsubscribeRefusalDto { error, reason }),
    )
        .into_response()
}

fn answered(request: &UnsubscribeRequest, repeated: bool) -> Response {
    Json(UnsubscribeResultDto {
        request: request.into(),
        repeated,
    })
    .into_response()
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/unsubscribe",
    params(("id" = String, Path, description = "Message id")),
    request_body = UnsubscribeConfirmRequest,
    responses(
        (status = 200, description = "Sent and recorded; or, with `repeated`, nothing sent and the stored request for the same list returned, because it was accepted or redirected less than 24 hours ago", body = UnsubscribeResultDto),
        (status = 400, description = "The body does not confirm, or one-click is not available for this message (`reason`)", body = UnsubscribeRefusalDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not an owner or admin", body = ErrorDto),
        (status = 404, description = "No such message, or the server computes no insights", body = ErrorDto),
        (status = 429, description = "A request for the same list failed less than a minute ago; `Retry-After` gives the seconds to wait", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn unsubscribe(
    State(state): State<Arc<ApiState>>,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    body: Result<Json<UnsubscribeConfirmRequest>, JsonRejection>,
) -> ApiResult<Response> {
    if !crate::access::manages(&user) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner and admins can do this".into(),
        ));
    }
    let id: MessageId = raw.parse().map_err(|_| ApiError::not_found("message"))?;
    let message = state
        .store
        .get_message(id)
        .await?
        .ok_or_else(|| ApiError::not_found("message"))?;
    let config = state
        .insights
        .as_ref()
        .ok_or_else(|| ApiError::not_found("insights"))?;
    if !matches!(body, Ok(Json(UnsubscribeConfirmRequest { confirm: true }))) {
        return Ok(refusal(None));
    }
    state
        .operations
        .fresh_insights(id)
        .await
        .map_err(crate::manage::refused)?;
    let _one_at_a_time = REQUESTS.lock().await;
    let view = state.store.insight_view(id).await?;
    let items = view
        .as_ref()
        .and_then(|v| Items::from_json(&v.insights.items).ok())
        .map(|read| read.items.items)
        .unwrap_or_default();
    let (https, declared, list_id) = match unsubscribe_item(config, &items).map(|i| &i.kind) {
        Some(InsightKind::Unsubscribe {
            https,
            one_click,
            list_id,
            ..
        }) => (https.as_ref(), *one_click, list_id.as_deref()),
        _ => (None, false, None),
    };
    let categories = view.as_ref().and_then(|v| v.categories.as_ref());
    let now = Timestamp::now();
    if let Some(reason) = one_click_reason(
        view.as_ref(),
        categories,
        declared,
        https,
        config.one_click.is_some(),
        None,
        now,
    ) {
        return Ok(refusal(Some(reason)));
    }
    let (Some(url), Some(sender), Some(key)) = (
        https,
        config.one_click.as_ref(),
        list_key(list_id, categories),
    ) else {
        return Ok(refusal(Some(OneClickReasonDto::NotAligned)));
    };
    let archive = message.archive_id;
    if let Some(last) = state.store.last_unsubscribe(archive, &key).await? {
        if recently_accepted(&last, now) {
            tracing::info!(%archive, message = %id, user = %user.id, outcome = last.outcome.as_str(), "one-click unsubscribe already requested");
            return Ok(answered(&last, true));
        }
        let age = now
            .as_millis()
            .saturating_sub(last.requested_at.as_millis());
        if age < RETRY_AFTER_MS {
            let wait = (RETRY_AFTER_MS - age.max(0) + 999) / 1000;
            let mut response = ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "the last request for this list failed less than a minute ago".into(),
            )
            .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(wait.max(1)));
            return Ok(response);
        }
    }
    let (outcome, status) = match sender.post(url.as_str()).await {
        Ok(response) => (
            match response.outcome {
                OneClickOutcome::Accepted => UnsubscribeOutcome::Accepted,
                OneClickOutcome::Redirected => UnsubscribeOutcome::Redirected,
                OneClickOutcome::HttpError => UnsubscribeOutcome::HttpError,
            },
            Some(response.status),
        ),
        Err(FetchError::Refused(_)) => (UnsubscribeOutcome::Refused, None),
        Err(FetchError::Unavailable(_)) => (UnsubscribeOutcome::Network, None),
    };
    let request = UnsubscribeRequest {
        id: UnsubscribeRequestId::new(),
        archive,
        message: Some(id),
        list_key: key,
        requested_at: Timestamp::now(),
        requested_by: user.id,
        outcome,
        status,
    };
    state.store.record_unsubscribe(&request).await?;
    tracing::info!(%archive, message = %id, user = %user.id, outcome = outcome.as_str(), status = ?status, "one-click unsubscribe sent");
    Ok(answered(&request, false))
}
