//! The rules executor: turns events into runs of the archive's flows, evaluates them with
//! `holtmail-rules` and carries out their plans through the same paths a person uses (drafts and
//! the outbox, mail changes, item changes and the scheduler), so every lock is checked again.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use holtmail_blob::BlobStore;
use holtmail_compose::{ExtraHeader, ForwardKind, ReplyKind};
use holtmail_core::{ArchiveId, FlowId, FlowRunId, IdentityId, ItemId, MessageId, Timestamp};
use holtmail_insight::KindTags;
use holtmail_mime::ParsedMessage;
use holtmail_query::{AttachmentFacts, Matcher, Verdict};
use holtmail_rules::params::{Answer, CounterScope, CounterWindow, Flag, Strategy, Trust};
use holtmail_rules::{
    Action, AliasFact, CounterFact, Event, EventFacts, FactSheet, Facts, FactsError, Flow, Mailbox,
    MessageFacts, Plan, PlannedAction, Spec, TemplateContext, TriggerKind, WithMatcher,
};
use holtmail_store::{
    AuditKind, CollectionKind, Draft, FlowAction, FlowMode, FlowRecord, FlowRun, FlowRunEnd,
    FlowRunStatus, Message, NewFlowRun, OccurrenceQuery, RuleEvent, Store, User,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;

use crate::outbox::{RouteOverrides, SendOptions, Sent, send_stored_draft};
use crate::write::{Scopes, access, audit};
use crate::{ApiError, ApiResult, ApiState};

/// How long runs are kept in the log.
pub const KEEP_RUNS: Duration = Duration::from_secs(30 * 86_400);
/// How long carried-out actions are kept, longer than the longest `days` of a reply.
pub const KEEP_ACTIONS: Duration = Duration::from_secs(400 * 86_400);

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const TICK: Duration = Duration::from_secs(30);
const EVENT_PAGE: u32 = 50;
const MAX_CATCH_UP_MS: i64 = HOUR_MS;
const TICK_KEY: &str = "rules.ticked_through";

/// Limits on what flows may do, so a rule cannot mail thousands of people or loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleLimits {
    /// Actions one flow carries out in an hour; later ones are skipped as `rate_limited`.
    pub actions_per_flow_hour: u64,
    /// Messages flows send as one identity in an hour; later ones are skipped as
    /// `rate_limited`.
    pub sends_per_identity_hour: u64,
    /// Runs of one flow that planned an action in an hour before the circuit breaker pauses it.
    pub breaker_runs_per_hour: u64,
}

impl Default for RuleLimits {
    fn default() -> Self {
        Self {
            actions_per_flow_hour: 60,
            sends_per_identity_hour: 30,
            breaker_runs_per_hour: 100,
        }
    }
}

/// What the executor tells outside the API: the owner when a flow is paused, and webhooks.
/// Implementations log their own failures; what they say carries ids only.
#[async_trait]
pub trait RuleHooks: Send + Sync {
    /// The circuit breaker paused `flow`; tell whoever runs the instance.
    async fn flow_paused(&self, archive: ArchiveId, flow: FlowId, reason: &str);

    /// A `notify` action ran in `run`; its title and body stay in the runs log.
    async fn notified(&self, archive: ArchiveId, flow: FlowId, run: FlowRunId);

    /// Whether a flow may name the webhook `hook`: it exists and its managers subscribed it to
    /// `flow.run`. The error is a reason for people.
    async fn webhook_allowed(&self, hook: &str) -> Result<(), String>;

    /// A `webhook` action: deliver the run's ids to the webhook `hook`. An unknown or disabled
    /// webhook, or one not subscribed to `flow.run`, is an error with a reason for people.
    async fn call(
        &self,
        hook: &str,
        archive: ArchiveId,
        flow: FlowId,
        run: FlowRunId,
        message: Option<MessageId>,
    ) -> Result<(), String>;
}

/// [`RuleHooks`] that tell nobody and refuse webhook actions.
pub struct NoRuleHooks;

#[async_trait]
impl RuleHooks for NoRuleHooks {
    async fn flow_paused(&self, _: ArchiveId, _: FlowId, _: &str) {}

    async fn notified(&self, _: ArchiveId, _: FlowId, _: FlowRunId) {}

    async fn webhook_allowed(&self, _: &str) -> Result<(), String> {
        Err("this server calls no webhooks from rules".into())
    }

    async fn call(
        &self,
        _: &str,
        _: ArchiveId,
        _: FlowId,
        _: FlowRunId,
        _: Option<MessageId>,
    ) -> Result<(), String> {
        Err("this server calls no webhooks from rules".into())
    }
}

/// What became of one planned action, kept in the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    /// The node that planned it.
    pub node: String,
    /// The action type, such as `reply`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `done`, `skipped`, `failed`, `pending` (waits for a person), `draft` (a draft waits in
    /// the outbox), `test` (test mode, nothing ran), `confirmed` or `dismissed`.
    pub result: String,
    /// Why, for people; never message content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The outbox message it queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox: Option<String>,
    /// The draft it left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
}

impl ActionResult {
    fn new(planned: &PlannedAction, result: &str) -> Self {
        Self {
            node: planned.node.clone(),
            kind: action_kind(&planned.action).to_owned(),
            result: result.to_owned(),
            detail: None,
            outbox: None,
            draft: None,
        }
    }

