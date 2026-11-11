use std::borrow::Cow;
use std::collections::BTreeSet;

use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_core::Timestamp;
use holtmail_mailview::{
    EmailRef, FlagChange, MailboxId, NewEmail, Outcome, PlaceResult, Refusal, Viewer,
};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::{BodyPart, MimePart};
use serde_json::{Map, Value, json};

use crate::blob;
use crate::email::{flag, keyword, valid_keyword};
use crate::error::{MethodError, SetError};
use crate::ids;
use crate::json::{Args, patch_set};
use crate::request::Ctx;
use crate::time::parse_date;

fn virtual_mailbox(id: &MailboxId) -> bool {
    matches!(id, MailboxId::All | MailboxId::Kept)
}

fn refusal(outcome: Outcome, what: &str) -> Option<SetError> {
    match outcome {
        Outcome::Held | Outcome::Applied => None,
        Outcome::Refused(Refusal::NotFound) => Some(SetError::new("notFound")),
        Outcome::Refused(Refusal::Modified) => Some(SetError::with(
            "forbidden",
            format!("the email changed; {what} again"),
        )),
        Outcome::Refused(Refusal::Cannot) => Some(SetError::with(
            "forbidden",
            format!("{what} is not possible here"),
        )),
        Outcome::Refused(Refusal::Locked(lock)) => Some(SetError::with(
            "forbidden",
            format!("{what} is not allowed ({lock:?})").to_lowercase(),
        )),
    }
}

fn first_refusal(results: &[PlaceResult], what: &str) -> Option<SetError> {
    results.iter().find_map(|r| refusal(r.outcome, what))
}

fn object_or_null(map: Map<String, Value>) -> Value {
    if map.is_empty() {
        Value::Null
    } else {
        Value::Object(map)
    }
}

/// Changes one email as an `Email/set` update asks.
async fn update_one(
    ctx: &Ctx<'_>,
    viewer: &Viewer,
    email: EmailRef,
    patch: &Map<String, Value>,
) -> Result<(), SetError> {
    let view = &ctx.inner.services.view;
    let actions = &ctx.inner.services.actions;
    if let Some(other) = patch.keys().find(|k| {
        let base = k.split('/').next().unwrap_or_default();
        base != "keywords" && base != "mailboxIds"
    }) {
        return Err(SetError::properties(
            "only keywords and mailboxIds can change",
            &[other.as_str()],
        ));
    }
    let Some(envelope) = view.envelopes(viewer, &[email]).await?.pop() else {
        return Err(SetError::new("notFound"));
    };
    let current_flags = view
        .flags(viewer, None, &[email])
        .await?
        .remove(&email)
        .unwrap_or_default();
    let current: Vec<String> = current_flags.iter().filter_map(|f| keyword(f)).collect();
    let lower = |k: &str| k.to_ascii_lowercase();
    let next = patch_set(&current, patch, "keywords", lower)
        .map_err(|reason| SetError::properties(reason, &["keywords"]))?;
    let current_ids: Vec<String> = envelope
        .mailboxes
        .iter()
        .filter(|m| !virtual_mailbox(m))
        .map(ids::mailbox)
        .collect();
    let next_ids = patch_set(&current_ids, patch, "mailboxIds", |k| k.to_string())
        .map_err(|reason| SetError::properties(reason, &["mailboxIds"]))?;
    if let Some(next) = next {
        if let Some(bad) = next.iter().find(|k| !valid_keyword(k)) {
            return Err(SetError::properties(
                format!("{bad} is not a valid keyword"),
                &["keywords"],
            ));
        }
        let change = FlagChange {
            add: next
                .iter()
                .filter(|k| !current.contains(k))
                .map(|k| flag(k))
                .collect(),
            remove: current
                .iter()
                .filter(|k| !next.contains(k))
                .map(|k| flag(k))
                .collect(),
        };
        if !change.add.is_empty() || !change.remove.is_empty() {
            let results = actions
                .set_flags(viewer, None, &[email], &change, None)
                .await?;
            if let Some(error) = results
                .iter()
                .find_map(|r| refusal(r.outcome, "changing keywords"))
            {
                return Err(error);
            }
        }
    }
    if let Some(next_ids) = next_ids {
        let mut next: Vec<MailboxId> = Vec::new();
        for id in &next_ids {
            match ids::parse_mailbox(id) {
                Some(m) if virtual_mailbox(&m) => {}
                Some(m) => next.push(m),
                None => {
                    return Err(SetError::properties(
                        format!("no mailbox {id}"),
                        &["mailboxIds"],
                    ));
                }
            }
        }
        let now: Vec<MailboxId> = envelope
            .mailboxes
            .iter()
            .filter(|m| !virtual_mailbox(m))
            .cloned()
            .collect();
        let removed: Vec<MailboxId> = now.iter().filter(|m| !next.contains(m)).cloned().collect();
        let added: Vec<MailboxId> = next.iter().filter(|m| !now.contains(m)).cloned().collect();
        if next.is_empty() && !now.is_empty() {
            return Err(SetError::properties(
                "an email needs at least one mailbox; destroy it instead",
                &["mailboxIds"],
            ));
        }
        let source = now.first().cloned().unwrap_or(MailboxId::All);
        let mut added = added.into_iter();
        let mut removed = removed.into_iter();
        if let (Some(from), Some(to)) = (removed.next(), added.next()) {
            let results = actions.move_to(viewer, &from, &[email], &to).await?;
            if let Some(error) = first_refusal(&results, "moving") {
                return Err(error);
            }
        }
        for to in added {
            let results = actions.copy_to(viewer, &source, &[email], &to).await?;
            if let Some(error) = first_refusal(&results, "adding to a mailbox") {
                return Err(error);
            }
        }
        for from in removed {
            let results = actions.remove(viewer, Some(&from), &[email]).await?;
            let gone = |r: &PlaceResult| r.outcome == Outcome::Refused(Refusal::NotFound);
            if let Some(error) = results
                .iter()
                .filter(|r| !gone(r))
                .find_map(|r| refusal(r.outcome, "removing from a mailbox"))
            {
                return Err(error);
            }
        }
    }
    Ok(())
}

