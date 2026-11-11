use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveTime;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::catalogue::{Capability, NodeType, Role, Spec, TriggerKind, node_type};
use crate::cron::Cron;
use crate::flow::{FLOW_FORMAT, Flow, MAX_NODES};
use crate::params::{CounterScope, CounterWindow};
use crate::template;

/// Stable codes for what is wrong with a flow; the interface translates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// The flow's format is newer than this build reads.
    Format,
    /// More than [`MAX_NODES`] nodes.
    TooManyNodes,
    /// A node id is empty, too long, uses other characters or is `trigger`.
    BadId,
    /// Two nodes share an id.
    DuplicateId,
    /// The type is not in the catalogue.
    UnknownType,
    /// The trigger's type is not a trigger.
    NotATrigger,
    /// A trigger type used as a node.
    TriggerInGraph,
    /// The parameters are newer than the catalogue's version of the type.
    UnsupportedVersion,
    /// The parameters do not fit the type's schema or its limits.
    BadParams,
    /// The node cannot follow the flow's trigger.
    NotForTrigger,
    /// `next` names a port the type does not have.
    UnknownPort,
    /// `next` names a node that does not exist.
    UnknownTarget,
    /// Following `next` comes back to a node.
    Cycle,
    /// A template does not parse or is too long.
    BadTemplate,
    /// The cron expression does not parse.
    BadCron,
    /// The time zone is not an IANA name.
    BadTimezone,
    /// A time of day is not `HH:MM`.
    BadTime,
    /// `confirm` on a node that changes and sends nothing.
    ConfirmNotAllowed,
}

/// One problem with a flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// The node id, `trigger` for the trigger, absent for the flow as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// What is wrong.
    pub code: IssueCode,
    /// Details for people; never contains message content.
    pub detail: String,
}

pub(crate) struct Compiled<'a> {
    pub trigger: TriggerKind,
    pub start: Option<&'a str>,
    pub nodes: HashMap<&'a str, CompiledNode<'a>>,
}

pub(crate) struct CompiledNode<'a> {
    pub ty: &'static NodeType,
    pub spec: Spec,
    pub next: &'a BTreeMap<String, String>,
    pub confirm: bool,
}

struct Issues(Vec<Issue>);

impl Issues {
    fn push(&mut self, node: Option<&str>, code: IssueCode, detail: impl Into<String>) {
        self.0.push(Issue {
            node: node.map(str::to_owned),
            code,
            detail: detail.into(),
        });
    }
}

/// Checks a flow against the catalogue: the format, ids, types and versions, parameters with
/// their schema and limits, templates, cron and time zones, which nodes may follow the
/// trigger, ports and targets, and that no path loops. Every issue is reported, in flow
/// order; an empty list means [`crate::evaluate`] accepts the flow. Nodes no path reaches
/// are allowed, so a flow can be saved half drawn.
pub fn validate(flow: &Flow) -> Result<(), Vec<Issue>> {
    compile(flow).map(|_| ())
}

fn good_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id != "trigger"
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) fn compile(flow: &Flow) -> Result<Compiled<'_>, Vec<Issue>> {
    let mut issues = Issues(Vec::new());
    if flow.format > FLOW_FORMAT {
        issues.push(
            None,
            IssueCode::Format,
            format!("format {} is newer than {FLOW_FORMAT}", flow.format),
        );
        return Err(issues.0);
    }
    if flow.nodes.len() > MAX_NODES {
        issues.push(
            None,
            IssueCode::TooManyNodes,
            format!("more than {MAX_NODES} nodes"),
        );
        return Err(issues.0);
    }

    let t = &flow.trigger;
    let mut trigger = None;
    match node_type(&t.kind) {
        None => issues.push(Some("trigger"), IssueCode::UnknownType, t.kind.clone()),
        Some(ty) if ty.role != Role::Trigger => {
            issues.push(Some("trigger"), IssueCode::NotATrigger, t.kind.clone())
        }
        Some(ty) => {
            if let Some(spec) = check_node(&mut issues, "trigger", ty, t.version, &t.params) {
                trigger = TriggerKind::from_id(ty.id);
                check_spec(&mut issues, "trigger", &spec);
            }
        }
    }

    let mut ids = BTreeSet::new();
    for node in &flow.nodes {
        if !good_id(&node.id) {
            issues.push(
                Some(&node.id),
                IssueCode::BadId,
                "1 to 64 of A-Z a-z 0-9 _ -",
            );
        } else if !ids.insert(node.id.as_str()) {
            issues.push(Some(&node.id), IssueCode::DuplicateId, node.id.clone());
        }
    }
    if let Some(next) = &t.next
        && !ids.contains(next.as_str())
    {
        issues.push(Some("trigger"), IssueCode::UnknownTarget, next.clone());
    }

    let mut nodes = HashMap::new();
    for node in &flow.nodes {
        let id = node.id.as_str();
        let Some(ty) = node_type(&node.kind) else {
            issues.push(Some(id), IssueCode::UnknownType, node.kind.clone());
            continue;
        };
        if ty.role == Role::Trigger {
            issues.push(Some(id), IssueCode::TriggerInGraph, node.kind.clone());
            continue;
        }
        if let Some(trigger) = trigger
            && !ty.triggers.contains(&trigger)
        {
            issues.push(
                Some(id),
                IssueCode::NotForTrigger,
                format!("{} cannot follow {}", ty.id, trigger.id()),
            );
        }
        for (port, target) in &node.next {
            if !ty.ports.contains(&port.as_str()) {
                issues.push(Some(id), IssueCode::UnknownPort, port.clone());
            }
            if !ids.contains(target.as_str()) {
                issues.push(Some(id), IssueCode::UnknownTarget, target.clone());
            }
        }
        if node.confirm && !ty.confirmable() {
            issues.push(Some(id), IssueCode::ConfirmNotAllowed, ty.id);
        }
        let Some(spec) = check_node(&mut issues, id, ty, node.version, &node.params) else {
            continue;
        };
        check_spec(&mut issues, id, &spec);
        check_templates(&mut issues, id, ty, &node.params);
        nodes.insert(
            id,
            CompiledNode {
                ty,
                spec,
                next: &node.next,
                confirm: node.confirm,
            },
        );
    }

    check_cycles(&mut issues, flow);

    match (issues.0.is_empty(), trigger) {
        (true, Some(trigger)) => Ok(Compiled {
            trigger,
            start: t.next.as_deref(),
            nodes,
        }),
        _ => Err(issues.0),
    }
}