    fn because(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The action type id of an action.
pub fn action_kind(action: &Action) -> &'static str {
    match action {
        Action::Label { .. } => "label",
        Action::SetFlags { .. } => "set_flags",
        Action::Move { .. } => "move",
        Action::Delete { .. } => "delete",
        Action::Reply { .. } => "reply",
        Action::Forward { .. } => "forward",
        Action::Send { .. } => "send",
        Action::Rsvp { .. } => "rsvp",
        Action::CreateEvent { .. } => "create_event",
        Action::CreateContact { .. } => "create_contact",
        Action::Notify { .. } => "notify",
        Action::Webhook { .. } => "webhook",
        Action::SetIdentity { .. } => "set_identity",
        Action::SetTransport { .. } => "set_transport",
        Action::SetAliasStrategy { .. } => "set_alias_strategy",
        Action::AddBcc { .. } => "add_bcc",
        Action::RequireConfirmation { .. } => "require_confirmation",
        Action::Block { .. } => "block",
    }
}

fn chrono(t: Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(t.as_millis()).unwrap_or_default()
}

fn from_chrono(t: DateTime<Utc>) -> Timestamp {
    Timestamp::from_millis(t.timestamp_millis())
}

fn lower(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The background executor: keeps events from ingest, fires schedules and upcoming events, and
/// runs the archive's flows for each in order.
pub struct Rules {
    state: ApiState,
    limits: RuleLimits,
    wake: Notify,
}

impl Rules {
    /// An executor over the API's state, with its hooks and switches.
    pub fn new(state: ApiState, limits: RuleLimits) -> Arc<Self> {
        Arc::new(Self {
            state,
            limits,
            wake: Notify::new(),
        })
    }

    /// Keeps the events a message just added by ingest raises: `message.archived` and, when it
    /// carries an invitation, `invitation.received`. Only live mail of an archive with flows
    /// raises events, and only while the `[write]` switches allow rules. Never fails: a store
    /// error is logged with ids.
    pub async fn observe(
        &self,
        archive: ArchiveId,
        message: MessageId,
        parsed: &ParsedMessage,
        live: bool,
    ) {
        let switches = self.state.write;
        if !live || !switches.enabled || !switches.rules {
            return;
        }
        let flows = match self.state.store.list_flows(archive).await {
            Ok(flows) => flows,
            Err(error) => {
                tracing::warn!(%archive, %message, %error, "rule flows not read");
                return;
            }
        };
        let wanted = |trigger: TriggerKind| {
            flows
                .iter()
                .any(|f| f.mode != FlowMode::Off && f.trigger == trigger.id())
        };
        let mut triggers = Vec::new();
        if wanted(TriggerKind::MessageArchived) {
            triggers.push(TriggerKind::MessageArchived);
        }
        if wanted(TriggerKind::InvitationReceived) && invitation(parsed).is_some() {
            triggers.push(TriggerKind::InvitationReceived);
        }
        for trigger in triggers {
            let event = RuleEvent {
                id: format!("{}:{message}", trigger.id()),
                archive_id: archive,
                trigger: trigger.id().into(),
                message_id: Some(message),
                item_id: None,
                live: true,
                payload: json!({}),
                at: Timestamp::now(),
            };
            if let Err(error) = self.state.store.put_rule_event(&event).await {
                tracing::warn!(%archive, %message, %error, "rule event not kept");
            }
        }
        self.wake.notify_one();
    }

    /// Runs the flows of every waiting event, oldest first; returns how many events were
    /// handled. An event whose flows could not all be decided because the store failed stays
    /// and is tried again; runs already claimed are not repeated.
    pub async fn run_due(&self) -> usize {
        let mut handled = 0;
        loop {
            let events = match self.state.store.due_rule_events(EVENT_PAGE).await {
                Ok(events) => events,
                Err(error) => {
                    tracing::warn!(%error, "rule events not read");
                    return handled;
                }
            };
            if events.is_empty() {
                return handled;
            }
            for event in &events {
                match handle_event(&self.state, &self.limits, event).await {
                    Ok(()) => {
                        if let Err(error) = self
                            .state
                            .store
                            .drop_rule_event(event.archive_id, &event.id)
                            .await
                        {
                            tracing::warn!(archive = %event.archive_id, %error, "rule event not dropped");
                            return handled;
                        }
                        handled += 1;
                    }
                    Err(error) => {
                        tracing::warn!(archive = %event.archive_id, error = %error.1, "rule event waits");
                        return handled;
                    }
                }
            }
        }
    }

    /// Keeps the `schedule` and `event.upcoming` events of `(from, to]`. Idempotent: an event
    /// kept twice runs once. The error is a reason for logs, without message content.
    pub async fn tick(&self, from: Timestamp, to: Timestamp) -> Result<(), String> {
        self.keep_timed(from, to).await.map_err(|e| e.1)
    }

    async fn keep_timed(&self, from: Timestamp, to: Timestamp) -> ApiResult<()> {
        let switches = self.state.write;
        if !switches.enabled || !switches.rules {
            return Ok(());
        }
        for archive in self.state.store.list_archives().await? {
            for flow in self.state.store.list_flows(archive.id).await? {
                if flow.mode == FlowMode::Off || flow.paused.is_some() {
                    continue;
                }
                if flow.trigger != TriggerKind::Schedule.id()
                    && flow.trigger != TriggerKind::EventUpcoming.id()
                {
                    continue;
                }
                let Some(definition) = load(&self.state, &flow).await? else {
                    continue;
                };
                for event in timed_events(&self.state, &flow, &definition, from, to).await? {
                    self.state.store.put_rule_event(&event).await?;
                }
            }
        }
        Ok(())
    }

    /// Wakes the loop of [`Rules::run`] to handle new events now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Marks runs left `running` by a stopped process `interrupted`, then handles events and
    /// ticks until the process ends, pruning the runs log once a day.
    pub async fn run(self: Arc<Self>) {
        match self.state.store.interrupt_flow_runs(Timestamp::now()).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(runs = n, "rule runs interrupted by the last stop"),
            Err(error) => tracing::warn!(%error, "rule runs not interrupted"),
        }
        let mut last_prune = 0i64;
        loop {
            let now = Timestamp::now();
            let from = match self.state.store.setting(TICK_KEY).await {
                Ok(Some(text)) => text
                    .parse::<i64>()
                    .ok()
                    .map(Timestamp::from_millis)
                    .filter(|t| now.as_millis() - t.as_millis() <= MAX_CATCH_UP_MS)
                    .unwrap_or(Timestamp::from_millis(
                        now.as_millis() - TICK.as_millis() as i64,
                    )),
                _ => Timestamp::from_millis(now.as_millis() - TICK.as_millis() as i64),
            };
            match self.tick(from, now).await {
                Ok(()) => {
                    let _ = self
                        .state
                        .store
                        .set_setting(TICK_KEY, Some(&now.as_millis().to_string()))
                        .await;
                }
                Err(error) => tracing::warn!(%error, "rule schedules not checked"),
            }
            self.run_due().await;
            if now.as_millis() - last_prune > DAY_MS {
                last_prune = now.as_millis();
                let runs = Timestamp::from_millis(now.as_millis() - KEEP_RUNS.as_millis() as i64);
                let actions =
                    Timestamp::from_millis(now.as_millis() - KEEP_ACTIONS.as_millis() as i64);
                match self.state.store.prune_flow_runs(runs, actions).await {
                    Ok(n) if n > 0 => tracing::info!(runs = n, "old rule runs pruned"),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "rule runs not pruned"),
                }
            }
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }
}

async fn load(state: &ApiState, flow: &FlowRecord) -> ApiResult<Option<Flow>> {
    let Some(version) = state.store.flow_version(flow.id, flow.version).await? else {
        return Ok(None);
    };
    Ok(Flow::from_json(&version.definition).ok())
}

async fn timed_events(
    state: &ApiState,
    flow: &FlowRecord,
    definition: &Flow,
    from: Timestamp,
    to: Timestamp,
) -> ApiResult<Vec<RuleEvent>> {
    let Some(kind) = holtmail_rules::node_type(&definition.trigger.kind) else {
        return Ok(Vec::new());
    };
    let mut events = Vec::new();
    match kind.parse(&definition.trigger.params) {
        Ok(Spec::Schedule(params)) => {
            let (Ok(cron), Ok(tz)) = (
                holtmail_rules::cron::Cron::parse(&params.cron),
                params.timezone.parse::<chrono_tz::Tz>(),
            ) else {
                return Ok(Vec::new());
            };
            let mut after = chrono(from);
            while let Some(next) = cron.next_after(after, tz) {
                if next > chrono(to) {
                    break;
                }
                let at = from_chrono(next);
                events.push(RuleEvent {
                    id: format!("schedule:{}:{}", flow.id, at.as_millis()),
                    archive_id: flow.archive_id,
                    trigger: TriggerKind::Schedule.id().into(),
                    message_id: None,
                    item_id: None,
                    live: false,
                    payload: json!({ "flow": flow.id.to_string() }),
                    at,
                });
                after = next;
                if events.len() >= 100 {
                    break;
                }
            }
        }
        Ok(Spec::EventUpcoming(params)) => {
            let lead = i64::from(params.minutes_before) * 60_000;
            let start = Timestamp::from_millis(from.as_millis() + lead);
            let end = Timestamp::from_millis(to.as_millis() + lead);
            let occurrences = state
                .store
                .occurrences(&OccurrenceQuery {
                    archive_id: flow.archive_id,
                    from: start,
                    to: Timestamp::from_millis(end.as_millis() + 1),
                    collections: None,
                    removed: false,
                })
                .await?;
            let mut seen = std::collections::BTreeSet::new();
            for o in occurrences {
                let starts = o.occurrence.starts_at;
                if starts <= start || starts > end || o.occurrence.cancelled {
                    continue;
                }
                if !seen.insert((o.group_id, starts)) {
                    continue;
                }
                events.push(RuleEvent {
                    id: format!("upcoming:{}:{}:{}", flow.id, o.group_id, starts.as_millis()),
                    archive_id: flow.archive_id,
                    trigger: TriggerKind::EventUpcoming.id().into(),
                    message_id: None,
                    item_id: Some(o.item_id),
                    live: false,
                    payload: json!({ "flow": flow.id.to_string(), "starts": starts.as_millis() }),
                    at: Timestamp::from_millis(starts.as_millis() - lead),
                });
            }
        }
        _ => {}
    }
    Ok(events)
}

fn payload_flow(event: &RuleEvent) -> Option<FlowId> {
    event.payload.get("flow")?.as_str()?.parse().ok()
}

async fn handle_event(state: &ApiState, limits: &RuleLimits, event: &RuleEvent) -> ApiResult<()> {
    let switches = state.write;
    if !switches.enabled || !switches.rules {
        return Ok(());
    }
    let only = payload_flow(event);
    for flow in state.store.list_flows(event.archive_id).await? {
        if flow.mode == FlowMode::Off || flow.paused.is_some() || flow.trigger != event.trigger {
            continue;
        }
        if only.is_some_and(|f| f != flow.id) {
            continue;
        }
        let stopped = match run_flow(state, limits, &flow, event, false).await? {
            Some(outcome) => outcome.stopped,
            None => earlier_stop(state, &flow, event).await?,
        };
        if stopped {
            break;
        }
    }
    Ok(())
}

/// Whether the run this flow already had for the event ended the chain: a live run whose plan
/// stopped, or one that never recorded its plan (the process or the store failed during it), so a
/// retried event never runs flows a `stop` may have ruled out.
async fn earlier_stop(state: &ApiState, flow: &FlowRecord, event: &RuleEvent) -> ApiResult<bool> {
    let Some(run) = state.store.flow_run_for_event(flow.id, &event.id).await? else {
        return Ok(false);
    };
    if run.new.test {
        return Ok(false);
    }
    Ok(match &run.plan {
        Some(plan) => plan.get("stopped").and_then(serde_json::Value::as_bool) == Some(true),
        None => matches!(
            run.status,
            FlowRunStatus::Running | FlowRunStatus::Interrupted
        ),
    })
}

/// What one run came to.
pub(crate) struct RunOutcome {
    pub run: FlowRun,
    pub stopped: bool,
}

/// Runs one flow for one event: claims the run keyed by flow version and event id, evaluates
/// and, unless the flow is in test mode or `test` is set, carries out the plan as the user who
/// saved the flow. `None` when the flow had a run for the event already, of this version or an
/// earlier one. A test run never stops the chain.
pub(crate) async fn run_flow(
    state: &ApiState,
    limits: &RuleLimits,
    flow: &FlowRecord,
    event: &RuleEvent,
    test: bool,
) -> ApiResult<Option<RunOutcome>> {
    let Some(version) = state.store.flow_version(flow.id, flow.version).await? else {
        return Ok(None);
    };
    let message = match event.message_id {
        Some(id) => state.store.get_message(id).await?,
        None => None,
    };
    let test = test || flow.mode == FlowMode::Test;
    if state
        .store
        .flow_run_for_event(flow.id, &event.id)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    let sender = message
        .as_ref()
        .and_then(|m| m.from.first())
        .map(|a| lower(&a.email));
    let Some(claimed) = state
        .store
        .claim_flow_run(&NewFlowRun {
            flow_id: flow.id,
            archive_id: flow.archive_id,
            version: flow.version,
            event_id: event.id.clone(),
            trigger: event.trigger.clone(),
            test,
            message_id: message.as_ref().map(|m| m.id),
            sender: sender.clone(),
            started_at: Timestamp::now(),
        })
        .await?
    else {
        return Ok(None);
    };
    let end = |status, plan: Option<&Plan>, results: Vec<ActionResult>, error: Option<String>| {
        FlowRunEnd {
            status,
            plan: plan.and_then(|p| serde_json::to_value(p).ok()),
            acted: acted(&results),
            results: serde_json::to_value(results).unwrap_or_default(),
            error,
            at: Timestamp::now(),
        }
    };
    let finish = |end: FlowRunEnd| async move {
        state.store.finish_flow_run(claimed.id, &end).await?;
        let run = state
            .store
            .get_flow_run(claimed.id)
            .await?
            .ok_or_else(|| ApiError::not_found("run"))?;
        ApiResult::Ok(run)
    };
    let definition = match Flow::from_json(&version.definition) {
        Ok(definition) => definition,
        Err(_) => {
            let run = finish(end(
                FlowRunStatus::Failed,
                None,
                Vec::new(),
                Some("the stored flow does not read".into()),
            ))
            .await?;
            return Ok(Some(RunOutcome {
                run,
                stopped: false,
            }));
        }
    };
    let facts = match facts_for(
        &*state.store,
        &*state.blobs,
        flow,
        &definition,
        event,
        message.as_ref(),
    )
    .await
    {
        Ok(facts) => facts,
        Err(error) => {
            let run = finish(end(
                FlowRunStatus::Failed,
                None,
                Vec::new(),
                Some(format!("the facts could not be loaded: {}", error.1)),
            ))
            .await?;
            return Ok(Some(RunOutcome {
                run,
                stopped: false,
            }));
        }
    };
    let rule_event = Event {
        id: event.id.clone(),
        trigger: TriggerKind::from_id(&event.trigger).unwrap_or(TriggerKind::Manual),
        at: chrono(event.at),
        live: event.live,
    };
    let plan = match holtmail_rules::evaluate(&definition, &rule_event, &facts) {
        Ok(plan) => plan,
        Err(error) => {
            let run = finish(end(
                FlowRunStatus::Failed,
                None,
                Vec::new(),
                Some(error.to_string()),
            ))
            .await?;
            return Ok(Some(RunOutcome {
                run,
                stopped: false,
            }));
        }
    };
    if test {
        let results = plan
            .actions
            .iter()
            .map(|a| ActionResult::new(a, "test"))
            .collect();
        let run = finish(end(FlowRunStatus::Done, Some(&plan), results, None)).await?;
        return Ok(Some(RunOutcome {
            run,
            stopped: false,
        }));
    }
    let saver = match saver(state, flow, &definition).await {
        Ok(user) => user,
        Err(reason) => {
            let run = finish(end(
                FlowRunStatus::Failed,
                Some(&plan),
                Vec::new(),
                Some(reason),
            ))
            .await?;
            return Ok(Some(RunOutcome {
                run,
                stopped: plan.stopped,
            }));
        }
    };
    let mut results = Vec::with_capacity(plan.actions.len());
    for planned in &plan.actions {
        let result = carry_out(
            state,
            limits,
            &saver,
            flow,
            claimed.id,
            planned,
            message.as_ref(),
        )
        .await;
        results.push(result);
    }
    let stopped = plan.stopped;
    let run = finish(end(FlowRunStatus::Done, Some(&plan), results, None)).await?;
    if run.acted {
        breaker(state, limits, flow).await?;
    }
    Ok(Some(RunOutcome { run, stopped }))
}

/// Whether a run carried out at least one action.
fn acted(results: &[ActionResult]) -> bool {
    results.iter().any(|r| r.result == "done")
}

/// The user who saved the flow, while they still may use rules in its archive and, for a flow
/// that sends, still hold a `sender` grant there.
async fn saver(state: &ApiState, flow: &FlowRecord, definition: &Flow) -> Result<User, String> {
    let user = state
        .store
        .get_user(flow.saved_by)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the person who saved the flow no longer exists".to_owned())?;
    may_act(state, &user, flow.archive_id, definition)
        .await
        .map_err(|error| format!("the person who saved the flow may not run it: {error}"))?;
    Ok(user)
}

/// `user` may run `definition` in `archive` with their own rights: rules are theirs to use and,
/// when the flow sends mail, their grant allows sending. The error is a reason for people.
pub(crate) async fn may_act(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    definition: &Flow,
) -> Result<(), String> {
    access(state)
        .may_use_rules(user, &Scopes::all(), archive)
        .await
        .map_err(|error| error.to_string())?;
    if sends_mail(definition)
        && crate::write::role_in(state, user, archive)
            .await
            .map_err(|e| e.1)?
            < Some(holtmail_store::GrantRole::Sender)
    {
        return Err("the grant on this archive does not allow sending".into());
    }
    Ok(())
}

/// The flow has an action that needs the `mail.send` capability.
pub(crate) fn sends_mail(definition: &Flow) -> bool {
    holtmail_rules::capabilities(definition).contains(&holtmail_rules::Capability::MailSend)
}

async fn breaker(state: &ApiState, limits: &RuleLimits, flow: &FlowRecord) -> ApiResult<()> {
    let since = Timestamp::from_millis(Timestamp::now().as_millis() - HOUR_MS);
    let acted = state
        .store
        .count_flow_runs(flow.id, since, true, None)
        .await?;
    if acted <= limits.breaker_runs_per_hour {
        return Ok(());
    }
    if state
        .store
        .pause_flow(flow.id, "circuit_breaker", Timestamp::now())
        .await?
    {
        tracing::warn!(flow = %flow.id, archive = %flow.archive_id, runs = acted, "rule flow paused by the circuit breaker");
        state
            .rule_hooks
            .flow_paused(flow.archive_id, flow.id, "circuit_breaker")
            .await;
        if let Ok(Some(user)) = state.store.get_user(flow.saved_by).await {
            let _ = audit(
                state,
                &user,
                AuditKind::RuleChanged,
                Some(flow.archive_id),
                None,
                vec![flow.id.to_string(), "paused".into()],
            )
            .await;
        }
    }
    Ok(())
}

fn sends(action: &Action) -> Option<&str> {
    match action {
        Action::Reply { identity, .. }
        | Action::Forward { identity, .. }
        | Action::Send { identity, .. } => Some(identity),
        _ => None,
    }
}

/// Carries out one planned action as `user`, or holds it when it needs confirmation.
#[allow(clippy::too_many_arguments)]
async fn carry_out(
    state: &ApiState,
    limits: &RuleLimits,
    user: &User,
    flow: &FlowRecord,
    run: FlowRunId,
    planned: &PlannedAction,
    message: Option<&Message>,
) -> ActionResult {
    let hour = Timestamp::from_millis(Timestamp::now().as_millis() - HOUR_MS);
    if planned.needs_confirmation {
        if sends(&planned.action).is_some() {
            return match draft_for(state, user, flow.archive_id, &planned.action, message).await {
                Ok(draft) => {
                    let mut result = ActionResult::new(planned, "draft");
                    result.draft = Some(draft.id.to_string());
                    result
                }
                Err(error) => ActionResult::new(planned, "failed").because(error.1),
            };
        }
        return ActionResult::new(planned, "pending").because("waits for a person to confirm");
    }
    match state.store.count_flow_actions(flow.id, hour).await {
        Ok(n) if n >= limits.actions_per_flow_hour => {
            return ActionResult::new(planned, "skipped")
                .because("rate_limited: too many actions of this flow in the last hour");
        }
        Err(error) => return ActionResult::new(planned, "failed").because(error.to_string()),
        Ok(_) => {}
    }
    let key = format!("rule:{}:{}:{}", flow.id, run, planned.node);
    let limit = Some(limits.sends_per_identity_hour);
    match execute(
        state,
        user,
        flow,
        run,
        &planned.action,
        message,
        Some(key),
        limit,
    )
    .await
    {
        Ok(done) => {
            if done.result == "done" {
                record(state, flow, run, &planned.action, done.identity).await;
            }
            let mut result = ActionResult::new(planned, &done.result);
            result.detail = done.detail;
            result.outbox = done.outbox;
            result
        }
        Err(error) => ActionResult::new(planned, "failed").because(error.1),
    }
}

/// Keeps a carried-out action for the rate limits and the once-per-sender rule. Never fails: a
/// store error is logged with ids.
pub(crate) async fn record(
    state: &ApiState,
    flow: &FlowRecord,
    run: FlowRunId,
    action: &Action,
    identity: Option<IdentityId>,
) {
    let recipient = match action {
        Action::Reply { to, .. } => Some(lower(to)),
        _ => None,
    };
    let recorded = state
        .store
        .record_flow_action(&FlowAction {
            run_id: run,
            flow_id: flow.id,
            kind: action_kind(action).into(),
            identity,
            recipient,
            at: Timestamp::now(),
        })
        .await;
    if let Err(error) = recorded {
        tracing::warn!(flow = %flow.id, %run, %error, "rule action not recorded");
    }
}

/// What a carried-out action did.
pub(crate) struct Done {
    pub result: String,
    pub detail: Option<String>,
    pub outbox: Option<String>,
    pub identity: Option<IdentityId>,
}

impl Done {
    fn finished() -> Self {
        Self {
            result: "done".into(),
            detail: None,
            outbox: None,
            identity: None,
        }
    }

