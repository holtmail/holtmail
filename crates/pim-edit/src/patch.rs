use bytes::Bytes;
use holtmail_collection::{Field, ItemPatch};
use holtmail_pim::ItemKind;
use serde::{Deserialize, Serialize};

use crate::lines::{self, Line};
use crate::model::{
    self, ANSWER_PARAMS, Category, Groups, INSTANCE_GROUPS, Item, Part, add_missing_zones,
    address_key, category, same, set_group, upstream_wins,
};
use crate::validate::{Invalid, single_uid};

/// What a change amounts to compared with its base. The names are the store's change classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Nothing that matters changed: only `DTSTAMP`, `LAST-MODIFIED`, `CREATED`, `SEQUENCE`,
    /// `PRODID`, `REV`, `X-MOZ-GENERATION`, folding, parameter order or `SCHEDULE-STATUS`.
    Noop,
    /// Only alarms, acknowledgements and client properties (`X-MOZ-*`, `X-APPLE-*`,
    /// `X-EVOLUTION-*`, and `X-AB*` in a vCard).
    Personal,
    /// The user's own `PARTSTAT` or `RSVP`, for the series or one instance, plus what RFC 6638
    /// §3.2.2.1 lets an attendee change (alarms, `TRANSP`, `PERCENT-COMPLETE`, `COMPLETED`).
    Reply,
    /// Anything else in an existing item.
    Edit,
    /// A new item.
    Create,
    /// A removed item.
    Delete,
}

impl Class {
    /// The lowercase name, as the store spells it (`noop`, `personal`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Noop => "noop",
            Class::Personal => "personal",
            Class::Reply => "reply",
            Class::Edit => "edit",
            Class::Create => "create",
            Class::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GroupChange {
    group: String,
    before: Vec<String>,
    after: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ComponentPatch {
    key: String,
    in_base: bool,
    in_mine: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    template: Option<String>,
    groups: Vec<GroupChange>,
}

/// A change against its base, by component (the series and each changed instance) and by
/// property group. Serializes to JSON for the store's opaque `patch` column; the shape is
/// private to this crate and stays readable by later versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    kind: ItemKind,
    class: Class,
    components: Vec<ComponentPatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    zones: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    respond: Option<crate::Respond>,
}

/// The result of [`classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// What the change amounts to.
    pub class: Class,
    /// The fields it touches, each once, in the order they appear; empty for `noop`.
    pub fields: Vec<Field>,
    /// The patch to store and rebase later.
    pub patch: Patch,
}

impl Patch {
    /// The class it was classified as.
    pub fn class(&self) -> Class {
        self.class
    }

    /// The item kind it applies to.
    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    /// The fields it touches, each once, in the order they appear.
    pub fn fields(&self) -> Vec<Field> {
        let mut out: Vec<Field> = Vec::new();
        for component in &self.components {
            if component.in_base != component.in_mine && !component.key.is_empty() {
                push_field(&mut out, Field::Recurrence);
            }
            for change in &component.groups {
                push_field(&mut out, model::field(self.kind, &change.group));
            }
        }
        out
    }

    /// The same patch carrying `respond`: a `reply` change that goes upstream through
    /// `CollectionWriter::respond` where the writer offers it. Ignored for other classes.
    pub fn with_respond(mut self, respond: crate::Respond) -> Self {
        if self.class == Class::Reply {
            self.respond = Some(respond);
        }
        self
    }

    /// How the `reply` change goes upstream, when [`Patch::with_respond`] said so.
    pub fn respond(&self) -> Option<&crate::Respond> {
        self.respond.as_ref()
    }

    /// Whether it changes nothing that matters.
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// The normalized addresses of the attendee entries it touches, each once, in order. For a
    /// `reply` these are the user's own entries, so they can stand in for the user's addresses
    /// when the reply is rebased.
    pub fn attendees(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for change in self.components.iter().flat_map(|c| &c.groups) {
            if let Some(address) = change.group.strip_prefix("ATTENDEE ")
                && !out.iter().any(|a| a == address)
            {
                out.push(address.to_owned());
            }
        }
        out
    }
}

