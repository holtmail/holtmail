use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_blob::BlobId;
use holtmail_compose::{ForwardKind, Mailbox, ReplyKind};
use holtmail_core::{ArchiveId, DraftId, MessageId, Timestamp};
use holtmail_route::{Leak, RouteError, Routed};
use holtmail_store::{Draft, DraftSave, DraftVersion, GrantRole, Message, User};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::dto::ErrorDto;
use crate::relay::{self, Routing};
use crate::send::{blob, forbidden, parse_archive, role};
use crate::write::{Scope, Scopes};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// The largest attachment one upload takes.
pub const MAX_ATTACHMENT: usize = 50 * 1024 * 1024;

/// One mailbox of a draft.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MailboxDto {
    #[serde(default)]
    pub name: Option<String>,
    pub address: String,
}

impl From<&Mailbox> for MailboxDto {
    fn from(m: &Mailbox) -> Self {
        Self {
            name: m.name.clone(),
            address: m.address.clone(),
        }
    }
}

impl From<&MailboxDto> for Mailbox {
    fn from(m: &MailboxDto) -> Self {
        Self {
            name: m.name.clone().filter(|n| !n.trim().is_empty()),
            address: m.address.trim().to_owned(),
        }
    }
}

/// A file of a draft. Stored drafts always carry `blob`, `name`, `content_type` and `size`.
/// When saving, an attachment is either one the draft already has (`blob`) or an attachment of
/// an archived message (`message` and `ordinal`), which the server resolves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DraftAttachment {
    #[serde(default)]
    pub blob: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub ordinal: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// For a part the HTML shows through `cid:`.
    #[serde(default)]
    pub content_id: Option<String>,
}

/// What the draft answers or forwards; used to pick the identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DraftContext {
    /// `new`, `reply` or `forward`.
    #[serde(default = "new_kind")]
    pub kind: String,
    /// The archived message it answers or forwards.
    #[serde(default)]
    pub message: Option<String>,
    /// A reverse alias the server made for the original's sender when the draft was started,
    /// so the route can show it; sending makes or asks for it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_alias: Option<String>,
    /// Reverse aliases the server made for other recipients (the `use_reverse_alias` fix).
    /// Sending asks the alias service again and trusts only those it confirms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reverse_contacts: Vec<ReverseContact>,
}

/// A recipient and the reverse alias that reaches it through the thread's alias.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReverseContact {
    pub contact: String,
    pub address: String,
}

fn new_kind() -> String {
    "new".into()
}

/// What the compose window holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DraftPayload {
    /// The identity picked by hand; `null` lets the route decide.
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub to: Vec<MailboxDto>,
    #[serde(default)]
    pub cc: Vec<MailboxDto>,
    #[serde(default)]
    pub bcc: Vec<MailboxDto>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    /// `Message-ID` it answers, without angle brackets.
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub context: DraftContext,
    /// When to send, Unix milliseconds; `null` for as soon as the undo window ends.
    #[serde(default)]
    pub send_at: Option<i64>,
}

