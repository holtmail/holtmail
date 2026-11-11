use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_core::{AccountId, ArchiveId, IdentityId, Timestamp, TransportId, UserId};
use holtmail_outbox::{Outbox, OutboxError, Transports};
use holtmail_secret::SecretBox;
use holtmail_store::{
    AuditKind, GrantRole, Identity, NewIdentity, SentCopyPolicy, Transport, TransportKind,
    TransportLimits, User,
};
use holtmail_submission::{Submission, SubmissionError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::access::manages;
use crate::dto::ErrorDto;
use crate::write::{audit, role_in};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// What the API sends mail with: the outbox, the server's way of building a transport's
/// submitter, and the secret box that seals transport credentials with context
/// `transport:<id>`.
#[derive(Clone)]
pub struct Sending {
    /// The queue every message goes through.
    pub outbox: Arc<Outbox>,
    /// Builds the submitter of a stored transport; also used by the transport check.
    pub transports: Arc<dyn Transports>,
    /// Seals transport passwords before they are stored.
    pub secrets: Arc<dyn SecretBox>,
    /// Takes a composed message from a person into the queue, with every lock checked again.
    pub submission: Arc<dyn Submission>,
    /// Delivers invitations, updates, cancellations and answers, shared with CalDAV and SMTP
    /// submission so nothing is sent twice.
    pub scheduler: Arc<dyn holtmail_schedule::Scheduler>,
}

pub(crate) fn sending(state: &ApiState) -> ApiResult<&Sending> {
    state.sending.as_ref().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "this server does not send mail".into(),
        )
    })
}

/// The stored bytes of a blob; a missing blob is a server failure.
pub(crate) async fn blob(
    state: &ApiState,
    id: &holtmail_blob::BlobId,
) -> ApiResult<axum::body::Bytes> {
    state.blobs.get(id).await?.ok_or_else(|| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored bytes are missing".into(),
        )
    })
}

pub(crate) fn forbidden(message: &str) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, message.into())
}

pub(crate) fn outbox_error(error: OutboxError) -> ApiError {
    match error {
        OutboxError::Invalid(reason) => ApiError::bad_request(reason),
        OutboxError::NotFound(what) => ApiError::not_found(what),
        OutboxError::State(state) => ApiError(
            StatusCode::CONFLICT,
            format!("not possible while {}", state.as_str()),
        ),
        other => ApiError(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}

pub(crate) fn submission_error(error: SubmissionError) -> ApiError {
    match error {
        SubmissionError::Forbidden(reason) => ApiError(StatusCode::FORBIDDEN, reason),
        SubmissionError::FromMismatch => {
            ApiError::bad_request("the message's From is not the route's")
        }
        SubmissionError::Leak => ApiError(StatusCode::CONFLICT, error.to_string()),
        SubmissionError::TooLarge { .. } => {
            ApiError::bad_request("the message is larger than the transport takes")
        }
        SubmissionError::RateLimited { .. } => {
            ApiError(StatusCode::TOO_MANY_REQUESTS, error.to_string())
        }
        SubmissionError::Invalid(reason) => ApiError::bad_request(reason),
        SubmissionError::NotFound(what) => ApiError::not_found(what),
        SubmissionError::TooLate(_) => ApiError(StatusCode::CONFLICT, error.to_string()),
        SubmissionError::Unavailable(_) => {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        }
    }
}

pub(crate) fn parse_archive(raw: &str) -> ApiResult<ArchiveId> {
    raw.parse().map_err(|_| ApiError::not_found("archive"))
}

/// The user's role in the archive, or `404` when they may not read it.
pub(crate) async fn role(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
) -> ApiResult<GrantRole> {
    if state.store.get_archive(archive).await?.is_none() {
        return Err(ApiError::not_found("archive"));
    }
    role_in(state, user, archive)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))
}

async fn managed_archive(state: &ApiState, user: &User, archive: ArchiveId) -> ApiResult<()> {
    role(state, user, archive).await?;
    if !manages(user) {
        return Err(forbidden("only the owner and admins can do this"));
    }
    Ok(())
}

fn millis(t: Timestamp) -> i64 {
    t.as_millis()
}

/// Limits of a transport; `null` is no limit of Holtmail's own.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct TransportLimitsDto {
    /// The largest message in bytes.
    #[serde(default)]
    pub max_size: Option<u64>,
    /// The most envelope recipients of one message.
    #[serde(default)]
    pub max_recipients: Option<u32>,
    /// The most messages handed over in any hour.
    #[serde(default)]
    pub per_hour: Option<u32>,
}

impl From<TransportLimits> for TransportLimitsDto {
    fn from(l: TransportLimits) -> Self {
        Self {
            max_size: l.max_size,
            max_recipients: l.max_recipients,
            per_hour: l.per_hour,
        }
    }
}

impl From<TransportLimitsDto> for TransportLimits {
    fn from(l: TransportLimitsDto) -> Self {
        Self {
            max_size: l.max_size,
            max_recipients: l.max_recipients,
            per_hour: l.per_hour,
        }
    }
}

/// A way out for mail. The password is never shown.
#[derive(Debug, Serialize, ToSchema)]
pub struct TransportDto {
    pub id: String,
    /// The archive it belongs to; `null` for an instance transport.
    pub archive: Option<String>,
    pub name: String,
    /// `smtp`, `jmap`, `graph` or `account`.
    pub kind: String,
    /// For `account`: the account whose connection it uses.
    pub account: Option<String>,
    /// For `jmap` and `graph`: the server's address.
    pub url: Option<String>,
    /// For `jmap`: `password` or `token`.
    pub auth: Option<String>,
    /// For `graph`: the mailbox it sends as; `null` for the token's own.
    pub mailbox: Option<String>,
    /// SMTP server; for `account`, an override of the default the server picks.
    pub host: Option<String>,
    pub port: Option<u16>,
    /// `tls`, `starttls` or `none`.
    pub security: Option<String>,
    pub username: Option<String>,
    /// Whether a password is stored.
    pub has_password: bool,
    /// Address patterns it may send as.
    pub allowed_from: Vec<String>,
    pub limits: TransportLimitsDto,
    pub enabled: bool,
    /// Unix milliseconds.
    pub created_at: i64,
    /// For an instance transport: the archives it is granted to.
    pub grants: Vec<String>,
}