fn push_field(out: &mut Vec<Field>, field: Field) {
    if !out.contains(&field) {
        out.push(field);
    }
}

fn diff_groups(base: &Groups, mine: &Groups) -> Vec<GroupChange> {
    let mut names: Vec<&String> = mine.keys().chain(base.keys()).collect();
    names.sort();
    names.dedup();
    let empty = Vec::new();
    names
        .into_iter()
        .filter_map(|group| {
            let before = base.get(group).unwrap_or(&empty);
            let after = mine.get(group).unwrap_or(&empty);
            (!same(group, before, after)).then(|| GroupChange {
                group: group.clone(),
                before: before.clone(),
                after: after.clone(),
            })
        })
        .collect()
}

fn diff(base: Option<&Item>, mine: &Item) -> Vec<ComponentPatch> {
    let base_parts = base.map(Item::parts).unwrap_or_default();
    let mine_parts = mine.parts();
    let mut out = Vec::new();
    for part in &mine_parts {
        match base_parts.iter().find(|b| b.key == part.key) {
            Some(old) => {
                let groups = diff_groups(&old.groups, &part.groups);
                if !groups.is_empty() {
                    out.push(ComponentPatch {
                        key: part.key.clone(),
                        in_base: true,
                        in_mine: true,
                        template: None,
                        groups,
                    });
                }
            }
            None => out.push(ComponentPatch {
                key: part.key.clone(),
                in_base: false,
                in_mine: true,
                template: Some(part.component.text()),
                groups: diff_groups(&Groups::new(), &part.groups),
            }),
        }
    }
    for old in &base_parts {
        if !mine_parts.iter().any(|p| p.key == old.key) {
            out.push(ComponentPatch {
                key: old.key.clone(),
                in_base: true,
                in_mine: false,
                template: None,
                groups: diff_groups(&old.groups, &Groups::new()),
            });
        }
    }
    out
}

fn is_own(group: &str, own: &[String]) -> bool {
    group
        .strip_prefix("ATTENDEE ")
        .is_some_and(|address| own.iter().any(|o| address_key(o) == address))
}

fn answer_only(before: &[String], after: &[String]) -> bool {
    match (before, after) {
        ([b], [a]) => match (Line::parse(b), Line::parse(a)) {
            (Some(b), Some(a)) => b.canonical(ANSWER_PARAMS) == a.canonical(ANSWER_PARAMS),
            _ => false,
        },
        _ => false,
    }
}

#[derive(Default)]
struct Tally {
    personal: bool,
    allowed: bool,
    answer: bool,
    other: bool,
}

impl Tally {
    fn group(&mut self, kind: ItemKind, change: &GroupChange, own: &[String]) {
        match category(kind, &change.group) {
            Category::Personal => self.personal = true,
            Category::AttendeeAllowed => self.allowed = true,
            Category::Other
                if is_own(&change.group, own) && answer_only(&change.before, &change.after) =>
            {
                self.answer = true
            }
            Category::Other => self.other = true,
        }
    }

    fn class(&self) -> Class {
        if self.other || (self.allowed && !self.answer) {
            Class::Edit
        } else if self.answer {
            Class::Reply
        } else if self.personal {
            Class::Personal
        } else {
            Class::Noop
        }
    }
}

fn instance_matches_time(part: &Part<'_>) -> bool {
    let rid = part.component.line("RECURRENCE-ID");
    let start = part.component.line("DTSTART");
    match (rid, start) {
        (Some(rid), Some(start)) => model::instance_key(rid) == model::instance_key(start),
        (Some(_), None) => true,
        _ => false,
    }
}

