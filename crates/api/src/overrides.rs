use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_core::{ArchiveId, ItemId, PersonId, PersonOverrideId, Timestamp};
use holtmail_store::{Person, PersonOverride, PersonOverrideKind, PersonQuery};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// A split or join of people made by hand. It names cards and addresses, never person ids, so
/// it holds through every rebuild of the people.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonOverrideDto {
    pub id: String,
    /// `join` or `split`.
    pub kind: String,
    /// A join: what is one person, as `item:<item id>` for a card or `email:<address>` for a
    /// person from mail. A split: the cards moved out, as `item:<item id>`.
    pub keys: Vec<String>,
    /// A split: the cards they were separated from. Empty for a join.
    pub from: Vec<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// The user who made it.
    pub created_by: Option<String>,
    /// The people that hold its keys now, each once: for a join in the order of its keys, for a
    /// split the people of `from` first.
    pub people: Vec<String>,
}

/// Cards to move out of a person into a person of their own.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SplitRequest {
    /// Item ids of some of the person's cards, not all of them.
    pub cards: Vec<String>,
}

/// People to make one.
#[derive(Debug, Deserialize, ToSchema)]
pub struct JoinRequest {
    /// Two or more person ids of one archive.
    pub people: Vec<String>,
}

fn holders(people: &[Person], value: &PersonOverride) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let keys: Vec<&String> = match value.kind {
        PersonOverrideKind::Join => value.keys.iter().collect(),
        PersonOverrideKind::Split => value.from.iter().chain(&value.keys).collect(),
    };
    for key in keys {
        if let Some(person) = people.iter().find(|p| holtmail_people::holds(p, key)) {
            let id = person.id.to_string();
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

pub(crate) fn override_dto(value: &PersonOverride, people: &[Person]) -> PersonOverrideDto {
    PersonOverrideDto {
        id: value.id.to_string(),
        kind: value.kind.as_str().into(),
        keys: value.keys.clone(),
        from: value.from.clone(),
        created_at: value.created_at.as_millis(),
        created_by: value.created_by.map(|u| u.to_string()),
        people: holders(people, value),
    }
}

/// Every person of the archive.
pub(crate) async fn all_people(state: &ApiState, archive: ArchiveId) -> ApiResult<Vec<Person>> {
    let mut out = Vec::new();
    let mut query = PersonQuery {
        archive_id: archive,
        text: None,
        source: None,
        differences: false,
        after: None,
        limit: 500,
    };
    loop {
        let page = state.store.list_people(&query).await?;
        out.extend(page.items);
        match page.next {
            Some(next) => query.after = Some(next),
            None => return Ok(out),
        }
    }
}

/// The splits and joins that touch the person: one of its keys or `from` keys is the person's.
pub(crate) async fn touching(
    state: &ApiState,
    person: &Person,
) -> ApiResult<Vec<PersonOverrideDto>> {
    let overrides: Vec<PersonOverride> = state
        .store
        .person_overrides(person.archive_id)
        .await?
        .into_iter()
        .filter(|o| {
            o.keys
                .iter()
                .chain(&o.from)
                .any(|k| holtmail_people::holds(person, k))
        })
        .collect();
    if overrides.is_empty() {
        return Ok(Vec::new());
    }
    let people = all_people(state, person.archive_id).await?;
    Ok(overrides.iter().map(|o| override_dto(o, &people)).collect())
}

async fn person(state: &ApiState, raw: &str) -> ApiResult<Person> {
    let id: PersonId = raw.parse().map_err(|_| ApiError::not_found("person"))?;
    state
        .store
        .get_person(id)
        .await?
        .ok_or_else(|| ApiError::not_found("person"))
}

static CHANGES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn rebuilt(state: &ApiState, archive: ArchiveId) -> ApiResult<Vec<Person>> {
    state
        .mailboxes
        .rebuild_people(archive)
        .await
        .map_err(crate::manage::refused)?;
    all_people(state, archive).await
}

async fn apply(
    state: &ApiState,
    value: PersonOverride,
    check: impl Fn(&[Person]) -> bool,
) -> ApiResult<(StatusCode, Json<PersonOverrideDto>)> {
    let archive = value.archive_id;
    let _one_at_a_time = CHANGES.lock().await;
    let mut overrides = state.store.person_overrides(archive).await?;
    overrides.push(value.clone());
    let preview = holtmail_people::preview(&*state.store, archive, &overrides).await?;
    if !check(&preview) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "a split made by hand keeps these people apart; undo it first".into(),
        ));
    }
    state.store.add_person_override(&value).await?;
    let people = rebuilt(state, archive).await?;
    tracing::info!(%archive, kind = value.kind.as_str(), people = people.len(), "people changed by hand");
    Ok((StatusCode::CREATED, Json(override_dto(&value, &people))))
}