impl DraftPayload {
    pub(crate) fn from_stored(value: &serde_json::Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}

fn mailbox_dtos(addresses: &[holtmail_core::Address]) -> Vec<MailboxDto> {
    addresses
        .iter()
        .map(|a| MailboxDto {
            name: a.name.clone(),
            address: a.email.clone(),
        })
        .collect()
}

/// What the compose window shows of a stored version. A raw version (a draft a mail client
/// saved over IMAP or JMAP) opens like "Edit as new": recipients, subject, the text body and
/// the attachments from its bytes, with the thread it answers kept, so the next save stores a
/// compose version on top.
pub(crate) async fn stored_payload(
    state: &ApiState,
    version: &DraftVersion,
) -> ApiResult<DraftPayload> {
    let Some(raw) = version.raw.as_ref().filter(|_| version.payload.is_null()) else {
        return Ok(DraftPayload::from_stored(&version.payload));
    };
    let Some(bytes) = state.blobs.get(raw).await? else {
        return Ok(DraftPayload::default());
    };
    let Ok(parsed) = holtmail_mime::parse(&bytes) else {
        return Ok(DraftPayload::default());
    };
    let mut attachments = Vec::new();
    for attachment in parsed.attachments.iter().filter(|a| !a.inline) {
        let size = attachment.content.len() as u64;
        let blob = state.blobs.put(attachment.content.clone()).await?;
        attachments.push(DraftAttachment {
            blob: Some(blob.to_string()),
            message: None,
            ordinal: None,
            name: attachment.filename.clone(),
            content_type: Some(attachment.content_type.clone()),
            size: Some(size),
            content_id: None,
        });
    }
    Ok(DraftPayload {
        identity: None,
        to: mailbox_dtos(&parsed.to),
        cc: mailbox_dtos(&parsed.cc),
        bcc: mailbox_dtos(&parsed.bcc),
        subject: parsed.subject.unwrap_or_default(),
        text: parsed.text.unwrap_or_default(),
        html: None,
        attachments,
        in_reply_to: parsed.in_reply_to.first().cloned(),
        references: parsed.references,
        context: DraftContext {
            kind: new_kind(),
            ..DraftContext::default()
        },
        send_at: None,
    })
}

/// Why a route looks the way it does: a stable `code` with the address, alias or account it
/// names.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteReasonDto {
    /// `chosen`, `original_recipient`, `plus_address`, `alias`, `account`, `default`,
    /// `plus_address_dropped`, and for replies through an alias service `alias_mailbox`,
    /// `no_alias_mailbox`, `not_alias_mailbox`, `alias_from`, `reverse_alias`,
    /// `computed_reverse_alias`, `unsigned_reverse_alias` or `reply_header`.
    pub code: String,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
}

/// How a draft is sent.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteDto {
    pub identity: String,
    /// The exact `From` address.
    pub from: String,
    pub name: Option<String>,
    pub transport: String,
    /// `direct`, `reverse_alias` or `reply_header`.
    pub strategy: String,
    /// The rule that picked the identity first, then notes.
    pub reasons: Vec<RouteReasonDto>,
    /// For a reply through an alias service: the address that takes the original sender's
    /// place in `To` (the reverse alias or the service's `Reply-To`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The alias the original came through, else the first alias another message of its thread
    /// came through: the address to send as for the `use_alias_identity` fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Where the strategy comes from: `alias`, `domain` or `service` for an override, `default`
    /// for the detector's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_source: Option<String>,
    /// The provider id of the alias service the original came through, such as `simplelogin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// For a reply through an alias service that replaces the original sender: the real sender,
    /// lowercase, whom [`RouteDto::to`] reaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
}

/// A recipient that would see the real mailbox behind an alias.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LeakDto {
    pub recipient: String,
    /// `to`, `cc` or `bcc`.
    pub field: String,
    /// `mixed_recipients`, `cc_not_reverse_alias`, `bcc_not_reverse_alias`,
    /// `forward_from_real_address` or `real_address_from`.
    pub reason: String,
    /// `use_reverse_alias`, `use_alias_identity` or `remove_recipient`.
    pub fix: String,
}

/// Why a draft has no route.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteErrorDto {
    /// `no_identity`, `unknown_identity`, `unknown_transport`, `not_allowed_from`,
    /// `needs_reverse_alias` (made when the draft is sent), `no_reply_header` or
    /// `no_alias_identity`.
    pub code: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub contact: Option<String>,
}

pub(crate) fn convert<T: Serialize, U: serde::de::DeserializeOwned>(value: &T) -> Option<U> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

pub(crate) fn route_dto(routed: &Routed, routing: &Routing) -> Option<RouteDto> {
    let mut dto: RouteDto = convert(&routed.route)?;
    dto.to = routed.to.clone();
    dto.alias = routed
        .alias
        .clone()
        .or_else(|| relay::thread_alias(routing));
    dto.strategy_source = routing
        .relay
        .as_ref()
        .and(routing.source)
        .map(str::to_owned);
    if let Some(relay) = routing.relay.as_ref().filter(|_| dto.alias.is_some()) {
        dto.provider = Some(relay.hit.provider.clone());
        if dto.to.is_some() {
            dto.contact = relay
                .hit
                .sender
                .as_ref()
                .map(|a| a.trim().to_ascii_lowercase())
                .or_else(|| routing.from.clone());
        }
    }
    Some(dto)
}

