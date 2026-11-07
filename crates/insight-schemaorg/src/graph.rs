use std::collections::{BTreeMap, BTreeSet};

use holtmail_insight::{MicrodataItem, MicrodataValue, Provenance};
use serde_json::Value;

/// Nodes one message may expand into, JSON-LD and microdata together.
pub const MAX_NODES: usize = 2_000;
/// Nesting depth the JSON-LD walk follows.
pub const MAX_JSON_DEPTH: usize = 32;
/// Nesting depth the microdata walk follows.
pub const MAX_MICRODATA_DEPTH: usize = 16;

/// The schema.org types the extractor reads; every other type is dropped while building.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ty {
    Flight,
    Lodging,
    Train,
    Bus,
    Event,
    Food,
    RentalCar,
    Order,
    Invoice,
    Parcel,
    Confirm,
    View,
    Track,
    CheckIn,
}

impl Ty {
    pub(crate) const ALL: [(&'static str, Ty, &'static str); 14] = [
        ("FlightReservation", Ty::Flight, "flight_reservation"),
        ("LodgingReservation", Ty::Lodging, "lodging_reservation"),
        ("TrainReservation", Ty::Train, "train_reservation"),
        ("BusReservation", Ty::Bus, "bus_reservation"),
        ("EventReservation", Ty::Event, "event_reservation"),
        (
            "FoodEstablishmentReservation",
            Ty::Food,
            "food_establishment_reservation",
        ),
        (
            "RentalCarReservation",
            Ty::RentalCar,
            "rental_car_reservation",
        ),
        ("Order", Ty::Order, "order"),
        ("Invoice", Ty::Invoice, "invoice"),
        ("ParcelDelivery", Ty::Parcel, "parcel_delivery"),
        ("ConfirmAction", Ty::Confirm, "confirm_action"),
        ("ViewAction", Ty::View, "view_action"),
        ("TrackAction", Ty::Track, "track_action"),
        ("CheckInAction", Ty::CheckIn, "check_in_action"),
    ];

    fn parse(text: &str) -> Option<Ty> {
        let name = local_name(text.trim())?;
        Self::ALL
            .iter()
            .find(|(known, _, _)| *known == name)
            .map(|(_, ty, _)| *ty)
    }

    /// The type's name for `explain`, in lowercase snake case.
    pub(crate) fn key(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, ty, _)| *ty == self)
            .map_or("", |(_, _, key)| key)
    }
}

/// `X` of `X`, `schema:X`, `http://schema.org/X` or `https://schema.org/X`.
pub(crate) fn local_name(text: &str) -> Option<&str> {
    let name = ["https://schema.org/", "http://schema.org/", "schema:"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .unwrap_or(text);
    (!name.is_empty() && !name.contains([':', '/', '#'])).then_some(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Val {
    Text(String),
    Node(usize),
}

#[derive(Debug, Default)]
pub(crate) struct Node {
    pub types: Vec<Ty>,
    pub id: Option<String>,
    pub props: Vec<(String, Val)>,
}

/// One top-level node and where it came from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Root {
    pub node: usize,
    pub provenance: Provenance,
    pub forwarded: bool,
    pub block: usize,
}

/// What stopped or cut the walk, for `explain`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cuts {
    pub unparsable: usize,
    pub depth: usize,
    pub references: usize,
    pub budget: bool,
}

/// The nodes of one message, JSON-LD and microdata mapped to one model.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub nodes: Vec<Node>,
    pub roots: Vec<Root>,
    pub cuts: Cuts,
    spent: usize,
}

type Slot<'a> = Option<(usize, &'a str)>;

struct Task<'a> {
    value: &'a Value,
    slot: Option<(usize, &'a str)>,
    depth: usize,
    resolved: bool,
}

impl Graph {
    pub(crate) fn build(
        ld_json: &[String],
        forward_at: Option<usize>,
        microdata: &[MicrodataItem],
    ) -> Self {
        let mut graph = Self::default();
        for (block, text) in ld_json.iter().enumerate() {
            if graph.cuts.budget {
                break;
            }
            let Ok(value) = serde_json::from_str::<Value>(text) else {
                graph.cuts.unparsable += 1;
                continue;
            };
            let forwarded = forward_at.is_some_and(|at| block >= at);
            graph.json_block(&value, block, forwarded);
        }
        for (index, item) in microdata.iter().enumerate() {
            if graph.cuts.budget {
                break;
            }
            if item.top_level {
                graph.microdata_root(microdata, index);
            }
        }
        graph
    }