#[utoipa::path(
    post,
    path = "/api/people/{id}/split",
    params(("id" = String, Path, description = "Person id")),
    request_body = SplitRequest,
    responses((status = 201, description = "The split; `people` is the person left with the other cards, then the person of the moved cards", body = PersonOverrideDto), (status = 400, description = "No cards, every card, or a card of another person", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such person", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn split(
    State(state): AppState,
    Path(raw): Path<String>,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Json(request): Json<SplitRequest>,
) -> ApiResult<(StatusCode, Json<PersonOverrideDto>)> {
    let person = person(&state, &raw).await?;
    let mut moved: Vec<ItemId> = Vec::new();
    for raw in &request.cards {
        let id: ItemId = raw
            .parse()
            .map_err(|_| ApiError::bad_request("cards are item ids"))?;
        if !person.cards.iter().any(|c| c.item_id == id) {
            return Err(ApiError::bad_request("a card is not one of this person's"));
        }
        if !moved.contains(&id) {
            moved.push(id);
        }
    }
    moved.sort();
    let rest: Vec<ItemId> = person
        .cards
        .iter()
        .map(|c| c.item_id)
        .filter(|id| !moved.contains(id))
        .collect();
    if moved.is_empty() || rest.is_empty() {
        return Err(ApiError::bad_request(
            "choose some of the person's cards, not none or all",
        ));
    }
    let value = PersonOverride {
        id: PersonOverrideId::new(),
        archive_id: person.archive_id,
        kind: PersonOverrideKind::Split,
        keys: moved.iter().map(|id| format!("item:{id}")).collect(),
        from: rest.iter().map(|id| format!("item:{id}")).collect(),
        created_at: Timestamp::now(),
        created_by: Some(user.id),
    };
    apply(&state, value, |_| true).await
}

#[utoipa::path(
    post,
    path = "/api/people/join",
    request_body = JoinRequest,
    responses((status = 201, description = "The join; `people` is the one person they are now", body = PersonOverrideDto), (status = 400, description = "Fewer than two people, or people of different archives", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such person", body = ErrorDto), (status = 409, description = "A split made by hand keeps them apart", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn join(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Json(request): Json<JoinRequest>,
) -> ApiResult<(StatusCode, Json<PersonOverrideDto>)> {
    let mut people: Vec<Person> = Vec::new();
    for raw in &request.people {
        let found = person(&state, raw).await?;
        if !people.iter().any(|p| p.id == found.id) {
            people.push(found);
        }
    }
    let Some(archive) = people.first().map(|p| p.archive_id) else {
        return Err(ApiError::bad_request("give two people"));
    };
    if people.len() < 2 {
        return Err(ApiError::bad_request("give two different people"));
    }
    if people.iter().any(|p| p.archive_id != archive) {
        return Err(ApiError::bad_request(
            "the people are in different archives",
        ));
    }
    let keys: Vec<String> = people
        .iter()
        .map(holtmail_people::anchor)
        .collect::<Option<_>>()
        .ok_or_else(|| ApiError::bad_request("a person has neither a card nor an address"))?;
    let value = PersonOverride {
        id: PersonOverrideId::new(),
        archive_id: archive,
        kind: PersonOverrideKind::Join,
        keys: keys.clone(),
        from: Vec::new(),
        created_at: Timestamp::now(),
        created_by: Some(user.id),
    };
    apply(&state, value, |result| {
        result
            .iter()
            .any(|p| keys.iter().all(|k| holtmail_people::holds(p, k)))
    })
    .await
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/people/overrides",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "The archive's splits and joins, oldest first", body = Vec<PersonOverrideDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_overrides(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<PersonOverrideDto>>> {
    let archive: ArchiveId = raw.parse().map_err(|_| ApiError::not_found("archive"))?;
    let overrides = state.store.person_overrides(archive).await?;
    if overrides.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let people = all_people(&state, archive).await?;
    Ok(Json(
        overrides.iter().map(|o| override_dto(o, &people)).collect(),
    ))
}

#[utoipa::path(
    delete,
    path = "/api/people/overrides/{id}",
    params(("id" = String, Path, description = "Override id")),
    responses((status = 200, description = "The undone split or join; `people` holds its keys after the rebuild", body = PersonOverrideDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such override", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn remove_override(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<PersonOverrideDto>> {
    let id: PersonOverrideId = raw.parse().map_err(|_| ApiError::not_found("override"))?;
    let value = state
        .store
        .get_person_override(id)
        .await?
        .ok_or_else(|| ApiError::not_found("override"))?;
    let archive = value.archive_id;
    let _one_at_a_time = CHANGES.lock().await;
    if !state.store.remove_person_override(id).await? {
        return Err(ApiError::not_found("override"));
    }
    let people = rebuilt(&state, archive).await?;
    tracing::info!(%archive, kind = value.kind.as_str(), people = people.len(), "people change undone");
    Ok(Json(override_dto(&value, &people)))
}
