use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, Timestamp};
use holtmail_serve_net::TrustedProxies;
use holtmail_store::{
    AuditKind, Device, DeviceSettings, InstanceRole, NewDevice, StoreError, User,
};

use crate::access::readable;
use crate::dto::*;
use crate::manage::name;
use crate::session::{body, no_store, session_token};
use crate::write::audit;
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// What the `[serve]` config says that the API acts on.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Whether members may make devices; `false` limits new devices and raises of an existing
    /// device's rights to owners and admins.
    pub members: bool,
    /// Peers whose `X-Forwarded-For` is believed when limiting sign-ins.
    pub trusted_proxies: TrustedProxies,
    /// How long after signing in a device may be made or its rights raised.
    pub recent_sign_in: Duration,
    /// The configured listeners devices connect to, in the order to show them.
    pub servers: Vec<ServeEndpoint>,
}

/// One configured protocol listener, as a device should connect to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeEndpoint {
    /// Which protocol it serves.
    pub protocol: DeviceProtocol,
    /// The name to connect to; `None` when the config names none.
    pub host: Option<String>,
    /// The port.
    pub port: u16,
    /// Whether TLS starts with the first byte; otherwise the client uses `STARTTLS`.
    pub implicit_tls: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            members: true,
            trusted_proxies: TrustedProxies::default(),
            recent_sign_in: Duration::from_secs(5 * 60),
            servers: Vec::new(),
        }
    }
}

fn may_make_devices(state: &ApiState, user: &User) -> ApiResult<()> {
    if state.serve.members || user.role != InstanceRole::Member {
        return Ok(());
    }
    Err(ApiError(
        StatusCode::FORBIDDEN,
        "only the owner and admins can make devices on this instance".into(),
    ))
}

async fn recent_sign_in(state: &ApiState, headers: &HeaderMap) -> ApiResult<()> {
    let token = session_token(headers).unwrap_or_default();
    let started = state
        .auth
        .signed_in_at(token)
        .await
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "sign-in failed".into()))?;
    let window = i64::try_from(state.serve.recent_sign_in.as_millis()).unwrap_or(i64::MAX);
    let cutoff = Timestamp::now().as_millis().saturating_sub(window);
    match started {
        Some(at) if at.as_millis() > cutoff => Ok(()),
        _ => Err(ApiError(
            StatusCode::PRECONDITION_REQUIRED,
            "sign in again to do this".into(),
        )),
    }
}

fn protocols(names: &[String]) -> ApiResult<BTreeSet<DeviceProtocol>> {
    let parsed = names
        .iter()
        .map(|name| {
            DeviceProtocol::parse(name)
                .ok_or_else(|| ApiError::bad_request(format!("unknown protocol {name:?}")))
        })
        .collect::<ApiResult<BTreeSet<_>>>()?;
    if parsed.is_empty() {
        return Err(ApiError::bad_request(
            "a device needs at least one protocol",
        ));
    }
    Ok(parsed)
}

fn scopes(names: &[String]) -> ApiResult<BTreeSet<DeviceScope>> {
    let parsed = names
        .iter()
        .map(|name| {
            DeviceScope::parse(name)
                .ok_or_else(|| ApiError::bad_request(format!("unknown scope {name:?}")))
        })
        .collect::<ApiResult<BTreeSet<_>>>()?;
    if parsed.is_empty() {
        return Err(ApiError::bad_request("a device needs at least one scope"));
    }
    Ok(parsed)
}

fn archive(raw: &str, readable: &[ArchiveId]) -> ApiResult<ArchiveId> {
    raw.parse::<ArchiveId>()
        .ok()
        .filter(|id| readable.contains(id))
        .ok_or_else(|| ApiError::not_found("archive"))
}

fn archive_set(raw: &[String], readable: &[ArchiveId]) -> ApiResult<BTreeSet<ArchiveId>> {
    raw.iter().map(|id| archive(id, readable)).collect()
}

fn needs_mail_archive(settings: &DeviceSettings) -> ApiResult<()> {
    if settings.mail_archive.is_none()
        && settings
            .protocols
            .iter()
            .any(|protocol| protocol.needs_mail_archive())
    {
        return Err(ApiError::bad_request(
            "imap and smtp need a mail archive to serve",
        ));
    }
    Ok(())
}

fn store_refusal(failure: StoreError) -> ApiError {
    match failure {
        StoreError::NotFound(what) => ApiError::not_found(what),
        StoreError::Invalid(message) => ApiError::bad_request(message),
        other => other.into(),
    }
}

