use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use holtmail_core::{ArchiveId, FlowId, FlowRunId, MessageId, Timestamp};
use holtmail_rules::{Flow, Issue, TriggerKind};
use holtmail_store::{
    AuditKind, FlowChange, FlowMode, FlowRecord, FlowRun, FlowSave, GrantRole, NewFlow, RuleEvent,
    User,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};

use crate::dto::ErrorDto;
use crate::rules_run::{
    ActionResult, RuleLimits, action_kind, execute, record, run_flow, sends_mail,
};
use crate::send::parse_archive;
use crate::write::{Scopes, access, audit, denied, role_in};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// The most messages one manual run takes.
pub const MAX_MANUAL: usize = 100;

/// A flow of an archive as of its current version.
#[derive(Debug, Serialize, ToSchema)]
pub struct FlowDto {
    pub id: String,
    pub archive: String,
    pub name: String,
    /// Place in the archive's order, from 0; flows run in this order and `stop` ends the chain.
    pub position: u32,
    /// `test` (runs are logged, nothing is carried out; new flows start here), `on` or `off`.
    pub mode: String,
    /// Set while the circuit breaker holds the flow (`circuit_breaker`); setting a mode clears
    /// it.
    pub paused: Option<String>,
    /// When it was paused, Unix milliseconds.
    pub paused_at: Option<i64>,
    pub version: u32,
    /// The trigger type of the current version.
    pub trigger: String,
    /// Who saved the current version; the flow's actions are limited by their rights.
    pub saved_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The current version's JSON.
    #[schema(value_type = Object)]
    pub flow: serde_json::Value,
}

fn flow_dto(f: &FlowRecord, definition: serde_json::Value) -> FlowDto {
    FlowDto {
        id: f.id.to_string(),
        archive: f.archive_id.to_string(),
        name: f.name.clone(),
        position: f.position,
        mode: f.mode.as_str().into(),
        paused: f.paused.as_ref().map(|p| p.reason.clone()),
        paused_at: f.paused.as_ref().map(|p| p.at.as_millis()),
        version: f.version,
        trigger: f.trigger.clone(),
        saved_by: f.saved_by.to_string(),
        created_at: f.created_at.as_millis(),
        updated_at: f.updated_at.as_millis(),
        flow: definition,
    }
}

/// A saved version.
#[derive(Debug, Serialize, ToSchema)]
pub struct FlowVersionDto {
    pub version: u32,
    pub saved_by: String,
    pub saved_at: i64,
    #[schema(value_type = Object)]
    pub flow: serde_json::Value,
}

/// A new flow.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewFlowRequest {
    pub name: String,
    /// The flow JSON (`holtmail-rules` format).
    #[schema(value_type = Object)]
    pub flow: serde_json::Value,
}

/// A new version of a flow.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveFlowRequest {
    /// The version the editor started from; another save in between answers `409`.
    pub version: u32,
    #[schema(value_type = Object)]
    pub flow: serde_json::Value,
}

/// Changes to a flow's settings.
#[derive(Debug, Deserialize, ToSchema)]
pub struct FlowChangeRequest {
    pub name: Option<String>,
    /// `test`, `on` or `off`; any mode clears a pause.
    pub mode: Option<String>,
    pub position: Option<u32>,
}

/// Why a flow was refused: every issue the validator found.
#[derive(Debug, Serialize, ToSchema)]
pub struct FlowIssuesDto {
    pub error: String,
    #[schema(value_type = Vec<Object>)]
    pub issues: Vec<Issue>,
}

/// A dry run: what the flow would do for an event, nothing carried out or logged.
#[derive(Debug, Deserialize, ToSchema)]
pub struct FlowTestRequest {
    /// An unsaved flow to test instead of the saved one (required for the archive-wide test).
    #[schema(value_type = Option<Object>)]
    pub flow: Option<serde_json::Value>,
    /// The message the event is about.
    pub message: Option<String>,
    /// Treat the message as live mail (default `true`).
    pub live: Option<bool>,
    /// When the event happens, Unix milliseconds; now by default.
    pub at: Option<i64>,
}

