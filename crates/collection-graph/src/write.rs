use async_trait::async_trait;
use bytes::Bytes;
use calcard::common::timezone::Tz;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc};
use holtmail_collection::{
    CollectionError, CollectionWriter, ItemPatch, Partstat, Precondition, RemoteItem, WriteCaps,
    Written,
};
use holtmail_pim::{Format, ItemKind, Normalized};
use reqwest::Method;
use serde_json::{Value, json};
use url::Url;

use crate::convert::{self, Draft};
use crate::http::{Reply, Scope, Write};
use crate::{GraphCollections, Place, json, place, protocol, string};

/// The single-value extended property a contact created by Holtmail carries its idempotency
/// key in, so a repeated create finds it.
pub(crate) const CREATE_KEY_PROPERTY: &str =
    "String {6f1d2c3b-8a4e-4b7f-9c5d-0e2a7b3c9d14} Name HoltmailCreateKey";

fn bare(tag: &str) -> &str {
    tag.trim().trim_start_matches("W/").trim_matches('"')
}

fn quoted(tag: &str) -> String {
    format!("W/\"{}\"", bare(tag))
}

fn stale(current: Option<String>) -> CollectionError {
    CollectionError::PreconditionFailed { current }
}

fn action(answer: Partstat) -> Result<&'static str, CollectionError> {
    match answer {
        Partstat::Accepted => Ok("accept"),
        Partstat::Tentative => Ok("tentativelyAccept"),
        Partstat::Declined => Ok("decline"),
        Partstat::NeedsAction => Err(CollectionError::Refused(
            "Microsoft Graph cannot take an answer back".into(),
        )),
    }
}

/// The moment a `RECURRENCE-ID` names: UTC with `Z`, else a local time in `zone`; or a date
/// for an all-day series.
enum Occurrence {
    At(i64),
    On(NaiveDate),
}

fn occurrence_of(rid: &str, zone: Option<&str>) -> Result<Occurrence, CollectionError> {
    let compact: String = rid
        .trim()
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    let unreadable = || CollectionError::Refused("an unreadable occurrence".into());
    if let Some(local) = compact.strip_suffix('Z') {
        let at = NaiveDateTime::parse_from_str(local, "%Y%m%dT%H%M%S").map_err(|_| unreadable())?;
        return Ok(Occurrence::At(at.and_utc().timestamp_millis()));
    }
    if compact.len() == 8 {
        return NaiveDate::parse_from_str(&compact, "%Y%m%d")
            .map(Occurrence::On)
            .map_err(|_| unreadable());
    }
    let at = NaiveDateTime::parse_from_str(&compact, "%Y%m%dT%H%M%S").map_err(|_| unreadable())?;
    let tz = zone
        .and_then(|z| z.parse::<Tz>().ok())
        .filter(|tz| !matches!(tz, Tz::Floating))
        .unwrap_or(Tz::UTC);
    let utc = tz
        .from_local_datetime(&at)
        .earliest()
        .ok_or_else(unreadable)?
        .timestamp_millis();
    Ok(Occurrence::At(utc))
}

fn instant(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| {
            let local = text.trim().trim_end_matches('Z');
            let local = local.get(..19).unwrap_or(local);
            NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|t| t.and_utc())
        })
}

