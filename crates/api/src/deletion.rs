use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_core::MessageId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::{AddressDto, ErrorDto, TaskDto};
use crate::manage::{archive, refused};
use crate::{ApiError, ApiResult, ApiState, CurrentUser, DeletionReview, DeletionSelection};

type AppState = State<Arc<ApiState>>;

/// What to delete: a search or a list of messages, exactly one of them.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DeletionRequest {
    /// A search as the search box reads it, e.g. `from:news@example.com before:2020-01-01`.
    #[serde(default)]
    pub query: Option<String>,
    /// Message ids; those not in the archive are left out.
    #[serde(default)]
    pub message_ids: Option<Vec<String>>,
}

/// One of the messages a deletion would remove.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeletionSampleDto {
    pub id: String,
    pub subject: Option<String>,
    pub from: Vec<AddressDto>,
    /// Unix milliseconds.
    pub date: i64,
    pub size: u64,
}

/// A frozen list of messages waiting to be deleted.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeletionReviewDto {
    /// Confirm with `POST /api/deletions/{id}/confirm`.
    pub id: String,
    pub archive_id: String,
    pub query: Option<String>,
    /// Messages it deletes; the confirmation repeats this number.
    pub count: u64,
    /// Their size together, in bytes.
    pub bytes: u64,
    /// Unix milliseconds.
    pub oldest: Option<i64>,
    /// Unix milliseconds.
    pub newest: Option<i64>,
    /// Up to 20 of them, newest first.
    pub sample: Vec<DeletionSampleDto>,
    /// Unix milliseconds; after this the review cannot be confirmed.
    pub expires_at: i64,
}

impl From<DeletionReview> for DeletionReviewDto {
    fn from(review: DeletionReview) -> Self {
        Self {
            id: review.id,
            archive_id: review.archive_id.to_string(),
            query: review.query,
            count: review.count,
            bytes: review.bytes,
            oldest: review.oldest.map(|t| t.as_millis()),
            newest: review.newest.map(|t| t.as_millis()),
            sample: review
                .sample
                .into_iter()
                .map(|m| DeletionSampleDto {
                    id: m.id.to_string(),
                    subject: m.subject,
                    from: m.from.iter().map(Into::into).collect(),
                    date: m.date.as_millis(),
                    size: m.size,
                })
                .collect(),
            expires_at: review.expires_at.as_millis(),
        }
    }
}

/// Confirms a review.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DeletionConfirmRequest {
    /// The review's `count`, typed again by the person deleting.
    pub count: u64,
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/deletions",
    params(("id" = String, Path, description = "Archive id")),
    request_body = DeletionRequest,
    responses((status = 200, description = "What would be deleted; nothing is deleted yet", body = DeletionReviewDto), (status = 400, description = "Neither or both of query and message_ids, a query that does not parse or is empty, nothing found, or more than 100000 messages", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn review(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<DeletionRequest>,
) -> ApiResult<Json<DeletionReviewDto>> {
    let archive_id = archive(&state, &raw).await?;
    let selection = match (request.query, request.message_ids) {
        (Some(query), None) => DeletionSelection::Query(query),
        (None, Some(ids)) => DeletionSelection::Messages(
            ids.iter()
                .map(|id| {
                    id.parse::<MessageId>()
                        .map_err(|_| ApiError::bad_request("a message id is not valid"))
                })
                .collect::<ApiResult<_>>()?,
        ),
        _ => {
            return Err(ApiError::bad_request("give either query or message_ids"));
        }
    };
    let review = state
        .operations
        .review_deletion(archive_id, selection, user.id)
        .await
        .map_err(refused)?;
    Ok(Json(review.into()))
}

#[utoipa::path(
    post,
    path = "/api/deletions/{id}/confirm",
    params(("id" = String, Path, description = "Review id")),
    request_body = DeletionConfirmRequest,
    responses((status = 202, description = "Deleting in the background", body = TaskDto), (status = 400, description = "An unknown, used or expired review, another user's review, or another count", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn confirm(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(id): Path<String>,
    Json(request): Json<DeletionConfirmRequest>,
) -> ApiResult<(StatusCode, Json<TaskDto>)> {
    let task = state
        .operations
        .confirm_deletion(&id, request.count, user.id)
        .await
        .map_err(refused)?;
    Ok((StatusCode::ACCEPTED, Json(task.into())))
}
