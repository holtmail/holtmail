use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use holtmail_core::{ArchiveId, CollectionId, EventGroupId, ItemId, Timestamp};
use holtmail_index::{DocumentKind, SortOrder};
use holtmail_pim::{CalendarObject, Event, Normalized};
use holtmail_store::{CollectionKind, ItemSummary, OccurrenceQuery};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::dto::{AddressDto, ErrorDto};
use crate::pim::{EventTimeDto, ItemVersionDto};
use crate::{ApiError, ApiResult, ApiState};

type AppState = State<Arc<ApiState>>;

/// The longest span one calendar request may cover.
const MAX_SPAN_MS: i64 = 400 * 86_400_000;
/// The most instances one calendar request returns.
const MAX_INSTANCES: usize = 5_000;

fn id<T: std::str::FromStr>(raw: &str, what: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| ApiError::not_found(what))
}

/// A calendar of the archive, for the toggles of the calendar views.
#[derive(Debug, Serialize, ToSchema)]
pub struct CalendarSourceDto {
    pub id: String,
    pub name: String,
    /// The color its source gave it, as written.
    pub color: Option<String>,
    /// The account it is read from; absent for imported files and once the account is gone.
    pub account_name: Option<String>,
    /// Whether it holds the events found in mail.
    pub from_mail: bool,
    /// When it was removed from its source, Unix milliseconds.
    pub removed_at: Option<i64>,
    /// Events not removed.
    pub items: u64,
}

/// One instance of an event in a span, its copies folded together.
#[derive(Debug, Serialize, ToSchema)]
pub struct CalendarOccurrenceDto {
    /// Open the event at `/api/events/{group_id}`.
    pub group_id: String,
    /// The leading copy: the highest `SEQUENCE`, then the latest `DTSTAMP` or `LAST-MODIFIED`.
    pub item_id: String,
    /// The leading copy's calendar.
    pub collection_id: String,
    /// The calendars of every copy, the leading one first.
    pub collection_ids: Vec<String>,
    /// How many copies of this instance the shown calendars hold.
    pub copies: u32,
    /// The original start naming the instance, such as `2025-03-10T08:00:00Z` or `2025-03-10`;
    /// absent for events that do not repeat.
    pub recurrence_id: Option<String>,
    pub title: Option<String>,
    pub location: Option<String>,
    /// Start in UTC, Unix milliseconds. Dates count from midnight UTC and floating times as if
    /// they were UTC.
    pub starts_at: i64,
    /// End in UTC, Unix milliseconds; for dates the midnight after the last day.
    pub ends_at: i64,
    /// The start as a wall-clock time in its own zone, `2025-03-10T09:00:00` or `2025-03-10`.
    pub start_local: String,
    pub end_local: String,
    /// The start's original zone; absent for dates and floating times.
    pub time_zone: Option<String>,
    pub all_day: bool,
    /// Times that read the same in every zone.
    pub floating: bool,
    pub recurring: bool,
    pub cancelled: bool,
    /// Whether this instance was changed apart from its series.
    pub changed: bool,
    /// Whether every copy was removed from its source.
    pub removed: bool,
}

/// The instances of a span and the calendars they come from.
#[derive(Debug, Serialize, ToSchema)]
pub struct CalendarRangeDto {
    /// By start.
    pub occurrences: Vec<CalendarOccurrenceDto>,
    /// Every calendar of the archive, removed ones too.
    pub calendars: Vec<CalendarSourceDto>,
    /// Whether instances were left out because there were too many.
    pub truncated: bool,
}

/// The span and filters of a calendar request.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CalendarParams {
    /// Start of the span, Unix milliseconds.
    pub from: i64,
    /// End of the span, Unix milliseconds; at most 400 days after `from`.
    pub to: i64,
    /// Comma-separated calendar ids to show; all calendars when absent, none when empty.
    pub collections: Option<String>,
    /// Whether events and calendars removed from their source are shown.
    pub show_removed: Option<bool>,
}

fn readable_object(summary: &ItemSummary) -> Option<CalendarObject> {
    holtmail_calendar::object(summary)
}