    fn spend(&mut self) -> bool {
        if self.spent >= MAX_NODES {
            self.cuts.budget = true;
            return false;
        }
        self.spent += 1;
        true
    }

    fn push_node(&mut self, node: Node, slot: Option<(usize, &str)>) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        self.attach(slot, Val::Node(index));
        index
    }

    fn attach(&mut self, slot: Option<(usize, &str)>, value: Val) {
        if let Some((parent, name)) = slot
            && let Some(parent) = self.nodes.get_mut(parent)
            && let Some(name) = local_name(name)
        {
            parent.props.push((name.to_owned(), value));
        }
    }

    fn json_block(&mut self, value: &Value, block: usize, forwarded: bool) {
        let mut tops: Vec<&Value> = Vec::new();
        match value {
            Value::Array(items) => tops.extend(items.iter().filter(|v| v.is_object())),
            Value::Object(map) => {
                if map.contains_key("@type") {
                    tops.push(value);
                }
                match map.get("@graph") {
                    Some(Value::Array(items)) => {
                        tops.extend(items.iter().filter(|v| v.is_object()));
                    }
                    Some(single @ Value::Object(_)) => tops.push(single),
                    _ => {}
                }
            }
            _ => {}
        }
        let mut ids: BTreeMap<&str, &Value> = BTreeMap::new();
        for top in &tops {
            if let Some(id) = top.get("@id").and_then(Value::as_str) {
                ids.entry(id).or_insert(top);
            }
        }
        for top in tops {
            let mark = self.nodes.len();
            match self.json_root(top, &ids) {
                Some(node) => self.roots.push(Root {
                    node,
                    provenance: Provenance::JsonLd,
                    forwarded,
                    block,
                }),
                None => {
                    self.nodes.truncate(mark);
                    if self.cuts.budget {
                        return;
                    }
                }
            }
        }
    }

    fn json_root(&mut self, top: &Value, ids: &BTreeMap<&str, &Value>) -> Option<usize> {
        let first = self.nodes.len();
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        if let Some(id) = top.get("@id").and_then(Value::as_str) {
            visited.insert(id);
        }
        let mut stack = vec![Task {
            value: top,
            slot: None,
            depth: 1,
            resolved: false,
        }];
        let mut root = None;
        while let Some(task) = stack.pop() {
            if task.depth > MAX_JSON_DEPTH {
                self.cuts.depth += 1;
                continue;
            }
            if !self.spend() {
                return None;
            }
            match task.value {
                Value::Object(map) => {
                    if map.len() == 1
                        && let Some(Value::String(id)) = map.get("@id")
                    {
                        match ids.get(id.as_str()) {
                            Some(target) if !task.resolved && visited.insert(id) => {
                                stack.push(Task {
                                    value: target,
                                    slot: task.slot,
                                    depth: task.depth,
                                    resolved: true,
                                });
                            }
                            _ => self.cuts.references += 1,
                        }
                        continue;
                    }
                    if let Some(inner) = map.get("@value") {
                        if let Some(text) = scalar(inner) {
                            self.attach(task.slot, Val::Text(text));
                        }
                        continue;
                    }
                    let node = Node {
                        types: types_of(map.get("@type")),
                        id: map.get("@id").and_then(Value::as_str).map(str::to_owned),
                        props: Vec::new(),
                    };
                    let index = self.push_node(node, task.slot);
                    root.get_or_insert(index);
                    for (name, value) in map.iter().rev() {
                        if name.starts_with('@') {
                            continue;
                        }
                        stack.push(Task {
                            value,
                            slot: Some((index, name)),
                            depth: task.depth + 1,
                            resolved: task.resolved,
                        });
                    }
                }
                Value::Array(items) => {
                    for value in items.iter().rev() {
                        stack.push(Task {
                            value,
                            slot: task.slot,
                            depth: task.depth + 1,
                            resolved: task.resolved,
                        });
                    }
                }
                other => {
                    if let Some(text) = scalar(other) {
                        self.attach(task.slot, Val::Text(text));
                    }
                }
            }
        }
        root.filter(|&r| r == first)
    }

    fn microdata_root(&mut self, items: &[MicrodataItem], index: usize) {
        let Some(item) = items.get(index) else {
            return;
        };
        let mark = self.nodes.len();
        let mut visited = BTreeSet::from([index]);
        let mut stack: Vec<(usize, Slot<'_>, usize)> = vec![(index, None, 1)];
        let mut root = None;
        while let Some((at, slot, depth)) = stack.pop() {
            let Some(current) = items.get(at) else {
                self.cuts.references += 1;
                continue;
            };
            if !self.spend() {
                self.nodes.truncate(mark);
                return;
            }
            let node = Node {
                types: current.types.iter().filter_map(|t| Ty::parse(t)).collect(),
                id: current.id.clone(),
                props: Vec::new(),
            };
            let node = self.push_node(node, slot);
            root.get_or_insert(node);
            let mut children = Vec::new();
            for property in &current.properties {
                match &property.value {
                    MicrodataValue::Text(text) | MicrodataValue::Url(text) => {
                        if !self.spend() {
                            self.nodes.truncate(mark);
                            return;
                        }
                        self.attach(Some((node, property.name.trim())), Val::Text(text.clone()));
                    }
                    MicrodataValue::Item(child) => {
                        if depth >= MAX_MICRODATA_DEPTH {
                            self.cuts.depth += 1;
                        } else if visited.insert(*child) {
                            children.push((*child, Some((node, property.name.trim())), depth + 1));
                        } else {
                            self.cuts.references += 1;
                        }
                    }
                }
            }
            stack.extend(children.into_iter().rev());
        }
        if let Some(node) = root {
            self.roots.push(Root {
                node,
                provenance: Provenance::Microdata,
                forwarded: item.below_forward,
                block: index,
            });
        }
    }

    pub(crate) fn node(&self, index: usize) -> Option<&Node> {
        self.nodes.get(index)
    }

    /// Every value of `name` on `node`, in order.
    pub(crate) fn values<'g, 'n>(
        &'g self,
        node: usize,
        name: &'n str,
    ) -> impl Iterator<Item = &'g Val> + use<'g, 'n> {
        self.node(node)
            .into_iter()
            .flat_map(|n| n.props.iter())
            .filter(move |(key, _)| key == name)
            .map(|(_, value)| value)
    }

    /// The first node value of `name`.
    pub(crate) fn child(&self, node: usize, name: &str) -> Option<usize> {
        self.values(node, name).find_map(|v| match v {
            Val::Node(n) => Some(*n),
            Val::Text(_) => None,
        })
    }

    /// The node at the end of `path`, following the first node value at each step.
    pub(crate) fn walk(&self, node: usize, path: &[&str]) -> Option<usize> {
        path.iter().try_fold(node, |at, name| self.child(at, name))
    }

    /// The first non-empty text value of `name`, trimmed.
    pub(crate) fn text(&self, node: usize, name: &str) -> Option<&str> {
        self.values(node, name).find_map(|v| match v {
            Val::Text(text) => Some(text.trim()).filter(|t| !t.is_empty()),
            Val::Node(_) => None,
        })
    }

    /// The text at the end of `path`: nodes for every name but the last, then its text.
    pub(crate) fn text_at(&self, node: usize, path: &[&str]) -> Option<&str> {
        let (last, rest) = path.split_last()?;
        self.text(self.walk(node, rest)?, last)
    }

    /// The name of the first value of `name`: the text itself, or the nested node's `name`.
    pub(crate) fn name_of(&self, node: usize, name: &str) -> Option<&str> {
        self.values(node, name).find_map(|v| match v {
            Val::Text(text) => Some(text.trim()).filter(|t| !t.is_empty()),
            Val::Node(n) => self.text(*n, "name"),
        })
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) if number.is_f64() => plain_decimal(&number.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn plain_decimal(text: &str) -> Option<String> {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (text, 0),
    };
    if exponent.abs() > 32 {
        return None;
    }
    let (negative, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{fraction}");
    let mut point = i32::try_from(whole.len()).ok()? + exponent;
    if point < 0 {
        digits.insert_str(0, &"0".repeat(point.unsigned_abs() as usize));
        point = 0;
    }
    let point = usize::try_from(point).ok()?;
    if point > digits.len() {
        digits.push_str(&"0".repeat(point - digits.len()));
    }
    let (whole, fraction) = digits.split_at(point);
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.trim_end_matches('0');
    let sign = if negative { "-" } else { "" };
    Some(if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    })
}