    fn skipped(detail: impl Into<String>) -> Self {
        Self {
            result: "skipped".into(),
            detail: Some(detail.into()),
            outbox: None,
            identity: None,
        }
    }
}

fn message_of<'a>(message: Option<&'a Message>, id: &str) -> ApiResult<&'a Message> {
    message
        .filter(|m| m.id.to_string() == id)
        .ok_or_else(|| ApiError::not_found("message"))
}

fn flag_name(flag: Flag) -> &'static str {
    match flag {
        Flag::Seen => "\\Seen",
        Flag::Flagged => "\\Flagged",
        Flag::Answered => "\\Answered",
    }
}

fn auto_header(auto: holtmail_rules::AutoSubmitted) -> ExtraHeader {
    ExtraHeader {
        name: "Auto-Submitted".into(),
        value: match auto {
            holtmail_rules::AutoSubmitted::AutoReplied => "auto-replied".into(),
            holtmail_rules::AutoSubmitted::AutoGenerated => "auto-generated".into(),
        },
    }
}

fn chosen(identity: &str) -> Option<String> {
    (identity != "auto").then(|| identity.to_owned())
}

/// The draft a sending action writes as the user, before it is sent or held.
async fn draft_for(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    action: &Action,
    message: Option<&Message>,
) -> ApiResult<Draft> {
    use crate::drafts::{DraftContext, DraftPayload, MailboxDto, template_draft};
    match action {
        Action::Reply {
            message: id,
            identity,
            to,
            subject,
            body,
            ..
        } => {
            let message = message_of(message, id)?;
            let (subject, body) = (subject.clone(), body.clone());
            let to = vec![MailboxDto {
                name: None,
                address: to.clone(),
            }];
            template_draft(
                state,
                user,
                message,
                None,
                ReplyKind::Sender,
                chosen(identity),
                move |payload| {
                    payload.to = to;
                    payload.subject = subject;
                    payload.text = body;
                    payload.html = None;
                    payload.cc.clear();
                },
            )
            .await
        }
        Action::Forward {
            message: id,
            identity,
            to,
            note,
            as_attachment,
            ..
        } => {
            let message = message_of(message, id)?;
            let kind = if *as_attachment {
                ForwardKind::Attachment
            } else {
                ForwardKind::Inline
            };
            let to: Vec<MailboxDto> = to
                .iter()
                .map(|a| MailboxDto {
                    name: None,
                    address: a.clone(),
                })
                .collect();
            let note = note.clone();
            template_draft(
                state,
                user,
                message,
                Some(kind),
                ReplyKind::Sender,
                chosen(identity),
                move |payload| {
                    payload.to = to;
                    if let Some(note) = note {
                        payload.text = format!("{note}\n\n{}", payload.text);
                        payload.html = None;
                    }
                },
            )
            .await
        }
        Action::Send {
            identity,
            to,
            subject,
            body,
            ..
        } => {
            let payload = DraftPayload {
                identity: chosen(identity),
                to: to
                    .iter()
                    .map(|a| MailboxDto {
                        name: None,
                        address: a.clone(),
                    })
                    .collect(),
                subject: subject.clone(),
                text: body.clone(),
                context: DraftContext {
                    kind: "new".into(),
                    ..DraftContext::default()
                },
                ..DraftPayload::default()
            };
            Ok(state
                .store
                .create_draft(
                    archive,
                    user.id,
                    &serde_json::to_value(&payload)?,
                    &[],
                    Timestamp::now(),
                )
                .await?)
        }
        _ => Err(ApiError::bad_request("not a sending action")),
    }
}

