use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use holtmail_core::{ArchiveId, CollectionId, ItemId, PersonId};
use holtmail_pim::Normalized;
use holtmail_store::{
    CollectionSummary, ItemKind, ItemSummary, PageCursor, Person, PersonQuery, PersonSource,
    StoreError,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::dto::{AttachmentDto, ErrorDto};
use crate::{ApiError, ApiResult, ApiState};

type AppState = State<Arc<ApiState>>;

const THREADS: u32 = 20;
const ATTACHMENTS: u32 = 12;
const MEETINGS: u32 = 50;

/// A person in a list: contact cards grouped by a shared email address or phone number, or an
/// address from mail alone.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonSummaryDto {
    /// Stable across rebuilds while the group keeps its smallest email address or phone.
    pub id: String,
    pub archive_id: String,
    /// The name most of their cards give, else the name their mail is written with most.
    pub name: Option<String>,
    /// The email address to show first.
    pub email: Option<String>,
    /// The phone number to show first, in E.164 form when known.
    pub phone: Option<String>,
    /// Every email address, lowercased.
    pub emails: Vec<String>,
    /// Every phone number in E.164 form.
    pub phones: Vec<String>,
    /// How many contact cards; 0 for a person from mail alone.
    pub cards: u32,
    /// How many of those were removed from their source.
    pub removed_cards: u32,
    /// The address books of their cards.
    pub collection_ids: Vec<String>,
    /// Messages with one of their addresses, counted per address.
    pub messages: u64,
    /// Messages the archive's owner sent to them.
    pub from_me: u64,
    /// The oldest of those messages, Unix milliseconds.
    pub first_message_at: Option<i64>,
    /// The newest of those messages, Unix milliseconds.
    pub last_message_at: Option<i64>,
    /// Whether their current cards disagree on a field.
    pub differences: bool,
    /// Whether every card was removed and no mail keeps them.
    pub removed: bool,
}

impl From<&Person> for PersonSummaryDto {
    fn from(person: &Person) -> Self {
        let mut collections: Vec<String> = person
            .cards
            .iter()
            .map(|c| c.collection_id.to_string())
            .collect();
        collections.dedup();
        Self {
            id: person.id.to_string(),
            archive_id: person.archive_id.to_string(),
            name: person.name.clone(),
            email: person.email.clone(),
            phone: person.phone.clone(),
            emails: person.emails.clone(),
            phones: person.phones.clone(),
            cards: u32::try_from(person.cards.len()).unwrap_or(u32::MAX),
            removed_cards: u32::try_from(person.cards.iter().filter(|c| c.removed).count())
                .unwrap_or(u32::MAX),
            collection_ids: collections,
            messages: person.mail.map_or(0, |m| m.messages),
            from_me: person.mail.map_or(0, |m| m.from_me),
            first_message_at: person.mail.map(|m| m.first_at.as_millis()),
            last_message_at: person.mail.map(|m| m.last_at.as_millis()),
            differences: person.differences,
            removed: person.removed,
        }
    }
}

/// One page of people.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonPage {
    pub items: Vec<PersonSummaryDto>,
    /// Pass as `after` for the next page; absent on the last page.
    pub next: Option<String>,
    /// How many people match the filters, on every page.
    pub total: u64,
}

/// Search, filters and paging of the people list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PeopleParams {
    /// Only people whose name, an email address or a phone number contains this text, without
    /// regard to letter case.
    pub q: Option<String>,
    /// `contacts` (with a card), `mail` (from mail alone) or an address book's id.
    pub source: Option<String>,
    /// `true` for only people whose cards disagree.
    pub has_differences: Option<bool>,
    /// `next` from the previous page.
    pub cursor: Option<String>,
    /// Page size, at most 500; 100 by default.
    pub limit: Option<u32>,
}