async fn destroy_one(ctx: &Ctx<'_>, viewer: &Viewer, email: EmailRef) -> Result<(), SetError> {
    let actions = &ctx.inner.services.actions;
    let results = match email {
        EmailRef::Draft { .. } => {
            actions
                .remove(viewer, Some(&MailboxId::Drafts), &[email])
                .await?
        }
        EmailRef::Message(_) => actions.remove(viewer, None, &[email]).await?,
    };
    match first_refusal(&results, "destroying") {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn mailbox_list(value: Option<&Value>) -> Result<Vec<MailboxId>, SetError> {
    let map = value
        .and_then(Value::as_object)
        .ok_or_else(|| SetError::properties("mailboxIds is required", &["mailboxIds"]))?;
    let mut out = Vec::new();
    for (id, on) in map {
        if on != &Value::Bool(true) {
            return Err(SetError::properties(
                "mailboxIds values must be true",
                &["mailboxIds"],
            ));
        }
        match ids::parse_mailbox(id) {
            Some(m) if !virtual_mailbox(&m) => out.push(m),
            _ => {
                return Err(SetError::properties(
                    format!("{id} cannot hold new emails"),
                    &["mailboxIds"],
                ));
            }
        }
    }
    if out.is_empty() {
        return Err(SetError::properties(
            "an email needs at least one mailbox",
            &["mailboxIds"],
        ));
    }
    Ok(out)
}

fn flags_of(value: Option<&Value>) -> Result<Vec<String>, SetError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| SetError::properties("keywords must be an object", &["keywords"]))?;
    let mut out = Vec::new();
    for (k, on) in map {
        if on != &Value::Bool(true) || !valid_keyword(k) {
            return Err(SetError::properties(
                "keywords must map valid keywords to true",
                &["keywords"],
            ));
        }
        out.push(flag(k));
    }
    Ok(out)
}

fn received_at(value: Option<&Value>) -> Result<Option<Timestamp>, SetError> {
    match value.filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .and_then(parse_date)
            .map(|ms| Some(Timestamp::from_millis(ms)))
            .ok_or_else(|| SetError::properties("receivedAt must be a UTCDate", &["receivedAt"])),
    }
}

