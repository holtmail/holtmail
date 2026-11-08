use std::collections::BTreeMap;

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{NaiveDateTime, TimeDelta};
use holtmail_collection::testing::{WriteProbe, writer_conformance};
use holtmail_collection::{CollectionWriter, Field, ItemPatch, Partstat, Precondition};
use holtmail_pim::{Format, ItemKind, Normalized};
use serde_json::{Value, json};

use super::{Fake, FakeGraph, item_id, name_of};
use crate::write::CREATE_KEY_PROPERTY;
use crate::*;
use holtmail_collection::WriteCaps;

const CAL: &str = "calendars/AQMkCal";

/// The create key a contacts `$filter` asks for.
pub(super) fn filtered_key(filter: &str) -> Option<String> {
    let start = filter.find("ep/value eq '")? + "ep/value eq '".len();
    let end = filter.rfind("')")?;
    Some(filter.get(start..end)?.replace("''", "'"))
}

fn utc(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim_end_matches('Z');
    NaiveDateTime::parse_from_str(text.get(..19).unwrap_or(text), "%Y-%m-%dT%H:%M:%S").ok()
}

fn stamp(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The occurrences of a daily or weekly series stored in UTC, in the asked window.
pub(super) fn instances(raw: &[u8], q: &BTreeMap<String, String>) -> Vec<Value> {
    let Ok(master) = serde_json::from_slice::<Value>(raw) else {
        return Vec::new();
    };
    let Some(first) = master
        .pointer("/start/dateTime")
        .and_then(Value::as_str)
        .and_then(utc)
    else {
        return Vec::new();
    };
    let interval = master
        .pointer("/recurrence/pattern/interval")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let step = match master
        .pointer("/recurrence/pattern/type")
        .and_then(Value::as_str)
    {
        Some("daily") => TimeDelta::days(interval),
        Some("weekly") => TimeDelta::weeks(interval),
        _ => return Vec::new(),
    };
    let count = master
        .pointer("/recurrence/range/numberOfOccurrences")
        .and_then(Value::as_i64)
        .unwrap_or(10);
    let from = q.get("startDateTime").and_then(|s| utc(s));
    let to = q.get("endDateTime").and_then(|s| utc(s));
    let id = master["id"].as_str().unwrap_or_default().to_owned();
    (0..count)
        .map(|i| first + step * i as i32)
        .filter(|at| from.is_none_or(|f| *at >= f) && to.is_none_or(|t| *at < t))
        .map(|at| json!({"id": format!("{id}~{}", stamp(at)), "originalStart": stamp(at)}))
        .collect()
}

impl Fake {
    fn store(&mut self, collection: &str, name: &str, mut value: Value) -> Value {
        self.seq += 1;
        let seq = self.seq;
        value["id"] = json!(item_id(name));
        value["changeKey"] = json!(seq.to_string());
        value["@odata.etag"] = json!(format!("W/\"{seq}\""));
        if collection.starts_with("calendars/") {
            let series = value.get("recurrence").is_some_and(|r| !r.is_null());
            value["type"] = json!(if series {
                "seriesMaster"
            } else {
                "singleInstance"
            });
        }
        let coll = self.collections.get_mut(collection).expect("a collection");
        coll.items.insert(
            name.into(),
            (seq, serde_json::to_vec(&value).expect("json")),
        );
        coll.log.push((seq, name.into()));
        value
    }

    fn value(&self, collection: &str, name: &str) -> Option<(u64, Value)> {
        let (seq, raw) = self.collections.get(collection)?.items.get(name)?;
        Some((*seq, serde_json::from_slice(raw).ok()?))
    }

    fn touch_item(&mut self, collection: &str, name: &str) {
        if let Some((_, mut value)) = self.value(collection, name) {
            value["categories"] = json!(["Touched elsewhere"]);
            self.store(collection, name, value);
        }
    }
}

fn answer(status: StatusCode, value: &Value) -> Response {
    (status, axum::Json(value.clone())).into_response()
}

fn verb_response(verb: &str) -> Option<&'static str> {
    match verb {
        "accept" => Some("accepted"),
        "tentativelyAccept" => Some("tentativelyAccepted"),
        "decline" => Some("declined"),
        _ => None,
    }
}