pub(crate) fn leak_dtos(leaks: &[Leak]) -> Vec<LeakDto> {
    leaks.iter().filter_map(convert).collect()
}

/// A draft at its newest version, with the route it would take now.
#[derive(Debug, Serialize, ToSchema)]
pub struct DraftDto {
    pub id: String,
    pub archive: String,
    /// Grows by one with every save; send it back to save again.
    pub version: u32,
    pub payload: DraftPayload,
    /// Unix milliseconds.
    pub saved_at: i64,
    /// Unix milliseconds.
    pub created_at: i64,
    pub route: Option<RouteDto>,
    pub route_error: Option<RouteErrorDto>,
    /// Recipients that would see the real mailbox behind an alias; sending refuses while any
    /// is left unless told to send anyway.
    pub leaks: Vec<LeakDto>,
}

/// One kept version of a draft.
#[derive(Debug, Serialize, ToSchema)]
pub struct DraftVersionDto {
    pub version: u32,
    pub payload: DraftPayload,
    /// Unix milliseconds.
    pub saved_at: i64,
}

impl DraftVersionDto {
    async fn of(state: &ApiState, v: &DraftVersion) -> ApiResult<Self> {
        Ok(Self {
            version: v.version,
            payload: stored_payload(state, v).await?,
            saved_at: v.saved_at.as_millis(),
        })
    }
}

/// A draft to start.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewDraftRequest {
    #[serde(default)]
    pub payload: DraftPayload,
}

/// A new version of a draft.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DraftSaveRequest {
    /// The version this one replaces; another save in between answers `409`.
    pub version: u32,
    pub payload: DraftPayload,
}

/// A reply to an archived message.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ReplyRequest {
    /// `sender` (the default), `all` or `list`.
    #[serde(default)]
    pub kind: Option<String>,
    /// An identity picked by hand.
    #[serde(default)]
    pub identity: Option<String>,
}

/// A forward of an archived message.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ForwardRequest {
    /// `inline` (the default) or `attachment`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub identity: Option<String>,
}

/// The file name and type of an uploaded attachment.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UploadParams {
    pub name: String,
    /// `application/octet-stream` when missing.
    pub content_type: Option<String>,
}

/// Whether the user may write drafts and send in the archive: a `sender` grant and the
/// `send:mail` scope. Switches and capabilities are checked when a message is sent.
pub(crate) async fn may_compose(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    archive: ArchiveId,
) -> ApiResult<()> {
    if role(state, user, archive).await? < GrantRole::Sender {
        return Err(forbidden(
            "your grant on this archive does not allow sending",
        ));
    }
    if !scopes.has(Scope::SendMail) {
        return Err(forbidden("these credentials do not allow sending"));
    }
    Ok(())
}

pub(crate) async fn readable_message(
    state: &ApiState,
    user: &User,
    raw: &str,
) -> ApiResult<Message> {
    let id: MessageId = raw.parse().map_err(|_| ApiError::not_found("message"))?;
    let message = state
        .store
        .get_message(id)
        .await?
        .ok_or_else(|| ApiError::not_found("message"))?;
    role(state, user, message.archive_id)
        .await
        .map_err(|_| ApiError::not_found("message"))?;
    Ok(message)
}