/// Carries out one action as `user`, through the same paths as the API's own endpoints. With
/// `sends_per_hour`, a message is not queued when flows sent that many as the routed identity in
/// the last hour (`skipped`, `rate_limited`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute(
    state: &ApiState,
    user: &User,
    flow: &FlowRecord,
    run: FlowRunId,
    action: &Action,
    message: Option<&Message>,
    key: Option<String>,
    sends_per_hour: Option<u64>,
) -> ApiResult<Done> {
    let scopes = Scopes::all();
    match action {
        Action::Label { message: id, name } => {
            let message = message_of(message, id)?;
            let mut labels = state.store.labels(message.id).await?;
            if labels.iter().any(|l| l == name) {
                return Ok(Done::skipped("the message has the label already"));
            }
            labels.push(name.clone());
            state.store.set_labels(message.id, &labels).await?;
            Ok(Done::finished())
        }
        Action::SetFlags {
            message: id,
            set,
            clear,
        } => {
            let message = message_of(message, id)?;
            let op = crate::mail_changes::MailChangeOp::Flags {
                add: set.iter().map(|f| flag_name(*f).to_owned()).collect(),
                remove: clear.iter().map(|f| flag_name(*f).to_owned()).collect(),
            };
            mail_change(state, user, &scopes, message, &op, None).await
        }
        Action::Move {
            message: id,
            folder,
        } => {
            let message = message_of(message, id)?;
            let (account, target) = target_folder(state, message, folder).await?;
            let op = crate::mail_changes::MailChangeOp::Move {
                folder: target.to_string(),
            };
            mail_change(
                state,
                user,
                &scopes,
                message,
                &op,
                Some(account.to_string()),
            )
            .await
        }
        Action::Delete { message: id } => {
            let message = message_of(message, id)?;
            let op = crate::mail_changes::MailChangeOp::Delete { expunge: false };
            mail_change(state, user, &scopes, message, &op, None).await
        }
        Action::Reply { auto_submitted, .. }
        | Action::Forward { auto_submitted, .. }
        | Action::Send { auto_submitted, .. } => {
            let archive = message.map(|m| m.archive_id);
            if let Some(archive) = archive {
                crate::outbox::may_send_somehow(state, user, &scopes, archive).await?;
            }
            let draft = draft_for(state, user, flow.archive_id, action, message).await?;
            crate::outbox::may_send_somehow(state, user, &scopes, draft.archive_id).await?;
            let sent = send_stored_draft(
                state,
                user,
                &scopes,
                &draft,
                SendOptions {
                    allow_leaks: false,
                    send_at: None,
                    key,
                    headers: vec![auto_header(*auto_submitted)],
                    routing: RouteOverrides::default(),
                    identity_limit: sends_per_hour.map(|max| crate::outbox::IdentityLimit {
                        max,
                        since: Timestamp::from_millis(Timestamp::now().as_millis() - HOUR_MS),
                    }),
                },
            )
            .await;
            match sent {
                Ok(Sent::Queued(entry)) => Ok(Done {
                    result: "done".into(),
                    detail: None,
                    outbox: Some(entry.id.to_string()),
                    identity: Some(entry.new.identity_id),
                }),
                Ok(Sent::Leaks(_)) => Ok(Done {
                    result: "draft".into(),
                    detail: Some(format!(
                        "recipients would see the real mailbox behind the alias; the draft {} waits for a person",
                        draft.id
                    )),
                    outbox: None,
                    identity: None,
                }),
                Ok(Sent::RateLimited) => {
                    let _ = state.store.delete_draft(draft.id).await;
                    Ok(Done::skipped(
                        "rate_limited: too many messages from this identity in the last hour",
                    ))
                }
                Err(error) => {
                    let _ = state.store.delete_draft(draft.id).await;
                    Err(error)
                }
            }
        }
        Action::Rsvp {
            message: id,
            answer,
            comment,
            ..
        } => {
            let Some(id) = id else {
                return Ok(Done::skipped("no message carries the invitation"));
            };
            let message = message_of(message, id)?.clone();
            let answer = match answer {
                Answer::Accepted => "accepted",
                Answer::Tentative => "tentative",
                Answer::Declined => "declined",
            };
            let done = crate::invitations::answer_message(
                state,
                user,
                &scopes,
                message,
                answer,
                comment.clone(),
            )
            .await?;
            Ok(Done {
                result: "done".into(),
                detail: Some(format!("answered through {}", done.route)),
                outbox: done.outbox.map(|o| o.id),
                identity: None,
            })
        }
        Action::CreateEvent {
            calendar,
            title,
            starts,
            duration_minutes,
        } => {
            let archive = flow.archive_id;
            let end = *starts + chrono::Duration::minutes(i64::from(*duration_minutes));
            let request = crate::pim_edit::ItemContentRequest {
                event: Some(crate::pim_edit::EventFormDto {
                    title: title.clone(),
                    start: starts.format("%Y-%m-%dT%H:%M:%S").to_string(),
                    end: Some(end.format("%Y-%m-%dT%H:%M:%S").to_string()),
                    time_zone: Some("UTC".into()),
                    ..crate::pim_edit::EventFormDto::default()
                }),
                invitations: Some(false),
                ..crate::pim_edit::ItemContentRequest::default()
            };
            crate::pim_edit::create_for_rule(
                state,
                user,
                &scopes,
                archive,
                CollectionKind::Calendar,
                calendar.as_deref(),
                &request,
            )
            .await?;
            Ok(Done::finished())
        }
        Action::CreateContact {
            address_book,
            name,
            address,
        } => {
            let request = crate::pim_edit::ItemContentRequest {
                contact: Some(crate::pim_edit::ContactFormDto {
                    full_name: name.clone(),
                    emails: vec![crate::pim_edit::LabeledDto {
                        value: address.clone(),
                        kinds: Vec::new(),
                    }],
                    ..crate::pim_edit::ContactFormDto::default()
                }),
                ..crate::pim_edit::ItemContentRequest::default()
            };
            crate::pim_edit::create_for_rule(
                state,
                user,
                &scopes,
                flow.archive_id,
                CollectionKind::Contacts,
                address_book.as_deref(),
                &request,
            )
            .await?;
            Ok(Done::finished())
        }
        Action::Notify { .. } => {
            state
                .rule_hooks
                .notified(flow.archive_id, flow.id, run)
                .await;
            Ok(Done::finished())
        }
        Action::Webhook { hook } => {
            state
                .rule_hooks
                .call(hook, flow.archive_id, flow.id, run, message.map(|m| m.id))
                .await
                .map_err(ApiError::bad_request)?;
            Ok(Done::finished())
        }
        Action::SetIdentity { .. }
        | Action::SetTransport { .. }
        | Action::SetAliasStrategy { .. }
        | Action::AddBcc { .. }
        | Action::RequireConfirmation { .. }
        | Action::Block { .. } => Ok(Done::skipped("only changes a message being sent")),
    }
}

