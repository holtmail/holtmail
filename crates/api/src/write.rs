use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
pub(crate) use holtmail_access::{Access, AccessError};
pub use holtmail_access::{Scope, Scopes, WriteSwitches};
use holtmail_core::{AccountId, ArchiveId, Timestamp, UserId};
use holtmail_store::{
    Account, AuditEntry, AuditKind, AuditQuery, Capabilities, Capability, GrantRole,
    IdempotencyClaim, IdempotencyKey, InstanceRole, NewAuditEntry, StoredResponse, User,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::access::manages;
use crate::dto::ErrorDto;
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// How long a response kept for an `Idempotency-Key` is replayed.
pub const IDEMPOTENCY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// How long an unfinished `Idempotency-Key` claim holds its key before it counts as abandoned,
/// as after a crash.
pub const IDEMPOTENCY_LEASE: Duration = Duration::from_secs(10 * 60);

const MAX_IDEMPOTENT_BODY: usize = 64 * 1024 * 1024;
const MAX_KEY_LEN: usize = 255;

/// The instance switches, as the API shows them.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[schema(as = WriteSwitches)]
pub struct WriteSwitchesDto {
    /// Any write to a mail, contacts or calendar server.
    pub enabled: bool,
    /// Sending mail; needs `enabled` too.
    pub send: bool,
    /// Running rule flows; needs `enabled` too.
    pub rules: bool,
}

impl From<WriteSwitches> for WriteSwitchesDto {
    fn from(switches: WriteSwitches) -> Self {
        Self {
            enabled: switches.enabled,
            send: switches.send,
            rules: switches.rules,
        }
    }
}

/// The answer to a refused check: `404` for what the user may not know about, `403` for a closed
/// lock, `500` when the store failed.
pub(crate) fn denied(error: AccessError) -> ApiError {
    match error {
        AccessError::NotFound(what) => ApiError::not_found(what),
        AccessError::Denied(denial) => ApiError(StatusCode::FORBIDDEN, denial.message().into()),
        AccessError::ConsentMissing => ApiError(StatusCode::FORBIDDEN, error.to_string()),
        AccessError::Store(error) => error.into(),
    }
}

/// The write decision for this request's state. Holds a handle to the store and a copy of the
/// switches, so it is cheap to make per call.
pub(crate) fn access(state: &ApiState) -> Access {
    Access::new(state.store.clone(), state.write)
}

/// The user's role in the archive, as [`Access::role_in`] decides it.
pub(crate) async fn role_in(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
) -> ApiResult<Option<GrantRole>> {
    Ok(access(state).role_in(user, archive).await?)
}

/// [`Access::may_write`]: an account in an archive the user may not read answers `404`, every
/// other refusal `403`.
pub(crate) async fn may_write(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    account: AccountId,
    capability: Capability,
) -> ApiResult<Account> {
    access(state)
        .may_write(user, scopes, account, capability)
        .await
        .map_err(denied)
}

/// The mailbox capabilities the user may use on the account now, by name
/// ([`Access::writable`]).
pub(crate) async fn writable(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    account: &Account,
) -> ApiResult<Vec<String>> {
    Ok(access(state)
        .writable(user, scopes, account)
        .await?
        .into_iter()
        .map(|c| c.as_str().to_owned())
        .collect())
}

/// Why someone has to sign in again before mail can be changed on the account: a mailbox
/// capability is on, but the sign-in lacks the consent ([`holtmail_access::write_consent_missing`]).
/// `None` otherwise.
pub(crate) async fn write_consent_reason(
    state: &ApiState,
    account: &Account,
) -> ApiResult<Option<String>> {
    if !holtmail_access::write_consent_missing(account) {
        return Ok(None);
    }
    let capabilities = state.store.account_capabilities(account.id).await?;
    Ok(holtmail_access::writes_mail(&capabilities).then(|| {
        "Changing mail on the server is turned on, but the sign-in does not allow Microsoft to change mail. Sign in again to give that permission; until then changes wait.".to_owned()
    }))
}

/// Records a write in the audit log. A write whose entry cannot be stored fails, so nothing is
/// written unrecorded; callers record before or in place of reporting success.
pub(crate) async fn audit(
    state: &ApiState,
    user: &User,
    kind: AuditKind,
    archive: Option<ArchiveId>,
    account: Option<AccountId>,
    targets: Vec<String>,
) -> ApiResult<()> {
    let count = targets.len() as u64;
    state
        .store
        .append_audit(&NewAuditEntry {
            at: Timestamp::now(),
            user: Some(user.id),
            kind,
            archive,
            account,
            targets,
            count,
        })
        .await?;
    Ok(())
}

fn refuse(status: StatusCode, message: &str) -> Response {
    ApiError(status, message.into()).into_response()
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_LEN && key.bytes().all(|b| b.is_ascii_graphic())
}

fn keepable(response: &Response) -> bool {
    let headers = response.headers();
    !response.status().is_server_error()
        && !headers.contains_key(header::SET_COOKIE)
        && !headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("no-store"))
}