/// The routing context of a draft: what it answers or forwards, when that message is readable
/// and in the same archive.
pub(crate) async fn routing_for(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    payload: &DraftPayload,
) -> ApiResult<Routing> {
    let Some(raw) = payload.context.message.as_deref() else {
        return Ok(relay::new_message(archive));
    };
    match readable_message(state, user, raw).await {
        Ok(message) if message.archive_id == archive => {
            let raw = state.blobs.get(&message.blob_id).await.ok().flatten();
            let parsed = raw.as_deref().and_then(|r| holtmail_mime::parse(r).ok());
            let mut routing = relay::routing_of(
                state,
                &message,
                parsed.as_ref(),
                payload.context.kind == "forward",
                payload.context.reverse_alias.clone(),
            )
            .await?;
            relay::trust(
                &mut routing,
                payload
                    .context
                    .reverse_contacts
                    .iter()
                    .map(|c| c.address.as_str()),
            );
            Ok(routing)
        }
        _ => Ok(relay::new_message(archive)),
    }
}

pub(crate) async fn draft_dto(state: &ApiState, user: &User, draft: &Draft) -> ApiResult<DraftDto> {
    let payload = stored_payload(state, &draft.current).await?;
    let routing = routing_for(state, user, draft.archive_id, &payload).await?;
    let routed = relay::route(state, user, &routing, payload.identity.as_deref()).await?;
    let (route, route_error, leaks) = match &routed {
        Ok(routed) => {
            let mut to = payload.to.clone();
            relay::apply_to(&mut to, routed, &routing, false);
            (
                route_dto(routed, &routing),
                None,
                leak_dtos(&relay::leaks(
                    &routing,
                    routed,
                    &to,
                    &payload.cc,
                    &payload.bcc,
                )),
            )
        }
        Err(error) => (None, convert(error), Vec::new()),
    };
    Ok(DraftDto {
        id: draft.id.to_string(),
        archive: draft.archive_id.to_string(),
        version: draft.current.version,
        payload,
        saved_at: draft.current.saved_at.as_millis(),
        created_at: draft.created_at.as_millis(),
        route,
        route_error,
        leaks,
    })
}

/// Checks the payload's attachments and resolves those of archived messages. Returns the
/// payload as it is stored and the blobs it uses.
async fn resolve(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    mut payload: DraftPayload,
    current: &[BlobId],
) -> ApiResult<(DraftPayload, Vec<BlobId>)> {
    let mut blobs = Vec::new();
    let mut attachments = Vec::with_capacity(payload.attachments.len());
    for attachment in payload.attachments {
        let resolved = match (&attachment.blob, &attachment.message) {
            (Some(raw), _) => {
                let blob: BlobId = raw
                    .parse()
                    .map_err(|_| ApiError::bad_request("malformed attachment blob"))?;
                if !current.contains(&blob) {
                    return Err(ApiError::bad_request(
                        "an attachment is neither in the draft nor in an archived message",
                    ));
                }
                attachment
            }
            (None, Some(raw)) => {
                let message = readable_message(state, user, raw).await?;
                if message.archive_id != archive {
                    return Err(ApiError::not_found("message"));
                }
                let ordinal = attachment
                    .ordinal
                    .ok_or_else(|| ApiError::bad_request("an attachment needs its ordinal"))?;
                let stored = state
                    .store
                    .attachments(message.id)
                    .await?
                    .into_iter()
                    .find(|a| a.ordinal == ordinal)
                    .ok_or_else(|| ApiError::not_found("attachment"))?;
                DraftAttachment {
                    blob: Some(stored.blob_id.to_string()),
                    message: None,
                    ordinal: None,
                    name: attachment
                        .name
                        .or(stored.filename)
                        .or_else(|| Some("attachment".into())),
                    content_type: Some(stored.content_type),
                    size: Some(stored.size),
                    content_id: attachment.content_id,
                }
            }
            (None, None) => {
                return Err(ApiError::bad_request(
                    "an attachment needs a blob or a message",
                ));
            }
        };
        if let Some(blob) = resolved.blob.as_deref().and_then(|b| b.parse().ok()) {
            blobs.push(blob);
        }
        attachments.push(resolved);
    }
    payload.attachments = attachments;
    Ok((payload, blobs))
}

pub(crate) fn blobs_of(payload: &DraftPayload) -> Vec<BlobId> {
    payload
        .attachments
        .iter()
        .filter_map(|a| a.blob.as_deref()?.parse().ok())
        .collect()
}