fn class_of(base: &Item, mine: &Item, components: &[ComponentPatch], own: &[String]) -> Class {
    let kind = base.kind;
    let mut tally = Tally::default();
    let base_parts = base.parts();
    let mine_parts = mine.parts();
    let master = base_parts.iter().find(|p| p.key.is_empty());
    for component in components {
        match (component.in_base, component.in_mine) {
            (true, true) => {
                for change in &component.groups {
                    tally.group(kind, change, own);
                }
            }
            (false, true) => {
                let part = mine_parts.iter().find(|p| p.key == component.key);
                match (master, part) {
                    (Some(master), Some(part))
                        if !component.key.is_empty() && instance_matches_time(part) =>
                    {
                        let mut master_groups = master.groups.clone();
                        let mut instance_groups = part.groups.clone();
                        for skip in INSTANCE_GROUPS {
                            master_groups.remove(*skip);
                            instance_groups.remove(*skip);
                        }
                        for change in diff_groups(&master_groups, &instance_groups) {
                            tally.group(kind, &change, own);
                        }
                    }
                    _ => tally.other = true,
                }
            }
            _ => tally.other = true,
        }
    }
    tally.class()
}

/// Classifies `mine` against `base` (`None` for a create), matching the user's own attendee
/// entry against `own` addresses (any spelling `normalize_email` accepts), and computes the
/// patch to store. Fails like [`crate::validate`] when `mine` cannot be read; a `base` that
/// cannot be read makes every part of `mine` an edit.
pub fn classify(
    kind: ItemKind,
    base: Option<&[u8]>,
    mine: &[u8],
    own: &[String],
) -> Result<Classified, Invalid> {
    let mine_item = Item::parse(kind, mine).ok_or(Invalid::Unreadable)?;
    single_uid(&mine_item)?;
    let base_item = base.and_then(|b| Item::parse(kind, b));
    let components = diff(base_item.as_ref(), &mine_item);
    let class = match (&base_item, base) {
        (_, None) => Class::Create,
        (None, Some(_)) => Class::Edit,
        (Some(base_item), Some(_)) => class_of(base_item, &mine_item, &components, own),
    };
    let patch = Patch {
        kind,
        class,
        components,
        zones: mine_item.zones().into_iter().map(|(_, z)| z).collect(),
        respond: None,
    };
    Ok(Classified {
        class,
        fields: if class == Class::Noop {
            Vec::new()
        } else {
            patch.fields()
        },
        patch,
    })
}

/// What re-applying a change on a newer upstream version gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebased {
    /// Upstream already holds everything the change meant (or the change meant nothing):
    /// the change is `applied`.
    Applied,
    /// The change holds on top of upstream; serve these bytes.
    Clean(Vec<u8>),
    /// Both sides changed a field where upstream wins (`DTSTART`, `DTEND`, `DURATION`, `DUE`,
    /// the recurrence, `STATUS:CANCELLED`, an attendee) or an instance the change touched
    /// vanished upstream. `served` has upstream's value for those fields and the change's for
    /// the rest; the change keeps its own bytes for "Keep mine".
    Conflict {
        /// The merged item.
        served: Vec<u8>,
        /// The fields where upstream won.
        fields: Vec<Field>,
    },
    /// A reply whose event was cancelled, or whose attendee entry or instance was removed
    /// upstream. `served` is upstream with whatever of the reply still applies.
    ReplyObsolete {
        /// The merged item.
        served: Vec<u8>,
    },
    /// The upstream version cannot be read as the item's kind; keep serving the change.
    Unreadable,
}

impl Rebased {
    /// The conflict reason code for the store: `overlap`, `reply_obsolete` or
    /// `upstream_unreadable`; `None` for `Applied` and `Clean`.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Rebased::Applied | Rebased::Clean(_) => None,
            Rebased::Conflict { .. } => Some("overlap"),
            Rebased::ReplyObsolete { .. } => Some("reply_obsolete"),
            Rebased::Unreadable => Some("upstream_unreadable"),
        }
    }

    /// The bytes to serve, when they differ from upstream's own.
    pub fn served(&self) -> Option<&[u8]> {
        match self {
            Rebased::Clean(b)
            | Rebased::Conflict { served: b, .. }
            | Rebased::ReplyObsolete { served: b } => Some(b),
            _ => None,
        }
    }
}