async fn calendars(state: &ApiState, archive: ArchiveId) -> ApiResult<Vec<CalendarSourceDto>> {
    let accounts = state.store.list_accounts(archive).await?;
    Ok(state
        .store
        .list_collections(archive)
        .await?
        .into_iter()
        .filter(|c| c.collection.kind == CollectionKind::Calendar)
        .map(|summary| {
            let c = summary.collection;
            CalendarSourceDto {
                id: c.id.to_string(),
                account_name: accounts
                    .iter()
                    .find(|a| Some(a.id) == c.account_id)
                    .map(|a| a.name.clone()),
                from_mail: c.remote_id == holtmail_calendar::MAIL_COLLECTION,
                name: c.name,
                color: c.color,
                removed_at: c.removed_at.map(|t| t.as_millis()),
                items: summary.items,
            }
        })
        .collect())
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/calendar",
    params(("id" = String, Path, description = "Archive id"), CalendarParams),
    responses((status = 200, description = "Instances of events overlapping the span with copies across calendars folded together, by start", body = CalendarRangeDto), (status = 400, description = "A span that ends before it starts or is longer than 400 days", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive or calendar id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn calendar(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<CalendarParams>,
) -> ApiResult<Json<CalendarRangeDto>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    if params.to <= params.from || params.to - params.from > MAX_SPAN_MS {
        return Err(ApiError::bad_request(
            "to must be after from, and at most 400 days after it",
        ));
    }
    let collections = params
        .collections
        .as_deref()
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(|c| id::<CollectionId>(c, "calendar"))
                .collect::<ApiResult<Vec<_>>>()
        })
        .transpose()?;
    let query = OccurrenceQuery {
        archive_id: archive,
        from: Timestamp::from_millis(params.from),
        to: Timestamp::from_millis(params.to),
        collections,
        removed: params.show_removed.unwrap_or(false),
    };
    let instances = holtmail_calendar::instances(&*state.store, &query).await?;
    let mut folded = holtmail_calendar::fold(instances);
    let truncated = folded.len() > MAX_INSTANCES;
    folded.truncate(MAX_INSTANCES);
    let mut objects: HashMap<ItemId, Option<(CalendarObject, bool)>> = HashMap::new();
    let mut occurrences = Vec::with_capacity(folded.len());
    for instance in folded {
        let leading = instance.leading();
        if let std::collections::hash_map::Entry::Vacant(slot) = objects.entry(leading.item_id) {
            let summary = state.store.get_item(leading.item_id).await?;
            slot.insert(summary.as_ref().and_then(|s| {
                readable_object(s).map(|o| (o, s.index.as_ref().is_some_and(|i| i.recurring)))
            }));
        }
        let Some((object, recurring)) = objects.get(&leading.item_id).and_then(Option::as_ref)
        else {
            continue;
        };
        let event = object
            .components
            .get(leading.occurrence.component as usize)
            .or_else(|| object.main());
        let main = object.main();
        let o = &leading.occurrence;
        occurrences.push(CalendarOccurrenceDto {
            group_id: leading.group_id.to_string(),
            item_id: leading.item_id.to_string(),
            collection_id: leading.collection_id.to_string(),
            collection_ids: instance
                .copies
                .iter()
                .map(|c| c.collection_id.to_string())
                .collect(),
            copies: u32::try_from(instance.copies.len()).unwrap_or(u32::MAX),
            recurrence_id: o.recurrence_id.clone(),
            title: event
                .and_then(|e| e.summary.clone())
                .or_else(|| main.and_then(|e| e.summary.clone())),
            location: event
                .and_then(|e| e.location.clone())
                .or_else(|| main.and_then(|e| e.location.clone())),
            starts_at: o.starts_at.as_millis(),
            ends_at: o.ends_at.as_millis(),
            start_local: o.start_local.clone(),
            end_local: o.end_local.clone(),
            time_zone: o.time_zone.clone(),
            all_day: o.all_day,
            floating: o.floating,
            recurring: *recurring,
            cancelled: o.cancelled,
            changed: o.changed,
            removed: instance.copies.iter().all(|c| c.removed),
        });
    }
    Ok(Json(CalendarRangeDto {
        occurrences,
        calendars: calendars(&state, archive).await?,
        truncated,
    }))
}

