use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use bytes::Bytes;
use holtmail_core::{ArchiveId, CollectionId, ItemChangeId, ItemId, MessageId, Timestamp};
use holtmail_ingest::Ingest;
use holtmail_pim::ItemKind;
use holtmail_pim_edit::{ContactForm, EventForm, FormError, Invalid};
use holtmail_pim_sync::{Edits, EditsError};
use holtmail_store::{
    Account, AuditKind, Capability, ChangeClass, ChangeContent, ChangeOp, ChangeOrigin,
    ChangeTarget, Collection, CollectionKind, Effective, Expect, ItemChange, ItemChangeQuery,
    ItemChangeState, NewItemChange, RecordedChange, Resolution, ServedState, User, WriteMode,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::access::{manages, readable};
use crate::dto::ErrorDto;
use crate::write::{Scopes, audit, may_write, role_in};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// A person on an event form.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PersonFormDto {
    pub email: String,
    pub name: Option<String>,
    /// `NEEDS-ACTION`, `ACCEPTED`, `TENTATIVE` or `DECLINED`; absent keeps what the item had,
    /// or asks a new attendee to answer.
    pub partstat: Option<String>,
}

/// A reminder before an event starts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AlarmFormDto {
    /// Minutes before the start; 0 at the start.
    pub minutes_before: i64,
}

/// The fields of the event form. Saving it on an existing event rewrites only what changed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct EventFormDto {
    /// The `UID` of a new event; one is made when absent. Ignored for an existing event.
    pub uid: Option<String>,
    pub title: String,
    /// `YYYY-MM-DD` for an all-day event, else `YYYY-MM-DDTHH:MM[:SS]` in `time_zone`.
    pub start: String,
    /// Like `start`, exclusive for all-day events; absent means one day or one hour.
    pub end: Option<String>,
    pub all_day: bool,
    /// `UTC`, an IANA or Windows zone name; absent for a floating time.
    pub time_zone: Option<String>,
    /// An `RRULE` value such as `FREQ=WEEKLY;BYDAY=MO`.
    pub recurrence: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub organizer: Option<PersonFormDto>,
    pub attendees: Vec<PersonFormDto>,
    pub alarms: Vec<AlarmFormDto>,
}

/// An email address or phone number with its kinds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LabeledDto {
    pub value: String,
    /// Lowercase `TYPE` values such as `work` or `cell`.
    pub kinds: Vec<String>,
}

/// The parts of a contact's name.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NamePartsDto {
    pub family: Option<String>,
    pub given: Option<String>,
    pub additional: Option<String>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
}

/// A postal address; every part is optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PostalAddressDto {
    pub kinds: Vec<String>,
    pub po_box: Option<String>,
    pub extended: Option<String>,
    pub street: Option<String>,
    pub locality: Option<String>,
    pub region: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
}

/// The fields of the contact form. Saving it on an existing card rewrites only what changed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ContactFormDto {
    /// The `UID` of a new card; one is made when absent. Ignored for an existing card.
    pub uid: Option<String>,
    /// Made from the name, the organization or the first address when absent.
    pub full_name: Option<String>,
    pub name: NamePartsDto,
    pub emails: Vec<LabeledDto>,
    pub phones: Vec<LabeledDto>,
    pub addresses: Vec<PostalAddressDto>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
}

fn convert<A: Serialize, B: serde::de::DeserializeOwned>(from: &A) -> ApiResult<B> {
    serde_json::to_value(from)
        .and_then(serde_json::from_value)
        .map_err(|_| ApiError::bad_request("malformed form"))
}

/// What an item should become: exactly one of a form of the collection's kind or raw
/// iCalendar or vCard text.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default)]
pub struct ItemContentRequest {
    pub event: Option<EventFormDto>,
    pub contact: Option<ContactFormDto>,
    /// The whole item as iCalendar (calendars) or vCard (address books).
    pub raw: Option<String>,
    /// A new contact from the first sender of this message (name and address); fields of
    /// `contact` win. Address books only.
    pub from_message: Option<String>,
    /// For an edit: the state the client saw (`ItemEditDto::state`); the edit is refused with
    /// `409` when something else is served now. Absent: the last writer wins.
    pub expect: Option<ServedStateDto>,
    /// For an event the user organizes: `false` records the change without mailing invitations,
    /// updates or cancellations to attendees. Absent or `true`: they are sent through the outbox
    /// unless the collection's server schedules itself.
    pub invitations: Option<bool>,
}

/// What is served for an item: its latest upstream version and the open change on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServedStateDto {
    pub version: Option<u32>,
    pub change: Option<String>,
}

impl From<ServedState> for ServedStateDto {
    fn from(state: ServedState) -> Self {
        Self {
            version: state.version,
            change: state.change.map(|c| c.to_string()),
        }
    }
}

/// The "Local edit" mark of an item whose served version comes from a change: `held` (not
/// sent to the account), `sending`, `conflict` (changed in the account too, or removed there)
/// or `rejected` (the account refused it).
#[derive(Debug, Serialize, ToSchema)]
pub struct LocalEditDto {
    pub change_id: String,
    pub state: String,
    /// `create`, `update` or `delete`.
    pub op: String,
    /// `noop`, `personal`, `reply`, `edit`, `create` or `delete`.
    pub class: String,
    /// A short code for a conflict or refusal, such as `overlap`, `upstream_deleted`,
    /// `reply_obsolete` or `refused`.
    pub reason: Option<String>,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    /// The collection's write mode: `held` changes are never sent, `write_through` ones are.
    pub write_mode: String,
    /// Unix milliseconds.
    pub created_at: i64,
}

/// A change to a contact or event made in Holtmail.
#[derive(Debug, Serialize, ToSchema)]
pub struct PimChangeDto {
    pub id: String,
    pub archive_id: String,
    pub collection_id: String,
    /// Absent for a create that has not reached the account yet.
    pub item_id: Option<String>,
    /// The href of a create before the item exists.
    pub href: Option<String>,
    /// `create`, `update` or `delete`.
    pub op: String,
    /// `noop`, `personal`, `reply`, `edit`, `create` or `delete`; `noop` and `personal` are
    /// never sent.
    pub class: String,
    /// `dav` or `api`.
    pub origin: String,
    /// The device a calendar or contacts app saved through; absent for the web app.
    pub device_id: Option<String>,
    /// `held`, `sending`, `applied`, `superseded`, `conflict`, `rejected` or `discarded`.
    pub state: String,
    pub reason: Option<String>,
    /// A newer upstream version landed since it was last looked at.
    pub upstream_changed: bool,
    /// The item disappeared upstream.
    pub upstream_deleted: bool,
    pub attempts: u32,
    /// A short code for the last temporary failure, such as `connection` or `not_allowed`.
    pub last_error: Option<String>,
    /// Unix milliseconds when a held change may be sent again.
    pub next_attempt_at: Option<i64>,
    pub user_id: Option<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds.
    pub updated_at: i64,
    /// The item's contact name or event summary as last derived; only in the open changes
    /// list, and absent for a create that has not reached the account.
    pub title: Option<String>,
}