/// Takes bytes into the first mailbox and adds the email to the others; the created object.
async fn take_in(
    ctx: &Ctx<'_>,
    viewer: &Viewer,
    raw: Bytes,
    mailboxes: &[MailboxId],
    flags: Vec<String>,
    received_at: Option<Timestamp>,
) -> Result<(EmailRef, Value), SetError> {
    let actions = &ctx.inner.services.actions;
    let size = raw.len();
    let blob = BlobId::of(&raw);
    let appended = actions
        .append(
            viewer,
            &mailboxes[0],
            NewEmail {
                raw,
                flags,
                received_at,
            },
        )
        .await?;
    if let Some(error) = refusal(appended.outcome, "adding an email") {
        return Err(error);
    }
    let email = appended
        .email
        .ok_or_else(|| SetError::with("serverFail", "the email was not taken in"))?;
    for other in &mailboxes[1..] {
        let results = actions
            .copy_to(viewer, &mailboxes[0], &[email], other)
            .await?;
        if let Some(error) = first_refusal(&results, "adding to a mailbox") {
            return Err(error);
        }
    }
    let envelope = ctx
        .inner
        .services
        .view
        .envelopes(viewer, &[email])
        .await?
        .pop();
    let thread = envelope
        .as_ref()
        .map(crate::email::thread_id)
        .unwrap_or_else(|| format!("T{}", ids::email(&email)));
    Ok((
        email,
        json!({
            "id": ids::email(&email),
            "blobId": ids::blob(&blob),
            "threadId": thread,
            "size": size,
        }),
    ))
}

fn address(value: &Value, property: &str) -> Result<Address<'static>, SetError> {
    let list = value
        .as_array()
        .ok_or_else(|| SetError::properties(format!("{property} must be a list"), &[property]))?;
    let mut out = Vec::new();
    for item in list {
        let email = item
            .get("email")
            .and_then(Value::as_str)
            .filter(|e| e.contains('@'))
            .ok_or_else(|| {
                SetError::properties(format!("{property} needs email addresses"), &[property])
            })?;
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .map(|n| n.to_string());
        out.push(Address::new_address(name, email.to_string()));
    }
    Ok(Address::new_list(out))
}

fn ids_header(value: &Value, property: &str) -> Result<Vec<String>, SetError> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim_matches(['<', '>']).to_string()))
                .collect()
        })
        .ok_or_else(|| SetError::properties(format!("{property} must be a list"), &[property]))
}

struct Builder<'c> {
    ctx: &'c Ctx<'c>,
    viewer: &'c Viewer,
    values: Map<String, Value>,
}

