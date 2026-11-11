use std::collections::HashMap;
use std::sync::Mutex;

use bytes::Bytes;
use holtmail_core::{ArchiveId, IdentityId, OutboxId, Timestamp, UserId};
use holtmail_mailview::EmailRef;
use holtmail_submission::{
    RecipientDelivery, SendIdentity, Submission, SubmissionError, SubmissionState, Submit,
    Submitted,
};
use serde_json::{Map, Value, json};

use crate::auth::Authed;
use crate::body;
use crate::email_set;
use crate::error::{MethodError, SetError};
use crate::ids;
use crate::json::Args;
use crate::request::Ctx;
use crate::time::{parse_date, utc_date};

struct Item {
    id: OutboxId,
    email: EmailRef,
    thread: String,
    identity: IdentityId,
    mail_from: String,
    created: u64,
    changed: u64,
    status: Submitted,
}

#[derive(Default)]
struct Log {
    seq: u64,
    items: Vec<Item>,
    destroyed: Vec<(OutboxId, u64)>,
}

/// The submissions made through this server, per user and archive, for
/// `EmailSubmission/get`, `/changes` and `/query`. States carry an epoch, so a state from
/// before a restart answers `cannotCalculateChanges`.
pub(crate) struct Registry {
    epoch: u32,
    logs: Mutex<HashMap<(UserId, ArchiveId), Log>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            epoch: (uuid::Uuid::now_v7().as_u128() as u32) | 1,
            logs: Mutex::new(HashMap::new()),
        }
    }
}

fn settled(state: SubmissionState) -> bool {
    !matches!(
        state,
        SubmissionState::Queued | SubmissionState::Sending | SubmissionState::Uncertain
    )
}

impl Registry {
    pub fn state(&self, user: UserId, archive: ArchiveId) -> String {
        let logs = self.logs.lock().expect("submissions");
        let seq = logs.get(&(user, archive)).map_or(0, |l| l.seq);
        format!("{}-{seq}", self.epoch)
    }

    fn parse(&self, state: &str) -> Option<u64> {
        let (epoch, seq) = state.split_once('-')?;
        (epoch.parse::<u32>().ok()? == self.epoch)
            .then(|| seq.parse().ok())
            .flatten()
    }

    /// Asks for the state of every submission not yet settled and records what moved.
    pub async fn refresh(&self, submission: &dyn Submission, authed: &Authed, archive: ArchiveId) {
        let pending: Vec<OutboxId> = {
            let logs = self.logs.lock().expect("submissions");
            logs.get(&(authed.principal.user, archive))
                .map(|l| {
                    l.items
                        .iter()
                        .filter(|i| !settled(i.status.state))
                        .map(|i| i.id)
                        .collect()
                })
                .unwrap_or_default()
        };
        for id in pending {
            let Ok(status) = submission.status(&authed.actor(), id).await else {
                continue;
            };
            self.update(authed.principal.user, archive, status);
        }
    }

    fn update(&self, user: UserId, archive: ArchiveId, status: Submitted) {
        let mut logs = self.logs.lock().expect("submissions");
        let log = logs.entry((user, archive)).or_default();
        let next = log.seq + 1;
        if let Some(item) = log.items.iter_mut().find(|i| i.id == status.id)
            && item.status != status
        {
            item.status = status;
            item.changed = next;
            log.seq = next;
        }
    }

    fn add(&self, user: UserId, archive: ArchiveId, mut item: Item) {
        let mut logs = self.logs.lock().expect("submissions");
        let log = logs.entry((user, archive)).or_default();
        if log.items.iter().any(|i| i.id == item.id) {
            return;
        }
        log.seq += 1;
        item.created = log.seq;
        item.changed = log.seq;
        log.items.push(item);
    }

    fn remove(&self, user: UserId, archive: ArchiveId, id: OutboxId) -> bool {
        let mut logs = self.logs.lock().expect("submissions");
        let log = logs.entry((user, archive)).or_default();
        let before = log.items.len();
        log.items.retain(|i| i.id != id);
        if log.items.len() == before {
            return false;
        }
        log.seq += 1;
        let seq = log.seq;
        log.destroyed.push((id, seq));
        true
    }

