//! Rules as flows: a trigger and a graph of conditions and actions, stored as versioned JSON
//! per archive.
//!
//! The crate is pure. [`validate`] checks a flow against the [`catalogue`]; [`evaluate`] turns
//! a flow, an [`Event`] and its [`Facts`] into a [`Plan`] of intended actions with the path
//! taken. The server carries out a plan through the outbox and the writers, where every
//! safety check sits. Templates render in a [`template`] sandbox.

mod catalogue;
pub mod cron;
mod evaluate;
mod facts;
mod flow;
pub mod params;
mod plan;
pub mod template;
mod validate;

pub use catalogue::{
    Capability, Docs, NodeType, NodeTypeDoc, Role, Spec, Text, TriggerKind, catalogue, describe,
    node_type,
};
pub use evaluate::{EvalError, EventContext, TemplateContext, evaluate};
pub use facts::{
    AliasFact, CounterFact, Event, EventFacts, FactSheet, Facts, FactsError, Mailbox, MessageFacts,
    WithMatcher,
};
pub use flow::{FLOW_FORMAT, Flow, MAX_NODES, Node, Position, TriggerNode};
pub use plan::{Action, AutoSubmitted, Ignored, Outcome, Plan, PlannedAction, Skip, Step};
pub use validate::{Issue, IssueCode, capabilities, counters_used, validate};

#[cfg(test)]
mod tests;
