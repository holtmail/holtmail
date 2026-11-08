use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use holtmail_auth::Signup;
use holtmail_core::{ArchiveId, UserId};
use holtmail_store::{AuditKind, GrantRole, InstanceRole, User};

use crate::CurrentUser;
use crate::dto::*;
use crate::session::{Reply, auth_error, body, error, no_store, signed_in};
use crate::write::audit;
use crate::{ApiError, ApiState};

type AppState = State<Arc<ApiState>>;

fn store_error(failure: holtmail_store::StoreError) -> crate::session::Refusal {
    let ApiError(status, message) = ApiError::from(failure);
    error(status, message)
}

fn role(raw: &str) -> Result<InstanceRole, crate::session::Refusal> {
    InstanceRole::parse(raw).ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "role is one of owner, admin and member",
        )
    })
}

fn archives(raw: &[String]) -> Result<Vec<ArchiveId>, crate::session::Refusal> {
    raw.iter()
        .map(|id| {
            id.parse()
                .map_err(|_| error(StatusCode::BAD_REQUEST, "an archive id is malformed"))
        })
        .collect()
}

async fn person(state: &ApiState, user: User) -> Result<PersonDto, crate::session::Refusal> {
    let grants = state
        .store
        .grant_roles(user.id)
        .await
        .map_err(store_error)?;
    Ok(PersonDto {
        created_at: user.created_at.as_millis(),
        archives: grants.iter().map(|g| g.archive.to_string()).collect(),
        grants: grants
            .iter()
            .map(|g| GrantDto {
                archive: g.archive.to_string(),
                role: g.role.as_str().into(),
            })
            .collect(),
        user: user.into(),
    })
}

fn owner_only(actor: &User, target: InstanceRole) -> Result<(), crate::session::Refusal> {
    if target == InstanceRole::Owner && actor.role != InstanceRole::Owner {
        return Err(error(
            StatusCode::FORBIDDEN,
            "only an owner can manage owners",
        ));
    }
    Ok(())
}

fn api_error(failure: ApiError) -> crate::session::Refusal {
    let ApiError(status, message) = failure;
    error(status, message)
}

/// Replaces the user's archive grants with `wanted`, keeping the role of archives that stay.
/// Only the owner takes away an editor or sender grant this way; every grant given or taken
/// away is audited before it changes.
async fn replace_grants(
    state: &ApiState,
    actor: &User,
    user: UserId,
    wanted: &[ArchiveId],
) -> Result<(), crate::session::Refusal> {
    let current = state.store.grant_roles(user).await.map_err(store_error)?;
    let dropped: Vec<_> = current
        .iter()
        .filter(|g| !wanted.contains(&g.archive))
        .collect();
    if actor.role != InstanceRole::Owner && dropped.iter().any(|g| g.role > GrantRole::Reader) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "only the owner takes away editor and sender grants",
        ));
    }
    for grant in &dropped {
        audit(
            state,
            actor,
            AuditKind::GrantChanged,
            Some(grant.archive),
            None,
            vec![user.to_string()],
        )
        .await
        .map_err(api_error)?;
    }
    for archive in wanted
        .iter()
        .filter(|a| !current.iter().any(|g| g.archive == **a))
    {
        audit(
            state,
            actor,
            AuditKind::GrantChanged,
            Some(*archive),
            None,
            vec![user.to_string(), GrantRole::Reader.as_str().to_owned()],
        )
        .await
        .map_err(api_error)?;
    }
    state
        .store
        .set_grants(user, wanted)
        .await
        .map_err(store_error)
}

async fn target(state: &ApiState, raw: &str) -> Result<User, crate::session::Refusal> {
    let id: UserId = raw
        .parse()
        .map_err(|_| error(StatusCode::NOT_FOUND, "user not found"))?;
    state
        .store
        .get_user(id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "user not found"))
}

