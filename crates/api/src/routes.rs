use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use holtmail_core::{ArchiveId, FolderId, MessageId, ThreadId};
use holtmail_index::SortOrder;
use holtmail_store::{Derived, DerivedKind, InsightView, Message, MessageQuery, PageCursor, User};

use crate::dto::*;
use crate::insights::Viewer;
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

fn id<T: std::str::FromStr>(raw: &str, what: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| ApiError::not_found(what))
}

#[utoipa::path(
    get,
    path = "/api/archives",
    responses((status = 200, body = Vec<ArchiveDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_archives(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
) -> ApiResult<Json<Vec<ArchiveDto>>> {
    let readable = crate::access::readable(&state, &user).await?;
    let mut out = Vec::new();
    for archive in state.store.list_archives().await? {
        if !readable.contains(&archive.id) {
            continue;
        }
        out.push(archive_dto(&state, archive).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, body = ArchiveDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_archive(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<ArchiveDto>> {
    let archive_id: ArchiveId = id(&raw, "archive")?;
    let archive = state
        .store
        .get_archive(archive_id)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))?;
    Ok(Json(archive_dto(&state, archive).await?))
}

pub(crate) async fn archive_dto(
    state: &ApiState,
    archive: holtmail_store::Archive,
) -> ApiResult<ArchiveDto> {
    let stats = state.store.archive_stats(archive.id).await?;
    let totals = state.store.remote_copy_totals(archive.id).await?;
    let older_pending = match archive.save_remote_images_since {
        Some(since) => {
            state
                .store
                .count_unscanned(archive.id, holtmail_store::ArchivedWhen::Before(since))
                .await?
        }
        None => 0,
    };
    let remote_images = RemoteImagesDto {
        since: archive.save_remote_images_since.map(|t| t.as_millis()),
        images: totals.images,
        bytes: totals.bytes,
        failed: totals.failed,
        older_pending,
    };
    Ok(ArchiveDto::new(archive, stats, remote_images))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/folders",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, body = Vec<FolderDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_folders(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<FolderDto>>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let folders = state.store.list_folders(archive).await?;
    Ok(Json(folders.into_iter().map(FolderDto::from).collect()))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/labels",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "Labels carried by at least one message, by name", body = Vec<LabelDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_labels(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<LabelDto>>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let labels = state.store.list_labels(archive).await?;
    Ok(Json(labels.into_iter().map(LabelDto::from).collect()))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/messages",
    params(("id" = String, Path, description = "Archive id"), ListParams),
    responses((status = 200, body = MessagePage), (status = 400, description = "A malformed cursor", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive or folder id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_messages(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<MessagePage>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let folder_id = params
        .folder
        .as_deref()
        .map(|f| id::<FolderId>(f, "folder"))
        .transpose()?;
    let page = state
        .store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id,
            label: params.label.filter(|l| !l.is_empty()),
            after: params.after.map(PageCursor),
            limit: params.limit.unwrap_or(50),
        })
        .await
        .map_err(|e| match e {
            holtmail_store::StoreError::Invalid(m) => ApiError::bad_request(m),
            other => other.into(),
        })?;
    Ok(Json(MessagePage {
        items: summaries(&state, &user, archive, &page.items, folder_id).await?,
        next: page.next.map(|c| c.0),
    }))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/search",
    params(("id" = String, Path, description = "Archive id"), SearchParams),
    responses((status = 200, body = SearchResponse), (status = 400, description = "The query does not parse; the message says why", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn search(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Query(params): Query<SearchParams>,
) -> ApiResult<Json<SearchResponse>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let query =
        holtmail_query::parse(&params.q).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let sort = match params.sort.as_deref() {
        Some("relevance") => SortOrder::Relevance,
        _ => SortOrder::Newest,
    };
    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let results = state
        .index
        .search(archive, &query, sort, offset, limit)
        .await
        .map_err(|e| match e {
            holtmail_index::IndexError::Invalid(m) => ApiError::bad_request(m),
            other => other.into(),
        })?;
    let messages = state.store.get_messages(&results.hits).await?;
    let end = u64::from(offset) + results.hits.len() as u64;
    let mut items = summaries(&state, &user, archive, &messages, None).await?;
    for (item, message) in items.iter_mut().zip(&messages) {
        item.attachment_match = results.attachment_hits.contains(&message.id);
    }
    Ok(Json(SearchResponse {
        items,
        total: results.total,
        next_offset: (end < results.total).then(|| u32::try_from(end).unwrap_or(u32::MAX)),
    }))
}

async fn summaries(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    messages: &[Message],
    folder: Option<FolderId>,
) -> ApiResult<Vec<MessageSummary>> {
    let ids: Vec<MessageId> = messages.iter().map(|m| m.id).collect();
    let extras = state.store.message_extras(&ids, folder).await?;
    let viewer = Viewer::new(state, user, archive).await?;
    Ok(messages
        .iter()
        .map(|m| {
            let extra = extras.get(&m.id);
            let kinds = viewer.kinds(m, extra.map_or(0, |e| e.insight_kinds));
            MessageSummary::new(m, extra, kinds)
        })
        .collect())
}

pub(crate) async fn load(state: &ApiState, raw: &str) -> ApiResult<(Message, Vec<u8>)> {
    let message_id: MessageId = id(raw, "message")?;
    let message = state
        .store
        .get_message(message_id)
        .await?
        .ok_or_else(|| ApiError::not_found("message"))?;
    let bytes = state
        .blobs
        .get(&message.blob_id)
        .await?
        .ok_or_else(|| ApiError::not_found("message bytes"))?;
    Ok((message, bytes.to_vec()))
}

async fn detail(
    state: &ApiState,
    message: Message,
    raw: &[u8],
    viewer: &Viewer,
    insight: Option<&InsightView>,
) -> ApiResult<MessageDetail> {
    let insights =
        crate::insights::message_insights(state.insights.as_ref(), insight, &message, viewer);
    let parsed = holtmail_mime::parse(raw).ok();
    let attachments = state.store.attachments(message.id).await?;
    let locations = state.store.locations(message.id).await?;
    let reply_to = parsed
        .as_ref()
        .map(|p| addresses(&p.reply_to))
        .unwrap_or_default();
    let recipients: Vec<String> = [&message.to, &message.cc, &message.bcc]
        .into_iter()
        .flatten()
        .map(|a| a.email.clone())
        .collect();
    let alias =
        crate::aliases::message_alias(state, message.archive_id, message.id, &recipients).await?;
    let people = crate::persons::address_people(
        state,
        message.archive_id,
        [&message.from, &message.to, &message.cc, &message.bcc]
            .into_iter()
            .flatten()
            .map(|a| a.email.clone())
            .chain(reply_to.iter().map(|a| a.email.clone()))
            .chain(
                alias
                    .as_ref()
                    .and_then(|a| a.sender.as_ref())
                    .map(|s| s.email.clone()),
            ),
    )
    .await?;
    Ok(MessageDetail {
        id: message.id.to_string(),
        archive_id: message.archive_id.to_string(),
        thread_id: message.thread_id.to_string(),
        rfc_message_id: message.rfc_message_id.clone(),
        subject: message.subject.clone(),
        from: addresses(&message.from),
        to: addresses(&message.to),
        cc: addresses(&message.cc),
        bcc: addresses(&message.bcc),
        reply_to,
        date: message.date.as_millis(),
        sent_at: message.sent_at.map(|t| t.as_millis()),
        received_at: message.received_at.map(|t| t.as_millis()),
        archived_at: message.archived_at.as_millis(),
        size: message.size,
        preview: message.preview.clone(),
        in_reply_to: parsed
            .as_ref()
            .map(|p| p.in_reply_to.clone())
            .unwrap_or_default(),
        references: parsed
            .as_ref()
            .map(|p| p.references.clone())
            .unwrap_or_default(),
        text: parsed.as_ref().and_then(|p| p.text.clone()),
        has_html: parsed.as_ref().is_some_and(|p| p.html.is_some()),
        attachments: attachments
            .iter()
            .map(|a| attachment_dto(&message, a))
            .collect(),
        locations: locations.into_iter().map(LocationDto::from).collect(),
        labels: state.store.labels(message.id).await?,
        invitations: crate::pim::invitations(state, &message).await?,
        people,
        alias,
        insights,
    })
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}",
    params(("id" = String, Path, description = "Message id")),
    responses((status = 200, body = MessageDetail), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_message(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<MessageDetail>> {
    let (message, bytes) = load(&state, &raw).await?;
    let viewer = Viewer::new(&state, &user, message.archive_id).await?;
    let insight = crate::insights::view(&state, message.id).await?;
    Ok(Json(
        detail(&state, message, &bytes, &viewer, insight.as_ref()).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/api/threads/{id}",
    params(("id" = String, Path, description = "Thread id")),
    responses((status = 200, body = ThreadDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such thread", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_thread(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<ThreadDto>> {
    let thread: ThreadId = id(&raw, "thread")?;
    let messages = state.store.thread_messages(thread).await?;
    let Some(first) = messages.first() else {
        return Err(ApiError::not_found("thread"));
    };
    let viewer = Viewer::new(&state, &user, first.archive_id).await?;
    let ids: Vec<MessageId> = messages.iter().map(|m| m.id).collect();
    let views = crate::insights::views(&state, &ids).await?;
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        let insight = views.get(&message.id);
        let bytes = state
            .blobs
            .get(&message.blob_id)
            .await?
            .ok_or_else(|| ApiError::not_found("message bytes"))?;
        out.push(detail(&state, message, &bytes, &viewer, insight).await?);
    }
    Ok(Json(ThreadDto {
        id: thread.to_string(),
        messages: out,
    }))
}

fn origin(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .filter(|s| *s == "https" || *s == "http")
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/body",
    params(("id" = String, Path, description = "Message id"), BodyParams),
    responses(
        (
            status = 200,
            description = "The sanitized HTML body as a full document",
            content_type = "text/html",
            headers(
                ("x-holtmail-remote-blocked" = String, description = "`1` when remote content was blocked, else `0`"),
                ("x-holtmail-remote-saved" = String, description = "`1` when saved copies of remote images are shown, else `0`"),
            ),
        ),
        (status = 400, description = "The message does not parse", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 404, description = "No such message", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn get_body(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<BodyParams>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (message, bytes) = load(&state, &raw).await?;
    let parsed = holtmail_mime::parse(&bytes).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let html = parsed
        .html
        .unwrap_or_else(|| plain_text_html(parsed.text.as_deref().unwrap_or_default()));
    let origin = origin(&headers);
    let attachments = state.store.attachments(message.id).await?;
    let inline_parts: HashMap<String, String> = attachments
        .iter()
        .filter_map(|a| {
            let cid = a.content_id.clone()?;
            Some((
                cid,
                format!(
                    "{origin}/api/messages/{}/attachments/{}",
                    message.id, a.ordinal
                ),
            ))
        })
        .collect();
    let remote = match params.remote.unwrap_or_default() {
        RemoteMode::Block => holtmail_sanitize::Remote::Block,
        RemoteMode::Proxy => holtmail_sanitize::Remote::Proxy,
        RemoteMode::Direct => holtmail_sanitize::Remote::Direct,
    };
    let use_saved = params.saved.unwrap_or(true);
    let saved = if use_saved {
        state
            .store
            .message_remote_copies(message.id)
            .await?
            .into_iter()
            .map(|copy| copy.key)
            .collect()
    } else {
        Default::default()
    };
    let route = if use_saved { "remote" } else { "live" };
    let sanitized = holtmail_sanitize::sanitize(
        &html,
        &holtmail_sanitize::Options {
            remote,
            proxy_prefix: format!("{origin}/api/messages/{}/{route}/", message.id),
            saved,
            inline_parts,
        },
    );
    let document = holtmail_sanitize::document(
        &sanitized.html,
        remote == holtmail_sanitize::Remote::Direct,
        &origin,
    );
    let mut response = document.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        "x-holtmail-remote-blocked",
        HeaderValue::from_static(if sanitized.remote_blocked { "1" } else { "0" }),
    );
    headers.insert(
        "x-holtmail-remote-saved",
        HeaderValue::from_static(if sanitized.saved_shown { "1" } else { "0" }),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(response)
}

fn plain_text_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!("<pre style=\"white-space: pre-wrap; font: inherit\">{escaped}</pre>")
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/raw",
    params(("id" = String, Path, description = "Message id")),
    responses((status = 200, description = "The original message", content_type = "message/rfc822"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_raw(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Response> {
    let (message, bytes) = load(&state, &raw).await?;
    let name = format!(
        "{}.eml",
        message
            .subject
            .as_deref()
            .map(file_safe)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| message.id.to_string())
    );
    Ok(download(bytes, "message/rfc822", &name, false))
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/pdf",
    params(("id" = String, Path, description = "Message id")),
    responses((status = 200, description = "The message as a PDF with the original message attached inside it", content_type = "application/pdf"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_pdf(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Response> {
    let (message, bytes) = load(&state, &raw).await?;
    let pdf = holtmail_export::message_pdf(&*state.store, &message, bytes.into())
        .await
        .map_err(|e| ApiError(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let name = format!(
        "{}.pdf",
        message
            .subject
            .as_deref()
            .map(file_safe)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| message.id.to_string())
    );
    Ok(download(pdf, "application/pdf", &name, false))
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/attachments/{ordinal}",
    params(("id" = String, Path, description = "Message id"), ("ordinal" = u32, Path, description = "The attachment's position, from 0")),
    responses((status = 200, description = "The attachment's content", content_type = "application/octet-stream"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message or attachment", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_attachment(
    State(state): AppState,
    Path((raw, ordinal)): Path<(String, u32)>,
) -> ApiResult<Response> {
    let message_id: MessageId = id(&raw, "message")?;
    let attachment = state
        .store
        .attachments(message_id)
        .await?
        .into_iter()
        .find(|a| a.ordinal == ordinal)
        .ok_or_else(|| ApiError::not_found("attachment"))?;
    let bytes = state
        .blobs
        .get(&attachment.blob_id)
        .await?
        .ok_or_else(|| ApiError::not_found("attachment bytes"))?;
    let showable = matches!(
        attachment.content_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif" | "application/pdf"
    );
    let content_type = if showable {
        attachment.content_type.as_str()
    } else {
        "application/octet-stream"
    };
    let name = attachment
        .filename
        .as_deref()
        .map(file_safe)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("attachment-{ordinal}"));
    Ok(download(bytes.to_vec(), content_type, &name, showable))
}

static THUMBNAILS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[utoipa::path(
    get,
    path = "/api/messages/{id}/attachments/{ordinal}/thumbnail",
    params(("id" = String, Path, description = "Message id"), ("ordinal" = u32, Path, description = "The attachment's position, from 0")),
    responses((status = 200, description = "A preview of the image at most 320 pixels on its longest side, made on first request and kept", content_type = "image/jpeg"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message or attachment, not a PNG, JPEG, GIF or WebP image, or one that does not decode", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_thumbnail(
    State(state): AppState,
    Path((raw, ordinal)): Path<(String, u32)>,
) -> ApiResult<Response> {
    let message_id: MessageId = id(&raw, "message")?;
    let attachment = state
        .store
        .attachments(message_id)
        .await?
        .into_iter()
        .find(|a| a.ordinal == ordinal)
        .ok_or_else(|| ApiError::not_found("attachment"))?;
    if !holtmail_thumbnail::supports(&attachment.content_type)
        || attachment.size > holtmail_thumbnail::MAX_SOURCE as u64
    {
        return Err(ApiError::not_found("thumbnail"));
    }
    let source = attachment.blob_id;
    match state.store.derived(&source, DerivedKind::Thumbnail).await? {
        Some(Derived::Nothing) => return Err(ApiError::not_found("thumbnail")),
        Some(Derived::Blob(blob)) => {
            if let Some(bytes) = state.blobs.get(&blob).await? {
                return Ok(thumbnail_response(bytes.to_vec()));
            }
        }
        None => {}
    }
    let bytes = state
        .blobs
        .get(&source)
        .await?
        .ok_or_else(|| ApiError::not_found("attachment bytes"))?;
    let made = {
        let _permit = THUMBNAILS.acquire().await?;
        tokio::task::spawn_blocking(move || holtmail_thumbnail::thumbnail(&bytes)).await?
    };
    match made {
        Ok(thumbnail) => {
            let blob = state
                .blobs
                .put(bytes::Bytes::from(thumbnail.bytes.clone()))
                .await?;
            state
                .store
                .set_derived(&source, DerivedKind::Thumbnail, &Derived::Blob(blob))
                .await?;
            Ok(thumbnail_response(thumbnail.bytes))
        }
        Err(_) => {
            state
                .store
                .set_derived(&source, DerivedKind::Thumbnail, &Derived::Nothing)
                .await?;
            Err(ApiError::not_found("thumbnail"))
        }
    }
}

fn thumbnail_response(bytes: Vec<u8>) -> Response {
    let (content_type, name) = if bytes.starts_with(b"\x89PNG") {
        ("image/png", "thumbnail.png")
    } else {
        ("image/jpeg", "thumbnail.jpg")
    };
    download(bytes, content_type, name, true)
}

pub(crate) fn disposition(filename: &str, inline: bool) -> String {
    format!(
        "{}; filename=\"{}\"; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        filename.replace(
            |c: char| !c.is_ascii() || c.is_ascii_control() || c == '"' || c == '\\',
            "_"
        ),
        percent_encode(filename)
    )
}

pub(crate) fn download(
    bytes: Vec<u8>,
    content_type: &str,
    filename: &str,
    inline: bool,
) -> Response {
    let disposition = disposition(filename, inline);
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    response
}

fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(120)
        .collect()
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub(crate) async fn unknown() -> ApiError {
    ApiError::not_found("endpoint")
}

pub(crate) async fn openapi() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::openapi_json(),
    )
}
