use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use holtmail_access::Scope;
use holtmail_blob::BlobId;
use holtmail_core::{MessageId, Timestamp};
use holtmail_pim::ItemKind;
use holtmail_recur::Window;
use holtmail_schedule::{Host, HostCaps, Scheduler};
use holtmail_store::{Collection, ScheduleInboxEntry, WriteMode};
use holtmail_submission::Actor;

use crate::model::{Caller, DavCollection, R, Res, Unavailable, Writable};
use crate::path::{self, CollectionKey, Home};
use crate::props::{self, Node, Wanted};
use crate::read::MAX_XML;
use crate::xml::{self, CALDAV, DAV};
use crate::{Dav, Shared, body_bytes, dav_error, text, unavailable, xml_response};

/// Where an account's calendar lives, as far as scheduling goes: the server's host and what the
/// account's writer can do there, whatever the locks say. Implemented by `apps/server` over its
/// collection writers.
#[async_trait]
pub trait CalendarHosts: Send + Sync {
    /// The host of `collection` and its writer's abilities; `None` when its server cannot be
    /// asked now, so nobody can tell whether it would schedule. A collection no server holds is
    /// `Some((Host::Mail, HostCaps::default()))`.
    async fn host(&self, collection: &Collection) -> Option<(Host, HostCaps)>;
}

/// The scheduling parts of a server.
#[derive(Clone)]
pub(crate) struct Scheduling {
    pub scheduler: Arc<dyn Scheduler>,
    pub hosts: Arc<dyn CalendarHosts>,
}

/// What a caller with calendar-auto-schedule is told and scheduled as.
#[derive(Debug, Clone)]
pub(crate) struct Sched {
    /// `calendar-user-address-set`: identity addresses and the aliases they cover, normalized,
    /// each once, in identity order.
    pub own: Vec<String>,
    /// The first writable calendar, for `schedule-default-calendar-URL`.
    pub default_calendar: Option<CollectionKey>,
}

/// The submission actor of a caller.
pub(crate) fn actor(caller: &Caller) -> Actor {
    Actor {
        user: caller.user.id,
        device: Some(caller.device),
        scopes: caller.device_scopes.clone(),
    }
}

impl Shared {
    /// Whether calendar-auto-schedule is advertised to `caller`: `[serve.dav] scheduling` with a
    /// scheduler, the device's `write:pim` and `send:mail`, and an active identity the user may
    /// send as in one of the device's archives. Only these stable facts count: a calendar whose
    /// server cannot be asked right now does not turn scheduling off for a request, its
    /// scheduled writes get `503` instead, so a client that saw the capability is never left
    /// with a change nobody was told about.
    pub(crate) async fn sched(&self, caller: &Caller) -> R<Option<Sched>> {
        if !self.options.scheduling || self.scheduling.is_none() {
            return Ok(None);
        }
        if !caller.scopes.has(Scope::WritePim) || !caller.scopes.has(Scope::SendMail) {
            return Ok(None);
        }
        let mut own: Vec<String> = Vec::new();
        for archive in &caller.archives {
            if self
                .access
                .may_send_somehow(&caller.user, &caller.scopes, *archive)
                .await
                .is_err()
            {
                continue;
            }
            for identity in self.store.user_identities(caller.user.id, *archive).await? {
                if !identity.fields.active {
                    continue;
                }
                for address in [
                    Some(&identity.fields.address),
                    identity.fields.alias.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(a) = holtmail_pim::normalize_email(address)
                        && !own.contains(&a)
                    {
                        own.push(a);
                    }
                }
            }
        }
        if own.is_empty() {
            return Ok(None);
        }
        let mut default_calendar = None;
        for coll in self.collections(caller, Home::Calendars).await? {
            let writable = self.writable(caller, &coll).await?;
            if !writable.advertised() {
                continue;
            }
            default_calendar.get_or_insert(coll.key);
            break;
        }
        Ok(Some(Sched {
            own,
            default_calendar,
        }))
    }

    /// The host of a calendar and what may be written there now: the writer's abilities only
    /// while every lock is open and the collection writes through, else none.
    pub(crate) async fn host_of(
        &self,
        coll: &DavCollection,
        writable: &Writable,
    ) -> Option<(Host, HostCaps)> {
        let scheduling = self.scheduling.as_ref()?;
        let stored = coll.stored.as_ref()?;
        let (host, caps) = scheduling.hosts.host(stored).await?;
        let open = matches!(writable, Writable::Yes(_))
            && stored.write_mode == WriteMode::WriteThrough
            && self.options.switches.enabled;
        Some((host, if open { caps } else { HostCaps::default() }))
    }
}

/// The name an inbox entry is served under.
fn entry_name(entry: &ScheduleInboxEntry) -> String {
    format!("{}-{}.ics", entry.message_id, entry.part)
}

