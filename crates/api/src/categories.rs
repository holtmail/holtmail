use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_category::{MessageKind, Topic, TopicSet};
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_store::{
    Account, AccountKind, CategoryFilter, CategoryOverride, CategoryOverrideId, OverrideScope,
    StoreError,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::dto::ErrorDto;
use crate::insights::InsightsConfig;
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// The most trusted authserv-ids an account can have.
pub(crate) const MAX_TRUSTED_IDS: usize = 16;

const MAX_TRUSTED_ID_BYTES: usize = 255;

const MAX_TOPICS: usize = 64;

const SUGGEST_FACTS: u32 = 1000;

const PRESETS: &[(&str, &[&str])] = &[
    ("gmail", &[]),
    ("fastmail", &[]),
    ("outlook", &[]),
    ("icloud", &[]),
    ("yandex", &[]),
];

/// The trusted authserv-ids a new account of `provider` (`gmail`, `fastmail`, `outlook`,
/// `icloud` or `yandex`) starts with; empty for any other provider. Every list stays empty until
/// a VERIFY entry confirms that the provider stamps that id and strips forged fields carrying it
/// (`docs/INSIGHTS.md` §4.3), so today it is empty for every provider.
pub fn trusted_authserv_preset(provider: &str) -> &'static [&'static str] {
    PRESETS
        .iter()
        .find(|(name, _)| *name == provider)
        .map_or(&[], |(_, ids)| ids)
}

fn provider_of(host: &str) -> Option<&'static str> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    Some(
        if under("gmail.com") || under("googlemail.com") || host == "google" {
            "gmail"
        } else if under("fastmail.com") {
            "fastmail"
        } else if under("office365.com") || under("outlook.com") || host == "microsoft" {
            "outlook"
        } else if under("mail.me.com") || under("icloud.com") {
            "icloud"
        } else if under("yandex.ru") || under("yandex.com") {
            "yandex"
        } else {
            return None;
        },
    )
}

/// Gives a new account the preset of its provider when its list is still empty.
pub(crate) async fn seed_trust(state: &ApiState, account: &Account) -> ApiResult<()> {
    let details = state.mailboxes.details(account);
    let host = details
        .oauth_provider
        .or_else(|| details.connection.map(|c| c.host))
        .or_else(|| {
            details
                .jmap
                .and_then(|j| url::Url::parse(&j.url).ok()?.host_str().map(str::to_owned))
        });
    let Some(preset) = host
        .as_deref()
        .and_then(provider_of)
        .map(trusted_authserv_preset)
    else {
        return Ok(());
    };
    if preset.is_empty() {
        return Ok(());
    }
    let lists = state.store.trusted_authserv_ids(account.archive_id).await?;
    if lists.get(&account.id).is_some_and(|ids| !ids.is_empty()) {
        return Ok(());
    }
    let ids: Vec<String> = preset.iter().map(|id| (*id).to_owned()).collect();
    state
        .store
        .set_trusted_authserv_ids(account.id, &ids)
        .await?;
    state.mailboxes.insights_changed(account.archive_id);
    Ok(())
}

/// The ids as stored: trimmed, at most [`MAX_TRUSTED_IDS`], each 1 to 255 bytes of printable
/// ASCII without spaces, duplicates (ignoring case) dropped, order kept.
pub(crate) fn trusted_ids(raw: &[String]) -> ApiResult<Vec<String>> {
    if raw.len() > MAX_TRUSTED_IDS * 16 {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_TRUSTED_IDS} trusted authentication servers"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for id in raw {
        let id = id.trim();
        if id.is_empty()
            || id.len() > MAX_TRUSTED_ID_BYTES
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(ApiError::bad_request(
                "a trusted authentication server id is 1 to 255 printable ASCII characters without spaces",
            ));
        }
        if !out.iter().any(|seen| seen.eq_ignore_ascii_case(id)) {
            out.push(id.to_owned());
        }
    }
    if out.len() > MAX_TRUSTED_IDS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_TRUSTED_IDS} trusted authentication servers"
        )));
    }
    Ok(out)
}