async fn own_draft(state: &ApiState, user: &User, raw: &str) -> ApiResult<Draft> {
    let id: DraftId = raw.parse().map_err(|_| ApiError::not_found("draft"))?;
    let draft = state
        .store
        .get_draft(id)
        .await?
        .filter(|d| d.user_id == user.id)
        .ok_or_else(|| ApiError::not_found("draft"))?;
    role(state, user, draft.archive_id)
        .await
        .map_err(|_| ApiError::not_found("draft"))?;
    Ok(draft)
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/drafts",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "The user's drafts, most recently saved first", body = Vec<DraftDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_drafts(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<DraftDto>>> {
    let archive = parse_archive(&raw)?;
    role(&state, &user, archive).await?;
    let mut out = Vec::new();
    for draft in state.store.list_drafts(archive, user.id).await? {
        out.push(draft_dto(&state, &user, &draft).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/drafts",
    params(("id" = String, Path, description = "Archive id")),
    request_body = NewDraftRequest,
    responses((status = 201, body = DraftDto), (status = 400, description = "An attachment that is neither in an archived message nor uploaded", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "No sender grant", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto)),
)]
pub(crate) async fn create_draft(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<NewDraftRequest>,
) -> ApiResult<(StatusCode, Json<DraftDto>)> {
    let archive = parse_archive(&raw)?;
    may_compose(&state, &user, &scopes, archive).await?;
    let mut payload = request.payload;
    payload.context.reverse_alias = None;
    payload.context.reverse_contacts = Vec::new();
    let (payload, blobs) = resolve(&state, &user, archive, payload, &[]).await?;
    let draft = state
        .store
        .create_draft(
            archive,
            user.id,
            &serde_json::to_value(&payload)?,
            &blobs,
            Timestamp::now(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(draft_dto(&state, &user, &draft).await?),
    ))
}

#[utoipa::path(
    get,
    path = "/api/drafts/{id}",
    params(("id" = String, Path, description = "Draft id")),
    responses((status = 200, body = DraftDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto)),
)]
pub(crate) async fn get_draft(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<DraftDto>> {
    let draft = own_draft(&state, &user, &raw).await?;
    Ok(Json(draft_dto(&state, &user, &draft).await?))
}

#[utoipa::path(
    put,
    path = "/api/drafts/{id}",
    params(("id" = String, Path, description = "Draft id")),
    request_body = DraftSaveRequest,
    responses((status = 200, body = DraftDto), (status = 400, description = "An unknown attachment", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "No sender grant", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto), (status = 409, description = "Another save came first; the body is the draft as it is now", body = DraftDto)),
)]
pub(crate) async fn save_draft(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<DraftSaveRequest>,
) -> ApiResult<(StatusCode, Json<DraftDto>)> {
    let draft = own_draft(&state, &user, &raw).await?;
    may_compose(&state, &user, &scopes, draft.archive_id).await?;
    let stored = stored_payload(&state, &draft.current).await?;
    let mut payload = request.payload;
    payload.context.reverse_alias = stored.context.reverse_alias;
    payload.context.reverse_contacts = stored.context.reverse_contacts;
    let mut current = draft.current.blobs.clone();
    current.extend(
        stored
            .attachments
            .iter()
            .filter_map(|a| a.blob.as_deref()?.parse::<BlobId>().ok()),
    );
    let (payload, blobs) = resolve(&state, &user, draft.archive_id, payload, &current).await?;
    match state
        .store
        .save_draft(
            draft.id,
            request.version,
            &serde_json::to_value(&payload)?,
            &blobs,
            Timestamp::now(),
        )
        .await?
    {
        DraftSave::Saved(draft) => Ok((
            StatusCode::OK,
            Json(draft_dto(&state, &user, &draft).await?),
        )),
        DraftSave::Conflict(draft) => Ok((
            StatusCode::CONFLICT,
            Json(draft_dto(&state, &user, &draft).await?),
        )),
    }
}

