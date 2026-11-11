use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use holtmail_core::{AccountId, ArchiveId, CollectionId, ItemId, MessageId};
use holtmail_pim::{Method, Normalized};
use holtmail_store::{
    Capability, CollectionSummary, ItemKind, ItemOrder, ItemQuery, ItemSummary, ItemVersion,
    Message, PageCursor, RemovedFilter, WriteMode,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::dto::ErrorDto;
use crate::write::{Scopes, may_write};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// An address book or a calendar.
#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionDto {
    pub id: String,
    pub archive_id: String,
    /// The account it was read from; absent once that account was removed.
    pub account_id: Option<String>,
    /// That account's name.
    pub account_name: Option<String>,
    /// When it was last synced from its server, Unix milliseconds; absent for imported files
    /// and the calendar of events found in mail.
    pub synced_at: Option<i64>,
    /// `contacts` or `calendar`.
    pub kind: String,
    pub name: String,
    pub color: Option<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// When it was noticed gone from the source, Unix milliseconds; it stays readable.
    pub removed_at: Option<i64>,
    /// Items not marked removed.
    pub items: u64,
    /// Items marked removed.
    pub removed_items: u64,
    /// `read_only`, `held` (edits stay in Holtmail) or `write_through` (edits are sent to the
    /// account).
    pub write_mode: String,
    /// Whether the current user may create, change and delete its items now: it belongs to an
    /// account, is not `read_only` and not removed, and every lock for `pim.edit` is open.
    pub editable: bool,
}

impl From<CollectionSummary> for CollectionDto {
    fn from(summary: CollectionSummary) -> Self {
        let c = summary.collection;
        Self {
            id: c.id.to_string(),
            archive_id: c.archive_id.to_string(),
            account_id: c.account_id.map(|a| a.to_string()),
            account_name: None,
            synced_at: c.synced_at.map(|t| t.as_millis()),
            kind: c.kind.as_str().into(),
            name: c.name,
            color: c.color,
            created_at: c.created_at.as_millis(),
            removed_at: c.removed_at.map(|t| t.as_millis()),
            items: summary.items,
            removed_items: summary.removed_items,
            write_mode: c.write_mode.as_str().into(),
            editable: false,
        }
    }
}

/// A contact, event or task with the fields of its latest version.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemSummaryDto {
    pub id: String,
    pub archive_id: String,
    pub collection_id: String,
    /// `contact`, `event`, `task` or `other`.
    pub kind: String,
    /// A contact's name or an event's summary.
    pub title: Option<String>,
    /// Whether the latest version could be read; an unreadable one is still archived.
    pub readable: bool,
    /// How many versions are archived.
    pub versions: u32,
    /// Unix milliseconds.
    pub first_seen_at: i64,
    /// When it was noticed gone from the source, Unix milliseconds.
    pub removed_at: Option<i64>,
    /// Start in UTC, Unix milliseconds.
    pub starts_at: Option<i64>,
    /// End in UTC, Unix milliseconds.
    pub ends_at: Option<i64>,
    pub all_day: bool,
    /// The start's original time zone, as written.
    pub time_zone: Option<String>,
    pub recurring: bool,
    /// iCalendar `UID`, or a contact's `UID`.
    pub uid: Option<String>,
    /// The organizer's address.
    pub organizer: Option<String>,
    /// Lowercased addresses: a contact's, or an event's organizer and attendees.
    pub emails: Vec<String>,
    /// A contact's phone numbers in E.164 form.
    pub phones: Vec<String>,
    /// Set when what is served comes from a change made in Holtmail or on a device.
    pub local_edit: Option<crate::pim_edit::LocalEditDto>,
}

impl From<&ItemSummary> for ItemSummaryDto {
    fn from(summary: &ItemSummary) -> Self {
        let item = &summary.item;
        let index = summary.index.as_ref();
        Self {
            id: item.id.to_string(),
            archive_id: item.archive_id.to_string(),
            collection_id: item.collection_id.to_string(),
            kind: item.kind.as_str().into(),
            title: index.and_then(|i| i.title.clone()),
            readable: index.is_some_and(|i| i.readable),
            versions: item.versions,
            first_seen_at: item.first_seen_at.as_millis(),
            removed_at: item.removed_at.map(|t| t.as_millis()),
            starts_at: index.and_then(|i| i.starts_at).map(|t| t.as_millis()),
            ends_at: index.and_then(|i| i.ends_at).map(|t| t.as_millis()),
            all_day: index.is_some_and(|i| i.all_day),
            time_zone: index.and_then(|i| i.time_zone.clone()),
            recurring: index.is_some_and(|i| i.recurring),
            uid: index.and_then(|i| i.uid.clone()),
            organizer: index.and_then(|i| i.organizer.clone()),
            emails: index.map(|i| i.emails.clone()).unwrap_or_default(),
            phones: index.map(|i| i.phones.clone()).unwrap_or_default(),
            local_edit: None,
        }
    }
}