/// What a dry run planned.
#[derive(Debug, Serialize, ToSchema)]
pub struct FlowTestDto {
    /// The plan: `ran`, `ignored`, `path` (each node with its outcome), `actions`, `stopped`.
    #[schema(value_type = Object)]
    pub plan: serde_json::Value,
    /// Why nothing could be planned; the plan is empty then.
    pub error: Option<String>,
}

/// A manual run on a selection or a search: exactly one of the two.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ManualRunRequest {
    /// Message ids of the flow's archive, at most 100.
    #[serde(default)]
    pub messages: Vec<String>,
    /// A search of the flow's archive in the search language; its newest 100 messages run.
    #[serde(default)]
    pub query: Option<String>,
}

/// One run in the runs log.
#[derive(Debug, Serialize, ToSchema)]
pub struct FlowRunDto {
    pub id: String,
    pub flow: String,
    pub version: u32,
    pub event: String,
    pub trigger: String,
    /// Evaluated only: the flow was in test mode.
    pub test: bool,
    pub message: Option<String>,
    /// `running`, `done`, `failed` or `interrupted`.
    pub status: String,
    #[schema(value_type = Option<Object>)]
    pub plan: Option<serde_json::Value>,
    /// What became of each planned action, in plan order.
    #[schema(value_type = Vec<Object>)]
    pub results: serde_json::Value,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

fn run_dto(r: &FlowRun) -> FlowRunDto {
    FlowRunDto {
        id: r.id.to_string(),
        flow: r.new.flow_id.to_string(),
        version: r.new.version,
        event: r.new.event_id.clone(),
        trigger: r.new.trigger.clone(),
        test: r.new.test,
        message: r.new.message_id.map(|m| m.to_string()),
        status: r.status.as_str().into(),
        plan: r.plan.clone(),
        results: r.results.clone(),
        error: r.error.clone(),
        started_at: r.new.started_at.as_millis(),
        finished_at: r.finished_at.map(|t| t.as_millis()),
    }
}

/// Paging of the runs log.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RunsParams {
    /// Only runs older than this run id.
    pub before: Option<String>,
    /// At most this many, 50 by default and 200 at most.
    pub limit: Option<u32>,
}

fn unprocessable(issues: Vec<Issue>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(FlowIssuesDto {
            error: "the flow is not valid".into(),
            issues,
        }),
    )
        .into_response()
}

async fn may_use(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    archive: ArchiveId,
) -> ApiResult<()> {
    if state.store.get_archive(archive).await?.is_none() {
        return Err(ApiError::not_found("archive"));
    }
    access(state)
        .may_use_rules(user, scopes, archive)
        .await
        .map_err(denied)
}

async fn flow_for(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    raw: &str,
) -> ApiResult<FlowRecord> {
    let id: FlowId = raw.parse().map_err(|_| ApiError::not_found("flow"))?;
    let flow = state
        .store
        .get_flow(id)
        .await?
        .ok_or_else(|| ApiError::not_found("flow"))?;
    if role_in(state, user, flow.archive_id).await?.is_none() {
        return Err(ApiError::not_found("flow"));
    }
    may_use(state, user, scopes, flow.archive_id).await?;
    Ok(flow)
}

async fn current(state: &ApiState, flow: &FlowRecord) -> ApiResult<serde_json::Value> {
    let version = state
        .store
        .flow_version(flow.id, flow.version)
        .await?
        .ok_or_else(|| ApiError::not_found("flow version"))?;
    Ok(serde_json::from_str(&version.definition).unwrap_or(serde_json::Value::Null))
}