    fn with<T>(&self, user: UserId, archive: ArchiveId, f: impl FnOnce(&Log) -> T) -> T {
        let mut logs = self.logs.lock().expect("submissions");
        f(logs.entry((user, archive)).or_default())
    }
}

fn undo_status(state: SubmissionState) -> &'static str {
    match state {
        SubmissionState::Queued => "pending",
        SubmissionState::Cancelled => "canceled",
        _ => "final",
    }
}

fn object(item: &Item) -> Value {
    let mut delivery = Map::new();
    for recipient in &item.status.recipients {
        let (reply, delivered) = match &recipient.delivery {
            RecipientDelivery::Pending => ("250 2.0.0 Queued".to_string(), "queued"),
            RecipientDelivery::Accepted => ("250 2.0.0 Accepted".to_string(), "unknown"),
            RecipientDelivery::Refused { code, text, .. } => (format!("{code} {text}"), "no"),
            RecipientDelivery::Bounced { status, text } => (
                format!(
                    "550 {} {}",
                    status.as_deref().unwrap_or("5.0.0"),
                    text.as_deref().unwrap_or("Bounced")
                ),
                "no",
            ),
        };
        delivery.insert(
            recipient.address.clone(),
            json!({"smtpReply": reply, "delivered": delivered, "displayed": "unknown"}),
        );
    }
    json!({
        "id": ids::submission(item.id),
        "identityId": ids::identity(item.identity),
        "emailId": ids::email(&item.email),
        "threadId": item.thread,
        "envelope": {
            "mailFrom": {"email": item.mail_from, "parameters": null},
            "rcptTo": item.status.recipients.iter()
                .map(|r| json!({"email": r.address, "parameters": null}))
                .collect::<Vec<_>>(),
        },
        "sendAt": utc_date(item.status.send_at.as_millis()),
        "undoStatus": undo_status(item.status.state),
        "deliveryStatus": delivery,
        "dsnBlobIds": [],
        "mdnBlobIds": [],
    })
}

fn identity_object(identity: &SendIdentity) -> Value {
    json!({
        "id": ids::identity(identity.id),
        "name": identity.name.clone().unwrap_or_default(),
        "email": identity.address,
        "replyTo": null,
        "bcc": null,
        "textSignature": "",
        "htmlSignature": "",
        "mayDelete": false,
    })
}

async fn identities(ctx: &Ctx<'_>, archive: ArchiveId) -> Result<Vec<SendIdentity>, MethodError> {
    match ctx
        .inner
        .services
        .submission
        .identities(&ctx.authed.actor(), archive)
        .await
    {
        Ok(list) => Ok(list),
        Err(SubmissionError::Unavailable(reason)) => {
            tracing::warn!(%reason, "submission unavailable");
            Err(MethodError::new("serverUnavailable"))
        }
        Err(_) => Ok(Vec::new()),
    }
}

fn identity_state(list: &[SendIdentity]) -> String {
    let mut hasher = blake3::Hasher::new();
    for identity in list {
        hasher.update(identity.id.to_string().as_bytes());
        hasher.update(identity.address.as_bytes());
        hasher.update(identity.name.as_deref().unwrap_or_default().as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex()[..16].to_string()
}

pub(crate) async fn identity_get(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let list = identities(ctx, archive).await?;
    let wanted = args.strings("ids")?;
    let mut found = Vec::new();
    let mut not_found = Vec::new();
    match wanted {
        None => found = list.iter().map(identity_object).collect(),
        Some(wanted) => {
            for id in wanted {
                match list.iter().find(|i| Some(i.id) == ids::parse_identity(&id)) {
                    Some(identity) => found.push(identity_object(identity)),
                    None => not_found.push(json!(id)),
                }
            }
        }
    }
    let properties = args.strings("properties")?;
    let found: Vec<Value> = found
        .into_iter()
        .map(|o| match o {
            Value::Object(map) => crate::json::only(map, properties.as_deref()),
            other => other,
        })
        .collect();
    Ok(json!({
        "accountId": ids::account(archive),
        "state": identity_state(&list),
        "list": found,
        "notFound": not_found,
    }))
}

pub(crate) async fn identity_changes(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let list = identities(ctx, archive).await?;
    let state = identity_state(&list);
    let since = args.required_string("sinceState")?;
    if since != state {
        return Err(MethodError::new("cannotCalculateChanges"));
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": since,
        "newState": state,
        "hasMoreChanges": false,
        "created": [],
        "updated": [],
        "destroyed": [],
    }))
}