/// One page of contacts or events.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemPage {
    pub items: Vec<ItemSummaryDto>,
    /// Pass as `after` for the next page; absent on the last page.
    pub next: Option<String>,
}

/// One difference between a version and the one before it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChangeDto {
    /// Such as `email`, `start`, or `instance 2025-03-17T09:00:00 Europe/Berlin summary` for a
    /// changed instance of a recurring event.
    pub field: String,
    /// Absent when the value was added.
    pub before: Option<String>,
    /// Absent when the value was removed.
    pub after: Option<String>,
}

/// A message a version of an item was found in.
#[derive(Debug, Serialize, ToSchema)]
pub struct SourceMessageDto {
    pub id: String,
    pub subject: Option<String>,
    pub from: Vec<crate::dto::AddressDto>,
    /// The message's date, Unix milliseconds.
    pub date: i64,
}

/// One archived version of an item.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemVersionDto {
    /// 1 for the first version. Versions found in mail are numbered in the order of their
    /// `SEQUENCE`, `DTSTAMP` and message date, so a version found later can move later ones up.
    pub number: u32,
    /// For a version found in mail: its iTIP method, lowercase (`request`, `reply`, `cancel`,
    /// `publish`, `add`, `counter`, `declinecounter`, `refresh`); absent when the calendar part
    /// named none or the version did not come from mail.
    pub method: Option<String>,
    /// For a version found in mail: its `SEQUENCE`.
    pub sequence: Option<i64>,
    /// The messages this version was found in, oldest first; empty when it did not come from mail.
    pub messages: Vec<SourceMessageDto>,
    /// When it was first seen, Unix milliseconds.
    pub seen_at: i64,
    /// `text/vcard` or `text/calendar`.
    pub content_type: String,
    /// The source's version tag.
    pub etag: Option<String>,
    /// Whether it could be read.
    pub readable: bool,
    /// The raw bytes exactly as received.
    pub raw_url: String,
    /// What changed from the version before; empty for the first. For a reply, counter or
    /// refresh found in mail: the attendees it speaks for, as `answer` changes.
    pub changes: Vec<ChangeDto>,
}

/// An item with every version and what changed between them.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemDetail {
    pub item: ItemSummaryDto,
    /// The normalized view of what is served: the open change when there is one (see
    /// `item.local_edit`), else the latest version: `{"type": "contact", ...}`,
    /// `{"type": "calendar", "components": [...]}` or `{"type": "unreadable"}`. For an event
    /// found in mail, the versions merged: the latest invitation with later instance changes,
    /// every attendee's latest answer, and `status` `CANCELLED` once cancelled.
    #[schema(value_type = Object)]
    pub data: serde_json::Value,
    /// Oldest first.
    pub versions: Vec<ItemVersionDto>,
    /// Versions and changes made in Holtmail or on devices, interleaved, oldest first.
    pub timeline: Vec<crate::pim_edit::TimelineEntryDto>,
}

/// Paging and filters of a contact or event list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ItemListParams {
    /// Only items of this collection.
    pub collection: Option<String>,
    /// `hide` (default) leaves out items removed from their source, `include` lists them too,
    /// `only` lists just them.
    pub removed: Option<String>,
    /// `next` from the previous page.
    pub after: Option<String>,
    /// Page size, at most 500; 50 by default.
    pub limit: Option<u32>,
}

