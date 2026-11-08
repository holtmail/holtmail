use async_trait::async_trait;
use bytes::Bytes;
use holtmail_collection::{
    CollectionError, CollectionKind, CollectionWriter, Field, ItemPatch, Partstat, Precondition,
    RemoteItem, WriteCaps, Written, media,
};
use serde_json::{Map, Value, json};

use super::{GetList, JmapCollections, Kind, Session, canonical, parse, protocol, split};

const STATE_RETRIES: usize = 5;

const SKIPPED: &[&str] = &[
    "id",
    "uid",
    "@type",
    "version",
    "iCalendar",
    "vCard",
    "calendarIds",
    "addressBookIds",
];

/// The JMAP properties that hold `field` in an object of `kind`; `None` for a field this writer
/// cannot set there.
fn properties(kind: CollectionKind, field: &Field) -> Option<&'static [&'static str]> {
    Some(match (kind, field) {
        (CollectionKind::Calendar, Field::Summary) => &["title"],
        (CollectionKind::Calendar, Field::Start) => &["start", "timeZone", "showWithoutTime"],
        (CollectionKind::Calendar, Field::End) => &["duration"],
        (CollectionKind::Calendar, Field::Recurrence) => &[
            "recurrenceRule",
            "recurrenceRules",
            "excludedRecurrenceRules",
            "recurrenceOverrides",
        ],
        (CollectionKind::Calendar, Field::Location) => &["locations", "virtualLocations"],
        (CollectionKind::Calendar, Field::Description) => {
            &["description", "descriptionContentType"]
        }
        (CollectionKind::Calendar, Field::Status) => &["status"],
        (CollectionKind::Calendar, Field::Organizer) => {
            &["organizerCalendarAddress", "replyTo", "participants"]
        }
        (CollectionKind::Calendar, Field::Attendees) => &["participants"],
        (CollectionKind::Calendar, Field::Alarms) => &["alerts", "useDefaultAlerts"],
        (CollectionKind::Calendar, Field::Categories) => &["keywords", "categories"],
        (CollectionKind::Calendar, Field::Transparency) => &["freeBusyStatus"],
        (CollectionKind::Calendar, Field::Privacy) => &["privacy"],
        (CollectionKind::Contacts, Field::Name) => &["name", "fullName"],
        (CollectionKind::Contacts, Field::Emails) => &["emails"],
        (CollectionKind::Contacts, Field::Phones) => &["phones"],
        (CollectionKind::Contacts, Field::Addresses) => &["addresses"],
        (CollectionKind::Contacts, Field::Organization) => &["organizations"],
        (CollectionKind::Contacts, Field::Title) => &["titles"],
        (CollectionKind::Contacts, Field::Notes) => &["notes"],
        (CollectionKind::Contacts, Field::Dates) => &["anniversaries"],
        (CollectionKind::Contacts, Field::Photo) => &["media"],
        (CollectionKind::Contacts, Field::Urls) => &["links"],
        _ => return None,
    })
}

const CALENDAR_FIELDS: &[Field] = &[
    Field::Summary,
    Field::Start,
    Field::End,
    Field::Recurrence,
    Field::Location,
    Field::Description,
    Field::Status,
    Field::Organizer,
    Field::Attendees,
    Field::Alarms,
    Field::Categories,
    Field::Transparency,
    Field::Privacy,
];

const CONTACT_FIELDS: &[Field] = &[
    Field::Name,
    Field::Emails,
    Field::Phones,
    Field::Addresses,
    Field::Organization,
    Field::Title,
    Field::Notes,
    Field::Dates,
    Field::Photo,
    Field::Urls,
];

fn every_property(kind: CollectionKind) -> Vec<&'static str> {
    let fields = match kind {
        CollectionKind::Calendar => CALENDAR_FIELDS,
        CollectionKind::Contacts => CONTACT_FIELDS,
    };
    let mut out: Vec<&'static str> = Vec::new();
    for field in fields {
        for name in properties(kind, field).unwrap_or_default() {
            if !out.contains(name) {
                out.push(name);
            }
        }
    }
    out
}

fn refused(what: &str) -> CollectionError {
    CollectionError::Refused(what.to_owned())
}