async fn mail_change(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    message: &Message,
    op: &crate::mail_changes::MailChangeOp,
    account: Option<String>,
) -> ApiResult<Done> {
    let done = crate::mail_changes::change_messages(
        state,
        user,
        scopes,
        vec![message.id],
        op,
        account.as_deref(),
    )
    .await?;
    if done.changes.is_empty() {
        return Ok(Done::skipped(
            done.skipped
                .first()
                .map(|s| s.reason.clone())
                .unwrap_or_else(|| "nothing to change".into()),
        ));
    }
    Ok(Done {
        result: "done".into(),
        detail: Some(format!(
            "{} changes held for the server",
            done.changes.len()
        )),
        outbox: None,
        identity: None,
    })
}

/// The folder a `move` names, by path (exactly, then ignoring case) or by role, in an account
/// that holds the message.
async fn target_folder(
    state: &ApiState,
    message: &Message,
    name: &str,
) -> ApiResult<(holtmail_core::AccountId, holtmail_core::FolderId)> {
    let mut accounts = Vec::new();
    for location in state.store.locations(message.id).await? {
        if location.removed_at.is_some() {
            continue;
        }
        if let Some(folder) = state.store.get_folder(location.folder_id).await?
            && !accounts.contains(&folder.account_id)
        {
            accounts.push(folder.account_id);
        }
    }
    let all: Vec<holtmail_store::Folder> = state
        .store
        .list_folders(message.archive_id)
        .await?
        .into_iter()
        .map(|f| f.folder)
        .collect();
    for account in accounts {
        let folders: Vec<&holtmail_store::Folder> =
            all.iter().filter(|f| f.account_id == account).collect();
        let found = folders
            .iter()
            .find(|f| f.path == name)
            .or_else(|| folders.iter().find(|f| f.path.eq_ignore_ascii_case(name)))
            .or_else(|| {
                folders.iter().find(|f| {
                    f.role
                        .is_some_and(|r| r.as_str().eq_ignore_ascii_case(name))
                })
            });
        if let Some(folder) = found {
            return Ok((account, folder.id));
        }
    }
    Err(ApiError::not_found("folder"))
}

