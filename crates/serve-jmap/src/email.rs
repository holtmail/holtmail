use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use holtmail_core::{Address, Timestamp};
use holtmail_mailview::{EmailRef, Envelope, Filter, MailboxId, TextField, Viewer};
use serde_json::{Map, Value, json};

use crate::body::{self, HeaderProperty};
use crate::error::MethodError;
use crate::ids;
use crate::json::{Args, only};
use crate::mailbox::{max_changes, since};
use crate::request::Ctx;
use crate::time::{parse_date, utc_date};

const DEFAULT_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "messageId",
    "inReplyTo",
    "references",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "sentAt",
    "hasAttachment",
    "preview",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
];

const RAW_PROPERTIES: &[&str] = &[
    "messageId",
    "inReplyTo",
    "references",
    "headers",
    "bodyStructure",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
];

const ENVELOPE_CHUNK: usize = 500;
const QUERY_CACHE: usize = 32;

/// The JMAP keyword for an IMAP flag; `None` for flags JMAP has no keyword for.
pub(crate) fn keyword(flag: &str) -> Option<String> {
    let lower = flag.to_ascii_lowercase();
    match lower.as_str() {
        "\\seen" => Some("$seen".into()),
        "\\flagged" => Some("$flagged".into()),
        "\\answered" => Some("$answered".into()),
        "\\draft" => Some("$draft".into()),
        _ if lower.starts_with('\\') => None,
        _ => Some(lower),
    }
}

/// The IMAP flag for a JMAP keyword.
pub(crate) fn flag(keyword: &str) -> String {
    match keyword.to_ascii_lowercase().as_str() {
        "$seen" => "\\Seen".into(),
        "$flagged" => "\\Flagged".into(),
        "$answered" => "\\Answered".into(),
        "$draft" => "\\Draft".into(),
        _ => keyword.to_string(),
    }
}

pub(crate) fn valid_keyword(keyword: &str) -> bool {
    !keyword.is_empty()
        && keyword.len() <= 255
        && keyword
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !b"()]{%*\"\\".contains(&b))
}

pub(crate) fn thread_id(envelope: &Envelope) -> String {
    match envelope.thread {
        Some(thread) => ids::thread(thread),
        None => format!("T{}", ids::email(&envelope.email)),
    }
}

fn addresses(list: &[Address]) -> Value {
    if list.is_empty() {
        return Value::Null;
    }
    Value::Array(
        list.iter()
            .map(|a| json!({"name": a.name, "email": a.email}))
            .collect(),
    )
}

fn keywords(flags: &[String]) -> Value {
    let mut out = Map::new();
    for flag in flags {
        if let Some(keyword) = keyword(flag) {
            out.insert(keyword, json!(true));
        }
    }
    Value::Object(out)
}