pub(crate) async fn identity_set(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let state = identity_state(&identities(ctx, archive).await?);
    let refuse = || SetError::with("forbidden", "identities are made in the web app").json();
    let keys = |key: &str| -> Result<Map<String, Value>, MethodError> {
        Ok(args
            .object(key)?
            .map(|m| m.keys().map(|k| (k.clone(), refuse())).collect())
            .unwrap_or_default())
    };
    let destroy: Map<String, Value> = args
        .strings("destroy")?
        .unwrap_or_default()
        .into_iter()
        .map(|k| (k, refuse()))
        .collect();
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": state,
        "newState": state,
        "notCreated": keys("create")?,
        "notUpdated": keys("update")?,
        "notDestroyed": destroy,
    }))
}

pub(crate) async fn get(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let user = ctx.authed.principal.user;
    let registry = &ctx.inner.sent;
    registry
        .refresh(&*ctx.inner.services.submission, ctx.authed, archive)
        .await;
    let wanted = args.strings("ids")?;
    let properties = args.strings("properties")?;
    let (list, not_found) = registry.with(user, archive, |log| {
        let mut list = Vec::new();
        let mut not_found = Vec::new();
        match &wanted {
            None => list = log.items.iter().map(object).collect(),
            Some(wanted) => {
                for id in wanted {
                    let resolved = ctx.resolve(id).and_then(ids::parse_submission);
                    match log.items.iter().find(|i| Some(i.id) == resolved) {
                        Some(item) => list.push(object(item)),
                        None => not_found.push(json!(id)),
                    }
                }
            }
        }
        (list, not_found)
    });
    let list: Vec<Value> = list
        .into_iter()
        .map(|o| match o {
            Value::Object(map) => crate::json::only(map, properties.as_deref()),
            other => other,
        })
        .collect();
    Ok(json!({
        "accountId": ids::account(archive),
        "state": registry.state(user, archive),
        "list": list,
        "notFound": not_found,
    }))
}

pub(crate) async fn changes(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let user = ctx.authed.principal.user;
    let registry = &ctx.inner.sent;
    registry
        .refresh(&*ctx.inner.services.submission, ctx.authed, archive)
        .await;
    let since_text = args.required_string("sinceState")?;
    let since = registry
        .parse(since_text)
        .ok_or_else(|| MethodError::new("cannotCalculateChanges"))?;
    let (created, updated, destroyed, seq) = registry.with(user, archive, |log| {
        let created: Vec<String> = log
            .items
            .iter()
            .filter(|i| i.created > since)
            .map(|i| ids::submission(i.id))
            .collect();
        let updated: Vec<String> = log
            .items
            .iter()
            .filter(|i| i.created <= since && i.changed > since)
            .map(|i| ids::submission(i.id))
            .collect();
        let destroyed: Vec<String> = log
            .destroyed
            .iter()
            .filter(|(_, at)| *at > since)
            .map(|(id, _)| ids::submission(*id))
            .collect();
        (created, updated, destroyed, log.seq)
    });
    if since > seq {
        return Err(MethodError::new("cannotCalculateChanges"));
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": since_text,
        "newState": registry.state(user, archive),
        "hasMoreChanges": false,
        "created": created,
        "updated": updated,
        "destroyed": destroyed,
    }))
}