/// The trust fields of an account's DTO: its list and, for a synced account with an empty list,
/// the suggested id. Both `None` for import accounts.
pub(crate) async fn account_trust(
    state: &ApiState,
    account: &Account,
    lists: &BTreeMap<AccountId, Vec<String>>,
) -> ApiResult<(Option<Vec<String>>, Option<String>)> {
    if account.kind == AccountKind::Import {
        return Ok((None, None));
    }
    let ids = lists.get(&account.id).cloned().unwrap_or_default();
    let suggested = if ids.is_empty() {
        holtmail_insights::suggest_authserv(
            &state
                .store
                .recent_auth_facts(account.id, SUGGEST_FACTS)
                .await?,
        )
    } else {
        None
    };
    Ok((Some(ids), suggested))
}

/// A list's `category` value: a kind or a topic name, `health` included.
pub(crate) fn filter(raw: Option<&str>) -> ApiResult<Option<CategoryFilter>> {
    let Some(raw) = raw.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    MessageKind::from_name(raw)
        .map(CategoryFilter::Kind)
        .or_else(|| Topic::from_name(raw).map(CategoryFilter::Topic))
        .map(Some)
        .ok_or_else(|| ApiError::bad_request(format!("unknown category {raw:?}")))
}

/// How many messages have one kind or topic.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryCountDto {
    /// The kind or topic name.
    pub id: String,
    pub count: u64,
}

/// The taxonomy of an archive with message counts, in taxonomy order, zeros included.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoriesDto {
    /// `personal`, `list`, `bulk`, `notification`.
    pub kinds: Vec<CategoryCountDto>,
    /// `purchases`, `travel`, `finance`, `health`, `social`, `jobs`, `events`, `government`;
    /// `health` only when asked for and turned on.
    pub topics: Vec<CategoryCountDto>,
}

/// What the category counts include.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CategoriesParams {
    /// `health` adds the health topic, when the server has it turned on.
    pub include: Option<String>,
}

/// A user's choice of kind, topics or both for every message of a sender.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryOverrideDto {
    pub id: String,
    /// `address` or `domain`.
    pub scope: String,
    /// The address, or the registrable domain, lowercase with an A-label domain.
    pub key: String,
    /// The kind it sets; `null` keeps the derived kind.
    pub kind: Option<String>,
    /// The topics it sets, replacing the derived ones; `null` keeps the derived topics.
    /// Sensitive topics apply only to aligned messages.
    pub topics: Option<Vec<String>>,
    /// Unix milliseconds when it was first made.
    pub created_at: i64,
    /// The user who first made it.
    pub created_by: String,
}

/// An override to store. It replaces the override with the same scope and key.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CategoryOverrideRequest {
    /// `address` or `domain`.
    pub scope: String,
    /// An address, or a domain whose registrable domain is used (an address is accepted there
    /// and its domain taken). Normalised: lowercase, A-label domain, no trailing dot.
    pub key: String,
    /// A kind name; missing or `null` keeps the derived kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Topic names, possibly empty; missing or `null` keeps the derived topics. At least one of
    /// `kind` and `topics` is needed.
    #[serde(default)]
    pub topics: Option<Vec<String>>,
}

fn config(state: &ApiState) -> ApiResult<&InsightsConfig> {
    state
        .insights
        .as_ref()
        .ok_or_else(|| ApiError::not_found("insights"))
}

fn names(topics: TopicSet, health: bool) -> Vec<String> {
    topics
        .iter()
        .filter(|t| health || *t != Topic::Health)
        .map(|t| t.name().to_owned())
        .collect()
}

fn override_dto(value: &CategoryOverride, health: bool) -> CategoryOverrideDto {
    CategoryOverrideDto {
        id: value.id.to_string(),
        scope: value.scope.as_str().to_owned(),
        key: value.key.clone(),
        kind: value.kind.map(|k| k.name().to_owned()),
        topics: value.topics.map(|t| names(t, health)),
        created_at: value.created_at.as_millis(),
        created_by: value.created_by.to_string(),
    }
}