/// Where a value of a person's field comes from.
#[derive(Debug, Serialize, ToSchema)]
pub struct ValueSourceDto {
    /// The contact card.
    pub item_id: String,
    pub collection_id: String,
    pub collection_name: String,
    /// The account the address book was read from; absent for imported files and removed
    /// accounts.
    pub account_name: Option<String>,
    /// Whether the card was removed from its source.
    pub removed: bool,
    /// When the card was last known to say this, Unix milliseconds: when it was removed, else
    /// when its address book last synced, else when its latest version was seen.
    pub seen_at: i64,
}

/// One value of a person's field.
#[derive(Debug, Serialize, ToSchema)]
pub struct FieldValueDto {
    /// As the first card that says it writes it.
    pub value: String,
    /// The cards that say it.
    pub sources: Vec<ValueSourceDto>,
}

/// A field of a person merged from their cards.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonFieldDto {
    /// `name`, `nickname`, `email`, `phone`, `organization`, `title`, `birthday`, `address` or
    /// `note`.
    pub name: String,
    pub values: Vec<FieldValueDto>,
    /// Whether their current cards disagree on it.
    pub differs: bool,
}

/// One of a person's contact cards.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonCardDto {
    /// Open its history at `/api/items/{item_id}`.
    pub item_id: String,
    pub collection_id: String,
    pub collection_name: String,
    pub account_name: Option<String>,
    /// The card's name.
    pub title: Option<String>,
    /// Whether its latest version could be read.
    pub readable: bool,
    /// How many versions are archived.
    pub versions: u32,
    /// Unix milliseconds.
    pub first_seen_at: i64,
    /// When it was removed from its source, Unix milliseconds.
    pub removed_at: Option<i64>,
    /// When it was last known as it is, Unix milliseconds.
    pub seen_at: i64,
}

/// Why two cards are the same person.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonLinkDto {
    pub from_item_id: String,
    pub to_item_id: String,
    /// `email`, `phone`, or `hand` for a join made by hand.
    pub kind: String,
    /// The shared address, the phone number in E.164 form, or the join's id.
    pub value: String,
}

/// Messages of a year.
#[derive(Debug, Serialize, ToSchema)]
pub struct YearCountDto {
    pub year: i32,
    pub messages: u64,
}

/// A conversation with a person.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonThreadDto {
    pub thread_id: String,
    /// The newest message of theirs in it; open it in the reader.
    pub message_id: String,
    pub subject: Option<String>,
    /// Messages of theirs in it.
    pub messages: u64,
    /// The newest one's date, Unix milliseconds.
    pub last_at: i64,
}

/// A file exchanged with a person.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonAttachmentDto {
    pub message_id: String,
    /// The message's date, Unix milliseconds.
    pub date: i64,
    /// Whether they sent it.
    pub from_them: bool,
    pub attachment: AttachmentDto,
}

/// What the mail says about a person: every message with one of their addresses, each once.
#[derive(Debug, Serialize, ToSchema)]
pub struct CorrespondenceDto {
    pub messages: u64,
    /// Sent by them.
    pub from_them: u64,
    /// With them among the recipients.
    pub to_them: u64,
    /// Unix milliseconds.
    pub first_at: Option<i64>,
    /// Unix milliseconds.
    pub last_at: Option<i64>,
    /// Oldest year first.
    pub years: Vec<YearCountDto>,
    /// The most recently active conversations first, at most 20.
    pub threads: Vec<PersonThreadDto>,
    /// The newest first, at most 12; calendar parts are left out.
    pub attachments: Vec<PersonAttachmentDto>,
}

/// An event a person organizes or attends.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingDto {
    /// Open it at `/api/items/{item_id}`.
    pub item_id: String,
    /// The event with its copies: open it at `/api/events/{group_id}`.
    pub group_id: Option<String>,
    pub title: Option<String>,
    /// Start in UTC, Unix milliseconds.
    pub starts_at: Option<i64>,
    /// End in UTC, Unix milliseconds.
    pub ends_at: Option<i64>,
    pub all_day: bool,
    pub recurring: bool,
    /// `organizer` or `attendee`.
    pub role: String,
    /// Their lowercase `PARTSTAT` as an attendee, such as `accepted`.
    pub status: Option<String>,
    /// Whether the event is cancelled.
    pub cancelled: bool,
    /// When it was removed from its calendar, Unix milliseconds.
    pub removed_at: Option<i64>,
}