/// A time of the event as written, and in the zone the viewer asked for.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventMomentDto {
    #[serde(flatten)]
    pub time: EventTimeDto,
    /// The wall-clock time in the viewer's zone (`tz`), such as `2025-03-10T09:00:00`; absent for
    /// dates, floating times and without `tz`.
    pub in_zone: Option<String>,
}

/// A weekday of a rule, with its ordinal: `{ ordinal: -1, weekday: "FR" }` is the last Friday.
#[derive(Debug, Serialize, ToSchema)]
pub struct RuleDayDto {
    pub ordinal: Option<i16>,
    /// `MO`, `TU`, `WE`, `TH`, `FR`, `SA` or `SU`.
    pub weekday: String,
}

/// A changed or cancelled instance of a recurring event.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChangedInstanceDto {
    /// The original start it replaces.
    pub recurrence_id: Option<EventTimeDto>,
    pub start: Option<EventTimeDto>,
    pub end: Option<EventTimeDto>,
    pub summary: Option<String>,
    pub cancelled: bool,
}

/// How an event repeats.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecurrenceDto {
    /// The `RRULE` as written; absent when it repeats by `RDATE` alone.
    pub rule: Option<String>,
    /// `YEARLY`, `MONTHLY`, `WEEKLY`, `DAILY`, `HOURLY`, `MINUTELY` or `SECONDLY`; absent when
    /// the rule cannot be read.
    pub frequency: Option<String>,
    pub interval: u32,
    pub count: Option<u32>,
    /// `UNTIL` as written, such as `20250428T070000Z` or `20250428`.
    pub until: Option<String>,
    pub by_day: Vec<RuleDayDto>,
    pub by_month_day: Vec<i32>,
    pub by_month: Vec<u32>,
    pub by_set_pos: Vec<i32>,
    pub by_year_day: Vec<i32>,
    pub by_week_no: Vec<i32>,
    pub by_hour: Vec<u32>,
    pub by_minute: Vec<u32>,
    /// `MO` unless the rule says otherwise.
    pub week_start: String,
    /// Extra dates (`RDATE`).
    pub rdates: Vec<EventTimeDto>,
    /// Left-out dates (`EXDATE`).
    pub exdates: Vec<EventTimeDto>,
    /// Instances changed or cancelled apart from the series.
    pub changed: Vec<ChangedInstanceDto>,
    /// The first start, Unix milliseconds.
    pub first_start: Option<i64>,
    /// The last end, Unix milliseconds; absent when it repeats without end.
    pub last_end: Option<i64>,
}

/// One copy of an event: an item in a calendar.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventCopyDto {
    pub item_id: String,
    pub collection_id: String,
    pub collection_name: String,
    pub color: Option<String>,
    pub account_name: Option<String>,
    /// Whether it is the event as the archived mail describes it.
    pub from_mail: bool,
    /// Whether it is the copy the event is shown from.
    pub leading: bool,
    pub sequence: Option<i64>,
    /// The later of `DTSTAMP` and `LAST-MODIFIED`, Unix milliseconds.
    pub stamp: Option<i64>,
    pub versions: u32,
    /// When the copy, or its calendar, was removed from its source, Unix milliseconds.
    pub removed_at: Option<i64>,
}

/// One copy's values of a field that differs between copies.
#[derive(Debug, Serialize, ToSchema)]
pub struct CopyValuesDto {
    pub item_id: String,
    /// Empty when the copy has no value.
    pub values: Vec<String>,
}

/// A field whose value differs between copies.
#[derive(Debug, Serialize, ToSchema)]
pub struct CopyDifferenceDto {
    /// Such as `start`, `location` or `attendee`; prefixed `instance <RECURRENCE-ID> ` for a
    /// changed instance.
    pub field: String,
    /// In the order of `copies`.
    pub values: Vec<CopyValuesDto>,
}

