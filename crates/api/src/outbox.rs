use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use holtmail_access::Denial;
use holtmail_blob::BlobId;
use holtmail_compose::{Attachment, Mailbox};
use holtmail_core::{ArchiveId, OutboxId, Timestamp};
use holtmail_store::{
    AuditKind, OutboxEntry, OutboxQuery, OutboxState, RecipientState, Transport, User,
};
use holtmail_submission::{Actor, Submit};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::access::manages;
use crate::drafts::{
    DraftAttachment, DraftContext, DraftDto, DraftPayload, LeakDto, MailboxDto, draft_dto,
    leak_dtos, routing_for,
};
use crate::dto::ErrorDto;
use crate::send::{
    blob as blob_bytes, forbidden, outbox_error, parse_archive, role, sending, submission_error,
};
use crate::write::{AccessError, Scopes, access, audit, denied, role_in};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// One envelope recipient of an outbox message and what became of it.
#[derive(Debug, Serialize, ToSchema)]
pub struct OutboxRecipientDto {
    pub address: String,
    /// `pending`, `accepted`, `refused`, or `bounced` when the server took it and a delivery
    /// status notification said it could not be delivered.
    pub state: String,
    /// For `refused`: a permanent (`5xx`) refusal.
    pub permanent: Option<bool>,
    pub code: Option<u16>,
    /// For `bounced`: the enhanced status code, such as `5.1.1`.
    pub status: Option<String>,
    /// The server's words.
    pub text: Option<String>,
}

/// A message in the outbox.
#[derive(Debug, Serialize, ToSchema)]
pub struct OutboxDto {
    pub id: String,
    pub archive: String,
    pub identity: String,
    pub transport: String,
    /// Who queued it; `null` for the server itself.
    pub user: Option<String>,
    /// How it is routed, as the router explained it.
    pub route: Option<crate::drafts::RouteDto>,
    /// The return path; empty for `<>`.
    pub mail_from: String,
    /// `draft`, `queued`, `sending`, `retrying`, `sent`, `failed`, `uncertain`, `bounced` or
    /// `cancelled`.
    pub state: String,
    /// Every envelope recipient, Bcc included, in order.
    pub recipients: Vec<OutboxRecipientDto>,
    pub size: u64,
    /// The `Message-ID` in the bytes.
    pub rfc_message_id: String,
    /// The `Subject` in the bytes, decoded; `null` when it has none.
    pub subject: Option<String>,
    /// The archived message, once the first attempt archived it.
    pub message: Option<String>,
    /// The provider's own copy that sync archived with other bytes (a Sent copy the server
    /// rewrote): a second copy of the same message.
    pub synced_copy: Option<String>,
    /// Something learned after sending that is not a failure, such as a delayed delivery.
    pub note: Option<String>,
    /// Unix milliseconds.
    pub send_at: i64,
    pub attempts: u32,
    /// Unix milliseconds.
    pub last_attempt_at: Option<i64>,
    /// Unix milliseconds.
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    /// Unix milliseconds.
    pub sent_at: Option<i64>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds.
    pub updated_at: i64,
    /// The outbox message this one sends again.
    pub resend_of: Option<String>,
    /// The append of the sent copy to the account's folder, for an identity whose sent-copy
    /// policy is `append:<folder>`; `null` when none was recorded.
    pub sent_copy: Option<crate::mail_changes::MailChangeDto>,
}