pub(crate) async fn get(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let state = view.state(&viewer).await?;
    let Some(wanted) = args.strings("ids")? else {
        return Err(MethodError::with(
            "requestTooLarge",
            "Email/get needs ids; find them with Email/query",
        ));
    };
    if wanted.len() > ctx.inner.settings.limits.max_objects_in_get {
        return Err(MethodError::new("requestTooLarge"));
    }
    let properties: Vec<String> = args
        .strings("properties")?
        .unwrap_or_else(|| DEFAULT_PROPERTIES.iter().map(|p| p.to_string()).collect());
    let body_properties: Vec<String> = args.strings("bodyProperties")?.unwrap_or_else(|| {
        body::DEFAULT_BODY_PROPERTIES
            .iter()
            .map(|p| p.to_string())
            .collect()
    });
    let fetch_text = args.bool("fetchTextBodyValues", false)?;
    let fetch_html = args.bool("fetchHTMLBodyValues", false)?;
    let fetch_all = args.bool("fetchAllBodyValues", false)?;
    let max_bytes = args.unsigned("maxBodyValueBytes")?;
    let mut refs = Vec::new();
    let mut not_found = Vec::new();
    for id in &wanted {
        match ctx.resolve(id).and_then(ids::parse_email) {
            Some(email) => refs.push((id.clone(), email)),
            None => not_found.push(json!(id)),
        }
    }
    let emails: Vec<EmailRef> = refs.iter().map(|(_, e)| *e).collect();
    let envelopes: HashMap<EmailRef, Envelope> = view
        .envelopes(&viewer, &emails)
        .await?
        .into_iter()
        .map(|e| (e.email, e))
        .collect();
    let flags = if properties.iter().any(|p| p == "keywords") {
        view.flags(&viewer, None, &emails).await?
    } else {
        HashMap::new()
    };
    let needs_raw = properties
        .iter()
        .any(|p| RAW_PROPERTIES.contains(&p.as_str()) || p.starts_with("header:"));
    let mut list = Vec::new();
    for (id, email) in refs {
        let Some(envelope) = envelopes.get(&email) else {
            not_found.push(json!(id));
            continue;
        };
        let mut out = Map::new();
        out.insert("id".into(), json!(ids::email(&email)));
        out.insert("blobId".into(), json!(ids::blob(&envelope.blob)));
        out.insert("threadId".into(), json!(thread_id(envelope)));
        let mailboxes: Map<String, Value> = envelope
            .mailboxes
            .iter()
            .map(|m| (ids::mailbox(m), json!(true)))
            .collect();
        out.insert("mailboxIds".into(), Value::Object(mailboxes));
        out.insert(
            "keywords".into(),
            keywords(flags.get(&email).map(Vec::as_slice).unwrap_or_default()),
        );
        out.insert("size".into(), json!(envelope.size));
        out.insert(
            "receivedAt".into(),
            json!(utc_date(envelope.received_at.as_millis())),
        );
        out.insert(
            "messageId".into(),
            json!(envelope.rfc_message_id.as_ref().map(|m| vec![m])),
        );
        out.insert(
            "inReplyTo".into(),
            json!(envelope.in_reply_to.as_ref().map(|m| vec![m])),
        );
        out.insert("references".into(), Value::Null);
        out.insert("sender".into(), addresses(&envelope.sender));
        out.insert("from".into(), addresses(&envelope.from));
        out.insert("to".into(), addresses(&envelope.to));
        out.insert("cc".into(), addresses(&envelope.cc));
        out.insert("bcc".into(), addresses(&envelope.bcc));
        out.insert("replyTo".into(), addresses(&envelope.reply_to));
        out.insert("subject".into(), json!(envelope.subject));
        out.insert(
            "sentAt".into(),
            json!(envelope.sent_at.map(|t| utc_date(t.as_millis()))),
        );
        out.insert("hasAttachment".into(), json!(envelope.has_attachment));
        out.insert("preview".into(), json!(envelope.preview));
        if needs_raw {
            let raw = view.raw(&viewer, &email).await?;
            let root = view.structure(&viewer, &email).await?;
            let header_fields = body::fields(&raw[..(root.header.end as usize).min(raw.len())]);
            for (property, name) in [
                ("messageId", "header:Message-ID:asMessageIds"),
                ("inReplyTo", "header:In-Reply-To:asMessageIds"),
                ("references", "header:References:asMessageIds"),
            ] {
                if let Some(header) = HeaderProperty::parse(name) {
                    out.insert(property.into(), header.value(&header_fields));
                }
            }
            out.insert(
                "headers".into(),
                Value::Array(
                    header_fields
                        .iter()
                        .map(|(n, v)| json!({"name": n, "value": v}))
                        .collect(),
                ),
            );
            for property in &properties {
                if let Some(header) = HeaderProperty::parse(property) {
                    out.insert(property.clone(), header.value(&header_fields));
                }
            }
            let lists = body::lists(&root);
            let parts = |list: &[&holtmail_mailview::Part]| {
                Value::Array(
                    list.iter()
                        .map(|p| body::part_json(&email, &raw, p, &body_properties))
                        .collect(),
                )
            };
            out.insert("textBody".into(), parts(&lists.text));
            out.insert("htmlBody".into(), parts(&lists.html));
            out.insert("attachments".into(), parts(&lists.attachments));
            let mut sub_properties = body_properties.clone();
            if !sub_properties.iter().any(|p| p == "subParts") {
                sub_properties.push("subParts".into());
            }
            out.insert(
                "bodyStructure".into(),
                body::part_json(&email, &raw, &root, &sub_properties),
            );
            let mut values = Map::new();
            let mut add = |part: &holtmail_mailview::Part| {
                if part.content_type.starts_with("text/") {
                    values
                        .entry(body::part_id(part))
                        .or_insert_with(|| body::body_value(&raw, part, max_bytes));
                }
            };
            if fetch_text || fetch_all {
                lists.text.iter().for_each(|p| add(p));
            }
            if fetch_html || fetch_all {
                lists.html.iter().for_each(|p| add(p));
            }
            if fetch_all {
                lists.attachments.iter().for_each(|p| add(p));
            }
            out.insert("bodyValues".into(), Value::Object(values));
        }
        list.push(only(out, Some(&properties)));
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "state": state.email.to_string(),
        "list": list,
        "notFound": not_found,
    }))
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
        state.email.max(since)
    };
    let key = |list: &[EmailRef]| list.iter().map(ids::email).collect::<Vec<_>>();
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": since.to_string(),
        "newState": new_state.to_string(),
        "hasMoreChanges": window.more,
        "created": key(&window.emails.created),
        "updated": key(&window.emails.updated),
        "destroyed": key(&window.emails.destroyed),
    }))
}