fn keeps(filter: &Map<String, Value>, item: &Item) -> Result<bool, MethodError> {
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
            results.push(keeps(condition, item)?);
        }
        return match operator.as_str() {
            Some("AND") => Ok(results.iter().all(|r| *r)),
            Some("OR") => Ok(results.iter().any(|r| *r)),
            Some("NOT") => Ok(!results.iter().any(|r| *r)),
            _ => Err(MethodError::new("unsupportedFilter")),
        };
    }
    let in_list = |value: &Value, id: &str| {
        value
            .as_array()
            .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(id)))
    };
    for (key, value) in filter {
        let keep = match key.as_str() {
            "identityIds" => in_list(value, &ids::identity(item.identity)),
            "emailIds" => in_list(value, &ids::email(&item.email)),
            "threadIds" => in_list(value, &item.thread),
            "undoStatus" => value.as_str() == Some(undo_status(item.status.state)),
            "before" => value
                .as_str()
                .and_then(parse_date)
                .is_some_and(|d| item.status.send_at.as_millis() < d),
            "after" => value
                .as_str()
                .and_then(parse_date)
                .is_some_and(|d| item.status.send_at.as_millis() >= d),
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
    let user = ctx.authed.principal.user;
    let registry = &ctx.inner.sent;
    registry
        .refresh(&*ctx.inner.services.submission, ctx.authed, archive)
        .await;
    let empty = Map::new();
    let filter = args.object("filter")?.unwrap_or(&empty);
    let mut descending = false;
    if let Some(Value::Array(sort)) = args.get("sort") {
        match sort.as_slice() {
            [] => {}
            [one]
                if matches!(
                    one.get("property").and_then(Value::as_str),
                    Some("sentAt" | "sendAt")
                ) =>
            {
                descending = one.get("isAscending").and_then(Value::as_bool) == Some(false);
            }
            _ => return Err(MethodError::new("unsupportedSort")),
        }
    }
    let ids_found = registry.with(user, archive, |log| {
        let mut items: Vec<&Item> = Vec::new();
        for item in &log.items {
            if keeps(filter, item)? {
                items.push(item);
            }
        }
        items.sort_by_key(|i| (i.status.send_at, i.created));
        if descending {
            items.reverse();
        }
        Ok::<_, MethodError>(
            items
                .iter()
                .map(|i| ids::submission(i.id))
                .collect::<Vec<_>>(),
        )
    })?;
    let total = ids_found.len();
    let position = args.unsigned("position")?.unwrap_or(0) as usize;
    let limit = args.unsigned("limit")?.map(|l| l as usize).unwrap_or(total);
    let page: Vec<&String> = ids_found.iter().skip(position).take(limit).collect();
    let mut out = json!({
        "accountId": ids::account(archive),
        "queryState": registry.state(user, archive),
        "canCalculateChanges": false,
        "position": position.min(total),
        "ids": page,
    });
    if args.bool("calculateTotal", false)? {
        out["total"] = json!(total);
    }
    Ok(out)
}

/// The bytes to queue: without `Bcc`, with `Date` and `Message-ID` added when missing, every
/// line ending in CRLF and the last line ended.
pub(crate) fn prepare(raw: &[u8], domain: &str) -> Vec<u8> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 2)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|p| p + 1))
        .unwrap_or(raw.len());
    let (head, rest) = raw.split_at(split);
    let mut kept: Vec<u8> = Vec::with_capacity(raw.len());
    let mut skipping = false;
    let (mut has_date, mut has_id) = (false, false);
    for line in head.split_inclusive(|b| *b == b'\n') {
        let folded = line.first().is_some_and(|b| *b == b' ' || *b == b'\t');
        if !folded {
            let name = line
                .split(|b| *b == b':')
                .next()
                .map(|n| String::from_utf8_lossy(n).trim().to_ascii_lowercase())
                .unwrap_or_default();
            skipping = name == "bcc" || name == "resent-bcc";
            has_date |= name == "date";
            has_id |= name == "message-id";
        }
        if !skipping {
            kept.extend_from_slice(line);
        }
    }
    let mut out = Vec::with_capacity(raw.len() + 128);
    if !has_date {
        let date =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(Timestamp::now().as_millis())
                .unwrap_or_default()
                .to_rfc2822();
        out.extend_from_slice(format!("Date: {date}\r\n").as_bytes());
    }
    if !has_id {
        out.extend_from_slice(
            format!("Message-ID: <{}@{domain}>\r\n", uuid::Uuid::now_v7()).as_bytes(),
        );
    }
    out.extend_from_slice(&kept);
    out.extend_from_slice(rest);
    crlf(out)
}

