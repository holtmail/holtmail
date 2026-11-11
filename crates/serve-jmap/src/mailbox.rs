use std::collections::HashMap;

use holtmail_mailview::{FolderRole, Mailbox, MailboxId, Viewer};
use serde_json::{Map, Value, json};

use crate::error::{MethodError, SetError};
use crate::ids;
use crate::json::{Args, only};
use crate::request::Ctx;

const COUNTS: &[&str] = &[
    "totalEmails",
    "unreadEmails",
    "totalThreads",
    "unreadThreads",
];

fn sort_order(mailbox: &Mailbox) -> u32 {
    match (&mailbox.id, mailbox.role) {
        (_, Some(FolderRole::Inbox)) => 1,
        (MailboxId::Drafts, _) | (_, Some(FolderRole::Drafts)) => 2,
        (_, Some(FolderRole::Sent)) => 3,
        (_, Some(FolderRole::Archive)) => 4,
        (_, Some(FolderRole::Junk)) => 5,
        (_, Some(FolderRole::Trash)) => 6,
        (MailboxId::All, _) => 7,
        (MailboxId::Kept, _) => 8,
        _ => 10,
    }
}

pub(crate) fn parent_ids(tree: &[Mailbox]) -> HashMap<MailboxId, Option<String>> {
    let by_path: HashMap<String, &MailboxId> =
        tree.iter().map(|m| (m.path.join("/"), &m.id)).collect();
    tree.iter()
        .map(|m| {
            let parent = (m.path.len() > 1)
                .then(|| by_path.get(&m.path[..m.path.len() - 1].join("/")))
                .flatten()
                .map(|id| ids::mailbox(id));
            (m.id.clone(), parent)
        })
        .collect()
}

pub(crate) async fn may_submit(ctx: &Ctx<'_>, viewer: &Viewer) -> bool {
    ctx.inner
        .services
        .submission
        .identities(&ctx.authed.actor(), viewer.archive)
        .await
        .map(|list| !list.is_empty())
        .unwrap_or(false)
}

async fn object(
    ctx: &Ctx<'_>,
    viewer: &Viewer,
    mailbox: &Mailbox,
    parents: &HashMap<MailboxId, Option<String>>,
    properties: Option<&[String]>,
    may_submit: bool,
) -> Result<Value, MethodError> {
    let wants = |p: &str| properties.is_none_or(|list| list.iter().any(|x| x == p));
    let mut out = Map::new();
    out.insert("id".into(), json!(ids::mailbox(&mailbox.id)));
    out.insert(
        "name".into(),
        json!(mailbox.path.last().cloned().unwrap_or_default()),
    );
    out.insert(
        "parentId".into(),
        json!(parents.get(&mailbox.id).cloned().flatten()),
    );
    out.insert("role".into(), json!(mailbox.role.map(ids::role_name)));
    out.insert("sortOrder".into(), json!(sort_order(mailbox)));
    if COUNTS.iter().any(|p| wants(p)) {
        let (total, unread) = if mailbox.selectable {
            let status = ctx.inner.services.view.status(viewer, &mailbox.id).await?;
            (status.messages, status.unseen)
        } else {
            (0, 0)
        };
        out.insert("totalEmails".into(), json!(total));
        out.insert("unreadEmails".into(), json!(unread));
        out.insert("totalThreads".into(), json!(total));
        out.insert("unreadThreads".into(), json!(unread));
    }
    let rights = mailbox.rights;
    out.insert(
        "myRights".into(),
        json!({
            "mayReadItems": rights.read,
            "mayAddItems": rights.add,
            "mayRemoveItems": rights.remove,
            "maySetSeen": rights.set_seen,
            "maySetKeywords": rights.set_keywords,
            "mayCreateChild": false,
            "mayRename": false,
            "mayDelete": false,
            "maySubmit": may_submit && rights.read,
        }),
    );
    out.insert("isSubscribed".into(), json!(mailbox.subscribed));
    Ok(only(out, properties))
}