fn exdates(raw: &[u8]) -> Vec<i64> {
    match holtmail_pim::normalize_as(Format::GraphEvent, ItemKind::Event, raw) {
        Normalized::Calendar(object) => object
            .main()
            .map(|m| m.exdates.iter().filter_map(|t| t.utc).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

impl GraphCollections {
    fn item_url(&self, place: &Place, id: &str) -> Result<Url, CollectionError> {
        match place {
            Place::Contacts(folder) => {
                let mut path = Self::contacts_path(folder);
                path.push(id);
                self.endpoint(&path, &[])
            }
            Place::Calendar(calendar) => {
                self.endpoint(&["me", "calendars", calendar, "events", id], &[])
            }
        }
    }

    fn draft(place: &Place, patch: &ItemPatch, create: bool) -> Result<Draft, CollectionError> {
        match place {
            Place::Contacts(_) => convert::contact(patch, create),
            Place::Calendar(_) => convert::event(patch, create),
        }
    }

    async fn current_tag(
        &self,
        place: &Place,
        id: &str,
    ) -> Result<Option<String>, CollectionError> {
        Ok(self
            .fetch_one(place, id.to_owned())
            .await?
            .and_then(|i| i.etag))
    }

    /// What a write answered: the item's id and tag from its JSON, else read back.
    async fn written(
        &self,
        place: &Place,
        body: &Bytes,
        id: Option<&str>,
    ) -> Result<Written, CollectionError> {
        if let Ok(value) = serde_json::from_slice::<Value>(body)
            && let Some(found) = string(&value, "id")
        {
            return Ok(Written {
                id: found,
                etag: string(&value, "changeKey"),
                item: None,
            });
        }
        let id = id.ok_or_else(|| protocol("Microsoft Graph answered a create without an id"))?;
        let item = self
            .fetch_one(place, id.to_owned())
            .await?
            .ok_or_else(|| stale(None))?;
        Ok(Written {
            id: item.id.clone(),
            etag: item.etag.clone(),
            item: Some(item),
        })
    }

    async fn created_contact(
        &self,
        folder: &Option<String>,
        key: &str,
    ) -> Result<Option<Written>, CollectionError> {
        let filter = format!(
            "singleValueExtendedProperties/Any(ep: ep/id eq '{CREATE_KEY_PROPERTY}' and ep/value eq '{}')",
            key.replace('\'', "''")
        );
        let url = self.endpoint(
            &Self::contacts_path(folder),
            &[("$filter", &filter), ("$select", "id,changeKey")],
        )?;
        let found = self
            .all(url, Scope::Account)
            .await?
            .ok_or(CollectionError::Gone)?;
        Ok(found.first().and_then(|item| {
            Some(Written {
                id: string(item, "id")?,
                etag: string(item, "changeKey"),
                item: None,
            })
        }))
    }

    async fn create(
        &self,
        place: &Place,
        patch: &ItemPatch,
        key: &str,
    ) -> Result<Written, CollectionError> {
        let mut body = Self::draft(place, patch, true)?.fields;
        let url = match place {
            Place::Calendar(calendar) => {
                body.insert("transactionId".into(), json!(key));
                self.endpoint(&["me", "calendars", calendar, "events"], &[])?
            }
            Place::Contacts(folder) => {
                if let Some(found) = self.created_contact(folder, key).await? {
                    return Ok(found);
                }
                body.insert(
                    "singleValueExtendedProperties".into(),
                    json!([{ "id": CREATE_KEY_PROPERTY, "value": key }]),
                );
                self.endpoint(&Self::contacts_path(folder), &[])?
            }
        };
        let body = Value::Object(body);
        let reply = self
            .http
            .write(Write {
                method: Method::POST,
                url,
                body: Some(&body),
                if_match: None,
            })
            .await?;
        match reply {
            Reply::Done(answer) => self.written(place, &answer, None).await,
            Reply::Missing => Err(CollectionError::Gone),
            Reply::Stale => Err(stale(None)),
            Reply::Refused(reason) => Err(CollectionError::Refused(reason)),
        }
    }

    async fn update(
        &self,
        place: &Place,
        id: &str,
        patch: &ItemPatch,
        precondition: Precondition,
    ) -> Result<Written, CollectionError> {
        let draft = Self::draft(place, patch, false)?;
        let current: RemoteItem = self
            .fetch_one(place, id.to_owned())
            .await?
            .ok_or_else(|| stale(None))?;
        let tag = current.etag.clone();
        let holds = match &precondition {
            Precondition::None => true,
            Precondition::Absent => false,
            Precondition::IfMatch(wanted) => tag.as_deref().map(bare) == Some(bare(wanted)),
            Precondition::IfUnchanged(raw) => *raw == current.raw,
        };
        if !holds {
            return Err(stale(tag));
        }
        if let Some(mut wanted) = draft.exdates {
            let mut held = exdates(&current.raw);
            wanted.sort_unstable();
            wanted.dedup();
            held.sort_unstable();
            held.dedup();
            if wanted != held {
                return Err(CollectionError::Refused(
                    "Microsoft Graph cannot exclude or restore occurrences by a change".into(),
                ));
            }
        }
        if draft.fields.is_empty() {
            return Ok(Written {
                id: current.id.clone(),
                etag: tag,
                item: Some(current),
            });
        }
        let if_match = match precondition {
            Precondition::None => None,
            _ => tag.as_deref().map(quoted),
        };
        let body = Value::Object(draft.fields);
        let reply = self
            .http
            .write(Write {
                method: Method::PATCH,
                url: self.item_url(place, id)?,
                body: Some(&body),
                if_match: if_match.as_deref(),
            })
            .await?;
        match reply {
            Reply::Done(answer) => self.written(place, &answer, Some(id)).await,
            Reply::Missing => Err(stale(None)),
            Reply::Stale => Err(stale(self.current_tag(place, id).await?)),
            Reply::Refused(reason) => Err(CollectionError::Refused(reason)),
        }
    }

    async fn occurrence(
        &self,
        calendar: &str,
        master: &RemoteItem,
        rid: &str,
    ) -> Result<String, CollectionError> {
        let value: Value = json(&master.raw)?;
        let zone = string(&value, "originalStartTimeZone");
        let wanted = occurrence_of(rid, zone.as_deref())?;
        let middle = match wanted {
            Occurrence::At(ms) => DateTime::from_timestamp_millis(ms),
            Occurrence::On(date) => date.and_hms_opt(12, 0, 0).map(|t| t.and_utc()),
        }
        .ok_or_else(|| CollectionError::Refused("an unreadable occurrence".into()))?;
        let format = |t: DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let from = format(middle - TimeDelta::days(2));
        let to = format(middle + TimeDelta::days(2));
        let url = self.endpoint(
            &[
                "me",
                "calendars",
                calendar,
                "events",
                &master.id,
                "instances",
            ],
            &[
                ("startDateTime", &from),
                ("endDateTime", &to),
                ("$select", "id,originalStart"),
            ],
        )?;
        let instances = self
            .all(url, Scope::Item)
            .await?
            .ok_or_else(|| stale(None))?;
        instances
            .iter()
            .find(|i| {
                let Some(at) = i
                    .get("originalStart")
                    .and_then(Value::as_str)
                    .and_then(instant)
                else {
                    return false;
                };
                match wanted {
                    Occurrence::At(ms) => at.timestamp_millis() == ms,
                    Occurrence::On(date) => at.date_naive() == date,
                }
            })
            .and_then(|i| string(i, "id"))
            .ok_or_else(|| {
                CollectionError::Refused("the series has no occurrence at that time".into())
            })
    }
}

#[async_trait]
impl CollectionWriter for GraphCollections {
    fn capabilities(&self, collection: &str) -> WriteCaps {
        let listed = self.listed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(editable) = listed.as_ref()
            && editable.get(collection) != Some(&true)
        {
            return WriteCaps::default();
        }
        drop(listed);
        match place(collection) {
            Ok(place) => WriteCaps {
                create: true,
                update: true,
                delete: true,
                respond: matches!(place, Place::Calendar(_)),
                field_patch: true,
                tags: true,
                max_size: None,
                keeps_uid: false,
            },
            Err(_) => WriteCaps::default(),
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
        let place = place(collection)?;
        match id {
            None => self.create(&place, change, idempotency).await,
            Some(id) => self.update(&place, id, change, precondition).await,
        }
    }

    async fn delete(
        &self,
        collection: &str,
        id: &str,
        if_match: Option<&str>,
    ) -> Result<(), CollectionError> {
        let place = place(collection)?;
        let mut header = None;
        if let Some(wanted) = if_match {
            let Some(tag) = self.fetch_one(&place, id.to_owned()).await?.map(|i| i.etag) else {
                return Ok(());
            };
            if tag.as_deref().map(bare) != Some(bare(wanted)) {
                return Err(stale(tag));
            }
            header = Some(quoted(wanted));
        }
        let reply = self
            .http
            .write(Write {
                method: Method::DELETE,
                url: self.item_url(&place, id)?,
                body: None,
                if_match: header.as_deref(),
            })
            .await?;
        match reply {
            Reply::Done(_) | Reply::Missing => Ok(()),
            Reply::Stale => Err(stale(self.current_tag(&place, id).await?)),
            Reply::Refused(reason) => Err(CollectionError::Refused(reason)),
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
        let place = place(collection)?;
        let Place::Calendar(calendar) = &place else {
            return Err(CollectionError::Refused(
                "contacts have no invitations".into(),
            ));
        };
        let verb = action(answer)?;
        let master = self
            .fetch_one(&place, id.to_owned())
            .await?
            .ok_or_else(|| stale(None))?;
        let target = match recurrence_id {
            None => master.id.clone(),
            Some(rid) => self.occurrence(calendar, &master, rid).await?,
        };
        let body = json!({ "sendResponse": notify_organizer });
        let reply = self
            .http
            .write(Write {
                method: Method::POST,
                url: self.endpoint(&["me", "calendars", calendar, "events", &target, verb], &[])?,
                body: Some(&body),
                if_match: None,
            })
            .await?;
        match reply {
            Reply::Done(_) => {}
            Reply::Missing => return Err(stale(None)),
            Reply::Stale => return Err(stale(self.current_tag(&place, id).await?)),
            Reply::Refused(reason) => return Err(CollectionError::Refused(reason)),
        }
        let item = self
            .fetch_one(&place, id.to_owned())
            .await?
            .ok_or_else(|| stale(None))?;
        Ok(Written {
            id: item.id.clone(),
            etag: item.etag.clone(),
            item: Some(item),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrences_read_in_their_zone() {
        let at = |rid: &str, zone: Option<&str>| match occurrence_of(rid, zone).unwrap() {
            Occurrence::At(ms) => DateTime::from_timestamp_millis(ms)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string(),
            Occurrence::On(date) => date.to_string(),
        };
        assert_eq!(at("20261208T100000Z", None), "2026-12-08T10:00:00Z");
        assert_eq!(
            at("20261208T100000", Some("Europe/Berlin")),
            "2026-12-08T09:00:00Z"
        );
        assert_eq!(
            at("20261208T100000", Some("W. Europe Standard Time")),
            "2026-12-08T09:00:00Z"
        );
        assert_eq!(at("2026-12-08", None), "2026-12-08");
        assert!(occurrence_of("next tuesday", None).is_err());
    }

    #[test]
    fn tags_compare_without_their_quoting() {
        assert_eq!(bare("W/\"abc\""), "abc");
        assert_eq!(bare("abc"), "abc");
        assert_eq!(quoted("abc"), "W/\"abc\"");
        assert_eq!(quoted("W/\"abc\""), "W/\"abc\"");
    }
}