#[utoipa::path(
    get,
    path = "/api/drafts/{id}/versions",
    params(("id" = String, Path, description = "Draft id")),
    responses((status = 200, description = "Kept versions, newest first", body = Vec<DraftVersionDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto)),
)]
pub(crate) async fn draft_versions(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<DraftVersionDto>>> {
    let draft = own_draft(&state, &user, &raw).await?;
    let mut out = Vec::new();
    for version in state.store.draft_versions(draft.id).await? {
        out.push(DraftVersionDto::of(&state, &version).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    delete,
    path = "/api/drafts/{id}",
    params(("id" = String, Path, description = "Draft id")),
    responses((status = 204, description = "Removed with every version"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Never: a user removes only their own drafts", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto)),
)]
pub(crate) async fn delete_draft(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let draft = own_draft(&state, &user, &raw).await?;
    state.store.delete_draft(draft.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/drafts/{id}/attachments",
    params(("id" = String, Path, description = "Draft id"), UploadParams),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 200, description = "The draft with the file attached as a new version", body = DraftDto), (status = 400, description = "An empty name or file", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "No sender grant", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto), (status = 413, description = "Larger than 50 MB", body = ErrorDto)),
)]
pub(crate) async fn upload_attachment(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Query(params): Query<UploadParams>,
    body: Bytes,
) -> ApiResult<Json<DraftDto>> {
    let draft = own_draft(&state, &user, &raw).await?;
    may_compose(&state, &user, &scopes, draft.archive_id).await?;
    let name = params.name.trim();
    if name.is_empty() || body.is_empty() {
        return Err(ApiError::bad_request(
            "an attachment needs a name and content",
        ));
    }
    if body.len() > MAX_ATTACHMENT {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachments are at most 50 MB".into(),
        ));
    }
    let size = body.len() as u64;
    let blob = state.blobs.put(body).await?;
    let mut payload = stored_payload(&state, &draft.current).await?;
    payload.attachments.push(DraftAttachment {
        blob: Some(blob.to_string()),
        name: Some(name.to_owned()),
        content_type: Some(
            params
                .content_type
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| "application/octet-stream".into()),
        ),
        size: Some(size),
        ..DraftAttachment::default()
    });
    let blobs = blobs_of(&payload);
    match state
        .store
        .save_draft(
            draft.id,
            draft.current.version,
            &serde_json::to_value(&payload)?,
            &blobs,
            Timestamp::now(),
        )
        .await?
    {
        DraftSave::Saved(draft) => Ok(Json(draft_dto(&state, &user, &draft).await?)),
        DraftSave::Conflict(_) => Err(ApiError(
            StatusCode::CONFLICT,
            "the draft changed while the file was uploaded; try again".into(),
        )),
    }
}

pub(crate) async fn own_addresses(state: &ApiState, archive: ArchiveId) -> ApiResult<Vec<String>> {
    let mut own: Vec<String> = state
        .store
        .list_identities(archive)
        .await?
        .into_iter()
        .map(|i| i.fields.address)
        .collect();
    own.extend(
        state
            .store
            .list_accounts(archive)
            .await?
            .into_iter()
            .filter_map(|a| a.address),
    );
    own.extend(state.store.own_alias_addresses(archive).await?);
    Ok(own)
}

fn route_mailbox(route: &Result<Routed, RouteError>) -> Mailbox {
    match route {
        Ok(routed) => Mailbox {
            name: routed.route.name.clone(),
            address: routed.route.from.clone(),
        },
        Err(_) => Mailbox::default(),
    }
}

async fn template(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    raw: &str,
    forward: Option<ForwardKind>,
    reply: ReplyKind,
    chosen: Option<String>,
) -> ApiResult<(StatusCode, Json<DraftDto>)> {
    let message = readable_message(state, user, raw).await?;
    may_compose(state, user, scopes, message.archive_id).await?;
    let stored = template_draft(state, user, &message, forward, reply, chosen, |_| {}).await?;
    Ok((
        StatusCode::CREATED,
        Json(draft_dto(state, user, &stored).await?),
    ))
}