pub(crate) async fn get(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let state = view.state(&viewer).await?;
    let wanted = args.strings("ids")?;
    if wanted
        .as_ref()
        .is_some_and(|w| w.len() > ctx.inner.settings.limits.max_objects_in_get)
    {
        return Err(MethodError::new("requestTooLarge"));
    }
    let properties = args.strings("properties")?;
    let tree = view.tree(&viewer).await?;
    let parents = parent_ids(&tree);
    let submit = if properties
        .as_ref()
        .is_none_or(|p| p.iter().any(|x| x == "myRights"))
    {
        may_submit(ctx, &viewer).await
    } else {
        false
    };
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    match &wanted {
        None => {
            for mailbox in &tree {
                list.push(
                    object(
                        ctx,
                        &viewer,
                        mailbox,
                        &parents,
                        properties.as_deref(),
                        submit,
                    )
                    .await?,
                );
            }
        }
        Some(wanted) => {
            for id in wanted {
                let found = ctx
                    .resolve(id)
                    .and_then(ids::parse_mailbox)
                    .and_then(|m| tree.iter().find(|t| t.id == m));
                match found {
                    Some(mailbox) => list.push(
                        object(
                            ctx,
                            &viewer,
                            mailbox,
                            &parents,
                            properties.as_deref(),
                            submit,
                        )
                        .await?,
                    ),
                    None => not_found.push(json!(id)),
                }
            }
        }
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "state": state.mailbox.to_string(),
        "list": list,
        "notFound": not_found,
    }))
}

pub(crate) fn since(args: &Args<'_>) -> Result<u64, MethodError> {
    args.required_string("sinceState")?
        .parse()
        .map_err(|_| MethodError::new("cannotCalculateChanges"))
}

pub(crate) fn max_changes(args: &Args<'_>) -> Result<u32, MethodError> {
    match args.unsigned("maxChanges")? {
        Some(0) => Err(MethodError::invalid("maxChanges must be positive")),
        Some(n) => Ok(n.min(5_000) as u32),
        None => Ok(5_000),
    }
}

pub(crate) async fn changes(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let since = since(args)?;
    let limit = max_changes(args)?;
    let state = view.state(&viewer).await?;
    let window = view.changes(&viewer, since, limit).await?;
    let new_state = if window.more {
        window.upto
    } else {
        state.mailbox.max(since)
    };
    let key = |list: &[MailboxId]| list.iter().map(ids::mailbox).collect::<Vec<_>>();
    let mut updated = key(&window.mailboxes.updated);
    let mut counts_only = false;
    if window.counts_changed {
        counts_only = window.mailboxes.is_empty();
        for mailbox in view.tree(&viewer).await? {
            let id = ids::mailbox(&mailbox.id);
            if mailbox.selectable
                && !window.mailboxes.created.contains(&mailbox.id)
                && !updated.contains(&id)
            {
                updated.push(id);
            }
        }
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": since.to_string(),
        "newState": new_state.to_string(),
        "hasMoreChanges": window.more,
        "created": key(&window.mailboxes.created),
        "updated": updated,
        "destroyed": key(&window.mailboxes.destroyed),
        "updatedProperties": if counts_only { json!(COUNTS) } else { Value::Null },
    }))
}

