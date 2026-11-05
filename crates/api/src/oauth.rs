use std::sync::Arc;

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use holtmail_core::AccountId;
use holtmail_store::AccountKind;

use crate::dto::*;
use crate::manage::{account, account_dto, archive, name, refused};
use crate::{ApiError, ApiResult, ApiState, OAuthStart, OAuthTarget};

type AppState = State<Arc<ApiState>>;

const DEFAULT_RETURN: &str = "/settings";

fn return_path(raw: Option<&str>) -> ApiResult<String> {
    let Some(path) = raw.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(DEFAULT_RETURN.to_owned());
    };
    let safe = path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= 200
        && !path.contains(['\\', '#'])
        && !path.chars().any(char::is_control);
    if !safe {
        return Err(ApiError::bad_request(
            "return_to is a path in the web app, like /settings",
        ));
    }
    Ok(path.to_owned())
}

#[utoipa::path(
    post,
    path = "/api/oauth/start",
    request_body = OAuthStartRequest,
    responses((status = 200, body = OAuthStartDto), (status = 400, description = "An unknown provider or kind, Graph with Google, a missing address, both or neither of archive_id and account_id, an import or JMAP account, or a bad return_to", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive or account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn start(
    State(state): AppState,
    Json(request): Json<OAuthStartRequest>,
) -> ApiResult<Json<OAuthStartDto>> {
    if !matches!(request.provider.as_str(), "google" | "microsoft") {
        return Err(ApiError::bad_request("provider is google or microsoft"));
    }
    let mut kind = match request.kind.as_deref().map(str::trim) {
        None | Some("" | "imap") => AccountKind::Imap,
        Some("graph") => AccountKind::Graph,
        Some(_) => return Err(ApiError::bad_request("kind is imap or graph")),
    };
    let target = match (&request.archive_id, &request.account_id) {
        (Some(raw), None) => OAuthTarget::NewAccount {
            archive: archive(&state, raw).await?,
            name: request
                .name
                .as_deref()
                .map(|n| name(n, "the name"))
                .transpose()?,
        },
        (None, Some(raw)) => {
            let account = account(&state, raw).await?;
            if !matches!(account.kind, AccountKind::Imap | AccountKind::Graph) {
                return Err(ApiError::bad_request(
                    "only IMAP and Graph accounts sign in with Google or Microsoft",
                ));
            }
            kind = account.kind;
            OAuthTarget::Account(account.id)
        }
        _ => {
            return Err(ApiError::bad_request(
                "give either archive_id for a new account or account_id to sign in again",
            ));
        }
    };
    if kind == AccountKind::Graph && request.provider != "microsoft" {
        return Err(ApiError::bad_request(
            "Microsoft Graph accounts sign in with microsoft",
        ));
    }
    let email = request.email.trim().to_owned();
    if matches!(target, OAuthTarget::NewAccount { .. }) && !email.contains('@') {
        return Err(ApiError::bad_request("enter the email address"));
    }
    let started = state
        .mailboxes
        .start_oauth(OAuthStart {
            provider: request.provider,
            email,
            target,
            kind,
            return_to: return_path(request.return_to.as_deref())?,
        })
        .await
        .map_err(refused)?;
    Ok(Json(started.into()))
}

#[utoipa::path(
    post,
    path = "/api/oauth/finish",
    request_body = OAuthFinishRequest,
    responses((status = 200, description = "Signed in; the account is stored and syncing", body = AccountDto), (status = 400, description = "Not a sign-in answer, an unknown or expired sign-in, access refused, or the mail server refused the token", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn finish(
    State(state): AppState,
    Json(request): Json<OAuthFinishRequest>,
) -> ApiResult<Json<AccountDto>> {
    if request.pasted_url.len() > 8 * 1024 {
        return Err(ApiError::bad_request("the pasted address is too long"));
    }
    let done = state
        .mailboxes
        .finish_oauth(&request.pasted_url)
        .await
        .map_err(refused)?;
    Ok(Json(account_dto(&state, done.account)))
}

fn back_to(path: &str, pairs: &[(&str, &str)]) -> Response {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    let separator = if path.contains('?') { '&' } else { '?' };
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, format!("{path}{separator}{query}")),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::REFERRER_POLICY, "no-referrer".to_owned()),
        ],
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/api/oauth/callback",
    params(
        ("code" = Option<String>, Query, description = "The one-time code"),
        ("state" = Option<String>, Query, description = "The sign-in it answers"),
        ("error" = Option<String>, Query, description = "Why access was not granted"),
    ),
    security(()),
    responses((status = 303, description = "Back to the page the sign-in started from, with `oauth=done&account=<id>` or `oauth_error=<reason>` added")),
)]
pub(crate) async fn callback(State(state): AppState, RawQuery(query): RawQuery) -> Response {
    let answer = format!("?{}", query.unwrap_or_default());
    let fallback = state
        .mailboxes
        .oauth_return_to(&answer)
        .unwrap_or_else(|| DEFAULT_RETURN.to_owned());
    match state.mailboxes.finish_oauth(&answer).await {
        Ok(done) => {
            let id: AccountId = done.account.id;
            back_to(
                &done.return_to,
                &[("oauth", "done"), ("account", &id.to_string())],
            )
        }
        Err(error) => back_to(&fallback, &[("oauth_error", &error.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_only_paths_in_the_app() {
        assert_eq!(super::return_path(None).ok().as_deref(), Some("/settings"));
        assert_eq!(
            super::return_path(Some(" ")).ok().as_deref(),
            Some("/settings")
        );
        assert_eq!(
            super::return_path(Some("/welcome")).ok().as_deref(),
            Some("/welcome")
        );
        for bad in [
            "https://example.com/",
            "//example.com/x",
            "/\\example.com",
            "settings",
            "/a\nb",
        ] {
            assert!(super::return_path(Some(bad)).is_err(), "{bad}");
        }
    }
}