impl Builder<'_> {
    fn text(&self, part: &Map<String, Value>, property: &str) -> Result<String, SetError> {
        let id = part
            .get("partId")
            .and_then(Value::as_str)
            .ok_or_else(|| SetError::properties("a text part needs a partId", &[property]))?;
        self.values
            .get(id)
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| SetError::properties(format!("no body value {id}"), &["bodyValues"]))
    }

    async fn leaf(
        &self,
        part: &Map<String, Value>,
        property: &str,
    ) -> Result<MimePart<'static>, SetError> {
        let kind = part
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text/plain")
            .to_ascii_lowercase();
        let mut content_type = ContentType::new(kind.clone());
        let mime = if part.contains_key("partId") {
            if !kind.starts_with("text/") {
                return Err(SetError::properties(
                    "a part with a partId must be text",
                    &[property],
                ));
            }
            content_type = content_type.attribute("charset", "utf-8");
            MimePart::new(
                content_type,
                BodyPart::Text(Cow::Owned(self.text(part, property)?)),
            )
        } else {
            let id = part.get("blobId").and_then(Value::as_str).ok_or_else(|| {
                SetError::properties("a part needs a partId or a blobId", &[property])
            })?;
            let id = self.ctx.resolve(id).unwrap_or(id);
            let bytes = blob::resolve(self.ctx, self.viewer, id)
                .await
                .map_err(|_| SetError::with("blobNotFound", "a blob could not be read"))?
                .ok_or_else(|| SetError::with("blobNotFound", format!("no blob {id}")))?;
            if let Some(charset) = part.get("charset").and_then(Value::as_str) {
                content_type = content_type.attribute("charset", charset.to_string());
            }
            MimePart::new(content_type, BodyPart::Binary(Cow::Owned(bytes.to_vec())))
        };
        let mut mime = mime;
        let name = part.get("name").and_then(Value::as_str).map(str::to_string);
        match part.get("disposition").and_then(Value::as_str) {
            Some("inline") => mime = mime.inline(),
            Some("attachment") => mime = mime.attachment(name.clone().unwrap_or_default()),
            _ if name.is_some() => mime = mime.attachment(name.clone().unwrap_or_default()),
            _ => {}
        }
        if let Some(cid) = part.get("cid").and_then(Value::as_str) {
            mime = mime.cid(cid.to_string());
        }
        if let Some(location) = part.get("location").and_then(Value::as_str) {
            mime = mime.location(location.to_string());
        }
        Ok(mime)
    }

    fn structure<'a>(
        &'a self,
        part: &'a Map<String, Value>,
        depth: usize,
    ) -> futures::future::BoxFuture<'a, Result<MimePart<'static>, SetError>> {
        Box::pin(async move {
            if depth > 10 {
                return Err(SetError::properties(
                    "bodyStructure is too deep",
                    &["bodyStructure"],
                ));
            }
            let Some(children) = part.get("subParts").and_then(Value::as_array) else {
                return self.leaf(part, "bodyStructure").await;
            };
            let kind = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("multipart/mixed")
                .to_ascii_lowercase();
            if !kind.starts_with("multipart/") {
                return Err(SetError::properties(
                    "only multipart parts have subParts",
                    &["bodyStructure"],
                ));
            }
            let mut parts = Vec::new();
            for child in children {
                let child = child.as_object().ok_or_else(|| {
                    SetError::properties("a part is not an object", &["bodyStructure"])
                })?;
                parts.push(self.structure(child, depth + 1).await?);
            }
            Ok(MimePart::new(
                ContentType::new(kind),
                BodyPart::Multipart(parts),
            ))
        })
    }
}

/// Builds RFC 5322 bytes from an `Email/set` create object (RFC 8621 §4.6).
pub(crate) async fn build(
    ctx: &Ctx<'_>,
    viewer: &Viewer,
    object: &Map<String, Value>,
) -> Result<Vec<u8>, SetError> {
    for forbidden in [
        "id",
        "blobId",
        "threadId",
        "size",
        "hasAttachment",
        "preview",
    ] {
        if object.contains_key(forbidden) {
            return Err(SetError::properties(
                format!("{forbidden} is set by the server"),
                &[forbidden],
            ));
        }
    }
    let mut message = MessageBuilder::new();
    for (key, value) in object {
        if value.is_null() {
            continue;
        }
        message = match key.as_str() {
            "from" => message.from(address(value, key)?),
            "to" => message.to(address(value, key)?),
            "cc" => message.cc(address(value, key)?),
            "bcc" => message.bcc(address(value, key)?),
            "replyTo" => message.reply_to(address(value, key)?),
            "sender" => message.sender(address(value, key)?),
            "subject" => message.subject(
                value
                    .as_str()
                    .ok_or_else(|| SetError::properties("subject must be a string", &["subject"]))?
                    .to_string(),
            ),
            "sentAt" => {
                let ms = value
                    .as_str()
                    .and_then(parse_date)
                    .ok_or_else(|| SetError::properties("sentAt must be a date", &["sentAt"]))?;
                message.date(Date::new(ms / 1000))
            }
            "messageId" => match ids_header(value, key)?.into_iter().next() {
                Some(id) => message.message_id(id),
                None => message,
            },
            "inReplyTo" => message.in_reply_to(ids_header(value, key)?),
            "references" => message.references(ids_header(value, key)?),
            "headers" => {
                let list = value
                    .as_array()
                    .ok_or_else(|| SetError::properties("headers must be a list", &["headers"]))?;
                for header in list {
                    let (Some(name), Some(raw)) = (
                        header.get("name").and_then(Value::as_str),
                        header.get("value").and_then(Value::as_str),
                    ) else {
                        return Err(SetError::properties(
                            "a header needs name and value",
                            &["headers"],
                        ));
                    };
                    message = message.header(name.to_string(), Raw::new(raw.trim().to_string()));
                }
                message
            }
            other if other.starts_with("header:") => {
                let header = crate::body::HeaderProperty::parse(other)
                    .filter(|h| !h.all && h.form == crate::body::Form::Raw)
                    .ok_or_else(|| {
                        SetError::properties("only raw header properties can be set", &[other])
                    })?;
                let raw = value
                    .as_str()
                    .ok_or_else(|| SetError::properties("a header must be a string", &[other]))?;
                message.header(header.name, Raw::new(raw.trim().to_string()))
            }
            _ => message,
        };
    }
    let builder = Builder {
        ctx,
        viewer,
        values: object
            .get("bodyValues")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    };
    if let Some(structure) = object.get("bodyStructure").and_then(Value::as_object) {
        if ["textBody", "htmlBody", "attachments"]
            .iter()
            .any(|k| object.get(*k).is_some_and(|v| !v.is_null()))
        {
            return Err(SetError::properties(
                "bodyStructure cannot come with textBody, htmlBody or attachments",
                &["bodyStructure"],
            ));
        }
        message = message.body(builder.structure(structure, 0).await?);
    } else {
        let single = |key: &str| -> Result<Option<&Map<String, Value>>, SetError> {
            match object.get(key).and_then(Value::as_array).map(Vec::as_slice) {
                None | Some([]) => Ok(None),
                Some([one]) => Ok(one.as_object()),
                Some(_) => Err(SetError::properties(
                    format!("{key} takes one part"),
                    &[key],
                )),
            }
        };
        if let Some(part) = single("textBody")? {
            message = message.text_body(builder.text(part, "textBody")?);
        }
        if let Some(part) = single("htmlBody")? {
            message = message.html_body(builder.text(part, "htmlBody")?);
        }
        if let Some(list) = object.get("attachments").and_then(Value::as_array) {
            for item in list {
                let part = item.as_object().ok_or_else(|| {
                    SetError::properties("an attachment is not an object", &["attachments"])
                })?;
                let mime = builder.leaf(part, "attachments").await?;
                message.attachments.get_or_insert_with(Vec::new).push(mime);
            }
        }
        if message.text_body.is_none()
            && message.html_body.is_none()
            && message.attachments.is_none()
        {
            message = message.text_body(String::new());
        }
    }
    message
        .write_to_vec()
        .map_err(|_| SetError::with("invalidEmail", "the email could not be built"))
}