fn check_node(
    issues: &mut Issues,
    id: &str,
    ty: &'static NodeType,
    version: u32,
    params: &serde_json::Value,
) -> Option<Spec> {
    if version == 0 || version > ty.version {
        issues.push(
            Some(id),
            IssueCode::UnsupportedVersion,
            format!("{} version {version}", ty.id),
        );
        return None;
    }
    match ty.parse(params) {
        Ok(spec) => Some(spec),
        Err(e) => {
            issues.push(Some(id), IssueCode::BadParams, e.to_string());
            None
        }
    }
}

fn check_templates(issues: &mut Issues, id: &str, ty: &NodeType, params: &serde_json::Value) {
    for field in ty.templates {
        if let Some(source) = params.get(field).and_then(|v| v.as_str())
            && let Err(e) = template::check(source)
        {
            issues.push(Some(id), IssueCode::BadTemplate, format!("{field}: {e}"));
        }
    }
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn address(s: &str) -> bool {
    let s = s.trim();
    match s.rsplit_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && s.len() <= 320
                && !s.contains(|c: char| c.is_whitespace() || c == ',' || c == '<' || c == '>')
        }
        None => false,
    }
}

fn check_spec(issues: &mut Issues, id: &str, spec: &Spec) {
    let mut bad = |code: IssueCode, detail: &str| issues.push(Some(id), code, detail);
    let tz_ok = |tz: &str| tz.parse::<Tz>().is_ok();
    let time_ok = |t: &str| t.len() == 5 && NaiveTime::parse_from_str(t, "%H:%M").is_ok();
    match spec {
        Spec::EventUpcoming(p) if !(1..=40320).contains(&p.minutes_before) => {
            bad(IssueCode::BadParams, "minutes_before: 1 to 40320")
        }
        Spec::Schedule(p) => {
            if let Err(e) = Cron::parse(&p.cron) {
                bad(IssueCode::BadCron, &e.to_string());
            }
            if !tz_ok(&p.timezone) {
                bad(IssueCode::BadTimezone, &p.timezone);
            }
        }
        Spec::Query(p) if blank(&p.query) || p.query.len() > 2000 => {
            bad(IssueCode::BadParams, "query: 1 to 2000 characters")
        }
        Spec::Header(p) => {
            if p.name.is_empty()
                || p.name.len() > 200
                || !p.name.bytes().all(|b| b.is_ascii_graphic() && b != b':')
            {
                bad(IssueCode::BadParams, "name: a header field name");
            }
            if p.op != crate::params::HeaderOp::Exists && p.value.is_empty() {
                bad(IssueCode::BadParams, "value: needed for this comparison");
            }
        }
        Spec::Alias(p) if p.service.as_deref().is_some_and(blank) => {
            bad(IssueCode::BadParams, "service: empty")
        }
        Spec::Trust(p) if p.any_of.is_empty() => bad(IssueCode::BadParams, "any_of: empty"),
        Spec::Insight(p) if blank(&p.kind) => bad(IssueCode::BadParams, "kind: empty"),
        Spec::TimeWindow(p) => {
            if !time_ok(&p.from) || !time_ok(&p.to) {
                bad(IssueCode::BadTime, "from and to: HH:MM");
            } else if p.from == p.to {
                bad(IssueCode::BadTime, "from and to are the same");
            }
            if !tz_ok(&p.timezone) {
                bad(IssueCode::BadTimezone, &p.timezone);
            }
        }
        Spec::Label(p) if blank(&p.name) => bad(IssueCode::BadParams, "name: empty"),
        Spec::SetFlags(p) => {
            if p.set.is_empty() && p.clear.is_empty() {
                bad(IssueCode::BadParams, "set or clear: at least one flag");
            }
            if p.set.iter().any(|f| p.clear.contains(f)) {
                bad(IssueCode::BadParams, "a flag is both set and cleared");
            }
        }
        Spec::Move(p) if blank(&p.folder) => bad(IssueCode::BadParams, "folder: empty"),
        Spec::Reply(p) => {
            if blank(&p.identity) {
                bad(IssueCode::BadParams, "identity: empty");
            }
            if !(1..=365).contains(&p.days) {
                bad(IssueCode::BadParams, "days: 1 to 365");
            }
        }
        Spec::Forward(p) => {
            if blank(&p.identity) {
                bad(IssueCode::BadParams, "identity: empty");
            }
            if p.to.is_empty() || p.to.len() > 20 || !p.to.iter().all(|a| address(a)) {
                bad(IssueCode::BadParams, "to: 1 to 20 addresses");
            }
        }
        Spec::Send(p) => {
            if blank(&p.identity) {
                bad(IssueCode::BadParams, "identity: empty");
            }
            if p.to.is_empty() || p.to.len() > 20 || !p.to.iter().all(|a| address(a)) {
                bad(IssueCode::BadParams, "to: 1 to 20 addresses");
            }
        }
        Spec::CreateEvent(p) => {
            if !(1..=10080).contains(&p.duration_minutes) {
                bad(IssueCode::BadParams, "duration_minutes: 1 to 10080");
            }
            if blank(&p.title) || blank(&p.starts) {
                bad(IssueCode::BadParams, "title and starts: needed");
            }
        }
        Spec::Notify(p) if blank(&p.title) => bad(IssueCode::BadParams, "title: empty"),
        Spec::Webhook(p) if blank(&p.hook) => bad(IssueCode::BadParams, "hook: empty"),
        Spec::SetIdentity(p) if blank(&p.identity) => bad(IssueCode::BadParams, "identity: empty"),
        Spec::SetTransport(p) if blank(&p.transport) => {
            bad(IssueCode::BadParams, "transport: empty")
        }
        Spec::AddBcc(p) if !address(&p.address) => bad(IssueCode::BadParams, "address: invalid"),
        Spec::RequireConfirmation(p) | Spec::Block(p)
            if blank(&p.reason) || p.reason.len() > 500 =>
        {
            bad(IssueCode::BadParams, "reason: 1 to 500 characters")
        }
        _ => {}
    }
}