fn setting<'a>(settings: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
}

pub(crate) fn transport_dto(t: &Transport, grants: Vec<ArchiveId>) -> TransportDto {
    TransportDto {
        id: t.id.to_string(),
        archive: t.archive_id.map(|a| a.to_string()),
        name: t.name.clone(),
        kind: t.kind.as_str().into(),
        account: t.account_id.map(|a| a.to_string()),
        url: setting(&t.settings, "url").map(str::to_owned),
        auth: setting(&t.settings, "auth").map(str::to_owned),
        mailbox: setting(&t.settings, "mailbox").map(str::to_owned),
        host: setting(&t.settings, "host").map(str::to_owned),
        port: t
            .settings
            .get("port")
            .and_then(|v| v.as_u64())
            .and_then(|p| u16::try_from(p).ok()),
        security: setting(&t.settings, "security").map(str::to_owned),
        username: setting(&t.settings, "username").map(str::to_owned),
        has_password: t.sealed_secret.is_some(),
        allowed_from: t.allowed_from.clone(),
        limits: t.limits.into(),
        enabled: t.enabled,
        created_at: millis(t.created_at),
        grants: grants.into_iter().map(|a| a.to_string()).collect(),
    }
}

/// A transport to add.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TransportRequest {
    pub name: String,
    /// `smtp`, `jmap` (JMAP submission), `graph` (Microsoft Graph `sendMail`), or `account` to
    /// send through an account's own server and sign-in (SMTP for IMAP accounts, JMAP for JMAP
    /// accounts, Graph for Microsoft Graph accounts).
    pub kind: String,
    /// For `account`: the account, in the same archive.
    #[serde(default)]
    pub account: Option<String>,
    /// For `smtp` required; for `account` it overrides the server's choice.
    #[serde(default)]
    pub host: Option<String>,
    /// 465 for `tls`, 587 otherwise when missing.
    #[serde(default)]
    pub port: Option<u16>,
    /// `tls`, `starttls` (the default) or `none` (only towards localhost).
    #[serde(default)]
    pub security: Option<String>,
    /// Sign-in name; needs `password`. For `account` it replaces the account's own sign-in.
    #[serde(default)]
    pub username: Option<String>,
    /// Sealed before it is stored and never shown again. For `jmap` with `auth` `token` and
    /// for `graph` it is the bearer token.
    #[serde(default)]
    pub password: Option<String>,
    /// For `jmap`: the session address or the server's address; for `graph`: the Graph
    /// endpoint, Microsoft's when missing.
    #[serde(default)]
    pub url: Option<String>,
    /// For `jmap`: `password` (the default; needs `username`) or `token`.
    #[serde(default)]
    pub auth: Option<String>,
    /// For `graph`: the user id or principal name to send as; the token's own mailbox when
    /// missing.
    #[serde(default)]
    pub mailbox: Option<String>,
    /// Address patterns it may send as: `ada@example.com`, `*@example.org`,
    /// `*@*.example.org`. Empty for `account` means the account's own address.
    #[serde(default)]
    pub allowed_from: Vec<String>,
    #[serde(default)]
    pub limits: TransportLimitsDto,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Changes to a transport; missing fields stay as they are.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct TransportUpdateRequest {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub security: Option<String>,
    /// An empty name removes the sign-in together with the password.
    pub username: Option<String>,
    /// Replaces the stored password or token.
    pub password: Option<String>,
    /// For `jmap` and `graph`: a new address needs the password or token again.
    pub url: Option<String>,
    /// For `jmap`: `password` or `token`.
    pub auth: Option<String>,
    /// For `graph`: the mailbox to send as; empty for the token's own.
    pub mailbox: Option<String>,
    pub allowed_from: Option<Vec<String>>,
    pub limits: Option<TransportLimitsDto>,
    pub enabled: Option<bool>,
}

/// The archives an instance transport is granted to.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TransportGrantsRequest {
    pub archives: Vec<String>,
}

/// What a transport check found.
#[derive(Debug, Serialize, ToSchema)]
pub struct TransportCheckDto {
    /// The server took the sign-in; nothing was sent.
    pub ok: bool,
    /// Why not, in the server's words.
    pub error: Option<String>,
    /// The largest message the server takes, when it said so.
    pub max_size: Option<u64>,
}

fn check_patterns(patterns: &[String]) -> ApiResult<Vec<String>> {
    patterns
        .iter()
        .map(|p| p.trim().to_owned())
        .map(|p| {
            if holtmail_route::valid_pattern(&p) {
                Ok(p)
            } else {
                Err(ApiError::bad_request(
                    "allowed_from takes addresses, *@domain and *@*.domain",
                ))
            }
        })
        .collect()
}

fn check_security(security: &str) -> ApiResult<()> {
    match security {
        "tls" | "starttls" | "none" => Ok(()),
        _ => Err(ApiError::bad_request(
            "security is one of tls, starttls and none",
        )),
    }
}

fn put_setting(settings: &mut serde_json::Value, key: &str, value: Option<serde_json::Value>) {
    if !settings.is_object() {
        *settings = serde_json::json!({});
    }
    let map = settings.as_object_mut().expect("an object");
    match value {
        Some(value) => {
            map.insert(key.into(), value);
        }
        None => {
            map.remove(key);
        }
    }
}

fn seal(sending: &Sending, id: TransportId, password: &str) -> ApiResult<Vec<u8>> {
    Ok(sending
        .secrets
        .seal(&format!("transport:{id}"), password.as_bytes())?)
}