/// Everything about a person.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonDetailDto {
    pub person: PersonSummaryDto,
    /// Their fields merged from every card, with where each value comes from.
    pub fields: Vec<PersonFieldDto>,
    /// Their cards, the current ones first.
    pub cards: Vec<PersonCardDto>,
    /// The shared addresses and phone numbers that made the cards one person.
    pub links: Vec<PersonLinkDto>,
    pub correspondence: CorrespondenceDto,
    /// Events whose organizer or attendees include them, the latest start first, at most 50;
    /// copies of one event in several calendars once.
    pub meetings: Vec<MeetingDto>,
    /// Splits and joins made by hand that touch one of their cards or addresses, oldest first.
    pub overrides: Vec<crate::overrides::PersonOverrideDto>,
}

/// An address that belongs to a person.
#[derive(Debug, Serialize, ToSchema)]
pub struct AddressPersonDto {
    /// The address as the message writes it.
    pub email: String,
    pub person_id: String,
}

/// The people of `emails` in the archive, each address once.
pub(crate) async fn address_people(
    state: &ApiState,
    archive: ArchiveId,
    emails: impl Iterator<Item = String>,
) -> ApiResult<Vec<AddressPersonDto>> {
    let mut unique: Vec<String> = Vec::new();
    for email in emails {
        if !unique.iter().any(|e| e.eq_ignore_ascii_case(&email)) {
            unique.push(email);
        }
    }
    Ok(state
        .store
        .people_by_email(archive, &unique)
        .await?
        .into_iter()
        .map(|(email, id)| AddressPersonDto {
            email,
            person_id: id.to_string(),
        })
        .collect())
}

fn id<T: std::str::FromStr>(raw: &str, what: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| ApiError::not_found(what))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/people",
    params(("id" = String, Path, description = "Archive id"), PeopleParams),
    responses((status = 200, description = "People by name without regard to letter case, nameless ones last", body = PersonPage), (status = 400, description = "A malformed cursor or source", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_people(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<PeopleParams>,
) -> ApiResult<Json<PersonPage>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let source = match params.source.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("contacts") => Some(PersonSource::Contacts),
        Some("mail") => Some(PersonSource::Mail),
        Some(other) => Some(PersonSource::Collection(
            other.parse::<CollectionId>().map_err(|_| {
                ApiError::bad_request("source is contacts, mail or an address book id")
            })?,
        )),
    };
    let mut query = PersonQuery {
        archive_id: archive,
        text: params.q.filter(|q| !q.trim().is_empty()),
        source,
        differences: params.has_differences.unwrap_or(false),
        after: None,
        limit: params.limit.unwrap_or(100),
    };
    let total = state.store.count_people(&query).await?;
    query.after = params.cursor.map(PageCursor);
    let page = state
        .store
        .list_people(&query)
        .await
        .map_err(|error| match error {
            StoreError::Invalid(message) => ApiError::bad_request(message),
            other => other.into(),
        })?;
    Ok(Json(PersonPage {
        items: page.items.iter().map(PersonSummaryDto::from).collect(),
        next: page.next.map(|c| c.0),
        total,
    }))
}

struct Sources {
    collections: Vec<CollectionSummary>,
    accounts: HashMap<String, String>,
}

impl Sources {
    fn collection(&self, id: CollectionId) -> Option<&CollectionSummary> {
        self.collections.iter().find(|c| c.collection.id == id)
    }