/// One copy's answer of an attendee.
#[derive(Debug, Serialize, ToSchema)]
pub struct CopyAnswerDto {
    pub item_id: String,
    /// Lowercase `PARTSTAT`; absent when the copy does not list them or has no answer.
    pub status: Option<String>,
}

/// An organizer or attendee, linked to a person when one has their address.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventPersonDto {
    pub email: Option<String>,
    pub name: Option<String>,
    /// Open them at `/api/people/{person_id}`.
    pub person_id: Option<String>,
    /// Their answer in the leading copy, lowercase.
    pub status: Option<String>,
    /// Their answer in each copy, in the order of `copies`; empty for the organizer.
    pub answers: Vec<CopyAnswerDto>,
}

/// A message about the event.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventMessageDto {
    pub id: String,
    pub thread_id: String,
    pub subject: Option<String>,
    pub from: Vec<AddressDto>,
    /// Unix milliseconds.
    pub date: i64,
}

/// A version of one copy, on the event's merged timeline.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventVersionDto {
    pub item_id: String,
    pub collection_name: String,
    /// When it happened: the date of the first message it came in, else when it was first seen.
    /// Unix milliseconds.
    pub at: i64,
    #[serde(flatten)]
    pub version: ItemVersionDto,
}

/// Everything about one event and its copies.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventDetailDto {
    pub group_id: String,
    pub archive_id: String,
    /// The leading copy.
    pub item_id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<EventMomentDto>,
    pub end: Option<EventMomentDto>,
    pub all_day: bool,
    /// The viewer's zone the `in_zone` times are in, when it names a known zone.
    pub viewer_zone: Option<String>,
    /// Lowercase `STATUS` of the leading copy.
    pub status: Option<String>,
    pub cancelled: bool,
    pub recurrence: Option<RecurrenceDto>,
    pub organizer: Option<EventPersonDto>,
    /// Everyone any copy lists, in the order they first appear.
    pub attendees: Vec<EventPersonDto>,
    /// The leading copy first, then by rank.
    pub copies: Vec<EventCopyDto>,
    /// Fields whose values differ between the copies.
    pub differences: Vec<CopyDifferenceDto>,
    /// Messages any copy's versions came in, oldest first.
    pub messages: Vec<EventMessageDto>,
    /// Every version of every copy, oldest first.
    pub timeline: Vec<EventVersionDto>,
    /// The leading copy as normalized: `{"type": "calendar", "components": [...]}`.
    #[schema(value_type = Object)]
    pub data: serde_json::Value,
}

/// The viewer's time zone for an event page.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventParams {
    /// An IANA zone such as `Europe/Paris`; times are also given in it.
    pub tz: Option<String>,
}

fn moment(time: &holtmail_pim::EventTime, zone: Option<&str>) -> EventMomentDto {
    let in_zone = match (time.utc, zone) {
        (Some(utc), Some(zone)) if time.value.contains('T') && time.time_zone.is_some() => {
            holtmail_recur::wall_clock(utc, zone)
        }
        _ => None,
    };
    EventMomentDto {
        time: EventTimeDto::from(time),
        in_zone,
    }
}

fn weekday(day: holtmail_recur::Weekday) -> String {
    holtmail_recur::weekday_code(day).into()
}