impl From<&ItemChange> for PimChangeDto {
    fn from(c: &ItemChange) -> Self {
        Self {
            id: c.id.to_string(),
            archive_id: c.archive_id.to_string(),
            collection_id: c.collection_id.to_string(),
            item_id: c.item_id.map(|i| i.to_string()),
            href: c.href.clone(),
            op: c.op.as_str().into(),
            class: c.class.as_str().into(),
            origin: c.origin.as_str().into(),
            device_id: c.device.clone(),
            state: c.state.as_str().into(),
            reason: c.reason.clone(),
            upstream_changed: c.upstream_changed,
            upstream_deleted: c.upstream_deleted,
            attempts: c.attempts,
            last_error: c.last_error.clone(),
            next_attempt_at: c.next_attempt_at.map(|t| t.as_millis()),
            user_id: c.user_id.map(|u| u.to_string()),
            created_at: c.created_at.as_millis(),
            updated_at: c.updated_at.as_millis(),
            title: None,
        }
    }
}

/// An item as the edit forms see it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemEditDto {
    pub item_id: String,
    pub collection_id: String,
    /// `text/calendar` or `text/vcard`.
    pub content_type: String,
    /// What is served now, as iCalendar or vCard text: the latest upstream version with the
    /// open change on top.
    pub raw: String,
    /// The event form read from `raw`; absent for contacts and what the form cannot show.
    pub event: Option<EventFormDto>,
    /// The contact form read from `raw`; absent for events.
    pub contact: Option<ContactFormDto>,
    /// Pass back as `expect` to refuse the edit when something else was saved meanwhile.
    pub state: ServedStateDto,
    pub local_edit: Option<LocalEditDto>,
    /// Whether the current user may save edits: the collection is not `read_only` and every
    /// lock for `pim.edit` is open.
    pub writable: bool,
}

/// A collection's write mode and its open changes.
#[derive(Debug, Serialize, ToSchema)]
pub struct WriteModeDto {
    pub collection_id: String,
    /// `read_only`, `held` or `write_through`.
    pub mode: String,
    /// Changes not yet applied or discarded.
    pub open_changes: u64,
    /// Unix milliseconds of the oldest open change.
    pub oldest_open_at: Option<i64>,
    /// Whether the current user may set `write_through`.
    pub may_write_through: bool,
    /// The modes the current user may set now, in order; always holds the current one. Only
    /// `read_only` for a collection no account owns or one of an import account, and nothing
    /// else for someone who does not manage the archive.
    pub allowed_modes: Vec<String>,
}

/// A new write mode.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WriteModeRequest {
    /// `read_only`, `held` or `write_through`.
    pub mode: String,
    /// Required to turn on `write_through` while changes are open: they are sent upstream.
    #[serde(default)]
    pub confirm: bool,
}

/// How to decide an open change.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveRequest {
    /// `keep_mine` (record the change again on the latest upstream version, re-creating the
    /// item when it was deleted upstream), `take_theirs` or `discard` (stop serving it), or
    /// `retry` (send a refused change again).
    pub action: String,
}

/// Filters of the open changes list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PimChangesParams {
    /// Comma-separated states among `held`, `sending`, `conflict`, `rejected`; every open
    /// state by default.
    pub state: Option<String>,
    /// Only this archive's.
    pub archive: Option<String>,
    /// Only this collection's.
    pub collection: Option<String>,
}

fn pim_kind(kind: holtmail_store::ItemKind) -> ItemKind {
    ItemKind::parse(kind.as_str()).unwrap_or(ItemKind::Other)
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}

fn forbidden(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, message.into())
}