/// The patch's item as the JSContact or JSCalendar object a JMAP server stores, converted by
/// `calcard` from vCard or iCalendar (JSON is taken as it is), without `calcard`'s conversion
/// notes.
pub(crate) fn object(
    kind: CollectionKind,
    patch: &ItemPatch,
) -> Result<Map<String, Value>, CollectionError> {
    let essence = patch
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let text = std::str::from_utf8(&patch.raw).map_err(|_| refused("the item is not UTF-8"))?;
    let value = match (kind, essence.as_str()) {
        (CollectionKind::Calendar, media::ICALENDAR) => {
            let calendar = calcard::icalendar::ICalendar::parse(text)
                .map_err(|_| refused("the item is not valid iCalendar"))?;
            serde_json::to_value(calendar.into_jscalendar::<String, String>())
                .map_err(|_| refused("the item cannot be converted"))?
        }
        (CollectionKind::Contacts, media::VCARD) => {
            let card = calcard::vcard::VCard::parse(text)
                .map_err(|_| refused("the item is not a valid vCard"))?;
            serde_json::to_value(card.into_jscontact::<String, String>())
                .map_err(|_| refused("the item cannot be converted"))?
        }
        (CollectionKind::Calendar, media::JSCALENDAR)
        | (CollectionKind::Contacts, media::JSCONTACT) => {
            serde_json::from_str(text).map_err(|_| refused("the item is not valid JSON"))?
        }
        _ => return Err(refused("the item's type does not fit the collection")),
    };
    let mut value = match value {
        Value::Object(map) if map.get("@type").and_then(Value::as_str) == Some("Group") => {
            let mut entries = match map.get("entries") {
                Some(Value::Array(entries)) => entries.clone(),
                _ => Vec::new(),
            };
            if entries.len() != 1 {
                return Err(refused("the item must hold exactly one event"));
            }
            entries.remove(0)
        }
        other => other,
    };
    let Value::Object(map) = &mut value else {
        return Err(refused("the item is not an object"));
    };
    if kind == CollectionKind::Calendar
        && map
            .get("@type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "Event")
    {
        return Err(refused("only events can be written to a JMAP calendar"));
    }
    map.remove("iCalendar");
    map.remove("vCard");
    map.remove("id");
    Ok(std::mem::take(map))
}

/// The update patch for `fields` (every field when `None`): each property that holds one of
/// them gets its value from `next`, or `null` when `next` lacks it and `current` has it.
fn update_patch(
    kind: CollectionKind,
    fields: Option<&[Field]>,
    next: &Map<String, Value>,
    current: &Map<String, Value>,
) -> Result<Map<String, Value>, CollectionError> {
    let mut names: Vec<&str> = Vec::new();
    match fields {
        Some(fields) => {
            for field in fields {
                let Some(props) = properties(kind, field) else {
                    return Err(refused("the change touches a field JMAP cannot set"));
                };
                for name in props {
                    if !names.contains(name) {
                        names.push(name);
                    }
                }
            }
        }
        None => {
            names = every_property(kind);
            for name in next.keys() {
                if !SKIPPED.contains(&name.as_str()) && !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
        }
    }
    let mut patch = Map::new();
    for name in names {
        match next.get(name) {
            Some(value) => {
                patch.insert(name.to_owned(), value.clone());
            }
            None if current.contains_key(name) => {
                patch.insert(name.to_owned(), Value::Null);
            }
            None => {}
        }
    }
    Ok(patch)
}

/// Whether `stored` holds what `patch` wrote: every property of the patch's fields (all of them
/// when `None`) equals the converted item's.
#[cfg(test)]
pub(crate) fn holds(kind: CollectionKind, stored: &[u8], patch: &ItemPatch) -> bool {
    let (Ok(next), Ok(Value::Object(stored))) =
        (object(kind, patch), serde_json::from_slice::<Value>(stored))
    else {
        return false;
    };
    let Ok(wanted) = update_patch(kind, patch.fields.as_deref(), &next, &stored) else {
        return false;
    };
    wanted
        .iter()
        .all(|(name, value)| stored.get(name).unwrap_or(&Value::Null) == value)
}

fn set_error(error: &Value) -> CollectionError {
    let kind = error
        .get("type")
        .and_then(Value::as_str)
        .filter(|k| k.len() <= 64 && k.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("unknown");
    match kind {
        "notFound" => CollectionError::PreconditionFailed { current: None },
        "rateLimit" | "serverUnavailable" => {
            CollectionError::Connection(format!("the JMAP server is busy: {kind}"))
        }
        _ => CollectionError::Refused(format!("the JMAP server refused the change: {kind}")),
    }
}

fn set_method_error(kind: &str) -> CollectionError {
    match kind {
        "accountReadOnly" | "forbidden" | "requestTooLarge" | "tooLarge" | "invalidArguments" => {
            CollectionError::Refused(format!("the JMAP server refused the change: {kind}"))
        }
        other => super::method_error(other),
    }
}

/// The occurrence key of a JSCalendar `recurrenceOverrides` map for an iCalendar
/// `RECURRENCE-ID`: a local date and time in the event's `timeZone`.
pub(crate) fn override_key(
    recurrence_id: &str,
    time_zone: Option<&str>,
) -> Result<String, CollectionError> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone};
    let invalid = || refused("the occurrence is not an iCalendar date or date-time");
    let text = recurrence_id.trim();
    let (text, utc) = match text.strip_suffix(['Z', 'z']) {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    let local = if text.len() == 8 {
        NaiveDate::parse_from_str(text, "%Y%m%d")
            .map_err(|_| invalid())?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(invalid)?
    } else {
        NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%S").map_err(|_| invalid())?
    };
    let local = match (utc, time_zone) {
        (true, Some(zone)) if !matches!(zone, "Etc/UTC" | "UTC" | "Etc/GMT" | "GMT") => {
            let zone: chrono_tz::Tz = zone
                .parse()
                .map_err(|_| refused("the event's time zone is unknown"))?;
            zone.from_utc_datetime(&local).naive_local()
        }
        _ => local,
    };
    Ok(local.format("%Y-%m-%dT%H:%M:%S").to_string())
}

fn address(text: &str) -> String {
    let text = text.trim();
    let text = text
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("mailto:"))
        .map_or(text, |_| &text[7..]);
    text.to_ascii_lowercase()
}

/// The addresses a participant can be reached at: its `calendarAddress`, `sendTo` and `email`.
fn participant_addresses(participant: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["calendarAddress", "email"] {
        if let Some(text) = participant.get(key).and_then(Value::as_str) {
            out.push(address(text));
        }
    }
    if let Some(send_to) = participant.get("sendTo").and_then(Value::as_object) {
        out.extend(send_to.values().filter_map(Value::as_str).map(address));
    }
    out
}

/// The id of the participant in `event` that is reached at one of `own`.
pub(crate) fn own_participant(event: &Value, own: &[String]) -> Option<String> {
    event
        .get("participants")?
        .as_object()?
        .iter()
        .find(|(_, p)| participant_addresses(p).iter().any(|a| own.contains(a)))
        .map(|(id, _)| id.clone())
}

fn status_word(answer: Partstat) -> &'static str {
    match answer {
        Partstat::NeedsAction => "needs-action",
        Partstat::Accepted => "accepted",
        Partstat::Tentative => "tentative",
        Partstat::Declined => "declined",
    }
}