fn parse_entry_name(name: &str) -> Option<(MessageId, u32)> {
    let stem = name.strip_suffix(".ics")?;
    let (message, part) = stem.rsplit_once('-')?;
    Some((message.parse().ok()?, part.parse().ok()?))
}

impl Shared {
    async fn inbox(&self, caller: &Caller) -> R<Vec<ScheduleInboxEntry>> {
        let mut out = Vec::new();
        for archive in &caller.archives {
            out.extend(self.store.inbox_entries(*archive, caller.user.id).await?);
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.received_at));
        Ok(out)
    }

    async fn inbox_entry(&self, caller: &Caller, name: &str) -> R<Option<ScheduleInboxEntry>> {
        let Some((message, part)) = parse_entry_name(name) else {
            return Ok(None);
        };
        Ok(self
            .inbox(caller)
            .await?
            .into_iter()
            .find(|e| e.message_id == message && e.part == part))
    }

    /// The served form of an inbox entry: the message's calendar part, byte for byte.
    async fn inbox_res(&self, entry: &ScheduleInboxEntry, bytes: bool) -> R<Option<Res>> {
        let href = entry_name(entry);
        if !bytes {
            return Ok(Some(Res {
                etag: inbox_etag(entry),
                href,
                kind: ItemKind::Event,
                bytes: None,
                schedule_tag: None,
            }));
        }
        let Some(message) = self.store.get_message(entry.message_id).await? else {
            return Ok(None);
        };
        let Some(raw) = self.blobs.get(&message.blob_id).await? else {
            return Ok(None);
        };
        let Some(part) = holtmail_mime::parse(&raw)
            .ok()
            .and_then(|p| p.calendars.get(entry.part as usize).cloned())
        else {
            return Ok(None);
        };
        Ok(Some(Res {
            etag: inbox_etag(entry),
            href,
            kind: ItemKind::Event,
            bytes: Some(part),
            schedule_tag: None,
        }))
    }
}

fn inbox_etag(entry: &ScheduleInboxEntry) -> String {
    BlobId::of(
        format!(
            "{}:{}:{}",
            entry.message_id,
            entry.part,
            entry.note.as_str()
        )
        .as_bytes(),
    )
    .to_string()
}

fn inbox_item_path(caller: &Caller, name: &str) -> String {
    format!("{}{}", path::inbox(caller.user.id), path::encode(name))
}

fn multistatus(responses: String) -> Response {
    xml_response(
        StatusCode::MULTI_STATUS,
        format!("{}{responses}</d:multistatus>", xml::open("multistatus")),
    )
}

/// `PROPFIND` on the schedule inbox or outbox, or an inbox resource.
pub(crate) async fn propfind(
    dav: &Dav,
    caller: &Caller,
    which: Box_,
    depth_one: bool,
    wanted: &Wanted,
) -> R<Response> {
    let shared = dav.shared();
    let mut out = String::new();
    match which {
        Box_::Inbox => {
            out.push_str(&props::response(
                caller,
                &path::inbox(caller.user.id),
                &Node::Inbox,
                wanted,
            ));
            if depth_one {
                for entry in shared.inbox(caller).await? {
                    if let Some(res) = shared.inbox_res(&entry, wanted.needs_bytes()).await? {
                        out.push_str(&props::response(
                            caller,
                            &inbox_item_path(caller, &res.href),
                            &Node::Resource(&res, false),
                            wanted,
                        ));
                    }
                }
            }
        }
        Box_::Outbox => out.push_str(&props::response(
            caller,
            &path::outbox(caller.user.id),
            &Node::Outbox,
            wanted,
        )),
        Box_::Item(name) => {
            let Some(entry) = shared.inbox_entry(caller, &name).await? else {
                return Ok(text(StatusCode::NOT_FOUND, "not found"));
            };
            let Some(res) = shared.inbox_res(&entry, wanted.needs_bytes()).await? else {
                return Ok(text(StatusCode::NOT_FOUND, "not found"));
            };
            out.push_str(&props::response(
                caller,
                &inbox_item_path(caller, &name),
                &Node::Resource(&res, false),
                wanted,
            ));
        }
    }
    Ok(multistatus(out))
}

/// What a scheduling path names.
pub(crate) enum Box_ {
    Inbox,
    Outbox,
    Item(String),
}