pub(crate) fn outbox_dto(e: &OutboxEntry) -> OutboxDto {
    OutboxDto {
        id: e.id.to_string(),
        archive: e.new.archive_id.to_string(),
        identity: e.new.identity_id.to_string(),
        transport: e.new.transport_id.to_string(),
        user: e.new.user_id.map(|u| u.to_string()),
        route: serde_json::from_value(e.new.route.clone()).ok(),
        mail_from: e.new.mail_from.clone(),
        state: e.state.as_str().into(),
        recipients: e
            .recipients
            .iter()
            .map(|r| {
                let (state, permanent, code, status, text) = match &r.state {
                    RecipientState::Pending => ("pending", None, None, None, None),
                    RecipientState::Accepted => ("accepted", None, None, None, None),
                    RecipientState::Refused {
                        permanent,
                        code,
                        text,
                    } => (
                        "refused",
                        Some(*permanent),
                        Some(*code),
                        None,
                        Some(text.clone()),
                    ),
                    RecipientState::Bounced { status, text } => {
                        ("bounced", None, None, status.clone(), text.clone())
                    }
                };
                OutboxRecipientDto {
                    address: r.address.clone(),
                    state: state.into(),
                    permanent,
                    code,
                    status,
                    text,
                }
            })
            .collect(),
        size: e.new.size,
        rfc_message_id: e.new.rfc_message_id.clone(),
        subject: None,
        message: e.message_id.map(|m| m.to_string()),
        synced_copy: e.synced_copy.map(|m| m.to_string()),
        note: e.note.clone(),
        send_at: e.new.send_at.as_millis(),
        attempts: e.attempts,
        last_attempt_at: e.last_attempt_at.map(|t| t.as_millis()),
        next_attempt_at: e.next_attempt_at.map(|t| t.as_millis()),
        last_error: e.last_error.clone(),
        sent_at: e.sent_at.map(|t| t.as_millis()),
        created_at: e.new.at.as_millis(),
        updated_at: e.updated_at.as_millis(),
        resend_of: e.new.resend_of.map(|r| r.to_string()),
        sent_copy: None,
    }
}

/// [`outbox_dto`] with the sent copy's change and the subject: the archived message's once there is one, otherwise read
/// from the stored bytes.
pub(crate) async fn outbox_view(state: &ApiState, e: &OutboxEntry) -> ApiResult<OutboxDto> {
    let mut dto = outbox_dto(e);
    dto.sent_copy = state
        .store
        .outbox_mail_changes(e.id)
        .await?
        .into_iter()
        .next_back()
        .map(Into::into);
    dto.subject = match e.message_id {
        Some(id) => state.store.get_message(id).await?.and_then(|m| m.subject),
        None => None,
    };
    if e.message_id.is_none()
        && let Some(bytes) = state.blobs.get(&e.new.blob_id).await?
    {
        dto.subject = holtmail_mime::parse(&bytes).ok().and_then(|p| p.subject);
    }
    Ok(dto)
}

/// When to send a draft.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct SendRequest {
    /// The version shown to the user; another save in between answers `409`.
    #[serde(default)]
    pub version: Option<u32>,
    /// Unix milliseconds; overrides the draft's `send_at`.
    #[serde(default)]
    pub send_at: Option<i64>,
    /// Send even though recipients would see the real mailbox behind an alias; audited.
    #[serde(default)]
    pub allow_leaks: bool,
}

/// Why a send was refused: recipients would see the real mailbox behind an alias.
#[derive(Debug, Serialize, ToSchema)]
pub struct LeaksDto {
    pub error: String,
    pub leaks: Vec<LeakDto>,
}

/// Recipients to send a message to again.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ResendRequest {
    /// Addresses from the original envelope; the refused ones when missing.
    #[serde(default)]
    pub recipients: Option<Vec<String>>,
}

/// An undone message and the draft it went back to.
#[derive(Debug, Serialize, ToSchema)]
pub struct UndoDto {
    pub outbox: OutboxDto,
    pub draft: DraftDto,
}

/// Which outbox messages to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OutboxParams {
    /// Comma-separated states; every state when missing.
    pub state: Option<String>,
    /// Only messages older than this id, for the next page.
    pub before: Option<String>,
    /// At most this many, 100 by default and 500 at most.
    pub limit: Option<u32>,
}

/// The check every send makes ([`holtmail_access::Access::may_send`]).
pub(crate) async fn may_send(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    archive: ArchiveId,
    transport: &Transport,
) -> ApiResult<()> {
    access(state)
        .may_send(user, scopes, archive, transport)
        .await
        .map_err(|error| match error {
            AccessError::Denied(Denial::Capability) => {
                forbidden("the transport's account does not allow sending")
            }
            other => denied(other),
        })
}