/// Parses and validates a flow and checks its queries with the search parser and its actions
/// against the saver's grant: sending needs a `sender` grant.
async fn checked(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    value: &serde_json::Value,
) -> ApiResult<Result<Flow, Vec<Issue>>> {
    let flow: Flow = match serde_json::from_value(value.clone()) {
        Ok(flow) => flow,
        Err(error) => {
            return Ok(Err(vec![Issue {
                node: None,
                code: holtmail_rules::IssueCode::Format,
                detail: error.to_string(),
            }]));
        }
    };
    let mut issues = match holtmail_rules::validate(&flow) {
        Ok(()) => Vec::new(),
        Err(issues) => issues,
    };
    for (node, query) in flow.queries() {
        if let Err(error) = holtmail_query::parse(query) {
            issues.push(Issue {
                node: Some(node.to_owned()),
                code: holtmail_rules::IssueCode::BadParams,
                detail: format!("query: {error}"),
            });
        }
    }
    for node in flow.nodes.iter().filter(|n| n.kind == "webhook") {
        let Some(hook) = node.params.get("hook").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if let Err(reason) = state.rule_hooks.webhook_allowed(hook).await {
            issues.push(Issue {
                node: Some(node.id.clone()),
                code: holtmail_rules::IssueCode::BadParams,
                detail: format!("hook: {reason}"),
            });
        }
    }
    if !issues.is_empty() {
        return Ok(Err(issues));
    }
    if sends_mail(&flow) && role_in(state, user, archive).await? < Some(GrantRole::Sender) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "the flow sends mail and your grant on this archive does not allow sending".into(),
        ));
    }
    Ok(Ok(flow))
}

/// Someone other than the saver may make a flow that sends carry out its actions (turn it on,
/// run it by hand) only with a `sender` grant, since it then sends as the saver.
async fn may_start(state: &ApiState, user: &User, flow: &FlowRecord) -> ApiResult<()> {
    if user.id == flow.saved_by {
        return Ok(());
    }
    let definition = match state.store.flow_version(flow.id, flow.version).await? {
        Some(version) => Flow::from_json(&version.definition).ok(),
        None => None,
    };
    if definition.as_ref().is_some_and(sends_mail)
        && role_in(state, user, flow.archive_id).await? < Some(GrantRole::Sender)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "the flow sends mail and your grant on this archive does not allow sending".into(),
        ));
    }
    Ok(())
}

fn record_audit(flow: &FlowRecord, action: &str) -> Vec<String> {
    vec![flow.id.to_string(), action.to_owned()]
}

#[utoipa::path(
    get,
    path = "/api/rules/catalogue",
    responses((status = 200, description = "Every node type: id, version, role, ports, capability, the triggers it may follow, template fields, whether confirm is offered, docs in English and Russian and the JSON Schema of its parameters (fields carry an x-holtmail hint for the editor)", body = Vec<Object>), (status = 401, description = "Not signed in", body = ErrorDto)),
)]
pub(crate) async fn catalogue() -> Json<serde_json::Value> {
    Json(serde_json::to_value(holtmail_rules::describe()).unwrap_or_default())
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/flows",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, description = "The archive's flows in their order", body = Vec<FlowDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto)),
)]
pub(crate) async fn list_flows(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<FlowDto>>> {
    let archive = parse_archive(&raw)?;
    may_use(&state, &user, &scopes, archive).await?;
    let mut out = Vec::new();
    for flow in state.store.list_flows(archive).await? {
        let definition = current(&state, &flow).await?;
        out.push(flow_dto(&flow, definition));
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/flows",
    params(("id" = String, Path, description = "Archive id")),
    request_body = NewFlowRequest,
    responses((status = 201, description = "Created in test mode at the end of the order", body = FlowDto), (status = 400, description = "An empty name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, no rules scope, or a flow that sends without a sender grant", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 422, description = "The flow is not valid", body = FlowIssuesDto)),
)]
pub(crate) async fn create_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<NewFlowRequest>,
) -> ApiResult<Response> {
    let archive = parse_archive(&raw)?;
    may_use(&state, &user, &scopes, archive).await?;
    let name = request.name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ApiError::bad_request("a name of 1 to 200 characters"));
    }
    let flow = match checked(&state, &user, archive, &request.flow).await? {
        Ok(flow) => flow,
        Err(issues) => return Ok(unprocessable(issues)),
    };
    let created = state
        .store
        .create_flow(&NewFlow {
            archive_id: archive,
            name: name.to_owned(),
            definition: flow.to_json(),
            trigger: flow.trigger.kind.clone(),
            saved_by: user.id,
            at: Timestamp::now(),
        })
        .await?;
    audit(
        &state,
        &user,
        AuditKind::RuleChanged,
        Some(archive),
        None,
        record_audit(&created, "created"),
    )
    .await?;
    let definition = current(&state, &created).await?;
    Ok((StatusCode::CREATED, Json(flow_dto(&created, definition))).into_response())
}