fn replay(stored: StoredResponse) -> Response {
    let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    let mut response = (status, stored.body).into_response();
    let headers = response.headers_mut();
    headers.remove(header::CONTENT_TYPE);
    if let Some(value) = stored
        .content_type
        .and_then(|v| HeaderValue::from_str(&v).ok())
    {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert("idempotent-replayed", HeaderValue::from_static("true"));
    response
}

/// Makes every mutating request that carries an `Idempotency-Key` header safe to retry: the same
/// key from the same user on the same method and path within [`IDEMPOTENCY_WINDOW`] gets the
/// first response again, with `Idempotent-Replayed: true`, and the handler does not run twice.
/// A key reused with another body answers `422`, one whose first request still runs `409`, and a
/// malformed key (not 1–255 visible ASCII characters) `400`. Server errors, responses with
/// `Set-Cookie` and responses marked `Cache-Control: no-store` are not kept, so their request may
/// run again. A request that never finishes (the client went away, the handler panicked)
/// releases its key; after a crash an unfinished claim is taken over once
/// [`IDEMPOTENCY_LEASE`] has passed. Requests without the header pass unchanged.
pub(crate) async fn idempotency(State(state): AppState, request: Request, next: Next) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let Some(value) = request.headers().get("idempotency-key") else {
        return next.run(request).await;
    };
    let Some(key) = value
        .to_str()
        .ok()
        .filter(|k| valid_key(k))
        .map(str::to_owned)
    else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key must be 1 to 255 visible ASCII characters",
        );
    };
    let Some(CurrentUser(user)) = request.extensions().get::<CurrentUser>().cloned() else {
        return next.run(request).await;
    };
    let key = IdempotencyKey {
        user: user.id,
        key,
        route: format!("{} {}", request.method(), request.uri().path()),
    };
    let (parts, body) = request.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_IDEMPOTENT_BODY).await else {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
    };
    let fingerprint = blake3::hash(&bytes).to_hex().to_string();
    let now = Timestamp::now();
    let since = Timestamp::from_millis(now.as_millis() - IDEMPOTENCY_WINDOW.as_millis() as i64);
    let abandoned = Timestamp::from_millis(now.as_millis() - IDEMPOTENCY_LEASE.as_millis() as i64);
    match state
        .store
        .claim_idempotency_key(&key, &fingerprint, now, since, abandoned)
        .await
    {
        Ok(IdempotencyClaim::Claimed) => {}
        Ok(IdempotencyClaim::InFlight) => {
            return refuse(
                StatusCode::CONFLICT,
                "a request with this Idempotency-Key is still running",
            );
        }
        Ok(IdempotencyClaim::Mismatch) => {
            return refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                "this Idempotency-Key was used with another request body",
            );
        }
        Ok(IdempotencyClaim::Done(stored)) => return replay(stored),
        Err(error) => return ApiError::from(error).into_response(),
    }
    let mut held = HeldKey {
        state: state.clone(),
        key: Some(key.clone()),
    };
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if !keepable(&response) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(body) = to_bytes(body, usize::MAX).await else {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, "response failed");
    };
    let stored = StoredResponse {
        status: parts.status.as_u16(),
        content_type: parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: body.to_vec(),
    };
    match state.store.complete_idempotency_key(&key, &stored).await {
        Ok(()) => held.key = None,
        Err(error) => tracing::warn!(%error, "idempotent response not kept"),
    }
    Response::from_parts(parts, Body::from(body))
}

