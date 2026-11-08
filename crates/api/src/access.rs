use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use holtmail_core::{ArchiveId, EventGroupId, ItemId, MessageId, PersonId, ThreadId};
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
/// change their own profile. Sending routes (transports, identities, drafts, the outbox and
/// replies and forwards) and mailbox changes (flags, moves, deletes, restores, retrying and
/// dismissing failed changes, an account's changes and retention) and contact and calendar
/// edits (creating, changing and deleting items, a person's card, open changes and their
/// resolution, write modes, answers to invitations) pass to their handlers, which check the grant themselves. Archives, messages, threads, contact or calendar items, events and
/// people a member may not read answer `404`, as if they did not exist.
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
        ["me" | "devices", ..] => return next.run(request).await,
        ["insights", "data"] if read => return next.run(request).await,
        ["transports" | "identities" | "drafts" | "outbox", ..]
        | [
            "archives",
            _,
            "transports" | "identities" | "default-identity" | "drafts" | "outbox",
            ..,
        ]
        | [
            "messages",
            _,
            "reply" | "forward" | "changes" | "restore" | "rsvp",
        ]
        | ["mail", "changes"]
        | ["mail-changes", _, "retry" | "dismiss"]
        | ["accounts", _, "mail-changes" | "retention"]
        | ["collections", _, "items" | "write-mode" | "organizing"]
        | ["items", _, "content" | "rsvp"]
        | ["pim", "changes", ..]
        | ["people", _, "card"] => return next.run(request).await,
        ["items", _] if *request.method() == Method::DELETE => return next.run(request).await,
        [
            "users" | "accounts" | "tasks" | "image-proxy" | "sync-schedule" | "oauth" | "ledger"
            | "deletions" | "webhooks" | "smtp" | "alias-providers" | "alias-services" | "audit",
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
        ["items", id, ..] => match id.parse::<ItemId>() {
            Ok(id) => match state.store.get_item(id).await {
                Ok(item) => item.map(|i| i.item.archive_id),
                Err(error) => return ApiError::from(error).into_response(),
            },
            Err(_) => None,
        },
        ["people", id] => match id.parse::<PersonId>() {
            Ok(id) => match state.store.get_person(id).await {
                Ok(person) => person.map(|p| p.archive_id),
                Err(error) => return ApiError::from(error).into_response(),
            },
            Err(_) => None,
        },
        ["events", id] => match id.parse::<EventGroupId>() {
            Ok(id) => match state.store.group_members(id).await {
                Ok(members) => members.first().map(|m| m.summary.item.archive_id),
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
        Ok(_) => ApiError::not_found(match parts[0] {
            "people" => "person",
            "events" => "event",
            other => other.trim_end_matches('s'),
        })
        .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