/// The locks of [`may_send`] that do not depend on the transport, checked before anything can
/// reach an alias service.
pub(crate) async fn may_send_somehow(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    archive: ArchiveId,
) -> ApiResult<()> {
    access(state)
        .may_send_somehow(user, scopes, archive)
        .await
        .map_err(denied)
}

fn mailboxes(list: &[MailboxDto]) -> Vec<Mailbox> {
    list.iter()
        .filter(|m| !m.address.trim().is_empty())
        .map(Into::into)
        .collect()
}

async fn entry_for(state: &ApiState, user: &User, raw: &str) -> ApiResult<OutboxEntry> {
    let id: OutboxId = raw
        .parse()
        .map_err(|_| ApiError::not_found("outbox message"))?;
    let entry = state
        .store
        .get_outbox(id)
        .await?
        .ok_or_else(|| ApiError::not_found("outbox message"))?;
    let mine = entry.new.user_id == Some(user.id);
    let visible =
        role_in(state, user, entry.new.archive_id).await?.is_some() && (mine || manages(user));
    if !visible {
        return Err(ApiError::not_found("outbox message"));
    }
    Ok(entry)
}

async fn transport_of(state: &ApiState, entry: &OutboxEntry) -> ApiResult<Transport> {
    state
        .store
        .get_transport(entry.new.transport_id)
        .await?
        .ok_or_else(|| ApiError::bad_request("the transport no longer exists"))
}