fn with_answer(theirs: &str, mine: &str) -> String {
    let (Some(t), Some(m)) = (Line::parse(theirs), Line::parse(mine)) else {
        return mine.to_owned();
    };
    let mut params: Vec<String> = t
        .params
        .iter()
        .filter(|(k, _)| !ANSWER_PARAMS.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}={}", lines::parameter(v)))
        .collect();
    for (k, v) in m
        .params
        .iter()
        .filter(|(k, _)| k == "PARTSTAT" || k == "RSVP")
    {
        params.push(format!("{k}={}", lines::parameter(v)));
    }
    let mut out = t.name.clone();
    for p in params {
        out.push(';');
        out.push_str(&p);
    }
    out.push(':');
    out.push_str(&t.value);
    out
}

fn cancelled(component: &lines::Component) -> bool {
    component
        .line("STATUS")
        .is_some_and(|l| l.value.trim().eq_ignore_ascii_case("CANCELLED"))
}

/// Re-applies `patch` on top of `theirs`, a newer upstream version of the item, with `own` the
/// addresses of the user who made the change. Group by group: what upstream left as it was in
/// the base takes the change's value; what upstream changed and the change did not keeps
/// upstream's; where both changed, upstream wins for `DTSTART`, `DTEND`, `DURATION`, `DUE`,
/// the recurrence, `STATUS:CANCELLED` and attendees (a conflict), and the change wins for
/// everything else. The user's own answer survives upstream rewriting the rest of their
/// attendee entry. Upstream's `DTSTAMP`, `SEQUENCE` and the like are kept. The same inputs
/// always give the same bytes.
pub fn rebase(patch: &Patch, theirs: &[u8], own: &[String]) -> Rebased {
    let kind = patch.kind;
    let Some(mut item) = Item::parse(kind, theirs) else {
        return Rebased::Unreadable;
    };
    if patch.components.is_empty() {
        return Rebased::Applied;
    }
    let mut applied = true;
    let mut conflicts: Vec<Field> = Vec::new();
    let mut obsolete = false;
    for component in &patch.components {
        let index = item
            .components()
            .iter()
            .position(|c| item.component_key(c) == component.key);
        let present = index.is_some();
        if present != component.in_mine {
            applied = false;
        }
        let theirs_groups = index
            .map(|i| model::groups(kind, item.components()[i]))
            .unwrap_or_default();
        let empty = Vec::new();
        if component.in_mine && present {
            for change in &component.groups {
                if !same(
                    &change.group,
                    theirs_groups.get(&change.group).unwrap_or(&empty),
                    &change.after,
                ) {
                    applied = false;
                }
            }
        }
        match (component.in_base, component.in_mine, index) {
            (_, true, None) if component.in_base => {
                if patch.class == Class::Reply {
                    obsolete = true;
                } else {
                    push_field(&mut conflicts, Field::Recurrence);
                }
            }
            (_, true, None) => {
                if let Some(template) = &component.template
                    && let Some(mut parsed) = lines::parse(template.as_bytes())
                    && let Some(new) = parsed.pop()
                {
                    insert_component(&mut item, new);
                }
            }
            (true, false, Some(_)) => {
                let unchanged = component.groups.iter().all(|c| {
                    same(
                        &c.group,
                        theirs_groups.get(&c.group).unwrap_or(&empty),
                        &c.before,
                    )
                }) && theirs_groups
                    .keys()
                    .all(|g| component.groups.iter().any(|c| &c.group == g));
                if unchanged {
                    remove_component(&mut item, &component.key);
                } else if patch.class == Class::Reply {
                    obsolete = true;
                } else {
                    push_field(&mut conflicts, Field::Recurrence);
                }
            }
            (false, false, _) | (true, false, None) => {}
            (_, true, Some(_)) => {
                let mut sets: Vec<(String, Vec<String>)> = Vec::new();
                for change in &component.groups {
                    let t = theirs_groups.get(&change.group).unwrap_or(&empty);
                    if same(&change.group, t, &change.before) {
                        sets.push((change.group.clone(), change.after.clone()));
                    } else if same(&change.group, t, &change.after) {
                    } else if is_own(&change.group, own)
                        && answer_only(&change.before, &change.after)
                    {
                        match (t.as_slice(), change.after.as_slice()) {
                            ([t], [m]) => {
                                sets.push((change.group.clone(), vec![with_answer(t, m)]))
                            }
                            _ => {
                                if patch.class == Class::Reply {
                                    obsolete = true;
                                } else {
                                    push_field(&mut conflicts, Field::Attendees);
                                }
                            }
                        }
                    } else if upstream_wins(&change.group, t) {
                        if patch.class == Class::Reply {
                            obsolete = true;
                        } else {
                            push_field(&mut conflicts, model::field(kind, &change.group));
                        }
                    } else {
                        sets.push((change.group.clone(), change.after.clone()));
                    }
                }
                let key = component.key.clone();
                if let Some(target) = component_mut(&mut item, &key) {
                    for (group, values) in &sets {
                        set_group(kind, target, group, values);
                    }
                    if patch.class == Class::Reply && cancelled(target) {
                        obsolete = true;
                    }
                }
            }
        }
    }
    if patch.class == Class::Reply
        && item
            .components()
            .iter()
            .any(|c| item.component_key(c).is_empty() && cancelled(c))
    {
        obsolete = true;
    }
    if applied && !obsolete && conflicts.is_empty() {
        return Rebased::Applied;
    }
    add_missing_zones(&mut item, &patch.zones);
    let served = item.write();
    if obsolete {
        Rebased::ReplyObsolete { served }
    } else if !conflicts.is_empty() {
        Rebased::Conflict {
            served,
            fields: conflicts,
        }
    } else {
        Rebased::Clean(served)
    }
}