#[utoipa::path(
    get,
    path = "/api/flows/{id}",
    params(("id" = String, Path, description = "Flow id")),
    responses((status = 200, body = FlowDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such flow", body = ErrorDto)),
)]
pub(crate) async fn get_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<FlowDto>> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    let definition = current(&state, &flow).await?;
    Ok(Json(flow_dto(&flow, definition)))
}

#[utoipa::path(
    put,
    path = "/api/flows/{id}",
    params(("id" = String, Path, description = "Flow id")),
    request_body = SaveFlowRequest,
    responses((status = 200, description = "Saved as the next version; the mode stays, a pause is cleared, and the saver's rights now limit its actions", body = FlowDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "As for creating", body = ErrorDto), (status = 404, description = "No such flow", body = ErrorDto), (status = 409, description = "Saved again since that version", body = ErrorDto), (status = 422, description = "The flow is not valid", body = FlowIssuesDto)),
)]
pub(crate) async fn save_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<SaveFlowRequest>,
) -> ApiResult<Response> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    let definition = match checked(&state, &user, flow.archive_id, &request.flow).await? {
        Ok(definition) => definition,
        Err(issues) => return Ok(unprocessable(issues)),
    };
    let saved = match state
        .store
        .save_flow(
            flow.id,
            request.version,
            &definition.to_json(),
            &definition.trigger.kind,
            user.id,
            Timestamp::now(),
        )
        .await?
    {
        FlowSave::Saved(saved) => saved,
        FlowSave::Stale { current } => {
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!("the flow was saved again since; version {current} is current"),
            ));
        }
    };
    audit(
        &state,
        &user,
        AuditKind::RuleChanged,
        Some(flow.archive_id),
        None,
        record_audit(&saved, "saved"),
    )
    .await?;
    let definition = current(&state, &saved).await?;
    Ok(Json(flow_dto(&saved, definition)).into_response())
}

#[utoipa::path(
    patch,
    path = "/api/flows/{id}",
    params(("id" = String, Path, description = "Flow id")),
    request_body = FlowChangeRequest,
    responses((status = 200, body = FlowDto), (status = 400, description = "An unknown mode or an empty name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, no rules scope, or turning on a flow that sends as someone else without a grant that allows sending", body = ErrorDto), (status = 404, description = "No such flow", body = ErrorDto)),
)]
pub(crate) async fn change_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<FlowChangeRequest>,
) -> ApiResult<Json<FlowDto>> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    let mode = request
        .mode
        .as_deref()
        .map(|m| FlowMode::parse(m).ok_or_else(|| ApiError::bad_request("mode is test, on or off")))
        .transpose()?;
    let name = request.name.as_deref().map(str::trim).map(str::to_owned);
    if name.as_ref().is_some_and(|n| n.is_empty() || n.len() > 200) {
        return Err(ApiError::bad_request("a name of 1 to 200 characters"));
    }
    if mode == Some(FlowMode::On) {
        may_start(&state, &user, &flow).await?;
    }
    let changed = state
        .store
        .change_flow(
            flow.id,
            &FlowChange {
                name,
                mode,
                position: request.position,
            },
            Timestamp::now(),
        )
        .await?;
    audit(
        &state,
        &user,
        AuditKind::RuleChanged,
        Some(flow.archive_id),
        None,
        record_audit(&changed, changed.mode.as_str()),
    )
    .await?;
    let definition = current(&state, &changed).await?;
    Ok(Json(flow_dto(&changed, definition)))
}