async fn build_transport(
    state: &ApiState,
    archive: Option<ArchiveId>,
    request: TransportRequest,
) -> ApiResult<Transport> {
    let sending = sending(state)?;
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("the name is empty"));
    }
    let kind = TransportKind::parse(&request.kind)
        .ok_or_else(|| ApiError::bad_request("kind is smtp, jmap, graph or account"))?;
    if matches!(kind, TransportKind::Jmap | TransportKind::Graph) {
        let name = name.to_owned();
        return build_http_transport(sending, archive, &name, kind, request);
    }
    let account = match (kind, request.account.as_deref()) {
        (TransportKind::Account, Some(raw)) => {
            let id: AccountId = raw
                .parse()
                .map_err(|_| ApiError::bad_request("malformed account id"))?;
            let account = state
                .store
                .get_account(id)
                .await?
                .filter(|a| Some(a.archive_id) == archive)
                .ok_or_else(|| ApiError::bad_request("the account is not in this archive"))?;
            Some(account)
        }
        (TransportKind::Account, None) => {
            return Err(ApiError::bad_request(
                "an account transport needs an account",
            ));
        }
        (_, Some(_)) => {
            return Err(ApiError::bad_request(
                "only account transports name an account",
            ));
        }
        (_, None) => None,
    };
    let host = request
        .host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty());
    if kind == TransportKind::Smtp && host.is_none() {
        return Err(ApiError::bad_request("an SMTP transport needs a host"));
    }
    let security = request.security.as_deref().unwrap_or("starttls");
    check_security(security)?;
    let username = request
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let password = request.password.as_deref().filter(|p| !p.is_empty());
    if username.is_some() != password.is_some() {
        return Err(ApiError::bad_request(
            "a user name and a password go together",
        ));
    }
    let own_server = account
        .as_ref()
        .and_then(|a| setting(&a.settings, "host"))
        .zip(host)
        .is_some_and(|(theirs, ours)| theirs.eq_ignore_ascii_case(ours));
    if kind == TransportKind::Account && host.is_some() && !own_server && password.is_none() {
        return Err(ApiError::bad_request(
            "an account transport sends to another server only with its own user name and password",
        ));
    }
    let mut allowed_from = check_patterns(&request.allowed_from)?;
    if allowed_from.is_empty()
        && let Some(address) = account.as_ref().and_then(|a| a.address.clone())
    {
        allowed_from.push(address.to_lowercase());
    }
    let id = TransportId::new();
    let mut settings = serde_json::json!({});
    if let Some(host) = host {
        put_setting(&mut settings, "host", Some(host.into()));
        let port = request
            .port
            .unwrap_or(if security == "tls" { 465 } else { 587 });
        put_setting(&mut settings, "port", Some(port.into()));
        put_setting(&mut settings, "security", Some(security.into()));
    }
    if let Some(username) = username {
        put_setting(&mut settings, "username", Some(username.into()));
    }
    Ok(Transport {
        id,
        archive_id: archive,
        name: name.to_owned(),
        kind,
        account_id: account.map(|a| a.id),
        settings,
        sealed_secret: password.map(|p| seal(sending, id, p)).transpose()?,
        allowed_from,
        limits: request.limits.into(),
        enabled: request.enabled,
        smtputf8: false,
        created_at: Timestamp::now(),
    })
}

fn check_auth(auth: &str) -> ApiResult<()> {
    match auth {
        "password" | "token" => Ok(()),
        _ => Err(ApiError::bad_request("auth is password or token")),
    }
}

fn check_url(kind: TransportKind, url: &str) -> ApiResult<String> {
    let refused =
        || ApiError::bad_request("the address must use https (http only towards localhost)");
    let raw = url.trim();
    let raw = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    let mut parsed = url::Url::parse(&raw).map_err(|_| refused())?;
    let local = match parsed.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(refused()),
    };
    if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && local)) {
        return Err(refused());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ApiError::bad_request(
            "put the user name and password in their own fields, not in the address",
        ));
    }
    parsed.set_fragment(None);
    if kind == TransportKind::Jmap && (parsed.path().is_empty() || parsed.path() == "/") {
        parsed.set_path("/.well-known/jmap");
    }
    Ok(parsed.to_string())
}

fn build_http_transport(
    sending: &Sending,
    archive: Option<ArchiveId>,
    name: &str,
    kind: TransportKind,
    request: TransportRequest,
) -> ApiResult<Transport> {
    if request.account.is_some() {
        return Err(ApiError::bad_request(
            "only account transports name an account",
        ));
    }
    let id = TransportId::new();
    let mut settings = serde_json::json!({});
    let username = request
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let password = request
        .password
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ApiError::bad_request("a password or token is needed"))?;
    if kind == TransportKind::Jmap {
        let url = request
            .url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| ApiError::bad_request("a JMAP transport needs the server's address"))?;
        put_setting(&mut settings, "url", Some(check_url(kind, url)?.into()));
        let auth = request.auth.as_deref().unwrap_or("password");
        check_auth(auth)?;
        if auth == "password" && username.is_none() {
            return Err(ApiError::bad_request(
                "a user name and a password go together",
            ));
        }
        put_setting(&mut settings, "auth", Some(auth.into()));
        if let Some(username) = username {
            put_setting(&mut settings, "username", Some(username.into()));
        }
    } else {
        if let Some(url) = request.url.as_deref().filter(|u| !u.trim().is_empty()) {
            put_setting(&mut settings, "url", Some(check_url(kind, url)?.into()));
        }
        if let Some(mailbox) = request
            .mailbox
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            put_setting(&mut settings, "mailbox", Some(mailbox.into()));
        }
    }
    Ok(Transport {
        id,
        archive_id: archive,
        name: name.to_owned(),
        kind,
        account_id: None,
        settings,
        sealed_secret: Some(seal(sending, id, password)?),
        allowed_from: check_patterns(&request.allowed_from)?,
        limits: request.limits.into(),
        enabled: request.enabled,
        smtputf8: false,
        created_at: Timestamp::now(),
    })
}

