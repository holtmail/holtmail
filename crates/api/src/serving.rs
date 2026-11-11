use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_core::{ArchiveId, MessageId, Timestamp};
use holtmail_store::User;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::access::readable;
use crate::dto::{AddressDto, ErrorDto, addresses};
use crate::{ApiError, ApiResult, ApiState, CurrentUser, Scope, Scopes};

type AppState = State<Arc<ApiState>>;

/// The most messages a local flags list names; `total` still counts them all.
pub const LOCAL_FLAGS_SHOWN: usize = 200;

/// How mail apps are served from one archive to this user.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServingArchiveDto {
    pub archive_id: String,
    /// Served mailboxes this user reads: the shared ones and the user's own drafts.
    pub mailboxes: u32,
    /// Of those, the ones whose first build is done.
    pub built: u32,
    /// Members with a UID, summed over every mailbox; a built one counts in full, so the sum
    /// only grows while a build runs.
    pub assigned: u64,
    /// Members to give a UID, summed over every mailbox.
    pub total: u64,
    /// Messages carrying local flags of this user.
    pub local_flagged: u64,
}

/// What Settings → Devices shows about serving mail to apps.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServingDto {
    /// Whether flags a mail app cannot write upstream are kept as local flags (`[serve]
    /// local_flags`).
    pub local_flags: bool,
    /// Every readable archive with served mailboxes or local flags of this user, in the order
    /// of the archive list.
    pub archives: Vec<ServingArchiveDto>,
}

/// One local flag: the user's own setting, kept only in Holtmail.
#[derive(Debug, Serialize, ToSchema)]
pub struct LocalFlagDto {
    pub flag: String,
    /// `true` when set, `false` when cleared although the mail server still has it.
    pub present: bool,
}

/// A message with local flags.
#[derive(Debug, Serialize, ToSchema)]
pub struct LocalFlaggedDto {
    pub message_id: String,
    pub subject: Option<String>,
    pub from: Vec<AddressDto>,
    /// Unix milliseconds.
    pub date: i64,
    pub flags: Vec<LocalFlagDto>,
}

/// The user's local flags in one archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct LocalFlagsDto {
    /// Whether local flags apply on this server; when off they are kept but not shown to apps.
    pub enabled: bool,
    /// Messages with local flags.
    pub total: u64,
    /// The newest of them by date, at most 200.
    pub messages: Vec<LocalFlaggedDto>,
}

/// How many messages had their local flags dropped.
#[derive(Debug, Serialize, ToSchema)]
pub struct DroppedLocalFlagsDto {
    pub dropped: u64,
}

/// Which archive's local flags.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LocalFlagsParams {
    /// The archive.
    pub archive: String,
    /// Only this message; every message of the archive when left out.
    pub message: Option<String>,
}

async fn readable_archive(state: &ApiState, user: &User, raw: &str) -> ApiResult<ArchiveId> {
    let archives = readable(state, user).await?;
    raw.parse::<ArchiveId>()
        .ok()
        .filter(|id| archives.contains(id))
        .ok_or_else(|| ApiError::not_found("archive"))
}

#[utoipa::path(
    get,
    path = "/api/devices/serving",
    responses((status = 200, description = "Build progress of served mailboxes and local flag counts, per archive", body = ServingDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn serving(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
) -> ApiResult<Json<ServingDto>> {
    let mut archives = Vec::new();
    for archive in readable(&state, &user).await? {
        let views: Vec<_> = state
            .store
            .served_mailboxes(archive)
            .await?
            .into_iter()
            .filter(|view| view.key.user().is_none_or(|owner| owner == user.id))
            .collect();
        let local_flagged = state.store.local_flagged(user.id, archive).await?.len() as u64;
        if views.is_empty() && local_flagged == 0 {
            continue;
        }
        let total =
            |view: &holtmail_store::ServedMailbox| view.progress.total.max(view.progress.assigned);
        archives.push(ServingArchiveDto {
            archive_id: archive.to_string(),
            mailboxes: views.len() as u32,
            built: views.iter().filter(|view| view.built).count() as u32,
            assigned: views
                .iter()
                .map(|view| {
                    if view.built {
                        total(view)
                    } else {
                        view.progress.assigned
                    }
                })
                .sum(),
            total: views.iter().map(total).sum(),
            local_flagged,
        });
    }
    Ok(Json(ServingDto {
        local_flags: state.serve.local_flags,
        archives,
    }))
}

#[utoipa::path(
    get,
    path = "/api/devices/local-flags",
    params(LocalFlagsParams),
    responses((status = 200, description = "The user's local flags in the archive, newest message first", body = LocalFlagsDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "An archive the user cannot read", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn local_flags(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Query(params): Query<LocalFlagsParams>,
) -> ApiResult<Json<LocalFlagsDto>> {
    let archive = readable_archive(&state, &user, &params.archive).await?;
    let flagged = state.store.local_flagged(user.id, archive).await?;
    let total = flagged.len() as u64;
    let ids: Vec<MessageId> = flagged.iter().map(|m| m.message_id).collect();
    let mut found: HashMap<MessageId, _> = state
        .store
        .get_messages(&ids)
        .await?
        .into_iter()
        .map(|message| (message.id, message))
        .collect();
    let mut messages: Vec<LocalFlaggedDto> = flagged
        .into_iter()
        .filter_map(|entry| {
            let message = found.remove(&entry.message_id)?;
            Some(LocalFlaggedDto {
                message_id: message.id.to_string(),
                subject: message.subject,
                from: addresses(&message.from),
                date: message.date.as_millis(),
                flags: entry
                    .flags
                    .into_iter()
                    .map(|f| LocalFlagDto {
                        flag: f.flag,
                        present: f.present,
                    })
                    .collect(),
            })
        })
        .collect();
    messages.sort_by(|a, b| b.date.cmp(&a.date).then(a.message_id.cmp(&b.message_id)));
    messages.truncate(LOCAL_FLAGS_SHOWN);
    Ok(Json(LocalFlagsDto {
        enabled: state.serve.local_flags,
        total,
        messages,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/devices/local-flags",
    params(LocalFlagsParams),
    responses((status = 200, description = "Dropped; mail apps show the flags of the mail server again. Idempotent", body = DroppedLocalFlagsDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Credentials without `write:mail`", body = ErrorDto), (status = 404, description = "An archive the user cannot read, or a malformed message id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn drop_local_flags(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Query(params): Query<LocalFlagsParams>,
) -> ApiResult<Json<DroppedLocalFlagsDto>> {
    if !scopes.has(Scope::WriteMail) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "these credentials do not allow this change".into(),
        ));
    }
    let archive = readable_archive(&state, &user, &params.archive).await?;
    let message = params
        .message
        .as_deref()
        .map(|raw| {
            raw.parse::<MessageId>()
                .map_err(|_| ApiError::not_found("message"))
        })
        .transpose()?;
    let dropped = state
        .store
        .drop_local_flags(user.id, archive, message, Timestamp::now())
        .await?;
    Ok(Json(DroppedLocalFlagsDto { dropped }))
}