/// Whether the new settings let the device do or see more than the old ones.
fn raises(old: &DeviceSettings, new: &DeviceSettings) -> bool {
    let wider_archives = match (&old.archives, &new.archives) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(before), Some(after)) => !after.is_subset(before),
    };
    !new.scopes.is_subset(&old.scopes)
        || !new.protocols.is_subset(&old.protocols)
        || wider_archives
        || (new.mail_archive.is_some() && new.mail_archive != old.mail_archive)
}

pub(crate) fn targets(device: &Device) -> Vec<String> {
    let names = |list: Vec<&str>| list.join(" ");
    vec![
        device.id.to_string(),
        device.settings.name.clone(),
        format!(
            "protocols: {}",
            names(
                device
                    .settings
                    .protocols
                    .iter()
                    .map(|p| p.as_str())
                    .collect()
            )
        ),
        format!(
            "scopes: {}",
            names(device.settings.scopes.iter().map(|s| s.as_str()).collect())
        ),
    ]
}

async fn own_device(state: &ApiState, user: &User, raw: &str) -> ApiResult<Device> {
    let id: DeviceId = raw.parse().map_err(|_| ApiError::not_found("device"))?;
    state
        .store
        .get_device(id)
        .await?
        .filter(|device| device.user_id == user.id)
        .ok_or_else(|| ApiError::not_found("device"))
}

#[utoipa::path(
    get,
    path = "/api/devices",
    responses((status = 200, description = "The user's own devices, revoked ones too, oldest first", body = [DeviceDto]), (status = 401, description = "Not signed in", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_devices(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
) -> ApiResult<Json<Vec<DeviceDto>>> {
    let devices = state.store.list_devices(user.id).await?;
    Ok(Json(devices.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/api/devices/setup",
    responses((status = 200, description = "Whether this user may make devices, and the listeners to connect to", body = DeviceSetupDto), (status = 401, description = "Not signed in", body = ErrorDto)),
)]
pub(crate) async fn device_setup(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
) -> Json<DeviceSetupDto> {
    Json(DeviceSetupDto {
        can_create: may_make_devices(&state, &user).is_ok(),
        servers: state
            .serve
            .servers
            .iter()
            .map(|server| DeviceServerDto {
                protocol: server.protocol.as_str().to_owned(),
                host: server.host.clone(),
                port: server.port,
                security: if server.implicit_tls {
                    "tls"
                } else {
                    "starttls"
                }
                .to_owned(),
            })
            .collect(),
    })
}

#[utoipa::path(
    post,
    path = "/api/devices",
    request_body = CreateDeviceRequest,
    responses(
        (status = 201, description = "Made; the app password is in this answer and never again", body = CreatedDeviceDto),
        (status = 400, description = "An empty name, no protocol, no scope, an unknown protocol or scope, or imap or smtp without a mail archive", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "A member when `[serve] members` is false", body = ErrorDto),
        (status = 404, description = "An archive the user cannot read", body = ErrorDto),
        (status = 428, description = "The last sign-in is too old; sign in again and repeat", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn create_device(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    let request: CreateDeviceRequest =
        body(&bytes).map_err(|_| ApiError::bad_request("malformed request body"))?;
    may_make_devices(&state, &user)?;
    let readable = readable(&state, &user).await?;
    let settings = DeviceSettings {
        name: name(&request.name, "the device name")?,
        mail_archive: request
            .mail_archive
            .as_deref()
            .map(|id| archive(id, &readable))
            .transpose()?,
        archives: request
            .archives
            .as_deref()
            .map(|ids| archive_set(ids, &readable))
            .transpose()?,
        protocols: protocols(&request.protocols)?,
        scopes: match &request.scopes {
            Some(names) => scopes(names)?,
            None => BTreeSet::from([DeviceScope::Read]),
        },
        implicit_seen: request.implicit_seen.unwrap_or(false),
    };
    needs_mail_archive(&settings)?;
    recent_sign_in(&state, &headers).await?;
    let secret = state.devices.new_secret().map_err(unavailable)?;
    let device = state
        .store
        .create_device(NewDevice {
            user_id: user.id,
            settings,
            secret_hash: Some(secret.hash),
            at: Timestamp::now(),
        })
        .await
        .map_err(store_refusal)?;
    if let Err(failure) = audit(
        &state,
        &user,
        AuditKind::DeviceCreated,
        device.settings.mail_archive,
        None,
        targets(&device),
    )
    .await
    {
        state.devices.revoke(device.id).await.map_err(unavailable)?;
        return Err(failure);
    }
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(CreatedDeviceDto {
                device: device.into(),
                secret: secret.secret,
            }),
        )
            .into_response(),
    ))
}

fn unavailable(failure: holtmail_device::DeviceAuthError) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string())
}