/// A stored draft answering or forwarding `message` as the user, routed as the compose window
/// routes it; `adjust` changes the payload before it is stored. The caller has checked that
/// the user may compose in the message's archive.
pub(crate) async fn template_draft(
    state: &ApiState,
    user: &User,
    message: &Message,
    forward: Option<ForwardKind>,
    reply: ReplyKind,
    chosen: Option<String>,
    adjust: impl FnOnce(&mut DraftPayload),
) -> ApiResult<Draft> {
    let archive = message.archive_id;
    let bytes = blob(state, &message.blob_id).await?;
    let parsed = holtmail_mime::parse(&bytes)
        .map_err(|_| ApiError::bad_request("the message cannot be read"))?;
    let original = holtmail_compose::Original::from_parsed(&parsed);
    let mut routing =
        relay::routing_of(state, message, Some(&parsed), forward.is_some(), None).await?;
    let kind = if forward.is_some() {
        "forward"
    } else {
        "reply"
    };
    let route = match relay::route_creating(state, user, &mut routing, chosen.as_deref()).await {
        Ok(route) => route,
        Err(_) => relay::route(state, user, &routing, chosen.as_deref()).await?,
    };
    let from = route_mailbox(&route);
    let draft = match forward {
        Some(kind) => holtmail_compose::forward(&original, &bytes, kind, from),
        None => {
            let own = own_addresses(state, archive).await?;
            holtmail_compose::reply(&original, reply, &own, from).map_err(|error| match error {
                holtmail_compose::ComposeError::NoListAddress => {
                    ApiError::bad_request("the message names no list to reply to")
                }
                other => ApiError::bad_request(other.to_string()),
            })?
        }
    };
    let mut attachments = Vec::with_capacity(draft.attachments.len());
    for attachment in draft.attachments {
        let size = attachment.bytes.len() as u64;
        let blob = state.blobs.put(Bytes::from(attachment.bytes)).await?;
        attachments.push(DraftAttachment {
            blob: Some(blob.to_string()),
            name: Some(attachment.name),
            content_type: Some(attachment.content_type),
            size: Some(size),
            content_id: attachment.content_id,
            ..DraftAttachment::default()
        });
    }
    let mut to: Vec<MailboxDto> = draft.to.iter().map(Into::into).collect();
    if let Ok(routed) = &route {
        relay::apply_to(&mut to, routed, &routing, true);
    }
    let mut payload = DraftPayload {
        identity: chosen,
        to,
        cc: draft.cc.iter().map(Into::into).collect(),
        bcc: Vec::new(),
        subject: draft.subject,
        text: draft.text,
        html: draft.html,
        attachments,
        in_reply_to: draft.in_reply_to,
        references: draft.references,
        context: DraftContext {
            kind: kind.into(),
            message: Some(message.id.to_string()),
            reverse_alias: routing.relay.as_ref().and_then(|r| r.reverse_alias.clone()),
            reverse_contacts: Vec::new(),
        },
        send_at: None,
    };
    adjust(&mut payload);
    let blobs = blobs_of(&payload);
    Ok(state
        .store
        .create_draft(
            archive,
            user.id,
            &serde_json::to_value(&payload)?,
            &blobs,
            Timestamp::now(),
        )
        .await?)
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/reply",
    params(("id" = String, Path, description = "Message id")),
    request_body = ReplyRequest,
    responses((status = 201, description = "A new draft answering the message, with its route", body = DraftDto), (status = 400, description = "An unknown kind, or a list reply to a message without a list", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "No sender grant", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto)),
)]
pub(crate) async fn reply(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ReplyRequest>,
) -> ApiResult<(StatusCode, Json<DraftDto>)> {
    let kind = match request.kind.as_deref().unwrap_or("sender") {
        "sender" => ReplyKind::Sender,
        "all" => ReplyKind::All,
        "list" => ReplyKind::List,
        _ => return Err(ApiError::bad_request("kind is sender, all or list")),
    };
    template(&state, &user, &scopes, &raw, None, kind, request.identity).await
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/forward",
    params(("id" = String, Path, description = "Message id")),
    request_body = ForwardRequest,
    responses((status = 201, description = "A new draft forwarding the message, with its route", body = DraftDto), (status = 400, description = "An unknown kind", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "No sender grant", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto)),
)]
pub(crate) async fn forward(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ForwardRequest>,
) -> ApiResult<(StatusCode, Json<DraftDto>)> {
    let kind = match request.kind.as_deref().unwrap_or("inline") {
        "inline" => ForwardKind::Inline,
        "attachment" => ForwardKind::Attachment,
        _ => return Err(ApiError::bad_request("kind is inline or attachment")),
    };
    template(
        &state,
        &user,
        &scopes,
        &raw,
        Some(kind),
        ReplyKind::Sender,
        request.identity,
    )
    .await
}

