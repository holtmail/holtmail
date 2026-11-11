use async_trait::async_trait;
use holtmail_core::{
    ArchiveId, FlowId, FlowRunId, IdentityId, ItemId, MessageId, Timestamp, UserId,
};

use crate::StoreError;

/// Whether a flow runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowMode {
    /// Runs are evaluated and logged, nothing is carried out. New flows start here.
    Test,
    /// Runs are carried out.
    On,
    /// The flow never runs.
    Off,
}

impl FlowMode {
    /// Every value, in a fixed order.
    pub const ALL: [FlowMode; 3] = [Self::Test, Self::On, Self::Off];

    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// The mode named by `as_str`, or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == name)
    }
}

/// A flow stopped by the circuit breaker; it runs again once someone turns it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowPause {
    /// Why, as a stable code such as `circuit_breaker`.
    pub reason: String,
    /// When.
    pub at: Timestamp,
}

/// A rule of an archive, as of its current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRecord {
    /// The flow.
    pub id: FlowId,
    /// Its archive.
    pub archive_id: ArchiveId,
    /// A name for people.
    pub name: String,
    /// Place in the archive's order, from 0; flows run in this order.
    pub position: u32,
    /// Whether it runs.
    pub mode: FlowMode,
    /// Set while the circuit breaker holds it; a paused flow does not run in any mode.
    pub paused: Option<FlowPause>,
    /// The current version, from 1.
    pub version: u32,
    /// The trigger type of the current version, such as `message.archived`.
    pub trigger: String,
    /// Who saved the current version; its actions are limited by this user's rights.
    pub saved_by: UserId,
    /// When the flow was created.
    pub created_at: Timestamp,
    /// When the flow or its mode last changed.
    pub updated_at: Timestamp,
}

/// One saved version of a flow. Versions are never changed or removed while the flow exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowVersion {
    /// The flow.
    pub flow_id: FlowId,
    /// From 1, one more per save.
    pub version: u32,
    /// The flow's JSON as saved.
    pub definition: String,
    /// Who saved it.
    pub saved_by: UserId,
    /// When.
    pub saved_at: Timestamp,
}

/// A flow to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFlow {
    /// Its archive.
    pub archive_id: ArchiveId,
    /// A name for people.
    pub name: String,
    /// The flow's JSON; becomes version 1.
    pub definition: String,
    /// Its trigger type.
    pub trigger: String,
    /// Who saved it.
    pub saved_by: UserId,
    /// When.
    pub at: Timestamp,
}

/// What [`RuleStore::save_flow`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowSave {
    /// A new version was stored.
    Saved(FlowRecord),
    /// The flow was saved again since the version the caller had; nothing changed.
    Stale {
        /// The version now current.
        current: u32,
    },
}

/// Changes to a flow's settings; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowChange {
    /// A new name.
    pub name: Option<String>,
    /// A new mode; any mode clears a pause.
    pub mode: Option<FlowMode>,
    /// A new place in the archive's order; the others keep their order around it.
    pub position: Option<u32>,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowRunStatus {
    /// Claimed and not finished yet.
    Running,
    /// Finished; each action's result is in the run.
    Done,
    /// The flow could not be evaluated, or the run was refused as a whole.
    Failed,
    /// The process stopped during the run; it is not tried again.
    Interrupted,
}

impl FlowRunStatus {
    /// Every value, in a fixed order.
    pub const ALL: [FlowRunStatus; 4] =
        [Self::Running, Self::Done, Self::Failed, Self::Interrupted];

    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    /// The status named by `as_str`, or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// A run to claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFlowRun {
    /// The flow.
    pub flow_id: FlowId,
    /// Its archive.
    pub archive_id: ArchiveId,
    /// The flow version evaluated.
    pub version: u32,
    /// The event's stable id; with the flow and version it keys the run.
    pub event_id: String,
    /// The event's trigger type.
    pub trigger: String,
    /// Evaluated only: the flow was in test mode, or a person asked for a dry run.
    pub test: bool,
    /// The event's message.
    pub message_id: Option<MessageId>,
    /// The message's sender address, lowercase, for counters.
    pub sender: Option<String>,
    /// When the run started.
    pub started_at: Timestamp,
}