fn crlf(raw: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 2);
    let mut bytes = raw.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' if bytes.peek() == Some(&b'\n') => {}
            b'\r' | b'\n' => out.extend_from_slice(b"\r\n"),
            other => out.push(other),
        }
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn address_of(value: Option<&Value>) -> Option<String> {
    value?.get("email")?.as_str().map(str::to_string)
}

async fn create(
    ctx: &Ctx<'_>,
    archive: ArchiveId,
    cid: &str,
    object: &Map<String, Value>,
) -> Result<Item, SetError> {
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let identity_id = object
        .get("identityId")
        .and_then(Value::as_str)
        .and_then(|i| ctx.resolve(i))
        .and_then(ids::parse_identity)
        .ok_or_else(|| SetError::properties("identityId is required", &["identityId"]))?;
    let email_id = object
        .get("emailId")
        .and_then(Value::as_str)
        .and_then(|i| ctx.resolve(i))
        .ok_or_else(|| SetError::properties("emailId is required", &["emailId"]))?
        .to_string();
    let email = ids::parse_email(&email_id)
        .ok_or_else(|| SetError::properties("no such email", &["emailId"]))?;
    let identity = identities(ctx, archive)
        .await
        .map_err(|e| SetError::with("serverFail", e.kind))?
        .into_iter()
        .find(|i| i.id == identity_id)
        .ok_or_else(|| SetError::with("forbiddenFrom", "you may not send as this identity"))?;
    let raw = view.raw(&viewer, &email).await?;
    let envelope = view.envelopes(&viewer, &[email]).await?.pop();
    let thread = envelope
        .as_ref()
        .map(crate::email::thread_id)
        .unwrap_or_else(|| format!("T{email_id}"));
    let (mail_from, recipients) = match object.get("envelope").filter(|e| !e.is_null()) {
        Some(given) => {
            let from = address_of(given.get("mailFrom"))
                .ok_or_else(|| SetError::properties("envelope needs mailFrom", &["envelope"]))?;
            let to: Vec<String> = given
                .get("rcptTo")
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(|r| address_of(Some(r))).collect())
                .unwrap_or_default();
            (from, to)
        }
        None => {
            let root = view.structure(&viewer, &email).await?;
            let fields = body::fields(&raw[..(root.header.end as usize).min(raw.len())]);
            let mut to = Vec::new();
            for wanted in ["to", "cc", "bcc"] {
                for (name, value) in &fields {
                    if name.eq_ignore_ascii_case(wanted)
                        && let Value::Array(list) = body::address_list(value)
                    {
                        to.extend(
                            list.iter()
                                .filter_map(|a| a.get("email").and_then(Value::as_str))
                                .map(str::to_string),
                        );
                    }
                }
            }
            (identity.address.clone(), to)
        }
    };
    let mut seen = std::collections::HashSet::new();
    let recipients: Vec<String> = recipients
        .into_iter()
        .filter(|r| seen.insert(r.to_ascii_lowercase()))
        .collect();
    if recipients.is_empty() {
        return Err(SetError::with(
            "noRecipients",
            "the email has no recipients",
        ));
    }
    let domain = identity
        .address
        .rsplit_once('@')
        .map(|(_, d)| d.to_string())
        .unwrap_or_else(|| "localhost".into());
    let message = Bytes::from(prepare(&raw, &domain));
    let mut key = blake3::Hasher::new();
    key.update(ctx.authed.principal.device.to_string().as_bytes());
    key.update(&[0]);
    key.update(email_id.as_bytes());
    key.update(&[0]);
    key.update(cid.as_bytes());
    let request = Submit {
        actor: ctx.authed.actor(),
        archive,
        identity: identity.id,
        mail_from: mail_from.clone(),
        recipients,
        message,
        send_at: None,
        undo_window: ctx.inner.settings.undo_window,
        idempotency_key: Some(format!("jmap:{}", key.finalize().to_hex())),
        allow_leaks: false,
        route: None,
        hold: false,
    };
    let status = ctx.inner.services.submission.queue(request).await?;
    Ok(Item {
        id: status.id,
        email,
        thread,
        identity: identity.id,
        mail_from,
        created: 0,
        changed: 0,
        status,
    })
}