/// The invitation a message carries: the first `REQUEST` calendar part's first event.
fn invitation(parsed: &ParsedMessage) -> Option<EventFacts> {
    parsed.calendars.iter().find_map(|bytes| {
        let method = holtmail_pim::calendar_method(bytes)?;
        if !method.eq_ignore_ascii_case("REQUEST") {
            return None;
        }
        let read = holtmail_pim::read(holtmail_pim::ItemKind::Event, bytes);
        let holtmail_pim::Normalized::Calendar(object) = read.normalized else {
            return None;
        };
        let event = object.components.into_iter().next()?;
        Some(EventFacts {
            uid: event.uid.unwrap_or_default(),
            summary: event.summary,
            starts: event
                .start
                .and_then(|s| s.utc)
                .and_then(DateTime::from_timestamp_millis),
            location: event.location,
            organizer: event.organizer.and_then(|o| {
                Some(Mailbox {
                    name: o.name,
                    address: o.email?,
                })
            }),
        })
    })
}

fn trust_of(state: &holtmail_category::TrustState) -> Trust {
    match state {
        holtmail_category::TrustState::Aligned { .. } => Trust::Aligned,
        holtmail_category::TrustState::Relayed { .. } => Trust::Relayed,
        holtmail_category::TrustState::Unverified => Trust::Unverified,
        holtmail_category::TrustState::Unknown => Trust::Unknown,
    }
}

/// Everything a flow may look at for one message, loaded from the store and the raw bytes.
pub(crate) struct Loaded {
    pub sheet: MessageFacts,
    pub query: holtmail_query::MessageFacts,
    pub invitation: Option<EventFacts>,
}

/// The archive's own addresses, lowercase: identities, accounts and aliases.
async fn own_addresses(store: &dyn Store, archive: ArchiveId) -> ApiResult<Vec<String>> {
    let mut own: Vec<String> = store
        .list_identities(archive)
        .await?
        .into_iter()
        .map(|i| i.fields.address)
        .collect();
    own.extend(
        store
            .list_accounts(archive)
            .await?
            .into_iter()
            .filter_map(|a| a.address),
    );
    own.extend(store.own_alias_addresses(archive).await?);
    Ok(own.iter().map(|a| lower(a)).collect())
}