pub(crate) async fn thread_get(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let state = view.state(&viewer).await?;
    let Some(wanted) = args.strings("ids")? else {
        return Err(MethodError::with("requestTooLarge", "Thread/get needs ids"));
    };
    if wanted.len() > ctx.inner.settings.limits.max_objects_in_get {
        return Err(MethodError::new("requestTooLarge"));
    }
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in &wanted {
        let emails = if let Some(thread) = ids::parse_thread(id) {
            match view.thread(&viewer, thread).await {
                Ok(emails) => Some(emails),
                Err(holtmail_mailview::MailViewError::NotFound(_)) => None,
                Err(error) => return Err(error.into()),
            }
        } else if let Some(draft) = id.strip_prefix('T').and_then(ids::parse_email) {
            let found = view.envelopes(&viewer, &[draft]).await?;
            (!found.is_empty()).then(|| vec![draft])
        } else {
            None
        };
        match emails {
            Some(emails) => list.push(json!({
                "id": id,
                "emailIds": emails.iter().map(ids::email).collect::<Vec<_>>(),
            })),
            None => not_found.push(json!(id)),
        }
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "state": state.thread.to_string(),
        "list": list,
        "notFound": not_found,
    }))
}

pub(crate) async fn thread_changes(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
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
        state.thread.max(since)
    };
    let key =
        |list: &[holtmail_core::ThreadId]| list.iter().map(|t| ids::thread(*t)).collect::<Vec<_>>();
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": since.to_string(),
        "newState": new_state.to_string(),
        "hasMoreChanges": window.more,
        "created": key(&window.threads.created),
        "updated": key(&window.threads.updated),
        "destroyed": key(&window.threads.destroyed),
    }))
}

/// What an `Email/query` filter asks of the mail view, plus what is checked on envelopes.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub mailbox: Option<MailboxId>,
    pub other_than: Vec<MailboxId>,
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub filters: Vec<Filter>,
    pub words: Vec<String>,
}