fn id<T: std::str::FromStr>(raw: &str, what: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| ApiError::not_found(what))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/collections",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "Address books and calendars, removed ones too, by kind and name", body = Vec<CollectionDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_collections(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<CollectionDto>>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let collections = state.store.list_collections(archive).await?;
    let accounts = state.store.list_accounts(archive).await?;
    let mut editors: HashMap<AccountId, bool> = HashMap::new();
    let mut out = Vec::with_capacity(collections.len());
    for summary in collections {
        let account = summary.collection.account_id;
        let open = summary.collection.write_mode != WriteMode::ReadOnly
            && summary.collection.removed_at.is_none();
        let editable = match account {
            Some(account) if open => match editors.get(&account) {
                Some(editable) => *editable,
                None => {
                    let editable = may_write(&state, &user, &scopes, account, Capability::PimEdit)
                        .await
                        .is_ok();
                    editors.insert(account, editable);
                    editable
                }
            },
            _ => false,
        };
        let mut dto = CollectionDto::from(summary);
        dto.editable = editable;
        dto.account_name = accounts
            .iter()
            .find(|a| Some(a.id) == account)
            .map(|a| a.name.clone());
        out.push(dto);
    }
    Ok(Json(out))
}

async fn list(
    state: &ApiState,
    raw: &str,
    params: ItemListParams,
    kind: ItemKind,
    order: ItemOrder,
) -> ApiResult<Json<ItemPage>> {
    let archive: ArchiveId = id(raw, "archive")?;
    let collection_id = params
        .collection
        .as_deref()
        .map(|c| id::<CollectionId>(c, "collection"))
        .transpose()?;
    let removed = match params.removed.as_deref() {
        None | Some("hide") => RemovedFilter::Hide,
        Some("include") => RemovedFilter::Include,
        Some("only") => RemovedFilter::Only,
        Some(_) => {
            return Err(ApiError::bad_request(
                "removed must be hide, include or only",
            ));
        }
    };
    let page = state
        .store
        .list_items(&ItemQuery {
            archive_id: archive,
            kind: Some(kind),
            collection_id,
            removed,
            order,
            after: params.after.map(PageCursor),
            limit: params.limit.unwrap_or(50),
        })
        .await
        .map_err(|error| match error {
            holtmail_store::StoreError::Invalid(message) => ApiError::bad_request(message),
            other => other.into(),
        })?;
    let open = state
        .store
        .open_changes(&holtmail_store::ItemChangeQuery {
            archive_id: archive,
            collection_id,
            states: Vec::new(),
            flagged_only: false,
            after: None,
            limit: 500,
        })
        .await?;
    let mut collections: Vec<holtmail_store::Collection> = Vec::new();
    let mut items = Vec::with_capacity(page.items.len());
    for summary in &page.items {
        let mut dto = ItemSummaryDto::from(summary);
        if let Some(change) = open
            .iter()
            .rev()
            .find(|c| c.item_id == Some(summary.item.id))
        {
            if !collections.iter().any(|c| c.id == change.collection_id)
                && let Some(found) = state.store.get_collection(change.collection_id).await?
            {
                collections.push(found);
            }
            if let Some(collection) = collections.iter().find(|c| c.id == change.collection_id) {
                dto.local_edit = crate::pim_edit::local_edit(state, collection, Some(change)).await;
            }
        }
        items.push(dto);
    }
    Ok(Json(ItemPage {
        items,
        next: page.next.map(|c| c.0),
    }))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/contacts",
    params(("id" = String, Path, description = "Archive id"), ItemListParams),
    responses((status = 200, description = "Contacts by name, without regard to letter case; unnamed ones last", body = ItemPage), (status = 400, description = "A malformed cursor or removed filter", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive or collection id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_contacts(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<ItemListParams>,
) -> ApiResult<Json<ItemPage>> {
    list(&state, &raw, params, ItemKind::Contact, ItemOrder::Title).await
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/events",
    params(("id" = String, Path, description = "Archive id"), ItemListParams),
    responses((status = 200, description = "Events by start; a recurring event once, by its first start; events without a start last", body = ItemPage), (status = 400, description = "A malformed cursor or removed filter", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive or collection id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_events(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<ItemListParams>,
) -> ApiResult<Json<ItemPage>> {
    list(&state, &raw, params, ItemKind::Event, ItemOrder::Start).await
}

fn pim_kind(kind: ItemKind) -> holtmail_pim::ItemKind {
    holtmail_pim::ItemKind::parse(kind.as_str()).unwrap_or(holtmail_pim::ItemKind::Other)
}

/// A time of an event: as written, in its zone, and in UTC.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventTimeDto {
    /// As written: `2025-03-10` for a date, `2025-03-10T09:00:00` for a time, with `Z` or an
    /// offset when written so.
    pub value: String,
    /// The original zone: a `TZID` such as `Europe/Berlin`, `UTC`, an offset, or absent for
    /// floating times and dates.
    pub time_zone: Option<String>,
    /// Unix milliseconds.
    pub utc: Option<i64>,
}

impl From<&holtmail_pim::EventTime> for EventTimeDto {
    fn from(t: &holtmail_pim::EventTime) -> Self {
        Self {
            value: t.value.clone(),
            time_zone: t.time_zone.clone(),
            utc: t.utc,
        }
    }
}

/// An organizer or attendee.
#[derive(Debug, Serialize, ToSchema)]
pub struct ParticipantDto {
    /// Lowercased address.
    pub email: Option<String>,
    pub name: Option<String>,
    /// Lowercase `PARTSTAT`, such as `accepted`, `declined`, `tentative`, `needs-action`.
    pub status: Option<String>,
}

impl From<&holtmail_pim::Participant> for ParticipantDto {
    fn from(p: &holtmail_pim::Participant) -> Self {
        Self {
            email: p.email.clone(),
            name: p.name.clone(),
            status: p.partstat.as_ref().map(|s| s.to_ascii_lowercase()),
        }
    }
}

/// An event or task a message's calendar part is a version of, as the archive knows it now.
#[derive(Debug, Serialize, ToSchema)]
pub struct InvitationDto {
    pub item_id: String,
    /// The event with its copies in calendars: open it at `/api/events/{group_id}`.
    pub group_id: Option<String>,
    /// iCalendar `UID`.
    pub uid: Option<String>,
    /// The number of this message's version among the item's versions.
    pub version: u32,
    /// This message's iTIP method, lowercase; absent when the part named none.
    pub method: Option<String>,
    /// The event's summary now.
    pub summary: Option<String>,
    pub start: Option<EventTimeDto>,
    pub end: Option<EventTimeDto>,
    pub all_day: bool,
    pub recurring: bool,
    pub location: Option<String>,
    pub organizer: Option<ParticipantDto>,
    /// Lowercase `STATUS` of the event now, such as `confirmed` or `cancelled`.
    pub status: Option<String>,
    pub cancelled: bool,
    /// The answer of the attendee whose address is one of the archive's own (an account's, an
    /// identity's or an own alias), when one matches. An answer given through the RSVP API
    /// shows here once its iMIP reply has left and is archived.
    pub my_status: Option<String>,
    /// How many versions the item has.
    pub version_count: u32,
    /// How many later versions changed the event itself (updates and cancellations, not
    /// answers).
    pub updates_after: u32,
    /// What scheduling made of this message's iTIP part for the event, when it took the
    /// message in: `applied`, `unauthenticated` (an attendee's answer without DKIM or DMARC
    /// pass; "Apply anyway" can merge it), `not_from_attendee`, `no_copy`, `unchanged`, `update`,
    /// `upstream_holds` or `own_organizer`.
    pub schedule_note: Option<String>,
}

fn lower(method: Option<&str>) -> Option<String> {
    method.map(str::to_ascii_lowercase)
}

/// The calendar parts of `message` as events of its archive, one entry per item, by item id.
pub(crate) async fn invitations(
    state: &ApiState,
    message: &Message,
) -> ApiResult<Vec<InvitationDto>> {
    let sources = state.store.message_versions(message.id).await?;
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let mine: Vec<String> = crate::pim_edit::edits(state)
        .own_addresses(message.archive_id)
        .await?
        .iter()
        .filter_map(|a| holtmail_pim::normalize_email(a))
        .collect();
    let notes = state.store.inbox_entries_of(message.id).await?;
    let mut out: Vec<InvitationDto> = Vec::new();
    for source in sources {
        if out.iter().any(|i| i.item_id == source.item_id.to_string()) {
            continue;
        }
        let Some(summary) = state.store.get_item(source.item_id).await? else {
            continue;
        };
        let versions = state.store.item_versions(source.item_id).await?;
        let this = versions.iter().find(|v| v.number == source.number);
        let updates_after = versions
            .iter()
            .filter(|v| v.number > source.number)
            .filter(|v| !Method::of(v.mail.as_ref().and_then(|m| m.method.as_deref())).is_answer())
            .count();
        let view: Normalized = summary
            .index
            .as_ref()
            .and_then(|i| serde_json::from_value(i.data.clone()).ok())
            .unwrap_or(Normalized::Unreadable);
        let main = match &view {
            Normalized::Calendar(calendar) => calendar.main(),
            _ => None,
        };
        let status = main
            .and_then(|e| e.status.as_ref())
            .map(|s| s.to_ascii_lowercase());
        let uid = summary
            .index
            .as_ref()
            .and_then(|i| i.uid.clone())
            .or(summary.item.key.clone());
        out.push(InvitationDto {
            item_id: source.item_id.to_string(),
            group_id: holtmail_calendar::group_of(&summary).map(|g| g.to_string()),
            schedule_note: notes
                .iter()
                .find(|n| uid.as_deref() == Some(n.uid.as_str()))
                .map(|n| n.note.as_str().to_owned()),
            uid,
            version: source.number,
            method: lower(
                this.and_then(|v| v.mail.as_ref())
                    .and_then(|m| m.method.as_deref()),
            ),
            summary: main.and_then(|e| e.summary.clone()),
            start: main.and_then(|e| e.start.as_ref()).map(EventTimeDto::from),
            end: main.and_then(|e| e.end.as_ref()).map(EventTimeDto::from),
            all_day: main.is_some_and(|e| e.all_day),
            recurring: main.is_some_and(|e| e.recurring),
            location: main.and_then(|e| e.location.clone()),
            organizer: main
                .and_then(|e| e.organizer.as_ref())
                .map(ParticipantDto::from),
            cancelled: status.as_deref() == Some("cancelled"),
            status,
            my_status: main.and_then(|e| {
                e.attendees
                    .iter()
                    .find(|a| a.email.as_ref().is_some_and(|m| mine.contains(m)))
                    .and_then(|a| a.partstat.as_ref())
                    .map(|s| s.to_ascii_lowercase())
            }),
            version_count: summary.item.versions,
            updates_after: u32::try_from(updates_after).unwrap_or(u32::MAX),
        });
    }
    Ok(out)
}

#[utoipa::path(
    get,
    path = "/api/items/{id}",
    params(("id" = String, Path, description = "Item id")),
    responses((status = 200, body = ItemDetail), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such item", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_item(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<ItemDetail>> {
    let item_id: ItemId = id(&raw, "item")?;
    let summary = state
        .store
        .get_item(item_id)
        .await?
        .ok_or_else(|| ApiError::not_found("item"))?;
    let (latest, versions, _) = history(&state, item_id, summary.item.kind).await?;
    let mut item = ItemSummaryDto::from(&summary);
    let mut data = serde_json::to_value(&latest).unwrap_or(serde_json::Value::Null);
    let effective = state
        .store
        .effective(&holtmail_store::ChangeTarget::Item { item: item_id })
        .await?;
    if let Some(effective) = effective.filter(|e| e.change.is_some())
        && let Some(collection) = state.store.get_collection(effective.collection_id).await?
    {
        item.local_edit =
            crate::pim_edit::local_edit(&state, &collection, effective.change.as_ref()).await;
        if let Some(served) = summary.index.as_ref().filter(|_| effective.served) {
            data = served.data.clone();
        }
    }
    Ok(Json(ItemDetail {
        item,
        data,
        versions,
        timeline: crate::pim_edit::timeline(&state, item_id).await?,
    }))
}

/// Every version of an item with what changed and the messages it came in, and the item as
/// those versions give it now: merged for an item from mail, the latest otherwise. The messages
/// are every message any version was found in.
pub(crate) async fn history(
    state: &ApiState,
    item_id: ItemId,
    kind: ItemKind,
) -> ApiResult<(Normalized, Vec<ItemVersionDto>, Vec<Message>)> {
    let kind = pim_kind(kind);
    let stored = state.store.item_versions(item_id).await?;
    let mut read: Vec<(Method, Normalized)> = Vec::with_capacity(stored.len());
    for version in &stored {
        let normalized = match state.blobs.get(&version.blob_id).await? {
            Some(bytes) => holtmail_pim::normalize_as(
                holtmail_pim::Format::stored(&version.content_type, kind),
                kind,
                &bytes,
            ),
            None => Normalized::Unreadable,
        };
        let method = version.mail.as_ref().and_then(|m| m.method.as_deref());
        read.push((Method::of(method), normalized));
    }
    let from_mail = stored.iter().any(|v| v.mail.is_some());
    let pairs: Vec<(Method, &Normalized)> = read.iter().map(|(m, n)| (*m, n)).collect();
    let (latest, timeline) = if from_mail {
        (holtmail_pim::merge(&pairs), holtmail_pim::timeline(&pairs))
    } else {
        let mut changes = Vec::with_capacity(read.len());
        let mut previous: Option<&Normalized> = None;
        for (_, normalized) in &read {
            changes.push(
                previous
                    .map(|before| holtmail_pim::diff(before, normalized))
                    .unwrap_or_default(),
            );
            previous = Some(normalized);
        }
        (
            read.last()
                .map(|(_, n)| n.clone())
                .unwrap_or(Normalized::Unreadable),
            changes,
        )
    };
    let sources = state.store.version_messages(item_id).await?;
    let ids: Vec<MessageId> = sources.iter().map(|s| s.message_id).collect();
    let messages = state.store.get_messages(&ids).await?;
    let mut versions = Vec::with_capacity(stored.len());
    for ((version, (_, normalized)), changes) in stored.into_iter().zip(&read).zip(timeline) {
        let mut found: Vec<&Message> = sources
            .iter()
            .filter(|s| s.number == version.number)
            .filter_map(|s| messages.iter().find(|m| m.id == s.message_id))
            .collect();
        found.sort_by_key(|m| (m.date, m.id));
        versions.push(version_dto(item_id, version, normalized, changes, &found));
    }
    Ok((latest, versions, messages))
}

fn version_dto(
    item_id: ItemId,
    version: ItemVersion,
    normalized: &Normalized,
    changes: Vec<holtmail_pim::Change>,
    messages: &[&Message],
) -> ItemVersionDto {
    let mail = version.mail.as_ref();
    ItemVersionDto {
        number: version.number,
        method: lower(mail.and_then(|m| m.method.as_deref())),
        sequence: mail.and_then(|m| m.sequence),
        messages: messages
            .iter()
            .map(|m| SourceMessageDto {
                id: m.id.to_string(),
                subject: m.subject.clone(),
                from: crate::dto::addresses(&m.from),
                date: m.date.as_millis(),
            })
            .collect(),
        seen_at: version.seen_at.as_millis(),
        content_type: version.content_type,
        etag: version.etag,
        readable: !matches!(normalized, Normalized::Unreadable),
        raw_url: format!("/api/items/{item_id}/versions/{}/raw", version.number),
        changes: changes
            .into_iter()
            .map(|c| ChangeDto {
                field: c.field,
                before: c.before,
                after: c.after,
            })
            .collect(),
    }
}

#[utoipa::path(
    get,
    path = "/api/items/{id}/versions/{number}/raw",
    params(("id" = String, Path, description = "Item id"), ("number" = u32, Path, description = "Version number, from 1")),
    responses((status = 200, description = "The version's bytes exactly as received, as a download: text/vcard, text/calendar, or the JSON a Microsoft Graph or JMAP server sent (application/vnd.microsoft.graph.contact+json, application/vnd.microsoft.graph.event+json, application/jscontact+json, application/jscalendar+json)", content_type = "text/calendar"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such item or version", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_version_raw(
    State(state): AppState,
    Path((raw, number)): Path<(String, u32)>,
) -> ApiResult<Response> {
    let item_id: ItemId = id(&raw, "item")?;
    let version = state
        .store
        .item_versions(item_id)
        .await?
        .into_iter()
        .find(|v| v.number == number)
        .ok_or_else(|| ApiError::not_found("version"))?;
    let bytes = state
        .blobs
        .get(&version.blob_id)
        .await?
        .ok_or_else(|| ApiError::not_found("version bytes"))?;
    let extension = match holtmail_pim::Format::from_content_type(&version.content_type) {
        Some(holtmail_pim::Format::VCard) => "vcf",
        Some(holtmail_pim::Format::ICalendar) | None => "ics",
        Some(_) => "json",
    };
    Ok(crate::routes::download(
        bytes.to_vec(),
        &version.content_type,
        &format!("{item_id}-v{number}.{extension}"),
        false,
    ))
}