fn matches(
    filter: &Map<String, Value>,
    mailbox: &Mailbox,
    parent: Option<&str>,
) -> Result<bool, MethodError> {
    if let Some(operator) = filter.get("operator") {
        let conditions = filter
            .get("conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| MethodError::invalid("conditions are missing"))?;
        let mut results = Vec::new();
        for condition in conditions {
            let condition = condition
                .as_object()
                .ok_or_else(|| MethodError::invalid("a condition is not an object"))?;
            results.push(matches(condition, mailbox, parent)?);
        }
        return match operator.as_str() {
            Some("AND") => Ok(results.iter().all(|r| *r)),
            Some("OR") => Ok(results.iter().any(|r| *r)),
            Some("NOT") => Ok(!results.iter().any(|r| *r)),
            _ => Err(MethodError::new("unsupportedFilter")),
        };
    }
    for (key, value) in filter {
        let keep = match key.as_str() {
            "parentId" => match value {
                Value::Null => parent.is_none(),
                Value::String(id) => parent == Some(id.as_str()),
                _ => return Err(MethodError::invalid("parentId must be a string or null")),
            },
            "name" => {
                let name = mailbox
                    .path
                    .last()
                    .map(|n| n.to_lowercase())
                    .unwrap_or_default();
                let wanted = value.as_str().unwrap_or_default().to_lowercase();
                name.contains(&wanted)
            }
            "role" => match value {
                Value::Null => mailbox.role.is_none(),
                Value::String(role) => mailbox.role.map(ids::role_name) == Some(role.as_str()),
                _ => return Err(MethodError::invalid("role must be a string or null")),
            },
            "hasAnyRole" => mailbox.role.is_some() == (value == &Value::Bool(true)),
            "isSubscribed" => mailbox.subscribed == (value == &Value::Bool(true)),
            _ => return Err(MethodError::new("unsupportedFilter")),
        };
        if !keep {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) async fn query(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let state = view.state(&viewer).await?;
    let tree = view.tree(&viewer).await?;
    let parents = parent_ids(&tree);
    let empty = Map::new();
    let filter = args.object("filter")?.unwrap_or(&empty);
    let mut list: Vec<&Mailbox> = Vec::new();
    for mailbox in &tree {
        let parent = parents.get(&mailbox.id).cloned().flatten();
        if matches(filter, mailbox, parent.as_deref())? {
            list.push(mailbox);
        }
    }
    if let Some(Value::Array(sort)) = args.get("sort") {
        for comparator in sort.iter().rev() {
            let property = comparator.get("property").and_then(Value::as_str);
            let ascending = comparator
                .get("isAscending")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            match property {
                Some("sortOrder") => list.sort_by_key(|m| sort_order(m)),
                Some("name") => list.sort_by_key(|m| m.path.last().map(|n| n.to_lowercase())),
                _ => return Err(MethodError::new("unsupportedSort")),
            }
            if !ascending {
                list.reverse();
            }
        }
    }
    let ids: Vec<String> = list.iter().map(|m| ids::mailbox(&m.id)).collect();
    let total = ids.len();
    let position = args.int("position")?.unwrap_or(0);
    let start = if position < 0 {
        total.saturating_sub(position.unsigned_abs() as usize)
    } else {
        (position as usize).min(total)
    };
    let limit = args.unsigned("limit")?.map(|l| l as usize).unwrap_or(total);
    let page: Vec<String> = ids.into_iter().skip(start).take(limit).collect();
    let mut out = json!({
        "accountId": ids::account(archive),
        "queryState": state.mailbox.to_string(),
        "canCalculateChanges": false,
        "position": start,
        "ids": page,
    });
    if args.bool("calculateTotal", false)? {
        out["total"] = json!(total);
    }
    Ok(out)
}

pub(crate) async fn set(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let actions = &ctx.inner.services.actions;
    let old = view.state(&viewer).await?.mailbox;
    if let Some(expected) = args.string("ifInState")?
        && expected != old.to_string()
    {
        return Err(MethodError::new("stateMismatch"));
    }
    let create = args.object("create")?;
    let update = args.object("update")?;
    let destroy = args.strings("destroy")?;
    let count = create.map_or(0, |c| c.len())
        + update.map_or(0, |u| u.len())
        + destroy.as_ref().map_or(0, |d| d.len());
    if count > ctx.inner.settings.limits.max_objects_in_set {
        return Err(MethodError::new("requestTooLarge"));
    }
    let mut not_created = Map::new();
    for key in create.into_iter().flat_map(|c| c.keys()) {
        not_created.insert(
            key.clone(),
            SetError::with("forbidden", "mailboxes come from the mail accounts").json(),
        );
    }
    let mut not_destroyed = Map::new();
    for id in destroy.iter().flatten() {
        not_destroyed.insert(
            id.clone(),
            SetError::with("forbidden", "mailboxes come from the mail accounts").json(),
        );
    }
    let tree = view.tree(&viewer).await?;
    let mut updated = Map::new();
    let mut not_updated = Map::new();
    for (id, patch) in update.into_iter().flatten() {
        let Some(mailbox) = ctx
            .resolve(id)
            .and_then(ids::parse_mailbox)
            .and_then(|m| tree.iter().find(|t| t.id == m))
        else {
            not_updated.insert(id.clone(), SetError::new("notFound").json());
            continue;
        };
        let Some(patch) = patch.as_object() else {
            not_updated.insert(id.clone(), SetError::new("invalidPatch").json());
            continue;
        };
        if let Some(other) = patch.keys().find(|k| k.as_str() != "isSubscribed") {
            not_updated.insert(
                id.clone(),
                SetError::properties(
                    "only isSubscribed can be changed; mailboxes come from the mail accounts",
                    &[other.as_str()],
                )
                .json(),
            );
            continue;
        }
        if let Some(subscribed) = patch.get("isSubscribed") {
            let Some(subscribed) = subscribed.as_bool() else {
                not_updated.insert(
                    id.clone(),
                    SetError::properties("isSubscribed must be true or false", &["isSubscribed"])
                        .json(),
                );
                continue;
            };
            if let Err(error) = actions.subscribe(&viewer, &mailbox.id, subscribed).await {
                not_updated.insert(id.clone(), SetError::from(error).json());
                continue;
            }
        }
        updated.insert(id.clone(), Value::Null);
    }
    let new = view.state(&viewer).await?.mailbox;
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": old.to_string(),
        "newState": new.to_string(),
        "created": null,
        "updated": if updated.is_empty() { Value::Null } else { Value::Object(updated) },
        "destroyed": null,
        "notCreated": if not_created.is_empty() { Value::Null } else { Value::Object(not_created) },
        "notUpdated": if not_updated.is_empty() { Value::Null } else { Value::Object(not_updated) },
        "notDestroyed": if not_destroyed.is_empty() { Value::Null } else { Value::Object(not_destroyed) },
    }))
}