fn unprocessable(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

fn edits_error(error: EditsError) -> ApiError {
    match error {
        EditsError::Store(e) => e.into(),
        EditsError::Missing => ApiError::not_found("item bytes"),
        other => ApiError(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}

fn invalid(kind: ItemKind, error: &Invalid) -> ApiError {
    unprocessable(format!("{error} ({})", error.precondition(kind)))
}

fn form_error(kind: ItemKind, error: FormError) -> ApiError {
    match error {
        FormError::Invalid(error) => invalid(kind, &error),
        other => unprocessable(other.to_string()),
    }
}

struct Ctx {
    ingest: Ingest,
    edits: Edits,
}

fn ctx(state: &ApiState) -> Ctx {
    let ingest = Ingest::new(
        state.blobs.clone(),
        state.store.clone(),
        state.index.clone(),
    );
    Ctx {
        edits: Edits::new(state.store.clone(), ingest.clone()),
        ingest,
    }
}

/// Held edits over the API's stores.
pub(crate) fn edits(state: &ApiState) -> Edits {
    ctx(state).edits
}

pub(crate) async fn collection_of(
    state: &ApiState,
    user: &User,
    id: CollectionId,
) -> ApiResult<Collection> {
    let collection = state
        .store
        .get_collection(id)
        .await?
        .ok_or_else(|| ApiError::not_found("collection"))?;
    if role_in(state, user, collection.archive_id).await?.is_none()
        && !readable(state, user)
            .await?
            .contains(&collection.archive_id)
    {
        return Err(ApiError::not_found("collection"));
    }
    Ok(collection)
}

/// Every lock for editing in `collection`: it is not `read_only` (it belongs to an account),
/// and the instance switch, the account's `pim.edit`, an editor grant and `write:pim` allow it.
async fn writable(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    collection: &Collection,
) -> ApiResult<Account> {
    let Some(account) = collection.account_id else {
        return Err(forbidden("this collection cannot be edited"));
    };
    if collection.write_mode == WriteMode::ReadOnly {
        return Err(forbidden("this collection is read-only"));
    }
    may_write(state, user, scopes, account, Capability::PimEdit).await
}

pub(crate) async fn account_name(
    state: &ApiState,
    account: Option<holtmail_core::AccountId>,
) -> Option<String> {
    let id = account?;
    state
        .store
        .get_account(id)
        .await
        .ok()
        .flatten()
        .map(|a| a.name)
}

pub(crate) async fn local_edit(
    state: &ApiState,
    collection: &Collection,
    change: Option<&ItemChange>,
) -> Option<LocalEditDto> {
    let change = change.filter(|c| c.state.is_open())?;
    Some(LocalEditDto {
        change_id: change.id.to_string(),
        state: change.state.as_str().into(),
        op: change.op.as_str().into(),
        class: change.class.as_str().into(),
        reason: change.reason.clone(),
        account_id: collection.account_id.map(|a| a.to_string()),
        account_name: account_name(state, collection.account_id).await,
        write_mode: collection.write_mode.as_str().into(),
        created_at: change.created_at.as_millis(),
    })
}

/// The bytes a request asks for, the kind they are, and the bytes they replace (what is served
/// now) for an edit.
async fn content(
    state: &ApiState,
    collection: &Collection,
    kind: ItemKind,
    base: Option<&[u8]>,
    request: &ItemContentRequest,
    organizer: Option<(String, Option<String>)>,
) -> ApiResult<(Vec<u8>, ItemKind)> {
    let given = [
        request.event.is_some(),
        request.contact.is_some() || request.from_message.is_some(),
        request.raw.is_some(),
    ]
    .iter()
    .filter(|g| **g)
    .count();
    if given != 1 {
        return Err(ApiError::bad_request(
            "give exactly one of event, contact (or from_message) and raw",
        ));
    }
    let now = Timestamp::now().as_millis();
    let contacts = collection.kind == CollectionKind::Contacts;
    if let Some(raw) = &request.raw {
        let kind = if contacts {
            ItemKind::Contact
        } else if base.is_some() {
            kind
        } else {
            holtmail_pim::Format::ICalendar.kind(raw.as_bytes())
        };
        return Ok((raw.clone().into_bytes(), kind));
    }
    if let Some(event) = &request.event {
        if contacts {
            return Err(ApiError::bad_request("an address book holds contacts"));
        }
        let mut form: EventForm = convert(event)?;
        if base.is_none() && form.uid.is_none() {
            form.uid = Some(ItemId::new().to_string());
        }
        let named = base
            .and_then(|b| holtmail_pim_edit::event_form(kind, b))
            .is_some_and(|f| f.organizer.is_some());
        if !form.attendees.is_empty()
            && form.organizer.is_none()
            && !named
            && let Some((email, name)) = organizer
        {
            form.organizer = Some(holtmail_pim_edit::Person {
                email,
                name,
                partstat: None,
            });
        }
        let kind = if base.is_some() {
            kind
        } else {
            ItemKind::Event
        };
        let bytes = holtmail_pim_edit::build_event(kind, &form, base, now)
            .map_err(|e| form_error(kind, e))?;
        return Ok((bytes, kind));
    }
    if !contacts {
        return Err(ApiError::bad_request("a calendar holds events"));
    }
    let mut form: ContactForm = match &request.contact {
        Some(contact) => convert(contact)?,
        None => ContactForm::default(),
    };
    if let Some(message) = &request.from_message {
        let id: MessageId = message
            .parse()
            .map_err(|_| ApiError::not_found("message"))?;
        let message = state
            .store
            .get_message(id)
            .await?
            .filter(|m| m.archive_id == collection.archive_id)
            .ok_or_else(|| ApiError::not_found("message"))?;
        let sender = message
            .from
            .first()
            .ok_or_else(|| unprocessable("the message names no sender"))?;
        if form.full_name.is_none() && form.name == Default::default() {
            form.full_name = sender.name.clone();
        }
        if form.emails.is_empty() {
            form.emails.push(holtmail_pim_edit::Labeled {
                value: sender.email.clone(),
                kinds: Vec::new(),
            });
        }
    }
    if base.is_none() && form.uid.is_none() {
        form.uid = Some(ItemId::new().to_string());
    }
    let bytes = holtmail_pim_edit::build_contact(&form, base, now)
        .map_err(|e| form_error(ItemKind::Contact, e))?;
    Ok((bytes, ItemKind::Contact))
}

struct Recorded {
    change: ItemChange,
}

async fn uid_taken(
    state: &ApiState,
    archive: ArchiveId,
    collection: CollectionId,
    uid: &str,
) -> ApiResult<bool> {
    let mut after = None;
    loop {
        let page = state
            .store
            .list_items(&holtmail_store::ItemQuery {
                archive_id: archive,
                kind: None,
                collection_id: Some(collection),
                removed: holtmail_store::RemovedFilter::Hide,
                order: holtmail_store::ItemOrder::Title,
                after,
                limit: 500,
            })
            .await?;
        if page
            .items
            .iter()
            .any(|i| i.item.key.as_deref() == Some(uid))
        {
            return Ok(true);
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(false),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn record(
    state: &ApiState,
    user: &User,
    account: &Account,
    ctx: &Ctx,
    target: ChangeTarget,
    op: ChangeOp,
    kind: ItemKind,
    bytes: Option<Vec<u8>>,
    base: Option<(u32, Bytes)>,
    expect: Expect,
    record_as: RecordAs,
) -> ApiResult<Recorded> {
    let own = ctx.edits.own_addresses(account.archive_id).await?;
    let (class, content, patch) = match bytes {
        Some(bytes) => {
            let valid =
                holtmail_pim_edit::validate(kind, &bytes, base.as_ref().map(|(_, b)| b.as_ref()))
                    .map_err(|e| invalid(kind, &e))?;
            if let (ChangeOp::Create, ChangeTarget::Href { collection, .. }) = (op, &target)
                && uid_taken(state, account.archive_id, *collection, &valid.uid).await?
            {
                return Err(unprocessable(format!(
                    "another item of the collection has this UID ({})",
                    holtmail_pim_edit::Invalid::UidChanged.precondition(kind)
                )));
            }
            let classified = holtmail_pim_edit::classify(
                kind,
                base.as_ref().map(|(_, b)| b.as_ref()),
                &bytes,
                &own,
            )
            .map_err(|e| invalid(kind, &e))?;
            if record_as.respond.is_some() && classified.class == holtmail_pim_edit::Class::Edit {
                return Err(conflict(
                    "an edit of this event has not reached the account yet; send or discard it first",
                ));
            }
            let class = if op == ChangeOp::Create {
                ChangeClass::Create
            } else {
                ChangeClass::parse(classified.class.as_str()).unwrap_or(ChangeClass::Edit)
            };
            let blob_id = state.blobs.put(Bytes::from(bytes)).await?;
            let patch = match record_as.respond.clone() {
                Some(respond) => classified.patch.with_respond(respond),
                None => classified.patch,
            };
            (
                class,
                Some(ChangeContent {
                    blob_id,
                    content_type: kind.content_type().into(),
                }),
                serde_json::to_value(&patch).ok(),
            )
        }
        None => (ChangeClass::Delete, None, None),
    };
    let new = NewItemChange {
        target,
        op,
        class,
        content,
        patch,
        base_version: base.map(|(n, _)| n),
        origin: ChangeOrigin::Api,
        user_id: Some(user.id),
        device: None,
        at: Timestamp::now(),
        not_before: record_as.not_before,
        messages: record_as.messages,
        schedule_tag: None,
    };
    match state.store.record_change(&new, expect).await {
        Ok(RecordedChange::Recorded(change)) => {
            if let Err(error) = audit(
                state,
                user,
                record_as.audit,
                Some(account.archive_id),
                Some(account.id),
                vec![change.id.to_string()],
            )
            .await
            {
                tracing::warn!(change = %change.id, error = %error.1, "recorded change not audited");
            }
            after(state, ctx, change.archive_id, change.item_id).await;
            Ok(Recorded { change: *change })
        }
        Ok(RecordedChange::PreconditionFailed { .. }) => Err(conflict(
            "the item changed since it was read; load it again",
        )),
        Err(holtmail_store::StoreError::Invalid(message)) => Err(ApiError::bad_request(message)),
        Err(error) => Err(error.into()),
    }
}

/// How a change is recorded: the respond spec of an answered invitation, the audit kind, when
/// it may go upstream and the held outbox messages that belong to it.
pub(crate) struct RecordAs {
    pub respond: Option<holtmail_pim_edit::Respond>,
    pub audit: AuditKind,
    pub not_before: Option<Timestamp>,
    pub messages: Vec<holtmail_core::OutboxId>,
}

impl Default for RecordAs {
    fn default() -> Self {
        Self {
            respond: None,
            audit: AuditKind::PimEdit,
            not_before: None,
            messages: Vec::new(),
        }
    }
}

/// Records `bytes`, the copy with the user's answer, as a change of the item `effective`
/// serves in `collection`, expecting what is served now. Locks are the caller's.
pub(crate) async fn record_answer(
    state: &ApiState,
    user: &User,
    account: &Account,
    collection: &Collection,
    effective: &Effective,
    bytes: Vec<u8>,
    record_as: RecordAs,
) -> ApiResult<ItemChange> {
    let ctx = ctx(state);
    let item = effective
        .item
        .as_ref()
        .ok_or_else(|| ApiError::not_found("item"))?
        .item
        .id;
    if collection.write_mode == WriteMode::ReadOnly {
        return Err(forbidden("this collection is read-only"));
    }
    let base = upstream_base(&ctx, effective, ItemKind::Event).await?;
    Ok(record(
        state,
        user,
        account,
        &ctx,
        ChangeTarget::Item { item },
        ChangeOp::Update,
        ItemKind::Event,
        Some(bytes),
        base,
        Expect::State(effective.state),
        record_as,
    )
    .await?
    .change)
}

async fn after(state: &ApiState, ctx: &Ctx, archive: ArchiveId, item: Option<ItemId>) {
    if let Some(item) = item {
        if let Err(error) = ctx.edits.derive(item).await {
            tracing::warn!(%item, %error, "derived data not rebuilt after an edit");
        }
        if let Err(error) = ctx.ingest.commit().await {
            tracing::warn!(%error, "search index not committed after an edit");
        }
    }
    state.mailboxes.pim_changed(archive);
}

/// The latest upstream version of the item as text, with its number: the base an edit is
/// classified against.
async fn upstream_base(
    ctx: &Ctx,
    effective: &Effective,
    kind: ItemKind,
) -> ApiResult<Option<(u32, Bytes)>> {
    let Some(latest) = &effective.latest else {
        return Ok(None);
    };
    Ok(ctx
        .edits
        .version_text(latest, kind)
        .await
        .map_err(edits_error)?
        .map(|t| (latest.number, t)))
}

async fn item_effective(state: &ApiState, item: ItemId) -> ApiResult<Effective> {
    state
        .store
        .effective(&ChangeTarget::Item { item })
        .await?
        .ok_or_else(|| ApiError::not_found("item"))
}

fn expect_of(expect: Option<&ServedStateDto>) -> ApiResult<Expect> {
    let Some(expect) = expect else {
        return Ok(Expect::Any);
    };
    let change = expect
        .change
        .as_deref()
        .map(|c| c.parse::<ItemChangeId>())
        .transpose()
        .map_err(|_| ApiError::bad_request("malformed expect.change"))?;
    Ok(Expect::State(ServedState {
        version: expect.version,
        change,
    }))
}

#[utoipa::path(
    post,
    path = "/api/collections/{id}/items",
    params(("id" = String, Path, description = "Collection id")),
    request_body = ItemContentRequest,
    responses((status = 201, description = "The held create; with write_through it is sent upstream, and the item appears once it is there", body = PimChangeDto), (status = 400, description = "Not exactly one of event, contact and raw, or a form of the other kind", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A read-only collection, or a closed lock: the instance switch, the account's pim.edit, the grant (editor) or the scope (write:pim)", body = ErrorDto), (status = 404, description = "No such collection or message", body = ErrorDto), (status = 422, description = "Invalid iCalendar or vCard, or a form that cannot become one", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn create_item(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ItemContentRequest>,
) -> ApiResult<(StatusCode, Json<PimChangeDto>)> {
    let id: CollectionId = raw.parse().map_err(|_| ApiError::not_found("collection"))?;
    let collection = collection_of(&state, &user, id).await?;
    let account = writable(&state, &user, &scopes, &collection).await?;
    let change = create_in(&state, &user, &scopes, &account, &collection, &request).await?;
    Ok((StatusCode::CREATED, Json(PimChangeDto::from(&change))))
}

async fn create_in(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    account: &Account,
    collection: &Collection,
    request: &ItemContentRequest,
) -> ApiResult<ItemChange> {
    let ctx = ctx(state);
    let kind = if collection.kind == CollectionKind::Contacts {
        ItemKind::Contact
    } else {
        ItemKind::Event
    };
    let organizer = crate::invitations::organizer_identity(state, user, account).await?;
    let (bytes, kind) = content(state, collection, kind, None, request, organizer).await?;
    let extension = if kind == ItemKind::Contact {
        "vcf"
    } else {
        "ics"
    };
    let href = format!("{}.{extension}", ItemId::new());
    scheduled(
        Scheduled {
            state,
            user,
            scopes,
            account,
            ctx: &ctx,
            collection,
            notify: request.invitations != Some(false),
        },
        ChangeTarget::Href {
            collection: collection.id,
            href,
        },
        ChangeOp::Create,
        kind,
        Some(bytes),
        None,
        None,
        Expect::Absent,
    )
    .await
}

struct Scheduled<'a> {
    state: &'a ApiState,
    user: &'a User,
    scopes: &'a Scopes,
    account: &'a Account,
    ctx: &'a Ctx,
    collection: &'a Collection,
    notify: bool,
}

/// Records a change; for an event the user organizes, also the invitations, updates and
/// cancellations it sends ([`crate::invitations::organize`]), held in the outbox until the
/// change is recorded and dropped when it is not.
#[allow(clippy::too_many_arguments)]
async fn scheduled(
    with: Scheduled<'_>,
    target: ChangeTarget,
    op: ChangeOp,
    kind: ItemKind,
    bytes: Option<Vec<u8>>,
    previous: Option<&[u8]>,
    base: Option<(u32, Bytes)>,
    expect: Expect,
) -> ApiResult<ItemChange> {
    let Scheduled {
        state,
        user,
        scopes,
        account,
        ctx,
        collection,
        notify,
    } = with;
    if kind != ItemKind::Event || !crate::invitations::is_event(collection) {
        return Ok(record(
            state,
            user,
            account,
            ctx,
            target,
            op,
            kind,
            bytes,
            base,
            expect,
            RecordAs::default(),
        )
        .await?
        .change);
    }
    let organizing = crate::invitations::organize(
        state,
        user,
        scopes,
        collection,
        previous,
        bytes.as_deref(),
        notify,
    )
    .await?;
    let bytes = match op {
        ChangeOp::Delete => None,
        _ => organizing.event.or(bytes),
    };
    let record_as = RecordAs {
        messages: crate::invitations::held_ids(&organizing.held),
        ..RecordAs::default()
    };
    match record(
        state, user, account, ctx, target, op, kind, bytes, base, expect, record_as,
    )
    .await
    {
        Ok(recorded) => {
            crate::invitations::release(state, user, organizing.held, "queued").await;
            Ok(recorded.change)
        }
        Err(error) => {
            crate::invitations::drop_held(
                state,
                &holtmail_submission::Actor::web(user.id),
                organizing.held,
            )
            .await;
            Err(error)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/items/{id}/edit",
    params(("id" = String, Path, description = "Item id")),
    responses((status = 200, body = ItemEditDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such item, or nothing served for it (deleted)", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_edit(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<ItemEditDto>> {
    let item: ItemId = raw.parse().map_err(|_| ApiError::not_found("item"))?;
    let effective = item_effective(&state, item).await?;
    let collection = collection_of(&state, &user, effective.collection_id).await?;
    let ctx = ctx(&state);
    let served = ctx
        .edits
        .served(&effective)
        .await
        .map_err(edits_error)?
        .ok_or_else(|| ApiError::not_found("item"))?;
    let (event, contact) = match served.kind {
        ItemKind::Contact => (
            None,
            holtmail_pim_edit::contact_form(&served.raw)
                .map(|f| convert(&f))
                .transpose()?,
        ),
        kind => (
            holtmail_pim_edit::event_form(kind, &served.raw)
                .map(|f| convert(&f))
                .transpose()?,
            None,
        ),
    };
    Ok(Json(ItemEditDto {
        item_id: item.to_string(),
        collection_id: collection.id.to_string(),
        content_type: served.content_type,
        raw: String::from_utf8_lossy(&served.raw).into_owned(),
        event,
        contact,
        state: effective.state.into(),
        local_edit: local_edit(&state, &collection, effective.change.as_ref()).await,
        writable: writable(&state, &user, &scopes, &collection).await.is_ok(),
    }))
}

#[utoipa::path(
    put,
    path = "/api/items/{id}/content",
    params(("id" = String, Path, description = "Item id")),
    request_body = ItemContentRequest,
    responses((status = 200, description = "The held change; with write_through it is sent upstream", body = PimChangeDto), (status = 400, description = "Not exactly one of event, contact and raw", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A read-only collection or a closed lock", body = ErrorDto), (status = 404, description = "No such item", body = ErrorDto), (status = 409, description = "Something else is served than `expect` says", body = ErrorDto), (status = 422, description = "Invalid iCalendar or vCard, a changed UID, or a form that cannot become one", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn edit_item(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ItemContentRequest>,
) -> ApiResult<Json<PimChangeDto>> {
    let item: ItemId = raw.parse().map_err(|_| ApiError::not_found("item"))?;
    Ok(Json(PimChangeDto::from(
        &edit(&state, &user, &scopes, item, &request).await?,
    )))
}

async fn edit(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    item: ItemId,
    request: &ItemContentRequest,
) -> ApiResult<ItemChange> {
    if request.from_message.is_some() {
        return Err(ApiError::bad_request(
            "from_message makes new contacts only",
        ));
    }
    let effective = item_effective(state, item).await?;
    let collection = collection_of(state, user, effective.collection_id).await?;
    let account = writable(state, user, scopes, &collection).await?;
    let ctx = ctx(state);
    let kind = pim_kind(
        effective
            .item
            .as_ref()
            .ok_or_else(|| ApiError::not_found("item"))?
            .item
            .kind,
    );
    let served = ctx
        .edits
        .served(&effective)
        .await
        .map_err(edits_error)?
        .ok_or_else(|| conflict("the item is deleted"))?;
    let organizer = crate::invitations::organizer_identity(state, user, &account).await?;
    let (bytes, kind) = content(
        state,
        &collection,
        kind,
        Some(&served.raw),
        request,
        organizer,
    )
    .await?;
    let base = upstream_base(&ctx, &effective, kind).await?;
    let expect = expect_of(request.expect.as_ref())?;
    scheduled(
        Scheduled {
            state,
            user,
            scopes,
            account: &account,
            ctx: &ctx,
            collection: &collection,
            notify: request.invitations != Some(false),
        },
        ChangeTarget::Item { item },
        ChangeOp::Update,
        kind,
        Some(bytes),
        Some(&served.raw),
        base,
        expect,
    )
    .await
}

/// The served state a delete expects.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteParams {
    /// `state.version` of `ItemEditDto`.
    pub version: Option<u32>,
    /// `state.change` of `ItemEditDto`; with neither, the last writer wins.
    pub change: Option<String>,
    /// For an event the user organizes: `false` deletes it without mailing attendees a
    /// cancellation.
    pub invitations: Option<bool>,
}

#[utoipa::path(
    delete,
    path = "/api/items/{id}",
    params(("id" = String, Path, description = "Item id"), DeleteParams),
    responses((status = 200, description = "The held delete; the archive keeps the item and its versions", body = PimChangeDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A read-only collection or a closed lock", body = ErrorDto), (status = 404, description = "No such item", body = ErrorDto), (status = 409, description = "Something else is served than expected", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn delete_item(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Query(params): Query<DeleteParams>,
) -> ApiResult<Json<PimChangeDto>> {
    let item: ItemId = raw.parse().map_err(|_| ApiError::not_found("item"))?;
    let effective = item_effective(&state, item).await?;
    let collection = collection_of(&state, &user, effective.collection_id).await?;
    let account = writable(&state, &user, &scopes, &collection).await?;
    let ctx = ctx(&state);
    let expect = if params.version.is_some() || params.change.is_some() {
        expect_of(Some(&ServedStateDto {
            version: params.version,
            change: params.change,
        }))?
    } else {
        Expect::Any
    };
    let base = effective.latest.as_ref().map(|v| (v.number, Bytes::new()));
    let kind = effective
        .item
        .as_ref()
        .map(|i| pim_kind(i.item.kind))
        .unwrap_or(ItemKind::Event);
    let previous = if kind == ItemKind::Event {
        ctx.edits
            .served(&effective)
            .await
            .map_err(edits_error)?
            .map(|s| s.raw)
    } else {
        None
    };
    let change = scheduled(
        Scheduled {
            state: &state,
            user: &user,
            scopes: &scopes,
            account: &account,
            ctx: &ctx,
            collection: &collection,
            notify: params.invitations != Some(false),
        },
        ChangeTarget::Item { item },
        ChangeOp::Delete,
        kind,
        None,
        previous.as_deref(),
        base,
        expect,
    )
    .await?;
    Ok(Json(PimChangeDto::from(&change)))
}

fn states(raw: Option<&str>) -> ApiResult<Vec<ItemChangeState>> {
    let Some(raw) = raw.filter(|r| !r.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(|s| {
            ItemChangeState::parse(s.trim())
                .filter(|s| s.is_open())
                .ok_or_else(|| {
                    ApiError::bad_request("state must be held, sending, conflict or rejected")
                })
        })
        .collect()
}

#[utoipa::path(
    get,
    path = "/api/pim/changes",
    params(PimChangesParams),
    responses((status = 200, description = "Open changes of the archives the user may read, oldest first, at most 500 per archive", body = Vec<PimChangeDto>), (status = 400, description = "An unknown state", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive or collection id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_changes(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Query(params): Query<PimChangesParams>,
) -> ApiResult<Json<Vec<PimChangeDto>>> {
    let wanted = states(params.state.as_deref())?;
    let archive = params
        .archive
        .as_deref()
        .map(|a| a.parse::<ArchiveId>())
        .transpose()
        .map_err(|_| ApiError::not_found("archive"))?;
    let collection = params
        .collection
        .as_deref()
        .map(|c| c.parse::<CollectionId>())
        .transpose()
        .map_err(|_| ApiError::not_found("collection"))?;
    let mut out = Vec::new();
    for id in readable(&state, &user).await? {
        if archive.is_some_and(|a| a != id) {
            continue;
        }
        let changes = state
            .store
            .open_changes(&ItemChangeQuery {
                archive_id: id,
                collection_id: collection,
                states: wanted.clone(),
                flagged_only: false,
                after: None,
                limit: 500,
            })
            .await?;
        for change in &changes {
            let mut dto = PimChangeDto::from(change);
            if let Some(item) = change.item_id {
                dto.title = state
                    .store
                    .get_item(item)
                    .await?
                    .and_then(|i| i.index)
                    .and_then(|i| i.title);
            }
            out.push(dto);
        }
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/pim/changes/{id}/resolve",
    params(("id" = String, Path, description = "Change id")),
    request_body = ResolveRequest,
    responses((status = 200, description = "The decided change, or for keep_mine the new change that takes its place", body = PimChangeDto), (status = 400, description = "An unknown action", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A read-only collection or a closed lock", body = ErrorDto), (status = 404, description = "No such change", body = ErrorDto), (status = 409, description = "The change is not in a state the action applies to (being sent, applied, or retry of a change that was not refused)", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn resolve_change(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ResolveRequest>,
) -> ApiResult<Json<PimChangeDto>> {
    let id: ItemChangeId = raw.parse().map_err(|_| ApiError::not_found("change"))?;
    let change = state
        .store
        .get_change(id)
        .await?
        .ok_or_else(|| ApiError::not_found("change"))?;
    let collection = collection_of(&state, &user, change.collection_id).await?;
    let account = match collection.account_id {
        Some(account) if change.class == ChangeClass::Reply && request.action == "discard" => {
            may_write(&state, &user, &scopes, account, Capability::PimRespond).await?
        }
        _ => writable(&state, &user, &scopes, &collection).await?,
    };
    let ctx = ctx(&state);
    let resolution = match request.action.as_str() {
        "keep_mine" => {
            let kept = keep_mine(&state, &user, &account, &collection, &ctx, &change).await?;
            return Ok(Json(PimChangeDto::from(&kept)));
        }
        "take_theirs" | "discard" => Resolution::Discard,
        "retry" => Resolution::Retry,
        _ => {
            return Err(ApiError::bad_request(
                "action must be keep_mine, take_theirs, discard or retry",
            ));
        }
    };
    let decided = state
        .store
        .resolve_change(id, &resolution, Timestamp::now())
        .await?
        .ok_or_else(|| conflict("the change is not in a state this applies to"))?;
    audit(
        &state,
        &user,
        AuditKind::PimEdit,
        Some(account.archive_id),
        Some(account.id),
        vec![decided.id.to_string()],
    )
    .await?;
    after(&state, &ctx, decided.archive_id, decided.item_id).await;
    Ok(Json(PimChangeDto::from(&decided)))
}

/// One field of a change compared with its base and the latest upstream version.
#[derive(Debug, Serialize, ToSchema)]
pub struct FieldDiffDto {
    /// The field as the item history names it, such as `summary`, `start`, `attendee`, `email`
    /// or `instance 2026-12-01T10:00:00 Europe/Berlin summary`.
    pub field: String,
    /// Its values in the version the change was made on; empty when it had none.
    pub base: Vec<String>,
    /// Its values in the change.
    pub mine: Vec<String>,
    /// Its values in the latest upstream version; empty when the item is gone upstream.
    pub theirs: Vec<String>,
    /// The change differs from the base here.
    pub mine_changed: bool,
    /// Upstream differs from the base here.
    pub theirs_changed: bool,
    /// Both differ from the base, and from each other.
    pub conflict: bool,
}

/// A change next to the version it was made on and the latest upstream version.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChangeDiffDto {
    pub change_id: String,
    /// `text/calendar` or `text/vcard`.
    pub content_type: String,
    /// The upstream version the change was made on; absent for a create.
    pub base_version: Option<u32>,
    /// The latest upstream version; absent for a create that has not reached the account.
    pub latest_version: Option<u32>,
    /// The item is gone upstream.
    pub upstream_deleted: bool,
    /// The change's own iCalendar or vCard text; absent for a delete.
    pub mine: Option<String>,
    /// The fields where the change or upstream differ from the base, in the order they first
    /// appear. Alarms, client properties, `SEQUENCE`, `REV` and `UID` are left out.
    pub fields: Vec<FieldDiffDto>,
}

#[utoipa::path(
    get,
    path = "/api/pim/changes/{id}/diff",
    params(("id" = String, Path, description = "Change id")),
    responses((status = 200, body = ChangeDiffDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such change, or one in an archive the user may not read", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn change_diff(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<ChangeDiffDto>> {
    let id: ItemChangeId = raw.parse().map_err(|_| ApiError::not_found("change"))?;
    let change = state
        .store
        .get_change(id)
        .await?
        .ok_or_else(|| ApiError::not_found("change"))?;
    let collection = collection_of(&state, &user, change.collection_id).await?;
    let ctx = ctx(&state);
    let mine = match &change.content {
        Some(content) => Some(
            state
                .blobs
                .get(&content.blob_id)
                .await?
                .ok_or_else(|| ApiError::not_found("change bytes"))?,
        ),
        None => None,
    };
    let mut kind = if collection.kind == CollectionKind::Contacts {
        ItemKind::Contact
    } else {
        ItemKind::Event
    };
    let (mut base, mut theirs, mut base_version, mut latest_version, mut gone) =
        (None, None, None, None, false);
    if let Some(item) = change.item_id {
        let effective = item_effective(&state, item).await?;
        if let Some(summary) = &effective.item {
            kind = pim_kind(summary.item.kind);
            gone = summary.item.removed_at.is_some();
        }
        if let Some(number) = change.base_version
            && let Some(version) = state
                .store
                .item_versions(item)
                .await?
                .into_iter()
                .find(|v| v.number == number)
        {
            base_version = Some(number);
            base = ctx
                .edits
                .version_text(&version, kind)
                .await
                .map_err(edits_error)?;
        }
        if let Some(latest) = &effective.latest {
            latest_version = Some(latest.number);
            if !gone {
                theirs = ctx
                    .edits
                    .version_text(latest, kind)
                    .await
                    .map_err(edits_error)?;
            }
        }
    }
    let fields = holtmail_pim_edit::field_diff(
        kind,
        base.as_deref(),
        mine.as_deref(),
        if change.item_id.is_some() {
            theirs.as_deref()
        } else {
            None
        },
    )
    .into_iter()
    .map(|f| FieldDiffDto {
        field: f.field,
        base: f.base,
        mine: f.mine,
        theirs: f.theirs,
        mine_changed: f.mine_changed,
        theirs_changed: f.theirs_changed,
        conflict: f.conflict,
    })
    .collect();
    Ok(Json(ChangeDiffDto {
        change_id: change.id.to_string(),
        content_type: if kind == ItemKind::Contact {
            "text/vcard".into()
        } else {
            "text/calendar".into()
        },
        base_version,
        latest_version,
        upstream_deleted: gone || change.upstream_deleted,
        mine: mine.map(|m| String::from_utf8_lossy(&m).into_owned()),
        fields,
    }))
}

/// Records the change again on the latest upstream version: its own bytes as an update (or a
/// delete again), or, when the item is gone upstream, as a create of a new item under a new
/// href, discarding the old change.
async fn keep_mine(
    state: &ApiState,
    user: &User,
    account: &Account,
    collection: &Collection,
    ctx: &Ctx,
    change: &ItemChange,
) -> ApiResult<ItemChange> {
    if !matches!(
        change.state,
        ItemChangeState::Held | ItemChangeState::Conflict | ItemChangeState::Rejected
    ) {
        return Err(conflict("the change is not in a state this applies to"));
    }
    let Some(item) = change.item_id else {
        return Err(conflict(
            "a create that has not reached upstream is kept as it is",
        ));
    };
    let effective = item_effective(state, item).await?;
    let kind = effective
        .item
        .as_ref()
        .map(|i| pim_kind(i.item.kind))
        .unwrap_or(ItemKind::Event);
    let mine = match &change.content {
        Some(content) => Some(
            state
                .blobs
                .get(&content.blob_id)
                .await?
                .ok_or_else(|| ApiError::not_found("change bytes"))?
                .to_vec(),
        ),
        None => None,
    };
    let gone = effective
        .item
        .as_ref()
        .is_some_and(|i| i.item.removed_at.is_some());
    if gone {
        let Some(mine) = mine else {
            state
                .store
                .resolve_change(change.id, &Resolution::Applied, Timestamp::now())
                .await?;
            audit(
                state,
                user,
                AuditKind::PimEdit,
                Some(account.archive_id),
                Some(account.id),
                vec![change.id.to_string()],
            )
            .await?;
            return Ok(state
                .store
                .get_change(change.id)
                .await?
                .unwrap_or_else(|| change.clone()));
        };
        let extension = if kind == ItemKind::Contact {
            "vcf"
        } else {
            "ics"
        };
        let recreated = record(
            state,
            user,
            account,
            ctx,
            ChangeTarget::Href {
                collection: collection.id,
                href: format!("{}.{extension}", ItemId::new()),
            },
            ChangeOp::Create,
            kind,
            Some(mine),
            None,
            Expect::Absent,
            RecordAs::default(),
        )
        .await?
        .change;
        state
            .store
            .resolve_change(change.id, &Resolution::Discard, Timestamp::now())
            .await?;
        audit(
            state,
            user,
            AuditKind::PimEdit,
            Some(account.archive_id),
            Some(account.id),
            vec![change.id.to_string(), recreated.id.to_string()],
        )
        .await?;
        after(state, ctx, change.archive_id, Some(item)).await;
        return Ok(recreated);
    }
    let base = upstream_base(ctx, &effective, kind).await?;
    let op = if mine.is_some() {
        ChangeOp::Update
    } else {
        ChangeOp::Delete
    };
    let base = match (op, base) {
        (ChangeOp::Delete, None) => effective.latest.as_ref().map(|v| (v.number, Bytes::new())),
        (_, base) => base,
    };
    Ok(record(
        state,
        user,
        account,
        ctx,
        ChangeTarget::Item { item },
        op,
        kind,
        mine,
        base,
        Expect::Any,
        RecordAs::default(),
    )
    .await?
    .change)
}

async fn open_of(state: &ApiState, collection: &Collection) -> ApiResult<Vec<ItemChange>> {
    Ok(state
        .store
        .open_changes(&ItemChangeQuery {
            archive_id: collection.archive_id,
            collection_id: Some(collection.id),
            states: Vec::new(),
            flagged_only: false,
            after: None,
            limit: 500,
        })
        .await?)
}

async fn mode_dto(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    collection: &Collection,
) -> ApiResult<WriteModeDto> {
    let open = open_of(state, collection).await?;
    let may_write_through = manages(user)
        && match collection.account_id {
            Some(account) => may_write(state, user, scopes, account, Capability::PimEdit)
                .await
                .is_ok(),
            None => false,
        };
    let imported = match collection.account_id {
        Some(account) => state
            .store
            .get_account(account)
            .await?
            .is_none_or(|a| a.kind == holtmail_store::AccountKind::Import),
        None => true,
    };
    let allowed_modes = [
        WriteMode::ReadOnly,
        WriteMode::Held,
        WriteMode::WriteThrough,
    ]
    .into_iter()
    .filter(|m| {
        *m == collection.write_mode
            || (manages(user)
                && match m {
                    WriteMode::ReadOnly => true,
                    WriteMode::Held => !imported,
                    WriteMode::WriteThrough => !imported && may_write_through,
                })
    })
    .map(|m| m.as_str().to_owned())
    .collect();
    Ok(WriteModeDto {
        collection_id: collection.id.to_string(),
        mode: collection.write_mode.as_str().into(),
        open_changes: open.len() as u64,
        oldest_open_at: open.first().map(|c| c.created_at.as_millis()),
        may_write_through,
        allowed_modes,
    })
}

#[utoipa::path(
    get,
    path = "/api/collections/{id}/write-mode",
    params(("id" = String, Path, description = "Collection id")),
    responses((status = 200, body = WriteModeDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such collection", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_write_mode(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<WriteModeDto>> {
    let id: CollectionId = raw.parse().map_err(|_| ApiError::not_found("collection"))?;
    let collection = collection_of(&state, &user, id).await?;
    Ok(Json(mode_dto(&state, &user, &scopes, &collection).await?))
}

#[utoipa::path(
    put,
    path = "/api/collections/{id}/write-mode",
    params(("id" = String, Path, description = "Collection id")),
    request_body = WriteModeRequest,
    responses((status = 200, body = WriteModeDto), (status = 400, description = "An unknown mode, or a mode other than read_only for a collection no account owns", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, or write_through without every lock for pim.edit", body = ErrorDto), (status = 404, description = "No such collection", body = ErrorDto), (status = 409, description = "write_through while changes are open, without confirm", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn put_write_mode(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<WriteModeRequest>,
) -> ApiResult<Json<WriteModeDto>> {
    let id: CollectionId = raw.parse().map_err(|_| ApiError::not_found("collection"))?;
    let collection = collection_of(&state, &user, id).await?;
    let mode = WriteMode::parse(&request.mode)
        .ok_or_else(|| ApiError::bad_request("mode must be read_only, held or write_through"))?;
    if !manages(&user) {
        return Err(forbidden("only the owner and admins can do this"));
    }
    if mode == WriteMode::WriteThrough && collection.write_mode != WriteMode::WriteThrough {
        let account = collection
            .account_id
            .ok_or_else(|| ApiError::bad_request("a collection no account owns stays read-only"))?;
        may_write(&state, &user, &scopes, account, Capability::PimEdit).await?;
        let open = open_of(&state, &collection).await?.len();
        if open > 0 && !request.confirm {
            return Err(conflict(format!(
                "{open} open changes would be sent to the account; send again with confirm"
            )));
        }
    }
    let changed = match state.store.set_write_mode(id, mode).await {
        Err(holtmail_store::StoreError::Invalid(message)) => {
            return Err(ApiError::bad_request(message));
        }
        other => other?,
    };
    audit(
        &state,
        &user,
        AuditKind::CapabilitiesChanged,
        Some(changed.archive_id),
        changed.account_id,
        vec![
            changed.id.to_string(),
            format!("write_mode:{}", mode.as_str()),
        ],
    )
    .await?;
    state.mailboxes.pim_changed(changed.archive_id);
    Ok(Json(mode_dto(&state, &user, &scopes, &changed).await?))
}

/// The card a person's edits go to: their card, not marked removed, in an editable address
/// book (one of an account, not `read_only`), in the address book stored first; the smallest
/// item id among cards there. `None` when no card qualifies.
pub(crate) async fn primary_card(
    state: &ApiState,
    person: &holtmail_store::Person,
) -> ApiResult<Option<(ItemId, Collection)>> {
    let mut best: Option<(ItemId, Collection)> = None;
    for card in person.cards.iter().filter(|c| !c.removed) {
        let Some(collection) = state.store.get_collection(card.collection_id).await? else {
            continue;
        };
        if collection.account_id.is_none()
            || collection.write_mode == WriteMode::ReadOnly
            || collection.removed_at.is_some()
        {
            continue;
        }
        let better = match &best {
            None => true,
            Some((item, current)) => {
                (collection.created_at, collection.id, card.item_id)
                    < (current.created_at, current.id, *item)
            }
        };
        if better {
            best = Some((card.item_id, collection));
        }
    }
    Ok(best)
}

#[utoipa::path(
    put,
    path = "/api/people/{id}/card",
    params(("id" = String, Path, description = "Person id")),
    request_body = ItemContentRequest,
    responses((status = 200, description = "The held change to the person's primary card (their card in the address book stored first among the editable ones), or a new card in the archive's first editable address book when they have none", body = PimChangeDto), (status = 400, description = "Not a contact form or raw vCard", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock", body = ErrorDto), (status = 404, description = "No such person", body = ErrorDto), (status = 409, description = "No editable address book, or something else served than `expect` says", body = ErrorDto), (status = 422, description = "Invalid vCard or form", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn edit_person_card(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ItemContentRequest>,
) -> ApiResult<Json<PimChangeDto>> {
    let id: holtmail_core::PersonId = raw.parse().map_err(|_| ApiError::not_found("person"))?;
    let person = state
        .store
        .get_person(id)
        .await?
        .ok_or_else(|| ApiError::not_found("person"))?;
    if role_in(&state, &user, person.archive_id).await?.is_none() {
        return Err(ApiError::not_found("person"));
    }
    if request.event.is_some() {
        return Err(ApiError::bad_request("a person's card is a contact"));
    }
    if let Some((item, _)) = primary_card(&state, &person).await? {
        return Ok(Json(PimChangeDto::from(
            &edit(&state, &user, &scopes, item, &request).await?,
        )));
    }
    let book = state
        .store
        .list_collections(person.archive_id)
        .await?
        .into_iter()
        .map(|s| s.collection)
        .filter(|c| {
            c.kind == CollectionKind::Contacts
                && c.account_id.is_some()
                && c.write_mode != WriteMode::ReadOnly
                && c.removed_at.is_none()
        })
        .min_by_key(|c| (c.created_at, c.id))
        .ok_or_else(|| conflict("no address book of this archive can be edited"))?;
    let account = writable(&state, &user, &scopes, &book).await?;
    let mut request = request;
    if request.contact.is_none() && request.raw.is_none() {
        request.contact = Some(ContactFormDto {
            full_name: person.name.clone(),
            emails: person
                .email
                .iter()
                .map(|e| LabeledDto {
                    value: e.clone(),
                    kinds: Vec::new(),
                })
                .collect(),
            ..ContactFormDto::default()
        });
    }
    let change = create_in(&state, &user, &scopes, &account, &book, &request).await?;
    Ok(Json(PimChangeDto::from(&change)))
}

/// One entry of an item's timeline: an upstream version or a change made in Holtmail, in the
/// order they happened.
#[derive(Debug, Serialize, ToSchema)]
pub struct TimelineEntryDto {
    /// `version` or `change`.
    pub kind: String,
    /// Unix milliseconds: when the version was first seen or the change made.
    pub at: i64,
    /// For a version: its number.
    pub number: Option<u32>,
    /// For a version: `upstream` or `writeback` (what the account held after Holtmail sent a
    /// change).
    pub origin: Option<String>,
    /// For a change, and a writeback version: the change.
    pub change: Option<PimChangeDto>,
}

/// The timeline of an item: its versions and its changes, oldest first; a version and a change
/// at the same moment list the version first.
pub(crate) async fn timeline(state: &ApiState, item: ItemId) -> ApiResult<Vec<TimelineEntryDto>> {
    let versions = state.store.item_versions(item).await?;
    let changes = state.store.changes_of(&ChangeTarget::Item { item }).await?;
    let mut out: Vec<(i64, u8, TimelineEntryDto)> = Vec::new();
    for version in versions {
        let change = version
            .change_id
            .and_then(|id| changes.iter().find(|c| c.id == id))
            .map(PimChangeDto::from);
        out.push((
            version.seen_at.as_millis(),
            0,
            TimelineEntryDto {
                kind: "version".into(),
                at: version.seen_at.as_millis(),
                number: Some(version.number),
                origin: Some(version.origin.as_str().into()),
                change,
            },
        ));
    }
    for change in &changes {
        out.push((
            change.created_at.as_millis(),
            1,
            TimelineEntryDto {
                kind: "change".into(),
                at: change.created_at.as_millis(),
                number: None,
                origin: None,
                change: Some(PimChangeDto::from(change)),
            },
        ));
    }
    out.sort_by_key(|(at, order, _)| (*at, *order));
    Ok(out.into_iter().map(|(_, _, e)| e).collect())
}

/// Creates an item in the archive as the user, for rule actions: in `collection` when given,
/// else in the first collection of `kind` the user may edit. Every lock of a form create
/// applies.
pub(crate) async fn create_for_rule(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    archive: ArchiveId,
    kind: CollectionKind,
    collection: Option<&str>,
    request: &ItemContentRequest,
) -> ApiResult<ItemChange> {
    let chosen = match collection {
        Some(raw) => {
            let id: CollectionId = raw.parse().map_err(|_| ApiError::not_found("collection"))?;
            let found = collection_of(state, user, id).await?;
            if found.archive_id != archive || found.kind != kind {
                return Err(ApiError::not_found("collection"));
            }
            found
        }
        None => {
            let mut found = None;
            for summary in state.store.list_collections(archive).await? {
                let candidate = summary.collection;
                if candidate.kind != kind || candidate.removed_at.is_some() {
                    continue;
                }
                if writable(state, user, scopes, &candidate).await.is_ok() {
                    found = Some(candidate);
                    break;
                }
            }
            found.ok_or_else(|| forbidden("no collection of this kind may be edited"))?
        }
    };
    let account = writable(state, user, scopes, &chosen).await?;
    create_in(state, user, scopes, &account, &chosen, request).await
}
