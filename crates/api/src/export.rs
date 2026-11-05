use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use holtmail_core::{ArchiveId, FolderId};
use holtmail_export::{ExportError, Exporter, Format, Selection};
use tokio::io::AsyncReadExt;

use crate::dto::*;
use crate::routes::disposition;
use crate::{ApiError, ApiResult, ApiState};

const CHUNK: usize = 64 * 1024;

#[utoipa::path(
    get,
    path = "/api/archives/{id}/export",
    params(("id" = String, Path, description = "Archive id"), ExportParams),
    responses(
        (status = 200, description = "The messages as one mbox file (`application/mbox`) or a zip of `.eml` files (`application/zip`), streamed as they are read; a download cut short means the export failed", content_type = "application/octet-stream"),
        (status = 400, description = "Unknown format, more than one of folder, label and q, or a query that does not parse", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 404, description = "No such archive or folder", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn export(
    State(state): State<Arc<ApiState>>,
    Path(raw): Path<String>,
    Query(params): Query<ExportParams>,
) -> ApiResult<Response> {
    let archive: ArchiveId = raw.parse().map_err(|_| ApiError::not_found("archive"))?;
    let format = Format::parse(&params.format)
        .ok_or_else(|| ApiError::bad_request("format is mbox or zip"))?;
    let nonempty = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let selection = match (
        nonempty(&params.folder),
        nonempty(&params.label),
        nonempty(&params.q),
    ) {
        (None, None, None) => Selection::All,
        (Some(folder), None, None) => Selection::Folder(
            folder
                .parse::<FolderId>()
                .map_err(|_| ApiError::not_found("folder"))?,
        ),
        (None, Some(label), None) => Selection::Label(label),
        (None, None, Some(q)) => Selection::Search(
            holtmail_query::parse(&q).map_err(|e| ApiError::bad_request(e.to_string()))?,
        ),
        _ => {
            return Err(ApiError::bad_request(
                "give at most one of folder, label and q",
            ));
        }
    };
    let exporter =
        Exporter::new(state.blobs.clone(), state.store.clone()).with_index(state.index.clone());
    let export =
        exporter
            .prepare(archive, selection, format)
            .await
            .map_err(|error| match error {
                ExportError::NotFound(what) => ApiError::not_found(what),
                ExportError::InvalidSearch(message) => ApiError::bad_request(message),
                other => other.into(),
            })?;
    let file_name = export.file_name();
    let (mut writer, reader) = tokio::io::duplex(CHUNK);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let result = export.write(&mut writer, &|_| {}).await;
        match &result {
            Ok(totals) => tracing::info!(
                archive = %archive,
                messages = totals.messages,
                bytes = totals.bytes,
                skipped = totals.skipped,
                millis = started.elapsed().as_millis() as u64,
                "export written"
            ),
            Err(error) => tracing::warn!(archive = %archive, %error, "export stopped"),
        }
        drop(writer);
        let _ = done_tx.send(result.is_ok());
    });
    let stream = futures::stream::unfold(Some((reader, done_rx)), |state| async move {
        let (mut reader, done) = state?;
        let mut buffer = vec![0; CHUNK];
        match reader.read(&mut buffer).await {
            Ok(0) => match done.await {
                Ok(true) => None,
                _ => Some((Err(std::io::Error::other("the export stopped")), None)),
            },
            Ok(n) => {
                buffer.truncate(n);
                Some((Ok(Bytes::from(buffer)), Some((reader, done))))
            }
            Err(error) => Some((Err(error), None)),
        }
    });
    let mut response = Body::from_stream(stream).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    if let Ok(value) = HeaderValue::from_str(&disposition(&file_name, false)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
