use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::Json;
use axum::extract::{Path, State};
use holtmail_core::{ArchiveId, MessageId};
use holtmail_insight::{Confidence, Insight, InsightKind, Items, KindTag, KindTags};
use holtmail_store::{InsightOutcome, InsightView, Message, User};
use serde::Serialize;
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::{ApiError, ApiResult, ApiState};

/// What the API needs to serve message insights. `ApiState.insights` is `None` when the server
/// runs no insights runner; messages then carry no `insights` and the status route answers `404`.
#[derive(Debug, Clone)]
pub struct InsightsConfig {
    /// The runner's current `extract_fp`. A stored row with another fingerprint is `outdated`.
    pub fingerprint: String,
    /// The lowest score at which a `Possible` item of a kind is shown. Kinds not listed show
    /// `Likely` and `Certain` items only.
    pub show_possible: HashMap<KindTag, u16>,
    /// The walk's progress, shared with the server that runs it.
    pub progress: Arc<InsightProgress>,
}

impl InsightsConfig {
    /// A config for `fingerprint` that shows no `Possible` items, with a fresh progress handle.
    pub fn new(fingerprint: impl Into<String>) -> Self {
        Self {
            fingerprint: fingerprint.into(),
            show_possible: HashMap::new(),
            progress: Arc::new(InsightProgress::default()),
        }
    }
}

/// Whether insights are being computed, and how many messages were done in the current or last
/// run. Safe to share between threads; readers see each update as a whole value.
#[derive(Debug, Default)]
pub struct InsightProgress {
    running: AtomicBool,
    done: AtomicU64,
}

impl InsightProgress {
    /// Marks a run as started and resets the count.
    pub fn start(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.running.store(true, Ordering::Release);
    }

    /// Adds `messages` to the count of the current run.
    pub fn advance(&self, messages: u64) {
        self.done.fetch_add(messages, Ordering::Relaxed);
    }

    /// Marks the run as finished; the count is kept until the next start.
    pub fn finish(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// Whether a run is going on.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Messages done in the current or last run.
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }
}

/// How current a message's insights are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightStatusDto {
    /// Computed by the current extractors.
    Ready,
    /// Not computed yet.
    Pending,
    /// Computed by older extractors; the items are shown until they are recomputed.
    Outdated,
    /// An extractor failed; the items of the others are shown.
    Partial,
    /// The message could not be parsed.
    Unparsable,
    /// The message bytes could not be read.
    Unreadable,
}

/// Whether the sender is who the message says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[allow(
    dead_code,
    reason = "the schema lists every state; trust is computed from Phase 38"
)]
pub enum TrustStateDto {
    Aligned,
    Relayed,
    Unverified,
    Unknown,
}

/// The sender trust of a message. Always `unknown` for now.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrustDto {
    pub state: TrustStateDto,
    /// The aligned domain, for `aligned`.
    pub domain: Option<String>,
    /// The alias service, for `relayed`.
    pub provider: Option<String>,
}

/// How sure the extractor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceDto {
    Possible,
    Likely,
    Certain,
}

/// One item found in a message, tagged by `type`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InsightDto {
    /// A one-time code to copy.
    Code {
        value: String,
        /// The origin of an origin-bound code line (`@example.com #123456`).
        origin_bound: Option<String>,
        confidence: ConfidenceDto,
        /// Rank among items of the same kind, `0..=1000`.
        score: u16,
        /// Found below an inline-forward marker.
        forwarded: bool,
    },
    /// The unsubscribe options the message declares.
    Unsubscribe {
        https: Option<String>,
        /// A `mailto:` URI.
        mailto: Option<String>,
        http: Option<String>,
        /// RFC 8058 one-click is declared. Whether the server may use it is in
        /// `MessageInsightsDto.unsubscribe.one_click`.
        one_click: bool,
        list_id: Option<String>,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
}