/// Releases a claimed key unless its response was kept, also when the request future is dropped
/// (a client that went away) or a handler panicked.
struct HeldKey {
    state: Arc<ApiState>,
    key: Option<IdempotencyKey>,
}

impl Drop for HeldKey {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        let store = self.state.store.clone();
        let release = async move {
            if let Err(error) = store.release_idempotency_key(&key).await {
                tracing::warn!(%error, "idempotency key not released");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(release);
            }
            Err(_) => tracing::warn!("idempotency key left to its lease"),
        }
    }
}

/// An account's write capabilities.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountCapabilitiesDto {
    pub account: String,
    /// Capabilities turned on for the account: `mail.flags`, `mail.move`, `mail.delete`,
    /// `mail.expunge`, `mail.append`, `mail.send`, `pim.edit`, `pim.respond`.
    pub capabilities: Vec<String>,
    /// The ones in effect: those turned on whose instance switch is on too.
    pub active: Vec<String>,
    /// The instance switches from the config.
    pub switches: WriteSwitchesDto,
}

/// Replaces an account's capabilities.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AccountCapabilitiesRequest {
    /// Every capability to have on; the rest are turned off.
    pub capabilities: Vec<String>,
}

fn capabilities_dto(
    account: AccountId,
    capabilities: &Capabilities,
    switches: WriteSwitches,
) -> AccountCapabilitiesDto {
    let names = |c: &mut dyn Iterator<Item = Capability>| c.map(|c| c.as_str().into()).collect();
    AccountCapabilitiesDto {
        account: account.to_string(),
        capabilities: names(&mut capabilities.iter()),
        active: names(&mut capabilities.iter().filter(|c| switches.allow(*c))),
        switches: switches.into(),
    }
}

async fn managed_account(state: &ApiState, user: &User, raw: &str) -> ApiResult<Account> {
    let id: AccountId = raw.parse().map_err(|_| ApiError::not_found("account"))?;
    let account = state
        .store
        .get_account(id)
        .await?
        .ok_or_else(|| ApiError::not_found("account"))?;
    if !manages(user) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner and admins can do this".into(),
        ));
    }
    Ok(account)
}

