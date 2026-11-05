use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use holtmail_core::{ArchiveId, MessageId, ThreadId};
use holtmail_store::{InstanceRole, User};

use crate::{ApiError, ApiResult, ApiState, CurrentUser};

/// Whether the user runs the instance: archives, accounts and people.
pub(crate) fn manages(user: &User) -> bool {
    matches!(user.role, InstanceRole::Owner | InstanceRole::Admin)
}

/// The archives the user may read: every archive for owners and admins, the granted ones for
/// members.
pub(crate) async fn readable(state: &ApiState, user: &User) -> ApiResult<Vec<ArchiveId>> {
    if manages(user) {
        return Ok(state
            .store
            .list_archives()
            .await?
            .into_iter()
            .map(|a| a.id)
            .collect());
    }
    Ok(state.store.grants(user.id).await?)
}

fn forbidden() -> Response {
    ApiError(
        StatusCode::FORBIDDEN,
        "only the owner and admins can do this".into(),
    )
    .into_response()
}

/// Lets owners and admins do everything; members may read the archives granted to them and
/// change their own profile. Archives, messages and threads a member may not read answer `404`,
/// as if they did not exist.
pub(crate) async fn check(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(CurrentUser(user)) = request.extensions().get::<CurrentUser>().cloned() else {
        return next.run(request).await;
    };
    if manages(&user) {
        return next.run(request).await;
    }
    let path = request.uri().path().trim_start_matches("/api/").to_owned();
    let parts: Vec<&str> = path.split('/').collect();
    let read = matches!(*request.method(), Method::GET | Method::HEAD);
    let archive = match parts.as_slice() {
        ["me", ..] => return next.run(request).await,
        [
            "users" | "accounts" | "tasks" | "image-proxy" | "sync-schedule",
            ..,
        ] => return forbidden(),
        _ if !read => return forbidden(),
        ["archives"] => return next.run(request).await,
        ["archives", id, ..] => id.parse::<ArchiveId>().ok(),
        ["messages", id, ..] => match id.parse::<MessageId>() {
            Ok(id) => match state.store.get_message(id).await {
                Ok(message) => message.map(|m| m.archive_id),
                Err(error) => return ApiError::from(error).into_response(),
            },
            Err(_) => None,
        },
        ["threads", id] => match id.parse::<ThreadId>() {
            Ok(id) => match state.store.thread_messages(id).await {
                Ok(messages) => messages.first().map(|m| m.archive_id),
                Err(error) => return ApiError::from(error).into_response(),
            },
            Err(_) => None,
        },
        _ => return next.run(request).await,
    };
    let Some(archive) = archive else {
        return next.run(request).await;
    };
    match state.store.grants(user.id).await {
        Ok(grants) if grants.contains(&archive) => next.run(request).await,
        Ok(_) => ApiError::not_found(parts[0].trim_end_matches('s')).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
