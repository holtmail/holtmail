use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The flow format this crate writes and the newest it reads.
pub const FLOW_FORMAT: u32 = 1;

/// The most nodes a flow may have, the trigger not counted.
pub const MAX_NODES: usize = 200;

/// A rule: one trigger and a graph of nodes, stored as JSON per archive.
///
/// The graph is followed from the trigger along one output port per node, so a run takes a
/// single path. Unknown fields are refused, so a flow read back is the flow that was saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    /// The format version of this JSON; [`FLOW_FORMAT`] for flows written now.
    #[serde(default = "format_one")]
    pub format: u32,
    /// What starts a run.
    pub trigger: TriggerNode,
    /// Conditions and actions, in any order.
    #[serde(default)]
    pub nodes: Vec<Node>,
}

fn format_one() -> u32 {
    FLOW_FORMAT
}

/// The trigger of a flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerNode {
    /// A trigger type id from the catalogue, such as `message.archived`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The version of the trigger type the parameters were written for.
    #[serde(default = "version_one")]
    pub version: u32,
    /// Parameters, checked against the type's schema.
    #[serde(default = "empty_object")]
    pub params: Value,
    /// The first node of the run; a flow with none plans nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Where the editor draws it; ignored by the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

/// A condition or an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Unique within the flow: 1 to 64 of `A-Z a-z 0-9 _ -`.
    pub id: String,
    /// A node type id from the catalogue, such as `query` or `reply`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The version of the node type the parameters were written for.
    #[serde(default = "version_one")]
    pub version: u32,
    /// Parameters, checked against the type's schema.
    #[serde(default = "empty_object")]
    pub params: Value,
    /// The node each output port leads to; a port left out ends the run there.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub next: BTreeMap<String, String>,
    /// For an action: it waits for a person instead of running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm: bool,
    /// Where the editor draws it; ignored by the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

/// A point on the editor's canvas.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// Horizontal.
    pub x: f64,
    /// Vertical.
    pub y: f64,
}

fn version_one() -> u32 {
    1
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

impl Flow {
    /// Reads a flow from JSON. Only the shape is checked here; see [`crate::validate`].
    pub fn from_json(json: &str) -> Result<Flow, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The flow as pretty JSON, fields in a stable order.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The node with this id.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Every search query of the flow's `query` nodes with the node id, in node order, so the
    /// caller can check them with the search language's parser.
    pub fn queries(&self) -> Vec<(&str, &str)> {
        self.nodes
            .iter()
            .filter(|n| n.kind == "query")
            .filter_map(|n| Some((n.id.as_str(), n.params.get("query")?.as_str()?)))
            .collect()
    }
}