#[utoipa::path(
    get,
    path = "/api/accounts/{id}/capabilities",
    params(("id" = String, Path, description = "Account id")),
    responses((status = 200, body = AccountCapabilitiesDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_capabilities(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<AccountCapabilitiesDto>> {
    let account = managed_account(&state, &user, &raw).await?;
    let capabilities = state.store.account_capabilities(account.id).await?;
    Ok(Json(capabilities_dto(
        account.id,
        &capabilities,
        state.write,
    )))
}

#[utoipa::path(
    put,
    path = "/api/accounts/{id}/capabilities",
    params(("id" = String, Path, description = "Account id")),
    request_body = AccountCapabilitiesRequest,
    responses((status = 200, body = AccountCapabilitiesDto), (status = 400, description = "An unknown capability, or mail.expunge without mail.delete", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn put_capabilities(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<AccountCapabilitiesRequest>,
) -> ApiResult<Json<AccountCapabilitiesDto>> {
    let account = managed_account(&state, &user, &raw).await?;
    let capabilities = request
        .capabilities
        .iter()
        .map(|name| {
            Capability::parse(name)
                .ok_or_else(|| ApiError::bad_request(format!("unknown capability {name:?}")))
        })
        .collect::<ApiResult<Capabilities>>()?;
    if capabilities.has(Capability::MailExpunge) && !capabilities.has(Capability::MailDelete) {
        return Err(ApiError::bad_request("mail.expunge needs mail.delete"));
    }
    let before = state.store.account_capabilities(account.id).await?;
    if before != capabilities {
        audit(
            &state,
            &user,
            AuditKind::CapabilitiesChanged,
            Some(account.archive_id),
            Some(account.id),
            capabilities.iter().map(|c| c.as_str().to_owned()).collect(),
        )
        .await?;
        state
            .store
            .set_account_capabilities(account.id, &capabilities)
            .await?;
    }
    Ok(Json(capabilities_dto(
        account.id,
        &capabilities,
        state.write,
    )))
}

/// A user's grant on an archive.
#[derive(Debug, Deserialize, ToSchema)]
pub struct GrantRequest {
    /// `reader`, `editor` or `sender`. Only the owner gives `editor` and `sender`.
    pub role: String,
}

fn parse_user(raw: &str) -> ApiResult<UserId> {
    raw.parse().map_err(|_| ApiError::not_found("user"))
}

fn parse_archive(raw: &str) -> ApiResult<ArchiveId> {
    raw.parse().map_err(|_| ApiError::not_found("archive"))
}

async fn grant_target(state: &ApiState, actor: &User, raw: &str) -> ApiResult<User> {
    let target = state
        .store
        .get_user(parse_user(raw)?)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    if target.role == InstanceRole::Owner && actor.role != InstanceRole::Owner {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only an owner can manage owners".into(),
        ));
    }
    Ok(target)
}

#[utoipa::path(
    put,
    path = "/api/users/{id}/grants/{archive}",
    params(("id" = String, Path, description = "User id"), ("archive" = String, Path, description = "Archive id")),
    request_body = GrantRequest,
    responses((status = 204, description = "Granted with this role"), (status = 400, description = "An unknown role", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, or an admin giving editor or sender", body = ErrorDto), (status = 404, description = "No such user or archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn put_grant(
    State(state): AppState,
    Extension(CurrentUser(actor)): Extension<CurrentUser>,
    Path((raw_user, raw_archive)): Path<(String, String)>,
    Json(request): Json<GrantRequest>,
) -> ApiResult<StatusCode> {
    let role = GrantRole::parse(&request.role)
        .ok_or_else(|| ApiError::bad_request("role is one of reader, editor and sender"))?;
    if role > GrantRole::Reader && actor.role != InstanceRole::Owner {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner gives editor and sender grants".into(),
        ));
    }
    let target = grant_target(&state, &actor, &raw_user).await?;
    let archive = parse_archive(&raw_archive)?;
    if state.store.get_archive(archive).await?.is_none() {
        return Err(ApiError::not_found("archive"));
    }
    let current = state
        .store
        .grant_roles(target.id)
        .await?
        .into_iter()
        .find(|g| g.archive == archive)
        .map(|g| g.role);
    if current.is_some_and(|c| c > GrantRole::Reader) && actor.role != InstanceRole::Owner {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner takes away editor and sender grants".into(),
        ));
    }
    audit(
        &state,
        &actor,
        AuditKind::GrantChanged,
        Some(archive),
        None,
        vec![target.id.to_string(), role.as_str().to_owned()],
    )
    .await?;
    state
        .store
        .set_grant_role(target.id, archive, Some(role))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/users/{id}/grants/{archive}",
    params(("id" = String, Path, description = "User id"), ("archive" = String, Path, description = "Archive id")),
    responses((status = 204, description = "No longer granted; idempotent"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, or an admin changing an owner or a writing grant", body = ErrorDto), (status = 404, description = "No such user or archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn delete_grant(
    State(state): AppState,
    Extension(CurrentUser(actor)): Extension<CurrentUser>,
    Path((raw_user, raw_archive)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let target = grant_target(&state, &actor, &raw_user).await?;
    let archive = parse_archive(&raw_archive)?;
    if state.store.get_archive(archive).await?.is_none() {
        return Err(ApiError::not_found("archive"));
    }
    let current = state
        .store
        .grant_roles(target.id)
        .await?
        .into_iter()
        .find(|g| g.archive == archive);
    let Some(current) = current else {
        return Ok(StatusCode::NO_CONTENT);
    };
    if current.role > GrantRole::Reader && actor.role != InstanceRole::Owner {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner takes away editor and sender grants".into(),
        ));
    }
    audit(
        &state,
        &actor,
        AuditKind::GrantChanged,
        Some(archive),
        None,
        vec![target.id.to_string()],
    )
    .await?;
    state.store.set_grant_role(target.id, archive, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Which audit entries to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditParams {
    /// Only entries of this archive.
    pub archive: Option<String>,
    /// Only entries older than this `seq`, for the next page.
    pub before: Option<u64>,
    /// At most this many, 100 by default and 500 at most.
    pub limit: Option<u32>,
}

/// One recorded write: ids and counts only.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEntryDto {
    pub seq: u64,
    /// Unix milliseconds.
    pub at: i64,
    /// The user who made it; missing for the server itself.
    pub user: Option<String>,
    /// `capabilities_changed`, `grant_changed`, `mail_flags`, `mail_move`, `mail_delete`,
    /// `mail_append`, `mail_send`, `pim_edit`, `pim_respond`, `rule_changed`, `transport_changed`,
    /// `identity_changed`, `outbox_changed`, `device_created`, `device_changed`,
    /// `device_revoked` or `device_secret_issued`.
    pub kind: String,
    pub archive: Option<String>,
    pub account: Option<String>,
    /// Ids of what it touched.
    pub targets: Vec<String>,
    pub count: u64,
}

impl From<AuditEntry> for AuditEntryDto {
    fn from(entry: AuditEntry) -> Self {
        let e = entry.entry;
        Self {
            seq: entry.seq,
            at: e.at.as_millis(),
            user: e.user.map(|id| id.to_string()),
            kind: e.kind.as_str().into(),
            archive: e.archive.map(|id| id.to_string()),
            account: e.account.map(|id| id.to_string()),
            targets: e.targets,
            count: e.count,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/audit",
    params(AuditParams),
    responses((status = 200, body = Vec<AuditEntryDto>), (status = 400, description = "A malformed archive id", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not the owner", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_audit(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Query(params): Query<AuditParams>,
) -> ApiResult<Json<Vec<AuditEntryDto>>> {
    if user.role != InstanceRole::Owner {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner reads the audit log".into(),
        ));
    }
    let archive = params
        .archive
        .as_deref()
        .map(|raw| {
            raw.parse::<ArchiveId>()
                .map_err(|_| ApiError::bad_request("malformed archive id"))
        })
        .transpose()?;
    let entries = state
        .store
        .list_audit(&AuditQuery {
            archive,
            before: params.before,
            limit: params.limit.unwrap_or(100).min(500),
        })
        .await?;
    Ok(Json(entries.into_iter().map(Into::into).collect()))
}

/// Runs the rest of the request on its own task, so a client that goes away cannot stop a
/// handler between recording a change and releasing the mail that belongs to it.
pub(crate) async fn detached(request: Request, next: Next) -> Response {
    match tokio::spawn(next.run(request)).await {
        Ok(response) => response,
        Err(_) => refuse(StatusCode::INTERNAL_SERVER_ERROR, "the request failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_keys() {
        assert!(valid_key("3f1c-a9"));
        assert!(!valid_key(""));
        assert!(!valid_key("has space"));
        assert!(!valid_key(&"x".repeat(256)));
    }
}