/// One run in the runs log.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowRun {
    /// The run.
    pub id: FlowRunId,
    /// What was claimed.
    pub new: NewFlowRun,
    /// How it ended.
    pub status: FlowRunStatus,
    /// The plan: nodes visited, decisions and intended actions.
    pub plan: Option<serde_json::Value>,
    /// What became of each planned action, in plan order.
    pub results: serde_json::Value,
    /// Why the run failed, without message content.
    pub error: Option<String>,
    /// Whether the run carried out at least one action; a test run never does.
    pub acted: bool,
    /// When it finished.
    pub finished_at: Option<Timestamp>,
}

/// What [`RuleStore::finish_flow_run`] records.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowRunEnd {
    /// `Done` or `Failed`.
    pub status: FlowRunStatus,
    /// The plan.
    pub plan: Option<serde_json::Value>,
    /// Each action's result.
    pub results: serde_json::Value,
    /// Why it failed.
    pub error: Option<String>,
    /// Whether at least one action was carried out.
    pub acted: bool,
    /// When.
    pub at: Timestamp,
}

/// An action a flow carried out, kept for rate limits and the once-per-sender rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowAction {
    /// The run.
    pub run_id: FlowRunId,
    /// The flow.
    pub flow_id: FlowId,
    /// The action type, such as `reply` or `move`.
    pub kind: String,
    /// The identity a message was sent as.
    pub identity: Option<IdentityId>,
    /// For a reply: the sender answered, lowercase.
    pub recipient: Option<String>,
    /// When.
    pub at: Timestamp,
}

/// An event waiting for the flows of its archive, kept until every flow had its run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEvent {
    /// Stable id, unique per archive.
    pub id: String,
    /// The archive.
    pub archive_id: ArchiveId,
    /// Trigger type, such as `message.archived`.
    pub trigger: String,
    /// The event's message.
    pub message_id: Option<MessageId>,
    /// The event's calendar item.
    pub item_id: Option<ItemId>,
    /// The message is live mail.
    pub live: bool,
    /// Anything else the trigger needs, as JSON; never message content.
    pub payload: serde_json::Value,
    /// When it happened.
    pub at: Timestamp,
}

/// Most runs [`RuleStore::flow_runs`] returns.
pub const MAX_RUNS_PAGE: u32 = 200;

/// Rules: flows with their versions, the runs log keyed by flow, version and event, the actions
/// flows carried out and the events waiting for them. Whether a flow may run and what it may do
/// is decided by callers, not here.
#[async_trait]
pub trait RuleStore: Send + Sync {
    /// Creates a flow in test mode at the end of the archive's order, with `definition` as
    /// version 1. `NotFound` for an unknown archive.
    async fn create_flow(&self, flow: &NewFlow) -> Result<FlowRecord, StoreError>;

    /// The flow, or `None`.
    async fn get_flow(&self, id: FlowId) -> Result<Option<FlowRecord>, StoreError>;

    /// The archive's flows in their order.
    async fn list_flows(&self, archive: ArchiveId) -> Result<Vec<FlowRecord>, StoreError>;

    /// Stores `definition` as the next version when the current one is `expected`, as a
    /// compare-and-set; otherwise [`FlowSave::Stale`] and nothing changes. The mode is kept and a
    /// pause cleared. `NotFound` for an unknown flow.
    #[allow(clippy::too_many_arguments)]
    async fn save_flow(
        &self,
        id: FlowId,
        expected: u32,
        definition: &str,
        trigger: &str,
        user: UserId,
        at: Timestamp,
    ) -> Result<FlowSave, StoreError>;

    /// Changes name, mode or position. Positions of the archive stay `0..n` without gaps.
    /// `NotFound` for an unknown flow.
    async fn change_flow(
        &self,
        id: FlowId,
        change: &FlowChange,
        at: Timestamp,
    ) -> Result<FlowRecord, StoreError>;

    /// Pauses a flow that is not paused yet. Returns `true` when this call paused it, so only
    /// one caller notifies anyone.
    async fn pause_flow(&self, id: FlowId, reason: &str, at: Timestamp)
    -> Result<bool, StoreError>;

    /// Removes the flow with its versions, runs and actions. `false` when there was none.
    async fn delete_flow(&self, id: FlowId) -> Result<bool, StoreError>;

    /// Every version of the flow, newest first.
    async fn flow_versions(&self, id: FlowId) -> Result<Vec<FlowVersion>, StoreError>;