fn condition(map: &Map<String, Value>, plan: &mut Plan, top: bool) -> Result<Filter, MethodError> {
    if let Some(operator) = map.get("operator") {
        let conditions = map
            .get("conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| MethodError::invalid("conditions are missing"))?;
        let operator = operator.as_str().unwrap_or_default();
        let nested_top = top && operator == "AND";
        let mut inner = Vec::new();
        for c in conditions {
            let c = c
                .as_object()
                .ok_or_else(|| MethodError::invalid("a condition is not an object"))?;
            inner.push(condition(c, plan, nested_top)?);
        }
        return match operator {
            "AND" => Ok(Filter::And(inner)),
            "OR" => Ok(Filter::Or(inner)),
            "NOT" => Ok(Filter::Not(Box::new(Filter::Or(inner)))),
            _ => Err(MethodError::with("unsupportedFilter", "unknown operator")),
        };
    }
    let mut all = Vec::new();
    for (key, value) in map {
        let text = || {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| MethodError::invalid(format!("{key} must be a string")))
        };
        let date = || {
            value
                .as_str()
                .and_then(parse_date)
                .ok_or_else(|| MethodError::invalid(format!("{key} must be a UTCDate")))
        };
        let size = || {
            value
                .as_u64()
                .ok_or_else(|| MethodError::invalid(format!("{key} must be a number")))
        };
        let filter = match key.as_str() {
            "inMailbox" => {
                if !top || plan.mailbox.is_some() {
                    return Err(MethodError::with(
                        "unsupportedFilter",
                        "inMailbox works once, outside OR and NOT",
                    ));
                }
                plan.mailbox = Some(
                    value
                        .as_str()
                        .and_then(ids::parse_mailbox)
                        .ok_or_else(|| MethodError::invalid("no such mailbox"))?,
                );
                continue;
            }
            "inMailboxOtherThan" => {
                if !top {
                    return Err(MethodError::with(
                        "unsupportedFilter",
                        "inMailboxOtherThan works outside OR and NOT",
                    ));
                }
                let list = value
                    .as_array()
                    .ok_or_else(|| MethodError::invalid("inMailboxOtherThan must be a list"))?;
                for id in list {
                    plan.other_than.push(
                        id.as_str()
                            .and_then(ids::parse_mailbox)
                            .ok_or_else(|| MethodError::invalid("no such mailbox"))?,
                    );
                }
                continue;
            }
            "before" if top => {
                plan.before = Some(date()?);
                continue;
            }
            "after" if top => {
                plan.after = Some(date()?);
                continue;
            }
            "before" => Filter::ReceivedBefore(Timestamp::from_millis(date()?)),
            "after" => Filter::ReceivedSince(Timestamp::from_millis(date()?)),
            "minSize" => Filter::Larger(size()?.saturating_sub(1)),
            "maxSize" => Filter::Smaller(size()?),
            "hasKeyword" => Filter::Flag(flag(&text()?)),
            "notKeyword" => Filter::Not(Box::new(Filter::Flag(flag(&text()?)))),
            "hasAttachment" => match value.as_bool() {
                Some(true) => Filter::HasAttachment,
                Some(false) => Filter::Not(Box::new(Filter::HasAttachment)),
                None => return Err(MethodError::invalid("hasAttachment must be true or false")),
            },
            "text" | "from" | "to" | "cc" | "bcc" | "subject" | "body" => {
                let words = text()?;
                plan.words
                    .extend(words.split_whitespace().map(str::to_lowercase));
                let field = match key.as_str() {
                    "text" => TextField::Any,
                    "from" => TextField::From,
                    "to" => TextField::To,
                    "cc" => TextField::Cc,
                    "bcc" => TextField::Bcc,
                    "subject" => TextField::Subject,
                    _ => TextField::Body,
                };
                Filter::Text(field, words)
            }
            "header" => {
                let list = value
                    .as_array()
                    .filter(|l| !l.is_empty() && l.len() <= 2)
                    .ok_or_else(|| {
                        MethodError::invalid("header must be [name] or [name, value]")
                    })?;
                let name = list[0].as_str().unwrap_or_default().to_string();
                let words = list.get(1).and_then(Value::as_str).unwrap_or_default();
                Filter::Text(TextField::Header(name), words.to_string())
            }
            _ => {
                return Err(MethodError::with(
                    "unsupportedFilter",
                    format!("{key} is not supported"),
                ));
            }
        };
        all.push(filter);
    }
    Ok(Filter::And(all))
}

pub(crate) fn plan(filter: Option<&Map<String, Value>>) -> Result<Plan, MethodError> {
    let mut plan = Plan::default();
    if let Some(filter) = filter {
        let f = condition(filter, &mut plan, true)?;
        plan.filters.push(f);
    }
    Ok(plan)
}

impl Plan {
    fn filter(&self) -> Filter {
        match self.filters.as_slice() {
            [] => Filter::All,
            [one] => one.clone(),
            many => Filter::And(many.to_vec()),
        }
    }

    fn keeps(&self, envelope: &Envelope) -> bool {
        let at = envelope.received_at.as_millis();
        if self.before.is_some_and(|b| at >= b) || self.after.is_some_and(|a| at < a) {
            return false;
        }
        if !self.other_than.is_empty() {
            let unified = envelope
                .mailboxes
                .iter()
                .any(|m| matches!(m, MailboxId::Role(_)));
            let counts = |m: &MailboxId| match m {
                MailboxId::All => false,
                MailboxId::Folder(_) => !unified,
                _ => true,
            };
            if !envelope
                .mailboxes
                .iter()
                .any(|m| counts(m) && !self.other_than.contains(m))
            {
                return false;
            }
        }
        true
    }
}

