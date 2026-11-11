use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::catalogue::TriggerKind;
use crate::params::{CounterScope, CounterWindow, Trust};

/// What happened, as the server hands it to [`crate::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Stable id of the event; with the flow version it keys the run, so a restart neither
    /// skips nor repeats it.
    pub id: String,
    /// What kind of event it is.
    pub trigger: TriggerKind,
    /// When it happened; time windows and templates read it.
    pub at: DateTime<Utc>,
    /// For `message.archived` and `invitation.received`: the message is live mail. Anything
    /// else (backfill, import, resync, rebuild) plans nothing.
    #[serde(default)]
    pub live: bool,
}

/// A name and an address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mailbox {
    /// Display name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The address.
    pub address: String,
}

/// The alias a message came through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasFact {
    /// The alias address.
    pub address: String,
    /// The alias service id, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

/// What is known about the event's message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFacts {
    /// Message id in the archive.
    pub id: String,
    /// The `From` mailbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Mailbox>,
    /// Decoded subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The `Date` of the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<DateTime<Utc>>,
    /// Header fields as (name, decoded value), in message order.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The alias it came through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<AliasFact>,
    /// Derived sender trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<Trust>,
    /// Kinds of the message's insights.
    #[serde(default)]
    pub insight_kinds: Vec<String>,
    /// Insight fields offered to templates, by name.
    #[serde(default)]
    pub insights: BTreeMap<String, String>,
    /// The sender's address belongs to a person in People.
    #[serde(default)]
    pub sender_is_person: bool,
}

/// A calendar event or invitation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFacts {
    /// iCalendar `UID`.
    pub uid: String,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts: Option<DateTime<Utc>>,
    /// Location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Organizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Mailbox>,
}

/// A counter the server counted for this event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterFact {
    /// What was counted.
    pub scope: CounterScope,
    /// Over which window.
    pub window: CounterWindow,
    /// The count, not including the event itself.
    pub count: u64,
}

/// Everything a flow may look at, loaded by the server before [`crate::evaluate`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactSheet {
    /// The event's message: archived, carrying an invitation, being sent, or a manual pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageFacts>,
    /// The invitation the message carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation: Option<EventFacts>,
    /// The upcoming event of `event.upcoming`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventFacts>,
    /// Counters the flow uses; see [`crate::counters_used`].
    #[serde(default)]
    pub counters: Vec<CounterFact>,
    /// The last automatic reply this flow planned to the message's sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<DateTime<Utc>>,
    /// Addresses of the archive's own accounts, identities and aliases, lowercase.
    #[serde(default)]
    pub own_addresses: Vec<String>,
}

/// Why a fact could not be given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactsError {
    /// The query does not parse.
    #[error("invalid query: {0}")]
    Query(String),
    /// The fact is not available for this event.
    #[error("fact not available: {0}")]
    Unavailable(String),
}

/// The facts of one event. Everything is in the [`FactSheet`] except search queries, which
/// the search language's `Matcher` answers on the message's stored facts.
pub trait Facts {
    /// The loaded facts.
    fn sheet(&self) -> &FactSheet;

    /// Whether the event's message matches the query. Same answer for the same query within
    /// one evaluation.
    fn matches(&self, query: &str) -> Result<bool, FactsError>;
}

/// A [`FactSheet`] with a function answering queries.
pub struct WithMatcher<F>(pub FactSheet, pub F);

impl<F> Facts for WithMatcher<F>
where
    F: Fn(&str) -> Result<bool, FactsError>,
{
    fn sheet(&self) -> &FactSheet {
        &self.0
    }

    fn matches(&self, query: &str) -> Result<bool, FactsError> {
        (self.1)(query)
    }
}