    fn names(&self, id: CollectionId) -> (String, Option<String>) {
        match self.collection(id) {
            Some(c) => (
                c.collection.name.clone(),
                c.collection
                    .account_id
                    .and_then(|a| self.accounts.get(&a.to_string()).cloned()),
            ),
            None => (String::new(), None),
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/people/{id}",
    params(("id" = String, Path, description = "Person id")),
    responses((status = 200, body = PersonDetailDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such person", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_person(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<PersonDetailDto>> {
    let person_id: PersonId = id(&raw, "person")?;
    let person = state
        .store
        .get_person(person_id)
        .await?
        .ok_or_else(|| ApiError::not_found("person"))?;
    let archive = person.archive_id;
    let sources = Sources {
        collections: state.store.list_collections(archive).await?,
        accounts: state
            .store
            .list_accounts(archive)
            .await?
            .into_iter()
            .map(|a| (a.id.to_string(), a.name))
            .collect(),
    };

    let mut summaries: Vec<(ItemSummary, i64)> = Vec::new();
    for card in &person.cards {
        let Some(summary) = state.store.get_item(card.item_id).await? else {
            continue;
        };
        let latest_seen = state
            .store
            .item_versions(card.item_id)
            .await?
            .last()
            .map_or(summary.item.first_seen_at, |v| v.seen_at);
        let synced = sources
            .collection(summary.item.collection_id)
            .and_then(|c| c.collection.synced_at);
        let seen = summary
            .item
            .removed_at
            .or(synced.map(|s| s.max(latest_seen)))
            .unwrap_or(latest_seen);
        summaries.push((summary, seen.as_millis()));
    }
    summaries.sort_by_key(|(s, seen)| (s.item.removed_at.is_some(), -seen, s.item.id));

    let cards: Vec<holtmail_people::Card> = summaries
        .iter()
        .map(|(s, _)| holtmail_people::card(s))
        .collect();
    let source_cards: Vec<holtmail_people::SourceCard<'_>> = cards
        .iter()
        .filter_map(|c| {
            c.contact
                .as_ref()
                .map(|contact| holtmail_people::SourceCard {
                    item_id: c.item_id,
                    removed: c.removed,
                    contact,
                })
        })
        .collect();
    let value_source = |item: ItemId| -> Option<ValueSourceDto> {
        let (summary, seen) = summaries.iter().find(|(s, _)| s.item.id == item)?;
        let (collection_name, account_name) = sources.names(summary.item.collection_id);
        Some(ValueSourceDto {
            item_id: item.to_string(),
            collection_id: summary.item.collection_id.to_string(),
            collection_name,
            account_name,
            removed: summary.item.removed_at.is_some(),
            seen_at: *seen,
        })
    };
    let fields = holtmail_people::fields(&source_cards)
        .into_iter()
        .map(|field| PersonFieldDto {
            name: field.name,
            differs: field.differs,
            values: field
                .values
                .into_iter()
                .map(|value| FieldValueDto {
                    value: value.value,
                    sources: value
                        .sources
                        .into_iter()
                        .filter_map(&value_source)
                        .collect(),
                })
                .collect(),
        })
        .collect();
    let card_dtos = summaries
        .iter()
        .map(|(summary, seen)| {
            let (collection_name, account_name) = sources.names(summary.item.collection_id);
            PersonCardDto {
                item_id: summary.item.id.to_string(),
                collection_id: summary.item.collection_id.to_string(),
                collection_name,
                account_name,
                title: summary.index.as_ref().and_then(|i| i.title.clone()),
                readable: summary.index.as_ref().is_some_and(|i| i.readable),
                versions: summary.item.versions,
                first_seen_at: summary.item.first_seen_at.as_millis(),
                removed_at: summary.item.removed_at.map(|t| t.as_millis()),
                seen_at: *seen,
            }
        })
        .collect();
    let links = person
        .links
        .iter()
        .map(|link| {
            let (kind, value) = match link.key.split_once(':') {
                Some(("tel", value)) => ("phone", value),
                Some(("hand", value)) => ("hand", value),
                Some((_, value)) => ("email", value),
                None => ("email", link.key.as_str()),
            };
            PersonLinkDto {
                from_item_id: link.from.to_string(),
                to_item_id: link.to.to_string(),
                kind: kind.into(),
                value: value.into(),
            }
        })
        .collect();

    let correspondence = correspondence(&state, &person).await?;
    let meetings = meetings(&state, &person).await?;
    let overrides = crate::overrides::touching(&state, &person).await?;
    Ok(Json(PersonDetailDto {
        person: PersonSummaryDto::from(&person),
        fields,
        cards: card_dtos,
        links,
        correspondence,
        meetings,
        overrides,
    }))
}

async fn correspondence(state: &ApiState, person: &Person) -> ApiResult<CorrespondenceDto> {
    let found = state
        .store
        .correspondence(person.archive_id, &person.mail_emails, THREADS, ATTACHMENTS)
        .await?;
    let latest: Vec<_> = found.threads.iter().map(|t| t.latest).collect();
    let messages = state.store.get_messages(&latest).await?;
    Ok(CorrespondenceDto {
        messages: found.messages,
        from_them: found.from_them,
        to_them: found.to_them,
        first_at: found.first_at.map(|t| t.as_millis()),
        last_at: found.last_at.map(|t| t.as_millis()),
        years: found
            .years
            .iter()
            .map(|y| YearCountDto {
                year: y.year,
                messages: y.messages,
            })
            .collect(),
        threads: found
            .threads
            .iter()
            .map(|t| PersonThreadDto {
                thread_id: t.thread_id.to_string(),
                message_id: t.latest.to_string(),
                subject: messages
                    .iter()
                    .find(|m| m.id == t.latest)
                    .and_then(|m| m.subject.clone()),
                messages: t.messages,
                last_at: t.last_at.as_millis(),
            })
            .collect(),
        attachments: found
            .attachments
            .iter()
            .map(|a| PersonAttachmentDto {
                message_id: a.message_id.to_string(),
                date: a.date.as_millis(),
                from_them: a.from_them,
                attachment: crate::dto::attachment_of(a.message_id, &a.attachment),
            })
            .collect(),
    })
}

async fn meetings(state: &ApiState, person: &Person) -> ApiResult<Vec<MeetingDto>> {
    let items = state
        .store
        .items_by_email(
            person.archive_id,
            ItemKind::Event,
            &person.mail_emails,
            MEETINGS,
        )
        .await?;
    let theirs = |email: Option<&String>| {
        email.is_some_and(|e| person.mail_emails.iter().any(|m| m.eq_ignore_ascii_case(e)))
    };
    Ok(items
        .iter()
        .map(|summary| {
            let index = summary.index.as_ref();
            let view: Option<Normalized> =
                index.and_then(|i| serde_json::from_value(i.data.clone()).ok());
            let main = match &view {
                Some(Normalized::Calendar(calendar)) => calendar.main(),
                _ => None,
            };
            let organizes = main
                .and_then(|e| e.organizer.as_ref())
                .is_some_and(|o| theirs(o.email.as_ref()));
            let status = main.and_then(|e| {
                e.attendees
                    .iter()
                    .find(|a| theirs(a.email.as_ref()))
                    .and_then(|a| a.partstat.as_ref())
                    .map(|s| s.to_ascii_lowercase())
            });
            MeetingDto {
                item_id: summary.item.id.to_string(),
                group_id: holtmail_calendar::group_of(summary).map(|g| g.to_string()),
                title: index.and_then(|i| i.title.clone()),
                starts_at: index.and_then(|i| i.starts_at).map(|t| t.as_millis()),
                ends_at: index.and_then(|i| i.ends_at).map(|t| t.as_millis()),
                all_day: index.is_some_and(|i| i.all_day),
                recurring: index.is_some_and(|i| i.recurring),
                role: if organizes { "organizer" } else { "attendee" }.into(),
                status,
                cancelled: main
                    .and_then(|e| e.status.as_deref())
                    .is_some_and(|s| s.eq_ignore_ascii_case("cancelled")),
                removed_at: summary.item.removed_at.map(|t| t.as_millis()),
            }
        })
        .fold(Vec::new(), |mut out: Vec<MeetingDto>, meeting| {
            if meeting.group_id.is_none() || !out.iter().any(|m| m.group_id == meeting.group_id) {
                out.push(meeting);
            }
            out
        }))
}
