use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Extensions, HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use holtmail_auth::{AuthError, SignedIn, Signup};
use holtmail_serve_net::TrustedProxies;
use holtmail_store::User;
use serde::de::DeserializeOwned;

use crate::dto::*;
use crate::{ApiError, ApiState};

/// The name of the session cookie.
pub const SESSION_COOKIE: &str = "holtmail_session";

type AppState = State<Arc<ApiState>>;
pub(crate) type Reply = Result<Response, Refusal>;

/// The signed-in user, put into the request's extensions for protected routes.
#[derive(Debug, Clone)]
pub struct CurrentUser(pub User);

pub(crate) struct Refusal {
    status: StatusCode,
    message: String,
    retry_after: Option<u64>,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let mut response = ApiError(self.status, self.message).into_response();
        if let Some(secs) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
}

pub(crate) fn error(status: StatusCode, message: impl Into<String>) -> Refusal {
    Refusal {
        status,
        message: message.into(),
        retry_after: None,
    }
}

fn unauthorized() -> Refusal {
    error(StatusCode::UNAUTHORIZED, "sign in required")
}

pub(crate) fn auth_error(failure: AuthError) -> Refusal {
    match failure {
        AuthError::InvalidCredentials => error(StatusCode::UNAUTHORIZED, failure.to_string()),
        AuthError::TooManyAttempts { retry_after } => Refusal {
            retry_after: Some(retry_after.as_secs()),
            ..error(StatusCode::TOO_MANY_REQUESTS, failure.to_string())
        },
        AuthError::InvalidSetupToken => error(StatusCode::FORBIDDEN, failure.to_string()),
        AuthError::EmailTaken => error(StatusCode::CONFLICT, failure.to_string()),
        AuthError::Invalid(message) => error(StatusCode::BAD_REQUEST, message),
        AuthError::Hashing | AuthError::Random | AuthError::Store(_) => {
            error(StatusCode::INTERNAL_SERVER_ERROR, "sign-in failed")
        }
    }
}

pub(crate) fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.trim())
}

fn secure(state: &ApiState, headers: &HeaderMap) -> bool {
    state.secure_cookies
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

fn cookie(value: &str, max_age: u64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure}"
    ))
    .expect("cookie characters are valid in a header")
}

pub(crate) fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(crate) fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, Refusal> {
    serde_json::from_slice(bytes)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "malformed request body"))
}

fn client_ip(state: &ApiState, headers: &HeaderMap, extensions: &Extensions) -> IpAddr {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if state.serve.trusted_proxies.contains(peer) {
        forwarded_client(headers, &state.serve.trusted_proxies).unwrap_or(peer)
    } else {
        peer
    }
}

/// The client address a trusted proxy reports in `X-Forwarded-For`: the rightmost hop that is not
/// itself a trusted proxy, or the leftmost one when all are. Hops to the left of a malformed one
/// are not believed.
pub(crate) fn forwarded_client(headers: &HeaderMap, trusted: &TrustedProxies) -> Option<IpAddr> {
    let hops = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|value| value.to_str().ok())
        .collect::<Option<Vec<_>>>()?
        .join(",");
    let mut client = None;
    for hop in hops.rsplit(',').map(str::trim) {
        let Some(ip) = hop
            .parse::<IpAddr>()
            .ok()
            .or_else(|| hop.parse::<SocketAddr>().ok().map(|socket| socket.ip()))
        else {
            break;
        };
        client = Some(ip.to_canonical());
        if !trusted.contains(ip) {
            break;
        }
    }
    client
}

pub(crate) fn signed_in(state: &ApiState, headers: &HeaderMap, session: SignedIn) -> Response {
    let max_age = state.auth.options().session_lifetime.as_secs();
    let mut response = no_store(
        Json(SessionDto {
            user: session.user.into(),
        })
        .into_response(),
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie(&session.token, max_age, secure(state, headers)),
    );
    response
}