#[utoipa::path(
    delete,
    path = "/api/flows/{id}",
    params(("id" = String, Path, description = "Flow id")),
    responses((status = 204, description = "Removed with its versions and runs"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such flow", body = ErrorDto)),
)]
pub(crate) async fn delete_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    state.store.delete_flow(flow.id).await?;
    audit(
        &state,
        &user,
        AuditKind::RuleChanged,
        Some(flow.archive_id),
        None,
        record_audit(&flow, "deleted"),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/flows/{id}/versions",
    params(("id" = String, Path, description = "Flow id")),
    responses((status = 200, description = "Every saved version, newest first", body = Vec<FlowVersionDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such flow", body = ErrorDto)),
)]
pub(crate) async fn flow_versions(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<FlowVersionDto>>> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    Ok(Json(
        state
            .store
            .flow_versions(flow.id)
            .await?
            .into_iter()
            .map(|v| FlowVersionDto {
                version: v.version,
                saved_by: v.saved_by.to_string(),
                saved_at: v.saved_at.as_millis(),
                flow: serde_json::from_str(&v.definition).unwrap_or(serde_json::Value::Null),
            })
            .collect(),
    ))
}

async fn dry_run(
    state: &ApiState,
    user: &User,
    record: &FlowRecord,
    flow: &Flow,
    request: &FlowTestRequest,
) -> ApiResult<FlowTestDto> {
    let message = match request.message.as_deref() {
        Some(raw) => {
            let message = crate::drafts::readable_message(state, user, raw).await?;
            if message.archive_id != record.archive_id {
                return Err(ApiError::not_found("message"));
            }
            Some(message)
        }
        None => None,
    };
    let at = request
        .at
        .map(Timestamp::from_millis)
        .unwrap_or_else(Timestamp::now);
    Ok(
        match crate::rules_run::dry_run(
            &*state.store,
            &*state.blobs,
            record,
            flow,
            message.as_ref(),
            request.live.unwrap_or(true),
            at,
        )
        .await
        {
            Ok(plan) => FlowTestDto {
                plan: serde_json::to_value(plan)?,
                error: None,
            },
            Err(error) => FlowTestDto {
                plan: serde_json::to_value(holtmail_rules::Plan::default())?,
                error: Some(error),
            },
        },
    )
}

#[utoipa::path(
    post,
    path = "/api/flows/{id}/test",
    params(("id" = String, Path, description = "Flow id")),
    request_body = FlowTestRequest,
    responses((status = 200, description = "The plan for the event, with the path taken; nothing is carried out or logged", body = FlowTestDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such flow or message", body = ErrorDto), (status = 422, description = "The unsaved flow is not valid", body = FlowIssuesDto)),
)]
pub(crate) async fn test_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<FlowTestRequest>,
) -> ApiResult<Response> {
    let record = flow_for(&state, &user, &scopes, &raw).await?;
    let value = match &request.flow {
        Some(value) => value.clone(),
        None => current(&state, &record).await?,
    };
    let flow = match checked(&state, &user, record.archive_id, &value).await? {
        Ok(flow) => flow,
        Err(issues) => return Ok(unprocessable(issues)),
    };
    Ok(Json(dry_run(&state, &user, &record, &flow, &request).await?).into_response())
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/flows/test",
    params(("id" = String, Path, description = "Archive id")),
    request_body = FlowTestRequest,
    responses((status = 200, description = "The plan of an unsaved flow for the event; nothing is carried out or logged", body = FlowTestDto), (status = 400, description = "No flow given", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such archive or message", body = ErrorDto), (status = 422, description = "The flow is not valid", body = FlowIssuesDto)),
)]
pub(crate) async fn test_new_flow(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<FlowTestRequest>,
) -> ApiResult<Response> {
    let archive = parse_archive(&raw)?;
    may_use(&state, &user, &scopes, archive).await?;
    let value = request
        .flow
        .clone()
        .ok_or_else(|| ApiError::bad_request("give the flow to test"))?;
    let flow = match checked(&state, &user, archive, &value).await? {
        Ok(flow) => flow,
        Err(issues) => return Ok(unprocessable(issues)),
    };
    let now = Timestamp::now();
    let record = FlowRecord {
        id: FlowId::new(),
        archive_id: archive,
        name: String::new(),
        position: 0,
        mode: FlowMode::Test,
        paused: None,
        version: 0,
        trigger: flow.trigger.kind.clone(),
        saved_by: user.id,
        created_at: now,
        updated_at: now,
    };
    Ok(Json(dry_run(&state, &user, &record, &flow, &request).await?).into_response())
}

/// A template to render against a message, as a rule with this trigger would.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TemplatePreviewRequest {
    /// The template source.
    pub template: String,
    /// The trigger whose context the template sees (default `manual`).
    pub trigger: Option<String>,
    /// The message the event is about; without one, message fields are empty.
    pub message: Option<String>,
    /// When the event happens, Unix milliseconds; now by default.
    pub at: Option<i64>,
}