    /// One version, or `None`.
    async fn flow_version(
        &self,
        id: FlowId,
        version: u32,
    ) -> Result<Option<FlowVersion>, StoreError>;

    /// Claims the run of `(flow, version, event)` as `Running`. `None` when that run exists
    /// already, whatever became of it, so an event never runs twice per flow version, also
    /// across restarts and concurrent callers.
    async fn claim_flow_run(&self, run: &NewFlowRun) -> Result<Option<FlowRun>, StoreError>;

    /// Records how a run ended. `NotFound` for an unknown run.
    async fn finish_flow_run(&self, id: FlowRunId, end: &FlowRunEnd) -> Result<(), StoreError>;

    /// Replaces the action result at `index` of a run's results when that result's `result`
    /// field is `expected`, as a compare-and-set, as when a person confirms or dismisses a held
    /// action: of concurrent callers expecting the same value exactly one gets `true`, and results
    /// at other indices are never touched. `false` and nothing changes when the field holds
    /// another value or there is no such index. `NotFound` for an unknown run.
    async fn set_flow_run_result(
        &self,
        id: FlowRunId,
        index: usize,
        expected: &str,
        result: &serde_json::Value,
    ) -> Result<bool, StoreError>;

    /// The run, or `None`.
    async fn get_flow_run(&self, id: FlowRunId) -> Result<Option<FlowRun>, StoreError>;

    /// The newest run of the flow for `event`, of any version, or `None`.
    async fn flow_run_for_event(
        &self,
        id: FlowId,
        event: &str,
    ) -> Result<Option<FlowRun>, StoreError>;

    /// The flow's runs newest first, older than `before` when given, at most `limit` (capped at
    /// [`MAX_RUNS_PAGE`]).
    async fn flow_runs(
        &self,
        id: FlowId,
        before: Option<FlowRunId>,
        limit: u32,
    ) -> Result<Vec<FlowRun>, StoreError>;

    /// Marks every `Running` run `Interrupted`; returns how many. Call once at start.
    async fn interrupt_flow_runs(&self, at: Timestamp) -> Result<u64, StoreError>;

    /// Removes runs started before `runs_before` and actions before `actions_before`; returns
    /// how many runs went.
    async fn prune_flow_runs(
        &self,
        runs_before: Timestamp,
        actions_before: Timestamp,
    ) -> Result<u64, StoreError>;

    /// Runs of the flow started at or after `since`, test runs too; only those recorded as
    /// `acted` when `acted`, and only those of `sender` when given.
    async fn count_flow_runs(
        &self,
        id: FlowId,
        since: Timestamp,
        acted: bool,
        sender: Option<&str>,
    ) -> Result<u64, StoreError>;

    /// Messages of the archive whose `From` is `address` (case ignored) archived at or after
    /// `since`, `except` not counted.
    async fn count_messages_from(
        &self,
        archive: ArchiveId,
        address: &str,
        since: Timestamp,
        except: Option<MessageId>,
    ) -> Result<u64, StoreError>;

    /// Records an action a flow carried out.
    async fn record_flow_action(&self, action: &FlowAction) -> Result<(), StoreError>;

    /// Actions of the flow at or after `since`.
    async fn count_flow_actions(&self, id: FlowId, since: Timestamp) -> Result<u64, StoreError>;

    /// Actions of any flow that sent as `identity` at or after `since`.
    async fn count_identity_flow_actions(
        &self,
        identity: IdentityId,
        since: Timestamp,
    ) -> Result<u64, StoreError>;

    /// When the flow last carried out a `reply` to `recipient` (case ignored), or `None`.
    async fn last_flow_reply(
        &self,
        id: FlowId,
        recipient: &str,
    ) -> Result<Option<Timestamp>, StoreError>;

    /// Keeps an event for the flows of its archive. Idempotent per archive and id: `false` when
    /// it is waiting already. An event kept again after it was handled only meets runs that
    /// exist, so it carries out nothing twice.
    async fn put_rule_event(&self, event: &RuleEvent) -> Result<bool, StoreError>;

    /// Waiting events, oldest first, at most `limit`.
    async fn due_rule_events(&self, limit: u32) -> Result<Vec<RuleEvent>, StoreError>;

    /// Removes a handled event. Harmless when it is gone.
    async fn drop_rule_event(&self, archive: ArchiveId, id: &str) -> Result<(), StoreError>;
}
