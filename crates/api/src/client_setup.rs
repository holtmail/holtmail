use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use holtmail_client_setup::{Account, Profile, Server, SetupError};
use holtmail_core::{DeviceId, DeviceProtocol, Timestamp, UserId};
use holtmail_store::{AuditKind, Device, StoreError, User};
use serde::Deserialize;

use crate::devices::{own_device, recent_sign_in, targets, unavailable};
use crate::dto::*;
use crate::session::no_store;
use crate::write::audit;
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// How long a profile link works after it was made.
pub const PROFILE_LINK_TTL: Duration = Duration::from_secs(10 * 60);

/// What clients are told about this instance, for profiles, autoconfig and the DAVx5 link.
#[derive(Debug, Clone)]
pub struct ClientSetup {
    /// The services and host names clients connect to.
    pub server: Server,
    /// The instance's stable id; with a device's id it fixes the profile's identifiers, so a
    /// profile downloaded again replaces the installed one.
    pub instance: String,
    /// The mail domains the operator controls. Autoconfig answers only for addresses at these
    /// domains, lowercase; none means it answers for nobody.
    pub mail_domains: Vec<String>,
}

/// One-time profile links waiting to be opened. Kept in memory only: a restart ends every
/// link, and the person asks for a new one.
#[derive(Clone, Default)]
pub struct ProfileLinks(Arc<Mutex<HashMap<[u8; 32], Link>>>);

#[derive(Clone, Copy)]
struct Link {
    device: DeviceId,
    user: UserId,
    expires: Instant,
}

impl fmt::Debug for ProfileLinks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileLinks").finish_non_exhaustive()
    }
}

impl ProfileLinks {
    fn issue(&self, device: DeviceId, user: UserId) -> ApiResult<String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "no random source".into()))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = Instant::now();
        let mut links = self.0.lock().unwrap_or_else(|e| e.into_inner());
        links.retain(|_, link| link.expires > now);
        links.insert(
            *blake3::hash(token.as_bytes()).as_bytes(),
            Link {
                device,
                user,
                expires: now + PROFILE_LINK_TTL,
            },
        );
        Ok(token)
    }

    fn take(&self, token: &str) -> Option<Link> {
        let mut links = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let link = links.remove(blake3::hash(token.as_bytes()).as_bytes())?;
        (link.expires > Instant::now()).then_some(link)
    }

    fn peek(&self, token: &str) -> Option<Link> {
        let links = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let link = links
            .get(blake3::hash(token.as_bytes()).as_bytes())
            .copied()?;
        (link.expires > Instant::now()).then_some(link)
    }

    /// Puts a link back after a failure that issued nothing, so the person can try again.
    fn restore(&self, token: &str, link: Link) {
        let mut links = self.0.lock().unwrap_or_else(|e| e.into_inner());
        links.insert(*blake3::hash(token.as_bytes()).as_bytes(), link);
    }
}

/// The largest body the profile confirmation `POST` reads: the form carries nothing.
pub(crate) const MAX_CONFIRM: usize = 1024;

const CONFIRM_PAGE: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"robots\" content=\"noindex\"><title>Holtmail profile</title>\
<style>body{font:16px/1.5 system-ui,sans-serif;margin:0;padding:32px 16px;color:#1a1a1a;background:#fff}\
main{max-width:28rem;margin:0 auto}button{font:inherit;padding:12px 20px;border:1px solid #1a1a1a;\
border-radius:0;background:#1a1a1a;color:#fff;cursor:pointer}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}button{background:#eee;color:#111;border-color:#eee}}</style>\
</head><body><main><h1>Install the Holtmail profile</h1>\
<p>This sets up mail, calendars and contacts on this device. Installing it gives the device a new \
app password; the old one stops working. The link works once.</p>\
<form method=\"post\"><button type=\"submit\">Download profile</button></form></main></body></html>\n";

fn setup(state: &ApiState) -> ApiResult<&ClientSetup> {
    state.serve.client.as_ref().ok_or_else(|| {
        ApiError(
            StatusCode::CONFLICT,
            "this server has no public_url, so it cannot set up clients".into(),
        )
    })
}

fn origin(server: &Server) -> String {
    let (scheme, default) = if server.web_tls {
        ("https", 443)
    } else {
        ("http", 80)
    };
    if server.web_port == default {
        format!("{scheme}://{}", server.web_host)
    } else {
        format!("{scheme}://{}:{}", server.web_host, server.web_port)
    }
}