pub(crate) async fn set(
    ctx: &mut Ctx<'_>,
    args: &Args<'_>,
) -> Result<Vec<(String, Value)>, MethodError> {
    let archive = ctx.account(args)?;
    let user = ctx.authed.principal.user;
    let inner = ctx.inner;
    inner
        .sent
        .refresh(&*inner.services.submission, ctx.authed, archive)
        .await;
    let old = inner.sent.state(user, archive);
    if let Some(expected) = args.string("ifInState")?
        && expected != old
    {
        return Err(MethodError::new("stateMismatch"));
    }
    let create_args = args.object("create")?.cloned().unwrap_or_default();
    let update_args = args.object("update")?.cloned().unwrap_or_default();
    let destroy_args = args.strings("destroy")?.unwrap_or_default();
    if create_args.len() + update_args.len() + destroy_args.len()
        > inner.settings.limits.max_objects_in_set
    {
        return Err(MethodError::new("requestTooLarge"));
    }
    let mut created = Map::new();
    let mut not_created = Map::new();
    let mut emails_by_creation: HashMap<String, EmailRef> = HashMap::new();
    for (cid, object) in &create_args {
        let Some(object) = object.as_object() else {
            not_created.insert(cid.clone(), SetError::new("invalidProperties").json());
            continue;
        };
        match create(ctx, archive, cid, object).await {
            Ok(item) => {
                let id = ids::submission(item.id);
                created.insert(
                    cid.clone(),
                    json!({
                        "id": id,
                        "threadId": item.thread,
                        "sendAt": utc_date(item.status.send_at.as_millis()),
                        "undoStatus": undo_status(item.status.state),
                    }),
                );
                emails_by_creation.insert(cid.clone(), item.email);
                ctx.created.insert(cid.clone(), id);
                inner.sent.add(user, archive, item);
            }
            Err(error) => {
                not_created.insert(cid.clone(), error.json());
            }
        }
    }
    let mut updated = Map::new();
    let mut not_updated = Map::new();
    for (id, patch) in &update_args {
        let Some(outbox) = ctx.resolve(id).and_then(ids::parse_submission) else {
            not_updated.insert(id.clone(), SetError::new("notFound").json());
            continue;
        };
        let known = inner.sent.with(user, archive, |log| {
            log.items.iter().any(|i| i.id == outbox)
        });
        if !known {
            not_updated.insert(id.clone(), SetError::new("notFound").json());
            continue;
        }
        let patch = patch.as_object().cloned().unwrap_or_default();
        if patch.keys().any(|k| k != "undoStatus")
            || patch.get("undoStatus").and_then(Value::as_str) != Some("canceled")
        {
            not_updated.insert(
                id.clone(),
                SetError::properties("only undoStatus can become canceled", &["undoStatus"]).json(),
            );
            continue;
        }
        match inner
            .services
            .submission
            .undo(&ctx.authed.actor(), outbox)
            .await
        {
            Ok(status) => {
                inner.sent.update(user, archive, status);
                updated.insert(id.clone(), Value::Null);
            }
            Err(error) => {
                not_updated.insert(id.clone(), SetError::from(error).json());
            }
        }
    }
    let mut destroyed = Vec::new();
    let mut not_destroyed = Map::new();
    for id in &destroy_args {
        match ctx.resolve(id).and_then(ids::parse_submission) {
            Some(outbox) if inner.sent.remove(user, archive, outbox) => destroyed.push(json!(id)),
            _ => {
                not_destroyed.insert(id.clone(), SetError::new("notFound").json());
            }
        }
    }
    let new = inner.sent.state(user, archive);
    let response = json!({
        "accountId": ids::account(archive),
        "oldState": old,
        "newState": new,
        "created": if created.is_empty() { Value::Null } else { Value::Object(created) },
        "updated": if updated.is_empty() { Value::Null } else { Value::Object(updated) },
        "destroyed": if destroyed.is_empty() { Value::Null } else { Value::Array(destroyed) },
        "notCreated": if not_created.is_empty() { Value::Null } else { Value::Object(not_created) },
        "notUpdated": if not_updated.is_empty() { Value::Null } else { Value::Object(not_updated) },
        "notDestroyed": if not_destroyed.is_empty() { Value::Null } else { Value::Object(not_destroyed) },
    });
    let mut answers = vec![("EmailSubmission/set".to_string(), response)];
    let email_of = |key: &str| -> Option<EmailRef> {
        match key.strip_prefix('#') {
            Some(cid) => emails_by_creation.get(cid).copied(),
            None => {
                let outbox = ids::parse_submission(key)?;
                inner.sent.with(user, archive, |log| {
                    log.items.iter().find(|i| i.id == outbox).map(|i| i.email)
                })
            }
        }
    };
    let mut email_updates = Map::new();
    if let Some(on_update) = args.object("onSuccessUpdateEmail")? {
        for (key, patch) in on_update {
            if let Some(email) = email_of(key) {
                email_updates.insert(ids::email(&email), patch.clone());
            }
        }
    }
    let mut email_destroys = Vec::new();
    for key in args.strings("onSuccessDestroyEmail")?.unwrap_or_default() {
        if let Some(email) = email_of(&key) {
            email_destroys.push(ids::email(&email));
        }
    }
    if !email_updates.is_empty() || !email_destroys.is_empty() {
        let implicit = email_set::apply(
            ctx,
            archive,
            None,
            None,
            (!email_updates.is_empty()).then_some(email_updates),
            (!email_destroys.is_empty()).then_some(email_destroys),
        )
        .await?;
        answers.push(("Email/set".to_string(), implicit));
    }
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_drops_bcc_and_adds_fields() {
        let raw = b"From: ada@example.com\r\nBcc: secret@example.org,\r\n other@example.org\r\nTo: bob@example.org\r\n\r\nHi\r\n";
        let out = String::from_utf8(prepare(raw, "example.com")).unwrap();
        assert!(!out.to_ascii_lowercase().contains("bcc"));
        assert!(!out.contains("other@example.org"));
        assert!(out.starts_with("Date: "));
        assert!(out.contains("Message-ID: <"));
        assert!(out.contains("@example.com>\r\n"));
        assert!(out.ends_with("To: bob@example.org\r\n\r\nHi\r\n"));
        let kept =
            b"Date: Mon, 02 Nov 2026 09:00:00 +0000\r\nMessage-ID: <x@example.com>\r\n\r\nB\r\n";
        assert_eq!(prepare(kept, "example.com"), kept.to_vec());
        let bare = b"Date: Mon, 02 Nov 2026 09:00:00 +0000\nMessage-ID: <x@example.com>\n\nA\rB";
        assert_eq!(
            prepare(bare, "example.com"),
            b"Date: Mon, 02 Nov 2026 09:00:00 +0000\r\nMessage-ID: <x@example.com>\r\n\r\nA\r\nB\r\n".to_vec()
        );
    }
}