async fn record(
    state: &ApiState,
    user: &User,
    entry: &OutboxEntry,
    account: Option<holtmail_core::AccountId>,
    action: &str,
) -> ApiResult<()> {
    audit(
        state,
        user,
        AuditKind::OutboxChanged,
        Some(entry.new.archive_id),
        account,
        vec![entry.id.to_string(), action.to_owned()],
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/drafts/{id}/send",
    params(("id" = String, Path, description = "Draft id")),
    request_body = SendRequest,
    responses((status = 201, description = "Queued; the draft is removed. It leaves when the undo window ends or at send_at", body = OutboxDto), (status = 400, description = "No route, a draft that cannot be built, a message the transport may not carry, or an alias service that gave no reverse alias", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Sending is off, the account does not allow it, or no sender grant", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto), (status = 409, description = "The draft was saved again since that version (ErrorDto), or recipients would see the real mailbox behind an alias and allow_leaks is not set (LeaksDto)", body = LeaksDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn send_draft(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendRequest>,
) -> ApiResult<Response> {
    let sending = sending(&state)?;
    let id: holtmail_core::DraftId = raw.parse().map_err(|_| ApiError::not_found("draft"))?;
    let draft = state
        .store
        .get_draft(id)
        .await?
        .filter(|d| d.user_id == user.id)
        .ok_or_else(|| ApiError::not_found("draft"))?;
    let archive = draft.archive_id;
    role(&state, &user, archive)
        .await
        .map_err(|_| ApiError::not_found("draft"))?;
    may_send_somehow(&state, &user, &scopes, archive).await?;
    if request.version.is_some_and(|v| v != draft.current.version) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the draft was saved again since that version".into(),
        ));
    }
    let mut payload = DraftPayload::from_stored(&draft.current.payload);
    let stored = DraftPayload {
        context: DraftContext {
            reverse_contacts: Vec::new(),
            reverse_alias: None,
            ..payload.context.clone()
        },
        ..payload.clone()
    };
    let mut routing = routing_for(&state, &user, archive, &stored).await?;
    let recipients: Vec<String> = payload
        .to
        .iter()
        .chain(&payload.cc)
        .chain(&payload.bcc)
        .map(|m| m.address.trim().to_ascii_lowercase())
        .collect();
    let contacts: Vec<(String, String)> = payload
        .context
        .reverse_contacts
        .iter()
        .filter(|c| recipients.contains(&c.address.trim().to_ascii_lowercase()))
        .map(|c| (c.contact.clone(), c.address.clone()))
        .collect();
    let trusted = crate::relay::confirmed(&state, &routing, &contacts).await;
    crate::relay::trust(&mut routing, trusted.iter().map(String::as_str));
    let routed =
        crate::relay::route_creating(&state, &user, &mut routing, payload.identity.as_deref())
            .await?
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
    crate::relay::apply_to(&mut payload.to, &routed, &routing, false);
    let leaks = crate::relay::leaks(&routing, &routed, &payload.to, &payload.cc, &payload.bcc);
    if !leaks.is_empty() && !request.allow_leaks {
        return Ok((
            StatusCode::CONFLICT,
            Json(LeaksDto {
                error: "recipients would see the real mailbox behind the alias".into(),
                leaks: leak_dtos(&leaks),
            }),
        )
            .into_response());
    }
    let route = routed.route;
    let transport = state
        .store
        .archive_transports(archive)
        .await?
        .into_iter()
        .find(|t| t.id.to_string() == route.transport)
        .ok_or_else(|| ApiError::bad_request("the archive may not use the transport"))?;
    may_send(&state, &user, &scopes, archive, &transport).await?;
    let identity = state
        .store
        .get_identity(
            route
                .identity
                .parse()
                .map_err(|_| ApiError::bad_request("the route names no identity"))?,
        )
        .await?
        .ok_or_else(|| ApiError::not_found("identity"))?;
    let mut attachments = Vec::with_capacity(payload.attachments.len());
    for (index, a) in payload.attachments.iter().enumerate() {
        let blob: BlobId = a
            .blob
            .as_deref()
            .and_then(|b| b.parse().ok())
            .filter(|b| draft.current.blobs.contains(b))
            .ok_or_else(|| ApiError::bad_request(format!("attachment {index} is missing")))?;
        attachments.push(Attachment {
            name: a.name.clone().unwrap_or_else(|| "attachment".into()),
            content_type: a
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            bytes: blob_bytes(&state, &blob).await?.to_vec(),
            content_id: a.content_id.clone(),
        });
    }
    let compose = holtmail_compose::Draft {
        from: Mailbox {
            name: route.name.clone(),
            address: route.from.clone(),
        },
        to: mailboxes(&payload.to),
        cc: mailboxes(&payload.cc),
        bcc: mailboxes(&payload.bcc),
        reply_to: identity
            .fields
            .reply_to
            .iter()
            .map(|r| Mailbox::new(r.clone()))
            .collect(),
        subject: payload.subject.clone(),
        text: payload.text.clone(),
        html: payload.html.clone().filter(|h| !h.trim().is_empty()),
        attachments,
        in_reply_to: payload.in_reply_to.clone(),
        references: payload.references.clone(),
        headers: Vec::new(),
        message_id_domain: None,
        date: None,
        calendar: None,
    };
    let built = holtmail_compose::build(&compose)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let send_at = request
        .send_at
        .or(payload.send_at)
        .map(Timestamp::from_millis);
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|k| format!("send:{k}"));
    let submitted = sending
        .submission
        .queue(Submit {
            actor: Actor::web(user.id),
            archive,
            identity: identity.id,
            mail_from: compose.from.address.clone(),
            recipients: built.recipients,
            message: Bytes::from(built.bytes),
            send_at,
            undo_window: None,
            idempotency_key: key,
            allow_leaks: !leaks.is_empty(),
            route: Some(route),
        })
        .await
        .map_err(submission_error)?;
    let entry = state
        .store
        .get_outbox(submitted.id)
        .await?
        .ok_or_else(|| ApiError::not_found("outbox message"))?;
    state.store.delete_draft(draft.id).await?;
    Ok((
        StatusCode::CREATED,
        Json(outbox_view(&state, &entry).await?),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/outbox",
    params(("id" = String, Path, description = "Archive id"), OutboxParams),
    responses((status = 200, description = "Newest first: every message for owners and admins, the user's own for others", body = Vec<OutboxDto>), (status = 400, description = "An unknown state or a malformed id", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto)),
)]
pub(crate) async fn list_outbox(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Query(params): Query<OutboxParams>,
) -> ApiResult<Json<Vec<OutboxDto>>> {
    let archive = parse_archive(&raw)?;
    role(&state, &user, archive).await?;
    let states = params
        .state
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| OutboxState::parse(s).ok_or_else(|| ApiError::bad_request("unknown state")))
        .collect::<ApiResult<Vec<_>>>()?;
    let before = params
        .before
        .as_deref()
        .map(|raw| {
            raw.parse::<OutboxId>()
                .map_err(|_| ApiError::bad_request("malformed outbox id"))
        })
        .transpose()?;
    let entries = state
        .store
        .list_outbox(&OutboxQuery {
            archive,
            states,
            before,
            user: (!manages(&user)).then_some(user.id),
            limit: params.limit.unwrap_or(100).min(500),
        })
        .await?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in &entries {
        out.push(outbox_view(&state, entry).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/outbox/{id}",
    params(("id" = String, Path, description = "Outbox message id")),
    responses((status = 200, body = OutboxDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message the user may see", body = ErrorDto)),
)]
pub(crate) async fn get_outbox(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<OutboxDto>> {
    let entry = entry_for(&state, &user, &raw).await?;
    Ok(Json(outbox_view(&state, &entry).await?))
}

#[utoipa::path(
    post,
    path = "/api/outbox/{id}/undo",
    params(("id" = String, Path, description = "Outbox message id")),
    responses((status = 200, description = "Cancelled before it left, and back as a draft", body = UndoDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not the user who queued it", body = ErrorDto), (status = 404, description = "No such message the user may see", body = ErrorDto), (status = 409, description = "It is no longer waiting", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn undo(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<UndoDto>> {
    let sending = sending(&state)?;
    let entry = entry_for(&state, &user, &raw).await?;
    if entry.new.user_id != Some(user.id) {
        return Err(forbidden("only the sender undoes a message"));
    }
    record(&state, &user, &entry, None, "undone").await?;
    let held = sending.outbox.undo(entry.id).await.map_err(outbox_error)?;
    let draft = match redraft(&state, &user, &held).await {
        Ok(draft) => draft,
        Err(error) => {
            if let Err(release) = sending
                .outbox
                .release(held.id, Some(held.new.send_at))
                .await
            {
                tracing::warn!(outbox = %held.id, error = %release, "undone message left held");
            }
            return Err(error);
        }
    };
    let entry = sending.outbox.cancel(held.id).await.map_err(outbox_error)?;
    Ok(Json(UndoDto {
        outbox: outbox_view(&state, &entry).await?,
        draft: draft_dto(&state, &user, &draft).await?,
    }))
}

async fn redraft(
    state: &ApiState,
    user: &User,
    entry: &OutboxEntry,
) -> ApiResult<holtmail_store::Draft> {
    let payload = payload_of(state, entry).await?;
    let blobs = payload
        .attachments
        .iter()
        .filter_map(|a| a.blob.as_deref()?.parse().ok())
        .collect::<Vec<BlobId>>();
    Ok(state
        .store
        .create_draft(
            entry.new.archive_id,
            user.id,
            &serde_json::to_value(&payload)?,
            &blobs,
            Timestamp::now(),
        )
        .await?)
}

async fn payload_of(state: &ApiState, entry: &OutboxEntry) -> ApiResult<DraftPayload> {
    let raw = blob_bytes(state, &entry.new.blob_id).await?;
    let parsed = holtmail_mime::parse(&raw)
        .map_err(|_| ApiError::bad_request("the queued message cannot be read"))?;
    let original = holtmail_compose::Original::from_parsed(&parsed);
    let identity = state.store.get_identity(entry.new.identity_id).await?;
    let visible: Vec<String> = original
        .to
        .iter()
        .chain(&original.cc)
        .map(|m| m.address.to_lowercase())
        .collect();
    let self_copy = identity
        .as_ref()
        .filter(|i| i.fields.bcc_self)
        .map(|i| i.fields.address.to_lowercase());
    let bcc = entry
        .new
        .recipients
        .iter()
        .filter(|r| !visible.contains(&r.to_lowercase()))
        .filter(|r| self_copy.as_deref() != Some(&r.to_lowercase()))
        .map(|r| MailboxDto {
            name: None,
            address: r.clone(),
        })
        .collect();
    let mut attachments = Vec::with_capacity(parsed.attachments.len());
    for part in &parsed.attachments {
        let size = part.content.len() as u64;
        let blob = state.blobs.put(part.content.clone()).await?;
        attachments.push(DraftAttachment {
            blob: Some(blob.to_string()),
            name: part.filename.clone(),
            content_type: Some(part.content_type.clone()),
            size: Some(size),
            content_id: part.content_id.clone().filter(|_| part.inline),
            ..DraftAttachment::default()
        });
    }
    Ok(DraftPayload {
        identity: Some(entry.new.identity_id.to_string()),
        to: original.to.iter().map(Into::into).collect(),
        cc: original.cc.iter().map(Into::into).collect(),
        bcc,
        subject: original.subject,
        text: original.text.unwrap_or_default(),
        html: original.html,
        attachments,
        in_reply_to: original.in_reply_to.first().cloned(),
        references: original.references,
        context: DraftContext::default(),
        send_at: None,
    })
}

#[utoipa::path(
    post,
    path = "/api/outbox/{id}/cancel",
    params(("id" = String, Path, description = "Outbox message id")),
    responses((status = 200, body = OutboxDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Never for the sender or a manager", body = ErrorDto), (status = 404, description = "No such message the user may see", body = ErrorDto), (status = 409, description = "It already left or ended", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn cancel(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<OutboxDto>> {
    let sending = sending(&state)?;
    let entry = entry_for(&state, &user, &raw).await?;
    record(&state, &user, &entry, None, "cancelled").await?;
    let entry = sending
        .outbox
        .cancel(entry.id)
        .await
        .map_err(outbox_error)?;
    Ok(Json(outbox_view(&state, &entry).await?))
}

#[utoipa::path(
    post,
    path = "/api/outbox/{id}/retry",
    params(("id" = String, Path, description = "Outbox message id")),
    responses((status = 200, description = "Queued again now, for every recipient that has not accepted", body = OutboxDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Sending is not allowed", body = ErrorDto), (status = 404, description = "No such message the user may see", body = ErrorDto), (status = 409, description = "Not retrying, failed or uncertain", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn retry(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<OutboxDto>> {
    let sending = sending(&state)?;
    let entry = entry_for(&state, &user, &raw).await?;
    let transport = transport_of(&state, &entry).await?;
    may_send(&state, &user, &scopes, entry.new.archive_id, &transport).await?;
    record(&state, &user, &entry, transport.account_id, "retried").await?;
    let entry = sending
        .outbox
        .retry(entry.id, Some(user.id))
        .await
        .map_err(outbox_error)?;
    Ok(Json(outbox_view(&state, &entry).await?))
}

#[utoipa::path(
    post,
    path = "/api/outbox/{id}/resend",
    params(("id" = String, Path, description = "Outbox message id")),
    request_body = ResendRequest,
    responses((status = 201, description = "A new outbox message with the same bytes", body = OutboxDto), (status = 400, description = "A recipient not in the original envelope, or the identity may no longer be used", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Sending is not allowed", body = ErrorDto), (status = 404, description = "No such message the user may see", body = ErrorDto), (status = 409, description = "It has not ended yet", body = ErrorDto), (status = 503, description = "The server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn resend(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ResendRequest>,
) -> ApiResult<(StatusCode, Json<OutboxDto>)> {
    let sending = sending(&state)?;
    let entry = entry_for(&state, &user, &raw).await?;
    let transport = transport_of(&state, &entry).await?;
    may_send(&state, &user, &scopes, entry.new.archive_id, &transport).await?;
    record(&state, &user, &entry, transport.account_id, "resent").await?;
    let again = sending
        .outbox
        .resend(entry.id, Some(user.id), request.recipients)
        .await
        .map_err(outbox_error)?;
    Ok((
        StatusCode::CREATED,
        Json(outbox_view(&state, &again).await?),
    ))
}