#[utoipa::path(
    get,
    path = "/api/session",
    security(()),
    responses((status = 200, body = SessionDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn current(State(state): AppState, headers: HeaderMap) -> Reply {
    let user = match session_token(&headers) {
        Some(token) => state.auth.session(token).await.map_err(auth_error)?,
        None => None,
    };
    let user = user.ok_or_else(unauthorized)?;
    Ok(no_store(
        Json(SessionDto { user: user.into() }).into_response(),
    ))
}

#[utoipa::path(
    get,
    path = "/api/setup",
    security(()),
    responses((status = 200, body = SetupStatusDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn setup_status(State(state): AppState) -> Reply {
    let needed = state.auth.setup_needed().await.map_err(auth_error)?;
    Ok(no_store(Json(SetupStatusDto { needed }).into_response()))
}

#[utoipa::path(
    post,
    path = "/api/setup",
    security(()),
    request_body = SetupRequest,
    responses(
        (status = 200, description = "The owner was created and signed in; sets the session cookie", body = SessionDto),
        (status = 400, description = "A malformed body, a short password, a malformed email or an empty name", body = ErrorDto),
        (status = 403, description = "Wrong or used setup token, or the instance already has users", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn setup(State(state): AppState, headers: HeaderMap, bytes: Bytes) -> Reply {
    let request: SetupRequest = body(&bytes)?;
    let session = state
        .auth
        .complete_setup(
            &request.token,
            Signup {
                email: request.email,
                name: request.name,
                password: request.password,
            },
        )
        .await
        .map_err(auth_error)?;
    Ok(signed_in(&state, &headers, session))
}

#[utoipa::path(
    post,
    path = "/api/login",
    security(()),
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Signed in; sets the session cookie", body = SessionDto),
        (status = 400, description = "A malformed body", body = ErrorDto),
        (status = 401, description = "Wrong email or password", body = ErrorDto),
        (
            status = 429,
            description = "Too many failed sign-ins",
            body = ErrorDto,
            headers(("Retry-After" = u64, description = "Seconds until another attempt is allowed")),
        ),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn login(
    State(state): AppState,
    headers: HeaderMap,
    extensions: Extensions,
    bytes: Bytes,
) -> Reply {
    let request: LoginRequest = body(&bytes)?;
    let session = state
        .auth
        .login(
            client_ip(&state, &headers, &extensions),
            &request.email,
            &request.password,
        )
        .await
        .map_err(auth_error)?;
    Ok(signed_in(&state, &headers, session))
}

#[utoipa::path(
    post,
    path = "/api/logout",
    security(()),
    responses((status = 204, description = "Signed out; clears the session cookie")),
)]
pub(crate) async fn logout(State(state): AppState, headers: HeaderMap) -> Reply {
    if let Some(token) = session_token(&headers) {
        state.auth.logout(token).await.map_err(auth_error)?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie("", 0, secure(&state, &headers)));
    Ok(response)
}

pub(crate) async fn require_session(
    State(state): AppState,
    mut request: Request,
    next: Next,
) -> Response {
    let user = match session_token(request.headers()) {
        Some(token) => match state.auth.session(token).await {
            Ok(user) => user,
            Err(failure) => return auth_error(failure).into_response(),
        },
        None => None,
    };
    match user {
        Some(user) => {
            request.extensions_mut().insert(CurrentUser(user));
            request.extensions_mut().insert(crate::Scopes::all());
            next.run(request).await
        }
        None => unauthorized().into_response(),
    }
}

pub(crate) async fn guard_writes(request: Request, next: Next) -> Response {
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        if !same_origin(&request) {
            return error(StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
        }
        if *request.method() == Method::POST && !accepted_media(request.headers()) {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected Content-Type: application/json or application/octet-stream",
            )
            .into_response();
        }
    }
    next.run(request).await
}

fn same_origin(request: &Request) -> bool {
    let headers = request.headers();
    let fetch_site = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok());
    if fetch_site == Some("cross-site") {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| request.uri().authority().map(|a| a.as_str()));
    let origin_host = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.split_once("://"))
        .map(|(_, rest)| rest);
    match (origin_host, host) {
        (Some(origin), Some(host)) => origin.eq_ignore_ascii_case(host),
        _ => false,
    }
}

fn accepted_media(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|media| {
            media.eq_ignore_ascii_case("application/json")
                || media.eq_ignore_ascii_case("application/octet-stream")
        })
}