/// `GET` or `HEAD` of an inbox resource: the calendar part of the archived message.
pub(crate) async fn get(dav: &Dav, caller: &Caller, name: &str, head: bool) -> Response {
    let shared = dav.shared();
    let run = async {
        let Some(entry) = shared.inbox_entry(caller, name).await? else {
            return Ok(text(StatusCode::NOT_FOUND, "not found"));
        };
        let Some(res) = shared.inbox_res(&entry, true).await? else {
            return Ok(text(StatusCode::NOT_FOUND, "not found"));
        };
        let raw = res.bytes.clone().unwrap_or_default();
        let length = raw.len();
        let mut response = (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    "text/calendar; charset=utf-8".to_owned(),
                ),
                (header::ETAG, res.quoted_etag()),
            ],
            if head { Body::empty() } else { Body::from(raw) },
        )
            .into_response();
        if head && let Ok(value) = length.to_string().parse() {
            response.headers_mut().insert(header::CONTENT_LENGTH, value);
        }
        R::Ok(response)
    };
    run.await.unwrap_or_else(|Unavailable| unavailable())
}

/// `DELETE` of an inbox resource: hidden from this user's inbox; the message stays archived.
pub(crate) async fn delete(dav: &Dav, caller: &Caller, name: &str) -> Response {
    let shared = dav.shared();
    let run = async {
        let Some(entry) = shared.inbox_entry(caller, name).await? else {
            return Ok(text(StatusCode::NOT_FOUND, "not found"));
        };
        shared
            .store
            .hide_inbox_entry(caller.user.id, entry.message_id, entry.part)
            .await?;
        R::Ok(StatusCode::NO_CONTENT.into_response())
    };
    run.await.unwrap_or_else(|Unavailable| unavailable())
}

/// `calendar-multiget` on the inbox.
pub(crate) async fn multiget(dav: &Dav, caller: &Caller, root: &xml::Element) -> Response {
    let shared = dav.shared();
    let wanted = Wanted::of(root);
    let inbox = path::inbox(caller.user.id);
    let run = async {
        let mut out = String::new();
        for href in root.children(DAV, "href") {
            let raw = path::href_path(&href.text);
            let name = raw
                .strip_prefix(&inbox)
                .and_then(path::decode)
                .unwrap_or_default();
            let found = match shared.inbox_entry(caller, &name).await? {
                Some(entry) => shared.inbox_res(&entry, true).await?,
                None => None,
            };
            match found {
                Some(res) => out.push_str(&props::response(
                    caller,
                    &inbox_item_path(caller, &name),
                    &Node::Resource(&res, false),
                    &wanted,
                )),
                None => out.push_str(&props::status_response(raw, "404 Not Found")),
            }
        }
        R::Ok(multistatus(out))
    };
    run.await.unwrap_or_else(|Unavailable| unavailable())
}

/// The content lines of an iCalendar text, unfolded: `(name, parameters, value)`.
fn lines(text: &str) -> Vec<(String, String, String)> {
    let mut unfolded: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match line.strip_prefix([' ', '\t']) {
            Some(more) if !unfolded.is_empty() => {
                if let Some(last) = unfolded.last_mut() {
                    last.push_str(more);
                }
            }
            _ => unfolded.push(line.to_owned()),
        }
    }
    unfolded
        .into_iter()
        .filter_map(|line| {
            let mut quoted = false;
            let colon = line.char_indices().find_map(|(i, c)| match c {
                '"' => {
                    quoted = !quoted;
                    None
                }
                ':' if !quoted => Some(i),
                _ => None,
            })?;
            let (head, value) = line.split_at(colon);
            let (name, params) = head.split_once(';').unwrap_or((head, ""));
            Some((
                name.to_ascii_uppercase(),
                params.to_owned(),
                value[1..].to_owned(),
            ))
        })
        .collect()
}

fn mailto(value: &str) -> Option<String> {
    let value = value.trim();
    let address = value
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("mailto:"))
        .map_or(value, |_| &value[7..]);
    holtmail_pim::normalize_email(address)
}