#[utoipa::path(
    get,
    path = "/api/users",
    responses((status = 200, body = Vec<PersonDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_users(State(state): AppState) -> Reply {
    let mut out = Vec::new();
    for user in state.store.list_users().await.map_err(store_error)? {
        out.push(person(&state, user).await?);
    }
    Ok(no_store(Json(out).into_response()))
}

#[utoipa::path(
    post,
    path = "/api/users",
    request_body = NewUserRequest,
    responses((status = 201, body = PersonDto), (status = 400, description = "A malformed email, an empty name or a short password", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, or an admin adding an owner", body = ErrorDto), (status = 409, description = "The email is taken", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn create_user(
    State(state): AppState,
    Extension(CurrentUser(actor)): Extension<CurrentUser>,
    bytes: Bytes,
) -> Reply {
    let request: NewUserRequest = body(&bytes)?;
    let role = role(request.role.as_deref().unwrap_or("member"))?;
    owner_only(&actor, role)?;
    let grants = archives(&request.archives)?;
    let user = state
        .auth
        .create_user(
            Signup {
                email: request.email,
                name: request.name,
                password: request.password,
            },
            role,
        )
        .await
        .map_err(auth_error)?;
    replace_grants(&state, &actor, user.id, &grants).await?;
    Ok((StatusCode::CREATED, Json(person(&state, user).await?)).into_response())
}

#[utoipa::path(
    patch,
    path = "/api/users/{id}",
    params(("id" = String, Path, description = "User id")),
    request_body = UserUpdateRequest,
    responses((status = 200, body = PersonDto), (status = 400, description = "Bad values, or changing your own role", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, an admin changing an owner, taking away an editor or sender grant, or changing the email or password of someone with one", body = ErrorDto), (status = 404, description = "No such user", body = ErrorDto), (status = 409, description = "The email is taken", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_user(
    State(state): AppState,
    Extension(CurrentUser(actor)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    bytes: Bytes,
) -> Reply {
    let request: UserUpdateRequest = body(&bytes)?;
    let mut user = target(&state, &raw).await?;
    owner_only(&actor, user.role)?;
    let writes = state
        .store
        .grant_roles(user.id)
        .await
        .map_err(store_error)?
        .iter()
        .any(|g| g.role > GrantRole::Reader);
    if writes
        && actor.role != InstanceRole::Owner
        && (request.email.is_some() || request.password.is_some())
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "only the owner changes the email or password of someone with an editor or sender grant",
        ));
    }
    let wanted = request.archives.as_deref().map(archives).transpose()?;
    if let Some(raw_role) = &request.role {
        let new_role = role(raw_role)?;
        owner_only(&actor, new_role)?;
        if user.id == actor.id && new_role != user.role {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "you cannot change your own role",
            ));
        }
        user.role = new_role;
    }
    if let Some(email) = request.email {
        user.email = email;
    }
    if let Some(name) = request.name {
        user.name = name;
    }
    let user = state.auth.update_user(user).await.map_err(auth_error)?;
    if let Some(wanted) = &wanted {
        replace_grants(&state, &actor, user.id, wanted).await?;
    }
    if let Some(password) = &request.password {
        state
            .auth
            .set_password(user.id, password)
            .await
            .map_err(auth_error)?;
    }
    Ok(Json(person(&state, user).await?).into_response())
}

#[utoipa::path(
    delete,
    path = "/api/users/{id}",
    params(("id" = String, Path, description = "User id")),
    responses((status = 204, description = "Removed with their sessions"), (status = 400, description = "Removing yourself", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, or an admin removing an owner", body = ErrorDto), (status = 404, description = "No such user", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn delete_user(
    State(state): AppState,
    Extension(CurrentUser(actor)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> Reply {
    let user = target(&state, &raw).await?;
    if user.id == actor.id {
        return Err(error(StatusCode::BAD_REQUEST, "you cannot remove yourself"));
    }
    owner_only(&actor, user.role)?;
    let devices = state
        .store
        .list_devices(user.id)
        .await
        .map_err(store_error)?;
    for device in devices.iter().filter(|d| d.revoked_at.is_none()) {
        crate::write::audit(
            &state,
            &actor,
            AuditKind::DeviceRevoked,
            device.settings.mail_archive,
            None,
            crate::devices::targets(device),
        )
        .await
        .map_err(|ApiError(status, message)| error(status, message))?;
    }
    state.devices.revoke_user(user.id).await.map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "removing the user failed",
        )
    })?;
    state
        .store
        .delete_user(user.id)
        .await
        .map_err(store_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    patch,
    path = "/api/me",
    request_body = ProfileRequest,
    responses((status = 200, body = SessionDto), (status = 400, description = "A malformed email or an empty name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 409, description = "The email is taken", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_me(
    State(state): AppState,
    Extension(CurrentUser(mut user)): Extension<CurrentUser>,
    bytes: Bytes,
) -> Reply {
    let request: ProfileRequest = body(&bytes)?;
    if let Some(email) = request.email {
        user.email = email;
    }
    if let Some(name) = request.name {
        user.name = name;
    }
    let user = state.auth.update_user(user).await.map_err(auth_error)?;
    Ok(no_store(
        Json(SessionDto { user: user.into() }).into_response(),
    ))
}

#[utoipa::path(
    post,
    path = "/api/me/password",
    request_body = PasswordRequest,
    responses((status = 200, description = "Changed; every other session ended and a new cookie is set", body = SessionDto), (status = 400, description = "The new password is too short", body = ErrorDto), (status = 401, description = "Not signed in, or the current password is wrong", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn change_password(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Reply {
    let request: PasswordRequest = body(&bytes)?;
    let session = state
        .auth
        .change_password(user, &request.current, &request.new)
        .await
        .map_err(auth_error)?;
    Ok(signed_in(&state, &headers, session))
}