fn update_http_transport(
    sending: &Sending,
    transport: &mut Transport,
    request: &TransportUpdateRequest,
) -> ApiResult<()> {
    let new_secret = request.password.as_deref().filter(|p| !p.is_empty());
    if let Some(url) = request.url.as_deref() {
        let url = if url.trim().is_empty() && transport.kind == TransportKind::Graph {
            None
        } else {
            Some(check_url(transport.kind, url)?)
        };
        if setting(&transport.settings, "url") != url.as_deref() && new_secret.is_none() {
            return Err(ApiError::bad_request(
                "a new address needs the password or token again, so a stored secret never reaches another server",
            ));
        }
        put_setting(&mut transport.settings, "url", url.map(Into::into));
    }
    if transport.kind == TransportKind::Jmap {
        if let Some(auth) = request.auth.as_deref() {
            check_auth(auth)?;
            let current = setting(&transport.settings, "auth").unwrap_or("password");
            if auth != current && new_secret.is_none() {
                return Err(ApiError::bad_request(
                    "another way of signing in needs the password or token again",
                ));
            }
            put_setting(&mut transport.settings, "auth", Some(auth.into()));
        }
        if let Some(username) = request.username.as_deref().map(str::trim) {
            put_setting(
                &mut transport.settings,
                "username",
                (!username.is_empty()).then(|| username.into()),
            );
        }
        if setting(&transport.settings, "auth") == Some("password")
            && setting(&transport.settings, "username").is_none()
        {
            return Err(ApiError::bad_request(
                "a user name and a password go together",
            ));
        }
    }
    if transport.kind == TransportKind::Graph
        && let Some(mailbox) = request.mailbox.as_deref().map(str::trim)
    {
        put_setting(
            &mut transport.settings,
            "mailbox",
            (!mailbox.is_empty()).then(|| mailbox.into()),
        );
    }
    if let Some(secret) = new_secret {
        transport.sealed_secret = Some(seal(sending, transport.id, secret)?);
    }
    Ok(())
}

