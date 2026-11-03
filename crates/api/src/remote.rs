use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use holtmail_fetch::FetchError;

use crate::dto::ErrorDto;
use crate::routes::load;
use crate::{ApiError, ApiResult, ApiState};

type AppState = State<Arc<ApiState>>;

const SHOWN: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
    "image/x-icon",
    "image/vnd.microsoft.icon",
];

#[utoipa::path(
    get,
    path = "/api/messages/{id}/remote/{key}",
    params(
        ("id" = String, Path, description = "Message id"),
        ("key" = String, Path, description = "BLAKE3 hex of the image URL, as used in the body"),
    ),
    responses(
        (status = 200, description = "The saved copy, or the image fetched now by the server", content_type = "image/*"),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 404, description = "The message links no such image, or it is not an image the server will fetch", body = ErrorDto),
        (status = 502, description = "The remote server could not be reached", body = ErrorDto),
    ),
)]
pub(crate) async fn get_remote(
    State(state): AppState,
    Path((raw, key)): Path<(String, String)>,
) -> ApiResult<Response> {
    remote(&state, &raw, &key, true).await
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/live/{key}",
    params(
        ("id" = String, Path, description = "Message id"),
        ("key" = String, Path, description = "BLAKE3 hex of the image URL, as used in the body"),
    ),
    responses(
        (status = 200, description = "The image fetched now by the server, ignoring any saved copy", content_type = "image/*"),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 404, description = "The message links no such image, or it is not an image the server will fetch", body = ErrorDto),
        (status = 502, description = "The remote server could not be reached", body = ErrorDto),
    ),
)]
pub(crate) async fn get_live(
    State(state): AppState,
    Path((raw, key)): Path<(String, String)>,
) -> ApiResult<Response> {
    remote(&state, &raw, &key, false).await
}

async fn remote(state: &ApiState, raw: &str, key: &str, use_saved: bool) -> ApiResult<Response> {
    let (_, bytes) = load(state, raw).await?;
    let url = holtmail_mime::parse(&bytes)
        .ok()
        .and_then(|parsed| parsed.html)
        .map(|html| holtmail_sanitize::remote_images(&html))
        .unwrap_or_default()
        .into_iter()
        .find(|url| holtmail_sanitize::remote_key(url) == key)
        .ok_or_else(|| ApiError::not_found("remote image"))?;

    if use_saved
        && let Some(copy) = state.store.remote_copy(key).await?
        && let (Some(blob), Some(content_type)) = (copy.blob_id, copy.content_type)
        && let Some(bytes) = state.blobs.get(&blob).await?
    {
        return Ok(image(
            bytes.to_vec(),
            &content_type,
            "private, max-age=31536000, immutable",
        ));
    }

    match state.fetcher.image(&url).await {
        Ok(fetched) => Ok(image(
            fetched.bytes.to_vec(),
            &fetched.content_type,
            "private, max-age=86400",
        )),
        Err(FetchError::Refused(_)) => Err(ApiError::not_found("remote image")),
        Err(FetchError::Unavailable(reason)) => Err(ApiError(StatusCode::BAD_GATEWAY, reason)),
    }
}

fn image(bytes: Vec<u8>, content_type: &str, cache: &'static str) -> Response {
    let content_type = if SHOWN.contains(&content_type) {
        content_type
    } else {
        "application/octet-stream"
    };
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    response
}