#[utoipa::path(
    patch,
    path = "/api/devices/{id}",
    params(("id" = String, Path, description = "Device id")),
    request_body = UpdateDeviceRequest,
    responses(
        (status = 200, body = DeviceDto),
        (status = 400, description = "An empty name, no protocol, no scope, an unknown protocol or scope, imap or smtp without a mail archive, or a revoked device", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "A raise of rights by a member when `[serve] members` is false", body = ErrorDto),
        (status = 404, description = "No such device of this user, or an archive the user cannot read", body = ErrorDto),
        (status = 428, description = "A raise of rights and the last sign-in is too old; sign in again and repeat", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn update_device(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<Response> {
    let request: UpdateDeviceRequest =
        body(&bytes).map_err(|_| ApiError::bad_request("malformed request body"))?;
    let device = own_device(&state, &user, &raw).await?;
    let readable = readable(&state, &user).await?;
    let mut settings = device.settings.clone();
    if let Some(raw) = &request.name {
        settings.name = name(raw, "the device name")?;
    }
    if let Some(mail_archive) = &request.mail_archive {
        settings.mail_archive = mail_archive
            .as_deref()
            .map(|id| archive(id, &readable))
            .transpose()?;
    }
    if let Some(archives) = &request.archives {
        settings.archives = archives
            .as_deref()
            .map(|ids| archive_set(ids, &readable))
            .transpose()?;
    }
    if let Some(names) = &request.protocols {
        settings.protocols = protocols(names)?;
    }
    if let Some(names) = &request.scopes {
        settings.scopes = scopes(names)?;
    }
    if let Some(implicit_seen) = request.implicit_seen {
        settings.implicit_seen = implicit_seen;
    }
    needs_mail_archive(&settings)?;
    let raised = raises(&device.settings, &settings);
    if raised {
        may_make_devices(&state, &user)?;
        recent_sign_in(&state, &headers).await?;
    }
    if device.revoked_at.is_some() {
        return Err(ApiError::bad_request("the device is revoked"));
    }
    audit(
        &state,
        &user,
        AuditKind::DeviceChanged,
        settings.mail_archive,
        None,
        targets(&Device {
            settings: settings.clone(),
            ..device.clone()
        }),
    )
    .await?;
    let updated = state
        .store
        .update_device_settings(device.id, &settings, raised.then(Timestamp::now))
        .await
        .map_err(store_refusal)?;
    if raises(&settings, &device.settings) {
        state.devices.end_sessions(updated.id);
    }
    Ok(Json(DeviceDto::from(updated)).into_response())
}

#[utoipa::path(
    delete,
    path = "/api/devices/{id}",
    params(("id" = String, Path, description = "Device id")),
    responses(
        (status = 204, description = "Revoked, or it already was. The device stays listed"),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 404, description = "No such device of this user", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn revoke_device(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Response> {
    let device = own_device(&state, &user, &raw).await?;
    if device.revoked_at.is_none() {
        audit(
            &state,
            &user,
            AuditKind::DeviceRevoked,
            device.settings.mail_archive,
            None,
            targets(&device),
        )
        .await?;
        state.devices.revoke(device.id).await.map_err(unavailable)?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DeviceSettings {
        DeviceSettings {
            name: "Phone".into(),
            mail_archive: None,
            archives: Some(BTreeSet::new()),
            protocols: BTreeSet::from([DeviceProtocol::Jmap]),
            scopes: BTreeSet::from([DeviceScope::Read, DeviceScope::WriteMail]),
            implicit_seen: false,
        }
    }

    #[test]
    fn a_raise_is_more_scopes_protocols_or_archives() {
        let old = settings();
        assert!(!raises(&old, &old));
        let renamed = DeviceSettings {
            name: "Tablet".into(),
            implicit_seen: true,
            ..old.clone()
        };
        assert!(!raises(&old, &renamed), "a rename is no raise");
        let fewer = DeviceSettings {
            scopes: BTreeSet::from([DeviceScope::Read]),
            ..old.clone()
        };
        assert!(!raises(&old, &fewer), "fewer scopes is no raise");
        assert!(raises(&fewer, &old), "a new scope is a raise");
        let imap = DeviceSettings {
            protocols: BTreeSet::from([DeviceProtocol::Jmap, DeviceProtocol::Imap]),
            ..old.clone()
        };
        assert!(raises(&old, &imap), "a new protocol is a raise");
        let every = DeviceSettings {
            archives: None,
            ..old.clone()
        };
        assert!(
            raises(&old, &every),
            "every archive after a list is a raise"
        );
        assert!(
            !raises(&every, &old),
            "a list after every archive is no raise"
        );
        let archive = ArchiveId::new();
        let mail = DeviceSettings {
            mail_archive: Some(archive),
            ..old.clone()
        };
        assert!(raises(&old, &mail), "a mail archive is a raise");
        assert!(
            !raises(&mail, &old),
            "dropping the mail archive is no raise"
        );
    }
}
