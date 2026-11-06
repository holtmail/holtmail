use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use holtmail_store::{LedgerEntry, LedgerHash};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::{ErrorDto, TaskDto};
use crate::manage::refused;
use crate::{ApiError, ApiResult, ApiState, LedgerVerification, VerifyLedger};

type AppState = State<Arc<ApiState>>;

/// One ledger entry.
#[derive(Debug, Serialize, ToSchema)]
pub struct LedgerEntryDto {
    /// Its position, from 1.
    pub seq: u64,
    /// `archived`, `deleted` or `archive_deleted`.
    pub event: String,
    /// When it happened, Unix milliseconds.
    pub at: i64,
    pub archive_id: String,
    pub message_id: Option<String>,
    /// The message's BLAKE3 hash.
    pub blob_id: Option<String>,
    /// BLAKE3 over the entry and the hash before it, 64 hex digits.
    pub hash: String,
}

impl From<LedgerEntry> for LedgerEntryDto {
    fn from(entry: LedgerEntry) -> Self {
        Self {
            seq: entry.seq,
            event: entry.event.as_str().into(),
            at: entry.at.as_millis(),
            archive_id: entry.archive_id.to_string(),
            message_id: entry.message_id.map(|m| m.to_string()),
            blob_id: entry.blob_id.map(|b| b.to_string()),
            hash: entry.hash.to_string(),
        }
    }
}

/// How a verification ended.
#[derive(Debug, Serialize, ToSchema)]
pub struct LedgerVerificationDto {
    /// Unix milliseconds.
    pub started_at: i64,
    /// Unix milliseconds.
    pub finished_at: i64,
    pub ok: bool,
    pub entries: u64,
    pub head_seq: Option<u64>,
    pub head_hash: Option<String>,
    /// The position of the anchor it was given.
    pub anchor_seq: Option<u64>,
    /// Message bytes read, when it was asked to read them.
    pub blobs_checked: Option<u64>,
    /// At most 100 lines with ids and positions.
    pub problems: Vec<String>,
    pub problem_count: u64,
}

impl From<LedgerVerification> for LedgerVerificationDto {
    fn from(v: LedgerVerification) -> Self {
        Self {
            started_at: v.started_at.as_millis(),
            finished_at: v.finished_at.as_millis(),
            ok: v.ok,
            entries: v.entries,
            head_seq: v.head_seq,
            head_hash: v.head_hash.map(|h| h.to_string()),
            anchor_seq: v.anchor_seq,
            blobs_checked: v.blobs_checked,
            problems: v.problems,
            problem_count: v.problem_count,
        }
    }
}

/// The ledger as it stands.
#[derive(Debug, Serialize, ToSchema)]
pub struct LedgerDto {
    /// The newest entry; write its `seq` and `hash` down elsewhere to prove later that nothing
    /// up to it changed.
    pub head: Option<LedgerEntryDto>,
    /// The last finished verification.
    pub last_verification: Option<LedgerVerificationDto>,
}

/// Starts a verification.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct VerifyLedgerRequest {
    /// Also read every message's bytes and compare their hash; as slow as reading the archive.
    #[serde(default)]
    pub blobs: bool,
    /// A head written down earlier, as `<seq>:<hash>`.
    #[serde(default)]
    pub anchor: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/ledger",
    responses((status = 200, body = LedgerDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_ledger(State(state): AppState) -> ApiResult<Json<LedgerDto>> {
    let status = state.operations.ledger().await.map_err(refused)?;
    Ok(Json(LedgerDto {
        head: status.head.map(Into::into),
        last_verification: status.last.map(Into::into),
    }))
}

fn anchor(raw: &str) -> ApiResult<(u64, LedgerHash)> {
    let bad = || ApiError::bad_request("the anchor is <seq>:<hash> with a 64-digit hex hash");
    let (seq, hash) = raw.trim().split_once(':').ok_or_else(bad)?;
    let seq: u64 = seq.trim().parse().map_err(|_| bad())?;
    if seq == 0 {
        return Err(bad());
    }
    Ok((seq, hash.parse().map_err(|_| bad())?))
}

#[utoipa::path(
    post,
    path = "/api/ledger/verify",
    request_body = VerifyLedgerRequest,
    responses((status = 202, description = "Started, or already running", body = TaskDto), (status = 400, description = "A malformed anchor", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn verify_ledger(
    State(state): AppState,
    Json(request): Json<VerifyLedgerRequest>,
) -> ApiResult<(StatusCode, Json<TaskDto>)> {
    let anchor = request
        .anchor
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .map(anchor)
        .transpose()?;
    let task = state
        .operations
        .verify_ledger(VerifyLedger {
            blobs: request.blobs,
            anchor,
        })
        .await
        .map_err(refused)?;
    Ok((StatusCode::ACCEPTED, Json(task.into())))
}