/// Graph's writes: create, patch, delete and answers, each checking `If-Match`.
pub(super) fn handle(
    f: &mut Fake,
    method: &Method,
    segments: &[&str],
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    if f.deny_writes {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (collection, rest): (String, &[&str]) = match segments {
        ["contacts", rest @ ..] => ("contacts".into(), rest),
        ["contactFolders", id, "contacts", rest @ ..] => (format!("contacts/{id}"), rest),
        ["calendars", id, "events", rest @ ..] => (format!("calendars/{id}"), rest),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if !f.collections.contains_key(&collection) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let sent: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let if_match = headers
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match (method.as_str(), rest) {
        ("POST", []) => {
            let mut value = sent;
            let key = if collection.starts_with("calendars/") {
                value
                    .as_object_mut()
                    .and_then(|o| o.remove("transactionId"))
            } else {
                value
                    .as_object_mut()
                    .and_then(|o| o.remove("singleValueExtendedProperties"))
                    .and_then(|p| p.pointer("/0/value").cloned())
            }
            .and_then(|k| k.as_str().map(str::to_owned));
            if let Some(key) = &key
                && collection.starts_with("calendars/")
                && let Some(name) = f
                    .transactions
                    .get(&(collection.clone(), key.clone()))
                    .cloned()
                && let Some((_, existing)) = f.value(&collection, &name)
            {
                return answer(StatusCode::CREATED, &existing);
            }
            if value.get("subject").and_then(Value::as_str) == Some("Refuse me") {
                return answer(
                    StatusCode::BAD_REQUEST,
                    &json!({"error": {"code": "ErrorInvalidRequest", "message": "Planning at noon"}}),
                );
            }
            f.created += 1;
            let name = format!("w{}", f.created);
            if let Some(key) = key {
                if collection.starts_with("calendars/") {
                    f.transactions
                        .insert((collection.clone(), key), name.clone());
                } else if let Some(coll) = f.collections.get_mut(&collection) {
                    coll.keys.insert(name.clone(), key);
                }
            }
            let stored = f.store(&collection, &name, value);
            answer(StatusCode::CREATED, &stored)
        }
        ("PATCH" | "DELETE", [id]) => {
            let name = name_of(id).to_owned();
            if f.race {
                f.race = false;
                f.touch_item(&collection, &name);
            }
            let Some((seq, mut value)) = f.value(&collection, &name) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Some(tag) = &if_match
                && *tag != format!("W/\"{seq}\"")
            {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            if method == Method::DELETE {
                f.delete(&collection, &name);
                return StatusCode::NO_CONTENT.into_response();
            }
            f.patches.push((sent.clone(), if_match));
            for (key, v) in sent.as_object().into_iter().flatten() {
                if v.is_null() {
                    value.as_object_mut().map(|o| o.remove(key));
                } else {
                    value[key] = v.clone();
                }
            }
            let stored = f.store(&collection, &name, value);
            answer(StatusCode::OK, &stored)
        }
        ("POST", [id, verb]) => {
            let Some(response) = verb_response(verb) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let (master, occurrence) = match id.split_once('~') {
                Some((m, o)) => (name_of(m).to_owned(), Some(o.to_owned())),
                None => (name_of(id).to_owned(), None),
            };
            let Some((_, mut value)) = f.value(&collection, &master) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let status = json!({"response": response, "time": "2026-11-08T09:00:00Z"});
            match &occurrence {
                None => value["responseStatus"] = status,
                Some(start) => {
                    let mut list = value
                        .get("exceptionOccurrences")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    list.retain(|o| o["originalStart"] != json!(start));
                    list.push(json!({"id": id, "originalStart": start, "responseStatus": status}));
                    value["exceptionOccurrences"] = json!(list);
                }
            }
            let notify = sent
                .get("sendResponse")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            f.responses.push((id.to_string(), response.into(), notify));
            f.store(&collection, &master, value);
            StatusCode::ACCEPTED.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[async_trait]
impl WriteProbe for FakeGraph {
    fn own_address(&self) -> String {
        "ada@example.com".into()
    }

    fn unknown_collection(&self) -> String {
        "calendars/AQMkNoSuch".into()
    }

    async fn read(&self, collection: &str, id: &str) -> Option<(Option<String>, Bytes)> {
        let f = self.0.lock().unwrap();
        let (seq, raw) = f.collections.get(collection)?.items.get(name_of(id))?;
        Some((Some(seq.to_string()), Bytes::from(raw.clone())))
    }

    async fn ids(&self, collection: &str) -> Vec<String> {
        let f = self.0.lock().unwrap();
        f.collections
            .get(collection)
            .map(|c| c.items.keys().map(|n| item_id(n)).collect())
            .unwrap_or_default()
    }

    async fn touch(&self, collection: &str, id: &str) {
        self.0.lock().unwrap().touch_item(collection, name_of(id));
    }

    async fn partstat(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
    ) -> Option<Partstat> {
        let (_, value) = self.0.lock().unwrap().value(collection, name_of(id))?;
        let status = match recurrence_id {
            None => value.pointer("/responseStatus/response")?.clone(),
            Some(rid) => {
                let at = NaiveDateTime::parse_from_str(rid.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
                    .ok()?;
                value
                    .get("exceptionOccurrences")?
                    .as_array()?
                    .iter()
                    .find(|o| o["originalStart"] == json!(stamp(at)))?
                    .pointer("/responseStatus/response")?
                    .clone()
            }
        };
        match status.as_str()? {
            "accepted" => Some(Partstat::Accepted),
            "tentativelyAccepted" => Some(Partstat::Tentative),
            "declined" => Some(Partstat::Declined),
            _ => Some(Partstat::NeedsAction),
        }
    }

    fn holds(&self, stored: &[u8], patch: &ItemPatch) -> bool {
        if patch.content_type == media::VCARD {
            let read = holtmail_pim::normalize_as(Format::GraphContact, ItemKind::Contact, stored);
            let wrote = holtmail_pim::normalize_as(Format::VCard, ItemKind::Contact, &patch.raw);
            let (Normalized::Contact(read), Normalized::Contact(wrote)) = (read, wrote) else {
                return false;
            };
            let emails = |c: &holtmail_pim::Contact| -> Vec<Option<String>> {
                c.emails.iter().map(|e| e.normalized.clone()).collect()
            };
            return read.display_name() == wrote.display_name() && emails(&read) == emails(&wrote);
        }
        let read = holtmail_pim::normalize_as(Format::GraphEvent, ItemKind::Event, stored);
        let wrote = holtmail_pim::normalize_as(Format::ICalendar, ItemKind::Event, &patch.raw);
        let (Normalized::Calendar(read), Normalized::Calendar(wrote)) = (read, wrote) else {
            return false;
        };
        let (Some(read), Some(wrote)) = (read.main(), wrote.main()) else {
            return false;
        };
        read.summary == wrote.summary
            && read.start.as_ref().and_then(|t| t.utc) == wrote.start.as_ref().and_then(|t| t.utc)
            && read.end.as_ref().and_then(|t| t.utc) == wrote.end.as_ref().and_then(|t| t.utc)
    }
}

fn ics(body: &str) -> ItemPatch {
    ItemPatch {
        content_type: media::ICALENDAR.into(),
        raw: Bytes::from(format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//test//EN\r\n\
             BEGIN:VEVENT\r\nUID:w1@example.com\r\nDTSTAMP:20261101T090000Z\r\n\
             DTSTART:20261201T100000Z\r\nDTEND:20261201T110000Z\r\n{body}END:VEVENT\r\nEND:VCALENDAR\r\n"
        )),
        uid: Some("w1@example.com".into()),
        fields: None,
    }
}

fn vcf(name: &str) -> ItemPatch {
    ItemPatch {
        content_type: media::VCARD.into(),
        raw: Bytes::from(format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1@example.com\r\nFN:{name}\r\n\
             EMAIL:ada@example.com\r\nEND:VCARD\r\n"
        )),
        uid: Some("c1@example.com".into()),
        fields: None,
    }
}

#[tokio::test]
async fn keeps_the_writer_contract_for_calendars() {
    let fake = FakeGraph::standard().await;
    let source = fake.source();
    source.collections().await.unwrap();
    writer_conformance(&source, &fake, CAL, CollectionKind::Calendar).await;
    let responses = fake.0.lock().unwrap().responses.clone();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0].1, "accepted");
    assert!(!responses[0].2, "no answer to the organizer when not asked");
    assert!(responses[1].0.ends_with("~2026-12-08T10:00:00Z"));
    assert!(responses[1].2);
}

#[tokio::test]
async fn keeps_the_writer_contract_for_contacts() {
    let fake = FakeGraph::standard().await;
    fake.add("contacts/F1", "Work", None, None);
    let source = fake.source();
    source.collections().await.unwrap();
    writer_conformance(&source, &fake, "contacts/F1", CollectionKind::Contacts).await;
    writer_conformance(&source, &fake, DEFAULT_CONTACTS, CollectionKind::Contacts).await;
}

#[tokio::test]
async fn updates_send_only_the_listed_fields_under_if_match() {
    let fake = FakeGraph::standard().await;
    let source = fake.source();
    let created = source
        .put(
            CAL,
            None,
            &ics("SUMMARY:Planning\r\nLOCATION:Room 4\r\n"),
            Precondition::Absent,
            "k1",
        )
        .await
        .unwrap();
    let mut change = ics("SUMMARY:Planning, moved\r\nLOCATION:Room 9\r\n");
    change.fields = Some(vec![Field::Summary]);
    let tag = created.etag.clone().unwrap();
    let written = source
        .put(
            CAL,
            Some(&created.id),
            &change,
            Precondition::IfMatch(tag.clone()),
            "k2",
        )
        .await
        .unwrap();
    assert_ne!(written.etag, Some(tag.clone()));
    let patches = fake.0.lock().unwrap().patches.clone();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].0, json!({"subject": "Planning, moved"}));
    assert_eq!(patches[0].1, Some(format!("W/\"{tag}\"")));
    let (_, stored) = fake.read(CAL, &created.id).await.unwrap();
    let stored: Value = serde_json::from_slice(&stored).unwrap();
    assert_eq!(
        stored["location"]["displayName"],
        json!("Room 4"),
        "unlisted fields stay"
    );
}

#[tokio::test]
async fn a_change_made_while_sending_fails_through_if_match() {
    let fake = FakeGraph::standard().await;
    let source = fake.source();
    let created = source
        .put(
            CAL,
            None,
            &ics("SUMMARY:Planning\r\n"),
            Precondition::Absent,
            "k1",
        )
        .await
        .unwrap();
    fake.0.lock().unwrap().race = true;
    let error = source
        .put(
            CAL,
            Some(&created.id),
            &ics("SUMMARY:Review\r\n"),
            Precondition::IfMatch(created.etag.clone().unwrap()),
            "k2",
        )
        .await
        .unwrap_err();
    let (now, _) = fake.read(CAL, &created.id).await.unwrap();
    assert!(
        matches!(&error, CollectionError::PreconditionFailed { current } if *current == now),
        "{error:?}"
    );
    fake.0.lock().unwrap().race = true;
    let error = source
        .delete(CAL, &created.id, now.as_deref())
        .await
        .unwrap_err();
    assert!(matches!(error, CollectionError::PreconditionFailed { .. }));
    assert!(fake.read(CAL, &created.id).await.is_some());
}

#[tokio::test]
async fn creates_are_idempotent_by_transaction_and_lookup() {
    let fake = FakeGraph::standard().await;
    let source = fake.source();
    let first = source
        .put(
            CAL,
            None,
            &ics("SUMMARY:Planning\r\n"),
            Precondition::Absent,
            "same",
        )
        .await
        .unwrap();
    let again = source
        .put(
            CAL,
            None,
            &ics("SUMMARY:Planning\r\n"),
            Precondition::Absent,
            "same",
        )
        .await
        .unwrap();
    assert_eq!(first.id, again.id);
    let posts: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.starts_with("POST"))
        .collect();
    assert_eq!(posts.len(), 2, "events rely on transactionId: {posts:?}");

    let contact = source
        .put(
            DEFAULT_CONTACTS,
            None,
            &vcf("Ada Quill"),
            Precondition::Absent,
            "it's-mine",
        )
        .await
        .unwrap();
    let repeat = source
        .put(
            DEFAULT_CONTACTS,
            None,
            &vcf("Ada Quill"),
            Precondition::Absent,
            "it's-mine",
        )
        .await
        .unwrap();
    assert_eq!(contact.id, repeat.id);
    let requests = fake.requests();
    assert_eq!(
        requests.iter().filter(|r| r.starts_with("POST")).count(),
        3,
        "the repeat found the contact instead of posting"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.contains("%24filter=") || r.contains("$filter="))
    );
    let f = fake.0.lock().unwrap();
    assert_eq!(
        f.collections[DEFAULT_CONTACTS]
            .keys
            .values()
            .collect::<Vec<_>>(),
        vec!["it's-mine"]
    );
    let (_, stored) = f.value(DEFAULT_CONTACTS, name_of(&contact.id)).unwrap();
    assert!(stored.get("singleValueExtendedProperties").is_none());
    assert!(!CREATE_KEY_PROPERTY.is_empty());
}