pub(crate) async fn set(ctx: &mut Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let create = args.object("create")?.cloned();
    let update = args.object("update")?.cloned();
    let destroy = args.strings("destroy")?;
    let if_in_state = args.string("ifInState")?.map(str::to_string);
    apply(ctx, archive, if_in_state, create, update, destroy).await
}

/// Runs an `Email/set` and answers its response.
pub(crate) async fn apply(
    ctx: &mut Ctx<'_>,
    archive: holtmail_core::ArchiveId,
    if_in_state: Option<String>,
    create: Option<Map<String, Value>>,
    update: Option<Map<String, Value>>,
    destroy: Option<Vec<String>>,
) -> Result<Value, MethodError> {
    let viewer = ctx.viewer(archive);
    let view = ctx.inner.services.view.clone();
    let old = view.state(&viewer).await?.email;
    if let Some(expected) = if_in_state
        && expected != old.to_string()
    {
        return Err(MethodError::new("stateMismatch"));
    }
    let count = create.as_ref().map_or(0, |c| c.len())
        + update.as_ref().map_or(0, |u| u.len())
        + destroy.as_ref().map_or(0, |d| d.len());
    if count > ctx.inner.settings.limits.max_objects_in_set {
        return Err(MethodError::new("requestTooLarge"));
    }
    let mut created = Map::new();
    let mut not_created = Map::new();
    for (cid, object) in create.into_iter().flatten() {
        let outcome = async {
            let object = object
                .as_object()
                .ok_or_else(|| SetError::new("invalidProperties"))?;
            let mailboxes = mailbox_list(object.get("mailboxIds"))?;
            let flags = flags_of(object.get("keywords"))?;
            let at = received_at(object.get("receivedAt"))?;
            let raw = build(ctx, &viewer, object).await?;
            take_in(ctx, &viewer, Bytes::from(raw), &mailboxes, flags, at).await
        }
        .await;
        match outcome {
            Ok((email, object)) => {
                ctx.created.insert(cid.clone(), ids::email(&email));
                created.insert(cid, object);
            }
            Err(error) => {
                not_created.insert(cid, error.json());
            }
        }
    }
    let mut updated = Map::new();
    let mut not_updated = Map::new();
    let destroying: BTreeSet<String> = destroy
        .iter()
        .flatten()
        .filter_map(|id| ctx.resolve(id).map(str::to_string))
        .collect();
    for (id, patch) in update.into_iter().flatten() {
        let Some(email) = ctx.resolve(&id).and_then(ids::parse_email) else {
            not_updated.insert(id, SetError::new("notFound").json());
            continue;
        };
        if destroying.contains(&ids::email(&email)) {
            not_updated.insert(id, SetError::new("willDestroy").json());
            continue;
        }
        let Some(patch) = patch.as_object() else {
            not_updated.insert(id, SetError::new("invalidPatch").json());
            continue;
        };
        match update_one(ctx, &viewer, email, patch).await {
            Ok(()) => {
                updated.insert(id, Value::Null);
            }
            Err(error) => {
                not_updated.insert(id, error.json());
            }
        }
    }
    let mut destroyed = Vec::new();
    let mut not_destroyed = Map::new();
    for id in destroy.into_iter().flatten() {
        let Some(email) = ctx.resolve(&id).and_then(ids::parse_email) else {
            not_destroyed.insert(id, SetError::new("notFound").json());
            continue;
        };
        match destroy_one(ctx, &viewer, email).await {
            Ok(()) => destroyed.push(json!(id)),
            Err(error) => {
                not_destroyed.insert(id, error.json());
            }
        }
    }
    let new = view.state(&viewer).await?.email;
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": old.to_string(),
        "newState": new.to_string(),
        "created": object_or_null(created),
        "updated": object_or_null(updated),
        "destroyed": if destroyed.is_empty() { Value::Null } else { Value::Array(destroyed) },
        "notCreated": object_or_null(not_created),
        "notUpdated": object_or_null(not_updated),
        "notDestroyed": object_or_null(not_destroyed),
    }))
}