/// A recipient to reach through the alias the draft's thread came through.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ReverseAliasRequest {
    /// The draft version shown; another save in between answers `409`.
    pub version: u32,
    pub recipient: String,
}

#[utoipa::path(
    post,
    path = "/api/drafts/{id}/reverse-alias",
    params(("id" = String, Path, description = "Draft id")),
    request_body = ReverseAliasRequest,
    responses((status = 200, description = "The `use_reverse_alias` fix of a leak: the recipient is replaced in To, Cc and Bcc by its reverse alias, saved as a new version", body = DraftDto), (status = 400, description = "The draft answers no mail received through an alias, the recipient is not in the draft, or the service gives no reverse alias", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "No sender grant", body = ErrorDto), (status = 404, description = "No such draft of the user", body = ErrorDto), (status = 409, description = "The draft was saved again since that version", body = ErrorDto)),
)]
pub(crate) async fn reverse_alias(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ReverseAliasRequest>,
) -> ApiResult<Json<DraftDto>> {
    let draft = own_draft(&state, &user, &raw).await?;
    may_compose(&state, &user, &scopes, draft.archive_id).await?;
    if request.version != draft.current.version {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the draft was saved again since that version".into(),
        ));
    }
    let mut payload = stored_payload(&state, &draft.current).await?;
    let routing = routing_for(&state, &user, draft.archive_id, &payload).await?;
    let alias = relay::thread_alias(&routing).ok_or_else(|| {
        ApiError::bad_request("the draft answers no mail received through an alias")
    })?;
    let recipient = holtmail_alias::parse_mailbox(&request.recipient)
        .map(|a| a.email.to_ascii_lowercase())
        .ok_or_else(|| ApiError::bad_request("the recipient is not an address"))?;
    let address = state
        .aliases
        .reverse_alias(draft.archive_id, &alias, &recipient)
        .await
        .map_err(|error| ApiError::bad_request(format!("no reverse alias for {alias}: {error}")))?
        .to_ascii_lowercase();
    let mut found = false;
    for list in [&mut payload.to, &mut payload.cc, &mut payload.bcc] {
        let mut placed = false;
        list.retain_mut(|m| {
            let current = m.address.trim().to_ascii_lowercase();
            if current != recipient && current != address {
                return true;
            }
            found |= current == recipient;
            if placed {
                return false;
            }
            placed = true;
            m.address = address.clone();
            true
        });
    }
    if !found {
        return Err(ApiError::bad_request("the recipient is not in the draft"));
    }
    payload
        .context
        .reverse_contacts
        .retain(|c| c.contact != recipient);
    payload.context.reverse_contacts.push(ReverseContact {
        contact: recipient,
        address,
    });
    let blobs = blobs_of(&payload);
    match state
        .store
        .save_draft(
            draft.id,
            draft.current.version,
            &serde_json::to_value(&payload)?,
            &blobs,
            Timestamp::now(),
        )
        .await?
    {
        DraftSave::Saved(draft) => Ok(Json(draft_dto(&state, &user, &draft).await?)),
        DraftSave::Conflict(_) => Err(ApiError(
            StatusCode::CONFLICT,
            "the draft was saved again since that version".into(),
        )),
    }
}