fn account(user: &User) -> Account {
    Account {
        login: user.email.clone(),
        display_name: user.name.clone(),
    }
}

fn profile(setup: &ClientSetup, user: &User, device: &Device, password: String) -> Profile {
    let protocols = &device.settings.protocols;
    Profile {
        instance: setup.instance.clone(),
        device: device.id.to_string(),
        account: account(user),
        app_password: password,
        mail: protocols.contains(&DeviceProtocol::Imap)
            && protocols.contains(&DeviceProtocol::Smtp),
        calendars: protocols.contains(&DeviceProtocol::Dav),
        contacts: protocols.contains(&DeviceProtocol::Dav),
    }
}

fn refusal(failure: SetupError) -> ApiError {
    match failure {
        SetupError::Empty => {
            ApiError::bad_request("a profile needs a device with imap and smtp, or with dav")
        }
        SetupError::NotOffered(service) => {
            ApiError::bad_request(format!("this server does not offer {service}"))
        }
        SetupError::Invalid(field) => ApiError(
            StatusCode::CONFLICT,
            format!("the server's setup has an unusable {field}"),
        ),
    }
}

#[utoipa::path(
    post,
    path = "/api/devices/{id}/profile",
    params(("id" = String, Path, description = "Device id")),
    responses(
        (status = 201, description = "A one-time link to the device's Apple profile. Opening it gives the device a fresh app password, which the profile carries; the old one stops working", body = ProfileLinkDto),
        (status = 400, description = "A revoked device, or one whose protocols this server cannot put in a profile (imap with smtp, or dav)", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 404, description = "No such device of this user", body = ErrorDto),
        (status = 409, description = "The server has no public_url or an unusable host name", body = ErrorDto),
        (status = 428, description = "The last sign-in is too old; sign in again and repeat", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn profile_link(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let device = own_device(&state, &user, &raw).await?;
    if device.revoked_at.is_some() {
        return Err(ApiError::bad_request("the device is revoked"));
    }
    let setup = setup(&state)?;
    holtmail_client_setup::mobileconfig(
        &setup.server,
        &profile(setup, &user, &device, "hm_unused".into()),
    )
    .map_err(refusal)?;
    recent_sign_in(&state, &headers).await?;
    let token = state.serve.profile_links.issue(device.id, user.id)?;
    let expires = Timestamp::now()
        .as_millis()
        .saturating_add(i64::try_from(PROFILE_LINK_TTL.as_millis()).unwrap_or(i64::MAX));
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(ProfileLinkDto {
                url: format!("{}/api/devices/profile/{token}", origin(&setup.server)),
                expires_at: expires,
            }),
        )
            .into_response(),
    ))
}

#[utoipa::path(
    get,
    path = "/api/devices/profile/{token}",
    params(("token" = String, Path, description = "The token of a link from `POST /api/devices/{id}/profile`")),
    security(()),
    responses(
        (status = 200, description = "A page whose button posts back to the same address for the profile. Changes nothing, so link previews and scanners never use the link up", content_type = "text/html"),
        (status = 404, description = "An unknown, used or expired link, or the device was revoked since", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn profile_page(
    State(state): AppState,
    Path(token): Path<String>,
) -> ApiResult<Response> {
    let gone = || ApiError::not_found("profile link");
    let link = state.serve.profile_links.peek(&token).ok_or_else(gone)?;
    let user = state.store.get_user(link.user).await?.ok_or_else(gone)?;
    let device = own_device(&state, &user, &link.device.to_string())
        .await
        .map_err(|_| gone())?;
    if device.revoked_at.is_some() {
        return Err(gone());
    }
    let mut response = (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'",
            ),
        ],
        CONFIRM_PAGE,
    )
        .into_response();
    response
        .headers_mut()
        .insert("x-robots-tag", HeaderValue::from_static("noindex"));
    Ok(no_store(response))
}

#[utoipa::path(
    post,
    path = "/api/devices/profile/{token}",
    params(("token" = String, Path, description = "The token of a link from `POST /api/devices/{id}/profile`")),
    security(()),
    responses(
        (status = 200, description = "The Apple profile (`application/x-apple-aspen-config`) with a fresh app password for the device. Works once, needs no session; the device's old app password stops working and its sessions end", content_type = "application/x-apple-aspen-config"),
        (status = 404, description = "An unknown, used or expired link, or the device was revoked since", body = ErrorDto),
        (status = 500, description = "The server failed; the link still works", body = ErrorDto),
    ),
)]
pub(crate) async fn download_profile(
    State(state): AppState,
    Path(token): Path<String>,
) -> ApiResult<Response> {
    let gone = || ApiError::not_found("profile link");
    let link = state.serve.profile_links.take(&token).ok_or_else(gone)?;
    match issue_profile(&state, link).await {
        Ok(response) => Ok(response),
        Err(Issued::Gone) => Err(gone()),
        Err(Issued::Failed(error)) => {
            state.serve.profile_links.restore(&token, link);
            Err(error)
        }
    }
}