#[tokio::test]
async fn refusals_change_nothing() {
    let fake = FakeGraph::standard().await;
    let source = fake.source();
    let created = source
        .put(
            CAL,
            None,
            &ics("SUMMARY:Planning\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\n"),
            Precondition::Absent,
            "k1",
        )
        .await
        .unwrap();
    let before = fake.requests().len();
    let mut organizer = ics("SUMMARY:Planning\r\n");
    organizer.fields = Some(vec![Field::Organizer]);
    assert!(matches!(
        source
            .put(CAL, Some(&created.id), &organizer, Precondition::None, "k2")
            .await,
        Err(CollectionError::Refused(_))
    ));
    assert_eq!(
        fake.requests().len(),
        before,
        "refused before anything was sent"
    );

    let mut excluded =
        ics("SUMMARY:Planning\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE:20261208T100000Z\r\n");
    excluded.fields = Some(vec![Field::Recurrence]);
    assert!(matches!(
        source
            .put(CAL, Some(&created.id), &excluded, Precondition::None, "k3")
            .await,
        Err(CollectionError::Refused(_))
    ));
    let mut same = ics("SUMMARY:Planning\r\nRRULE:FREQ=WEEKLY;COUNT=6\r\n");
    same.fields = Some(vec![Field::Recurrence]);
    source
        .put(CAL, Some(&created.id), &same, Precondition::None, "k4")
        .await
        .unwrap();

    assert!(matches!(
        source.put(CAL, None, &ics("SUMMARY:Refuse me\r\n"), Precondition::Absent, "k5").await,
        Err(CollectionError::Refused(reason)) if reason.contains("ErrorInvalidRequest") && !reason.contains("noon")
    ));
    assert!(matches!(
        source
            .respond(CAL, &created.id, None, Partstat::NeedsAction, false)
            .await,
        Err(CollectionError::Refused(_))
    ));
    assert!(matches!(
        source
            .respond(
                CAL,
                &created.id,
                Some("20261209T100000Z"),
                Partstat::Accepted,
                false
            )
            .await,
        Err(CollectionError::Refused(_))
    ));
    assert!(matches!(
        source
            .respond(DEFAULT_CONTACTS, "AAMk-w1", None, Partstat::Accepted, false)
            .await,
        Err(CollectionError::Refused(_))
    ));
    assert!(fake.0.lock().unwrap().responses.is_empty());
}

