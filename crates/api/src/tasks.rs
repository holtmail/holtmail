use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::dto::*;
use crate::mailboxes::MailboxError;
use crate::{ApiError, ApiResult, ApiState};

type AppState = State<Arc<ApiState>>;

#[utoipa::path(
    get,
    path = "/api/tasks",
    responses((status = 200, description = "Running tasks and the most recent finished ones, newest first", body = [TaskDto]), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn list_tasks(State(state): AppState) -> Json<Vec<TaskDto>> {
    Json(
        state
            .mailboxes
            .tasks()
            .into_iter()
            .map(Into::into)
            .collect(),
    )
}

#[utoipa::path(
    post,
    path = "/api/tasks/{id}/cancel",
    params(("id" = String, Path, description = "Task id")),
    responses((status = 204, description = "Asked to stop; it stops after the current step"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 409, description = "Unknown, finished or not cancellable", body = ErrorDto)),
)]
pub(crate) async fn cancel_task(
    State(state): AppState,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state
        .mailboxes
        .cancel_task(&id)
        .map_err(|error| match error {
            MailboxError::Invalid(message) => ApiError(StatusCode::CONFLICT, message),
            MailboxError::Backend(message) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, message),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/image-proxy",
    responses((status = 200, body = ImageProxyDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn get_image_proxy(State(state): AppState) -> Json<ImageProxyDto> {
    Json(state.mailboxes.image_proxy().into())
}

#[utoipa::path(
    put,
    path = "/api/image-proxy",
    request_body = ImageProxyRequest,
    responses((status = 200, body = ImageProxyDto), (status = 400, description = "A bad or unreachable proxy, or one set in the server's config", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn set_image_proxy(
    State(state): AppState,
    Json(request): Json<ImageProxyRequest>,
) -> ApiResult<Json<ImageProxyDto>> {
    let proxy = request
        .proxy
        .map(|proxy| proxy.trim().to_owned())
        .filter(|proxy| !proxy.is_empty());
    let proxy = state
        .mailboxes
        .set_image_proxy(proxy)
        .await
        .map_err(|error| match error {
            MailboxError::Invalid(message) => ApiError::bad_request(message),
            MailboxError::Backend(message) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, message),
        })?;
    Ok(Json(proxy.into()))
}

#[utoipa::path(
    get,
    path = "/api/sync-schedule",
    responses((status = 200, body = SyncScheduleDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn get_sync_schedule(State(state): AppState) -> Json<SyncScheduleDto> {
    Json(schedule_dto(state.mailboxes.sync_schedule()))
}

#[utoipa::path(
    put,
    path = "/api/sync-schedule",
    request_body = SyncScheduleRequest,
    responses((status = 200, body = SyncScheduleDto), (status = 400, description = "Above a week, or set in the server's config", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto)),
)]
pub(crate) async fn set_sync_schedule(
    State(state): AppState,
    Json(request): Json<SyncScheduleRequest>,
) -> ApiResult<Json<SyncScheduleDto>> {
    let schedule = state
        .mailboxes
        .set_sync_schedule(request.minutes)
        .await
        .map_err(|error| match error {
            MailboxError::Invalid(message) => ApiError::bad_request(message),
            MailboxError::Backend(message) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, message),
        })?;
    Ok(Json(schedule_dto(schedule)))
}

fn schedule_dto(schedule: crate::SyncSchedule) -> SyncScheduleDto {
    SyncScheduleDto {
        minutes: schedule.minutes,
        from_config: schedule.from_config,
    }
}