/// A rendered template, or why it does not render.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplatePreviewDto {
    /// The rendered text; absent when the template fails.
    pub text: Option<String>,
    /// Why the template does not render.
    pub error: Option<String>,
    /// The values the template saw: `sender`, `subject`, `date`, `alias`, `insights`,
    /// `invitation`, `event`, `now`.
    #[schema(value_type = Object)]
    pub context: serde_json::Value,
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/flows/preview",
    params(("id" = String, Path, description = "Archive id")),
    request_body = TemplatePreviewRequest,
    responses((status = 200, description = "The template rendered against the message, with the context it saw; nothing is stored", body = TemplatePreviewDto), (status = 400, description = "Unknown trigger", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such archive or message", body = ErrorDto)),
)]
pub(crate) async fn preview_template(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<TemplatePreviewRequest>,
) -> ApiResult<Json<TemplatePreviewDto>> {
    let archive = parse_archive(&raw)?;
    may_use(&state, &user, &scopes, archive).await?;
    let trigger = match request.trigger.as_deref() {
        None => TriggerKind::Manual,
        Some(id) => {
            TriggerKind::from_id(id).ok_or_else(|| ApiError::bad_request("unknown trigger"))?
        }
    };
    let message = match request.message.as_deref() {
        Some(raw) => {
            let message = crate::drafts::readable_message(&state, &user, raw).await?;
            if message.archive_id != archive {
                return Err(ApiError::not_found("message"));
            }
            Some(message)
        }
        None => None,
    };
    let at = request
        .at
        .map(Timestamp::from_millis)
        .unwrap_or_else(Timestamp::now);
    let now = Timestamp::now();
    let record = FlowRecord {
        id: FlowId::new(),
        archive_id: archive,
        name: String::new(),
        position: 0,
        mode: FlowMode::Test,
        paused: None,
        version: 0,
        trigger: trigger.id().into(),
        saved_by: user.id,
        created_at: now,
        updated_at: now,
    };
    let (context, text) = crate::rules_run::preview_template(
        &*state.store,
        &*state.blobs,
        &record,
        trigger,
        message.as_ref(),
        at,
        &request.template,
    )
    .await?;
    let (text, error) = match text {
        Ok(text) => (Some(text), None),
        Err(error) => (None, Some(error)),
    };
    Ok(Json(TemplatePreviewDto {
        text,
        error,
        context: serde_json::to_value(context)?,
    }))
}