enum Issued {
    Gone,
    Failed(ApiError),
}

impl From<ApiError> for Issued {
    fn from(error: ApiError) -> Self {
        Self::Failed(error)
    }
}

impl From<StoreError> for Issued {
    fn from(error: StoreError) -> Self {
        Self::Failed(error.into())
    }
}

async fn issue_profile(state: &ApiState, link: Link) -> Result<Response, Issued> {
    let setup = setup(state)?;
    let user = state.store.get_user(link.user).await?.ok_or(Issued::Gone)?;
    let device = own_device(state, &user, &link.device.to_string())
        .await
        .map_err(|_| Issued::Gone)?;
    if device.revoked_at.is_some() {
        return Err(Issued::Gone);
    }
    let secret = state.devices.new_secret().map_err(unavailable)?;
    let rendered = holtmail_client_setup::mobileconfig(
        &setup.server,
        &profile(setup, &user, &device, secret.secret),
    )
    .map_err(refusal)?;
    state
        .store
        .replace_device_secret(device.id, &secret.hash)
        .await
        .map_err(|failure| match failure {
            StoreError::NotFound(_) | StoreError::Invalid(_) => Issued::Gone,
            other => other.into(),
        })?;
    state.devices.end_sessions(device.id);
    if let Err(error) = audit(
        state,
        &user,
        AuditKind::DeviceSecretIssued,
        device.settings.mail_archive,
        None,
        targets(&device),
    )
    .await
    {
        tracing::warn!(device = %device.id, error = %error.1, "issued device secret not audited");
    }
    let mut response = rendered.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-apple-aspen-config"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"holtmail.mobileconfig\""),
    );
    Ok(no_store(response))
}

#[derive(Debug, Deserialize)]
pub(crate) struct AutoconfigQuery {
    emailaddress: Option<String>,
}

/// Thunderbird autoconfig at `/.well-known/autoconfig/mail/config-v1.1.xml` and, for a host
/// `autoconfig.<domain>`, at `/mail/config-v1.1.xml`. Answers only for the configured mail
/// domains; anything else is `404`, so the server never claims a domain it does not run.
pub(crate) async fn autoconfig(
    State(state): AppState,
    Query(query): Query<AutoconfigQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(setup) = state.serve.client.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|host| host.rsplit_once(':').map_or(host, |(name, _)| name))
        .map(|host| host.strip_prefix("autoconfig.").unwrap_or(host));
    let asked = match query.emailaddress.as_deref() {
        Some(address) => address.rsplit_once('@').map(|(_, domain)| domain),
        None => host,
    };
    let Some(domain) = asked
        .map(str::to_ascii_lowercase)
        .filter(|domain| setup.mail_domains.contains(domain))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match holtmail_client_setup::autoconfig(&setup.server, &domain) {
        Ok(xml) => (
            [
                (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
                (header::CACHE_CONTROL, "max-age=3600"),
            ],
            xml,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The mail domains Thunderbird autoconfig answers for, in config order; empty without a
/// `public_url` or when the configured servers give no usable autoconfig.
pub(crate) fn autoconfig_domains(state: &ApiState) -> Vec<String> {
    let Some(setup) = state.serve.client.as_ref() else {
        return Vec::new();
    };
    setup
        .mail_domains
        .iter()
        .filter(|domain| holtmail_client_setup::autoconfig(&setup.server, domain).is_ok())
        .cloned()
        .collect()
}

/// The DAVx5 hand-off link for `user`, when this server offers DAV over https.
pub(crate) fn davx5(state: &ApiState, user: &User) -> Option<String> {
    let setup = state.serve.client.as_ref()?;
    holtmail_client_setup::davx5_link(&setup.server, &user.email).ok()
}

/// Whether an Apple profile can be offered at all.
pub(crate) fn profiles(state: &ApiState) -> bool {
    state.serve.client.as_ref().is_some_and(|setup| {
        let server = &setup.server;
        server.dav || (server.imap.offered() && server.submission.offered())
    })
}