/// Why one-click unsubscribe is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[allow(
    dead_code,
    reason = "the schema lists every reason; one-click is checked from Phase 38"
)]
pub enum OneClickReasonDto {
    NoPostHeader,
    NotHttps,
    MultipleFields,
    NotCovered,
    NotAligned,
    NoTrustedAuthserv,
    /// One-click requests are turned off on this server.
    Disabled,
    RecentlyRequested,
    /// Members cannot unsubscribe.
    Members,
}

/// Whether the server can unsubscribe with one click.
#[derive(Debug, Serialize, ToSchema)]
pub struct OneClickDto {
    pub available: bool,
    /// Why not, when not available.
    pub reason: Option<OneClickReasonDto>,
}

/// How to leave the list a message came from.
#[derive(Debug, Serialize, ToSchema)]
pub struct UnsubscribeDto {
    pub https: Option<String>,
    /// A `mailto:` URI, opened by the reader's own mail program.
    pub mailto: Option<String>,
    pub http: Option<String>,
    pub one_click: OneClickDto,
}

/// What was found in a message.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessageInsightsDto {
    pub status: InsightStatusDto,
    pub trust: TrustDto,
    /// The `From` is one of the archive's own addresses: an account's or an alias's.
    pub own: bool,
    /// `Likely` and `Certain` items, and `Possible` ones the server is set to show.
    pub items: Vec<InsightDto>,
    /// Present when the message declares a way to unsubscribe.
    pub unsubscribe: Option<UnsubscribeDto>,
}

/// Progress of computing insights in an archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct InsightsStatusDto {
    /// Messages with no insights or with insights from older extractors.
    pub pending: u64,
    /// The server is computing insights now.
    pub running: bool,
}

/// Who reads, and what counts as their own mail.
pub(crate) struct Viewer {
    member: bool,
    own: HashSet<String>,
}

impl Viewer {
    pub(crate) async fn new(state: &ApiState, user: &User, archive: ArchiveId) -> ApiResult<Self> {
        let mut own: HashSet<String> = state
            .store
            .list_accounts(archive)
            .await?
            .into_iter()
            .filter_map(|a| a.address)
            .map(|a| a.trim().to_lowercase())
            .collect();
        own.extend(state.store.own_alias_addresses(archive).await?);
        Ok(Self {
            member: !crate::access::manages(user),
            own,
        })
    }

    fn owns(&self, message: &Message) -> bool {
        message
            .from
            .iter()
            .any(|a| self.own.contains(&a.email.trim().to_lowercase()))
    }

    /// The insight kinds a list row shows for `message`.
    pub(crate) fn kinds(&self, message: &Message, bits: u64) -> Vec<String> {
        let hide_code = self.member || (bits != 0 && self.owns(message));
        KindTags::from_bits(bits)
            .iter()
            .filter(|k| !(hide_code && *k == KindTag::Code))
            .map(|k| k.name().to_owned())
            .collect()
    }
}

/// The views of `ids`, read once; empty when the server has no insights runner.
pub(crate) async fn views(
    state: &ApiState,
    ids: &[MessageId],
) -> ApiResult<HashMap<MessageId, InsightView>> {
    if state.insights.is_none() || ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(state.store.insight_views_among(ids).await?)
}

/// The view of one message; `None` when it has none or the server has no insights runner.
pub(crate) async fn view(state: &ApiState, id: MessageId) -> ApiResult<Option<InsightView>> {
    if state.insights.is_none() {
        return Ok(None);
    }
    Ok(state.store.insight_view(id).await?)
}