#[utoipa::path(
    post,
    path = "/api/flows/{id}/run",
    params(("id" = String, Path, description = "Flow id")),
    request_body = ManualRunRequest,
    responses((status = 200, description = "One run per message (a selection in order, a search newest first); a flow in test mode only logs. A message the flow already ran on is left out", body = Vec<FlowRunDto>), (status = 400, description = "Neither or both of messages and query, more than 100 messages, a query that does not parse, or the flow's trigger is not manual", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, no rules scope, or the flow sends as someone else and the grant does not allow sending", body = ErrorDto), (status = 404, description = "No such flow or message", body = ErrorDto)),
)]
pub(crate) async fn run_manual(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<ManualRunRequest>,
) -> ApiResult<Json<Vec<FlowRunDto>>> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    if flow.trigger != TriggerKind::Manual.id() {
        return Err(ApiError::bad_request(
            "only a flow with the manual trigger runs on a selection",
        ));
    }
    let ids = match (&request.query, request.messages.is_empty()) {
        (Some(query), true) => {
            let parsed =
                holtmail_query::parse(query).map_err(|e| ApiError::bad_request(e.to_string()))?;
            state
                .index
                .search(
                    flow.archive_id,
                    &parsed,
                    holtmail_index::SortOrder::Newest,
                    0,
                    MAX_MANUAL as u32,
                )
                .await
                .map_err(|e| match e {
                    holtmail_index::IndexError::Invalid(m) => ApiError::bad_request(m),
                    other => other.into(),
                })?
                .hits
                .iter()
                .map(ToString::to_string)
                .collect()
        }
        (None, false) if request.messages.len() <= MAX_MANUAL => request.messages.clone(),
        _ => {
            return Err(ApiError::bad_request(
                "give 1 to 100 messages or a query, not both",
            ));
        }
    };
    if flow.mode == FlowMode::Off || flow.paused.is_some() {
        return Err(ApiError::bad_request("the flow is off or paused"));
    }
    if flow.mode == FlowMode::On {
        may_start(&state, &user, &flow).await?;
    }
    let mut messages = Vec::with_capacity(ids.len());
    for raw in &ids {
        let message = crate::drafts::readable_message(&state, &user, raw).await?;
        if message.archive_id != flow.archive_id {
            return Err(ApiError::not_found("message"));
        }
        messages.push(message);
    }
    let batch = MessageId::new();
    let mut runs = Vec::new();
    for message in messages {
        let event = RuleEvent {
            id: format!("manual:{batch}:{}", message.id),
            archive_id: flow.archive_id,
            trigger: TriggerKind::Manual.id().into(),
            message_id: Some(message.id),
            item_id: None,
            live: false,
            payload: json!({ "flow": flow.id.to_string() }),
            at: Timestamp::now(),
        };
        if let Some(outcome) =
            run_flow(&state, &RuleLimits::default(), &flow, &event, false).await?
        {
            runs.push(run_dto(&outcome.run));
        }
    }
    audit(
        &state,
        &user,
        AuditKind::RuleChanged,
        Some(flow.archive_id),
        None,
        vec![flow.id.to_string(), "run".into(), runs.len().to_string()],
    )
    .await?;
    Ok(Json(runs))
}

#[utoipa::path(
    get,
    path = "/api/flows/{id}/runs",
    params(("id" = String, Path, description = "Flow id"), RunsParams),
    responses((status = 200, description = "The runs log, newest first, kept 30 days", body = Vec<FlowRunDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such flow", body = ErrorDto)),
)]
pub(crate) async fn flow_runs(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Query(params): Query<RunsParams>,
) -> ApiResult<Json<Vec<FlowRunDto>>> {
    let flow = flow_for(&state, &user, &scopes, &raw).await?;
    let before = params
        .before
        .as_deref()
        .map(|b| {
            b.parse::<FlowRunId>()
                .map_err(|_| ApiError::bad_request("malformed run id"))
        })
        .transpose()?;
    let runs = state
        .store
        .flow_runs(flow.id, before, params.limit.unwrap_or(50).clamp(1, 200))
        .await?;
    Ok(Json(runs.iter().map(run_dto).collect()))
}

async fn run_for(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    raw: &str,
) -> ApiResult<(FlowRecord, FlowRun)> {
    let id: FlowRunId = raw.parse().map_err(|_| ApiError::not_found("run"))?;
    let run = state
        .store
        .get_flow_run(id)
        .await?
        .ok_or_else(|| ApiError::not_found("run"))?;
    let flow = flow_for(state, user, scopes, &run.new.flow_id.to_string())
        .await
        .map_err(|_| ApiError::not_found("run"))?;
    Ok((flow, run))
}