fn check_cycles(issues: &mut Issues, flow: &Flow) {
    let edges: HashMap<&str, Vec<&str>> = flow
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.next.values().map(String::as_str).collect()))
        .collect();
    let mut state: HashMap<&str, u8> = HashMap::new();
    for node in &flow.nodes {
        let root = node.id.as_str();
        if state.contains_key(root) {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state.insert(root, 1);
        while let Some(top) = stack.last_mut() {
            let id = top.0;
            let next = edges.get(id).and_then(|e| e.get(top.1)).copied();
            top.1 += 1;
            match next {
                None => {
                    state.insert(id, 2);
                    stack.pop();
                }
                Some(n) if !edges.contains_key(n) => {}
                Some(n) => match state.get(n) {
                    Some(1) => {
                        issues.push(
                            Some(id),
                            IssueCode::Cycle,
                            format!("{id} leads back to {n}"),
                        );
                    }
                    Some(_) => {}
                    None => {
                        state.insert(n, 1);
                        stack.push((n, 0));
                    }
                },
            }
        }
    }
}

/// Every capability the flow's actions need, for checking the saver's grants when it is
/// saved and again before a run. Unknown types and bad parameters are skipped.
pub fn capabilities(flow: &Flow) -> BTreeSet<Capability> {
    flow.nodes
        .iter()
        .filter_map(|n| node_type(&n.kind)?.capability)
        .collect()
}

/// Every counter the flow's `counter` nodes read, so the server loads only those into the
/// [`crate::FactSheet`].
pub fn counters_used(flow: &Flow) -> BTreeSet<(CounterScope, CounterWindow)> {
    flow.nodes
        .iter()
        .filter_map(|n| match node_type(&n.kind)?.parse(&n.params).ok()? {
            Spec::Counter(p) => Some((p.scope, p.window)),
            _ => None,
        })
        .collect()
}
