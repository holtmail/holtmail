use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use holtmail_core::ArchiveId;
use serde_json::{Map, Value, json};

use crate::auth::{self, Authed};
use crate::request::{CORE, MAIL, SUBMISSION, WEBSOCKET};
use crate::{INSTANCE_CAPABILITY, Inner, blob, ids, mailbox};

/// `GET /.well-known/jmap`: a 307 to the session resource (RFC 8620 §2.2).
pub(crate) async fn well_known(State(inner): State<Arc<Inner>>) -> Response {
    let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
    if let Ok(location) =
        HeaderValue::from_str(&format!("{}/jmap/session", inner.settings.public_url))
    {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response
}

/// The session state: changes when the accounts, the device's rights or the login change.
pub(crate) async fn state(inner: &Inner, authed: &Authed) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(authed.login.to_ascii_lowercase().as_bytes());
    for archive in &authed.principal.archives {
        hasher.update(archive.to_string().as_bytes());
    }
    for scope in &authed.principal.scopes {
        hasher.update(scope.as_str().as_bytes());
    }
    hasher.update(&inner.settings.limits.max_size_upload.to_le_bytes());
    hasher.finalize().to_hex()[..16].to_string()
}

async fn account(inner: &Inner, authed: &Authed, archive: ArchiveId) -> Value {
    let viewer = authed.viewer(archive);
    let tree = inner.services.view.tree(&viewer).await.unwrap_or_default();
    let ctx = crate::request::Ctx {
        inner,
        authed,
        created: Default::default(),
    };
    let may_submit = mailbox::may_submit(&ctx, &viewer).await;
    let writable = may_submit
        || tree
            .iter()
            .any(|m| m.rights.add || m.rights.remove || m.rights.set_seen || m.rights.set_keywords);
    let name = match inner.services.hub.archive_name(archive).await {
        Some(name) => name,
        None => authed.login.clone(),
    };
    let max_upload = blob::max_upload(inner, &viewer).await;
    let mut capabilities = Map::new();
    capabilities.insert(
        MAIL.into(),
        json!({
            "maxMailboxesPerEmail": null,
            "maxMailboxDepth": null,
            "maxSizeMailboxName": 255,
            "maxSizeAttachmentsPerEmail": max_upload,
            "emailQuerySortOptions": ["receivedAt"],
            "mayCreateTopLevelMailbox": false,
        }),
    );
    capabilities.insert(
        SUBMISSION.into(),
        json!({"maxDelayedSend": 0, "submissionExtensions": {}}),
    );
    json!({
        "name": name,
        "isPersonal": true,
        "isReadOnly": !writable,
        "accountCapabilities": capabilities,
    })
}

/// The session resource (RFC 8620 §2).
pub(crate) async fn document(inner: &Inner, authed: &Authed) -> Value {
    let base = &inner.settings.public_url;
    let limits = &inner.settings.limits;
    let mut accounts = Map::new();
    for archive in &authed.principal.archives {
        accounts.insert(
            ids::account(*archive),
            account(inner, authed, *archive).await,
        );
    }
    let primary = authed.principal.archives.first().map(|a| ids::account(*a));
    let max_upload = match authed.principal.archives.first() {
        Some(archive) => blob::max_upload(inner, &authed.viewer(*archive)).await,
        None => limits.max_size_upload,
    };
    let socket = match base.strip_prefix("https://") {
        Some(rest) => format!("wss://{rest}/jmap/ws"),
        None => format!("ws://{}/jmap/ws", base.trim_start_matches("http://")),
    };
    let mut capabilities = Map::new();
    capabilities.insert(
        CORE.into(),
        json!({
            "maxSizeUpload": max_upload,
            "maxConcurrentUpload": limits.max_concurrent_upload,
            "maxSizeRequest": limits.max_size_request,
            "maxConcurrentRequests": limits.max_concurrent_requests,
            "maxCallsInRequest": limits.max_calls_in_request,
            "maxObjectsInGet": limits.max_objects_in_get,
            "maxObjectsInSet": limits.max_objects_in_set,
            "collationAlgorithms": ["i;ascii-casemap"],
        }),
    );
    capabilities.insert(MAIL.into(), json!({}));
    capabilities.insert(SUBMISSION.into(), json!({}));
    capabilities.insert(
        WEBSOCKET.into(),
        json!({"url": socket, "supportsPush": true}),
    );
    if let Some(token) = &inner.settings.instance_token {
        capabilities.insert(INSTANCE_CAPABILITY.into(), json!({"token": token}));
    }
    let mut primaries = Map::new();
    if let Some(primary) = primary {
        primaries.insert(MAIL.into(), json!(primary));
        primaries.insert(SUBMISSION.into(), json!(primary));
    }
    json!({
        "capabilities": capabilities,
        "accounts": accounts,
        "primaryAccounts": primaries,
        "username": authed.login,
        "apiUrl": format!("{base}/jmap/api"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
        "uploadUrl": format!("{base}/jmap/upload/{{accountId}}"),
        "eventSourceUrl": format!("{base}/jmap/eventsource?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": state(inner, authed).await,
    })
}

pub(crate) async fn handle(State(inner): State<Arc<Inner>>, request: Request) -> Response {
    let authed =
        match auth::authenticate(&inner, request.headers(), auth::peer(request.extensions())).await
        {
            Ok(authed) => authed,
            Err(response) => return *response,
        };
    let mut response = Json(document(&inner, &authed).await).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store"),
    );
    response
}