fn component_mut<'a>(item: &'a mut Item, key: &str) -> Option<&'a mut lines::Component> {
    if item.is_contact() {
        return Some(&mut item.top);
    }
    let name = model::component_name(item.kind);
    item.top.children.iter_mut().find(|c| {
        c.name == name
            && c.line("RECURRENCE-ID")
                .map(model::instance_key)
                .unwrap_or_default()
                == key
    })
}

fn insert_component(item: &mut Item, component: lines::Component) {
    if item.is_contact() {
        item.top = component;
        return;
    }
    item.top.children.push(component);
}

fn remove_component(item: &mut Item, key: &str) {
    if item.is_contact() {
        return;
    }
    let name = model::component_name(item.kind);
    item.top.children.retain(|c| {
        !(c.name == name
            && c.line("RECURRENCE-ID")
                .map(model::instance_key)
                .unwrap_or_default()
                == key)
    });
}

/// The bytes a held change serves: `mine` (the change's own bytes, based on upstream version
/// `based_on`) when `latest` (the item's newest upstream version as iCalendar or vCard text,
/// with its number) is that same version, is missing, or there is no `patch`; otherwise the
/// patch re-applied on `latest` with [`rebase`] (`latest` itself when upstream already holds
/// the change, `mine` when `latest` cannot be read). The same inputs always give the same bytes.
pub fn serve(
    patch: Option<&Patch>,
    mine: &[u8],
    based_on: Option<u32>,
    latest: Option<(u32, &[u8])>,
    own: &[String],
) -> Vec<u8> {
    let (Some(patch), Some((number, theirs))) = (patch, latest) else {
        return mine.to_vec();
    };
    if based_on == Some(number) {
        return mine.to_vec();
    }
    match rebase(patch, theirs, own) {
        Rebased::Applied => theirs.to_vec(),
        Rebased::Unreadable => mine.to_vec(),
        rebased => rebased
            .served()
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| mine.to_vec()),
    }
}

/// The write-through form of a change: `served` (the bytes Holtmail serves for it, the
/// client's own or a rebase) with the fields `patch` touches. A create carries `fields: None`,
/// so a field-patching writer sends everything.
pub fn item_patch(patch: &Patch, served: &[u8]) -> ItemPatch {
    let uid = Item::parse(patch.kind, served).and_then(|i| single_uid(&i).ok());
    ItemPatch {
        content_type: patch.kind.content_type().to_owned(),
        raw: Bytes::copy_from_slice(served),
        uid,
        fields: (patch.class != Class::Create).then(|| patch.fields()),
    }
}