#[tokio::test]
async fn a_write_the_account_may_not_make_is_refused() {
    let fake = FakeGraph::standard().await;
    fake.0.lock().unwrap().deny_writes = true;
    assert!(matches!(
        fake.source()
            .put(
                CAL,
                None,
                &ics("SUMMARY:Planning\r\n"),
                Precondition::Absent,
                "k1"
            )
            .await,
        Err(CollectionError::Refused(_))
    ));
}

#[tokio::test]
async fn a_listed_read_only_calendar_offers_nothing() {
    let fake = FakeGraph::standard().await;
    fake.add("calendars/AQMkShared", "Holidays", None, None);
    fake.0
        .lock()
        .unwrap()
        .collections
        .get_mut("calendars/AQMkShared")
        .unwrap()
        .read_only = true;
    let source = fake.source();
    assert!(source.capabilities("calendars/AQMkShared").update);
    source.collections().await.unwrap();
    assert_eq!(
        source.capabilities("calendars/AQMkShared"),
        WriteCaps::default()
    );
    assert_eq!(
        source.capabilities("calendars/AQMkNoSuch"),
        WriteCaps::default()
    );
    assert!(source.capabilities(CAL).update);
    assert!(source.capabilities(DEFAULT_CONTACTS).create);
}

#[test]
fn capabilities_follow_the_collection_id() {
    let source = GraphCollections::new(
        Some("https://graph.example.com/v1.0"),
        Credentials::Bearer("token".into()),
        Duration::from_secs(1),
    )
    .unwrap();
    let calendar = source.capabilities(CAL);
    assert!(calendar.respond && calendar.field_patch && calendar.tags);
    let contacts = source.capabilities("contacts/F1");
    assert!(contacts.create && !contacts.respond);
    assert_eq!(source.capabilities("elsewhere"), WriteCaps::default());
}