pub(crate) async fn import(ctx: &mut Ctx<'_>, args: &Args<'_>) -> Result<Value, MethodError> {
    let archive = ctx.account(args)?;
    let viewer = ctx.viewer(archive);
    let view = ctx.inner.services.view.clone();
    let old = view.state(&viewer).await?.email;
    if let Some(expected) = args.string("ifInState")?
        && expected != old.to_string()
    {
        return Err(MethodError::new("stateMismatch"));
    }
    let emails = args
        .object("emails")?
        .ok_or_else(|| MethodError::invalid("emails is required"))?
        .clone();
    if emails.len() > ctx.inner.settings.limits.max_objects_in_set {
        return Err(MethodError::new("requestTooLarge"));
    }
    let mut created = Map::new();
    let mut not_created = Map::new();
    for (cid, object) in emails {
        let outcome = async {
            let object = object
                .as_object()
                .ok_or_else(|| SetError::new("invalidProperties"))?;
            let id = object
                .get("blobId")
                .and_then(Value::as_str)
                .ok_or_else(|| SetError::properties("blobId is required", &["blobId"]))?;
            let id = ctx.resolve(id).unwrap_or(id);
            let raw = blob::resolve(ctx, &viewer, id)
                .await
                .map_err(|_| SetError::with("blobNotFound", "the blob could not be read"))?
                .ok_or_else(|| SetError::new("blobNotFound"))?;
            let mailboxes = mailbox_list(object.get("mailboxIds"))?;
            let flags = flags_of(object.get("keywords"))?;
            let at = received_at(object.get("receivedAt"))?;
            take_in(ctx, &viewer, raw, &mailboxes, flags, at).await
        }
        .await;
        match outcome {
            Ok((email, object)) => {
                ctx.created.insert(cid.clone(), ids::email(&email));
                created.insert(cid, object);
            }
            Err(error) => {
                not_created.insert(cid, error.json());
            }
        }
    }
    let new = view.state(&viewer).await?.email;
    Ok(json!({
        "accountId": ids::account(archive),
        "oldState": old.to_string(),
        "newState": new.to_string(),
        "created": object_or_null(created),
        "notCreated": object_or_null(not_created),
    }))
}