/// Loads a message's facts.
pub(crate) async fn message_facts(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    message: &Message,
) -> ApiResult<Loaded> {
    let raw = blobs.get(&message.blob_id).await?;
    let parsed = raw.as_deref().and_then(|r| holtmail_mime::parse(r).ok());
    let alias = store.message_alias(message.id).await?;
    let view = store.insight_view(message.id).await?;
    let mut folders = Vec::new();
    let mut flags = Vec::new();
    let mut accounts = Vec::new();
    for location in store.locations(message.id).await? {
        if location.removed_at.is_some() {
            continue;
        }
        if let Some(folder) = store.get_folder(location.folder_id).await? {
            folders.push(folder.path);
            if let Some(role) = folder.role {
                folders.push(role.as_str().to_owned());
            }
            if !accounts.contains(&folder.account_id) {
                accounts.push(folder.account_id);
            }
        }
        flags.extend(location.flags);
    }
    folders.sort();
    folders.dedup();
    flags.sort();
    flags.dedup();
    let headers: Vec<(String, String)> = parsed
        .as_ref()
        .map(|p| {
            p.headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let (insight_kinds, insights_known) = match &view {
        Some(view) => (
            KindTags::from_bits(view.insights.kinds)
                .iter()
                .map(|t| t.name().to_owned())
                .collect::<Vec<_>>(),
            true,
        ),
        None => (Vec::new(), false),
    };
    let categories = view.as_ref().and_then(|v| v.categories.as_ref()).map(|c| {
        std::iter::once(format!("kind:{}", c.row.kind.name()))
            .chain(c.row.topics.iter().map(|t| format!("topic:{}", t.name())))
            .collect::<Vec<_>>()
    });
    let trust = view
        .as_ref()
        .and_then(|v| v.categories.as_ref())
        .map(|c| trust_of(&c.row.write.trust));
    let from: Vec<holtmail_core::Address> = message
        .from
        .iter()
        .chain(alias.as_ref().and_then(|a| a.sender.as_ref()))
        .cloned()
        .collect();
    let sender_address = alias
        .as_ref()
        .and_then(|a| a.sender.as_ref())
        .or(message.from.first());
    let sender_is_person = match sender_address {
        Some(a) => !store
            .people_by_email(message.archive_id, std::slice::from_ref(&a.email))
            .await?
            .is_empty(),
        None => false,
    };
    let query = holtmail_query::MessageFacts {
        message_id: message.id,
        thread_id: message.thread_id,
        accounts,
        subject: message.subject.clone().unwrap_or_default(),
        from,
        to: message.to.clone(),
        cc: message.cc.clone(),
        bcc: message.bcc.clone(),
        headers: headers.clone(),
        body: parsed.as_ref().map(|p| p.text.clone().unwrap_or_default()),
        attachments: parsed
            .as_ref()
            .map(|p| {
                p.attachments
                    .iter()
                    .map(|a| AttachmentFacts {
                        filename: a.filename.clone(),
                        content_type: a.content_type.clone(),
                        size: a.content.len() as u64,
                        inline: a.inline,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        attachment_text: attachment_text(store, blobs, message.id).await?,
        folders,
        flags,
        labels: store.labels(message.id).await?,
        date: message.date,
        size: message.size,
        insights: insights_known.then(|| insight_kinds.clone()),
        categories,
    };
    let sheet = MessageFacts {
        id: message.id.to_string(),
        sender: sender_address.map(|a| Mailbox {
            name: a.name.clone(),
            address: a.email.clone(),
        }),
        subject: message.subject.clone(),
        date: Some(chrono(message.date)),
        headers,
        alias: alias.as_ref().map(|a| AliasFact {
            address: a.alias.clone(),
            service: Some(a.provider.clone()),
        }),
        trust,
        insight_kinds,
        insights: view.as_ref().map(insight_fields).unwrap_or_default(),
        sender_is_person,
    };
    Ok(Loaded {
        sheet,
        query,
        invitation: parsed.as_ref().and_then(invitation),
    })
}

const MAX_ATTACHMENT_TEXT: usize = 4 << 20;

/// The text ingest extracted from the message's attachments, as the index has it; an empty text
/// without attachments, `None` while an attachment's text is not extracted yet.
async fn attachment_text(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    message: MessageId,
) -> ApiResult<Option<String>> {
    let mut out = String::new();
    for attachment in store.attachments(message).await? {
        if attachment.inline || out.len() >= MAX_ATTACHMENT_TEXT {
            continue;
        }
        let text = match store
            .derived(&attachment.blob_id, holtmail_store::DerivedKind::Text)
            .await?
        {
            None => return Ok(None),
            Some(holtmail_store::Derived::Nothing) => continue,
            Some(holtmail_store::Derived::Blob(blob)) => match blobs.get(&blob).await? {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => return Ok(None),
            },
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&text);
    }
    if out.len() > MAX_ATTACHMENT_TEXT {
        let mut end = MAX_ATTACHMENT_TEXT;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    Ok(Some(out))
}

const TEMPLATE_KINDS: [&str; 5] = ["order", "invoice", "parcel", "reservation", "pass"];
const MAX_INSIGHT_VALUE: usize = 500;

/// The insight values offered to templates: for the first shown item of each of order, invoice,
/// parcel, reservation and pass, its plain values as `<kind>_<field>` (nested ones joined by
/// `_`). Codes and links are never offered.
fn insight_fields(view: &holtmail_store::InsightView) -> BTreeMap<String, String> {
    fn flatten(
        prefix: &str,
        value: &serde_json::Value,
        depth: u8,
        out: &mut BTreeMap<String, String>,
    ) {
        match value {
            serde_json::Value::Object(map) if depth < 3 => {
                for (key, value) in map {
                    flatten(&format!("{prefix}_{key}"), value, depth + 1, out);
                }
            }
            serde_json::Value::String(text) => {
                out.entry(prefix.to_owned())
                    .or_insert_with(|| text.chars().take(MAX_INSIGHT_VALUE).collect());
            }
            serde_json::Value::Number(n) => {
                out.entry(prefix.to_owned())
                    .or_insert_with(|| n.to_string());
            }
            serde_json::Value::Bool(b) => {
                out.entry(prefix.to_owned())
                    .or_insert_with(|| b.to_string());
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    let Ok(read) = holtmail_insight::Items::from_json(&view.insights.items) else {
        return out;
    };
    let mut seen = Vec::new();
    for item in read.items.items {
        if item.forwarded || item.confidence < holtmail_insight::Confidence::Likely {
            continue;
        }
        let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(&item.kind) else {
            continue;
        };
        let Some(kind) = fields
            .remove("type")
            .and_then(|t| t.as_str().map(str::to_owned))
        else {
            continue;
        };
        if !TEMPLATE_KINDS.contains(&kind.as_str()) || seen.contains(&kind) {
            continue;
        }
        let fields = match fields.remove("r") {
            Some(serde_json::Value::Object(inner)) if kind == "reservation" => inner,
            _ => fields,
        };
        for (key, value) in &fields {
            flatten(&format!("{kind}_{key}"), value, 0, &mut out);
        }
        seen.push(kind);
    }
    out
}

/// The facts of one event for one flow: the message's, the upcoming event's, the counters the
/// flow reads and its last reply to the sender.
pub(crate) async fn facts_for(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    flow: &FlowRecord,
    definition: &Flow,
    event: &RuleEvent,
    message: Option<&Message>,
) -> ApiResult<impl Facts + use<>> {
    let mut sheet = FactSheet {
        own_addresses: own_addresses(store, flow.archive_id).await?,
        ..FactSheet::default()
    };
    let mut query = None;
    if let Some(message) = message {
        let loaded = message_facts(store, blobs, message).await?;
        sheet.invitation = loaded.invitation;
        query = Some(loaded.query);
        sheet.message = Some(loaded.sheet);
    }
    if let Some(item) = event.item_id {
        sheet.event = upcoming(store, item, event).await?;
    }
    let sender = sheet
        .message
        .as_ref()
        .and_then(|m| m.sender.as_ref())
        .map(|s| lower(&s.address));
    for (scope, window) in holtmail_rules::counters_used(definition) {
        let span = match window {
            CounterWindow::Hour => HOUR_MS,
            CounterWindow::Day => DAY_MS,
            CounterWindow::Week => 7 * DAY_MS,
        };
        let since = Timestamp::from_millis(event.at.as_millis() - span);
        let count = match (scope, &sender) {
            (CounterScope::Sender, Some(sender)) => {
                store
                    .count_messages_from(flow.archive_id, sender, since, message.map(|m| m.id))
                    .await?
            }
            (CounterScope::Sender, None) => 0,
            (CounterScope::Flow, _) => store.count_flow_runs(flow.id, since, true, None).await?,
        };
        sheet.counters.push(CounterFact {
            scope,
            window,
            count,
        });
    }
    if let Some(sender) = &sender {
        sheet.last_reply = store.last_flow_reply(flow.id, sender).await?.map(chrono);
    }
    Ok(WithMatcher(sheet, move |text: &str| {
        let Some(facts) = &query else {
            return Err(FactsError::Unavailable("message".into()));
        };
        let matcher = Matcher::parse(text).map_err(|e| FactsError::Query(e.to_string()))?;
        Ok(match matcher.test(facts) {
            Verdict::Match => true,
            Verdict::NoMatch => false,
            Verdict::Unknown => {
                tracing::info!(message = %facts.message_id, "a rule query needs facts not derived yet; taken as no match");
                false
            }
        })
    }))
}

async fn upcoming(
    store: &dyn Store,
    item: ItemId,
    event: &RuleEvent,
) -> ApiResult<Option<EventFacts>> {
    let Some(summary) = store.get_item(item).await? else {
        return Ok(None);
    };
    let starts = event
        .payload
        .get("starts")
        .and_then(serde_json::Value::as_i64)
        .and_then(DateTime::from_timestamp_millis);
    let first = summary.index.as_ref().and_then(|i| {
        match serde_json::from_value::<holtmail_pim::Normalized>(i.data.clone()).ok()? {
            holtmail_pim::Normalized::Calendar(object) => object.components.into_iter().next(),
            _ => None,
        }
    });
    let organizer = first
        .as_ref()
        .and_then(|e| e.organizer.as_ref())
        .and_then(|o| {
            Some(Mailbox {
                name: o.name.clone(),
                address: o.email.clone()?,
            })
        })
        .or_else(|| {
            summary
                .index
                .as_ref()
                .and_then(|i| i.organizer.clone())
                .map(|address| Mailbox {
                    name: None,
                    address,
                })
        });
    Ok(Some(EventFacts {
        uid: summary
            .index
            .as_ref()
            .and_then(|i| i.uid.clone())
            .unwrap_or_else(|| item.to_string()),
        summary: summary.index.as_ref().and_then(|i| i.title.clone()),
        starts,
        location: first.and_then(|e| e.location),
        organizer,
    }))
}

/// What `user`'s own `message.sending` flows ask of a draft they are about to send: only flows
/// this user saved apply, and only while the user may still run them (rules, and a `sender`
/// grant for flows that change the route). Flows in test mode are logged only and never end the
/// chain; the decision is the same for the same draft version, so a send repeated after a
/// confirmation gets it again. An error in one flow is logged and that flow is left out, so a
/// broken rule never stops a person from sending. A rule that adds a blind copy or changes the
/// identity or transport is written to the audit log.
pub(crate) async fn sending_decision(
    state: &ApiState,
    user: &User,
    draft: &Draft,
) -> ApiResult<RouteOverrides> {
    let mut overrides = RouteOverrides::default();
    let switches = state.write;
    if !switches.enabled || !switches.rules {
        return Ok(overrides);
    }
    let flows: Vec<FlowRecord> = state
        .store
        .list_flows(draft.archive_id)
        .await?
        .into_iter()
        .filter(|f| {
            f.mode != FlowMode::Off
                && f.paused.is_none()
                && f.saved_by == user.id
                && f.trigger == TriggerKind::MessageSending.id()
        })
        .collect();
    if flows.is_empty() {
        return Ok(overrides);
    }
    let payload = crate::drafts::stored_payload(state, &draft.current).await?;
    let event = RuleEvent {
        id: format!("sending:{}:{}", draft.id, draft.current.version),
        archive_id: draft.archive_id,
        trigger: TriggerKind::MessageSending.id().into(),
        message_id: None,
        item_id: None,
        live: true,
        payload: json!({}),
        at: Timestamp::now(),
    };
    let identity = match payload.identity.as_deref().and_then(|i| i.parse().ok()) {
        Some(id) => state.store.get_identity(id).await?,
        None => match state
            .store
            .default_identity(user.id, draft.archive_id)
            .await?
        {
            Some(id) => state.store.get_identity(id).await?,
            None => None,
        },
    };
    let address = |m: &crate::drafts::MailboxDto| holtmail_core::Address {
        name: m.name.clone(),
        email: m.address.clone(),
    };
    let from = identity
        .as_ref()
        .map(|i| holtmail_core::Address {
            name: i.fields.name.clone(),
            email: i.fields.address.clone(),
        })
        .into_iter()
        .collect::<Vec<_>>();
    let query = holtmail_query::MessageFacts {
        message_id: MessageId::new(),
        thread_id: holtmail_core::ThreadId::new(),
        accounts: Vec::new(),
        subject: payload.subject.clone(),
        from: from.clone(),
        to: payload.to.iter().map(address).collect(),
        cc: payload.cc.iter().map(address).collect(),
        bcc: payload.bcc.iter().map(address).collect(),
        headers: Vec::new(),
        body: Some(payload.text.clone()),
        attachments: Vec::new(),
        attachment_text: Some(String::new()),
        folders: Vec::new(),
        flags: Vec::new(),
        labels: Vec::new(),
        date: Timestamp::now(),
        size: 0,
        insights: Some(Vec::new()),
        categories: Some(Vec::new()),
    };
    let sheet = FactSheet {
        message: Some(MessageFacts {
            id: format!("draft:{}", draft.id),
            sender: from.first().map(|a| Mailbox {
                name: a.name.clone(),
                address: a.email.clone(),
            }),
            subject: Some(payload.subject.clone()),
            date: Some(Utc::now()),
            ..MessageFacts::default()
        }),
        own_addresses: own_addresses(&*state.store, draft.archive_id).await?,
        ..FactSheet::default()
    };
    let rule_event = Event {
        id: event.id.clone(),
        trigger: TriggerKind::MessageSending,
        at: Utc::now(),
        live: true,
    };
    let mut audited = Vec::new();
    for flow in flows {
        let Some(definition) = load(state, &flow).await? else {
            continue;
        };
        if let Err(reason) = may_act(state, user, flow.archive_id, &definition).await {
            tracing::info!(flow = %flow.id, %reason, "a sending rule is left out");
            continue;
        }
        let facts = WithMatcher(sheet.clone(), |text: &str| {
            let matcher = Matcher::parse(text).map_err(|e| FactsError::Query(e.to_string()))?;
            Ok(match matcher.test(&query) {
                Verdict::Match => true,
                Verdict::NoMatch => false,
                Verdict::Unknown => {
                    tracing::info!(draft = %draft.id, "a rule query needs facts a draft lacks; taken as no match");
                    false
                }
            })
        });
        let plan = holtmail_rules::evaluate(&definition, &rule_event, &facts);
        let test = flow.mode == FlowMode::Test;
        let claimed = state
            .store
            .claim_flow_run(&NewFlowRun {
                flow_id: flow.id,
                archive_id: flow.archive_id,
                version: flow.version,
                event_id: event.id.clone(),
                trigger: event.trigger.clone(),
                test,
                message_id: None,
                sender: None,
                started_at: Timestamp::now(),
            })
            .await?;
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                if let Some(run) = claimed {
                    state
                        .store
                        .finish_flow_run(
                            run.id,
                            &FlowRunEnd {
                                status: FlowRunStatus::Failed,
                                plan: None,
                                results: json!([]),
                                error: Some(error.to_string()),
                                acted: false,
                                at: Timestamp::now(),
                            },
                        )
                        .await?;
                }
                continue;
            }
        };
        let mut results = Vec::new();
        for planned in &plan.actions {
            if test {
                results.push(ActionResult::new(planned, "test"));
                continue;
            }
            match &planned.action {
                Action::SetIdentity { identity } => {
                    overrides.identity = Some(identity.clone());
                    audited.push(format!("{}:set_identity", flow.id));
                }
                Action::SetTransport { transport } => {
                    overrides.transport = Some(transport.clone());
                    audited.push(format!("{}:set_transport", flow.id));
                }
                Action::SetAliasStrategy { strategy } => {
                    overrides.strategy = Some(match strategy {
                        Strategy::Direct => holtmail_route::Strategy::Direct,
                        Strategy::ReverseAlias => holtmail_route::Strategy::ReverseAlias,
                        Strategy::ReplyHeader => holtmail_route::Strategy::ReplyHeader,
                    })
                }
                Action::AddBcc { address } => {
                    overrides.bcc.push(address.clone());
                    audited.push(format!("{}:add_bcc", flow.id));
                }
                Action::RequireConfirmation { reason } => {
                    overrides.confirm.get_or_insert_with(|| reason.clone());
                }
                Action::Block { reason } => {
                    overrides.block.get_or_insert_with(|| reason.clone());
                }
                _ => {
                    results.push(
                        ActionResult::new(planned, "skipped")
                            .because("only routing actions run on message.sending"),
                    );
                    continue;
                }
            }
            results.push(ActionResult::new(planned, "done"));
        }
        if let Some(run) = claimed {
            state
                .store
                .finish_flow_run(
                    run.id,
                    &FlowRunEnd {
                        status: FlowRunStatus::Done,
                        plan: serde_json::to_value(&plan).ok(),
                        results: serde_json::to_value(&results).unwrap_or_default(),
                        error: None,
                        acted: acted(&results),
                        at: Timestamp::now(),
                    },
                )
                .await?;
        }
        if !test && (plan.stopped || overrides.block.is_some()) {
            break;
        }
    }
    if !audited.is_empty() {
        let mut targets = vec![draft.id.to_string(), "rule".into()];
        targets.extend(audited);
        audit(
            state,
            user,
            AuditKind::MailSend,
            Some(draft.archive_id),
            None,
            targets,
        )
        .await?;
    }
    Ok(overrides)
}

/// A dry run: what `definition` would plan for one event about `message` (or none), with the
/// facts and counters of the stored flow `flow`. Nothing is carried out or logged. The error is
/// a reason for people, without message content.
pub async fn dry_run(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    flow: &FlowRecord,
    definition: &Flow,
    message: Option<&Message>,
    live: bool,
    at: Timestamp,
) -> Result<Plan, String> {
    let trigger = TriggerKind::from_id(&definition.trigger.kind).unwrap_or(TriggerKind::Manual);
    let event = RuleEvent {
        id: format!("test:{}", MessageId::new()),
        archive_id: flow.archive_id,
        trigger: trigger.id().into(),
        message_id: message.map(|m| m.id),
        item_id: None,
        live,
        payload: json!({}),
        at,
    };
    let facts = facts_for(store, blobs, flow, definition, &event, message)
        .await
        .map_err(|e| e.1)?;
    let rule_event = Event {
        id: event.id.clone(),
        trigger,
        at: chrono(at),
        live,
    };
    holtmail_rules::evaluate(definition, &rule_event, &facts).map_err(|e| e.to_string())
}

/// What a template renders to for a message, and the context it saw; the error when it does not
/// render. Uses the same facts and context as a run of a flow with this trigger.
pub async fn preview_template(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    flow: &FlowRecord,
    trigger: TriggerKind,
    message: Option<&Message>,
    at: Timestamp,
    template: &str,
) -> ApiResult<(TemplateContext, Result<String, String>)> {
    let definition = Flow {
        format: holtmail_rules::FLOW_FORMAT,
        trigger: holtmail_rules::TriggerNode {
            kind: trigger.id().into(),
            version: 1,
            params: json!({}),
            next: None,
            position: None,
        },
        nodes: Vec::new(),
    };
    let event = RuleEvent {
        id: format!("preview:{}", MessageId::new()),
        archive_id: flow.archive_id,
        trigger: trigger.id().into(),
        message_id: message.map(|m| m.id),
        item_id: None,
        live: true,
        payload: json!({}),
        at,
    };
    let facts = facts_for(store, blobs, flow, &definition, &event, message).await?;
    let rule_event = Event {
        id: event.id.clone(),
        trigger,
        at: chrono(at),
        live: true,
    };
    let context = TemplateContext::new(&rule_event, facts.sheet());
    let text = holtmail_rules::template::render(template, &context).map_err(|e| e.to_string());
    Ok((context, text))
}

#[cfg(test)]
mod tests {
    use holtmail_insight::{Confidence, Insight, InsightKind, Items, PartRef, Provenance};

    use super::*;

    fn item(kind: InsightKind, forwarded: bool) -> Insight {
        Insight {
            kind,
            provenance: Provenance::JsonLd,
            confidence: Confidence::Certain,
            score: 1000,
            origin: PartRef::Html,
            forwarded,
        }
    }

    #[test]
    fn templates_see_plain_insight_values_but_never_codes() {
        let order = |number: &str| InsightKind::Order {
            number: Some(number.into()),
            merchant: Some("Shop".into()),
            total: None,
            status: None,
            url: None,
        };
        let items = Items {
            items: vec![
                item(order("A-1"), true),
                item(
                    InsightKind::Code {
                        value: "123456".into(),
                        origin_bound: None,
                    },
                    false,
                ),
                item(order("B-2"), false),
                item(order("C-3"), false),
            ],
        };
        let view = holtmail_store::InsightView {
            insights: holtmail_store::InsightRow {
                message: MessageId::new(),
                archive: ArchiveId::new(),
                fingerprint: "fp".into(),
                outcome: holtmail_store::InsightOutcome::Ok,
                truncated: false,
                from_address: None,
                signals: 0,
                kinds: 0,
                auth: "{}".into(),
                items: items.to_json(),
                computed_at: Timestamp::now(),
            },
            categories: None,
        };
        let fields = insight_fields(&view);
        assert_eq!(fields.get("order_number").map(String::as_str), Some("B-2"));
        assert_eq!(
            fields.get("order_merchant").map(String::as_str),
            Some("Shop")
        );
        assert!(fields.keys().all(|k| k.starts_with("order_")), "{fields:?}");
    }
}
