use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::ApiState;

type AppState = State<Arc<ApiState>>;

/// The largest push message accepted; WebDAV-Push messages are a few hundred bytes.
pub(crate) const MAX_MESSAGE: usize = 16 * 1024;

#[utoipa::path(
    post,
    path = "/api/push/dav/{id}",
    params(("id" = String, Path, description = "The push address this server registered")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "A Web Push message encrypted with `aes128gcm` (RFC 8291)"),
    security(()),
    responses((status = 201, description = "Accepted; the account's contacts and calendars sync soon"), (status = 410, description = "Not a push address of this server, or a message it cannot decrypt")),
)]
pub(crate) async fn receive(
    State(state): AppState,
    Path(id): Path<String>,
    body: Bytes,
) -> StatusCode {
    let plausible = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plausible && state.mailboxes.receive_push(&id, &body).await {
        StatusCode::CREATED
    } else {
        StatusCode::GONE
    }
}