fn transport_audit(t: &Transport, action: &str) -> Vec<String> {
    vec![t.id.to_string(), action.to_owned()]
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/transports",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, body = Vec<TransportDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_transports(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<TransportDto>>> {
    let archive = parse_archive(&raw)?;
    role(&state, &user, archive).await?;
    let transports = state.store.archive_transports(archive).await?;
    Ok(Json(
        transports
            .iter()
            .map(|t| transport_dto(t, Vec::new()))
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/transports",
    params(("id" = String, Path, description = "Archive id")),
    request_body = TransportRequest,
    responses((status = 201, body = TransportDto), (status = 400, description = "A missing or invalid field", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn create_transport(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<TransportRequest>,
) -> ApiResult<(StatusCode, Json<TransportDto>)> {
    let archive = parse_archive(&raw)?;
    managed_archive(&state, &user, archive).await?;
    let transport = build_transport(&state, Some(archive), request).await?;
    audit(
        &state,
        &user,
        AuditKind::TransportChanged,
        Some(archive),
        transport.account_id,
        transport_audit(&transport, "created"),
    )
    .await?;
    state.store.create_transport(&transport).await?;
    Ok((
        StatusCode::CREATED,
        Json(transport_dto(&transport, Vec::new())),
    ))
}

#[utoipa::path(
    get,
    path = "/api/transports",
    responses((status = 200, body = Vec<TransportDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_instance_transports(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
) -> ApiResult<Json<Vec<TransportDto>>> {
    if !manages(&user) {
        return Err(forbidden("only the owner and admins can do this"));
    }
    let mut out = Vec::new();
    for t in state.store.instance_transports().await? {
        let grants = state.store.transport_grants(t.id).await?;
        out.push(transport_dto(&t, grants));
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/transports",
    request_body = TransportRequest,
    responses((status = 201, body = TransportDto), (status = 400, description = "A missing or invalid field, or an account transport", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn create_instance_transport(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Json(request): Json<TransportRequest>,
) -> ApiResult<(StatusCode, Json<TransportDto>)> {
    if !manages(&user) {
        return Err(forbidden("only the owner and admins can do this"));
    }
    if request.kind == "account" {
        return Err(ApiError::bad_request(
            "an instance transport cannot use an account",
        ));
    }
    let transport = build_transport(&state, None, request).await?;
    audit(
        &state,
        &user,
        AuditKind::TransportChanged,
        None,
        None,
        transport_audit(&transport, "created"),
    )
    .await?;
    state.store.create_transport(&transport).await?;
    Ok((
        StatusCode::CREATED,
        Json(transport_dto(&transport, Vec::new())),
    ))
}

async fn managed_transport(state: &ApiState, user: &User, raw: &str) -> ApiResult<Transport> {
    let id: TransportId = raw.parse().map_err(|_| ApiError::not_found("transport"))?;
    let transport = state
        .store
        .get_transport(id)
        .await?
        .ok_or_else(|| ApiError::not_found("transport"))?;
    match transport.archive_id {
        Some(archive) => managed_archive(state, user, archive).await?,
        None if manages(user) => {}
        None => return Err(ApiError::not_found("transport")),
    }
    Ok(transport)
}

#[utoipa::path(
    patch,
    path = "/api/transports/{id}",
    params(("id" = String, Path, description = "Transport id")),
    request_body = TransportUpdateRequest,
    responses((status = 200, body = TransportDto), (status = 400, description = "An invalid field, or a new host without the password again", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such transport", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn update_transport(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<TransportUpdateRequest>,
) -> ApiResult<Json<TransportDto>> {
    let mut transport = managed_transport(&state, &user, &raw).await?;
    let sending = sending(&state)?;
    if let Some(name) = request.name.as_deref().map(str::trim) {
        if name.is_empty() {
            return Err(ApiError::bad_request("the name is empty"));
        }
        transport.name = name.to_owned();
    }
    let new_password = request.password.as_deref().filter(|p| !p.is_empty());
    let http = matches!(transport.kind, TransportKind::Jmap | TransportKind::Graph);
    if http {
        if request.host.is_some() || request.port.is_some() || request.security.is_some() {
            return Err(ApiError::bad_request(
                "JMAP and Graph transports have an address, not a host",
            ));
        }
        update_http_transport(sending, &mut transport, &request)?;
    } else if request.url.is_some() || request.auth.is_some() || request.mailbox.is_some() {
        return Err(ApiError::bad_request(
            "only JMAP and Graph transports have an address",
        ));
    }
    if let Some(host) = request.host.as_deref().map(str::trim).filter(|_| !http) {
        if host.is_empty() && transport.kind == TransportKind::Smtp {
            return Err(ApiError::bad_request("an SMTP transport needs a host"));
        }
        let account_host = match transport.account_id {
            Some(account) => state
                .store
                .get_account(account)
                .await?
                .and_then(|a| setting(&a.settings, "host").map(str::to_lowercase)),
            None => None,
        };
        let moved = setting(&transport.settings, "host") != Some(host) && !host.is_empty();
        let elsewhere = account_host.as_deref() != Some(host.to_lowercase().as_str());
        if moved
            && new_password.is_none()
            && (transport.sealed_secret.is_some()
                || (transport.kind == TransportKind::Account && elsewhere))
        {
            return Err(ApiError::bad_request(
                "a new host needs the user name and password again, so a stored secret never reaches another server",
            ));
        }
        put_setting(
            &mut transport.settings,
            "host",
            (!host.is_empty()).then(|| host.into()),
        );
    }
    if let Some(security) = request.security.as_deref() {
        check_security(security)?;
        put_setting(&mut transport.settings, "security", Some(security.into()));
    }
    if let Some(port) = request.port {
        put_setting(&mut transport.settings, "port", Some(port.into()));
    }
    match request.username.as_deref().map(str::trim).filter(|_| !http) {
        Some("") => {
            put_setting(&mut transport.settings, "username", None);
            transport.sealed_secret = None;
        }
        Some(username) => {
            put_setting(&mut transport.settings, "username", Some(username.into()));
            if request.password.is_none() && transport.sealed_secret.is_none() {
                return Err(ApiError::bad_request(
                    "a user name and a password go together",
                ));
            }
        }
        None => {}
    }
    if let Some(password) = request
        .password
        .as_deref()
        .filter(|p| !p.is_empty() && !http)
    {
        if setting(&transport.settings, "username").is_none() {
            return Err(ApiError::bad_request(
                "a user name and a password go together",
            ));
        }
        transport.sealed_secret = Some(seal(sending, transport.id, password)?);
    }
    if let Some(patterns) = &request.allowed_from {
        transport.allowed_from = check_patterns(patterns)?;
    }
    if let Some(limits) = request.limits {
        transport.limits = limits.into();
    }
    if let Some(enabled) = request.enabled {
        transport.enabled = enabled;
    }
    audit(
        &state,
        &user,
        AuditKind::TransportChanged,
        transport.archive_id,
        transport.account_id,
        transport_audit(&transport, "changed"),
    )
    .await?;
    state.store.update_transport(&transport).await?;
    let grants = state.store.transport_grants(transport.id).await?;
    Ok(Json(transport_dto(&transport, grants)))
}

#[utoipa::path(
    delete,
    path = "/api/transports/{id}",
    params(("id" = String, Path, description = "Transport id")),
    responses((status = 204, description = "Removed"), (status = 400, description = "An identity still uses it", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such transport", body = ErrorDto)),
)]
pub(crate) async fn delete_transport(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let transport = managed_transport(&state, &user, &raw).await?;
    audit(
        &state,
        &user,
        AuditKind::TransportChanged,
        transport.archive_id,
        transport.account_id,
        transport_audit(&transport, "removed"),
    )
    .await?;
    match state.store.delete_transport(transport.id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(holtmail_store::StoreError::Invalid(_)) => Err(ApiError::bad_request(
            "an identity still sends through this transport",
        )),
        Err(error) => Err(error.into()),
    }
}

#[utoipa::path(
    put,
    path = "/api/transports/{id}/grants",
    params(("id" = String, Path, description = "Transport id")),
    request_body = TransportGrantsRequest,
    responses((status = 200, body = TransportDto), (status = 400, description = "An archive transport, or a removed archive still has an identity using it", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such transport or archive", body = ErrorDto)),
)]
pub(crate) async fn put_transport_grants(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<TransportGrantsRequest>,
) -> ApiResult<Json<TransportDto>> {
    let transport = managed_transport(&state, &user, &raw).await?;
    if transport.archive_id.is_some() {
        return Err(ApiError::bad_request(
            "only instance transports are granted to archives",
        ));
    }
    let archives = request
        .archives
        .iter()
        .map(|raw| parse_archive(raw))
        .collect::<ApiResult<Vec<_>>>()?;
    audit(
        &state,
        &user,
        AuditKind::TransportChanged,
        None,
        None,
        transport_audit(&transport, "granted"),
    )
    .await?;
    match state
        .store
        .set_transport_grants(transport.id, &archives)
        .await
    {
        Ok(()) => {}
        Err(holtmail_store::StoreError::Invalid(reason)) => {
            return Err(ApiError::bad_request(reason));
        }
        Err(error) => return Err(error.into()),
    }
    let grants = state.store.transport_grants(transport.id).await?;
    Ok(Json(transport_dto(&transport, grants)))
}

#[utoipa::path(
    post,
    path = "/api/transports/{id}/check",
    params(("id" = String, Path, description = "Transport id")),
    responses((status = 200, body = TransportCheckDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such transport", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn check_transport(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<TransportCheckDto>> {
    let transport = managed_transport(&state, &user, &raw).await?;
    Ok(Json(run_check(&state, sending(&state)?, &transport).await))
}

pub(crate) async fn run_check(
    state: &ApiState,
    sending: &Sending,
    transport: &Transport,
) -> TransportCheckDto {
    let result = match sending.transports.submitter(transport).await {
        Ok(submitter) => submitter.check().await,
        Err(error) => Err(error),
    };
    match result {
        Ok(limits) => {
            if let Err(error) = state
                .store
                .set_transport_smtputf8(transport.id, limits.smtputf8)
                .await
            {
                tracing::warn!(transport = %transport.id, %error, "transport check not recorded");
            }
            TransportCheckDto {
                ok: true,
                error: None,
                max_size: limits.max_size,
            }
        }
        Err(error) => TransportCheckDto {
            ok: false,
            error: Some(error.to_string()),
            max_size: None,
        },
    }
}

/// An address people may send as.
#[derive(Debug, Serialize, ToSchema)]
pub struct IdentityDto {
    pub id: String,
    pub archive: String,
    pub address: String,
    pub name: Option<String>,
    pub signature_html: Option<String>,
    pub signature_text: Option<String>,
    pub reply_to: Option<String>,
    pub transport: String,
    /// `provider`, `append:<folder>` or `none`.
    pub sent_copy: String,
    pub bcc_self: bool,
    pub account: Option<String>,
    pub alias: Option<String>,
    pub plus_addresses: bool,
    pub active: bool,
    pub created_by: Option<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Users who may send as it.
    pub users: Vec<String>,
    /// Whether it is the current user's default in the archive.
    pub default: bool,
    /// Set when the provider sent mail as this identity with another `From`, as its synced
    /// Sent copy showed; `null` otherwise.
    pub rewritten_from: Option<RewrittenFromDto>,
}

/// The provider rewrote an identity's `From`.
#[derive(Debug, Serialize, ToSchema)]
pub struct RewrittenFromDto {
    /// The `From` address the provider's copy carried.
    pub seen: String,
    /// Unix milliseconds.
    pub at: i64,
    /// What happened and what to do, for people.
    pub explanation: String,
}

fn rewritten_explanation(address: &str, seen: &str) -> String {
    format!(
        "The mail server sent your last message as {seen}, not {address}: it replaced the From address. \
         Recipients saw {seen}. Gmail does this until {address} is added under \
         Settings → Accounts → \"Send mail as\"; other providers need the address allowed for the account. \
         Allow it there, or change this identity to {seen}."
    )
}

pub(crate) fn identity_dto(i: &Identity, users: Vec<UserId>, default: bool) -> IdentityDto {
    let f = &i.fields;
    IdentityDto {
        id: i.id.to_string(),
        archive: f.archive_id.to_string(),
        address: f.address.clone(),
        name: f.name.clone(),
        signature_html: f.signature_html.clone(),
        signature_text: f.signature_text.clone(),
        reply_to: f.reply_to.clone(),
        transport: f.transport_id.to_string(),
        sent_copy: f.sent_copy.to_stored(),
        bcc_self: f.bcc_self,
        account: f.account_id.map(|a| a.to_string()),
        alias: f.alias.clone(),
        plus_addresses: f.plus_addresses,
        active: f.active,
        created_by: f.created_by.map(|u| u.to_string()),
        created_at: millis(i.created_at),
        users: users.into_iter().map(|u| u.to_string()).collect(),
        default,
        rewritten_from: i.rewritten_from.as_ref().map(|r| RewrittenFromDto {
            seen: r.seen.clone(),
            at: millis(r.at),
            explanation: rewritten_explanation(&f.address, &r.seen),
        }),
    }
}

/// An identity to add. It starts inactive unless `active` is set, and becomes active only when
/// its transport may send as the address and the transport check passes.
#[derive(Debug, Deserialize, ToSchema)]
pub struct IdentityRequest {
    pub address: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub signature_html: Option<String>,
    #[serde(default)]
    pub signature_text: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The transport: the archive's own or an instance transport granted to it.
    pub transport: String,
    /// `provider`, `append:<folder>` or `none`; `provider` for account transports and `none`
    /// otherwise when missing.
    #[serde(default)]
    pub sent_copy: Option<String>,
    #[serde(default)]
    pub bcc_self: bool,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub plus_addresses: bool,
    #[serde(default)]
    pub active: bool,
}

/// Changes to an identity; missing fields stay as they are, empty strings clear optional ones.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct IdentityUpdateRequest {
    pub address: Option<String>,
    pub name: Option<String>,
    pub signature_html: Option<String>,
    pub signature_text: Option<String>,
    pub reply_to: Option<String>,
    pub transport: Option<String>,
    pub sent_copy: Option<String>,
    pub bcc_self: Option<bool>,
    pub account: Option<String>,
    pub alias: Option<String>,
    pub plus_addresses: Option<bool>,
    pub active: Option<bool>,
}

/// Who may send as an identity.
#[derive(Debug, Deserialize, ToSchema)]
pub struct IdentityUsersRequest {
    pub users: Vec<String>,
}

/// The current user's default identity in an archive.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DefaultIdentityRequest {
    /// An identity the user may send as; `null` clears the default.
    pub identity: Option<String>,
}

/// An address that could become an identity.
#[derive(Debug, Serialize, ToSchema)]
pub struct IdentitySuggestionDto {
    pub address: String,
    /// `account` (an account's address), `alias` (the alias store) or `sent` (a sender of the
    /// archive's sent mail).
    pub source: String,
    pub account: Option<String>,
    pub alias: Option<String>,
    /// A transport that may send as the address, when the archive has one.
    pub transport: Option<String>,
}

fn optional(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn check_address(address: &str, field: &str) -> ApiResult<String> {
    let address = address.trim().to_lowercase();
    let valid = address.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && domain.contains('.') && !address.contains(char::is_whitespace)
    });
    if valid {
        Ok(address)
    } else {
        Err(ApiError::bad_request(format!("{field} is not an address")))
    }
}

fn parse_sent_copy(raw: &str) -> ApiResult<SentCopyPolicy> {
    SentCopyPolicy::from_stored(raw.trim())
        .ok_or_else(|| ApiError::bad_request("sent_copy is provider, append:<folder> or none"))
}

async fn archive_transport(
    state: &ApiState,
    archive: ArchiveId,
    raw: &str,
) -> ApiResult<Transport> {
    let id: TransportId = raw
        .parse()
        .map_err(|_| ApiError::bad_request("malformed transport id"))?;
    state
        .store
        .archive_transports(archive)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::bad_request("the archive may not use this transport"))
}

async fn archive_account(state: &ApiState, archive: ArchiveId, raw: &str) -> ApiResult<AccountId> {
    let id: AccountId = raw
        .parse()
        .map_err(|_| ApiError::bad_request("malformed account id"))?;
    state
        .store
        .get_account(id)
        .await?
        .filter(|a| a.archive_id == archive)
        .map(|a| a.id)
        .ok_or_else(|| ApiError::bad_request("the account is not in this archive"))
}

/// Refuses to activate an identity unless its transport may send as the address and the
/// transport check passes.
async fn may_activate(state: &ApiState, fields: &NewIdentity) -> ApiResult<()> {
    let sending = sending(state)?;
    let transport =
        archive_transport(state, fields.archive_id, &fields.transport_id.to_string()).await?;
    let allowed = transport
        .allowed_from
        .iter()
        .any(|p| holtmail_route::allows(p, &fields.address));
    if !allowed {
        return Err(ApiError::bad_request(
            "the transport may not send as this address",
        ));
    }
    let check = run_check(state, sending, &transport).await;
    if let Some(error) = check.error {
        return Err(ApiError::bad_request(format!(
            "the transport check failed: {error}"
        )));
    }
    Ok(())
}

async fn identity_view(
    state: &ApiState,
    user: &User,
    identity: &Identity,
) -> ApiResult<IdentityDto> {
    let users = state.store.identity_users(identity.id).await?;
    let default = state
        .store
        .default_identity(user.id, identity.fields.archive_id)
        .await?
        == Some(identity.id);
    Ok(identity_dto(identity, users, default))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/identities",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "Every identity for owners and admins, the ones the user may send as for others", body = Vec<IdentityDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_identities(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<IdentityDto>>> {
    let archive = parse_archive(&raw)?;
    role(&state, &user, archive).await?;
    let identities = if manages(&user) {
        state.store.list_identities(archive).await?
    } else {
        state.store.user_identities(user.id, archive).await?
    };
    let mut out = Vec::with_capacity(identities.len());
    for identity in &identities {
        out.push(identity_view(&state, &user, identity).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/identities/suggestions",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "Addresses of accounts, aliases and sent mail that are no identity yet", body = Vec<IdentitySuggestionDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto)),
)]
pub(crate) async fn identity_suggestions(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<IdentitySuggestionDto>>> {
    let archive = parse_archive(&raw)?;
    managed_archive(&state, &user, archive).await?;
    let mut known: std::collections::HashSet<String> = state
        .store
        .list_identities(archive)
        .await?
        .into_iter()
        .map(|i| i.fields.address.to_lowercase())
        .collect();
    let transports = state.store.archive_transports(archive).await?;
    let transport_for = |address: &str, account: Option<AccountId>| {
        transports
            .iter()
            .filter(|t| t.enabled)
            .filter(|t| {
                t.allowed_from
                    .iter()
                    .any(|p| holtmail_route::allows(p, address))
            })
            .max_by_key(|t| account.is_some() && t.account_id == account)
            .map(|t| t.id.to_string())
    };
    let mut out = Vec::new();
    let mut add = |address: String,
                   source: &str,
                   account: Option<AccountId>,
                   alias: Option<String>,
                   known: &mut std::collections::HashSet<String>| {
        let address = address.trim().to_lowercase();
        if address.contains('@') && known.insert(address.clone()) {
            out.push(IdentitySuggestionDto {
                transport: transport_for(&address, account),
                address,
                source: source.into(),
                account: account.map(|a| a.to_string()),
                alias,
            });
        }
    };
    for account in state.store.list_accounts(archive).await? {
        if let Some(address) = account.address.clone() {
            add(address, "account", Some(account.id), None, &mut known);
        }
    }
    for alias in state.store.aliases(archive).await? {
        let address = alias.address.clone();
        add(address.clone(), "alias", None, Some(address), &mut known);
    }
    for address in state.store.mail_addresses(archive).await?.own {
        add(address, "sent", None, None, &mut known);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/identities",
    params(("id" = String, Path, description = "Archive id")),
    request_body = IdentityRequest,
    responses((status = 201, body = IdentityDto), (status = 400, description = "An invalid field, or activation refused because the transport may not send as the address or its check failed", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn create_identity(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<IdentityRequest>,
) -> ApiResult<(StatusCode, Json<IdentityDto>)> {
    let archive = parse_archive(&raw)?;
    managed_archive(&state, &user, archive).await?;
    let transport = archive_transport(&state, archive, &request.transport).await?;
    let sent_copy = match request.sent_copy.as_deref() {
        Some(raw) => parse_sent_copy(raw)?,
        None if matches!(
            transport.kind,
            TransportKind::Account | TransportKind::Jmap | TransportKind::Graph
        ) =>
        {
            SentCopyPolicy::Provider
        }
        None => SentCopyPolicy::None,
    };
    let account = match optional(request.account) {
        Some(raw) => Some(archive_account(&state, archive, &raw).await?),
        None => None,
    };
    let fields = NewIdentity {
        archive_id: archive,
        address: check_address(&request.address, "address")?,
        name: optional(request.name),
        signature_html: optional(request.signature_html),
        signature_text: optional(request.signature_text),
        reply_to: optional(request.reply_to)
            .map(|r| check_address(&r, "reply_to"))
            .transpose()?,
        transport_id: transport.id,
        sent_copy,
        bcc_self: request.bcc_self,
        account_id: account,
        alias: optional(request.alias).map(|a| a.to_lowercase()),
        plus_addresses: request.plus_addresses,
        active: request.active,
        created_by: Some(user.id),
    };
    if fields.active {
        may_activate(&state, &fields).await?;
    }
    let identity = state.store.create_identity(fields).await?;
    if state
        .store
        .default_identity(user.id, archive)
        .await?
        .is_none()
    {
        state
            .store
            .set_default_identity(user.id, archive, Some(identity.id))
            .await?;
    }
    audit(
        &state,
        &user,
        AuditKind::IdentityChanged,
        Some(archive),
        identity.fields.account_id,
        vec![identity.id.to_string(), "created".into()],
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(identity_view(&state, &user, &identity).await?),
    ))
}

async fn managed_identity(state: &ApiState, user: &User, raw: &str) -> ApiResult<Identity> {
    let id: IdentityId = raw.parse().map_err(|_| ApiError::not_found("identity"))?;
    let identity = state
        .store
        .get_identity(id)
        .await?
        .ok_or_else(|| ApiError::not_found("identity"))?;
    managed_archive(state, user, identity.fields.archive_id)
        .await
        .map_err(|error| match error.0 {
            StatusCode::NOT_FOUND => ApiError::not_found("identity"),
            _ => error,
        })?;
    Ok(identity)
}

#[utoipa::path(
    patch,
    path = "/api/identities/{id}",
    params(("id" = String, Path, description = "Identity id")),
    request_body = IdentityUpdateRequest,
    responses((status = 200, body = IdentityDto), (status = 400, description = "An invalid field, or activation refused", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such identity", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn update_identity(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<IdentityUpdateRequest>,
) -> ApiResult<Json<IdentityDto>> {
    let mut identity = managed_identity(&state, &user, &raw).await?;
    let archive = identity.fields.archive_id;
    let was_active = identity.fields.active;
    let f = &mut identity.fields;
    if let Some(address) = &request.address {
        f.address = check_address(address, "address")?;
    }
    if let Some(name) = request.name {
        f.name = optional(Some(name));
    }
    if let Some(html) = request.signature_html {
        f.signature_html = optional(Some(html));
    }
    if let Some(text) = request.signature_text {
        f.signature_text = optional(Some(text));
    }
    if let Some(reply_to) = request.reply_to {
        f.reply_to = optional(Some(reply_to))
            .map(|r| check_address(&r, "reply_to"))
            .transpose()?;
    }
    if let Some(raw) = &request.transport {
        f.transport_id = archive_transport(&state, archive, raw).await?.id;
    }
    if let Some(raw) = &request.sent_copy {
        f.sent_copy = parse_sent_copy(raw)?;
    }
    if let Some(bcc) = request.bcc_self {
        f.bcc_self = bcc;
    }
    if let Some(raw) = request.account {
        f.account_id = match optional(Some(raw)) {
            Some(raw) => Some(archive_account(&state, archive, &raw).await?),
            None => None,
        };
    }
    if let Some(alias) = request.alias {
        f.alias = optional(Some(alias)).map(|a| a.to_lowercase());
    }
    if let Some(plus) = request.plus_addresses {
        f.plus_addresses = plus;
    }
    if let Some(active) = request.active {
        f.active = active;
    }
    let moved = request.address.is_some() || request.transport.is_some();
    if identity.fields.active && (!was_active || moved) {
        may_activate(&state, &identity.fields).await?;
    }
    audit(
        &state,
        &user,
        AuditKind::IdentityChanged,
        Some(archive),
        identity.fields.account_id,
        vec![identity.id.to_string(), "changed".into()],
    )
    .await?;
    state.store.update_identity(&identity).await?;
    Ok(Json(identity_view(&state, &user, &identity).await?))
}

#[utoipa::path(
    delete,
    path = "/api/identities/{id}",
    params(("id" = String, Path, description = "Identity id")),
    responses((status = 204, description = "Removed; queued messages keep their identity id"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such identity", body = ErrorDto)),
)]
pub(crate) async fn delete_identity(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let identity = managed_identity(&state, &user, &raw).await?;
    audit(
        &state,
        &user,
        AuditKind::IdentityChanged,
        Some(identity.fields.archive_id),
        identity.fields.account_id,
        vec![identity.id.to_string(), "removed".into()],
    )
    .await?;
    state.store.delete_identity(identity.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/api/identities/{id}/users",
    params(("id" = String, Path, description = "Identity id")),
    request_body = IdentityUsersRequest,
    responses((status = 200, body = IdentityDto), (status = 400, description = "A malformed user id", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such identity or user", body = ErrorDto)),
)]
pub(crate) async fn put_identity_users(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<IdentityUsersRequest>,
) -> ApiResult<Json<IdentityDto>> {
    let identity = managed_identity(&state, &user, &raw).await?;
    let users = request
        .users
        .iter()
        .map(|raw| {
            raw.parse::<UserId>()
                .map_err(|_| ApiError::bad_request("malformed user id"))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let mut targets = vec![identity.id.to_string(), "users".into()];
    targets.extend(users.iter().map(|u| u.to_string()));
    audit(
        &state,
        &user,
        AuditKind::IdentityChanged,
        Some(identity.fields.archive_id),
        identity.fields.account_id,
        targets,
    )
    .await?;
    state.store.set_identity_users(identity.id, &users).await?;
    Ok(Json(identity_view(&state, &user, &identity).await?))
}

#[utoipa::path(
    put,
    path = "/api/archives/{id}/default-identity",
    params(("id" = String, Path, description = "Archive id")),
    request_body = DefaultIdentityRequest,
    responses((status = 204, description = "Set or cleared"), (status = 400, description = "Not an identity the user may send as", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not allowed to send in this archive", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto)),
)]
pub(crate) async fn put_default_identity(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Json(request): Json<DefaultIdentityRequest>,
) -> ApiResult<StatusCode> {
    let archive = parse_archive(&raw)?;
    if role(&state, &user, archive).await? < GrantRole::Sender {
        return Err(forbidden(
            "your grant on this archive does not allow sending",
        ));
    }
    let identity = match request.identity.as_deref() {
        Some(raw) => {
            let mine = state.store.user_identities(user.id, archive).await?;
            let found = mine
                .into_iter()
                .find(|i| i.id.to_string() == raw)
                .ok_or_else(|| ApiError::bad_request("not an identity you may send as"))?;
            Some(found.id)
        }
        None => None,
    };
    state
        .store
        .set_default_identity(user.id, archive, identity)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