static CHANGES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[utoipa::path(
    get,
    path = "/api/archives/{id}/categories",
    params(("id" = String, Path, description = "Archive id"), CategoriesParams),
    responses(
        (status = 200, body = CategoriesDto),
        (status = 400, description = "An unknown `include` value", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not allowed", body = ErrorDto),
        (status = 404, description = "No such archive, or the server computes no insights", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn categories(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<CategoriesParams>,
) -> ApiResult<Json<CategoriesDto>> {
    let archive = crate::manage::archive(&state, &raw).await?;
    let config = config(&state)?;
    let include = match params.include.as_deref().map(str::trim) {
        None | Some("") => false,
        Some("health") => true,
        Some(_) => return Err(ApiError::bad_request("include is `health`")),
    };
    let counts = state
        .store
        .category_counts(archive, include && config.health)
        .await?;
    let count = |id: &str, count: u64| CategoryCountDto {
        id: id.to_owned(),
        count,
    };
    Ok(Json(CategoriesDto {
        kinds: counts
            .kinds
            .iter()
            .map(|(k, n)| count(k.name(), *n))
            .collect(),
        topics: counts
            .topics
            .iter()
            .filter(|(t, _)| (include && config.health) || *t != Topic::Health)
            .map(|(t, n)| count(t.name(), *n))
            .collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/category-overrides",
    params(("id" = String, Path, description = "Archive id")),
    responses(
        (status = 200, description = "Address overrides first, then by key", body = Vec<CategoryOverrideDto>),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not allowed", body = ErrorDto),
        (status = 404, description = "No such archive, or the server computes no insights", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn list_category_overrides(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<CategoryOverrideDto>>> {
    let archive = crate::manage::archive(&state, &raw).await?;
    let config = config(&state)?;
    Ok(Json(
        state
            .store
            .category_overrides(archive)
            .await?
            .iter()
            .map(|o| override_dto(o, config.health))
            .collect(),
    ))
}

#[utoipa::path(
    put,
    path = "/api/archives/{id}/category-overrides",
    params(("id" = String, Path, description = "Archive id")),
    request_body = CategoryOverrideRequest,
    responses(
        (status = 200, description = "Stored, replacing the override with the same scope and key; the counts and lists follow at once", body = CategoryOverrideDto),
        (status = 400, description = "Nothing to change, an unknown scope, kind or topic, or a key that is not an address or domain", body = ErrorDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not an owner or admin", body = ErrorDto),
        (status = 404, description = "No such archive, or the server computes no insights", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn put_category_override(
    State(state): AppState,
    Path(raw): Path<String>,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Json(request): Json<CategoryOverrideRequest>,
) -> ApiResult<Json<CategoryOverrideDto>> {
    let archive = crate::manage::archive(&state, &raw).await?;
    let config = config(&state)?;
    let scope = OverrideScope::parse(request.scope.trim())
        .ok_or_else(|| ApiError::bad_request("scope is `address` or `domain`"))?;
    let key = holtmail_insights::override_key(scope, &request.key).ok_or_else(|| {
        ApiError::bad_request(match scope {
            OverrideScope::Address => "the key is not an address",
            OverrideScope::Domain => "the key is not a domain",
        })
    })?;
    if request.kind.is_none() && request.topics.is_none() {
        return Err(ApiError::bad_request("nothing to change"));
    }
    let kind = request
        .kind
        .as_deref()
        .map(|name| {
            MessageKind::from_name(name.trim())
                .ok_or_else(|| ApiError::bad_request(format!("unknown kind {name:?}")))
        })
        .transpose()?;
    let topics = request
        .topics
        .as_deref()
        .map(|list| {
            if list.len() > MAX_TOPICS {
                return Err(ApiError::bad_request("too many topics"));
            }
            list.iter()
                .map(|name| match Topic::from_name(name.trim()) {
                    Some(Topic::Health) if !config.health => Err(ApiError::bad_request(
                        "the health topic is turned off on this server",
                    )),
                    Some(topic) => Ok(topic),
                    None => Err(ApiError::bad_request(format!("unknown topic {name:?}"))),
                })
                .collect::<ApiResult<TopicSet>>()
        })
        .transpose()?;
    let _guard = CHANGES.lock().await;
    let existing = state
        .store
        .category_overrides(archive)
        .await?
        .into_iter()
        .find(|o| o.scope == scope && o.key == key);
    let kept_health = !config.health
        && existing
            .as_ref()
            .and_then(|o| o.topics)
            .is_some_and(|t| t.contains(Topic::Health));
    let topics = topics.map(|t| {
        if kept_health {
            t.with(Topic::Health)
        } else {
            t
        }
    });
    let value = CategoryOverride {
        id: existing
            .as_ref()
            .map_or_else(CategoryOverrideId::new, |o| o.id),
        archive_id: archive,
        scope,
        key,
        kind,
        topics,
        created_at: Timestamp::now(),
        created_by: user.id,
    };
    let (stored, _) = state
        .store
        .put_category_override(&value, config.policy())
        .await
        .map_err(|e| match e {
            StoreError::Invalid(m) => ApiError::bad_request(m),
            StoreError::NotFound(what) => ApiError::not_found(what),
            other => other.into(),
        })?;
    state.mailboxes.insights_changed(archive);
    Ok(Json(override_dto(&stored, config.health)))
}

#[utoipa::path(
    delete,
    path = "/api/archives/{id}/category-overrides/{override}",
    params(
        ("id" = String, Path, description = "Archive id"),
        ("override" = String, Path, description = "Override id"),
    ),
    responses(
        (status = 204, description = "Removed; the messages it matched get their derived categories back"),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not an owner or admin", body = ErrorDto),
        (status = 404, description = "No such archive or override in it, or the server computes no insights", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn delete_category_override(
    State(state): AppState,
    Path((raw, raw_override)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let archive: ArchiveId = crate::manage::archive(&state, &raw).await?;
    let config = config(&state)?;
    let id: CategoryOverrideId = crate::manage::id(&raw_override, "override")?;
    let _guard = CHANGES.lock().await;
    state
        .store
        .delete_category_override(archive, id, config.policy())
        .await
        .map_err(|e| match e {
            StoreError::NotFound(_) => ApiError::not_found("override"),
            other => other.into(),
        })?;
    state.mailboxes.insights_changed(archive);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_stay_empty_until_verified() {
        for provider in ["gmail", "fastmail", "outlook", "icloud", "yandex", "other"] {
            assert!(trusted_authserv_preset(provider).is_empty(), "{provider}");
        }
        assert_eq!(provider_of("imap.gmail.com"), Some("gmail"));
        assert_eq!(provider_of("google"), Some("gmail"));
        assert_eq!(provider_of("outlook.office365.com"), Some("outlook"));
        assert_eq!(provider_of("imap.mail.me.com"), Some("icloud"));
        assert_eq!(provider_of("api.fastmail.com"), Some("fastmail"));
        assert_eq!(provider_of("imap.yandex.ru."), Some("yandex"));
        assert_eq!(provider_of("notgmail.com"), None);
        assert_eq!(provider_of("mail.example.com"), None);
    }

    #[test]
    fn checks_trusted_ids() {
        let ids =
            |raw: &[&str]| trusted_ids(&raw.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert_eq!(
            ids(&[" mx.example.org ", "MX.example.org", "mx.example.com"]).ok(),
            Some(vec![
                "mx.example.org".to_owned(),
                "mx.example.com".to_owned()
            ])
        );
        assert_eq!(ids(&[]).ok(), Some(Vec::new()));
        for bad in [
            &["mx example.org"][..],
            &[""],
            &["mx.exämple.org"],
            &["a\u{7f}"],
        ] {
            assert!(ids(bad).is_err(), "{bad:?}");
        }
        assert!(ids(&["a".repeat(255).as_str()]).is_ok());
        assert!(ids(&["a".repeat(256).as_str()]).is_err());
        let many: Vec<String> = (0..17).map(|i| format!("mx{i}.example.org")).collect();
        assert!(trusted_ids(&many).is_err());
        assert!(trusted_ids(&many[..16]).is_ok());
        let repeated: Vec<String> = (0..40).map(|_| "mx.example.org".to_owned()).collect();
        assert_eq!(trusted_ids(&repeated).ok().map(|v| v.len()), Some(1));
    }

    #[test]
    fn reads_category_filters() {
        assert_eq!(filter(None).ok(), Some(None));
        assert_eq!(filter(Some("")).ok(), Some(None));
        assert_eq!(
            filter(Some("bulk")).ok(),
            Some(Some(CategoryFilter::Kind(MessageKind::Bulk)))
        );
        assert_eq!(
            filter(Some("health")).ok(),
            Some(Some(CategoryFilter::Topic(Topic::Health)))
        );
        assert!(filter(Some("newsletter")).is_err());
        assert!(filter(Some("Travel")).is_err());
    }
}