/// Recent `Email/query` answers, so paging through one result does not search again.
#[derive(Default)]
pub(crate) struct QueryCache(Mutex<VecDeque<(String, Vec<String>)>>);

impl QueryCache {
    fn get(&self, key: &str) -> Option<Vec<String>> {
        let cache = self.0.lock().expect("query cache");
        cache.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn put(&self, key: String, ids: Vec<String>) {
        let mut cache = self.0.lock().expect("query cache");
        cache.retain(|(k, _)| k != &key);
        cache.push_front((key, ids));
        cache.truncate(QUERY_CACHE);
    }
}

async fn envelopes(
    ctx: &Ctx<'_>,
    viewer: &Viewer,
    emails: &[EmailRef],
) -> Result<Vec<Envelope>, MethodError> {
    let mut out = Vec::with_capacity(emails.len());
    for chunk in emails.chunks(ENVELOPE_CHUNK) {
        out.extend(ctx.inner.services.view.envelopes(viewer, chunk).await?);
    }
    Ok(out)
}

pub(crate) async fn query(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let state = view.state(&viewer).await?;
    let filter = args.object("filter")?;
    let plan = plan(filter)?;
    let mut ascending = false;
    if let Some(sort) = args.get("sort") {
        let sort = sort
            .as_array()
            .ok_or_else(|| MethodError::invalid("sort must be a list"))?;
        match sort.as_slice() {
            [] => {}
            [one] if one.get("property").and_then(Value::as_str) == Some("receivedAt") => {
                ascending = one
                    .get("isAscending")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
            }
            _ => {
                return Err(MethodError::with(
                    "unsupportedSort",
                    "only receivedAt is supported",
                ));
            }
        }
    }
    let collapse = args.bool("collapseThreads", false)?;
    let key = format!(
        "{}|{}|{}|{}|{}|{}",
        viewer.user,
        archive,
        state.email,
        serde_json::to_string(&filter).unwrap_or_default(),
        ascending,
        collapse
    );
    let ids = match ctx.inner.queries.get(&key) {
        Some(ids) => ids,
        None => {
            let mailbox = plan.mailbox.clone().unwrap_or(MailboxId::All);
            let members = view.search(&viewer, &mailbox, &plan.filter()).await?;
            let emails: Vec<EmailRef> = members.iter().map(|m| m.email).collect();
            let mut found: Vec<Envelope> = envelopes(ctx, &viewer, &emails)
                .await?
                .into_iter()
                .filter(|e| plan.keeps(e))
                .collect();
            found.sort_by(|a, b| {
                a.received_at
                    .cmp(&b.received_at)
                    .then_with(|| a.email.cmp(&b.email))
            });
            if !ascending {
                found.reverse();
            }
            if collapse {
                let mut seen = HashSet::new();
                found.retain(|e| seen.insert(thread_id(e)));
            }
            let ids: Vec<String> = found.iter().map(|e| ids::email(&e.email)).collect();
            ctx.inner.queries.put(key, ids.clone());
            ids
        }
    };
    let total = ids.len();
    let mut start = match args.int("position")?.unwrap_or(0) {
        p if p < 0 => total.saturating_sub(p.unsigned_abs() as usize),
        p => (p as usize).min(total),
    };
    if let Some(anchor) = args.string("anchor")? {
        let at = ids
            .iter()
            .position(|i| i == anchor)
            .ok_or_else(|| MethodError::new("anchorNotFound"))?;
        let offset = args.int("anchorOffset")?.unwrap_or(0);
        start = (at as i64 + offset).clamp(0, total as i64) as usize;
    }
    let cap = ctx.inner.settings.limits.max_query_limit;
    let asked = args.unsigned("limit")?.map(|l| l as usize);
    let limit = asked.unwrap_or(cap).min(cap);
    let page: Vec<&String> = ids.iter().skip(start).take(limit).collect();
    let mut out = json!({
        "accountId": ids::account(archive),
        "queryState": state.email.to_string(),
        "canCalculateChanges": false,
        "position": start,
        "ids": page,
    });
    if args.bool("calculateTotal", false)? {
        out["total"] = json!(total);
    }
    if asked.is_some_and(|a| a > cap) {
        out["limit"] = json!(cap);
    }
    Ok(out)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Byte ranges in `text` where a word of `words` starts a word, ignoring case.
fn hits(text: &str, words: &[String]) -> Vec<(usize, usize)> {
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for word in words.iter().filter(|w| !w.is_empty()) {
        let mut from = 0;
        while let Some(found) = lower[from..].find(word.as_str()) {
            let at = from + found;
            let starts = lower[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            if starts {
                out.push((at, at + word.len()));
            }
            from = at + word.len();
        }
    }
    out.sort();
    out.dedup_by(|b, a| b.0 < a.1);
    out
}

/// `text` with the hits marked, escaped as HTML; `None` without a hit.
pub(crate) fn highlight(text: &str, words: &[String], window: usize) -> Option<String> {
    let found = hits(text, words);
    let first = found.first()?.0;
    let mut start = first.saturating_sub(window / 5);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + window).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::new();
    let mut at = start;
    for (s, e) in found.into_iter().filter(|(s, e)| *s >= start && *e <= end) {
        out.push_str(&escape(&text[at..s]));
        out.push_str("<mark>");
        out.push_str(&escape(&text[s..e]));
        out.push_str("</mark>");
        at = e;
    }
    out.push_str(&escape(&text[at..end]));
    Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) async fn snippets(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = &ctx.inner.services.view;
    let plan = plan(args.object("filter")?)?;
    let wanted = args
        .strings("emailIds")?
        .ok_or_else(|| MethodError::invalid("emailIds is required"))?;
    if wanted.len() > ctx.inner.settings.limits.max_objects_in_get {
        return Err(MethodError::new("requestTooLarge"));
    }
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in &wanted {
        let Some(email) = ctx.resolve(id).and_then(ids::parse_email) else {
            not_found.push(json!(id));
            continue;
        };
        let Some(envelope) = view.envelopes(&viewer, &[email]).await?.pop() else {
            not_found.push(json!(id));
            continue;
        };
        let subject = envelope
            .subject
            .as_deref()
            .and_then(|s| highlight(s, &plan.words, 255));
        let preview = if plan.words.is_empty() {
            None
        } else {
            let raw = view.raw(&viewer, &email).await?;
            let root = view.structure(&viewer, &email).await?;
            highlight(&body::plain_text(&raw, &root), &plan.words, 255)
        };
        list.push(json!({"emailId": id, "subject": subject, "preview": preview}));
    }
    Ok(json!({
        "accountId": ids::account(archive),
        "list": list,
        "notFound": if not_found.is_empty() { Value::Null } else { Value::Array(not_found) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_flags() {
        assert_eq!(keyword("\\Seen").as_deref(), Some("$seen"));
        assert_eq!(keyword("\\Deleted"), None);
        assert_eq!(keyword("$Forwarded").as_deref(), Some("$forwarded"));
        assert_eq!(flag("$seen"), "\\Seen");
        assert_eq!(flag("$forwarded"), "$forwarded");
        assert!(valid_keyword("$junk"));
        assert!(!valid_keyword("a b"));
    }

    #[test]
    fn highlights_word_starts_only() {
        let words = vec!["num".to_string()];
        assert_eq!(
            highlight("The numbers <are> in", &words, 255).as_deref(),
            Some("The <mark>num</mark>bers &lt;are&gt; in")
        );
        assert_eq!(highlight("venumber", &words, 255), None);
    }

    #[test]
    fn plans_split_mailbox_and_text() {
        let filter = json!({"operator": "AND", "conditions": [
            {"inMailbox": "role-inbox"}, {"text": "Quarterly Numbers"}, {"after": "2026-11-01T00:00:00Z"}
        ]});
        let plan = plan(filter.as_object()).unwrap();
        assert_eq!(
            plan.mailbox,
            Some(MailboxId::Role(holtmail_mailview::FolderRole::Inbox))
        );
        assert_eq!(plan.words, vec!["quarterly", "numbers"]);
        assert!(plan.after.is_some());
        let nested = json!({"operator": "OR", "conditions": [{"inMailbox": "all"}]});
        assert_eq!(
            super::plan(nested.as_object()).unwrap_err().kind,
            "unsupportedFilter"
        );
    }
}