fn recurrence(object: &CalendarObject, main: &Event) -> Option<RecurrenceDto> {
    if main.rrule.is_none() && main.rdates.is_empty() {
        return None;
    }
    let rule = main
        .rrule
        .as_deref()
        .and_then(|r| holtmail_recur::Rule::parse(r).ok());
    let span = holtmail_recur::span(object);
    Some(RecurrenceDto {
        rule: main.rrule.clone(),
        frequency: rule.as_ref().map(|r| r.frequency.as_str().to_owned()),
        interval: rule.as_ref().map_or(1, |r| r.interval),
        count: rule.as_ref().and_then(|r| r.count),
        until: rule.as_ref().and_then(|r| r.until.clone()),
        by_day: rule
            .as_ref()
            .map(|r| {
                r.by_day
                    .iter()
                    .map(|d| RuleDayDto {
                        ordinal: d.ordinal,
                        weekday: weekday(d.weekday),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        by_month_day: rule
            .as_ref()
            .map(|r| r.by_month_day.clone())
            .unwrap_or_default(),
        by_month: rule
            .as_ref()
            .map(|r| r.by_month.clone())
            .unwrap_or_default(),
        by_set_pos: rule
            .as_ref()
            .map(|r| r.by_set_pos.clone())
            .unwrap_or_default(),
        by_year_day: rule
            .as_ref()
            .map(|r| r.by_year_day.clone())
            .unwrap_or_default(),
        by_week_no: rule
            .as_ref()
            .map(|r| r.by_week_no.clone())
            .unwrap_or_default(),
        by_hour: rule.as_ref().map(|r| r.by_hour.clone()).unwrap_or_default(),
        by_minute: rule
            .as_ref()
            .map(|r| r.by_minute.clone())
            .unwrap_or_default(),
        week_start: weekday(
            rule.as_ref()
                .map_or(holtmail_recur::Weekday::Mon, |r| r.week_start),
        ),
        rdates: main.rdates.iter().map(EventTimeDto::from).collect(),
        exdates: main.exdates.iter().map(EventTimeDto::from).collect(),
        changed: object
            .components
            .iter()
            .filter(|e| e.recurrence_id.is_some())
            .map(|e| ChangedInstanceDto {
                recurrence_id: e.recurrence_id.as_ref().map(EventTimeDto::from),
                start: e.start.as_ref().map(EventTimeDto::from),
                end: e.end.as_ref().map(EventTimeDto::from),
                summary: e.summary.clone(),
                cancelled: e
                    .status
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED")),
            })
            .collect(),
        first_start: span.map(|s| s.start),
        last_end: span.and_then(|s| s.end),
    })
}

#[utoipa::path(
    get,
    path = "/api/events/{id}",
    params(("id" = String, Path, description = "Event group id"), EventParams),
    responses((status = 200, body = EventDetailDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such event", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_event(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<EventParams>,
) -> ApiResult<Json<EventDetailDto>> {
    let group: EventGroupId = id(&raw, "event")?;
    let mut members = state.store.group_members(group).await?;
    if members.is_empty() {
        return Err(ApiError::not_found("event"));
    }
    let rank = |m: &holtmail_store::GroupMember| {
        (
            m.series.sequence,
            m.series.stamp,
            m.summary.item.removed_at.is_some() || m.collection_removed_at.is_some(),
            m.summary.item.id,
        )
    };
    members.sort_by(|a, b| holtmail_calendar::leads(rank(a), rank(b)));
    let archive = members[0].summary.item.archive_id;
    let accounts = state.store.list_accounts(archive).await?;
    let collections: BTreeMap<CollectionId, holtmail_store::Collection> = state
        .store
        .list_collections(archive)
        .await?
        .into_iter()
        .map(|c| (c.collection.id, c.collection))
        .collect();

    let mut copies = Vec::with_capacity(members.len());
    let mut views: Vec<(ItemId, Normalized)> = Vec::with_capacity(members.len());
    let mut timeline: Vec<EventVersionDto> = Vec::new();
    let mut messages: BTreeMap<String, EventMessageDto> = BTreeMap::new();
    for (at, member) in members.iter().enumerate() {
        let item = &member.summary.item;
        let collection = collections.get(&item.collection_id);
        let collection_name = collection.map(|c| c.name.clone()).unwrap_or_default();
        let (latest, versions, found) = crate::pim::history(&state, item.id, item.kind).await?;
        for message in found {
            messages
                .entry(message.id.to_string())
                .or_insert_with(|| EventMessageDto {
                    id: message.id.to_string(),
                    thread_id: message.thread_id.to_string(),
                    subject: message.subject.clone(),
                    from: crate::dto::addresses(&message.from),
                    date: message.date.as_millis(),
                });
        }
        for version in versions {
            timeline.push(EventVersionDto {
                item_id: item.id.to_string(),
                collection_name: collection_name.clone(),
                at: version.messages.first().map_or(version.seen_at, |m| m.date),
                version,
            });
        }
        views.push((item.id, latest));
        copies.push(EventCopyDto {
            item_id: item.id.to_string(),
            collection_id: item.collection_id.to_string(),
            collection_name,
            color: collection.and_then(|c| c.color.clone()),
            account_name: collection
                .and_then(|c| c.account_id)
                .and_then(|a| accounts.iter().find(|x| x.id == a))
                .map(|a| a.name.clone()),
            from_mail: collection
                .is_some_and(|c| c.remote_id == holtmail_calendar::MAIL_COLLECTION),
            leading: at == 0,
            sequence: member.series.sequence,
            stamp: member.series.stamp.map(|t| t.as_millis()),
            versions: item.versions,
            removed_at: item
                .removed_at
                .or(member.collection_removed_at)
                .map(|t| t.as_millis()),
        });
    }
    timeline.sort_by(|a, b| {
        (a.at, &a.item_id, a.version.number).cmp(&(b.at, &b.item_id, b.version.number))
    });

    let pairs: Vec<(ItemId, &Normalized)> = views.iter().map(|(id, n)| (*id, n)).collect();
    let differences = holtmail_calendar::differences(&pairs)
        .into_iter()
        .map(|d| CopyDifferenceDto {
            field: d.field,
            values: d
                .values
                .into_iter()
                .map(|(item, values)| CopyValuesDto {
                    item_id: item.to_string(),
                    values,
                })
                .collect(),
        })
        .collect();
    let answered = holtmail_calendar::answers(&pairs);

    let leading = &views[0].1;
    let object = match leading {
        Normalized::Calendar(c) => Some(c),
        _ => None,
    };
    let main = object.and_then(|o| o.main());
    let mut emails: Vec<String> = answered.iter().filter_map(|a| a.email.clone()).collect();
    if let Some(email) = main
        .and_then(|m| m.organizer.as_ref())
        .and_then(|o| o.email.clone())
    {
        emails.push(email);
    }
    let people = state.store.people_by_email(archive, &emails).await?;
    let person = |email: Option<&String>| {
        email.and_then(|e| {
            people
                .iter()
                .find(|(x, _)| x.eq_ignore_ascii_case(e))
                .map(|(_, p)| p.to_string())
        })
    };
    let zone = params
        .tz
        .as_deref()
        .map(str::trim)
        .filter(|z| holtmail_recur::wall_clock(0, z).is_some())
        .map(str::to_owned);
    let status = main
        .and_then(|m| m.status.as_ref())
        .map(|s| s.to_ascii_lowercase());
    Ok(Json(EventDetailDto {
        group_id: group.to_string(),
        archive_id: archive.to_string(),
        item_id: views[0].0.to_string(),
        title: main.and_then(|m| m.summary.clone()),
        description: main.and_then(|m| m.description.clone()),
        location: main.and_then(|m| m.location.clone()),
        start: main
            .and_then(|m| m.start.as_ref())
            .map(|t| moment(t, zone.as_deref())),
        end: main
            .and_then(|m| m.end.as_ref())
            .map(|t| moment(t, zone.as_deref())),
        all_day: main.is_some_and(|m| m.all_day),
        viewer_zone: zone.clone(),
        cancelled: status.as_deref() == Some("cancelled"),
        status,
        recurrence: object.zip(main).and_then(|(o, m)| recurrence(o, m)),
        organizer: main
            .and_then(|m| m.organizer.as_ref())
            .map(|o| EventPersonDto {
                email: o.email.clone(),
                name: o.name.clone(),
                person_id: person(o.email.as_ref()),
                status: None,
                answers: Vec::new(),
            }),
        attendees: answered
            .into_iter()
            .map(|a| EventPersonDto {
                person_id: person(a.email.as_ref()),
                status: a.answers.first().and_then(|(_, s)| s.clone()),
                answers: a
                    .answers
                    .into_iter()
                    .map(|(item, status)| CopyAnswerDto {
                        item_id: item.to_string(),
                        status,
                    })
                    .collect(),
                email: a.email,
                name: a.name,
            })
            .collect(),
        copies,
        differences,
        messages: {
            let mut list: Vec<EventMessageDto> = messages.into_values().collect();
            list.sort_by(|a, b| (a.date, &a.id).cmp(&(b.date, &b.id)));
            list
        },
        timeline,
        data: serde_json::to_value(leading).unwrap_or(serde_json::Value::Null),
    }))
}

/// A contact or event a search found.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemHitDto {
    pub item_id: String,
    /// `event` or `contact`.
    pub kind: String,
    pub title: Option<String>,
    /// An event's start, Unix milliseconds.
    pub starts_at: Option<i64>,
    pub all_day: bool,
    pub recurring: bool,
    /// An event's group: open it at `/api/events/{group_id}`.
    pub group_id: Option<String>,
    /// The person a contact belongs to: open them at `/api/people/{person_id}`.
    pub person_id: Option<String>,
    /// A contact's addresses, or an event's organizer and attendees.
    pub emails: Vec<String>,
    /// When it was removed from its source, Unix milliseconds.
    pub removed_at: Option<i64>,
}

/// One page of contacts and events a search found.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemSearchResponse {
    pub items: Vec<ItemHitDto>,
    pub total: u64,
    /// Pass as `offset` for the next page; absent on the last page.
    pub next_offset: Option<u32>,
}

/// A search of contacts and events.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ItemSearchParams {
    /// The search, in the same language as mail search.
    pub q: String,
    pub offset: Option<u32>,
    /// At most 100; 20 by default.
    pub limit: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/search/items",
    params(("id" = String, Path, description = "Archive id"), ItemSearchParams),
    responses((status = 200, description = "Events by start, the latest first, then contacts; copies of one event in several calendars once per page", body = ItemSearchResponse), (status = 400, description = "The search cannot be parsed", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn search_items(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<ItemSearchParams>,
) -> ApiResult<Json<ItemSearchResponse>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let query =
        holtmail_query::parse(&params.q).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let results = state
        .index
        .search_items(archive, &query, SortOrder::Newest, offset, limit)
        .await
        .map_err(|e| match e {
            holtmail_index::IndexError::Invalid(m) => ApiError::bad_request(m),
            other => other.into(),
        })?;
    let mut items: Vec<ItemHitDto> = Vec::with_capacity(results.hits.len());
    let mut folded = 0u64;
    for hit in &results.hits {
        let Some(summary) = state.store.get_item(hit.item_id).await? else {
            continue;
        };
        let group_id = holtmail_calendar::group_of(&summary).map(|g| g.to_string());
        if group_id.is_some() && items.iter().any(|i| i.group_id == group_id) {
            folded += 1;
            continue;
        }
        let index = summary.index.as_ref();
        let emails = index.map(|i| i.emails.clone()).unwrap_or_default();
        let person_id = if hit.kind == DocumentKind::Contact && !emails.is_empty() {
            state
                .store
                .people_by_email(archive, &emails[..1])
                .await?
                .first()
                .map(|(_, p)| p.to_string())
        } else {
            None
        };
        items.push(ItemHitDto {
            item_id: hit.item_id.to_string(),
            kind: match hit.kind {
                DocumentKind::Contact => "contact",
                _ => "event",
            }
            .into(),
            title: index.and_then(|i| i.title.clone()),
            starts_at: index.and_then(|i| i.starts_at).map(|t| t.as_millis()),
            all_day: index.is_some_and(|i| i.all_day),
            recurring: index.is_some_and(|i| i.recurring),
            group_id,
            person_id,
            emails,
            removed_at: summary.item.removed_at.map(|t| t.as_millis()),
        });
    }
    let end = u64::from(offset) + results.hits.len() as u64;
    Ok(Json(ItemSearchResponse {
        items,
        total: results.total.saturating_sub(folded),
        next_offset: (end < results.total).then(|| u32::try_from(end).unwrap_or(u32::MAX)),
    }))
}