/// The patch that sets participant `pid`'s answer, for the series or the occurrence `key`.
fn respond_patch(
    event: &Value,
    pid: &str,
    key: Option<&str>,
    status: &str,
) -> Result<Map<String, Value>, CollectionError> {
    let mut patch = Map::new();
    let Some(key) = key else {
        patch.insert(
            format!("participants/{}/participationStatus", escape(pid)),
            json!(status),
        );
        return Ok(patch);
    };
    let pointer = format!("participants/{}/participationStatus", escape(pid));
    match event.get("recurrenceOverrides").and_then(Value::as_object) {
        Some(overrides) => {
            let mut item = match overrides.get(key) {
                Some(Value::Object(item)) => item.clone(),
                Some(_) => return Err(refused("the occurrence cannot be answered")),
                None => Map::new(),
            };
            if item.get("excluded").and_then(Value::as_bool) == Some(true) {
                return Err(refused("the occurrence was cancelled"));
            }
            match item
                .get_mut("participants")
                .and_then(Value::as_object_mut)
                .and_then(|p| p.get_mut(pid))
                .and_then(Value::as_object_mut)
            {
                Some(own) => {
                    own.insert("participationStatus".into(), json!(status));
                }
                None => {
                    item.insert(pointer, json!(status));
                }
            }
            patch.insert(
                format!("recurrenceOverrides/{}", escape(key)),
                Value::Object(item),
            );
        }
        None => {
            patch.insert(
                "recurrenceOverrides".into(),
                json!({ key: { pointer: status } }),
            );
        }
    }
    Ok(patch)
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

struct Current {
    state: String,
    item: Option<Value>,
}

impl JmapCollections {
    /// The item state and item `id` when it is in collection `coll`; `Gone` for a missing
    /// collection.
    async fn current(
        &self,
        session: &Session,
        kind: &Kind,
        coll: &str,
        id: &str,
    ) -> Result<Current, CollectionError> {
        let account = Self::account(session, kind)?;
        let answers = self
            .call(
                session,
                kind,
                vec![
                    (
                        format!("{}/get", kind.collection),
                        json!({ "accountId": account, "ids": [coll], "properties": ["id"] }),
                    ),
                    (
                        format!("{}/get", kind.item),
                        json!({ "accountId": account, "ids": [id] }),
                    ),
                ],
            )
            .await?;
        let collections: GetList<'_> = parse(&answers[0])?;
        if collections.list.is_empty() {
            return Err(CollectionError::Gone);
        }
        let items: GetList<'_> = parse(&answers[1])?;
        let mut found = None;
        for raw in items.list {
            let value: Value = serde_json::from_str(raw.get())
                .map_err(|_| protocol("the JMAP server sent an invalid object"))?;
            let here = value.get("id").and_then(Value::as_str) == Some(id)
                && value
                    .get(kind.member)
                    .and_then(|m| m.get(coll))
                    .is_some_and(|v| v.as_bool() != Some(false));
            if here {
                found = Some(value);
            }
        }
        Ok(Current {
            state: items.state,
            item: found,
        })
    }

    fn written(kind: &Kind, id: &str, value: Value) -> Result<Written, CollectionError> {
        let raw = serde_json::to_vec(&canonical(value))
            .map_err(|_| protocol("the JMAP server sent an invalid object"))?;
        Ok(Written {
            id: id.to_owned(),
            etag: None,
            item: Some(RemoteItem {
                id: id.to_owned(),
                etag: None,
                content_type: kind.media.to_owned(),
                raw: Bytes::from(raw),
            }),
        })
    }

    async fn reread(
        &self,
        session: &Session,
        kind: &Kind,
        coll: &str,
        id: &str,
    ) -> Result<Written, CollectionError> {
        match self.current(session, kind, coll, id).await?.item {
            Some(value) => Self::written(kind, id, value),
            None => Ok(Written {
                id: id.to_owned(),
                etag: None,
                item: None,
            }),
        }
    }

    /// Sends one `/set` call; `Ok(None)` when `if_in_state` no longer holds.
    async fn set(
        &self,
        session: &Session,
        kind: &Kind,
        mut args: Value,
        if_in_state: Option<&str>,
    ) -> Result<Option<Value>, CollectionError> {
        args["accountId"] = json!(Self::account(session, kind)?);
        if let Some(state) = if_in_state {
            args["ifInState"] = json!(state);
        }
        let mut answers = self
            .call_each(session, kind, vec![(format!("{}/set", kind.item), args)])
            .await?;
        match answers.remove(0) {
            Ok(raw) => Ok(Some(parse(&raw)?)),
            Err(kind) if kind == "stateMismatch" => Ok(None),
            Err(kind) => Err(set_method_error(&kind)),
        }
    }

    async fn find_uid(
        &self,
        session: &Session,
        kind: &Kind,
        coll: &str,
        uid: &str,
    ) -> Result<Option<String>, CollectionError> {
        let account = Self::account(session, kind)?;
        let answers = self
            .call(
                session,
                kind,
                vec![
                    (
                        format!("{}/get", kind.collection),
                        json!({ "accountId": account, "ids": [coll], "properties": ["id"] }),
                    ),
                    (
                        format!("{}/query", kind.item),
                        json!({
                            "accountId": account,
                            "filter": { kind.filter: coll, "uid": uid },
                            "limit": 2,
                        }),
                    ),
                ],
            )
            .await?;
        let collections: GetList<'_> = parse(&answers[0])?;
        if collections.list.is_empty() {
            return Err(CollectionError::Gone);
        }
        let page: Value = parse(&answers[1])?;
        Ok(page
            .get("ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    async fn create(
        &self,
        session: &Session,
        kind: &Kind,
        coll: &str,
        change: &ItemPatch,
        idempotency: &str,
    ) -> Result<Written, CollectionError> {
        let mut item = object(kind.kind, change)?;
        let uid = change
            .uid
            .clone()
            .or_else(|| item.get("uid").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| idempotency.to_owned());
        if let Some(id) = self.find_uid(session, kind, coll, &uid).await? {
            let found = self.current(session, kind, coll, &id).await?.item;
            let same = found.as_ref().is_some_and(|found| {
                item.iter()
                    .filter(|(k, _)| k.as_str() != "uid")
                    .all(|(k, v)| found.get(k) == Some(v))
            });
            if !same {
                return Err(CollectionError::PreconditionFailed { current: None });
            }
            return match found {
                Some(value) => Self::written(kind, &id, value),
                None => self.reread(session, kind, coll, &id).await,
            };
        }
        item.insert("uid".into(), json!(uid));
        item.insert(kind.member.into(), json!({ coll: true }));
        let answer = self
            .set(session, kind, json!({ "create": { "c0": item } }), None)
            .await?
            .ok_or_else(|| protocol("the JMAP server answered a state it was not asked for"))?;
        if let Some(error) = answer.get("notCreated").and_then(|n| n.get("c0")) {
            return Err(match set_error(error) {
                CollectionError::PreconditionFailed { .. } => {
                    refused("the JMAP server refused the change: notFound")
                }
                other => other,
            });
        }
        let id = answer
            .get("created")
            .and_then(|c| c.get("c0"))
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| protocol("the JMAP server did not say what it created"))?
            .to_owned();
        self.reread(session, kind, coll, &id).await
    }

    async fn update(
        &self,
        session: &Session,
        kind: &Kind,
        coll: &str,
        id: &str,
        change: &ItemPatch,
        precondition: &Precondition,
    ) -> Result<Written, CollectionError> {
        let next = object(kind.kind, change)?;
        let expected = match precondition {
            Precondition::IfUnchanged(raw) => Some(
                serde_json::from_slice::<Value>(raw)
                    .map_err(|_| CollectionError::PreconditionFailed { current: None })?,
            ),
            Precondition::IfMatch(_) => {
                return Err(refused("JMAP keeps no version tags to match"));
            }
            Precondition::None | Precondition::Absent => None,
        };
        for _ in 0..STATE_RETRIES {
            let current = self.current(session, kind, coll, id).await?;
            let Some(item) = current.item else {
                return Err(CollectionError::PreconditionFailed { current: None });
            };
            if matches!(precondition, Precondition::Absent) {
                return Err(CollectionError::PreconditionFailed { current: None });
            }
            if expected.as_ref().is_some_and(|e| *e != item) {
                return Err(CollectionError::PreconditionFailed { current: None });
            }
            let empty = Map::new();
            let patch = update_patch(
                kind.kind,
                change.fields.as_deref(),
                &next,
                item.as_object().unwrap_or(&empty),
            )?;
            if patch.is_empty() {
                return Self::written(kind, id, item);
            }
            let guard = expected.is_some().then_some(current.state.as_str());
            let Some(answer) = self
                .set(session, kind, json!({ "update": { id: patch } }), guard)
                .await?
            else {
                continue;
            };
            if let Some(error) = answer.get("notUpdated").and_then(|n| n.get(id)) {
                return Err(set_error(error));
            }
            return self.reread(session, kind, coll, id).await;
        }
        Err(CollectionError::Connection(
            "the JMAP account kept changing while writing".into(),
        ))
    }

    async fn own_addresses(
        &self,
        session: &Session,
        kind: &Kind,
    ) -> Result<Vec<String>, CollectionError> {
        let account = Self::account(session, kind)?;
        let mut own: Vec<String> = session.username.iter().map(|u| address(u)).collect();
        let answers = self
            .call_each(
                session,
                kind,
                vec![(
                    "ParticipantIdentity/get".into(),
                    json!({ "accountId": account, "ids": null }),
                )],
            )
            .await?;
        if let Some(Ok(raw)) = answers.first() {
            let list: Value = parse(raw)?;
            for identity in list
                .get("list")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                own.extend(participant_addresses(identity));
            }
        }
        own.retain(|a| !a.is_empty());
        Ok(own)
    }
}

#[async_trait]
impl CollectionWriter for JmapCollections {
    /// Creates, updates and deletes in every address book and calendar of the session's
    /// accounts, patching only the changed fields; answers invitations in calendars. No version
    /// tags: callers use [`Precondition::IfUnchanged`]. Nothing for a collection id that names
    /// neither, or a kind the session is known not to offer.
    fn capabilities(&self, collection: &str) -> WriteCaps {
        let Ok((kind, _)) = split(collection) else {
            return WriteCaps::default();
        };
        if let Ok(cached) = self.session.try_lock()
            && let Some(session) = cached.as_ref()
            && session.account(kind).is_none()
        {
            return WriteCaps::default();
        }
        WriteCaps {
            create: true,
            update: true,
            delete: true,
            respond: kind.kind == CollectionKind::Calendar,
            field_patch: true,
            tags: false,
            max_size: None,
            keeps_uid: true,
        }
    }

    async fn put(
        &self,
        collection: &str,
        id: Option<&str>,
        change: &ItemPatch,
        precondition: Precondition,
        idempotency: &str,
    ) -> Result<Written, CollectionError> {
        let (kind, coll) = split(collection)?;
        let session = self.session().await?;
        match id {
            None => self.create(&session, kind, coll, change, idempotency).await,
            Some(id) => {
                self.update(&session, kind, coll, id, change, &precondition)
                    .await
            }
        }
    }

    async fn delete(
        &self,
        collection: &str,
        id: &str,
        if_match: Option<&str>,
    ) -> Result<(), CollectionError> {
        let (kind, coll) = split(collection)?;
        let session = self.session().await?;
        if self.current(&session, kind, coll, id).await?.item.is_none() {
            return Ok(());
        }
        if if_match.is_some() {
            return Err(CollectionError::PreconditionFailed { current: None });
        }
        let answer = self
            .set(&session, kind, json!({ "destroy": [id] }), None)
            .await?
            .ok_or_else(|| protocol("the JMAP server answered a state it was not asked for"))?;
        match answer.get("notDestroyed").and_then(|n| n.get(id)) {
            Some(error) => match set_error(error) {
                CollectionError::PreconditionFailed { .. } => Ok(()),
                other => Err(other),
            },
            None => Ok(()),
        }
    }

    async fn respond(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
        answer: Partstat,
        notify_organizer: bool,
    ) -> Result<Written, CollectionError> {
        let (kind, coll) = split(collection)?;
        if kind.kind != CollectionKind::Calendar {
            return Err(refused("only events can be answered"));
        }
        let session = self.session().await?;
        let Some(event) = self.current(&session, kind, coll, id).await?.item else {
            return Err(CollectionError::PreconditionFailed { current: None });
        };
        let own = self.own_addresses(&session, kind).await?;
        let pid = own_participant(&event, &own)
            .ok_or_else(|| refused("the account is not an attendee of the event"))?;
        let key = recurrence_id
            .map(|r| override_key(r, event.get("timeZone").and_then(Value::as_str)))
            .transpose()?;
        let patch = respond_patch(&event, &pid, key.as_deref(), status_word(answer))?;
        let answer = self
            .set(
                &session,
                kind,
                json!({
                    "update": { id: patch },
                    "sendSchedulingMessages": notify_organizer,
                }),
                None,
            )
            .await?
            .ok_or_else(|| protocol("the JMAP server answered a state it was not asked for"))?;
        if let Some(error) = answer.get("notUpdated").and_then(|n| n.get(id)) {
            return Err(set_error(error));
        }
        self.reread(&session, kind, coll, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrence_keys_follow_the_event_zone() {
        assert_eq!(
            override_key("20261208T100000Z", Some("Etc/UTC")).unwrap(),
            "2026-12-08T10:00:00"
        );
        assert_eq!(
            override_key("20261208T100000Z", Some("Europe/Berlin")).unwrap(),
            "2026-12-08T11:00:00"
        );
        assert_eq!(
            override_key("20261208T100000", Some("Europe/Berlin")).unwrap(),
            "2026-12-08T10:00:00"
        );
        assert_eq!(
            override_key("20261208", None).unwrap(),
            "2026-12-08T00:00:00"
        );
        assert!(matches!(
            override_key("next week", None),
            Err(CollectionError::Refused(_))
        ));
    }

    #[test]
    fn a_field_patch_sets_only_its_properties() {
        let next: Map<String, Value> =
            serde_json::from_value(json!({ "title": "New", "start": "2026-12-01T10:00:00" }))
                .unwrap();
        let current: Map<String, Value> = serde_json::from_value(
            json!({ "title": "Old", "description": "kept", "showWithoutTime": false }),
        )
        .unwrap();
        let patch = update_patch(
            CollectionKind::Calendar,
            Some(&[Field::Summary, Field::Start]),
            &next,
            &current,
        )
        .unwrap();
        assert_eq!(
            Value::Object(patch),
            json!({ "title": "New", "start": "2026-12-01T10:00:00", "showWithoutTime": null })
        );
        assert!(matches!(
            update_patch(
                CollectionKind::Calendar,
                Some(&[Field::Property("X-FOO".into())]),
                &next,
                &current
            ),
            Err(CollectionError::Refused(_))
        ));
        assert!(matches!(
            update_patch(
                CollectionKind::Calendar,
                Some(&[Field::Emails]),
                &next,
                &current
            ),
            Err(CollectionError::Refused(_))
        ));
    }

    #[test]
    fn a_whole_replace_clears_what_the_item_lost() {
        let next: Map<String, Value> =
            serde_json::from_value(json!({ "title": "New", "uid": "u1" })).unwrap();
        let current: Map<String, Value> =
            serde_json::from_value(json!({ "title": "Old", "description": "gone", "x-server": 1 }))
                .unwrap();
        let patch = update_patch(CollectionKind::Calendar, None, &next, &current).unwrap();
        assert_eq!(
            Value::Object(patch),
            json!({ "title": "New", "description": null })
        );
    }

    #[test]
    fn vcards_and_icalendar_become_jmap_objects() {
        let card = ItemPatch {
            content_type: media::VCARD.into(),
            raw: Bytes::from_static(
                b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1@example.com\r\nFN:Ada King\r\n\
                  EMAIL:ada.king@example.com\r\nEND:VCARD\r\n",
            ),
            uid: None,
            fields: None,
        };
        let object = object(CollectionKind::Contacts, &card).unwrap();
        assert_eq!(object["name"]["full"], "Ada King");
        assert!(!object.contains_key("vCard"));
        let task = ItemPatch {
            content_type: media::ICALENDAR.into(),
            raw: Bytes::from_static(
                b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:t1\r\nSUMMARY:x\r\n\
                  END:VTODO\r\nEND:VCALENDAR\r\n",
            ),
            uid: None,
            fields: None,
        };
        assert!(matches!(
            super::object(CollectionKind::Calendar, &task),
            Err(CollectionError::Refused(_))
        ));
        assert!(matches!(
            super::object(CollectionKind::Calendar, &card),
            Err(CollectionError::Refused(_))
        ));
    }

    #[test]
    fn answering_an_occurrence_patches_its_override() {
        let event =
            json!({ "recurrenceOverrides": { "2026-12-08T10:00:00": { "title": "Moved" } } });
        let patch = respond_patch(&event, "p1", Some("2026-12-08T10:00:00"), "declined").unwrap();
        assert_eq!(
            Value::Object(patch),
            json!({ "recurrenceOverrides/2026-12-08T10:00:00": {
                "title": "Moved", "participants/p1/participationStatus": "declined" } })
        );
        let patch = respond_patch(&json!({}), "p/1", None, "accepted").unwrap();
        assert_eq!(
            Value::Object(patch),
            json!({ "participants/p~11/participationStatus": "accepted" })
        );
        let cancelled = json!({ "recurrenceOverrides": { "k": { "excluded": true } } });
        assert!(respond_patch(&cancelled, "p1", Some("k"), "accepted").is_err());
    }
}