/// `POST` to the schedule outbox: a free-busy request for the user's own addresses, answered
/// from the served calendars; any other address is `3.7`. Nothing is sent to anyone.
pub(crate) async fn post(dav: &Dav, caller: &Caller, headers: &HeaderMap, body: Body) -> Response {
    let Some(sched) = &caller.sched else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let essence = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase());
    if essence.as_deref() != Some("text/calendar") {
        return dav_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            CALDAV,
            "supported-calendar-data",
        );
    }
    let Some(bytes) = body_bytes(body, MAX_XML).await else {
        return dav_error(StatusCode::FORBIDDEN, CALDAV, "max-resource-size");
    };
    let text_body = String::from_utf8_lossy(&bytes);
    let lines = lines(&text_body);
    let value = |name: &str| {
        lines
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, _, v)| v.clone())
    };
    let is_freebusy = lines
        .iter()
        .any(|(n, _, v)| n == "BEGIN" && v.eq_ignore_ascii_case("VFREEBUSY"));
    if !is_freebusy || !value("METHOD").is_some_and(|m| m.eq_ignore_ascii_case("REQUEST")) {
        return dav_error(StatusCode::FORBIDDEN, CALDAV, "valid-calendar-data");
    }
    let organizer = value("ORGANIZER").and_then(|o| mailto(&o));
    if !organizer.as_ref().is_some_and(|o| sched.own.contains(o)) {
        return dav_error(StatusCode::FORBIDDEN, CALDAV, "organizer-allowed");
    }
    let (Some(from), Some(to)) = (
        value("DTSTART").and_then(|v| crate::filter::parse_utc(&v)),
        value("DTEND").and_then(|v| crate::filter::parse_utc(&v)),
    ) else {
        return dav_error(StatusCode::FORBIDDEN, CALDAV, "valid-calendar-data");
    };
    if from >= to {
        return dav_error(StatusCode::FORBIDDEN, CALDAV, "valid-calendar-data");
    }
    let uid = value("UID");
    let attendees: Vec<String> = lines
        .iter()
        .filter(|(n, _, _)| n == "ATTENDEE")
        .map(|(_, _, v)| v.trim().to_owned())
        .collect();
    let shared = dav.shared();
    let run = async {
        let mut busy: Option<Vec<(i64, i64)>> = None;
        let mut out = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<c:schedule-response xmlns:d=\"{DAV}\" xmlns:c=\"{CALDAV}\">"
        );
        for attendee in &attendees {
            let recipient = format!(
                "<c:recipient><d:href>{}</d:href></c:recipient>",
                xml::escape(attendee)
            );
            let own = mailto(attendee).filter(|a| sched.own.contains(a));
            let Some(address) = own else {
                out.push_str(&format!(
                    "<c:response>{recipient}<c:request-status>3.7;Invalid calendar user</c:request-status></c:response>"
                ));
                continue;
            };
            if busy.is_none() {
                busy = Some(shared.busy(caller, Window { from, to }).await?);
            }
            let calendar = freebusy_reply(
                organizer.as_deref().unwrap_or_default(),
                &address,
                uid.as_deref(),
                from,
                to,
                busy.as_deref().unwrap_or_default(),
            );
            out.push_str(&format!(
                "<c:response>{recipient}<c:request-status>2.0;Success</c:request-status><c:calendar-data>{}</c:calendar-data></c:response>",
                xml::escape(&calendar)
            ));
        }
        out.push_str("</c:schedule-response>");
        R::Ok(xml_response(StatusCode::OK, out))
    };
    run.await.unwrap_or_else(|Unavailable| unavailable())
}

impl Shared {
    /// Busy periods of every calendar the caller sees, merged.
    async fn busy(&self, caller: &Caller, window: Window) -> R<Vec<(i64, i64)>> {
        let mut periods = Vec::new();
        for coll in self.collections(caller, Home::Calendars).await? {
            for res in self.members(caller, &coll, true).await? {
                if let Some(raw) = &res.bytes {
                    periods.extend(crate::filter::busy(res.kind, raw, window));
                }
            }
        }
        periods.sort();
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for (start, end) in periods {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        Ok(merged)
    }
}

fn freebusy_reply(
    organizer: &str,
    attendee: &str,
    uid: Option<&str>,
    from: i64,
    to: i64,
    busy: &[(i64, i64)],
) -> String {
    let fmt = crate::filter::format_utc;
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//DAV//EN\r\nMETHOD:REPLY\r\nBEGIN:VFREEBUSY\r\n",
    );
    if let Some(uid) = uid {
        out.push_str(&format!("UID:{uid}\r\n"));
    }
    out.push_str(&format!(
        "DTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nORGANIZER:mailto:{organizer}\r\nATTENDEE:mailto:{attendee}\r\n",
        fmt(Timestamp::now().as_millis()),
        fmt(from),
        fmt(to)
    ));
    for (start, end) in busy {
        out.push_str(&format!(
            "FREEBUSY:{}/{}\r\n",
            fmt((*start).max(from)),
            fmt((*end).min(to))
        ));
    }
    out.push_str("END:VFREEBUSY\r\nEND:VCALENDAR\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_inbox_names_and_lines() {
        let id = MessageId::new();
        assert_eq!(parse_entry_name(&format!("{id}-2.ics")), Some((id, 2)));
        assert_eq!(parse_entry_name("x.ics"), None);
        let lines = lines("BEGIN:VCALENDAR\r\nATTENDEE;CN=\"A: B\":mailto:a@exa\r\n mple.com\r\n");
        assert_eq!(lines[1].0, "ATTENDEE");
        assert_eq!(lines[1].2, "mailto:a@example.com");
        assert_eq!(
            mailto("MAILTO:Bea@Example.org").as_deref(),
            Some("bea@example.org")
        );
    }
}