#[utoipa::path(
    get,
    path = "/api/flow-runs/{id}",
    params(("id" = String, Path, description = "Run id")),
    responses((status = 200, body = FlowRunDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such run", body = ErrorDto)),
)]
pub(crate) async fn get_run(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<FlowRunDto>> {
    let (_, run) = run_for(&state, &user, &scopes, &raw).await?;
    Ok(Json(run_dto(&run)))
}

async fn decide_pending(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    raw: &str,
    index: usize,
    confirm: bool,
) -> ApiResult<FlowRunDto> {
    let (flow, run) = run_for(state, user, scopes, raw).await?;
    let results: Vec<ActionResult> = serde_json::from_value(run.results.clone())
        .map_err(|_| ApiError::bad_request("the run's results do not read"))?;
    let plan: holtmail_rules::Plan = run
        .plan
        .clone()
        .and_then(|p| serde_json::from_value(p).ok())
        .ok_or_else(|| ApiError::bad_request("the run has no plan"))?;
    let (Some(result), Some(planned)) = (results.get(index), plan.actions.get(index)) else {
        return Err(ApiError::not_found("action"));
    };
    let not_pending = |now: &str| {
        ApiError(
            StatusCode::CONFLICT,
            format!("the action is {now}, not pending"),
        )
    };
    if result.result != "pending" {
        return Err(not_pending(&result.result));
    }
    let mut decided = result.clone();
    if confirm {
        let mut claimed = result.clone();
        claimed.result = "confirming".into();
        claimed.detail = Some(format!("being confirmed by {}", user.id));
        if !state
            .store
            .set_flow_run_result(run.id, index, "pending", &serde_json::to_value(&claimed)?)
            .await?
        {
            return Err(not_pending("decided already"));
        }
        let message = match run.new.message_id {
            Some(id) => state.store.get_message(id).await?,
            None => None,
        };
        let key = format!("rule-confirm:{}:{index}", run.id);
        match execute(
            state,
            user,
            &flow,
            run.id,
            &planned.action,
            message.as_ref(),
            Some(key),
            None,
        )
        .await
        {
            Ok(done) if done.result == "done" => {
                record(state, &flow, run.id, &planned.action, done.identity).await;
                decided.result = "confirmed".into();
                decided.detail = Some(format!("confirmed by {}", user.id));
            }
            Ok(done) => {
                decided.result = done.result;
                decided.detail = done.detail;
            }
            Err(error) => {
                decided.result = "failed".into();
                decided.detail = Some(error.1);
            }
        }
        state
            .store
            .set_flow_run_result(
                run.id,
                index,
                "confirming",
                &serde_json::to_value(&decided)?,
            )
            .await?;
    } else {
        decided.result = "dismissed".into();
        decided.detail = Some(format!("dismissed by {}", user.id));
        if !state
            .store
            .set_flow_run_result(run.id, index, "pending", &serde_json::to_value(&decided)?)
            .await?
        {
            return Err(not_pending("decided already"));
        }
    }
    let kind = action_kind(&planned.action);
    audit(
        state,
        user,
        AuditKind::RuleChanged,
        Some(flow.archive_id),
        None,
        vec![
            run.id.to_string(),
            if confirm { "confirmed" } else { "dismissed" }.into(),
            kind.into(),
        ],
    )
    .await?;
    let run = state
        .store
        .get_flow_run(run.id)
        .await?
        .ok_or_else(|| ApiError::not_found("run"))?;
    Ok(run_dto(&run))
}

#[utoipa::path(
    post,
    path = "/api/flow-runs/{id}/actions/{index}/confirm",
    params(("id" = String, Path, description = "Run id"), ("index" = usize, Path, description = "The action's place in the plan, from 0")),
    responses((status = 200, description = "The held action was carried out as the person confirming, with every lock checked for them (`confirmed`, or `failed` with the reason)", body = FlowRunDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such run or action", body = ErrorDto), (status = 409, description = "The action is not pending", body = ErrorDto)),
)]
pub(crate) async fn confirm_action(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path((raw, index)): Path<(String, usize)>,
) -> ApiResult<Json<FlowRunDto>> {
    Ok(Json(
        decide_pending(&state, &user, &scopes, &raw, index, true).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/flow-runs/{id}/actions/{index}/dismiss",
    params(("id" = String, Path, description = "Run id"), ("index" = usize, Path, description = "The action's place in the plan, from 0")),
    responses((status = 200, description = "The held action will never run (`dismissed`)", body = FlowRunDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Rules are off, no editor grant, or no rules scope", body = ErrorDto), (status = 404, description = "No such run or action", body = ErrorDto), (status = 409, description = "The action is not pending", body = ErrorDto)),
)]
pub(crate) async fn dismiss_action(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path((raw, index)): Path<(String, usize)>,
) -> ApiResult<Json<FlowRunDto>> {
    Ok(Json(
        decide_pending(&state, &user, &scopes, &raw, index, false).await?,
    ))
}