fn types_of(value: Option<&Value>) -> Vec<Ty> {
    let mut out = Vec::new();
    match value {
        Some(Value::String(text)) => out.extend(Ty::parse(text)),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(ty) = item.as_str().and_then(Ty::parse)
                    && !out.contains(&ty)
                {
                    out.push(ty);
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use holtmail_insight::MicrodataProperty;

    use super::*;

    fn root(graph: &Graph, index: usize) -> Root {
        *graph.roots.get(index).expect("root")
    }

    fn json(blocks: &[&str]) -> Graph {
        let blocks: Vec<String> = blocks.iter().map(|b| (*b).to_owned()).collect();
        Graph::build(&blocks, None, &[])
    }

    fn item(types: &[&str], top: bool, props: Vec<(&str, MicrodataValue)>) -> MicrodataItem {
        let mut item = MicrodataItem::new(types.iter().map(|t| (*t).to_owned()).collect());
        item.top_level = top;
        item.properties = props
            .into_iter()
            .map(|(name, value)| MicrodataProperty {
                name: name.into(),
                value,
            })
            .collect();
        item
    }

    #[test]
    fn reads_graphs_arrays_and_uri_types() {
        let graph = json(&[
            r#"{"@context":"https://schema.org","@graph":[{"@type":"Order","orderNumber":"A1"},{"@type":["Thing","https://schema.org/Invoice"]}]}"#,
            r#"[{"@type":"http://schema.org/ParcelDelivery"},{"@type":"Unknown"}, 5]"#,
            r#"{"@type":"schema:ViewAction","name":{"@value":"Open"}}"#,
        ]);
        let types: Vec<Vec<Ty>> = graph
            .roots
            .iter()
            .map(|r| graph.node(r.node).unwrap().types.clone())
            .collect();
        assert_eq!(
            types,
            [
                vec![Ty::Order],
                vec![Ty::Invoice],
                vec![Ty::Parcel],
                vec![],
                vec![Ty::View]
            ]
        );
        assert_eq!(graph.text(root(&graph, 0).node, "orderNumber"), Some("A1"));
        assert_eq!(graph.text(root(&graph, 4).node, "name"), Some("Open"));
        assert_eq!(root(&graph, 2).block, 1);
    }

    #[test]
    fn ids_resolve_one_level_against_top_level_nodes() {
        let graph = json(&[r##"{"@graph":[
            {"@id":"#r","@type":"FlightReservation","reservationFor":{"@id":"#f"},"self":{"@id":"#r"}},
            {"@id":"#f","@type":"Flight","flightNumber":"123","airline":{"@id":"#a"}},
            {"@id":"#a","iataCode":"XA"}
        ]}"##]);
        let reservation = root(&graph, 0).node;
        let flight = graph.child(reservation, "reservationFor").unwrap();
        assert_eq!(graph.text(flight, "flightNumber"), Some("123"));
        assert_eq!(graph.child(flight, "airline"), None);
        assert_eq!(graph.child(reservation, "self"), None);
        assert_eq!(
            graph.text_at(root(&graph, 1).node, &["airline", "iataCode"]),
            Some("XA")
        );
    }

    #[test]
    fn id_cycles_end() {
        let graph = json(&[r##"[
            {"@id":"a","@type":"Order","next":{"@id":"b"}},
            {"@id":"b","@type":"Order","next":{"@id":"a"}}
        ]"##]);
        assert_eq!(graph.roots.len(), 2);
        assert!(graph.cuts.references >= 2);
        assert!(graph.nodes.len() <= 4);
    }

    #[test]
    fn depth_is_cut_at_32() {
        let mut deep = String::from(r#"{"@type":"Order","orderNumber":"D1","x":"#);
        for _ in 0..60 {
            deep.push_str(r#"{"y":"#);
        }
        deep.push('1');
        deep.push_str(&"}".repeat(61));
        let graph = json(&[&deep]);
        assert_eq!(graph.roots.len(), 1);
        assert!(graph.cuts.depth > 0);
        assert!(graph.nodes.len() <= MAX_JSON_DEPTH);
        assert_eq!(graph.text(root(&graph, 0).node, "orderNumber"), Some("D1"));

        let too_deep = format!("{}{}", "[".repeat(500), "]".repeat(500));
        assert_eq!(json(&[&too_deep]).cuts.unparsable, 1);
    }

    #[test]
    fn budget_stops_the_walk() {
        let mut many = String::from("[");
        for i in 0..1_500 {
            if i > 0 {
                many.push(',');
            }
            many.push_str(&format!(r#"{{"@type":"Order","orderNumber":"N{i}"}}"#));
        }
        many.push(']');
        let graph = json(&[&many, r#"{"@type":"Order","orderNumber":"LATE"}"#]);
        assert!(graph.cuts.budget);
        assert_eq!(graph.roots.len(), MAX_NODES / 2);
        assert!(graph.spent <= MAX_NODES);
        assert!(graph.roots.iter().all(|r| r.block == 0));
    }

    #[test]
    fn fan_in_is_bounded_by_the_budget() {
        let mut block = String::from(r##"[{"@id":"#big","@type":"Order","p":["##);
        for i in 0..1_000 {
            if i > 0 {
                block.push(',');
            }
            block.push_str(&i.to_string());
        }
        block.push_str("]}");
        for _ in 0..50 {
            block.push_str(r##",{"@type":"Order","a":{"@id":"#big"},"b":{"@id":"#big"}}"##);
        }
        block.push(']');
        let graph = json(&[&block]);
        assert!(graph.cuts.budget);
        assert!(graph.spent <= MAX_NODES);
    }

    #[test]
    fn microdata_cycles_and_bad_indexes_end() {
        let items = vec![
            item(
                &["https://schema.org/LodgingReservation"],
                true,
                vec![
                    ("reservationNumber", MicrodataValue::Text("HX-1".into())),
                    ("reservationFor", MicrodataValue::Item(1)),
                    ("missing", MicrodataValue::Item(99)),
                    ("self", MicrodataValue::Item(0)),
                ],
            ),
            item(
                &["https://schema.org/Hotel"],
                false,
                vec![
                    ("name", MicrodataValue::Text("Example Inn".into())),
                    ("back", MicrodataValue::Item(0)),
                    ("loop", MicrodataValue::Item(1)),
                ],
            ),
        ];
        let graph = Graph::build(&[], None, &items);
        assert_eq!(graph.roots.len(), 1);
        let root = root(&graph, 0);
        assert_eq!(root.provenance, Provenance::Microdata);
        assert_eq!(graph.text(root.node, "reservationNumber"), Some("HX-1"));
        assert_eq!(
            graph.text_at(root.node, &["reservationFor", "name"]),
            Some("Example Inn")
        );
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.cuts.references, 4);
    }

    #[test]
    fn microdata_depth_is_cut_at_16() {
        let items: Vec<MicrodataItem> = (0..40)
            .map(|i| {
                item(
                    &["https://schema.org/Thing"],
                    i == 0,
                    vec![("next", MicrodataValue::Item(i + 1))],
                )
            })
            .collect();
        let graph = Graph::build(&[], None, &items);
        assert_eq!(graph.nodes.len(), MAX_MICRODATA_DEPTH);
        assert_eq!(graph.cuts.depth, 1);
    }

    #[test]
    fn forwarded_blocks_and_items_are_marked() {
        let mut below = item(&["https://schema.org/Order"], true, vec![]);
        below.below_forward = true;
        let blocks = vec![
            r#"{"@type":"Order"}"#.to_owned(),
            r#"{"@type":"Order"}"#.to_owned(),
        ];
        let graph = Graph::build(&blocks, Some(1), &[below]);
        let flags: Vec<bool> = graph.roots.iter().map(|r| r.forwarded).collect();
        assert_eq!(flags, [false, true, true]);
    }

    #[test]
    fn float_prices_become_plain_decimals() {
        for (json, text) in [
            ("1e3", "1000"),
            ("12800.0", "12800"),
            ("12.50", "12.5"),
            ("1e16", "10000000000000000"),
            ("1.5e-7", "0.00000015"),
            ("-3.25", "-3.25"),
            ("0.5", "0.5"),
        ] {
            let value: Value = serde_json::from_str(json).expect("json");
            assert_eq!(scalar(&value).as_deref(), Some(text), "{json}");
        }
        let value: Value = serde_json::from_str("1e300").expect("json");
        assert_eq!(scalar(&value), None);
        let value: Value = serde_json::from_str("12800").expect("json");
        assert_eq!(scalar(&value).as_deref(), Some("12800"));
    }
}