/// The insights of `message` as `viewer` may see them; `None` without an insights runner.
pub(crate) fn message_insights(
    config: Option<&InsightsConfig>,
    view: Option<&InsightView>,
    message: &Message,
    viewer: &Viewer,
) -> Option<MessageInsightsDto> {
    let config = config?;
    let own = viewer.owns(message);
    let (status, found) = match view {
        None => (InsightStatusDto::Pending, Vec::new()),
        Some(view) => {
            let row = &view.insights;
            let mut status = match row.outcome {
                InsightOutcome::Ok => InsightStatusDto::Ready,
                InsightOutcome::Partial => InsightStatusDto::Partial,
                InsightOutcome::Unparsable => InsightStatusDto::Unparsable,
                InsightOutcome::Unreadable => InsightStatusDto::Unreadable,
            };
            let found = match Items::from_json(&row.items) {
                Ok(read) => {
                    if read.outdated() {
                        status = InsightStatusDto::Outdated;
                    }
                    read.items.items
                }
                Err(_) => {
                    status = InsightStatusDto::Outdated;
                    Vec::new()
                }
            };
            if row.fingerprint != config.fingerprint {
                status = InsightStatusDto::Outdated;
            }
            (status, found)
        }
    };
    let shown: Vec<&Insight> = found
        .iter()
        .filter(|i| visible(config, i))
        .filter(|i| match i.kind {
            InsightKind::Code { .. } => !viewer.member && !own,
            InsightKind::Unsubscribe { .. } => !viewer.member,
            _ => true,
        })
        .collect();
    let unsubscribe = found
        .iter()
        .filter(|i| visible(config, i))
        .find_map(|i| match &i.kind {
            InsightKind::Unsubscribe {
                https,
                mailto,
                http,
                ..
            } => Some(if viewer.member {
                UnsubscribeDto {
                    https: None,
                    mailto: None,
                    http: None,
                    one_click: OneClickDto {
                        available: false,
                        reason: Some(OneClickReasonDto::Members),
                    },
                }
            } else {
                UnsubscribeDto {
                    https: https.as_ref().map(|u| u.as_str().to_owned()),
                    mailto: mailto.clone(),
                    http: http.as_ref().map(|u| u.as_str().to_owned()),
                    one_click: OneClickDto {
                        available: false,
                        reason: Some(OneClickReasonDto::Disabled),
                    },
                }
            }),
            _ => None,
        });
    Some(MessageInsightsDto {
        status,
        trust: TrustDto {
            state: TrustStateDto::Unknown,
            domain: None,
            provider: None,
        },
        own,
        items: shown.into_iter().filter_map(item_dto).collect(),
        unsubscribe,
    })
}

fn visible(config: &InsightsConfig, item: &Insight) -> bool {
    item.confidence >= Confidence::Likely
        || config
            .show_possible
            .get(&item.kind.tag())
            .is_some_and(|min| item.score >= *min)
}

fn confidence(confidence: Confidence) -> ConfidenceDto {
    match confidence {
        Confidence::Possible => ConfidenceDto::Possible,
        Confidence::Likely => ConfidenceDto::Likely,
        Confidence::Certain => ConfidenceDto::Certain,
    }
}

fn item_dto(item: &Insight) -> Option<InsightDto> {
    match &item.kind {
        InsightKind::Code {
            value,
            origin_bound,
        } => Some(InsightDto::Code {
            value: value.clone(),
            origin_bound: origin_bound.clone(),
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } => Some(InsightDto::Unsubscribe {
            https: https.as_ref().map(|u| u.as_str().to_owned()),
            mailto: mailto.clone(),
            http: http.as_ref().map(|u| u.as_str().to_owned()),
            one_click: *one_click,
            list_id: list_id.clone(),
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
        _ => None,
    }
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/insights/status",
    params(("id" = String, Path, description = "Archive id")),
    responses(
        (status = 200, body = InsightsStatusDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not allowed", body = ErrorDto),
        (status = 404, description = "No such archive, or the server computes no insights", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn insights_status(
    State(state): State<Arc<ApiState>>,
    Path(raw): Path<String>,
) -> ApiResult<Json<InsightsStatusDto>> {
    let archive: ArchiveId = raw.parse().map_err(|_| ApiError::not_found("archive"))?;
    state
        .store
        .get_archive(archive)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))?;
    let config = state
        .insights
        .as_ref()
        .ok_or_else(|| ApiError::not_found("insights"))?;
    Ok(Json(InsightsStatusDto {
        pending: state
            .store
            .count_needing_insights(archive, &config.fingerprint)
            .await?,
        running: config.progress.running(),
    }))
}
