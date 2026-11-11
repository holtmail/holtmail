use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use holtmail_core::{ItemId, Timestamp};
use holtmail_pim::{Format, ItemKind};
use holtmail_store::{
    AuditKind, ChangeClass, ChangeContent, ChangeOp, ChangeOrigin, ChangeTarget, CollectionKind,
    Effective, Expect, ItemOrder, ItemQuery, NewAuditEntry, NewItemChange, RecordedChange,
    RemovedFilter, StoreError,
};

use crate::model::{Caller, DavCollection, R, Res, Unavailable, Writable};
use crate::path::{Home, Target};
use crate::props::{self, MAX_ICALENDAR, MAX_VCARD};
use crate::read::MAX_XML;
use crate::sched::actor;
use crate::xml::{self, APPLE, CALDAV, CARDDAV, DAV, Element};
use crate::{Dav, body_bytes, dav_error, text, unavailable, xml_response};
use holtmail_schedule::{Change, ClientPut, Delivery, Origin, ScheduleError};

/// Display properties a client may set on a collection, for the signed-in user only.
const DISPLAY: [(&str, &str); 6] = [
    (DAV, "displayname"),
    (APPLE, "calendar-color"),
    (APPLE, "calendar-order"),
    (CALDAV, "calendar-description"),
    (CARDDAV, "addressbook-description"),
    (CALDAV, "calendar-timezone"),
];

/// Longest display property value kept.
const MAX_DISPLAY: usize = 16 * 1024;

fn data_ns(home: Home) -> &'static str {
    match home {
        Home::Calendars => CALDAV,
        Home::AddressBooks => CARDDAV,
    }
}

fn denied() -> Response {
    dav_error(StatusCode::FORBIDDEN, DAV, "need-privileges")
}

fn precondition_failed() -> Response {
    text(StatusCode::PRECONDITION_FAILED, "the resource changed")
}

/// Whether an `If-Match` value holds for what is served.
fn etag_matches(value: &str, res: &Res) -> bool {
    let ours = res.quoted_etag();
    value
        .split(',')
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == ours)
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

struct Located {
    coll: DavCollection,
    href: String,
    writable: Writable,
    effective: Option<Effective>,
    served: Option<Res>,
}

async fn locate(dav: &Dav, caller: &Caller, target: &Target) -> Result<Located, Box<Response>> {
    let Target::Resource(home, _, key, href) = target else {
        return Err(Box::new(text(
            StatusCode::METHOD_NOT_ALLOWED,
            "not a resource",
        )));
    };
    let shared = dav.shared();
    let inner = async {
        let Some(coll) = shared.collection(caller, *home, key).await? else {
            return Ok(Err(Box::new(text(StatusCode::NOT_FOUND, "not found"))));
        };
        let Some(stored) = coll.stored.clone() else {
            return Ok(Err(Box::new(denied())));
        };
        let writable = shared.writable(caller, &coll).await?;
        if matches!(writable, Writable::No) {
            return Ok(Err(Box::new(denied())));
        }
        let effective = shared
            .store
            .effective(&ChangeTarget::Href {
                collection: stored.id,
                href: href.clone(),
            })
            .await?;
        let served = match &effective {
            Some(e) => shared.resolve(&coll, e.clone(), true).await?,
            None => None,
        };
        R::Ok(Ok(Located {
            coll,
            href: href.clone(),
            writable,
            effective,
            served,
        }))
    };
    match inner.await {
        Ok(result) => result,
        Err(Unavailable) => Err(Box::new(unavailable())),
    }
}

/// Checks `If-Match` and `If-None-Match: *` against what is served; the precondition to record
/// with, or the 412.
fn preconditions(headers: &HeaderMap, located: &Located) -> Result<Expect, Box<Response>> {
    let state = located.effective.as_ref().map(|e| e.state);
    if header_text(headers, "if-none-match") == Some("*") {
        if located.served.is_some() {
            return Err(Box::new(precondition_failed()));
        }
        return Ok(Expect::Absent);
    }
    if let Some(value) = header_text(headers, "if-match") {
        return match (&located.served, state) {
            (Some(res), Some(state)) if etag_matches(value, res) => Ok(Expect::State(state)),
            _ => Err(Box::new(precondition_failed())),
        };
    }
    Ok(Expect::Any)
}

fn content_type_fits(headers: &HeaderMap, home: Home) -> bool {
    let Some(value) = header_text(headers, header::CONTENT_TYPE.as_str()) else {
        return true;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match home {
        Home::Calendars => essence == "text/calendar",
        Home::AddressBooks => matches!(
            essence.as_str(),
            "text/vcard" | "text/x-vcard" | "text/directory"
        ),
    }
}

async fn uid_taken(dav: &Dav, coll: &DavCollection, uid: &str) -> R<bool> {
    let Some(stored) = &coll.stored else {
        return Ok(false);
    };
    let mut after = None;
    loop {
        let page = dav
            .shared()
            .store
            .list_items(&ItemQuery {
                archive_id: coll.archive,
                kind: None,
                collection_id: Some(stored.id),
                removed: RemovedFilter::Hide,
                order: ItemOrder::Title,
                after,
                limit: 500,
            })
            .await?;
        if page
            .items
            .iter()
            .any(|i| i.item.key.as_deref() == Some(uid))
        {
            return Ok(true);
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(false),
        }
    }
}

async fn after_change(
    dav: &Dav,
    caller: &Caller,
    located: &Located,
    change: &holtmail_store::ItemChange,
) {
    let shared = dav.shared();
    if let Some(item) = change.item_id {
        if let Err(error) = shared.edits.derive(item).await {
            tracing::warn!(%item, %error, "dav: derived data not rebuilt after an edit");
        }
        if let Err(error) = shared.ingest.commit().await {
            tracing::warn!(%error, "dav: search index not committed after an edit");
        }
    }
    let entry = NewAuditEntry {
        at: Timestamp::now(),
        user: Some(caller.user.id),
        kind: AuditKind::PimEdit,
        archive: Some(change.archive_id),
        account: located.writable.account().map(|a| a.id),
        targets: vec![change.id.to_string(), format!("device:{}", caller.device)],
        count: 1,
    };
    if let Err(error) = shared.store.append_audit(&entry).await {
        tracing::warn!(change = %change.id, %error, "dav: recorded change not audited");
    }
    if let Some(hook) = &shared.hook {
        hook(change.archive_id);
    }
}

async fn record(
    dav: &Dav,
    new: &NewItemChange,
    expect: Expect,
) -> Result<holtmail_store::ItemChange, Box<Response>> {
    match dav.shared().store.record_change(new, expect).await {
        Ok(RecordedChange::Recorded(change)) => Ok(*change),
        Ok(RecordedChange::PreconditionFailed { .. }) => Err(Box::new(precondition_failed())),
        Err(StoreError::Invalid(_)) => Err(Box::new(text(
            StatusCode::FORBIDDEN,
            "this change cannot be made to the resource",
        ))),
        Err(StoreError::NotFound(_)) => Err(Box::new(text(StatusCode::NOT_FOUND, "not found"))),
        Err(error) => {
            tracing::warn!(%error, "dav: change not recorded");
            Err(Box::new(unavailable()))
        }
    }
}

/// Whether a write of `kind` to this collection goes through the scheduler.
fn scheduled(dav: &Dav, caller: &Caller, located: &Located, kind: ItemKind) -> bool {
    caller.sched.is_some()
        && dav.shared().scheduling.is_some()
        && located.coll.home == Home::Calendars
        && kind == ItemKind::Event
}

fn schedule_error(error: ScheduleError) -> Box<Response> {
    match error {
        ScheduleError::Invalid(_) => Box::new(dav_error(
            StatusCode::FORBIDDEN,
            CALDAV,
            "valid-calendar-data",
        )),
        ScheduleError::Unavailable(reason) => {
            tracing::warn!(%reason, "dav: scheduling unavailable");
            Box::new(unavailable())
        }
    }
}

/// Delivers what a change of an event calls for, before it is recorded.
async fn deliver(
    dav: &Dav,
    caller: &Caller,
    located: &Located,
    next: Option<Bytes>,
) -> Result<Delivery, Box<Response>> {
    let shared = dav.shared();
    let (Some(scheduling), Some(sched)) = (&shared.scheduling, &caller.sched) else {
        return Err(Box::new(unavailable()));
    };
    let Some((host, caps)) = shared.host_of(&located.coll, &located.writable).await else {
        return Err(Box::new(unavailable()));
    };
    scheduling
        .scheduler
        .deliver(&Change {
            actor: actor(caller),
            archive: located.coll.archive,
            origin: Origin::Device,
            host,
            caps,
            own: sched.own.clone(),
            previous: located.served.as_ref().and_then(|r| r.bytes.clone()),
            next,
            reply_as: None,
            references: Vec::new(),
            comment: None,
        })
        .await
        .map_err(schedule_error)
}

/// Lets the held messages of a delivery go once its change is recorded. A failure is logged:
/// the change exists, and its messages are released at the next start.
async fn confirm(dav: &Dav, caller: &Caller, delivery: Option<&Delivery>) {
    let (Some(scheduling), Some(delivery)) = (&dav.shared().scheduling, delivery) else {
        return;
    };
    if delivery.queued.is_empty() {
        return;
    }
    if let Err(error) = scheduling.scheduler.confirm(&actor(caller), delivery).await {
        tracing::warn!(%error, "dav: scheduling messages not released");
    }
}

/// Takes back what a delivery queued when its change could not be recorded.
async fn withdraw(dav: &Dav, caller: &Caller, delivery: Option<&Delivery>) {
    let (Some(scheduling), Some(delivery)) = (&dav.shared().scheduling, delivery) else {
        return;
    };
    if delivery.queued.is_empty() {
        return;
    }
    if let Err(error) = scheduling
        .scheduler
        .withdraw(&actor(caller), delivery)
        .await
    {
        tracing::warn!(%error, "dav: scheduling messages not taken back");
    }
}

/// Checks `If-Schedule-Tag-Match` against what is served: the state to record against and
/// whether the header was given and matched, or the 412.
fn schedule_tag_matches(
    headers: &HeaderMap,
    located: &Located,
    expect: Expect,
) -> Result<(Expect, bool), Box<Response>> {
    let Some(value) = header_text(headers, "if-schedule-tag-match") else {
        return Ok((expect, false));
    };
    let ours = located.served.as_ref().and_then(Res::quoted_schedule_tag);
    let state = located.effective.as_ref().map(|e| e.state);
    match (ours, state) {
        (Some(ours), Some(state)) if value.split(',').map(str::trim).any(|t| t == ours) => Ok((
            match expect {
                Expect::Any => Expect::State(state),
                other => other,
            },
            true,
        )),
        _ => Err(Box::new(precondition_failed())),
    }
}

fn kind_for(located: &Located, body: &[u8]) -> ItemKind {
    let stored = located.coll.stored.as_ref();
    if stored.is_some_and(|c| c.kind == CollectionKind::Contacts) {
        return ItemKind::Contact;
    }
    match located
        .effective
        .as_ref()
        .and_then(|e| e.item.as_ref())
        .map(|i| i.item.kind)
    {
        Some(kind) => ItemKind::parse(kind.as_str()).unwrap_or(ItemKind::Other),
        None => Format::ICalendar.kind(body),
    }
}

/// `PUT`: a held create, or a held change of what is served, through `pim-edit`'s validation
/// and classification and the store's compare-and-set.
pub(crate) async fn put(
    dav: &Dav,
    caller: &Caller,
    target: &Target,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let Target::Resource(home, ..) = target else {
        return text(StatusCode::METHOD_NOT_ALLOWED, "not a resource");
    };
    let home = *home;
    let ns = data_ns(home);
    let limit = match home {
        Home::Calendars => MAX_ICALENDAR,
        Home::AddressBooks => MAX_VCARD,
    };
    let located = match locate(dav, caller, target).await {
        Ok(located) => located,
        Err(response) => return *response,
    };
    if !content_type_fits(headers, home) {
        return dav_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ns,
            match home {
                Home::Calendars => "supported-calendar-data",
                Home::AddressBooks => "supported-address-data",
            },
        );
    }
    let Some(bytes) = body_bytes(body, limit).await else {
        return dav_error(StatusCode::FORBIDDEN, ns, "max-resource-size");
    };
    let expect = match preconditions(headers, &located) {
        Ok(expect) => expect,
        Err(response) => return *response,
    };
    let (expect, tag_matched) = match caller.sched.is_some() && home == Home::Calendars {
        true => match schedule_tag_matches(headers, &located, expect) {
            Ok(checked) => checked,
            Err(response) => return *response,
        },
        false => (expect, false),
    };
    match put_inner(dav, caller, &located, bytes, expect, tag_matched).await {
        Ok(response) => response,
        Err(Unavailable) => unavailable(),
    }
}

async fn put_inner(
    dav: &Dav,
    caller: &Caller,
    located: &Located,
    bytes: Bytes,
    expect: Expect,
    tag_matched: bool,
) -> R<Response> {
    let shared = dav.shared();
    let Some(stored) = &located.coll.stored else {
        return Ok(denied());
    };
    let ns = data_ns(located.coll.home);
    let kind = kind_for(located, &bytes);
    let item = located
        .effective
        .as_ref()
        .and_then(|e| e.item.as_ref())
        .map(|i| i.item.id);
    let open_create = located
        .effective
        .as_ref()
        .is_some_and(|e| e.item.is_none() && e.served && e.change.is_some());
    let (target, op) = match (item, open_create) {
        (Some(item), _) => (ChangeTarget::Item { item }, ChangeOp::Update),
        (None, true) => (
            ChangeTarget::Href {
                collection: stored.id,
                href: located.href.clone(),
            },
            ChangeOp::Update,
        ),
        (None, false) => (
            ChangeTarget::Href {
                collection: stored.id,
                href: located.href.clone(),
            },
            ChangeOp::Create,
        ),
    };
    let base = match located.effective.as_ref().and_then(|e| e.latest.as_ref()) {
        Some(latest) if item.is_some() => shared
            .edits
            .version_text(latest, kind)
            .await?
            .map(|text| (latest.number, text)),
        _ => None,
    };
    let replaces = located.served.as_ref().and_then(|r| r.bytes.clone());
    let client = bytes.clone();
    let scheduled = scheduled(dav, caller, located, kind);
    let bytes = match (scheduled, &caller.sched, &shared.scheduling) {
        (true, Some(sched), Some(scheduling)) => {
            let prepared = scheduling
                .scheduler
                .prepare(&ClientPut {
                    previous: replaces.clone(),
                    body: bytes,
                    own: sched.own.clone(),
                    keep_answers: tag_matched,
                })
                .await;
            match prepared {
                Ok(prepared) => prepared.body,
                Err(error) => return Ok(*schedule_error(error)),
            }
        }
        _ => bytes,
    };
    let valid = match holtmail_pim_edit::validate(kind, &bytes, replaces.as_deref()) {
        Ok(valid) => valid,
        Err(invalid) => {
            return Ok(dav_error(
                StatusCode::FORBIDDEN,
                ns,
                invalid.precondition(kind),
            ));
        }
    };
    if op == ChangeOp::Create && uid_taken(dav, &located.coll, &valid.uid).await? {
        return Ok(dav_error(StatusCode::FORBIDDEN, ns, "no-uid-conflict"));
    }
    let delivery = match scheduled {
        true => match deliver(dav, caller, located, Some(bytes.clone())).await {
            Ok(delivery) => Some(delivery),
            Err(response) => return Ok(*response),
        },
        false => None,
    };
    let bytes = delivery
        .as_ref()
        .and_then(|d| d.body.clone())
        .unwrap_or(bytes);
    let own = match shared.edits.own_addresses(located.coll.archive).await {
        Ok(own) => own,
        Err(error) => {
            withdraw(dav, caller, delivery.as_ref()).await;
            return Err(error.into());
        }
    };
    let classified = match holtmail_pim_edit::classify(
        kind,
        base.as_ref().map(|(_, b)| b.as_ref()),
        &bytes,
        &own,
    ) {
        Ok(classified) => classified,
        Err(invalid) => {
            withdraw(dav, caller, delivery.as_ref()).await;
            return Ok(dav_error(
                StatusCode::FORBIDDEN,
                ns,
                invalid.precondition(kind),
            ));
        }
    };
    let class = match op {
        ChangeOp::Create => ChangeClass::Create,
        _ => match ChangeClass::parse(classified.class.as_str()) {
            Some(ChangeClass::Create) | None => ChangeClass::Edit,
            Some(class) => class,
        },
    };
    let blob_id = match shared.blobs.put(bytes.clone()).await {
        Ok(blob_id) => blob_id,
        Err(error) => {
            withdraw(dav, caller, delivery.as_ref()).await;
            return Err(error.into());
        }
    };
    let new = NewItemChange {
        target: target.clone(),
        op,
        class,
        content: Some(ChangeContent {
            blob_id,
            content_type: kind.content_type().to_owned(),
        }),
        patch: serde_json::to_value(&classified.patch).ok(),
        base_version: base.as_ref().map(|(n, _)| *n),
        origin: ChangeOrigin::Dav,
        user_id: Some(caller.user.id),
        device: Some(caller.device.to_string()),
        at: Timestamp::now(),
        not_before: None,
        messages: delivery
            .as_ref()
            .map(|d| d.queued.clone())
            .unwrap_or_default(),
        schedule_tag: None,
    };
    let change = match record(dav, &new, expect).await {
        Ok(change) => change,
        Err(response) => {
            withdraw(dav, caller, delivery.as_ref()).await;
            return Ok(*response);
        }
    };
    confirm(dav, caller, delivery.as_ref()).await;
    after_change(dav, caller, located, &change).await;
    let served = match shared.store.effective(&target).await? {
        Some(effective) => shared.resolve(&located.coll, effective, true).await?,
        None => None,
    };
    let etag = served
        .as_ref()
        .filter(|res| res.bytes.as_ref() == Some(&client))
        .map(Res::quoted_etag);
    let schedule_tag = served
        .as_ref()
        .filter(|_| caller.sched.is_some())
        .and_then(Res::quoted_schedule_tag);
    let status = match op {
        ChangeOp::Create => StatusCode::CREATED,
        _ => StatusCode::NO_CONTENT,
    };
    let mut response = status.into_response();
    if let Some(etag) = etag
        && let Ok(value) = etag.parse()
    {
        response.headers_mut().insert(header::ETAG, value);
    }
    if let Some(tag) = schedule_tag
        && let Ok(value) = tag.parse()
    {
        response.headers_mut().insert("schedule-tag", value);
    }
    Ok(response)
}

/// `DELETE`: a held delete of what is served. The archived item and its versions stay.
pub(crate) async fn delete(
    dav: &Dav,
    caller: &Caller,
    target: &Target,
    headers: &HeaderMap,
) -> Response {
    if !matches!(target, Target::Resource(..)) {
        return text(StatusCode::FORBIDDEN, "collections come from accounts");
    }
    let located = match locate(dav, caller, target).await {
        Ok(located) => located,
        Err(response) => return *response,
    };
    let (Some(effective), Some(served)) = (&located.effective, &located.served) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let expect = match preconditions(headers, &located) {
        Ok(Expect::Absent) => return precondition_failed(),
        Ok(expect) => expect,
        Err(response) => return *response,
    };
    let scheduled = scheduled(dav, caller, &located, served.kind);
    let expect = match scheduled {
        true => match schedule_tag_matches(headers, &located, expect) {
            Ok((expect, _)) => expect,
            Err(response) => return *response,
        },
        false => expect,
    };
    let delivery = match scheduled {
        true => match deliver(dav, caller, &located, None).await {
            Ok(delivery) => Some(delivery),
            Err(response) => return *response,
        },
        false => None,
    };
    let Some(stored) = &located.coll.stored else {
        return denied();
    };
    let item: Option<ItemId> = effective.item.as_ref().map(|i| i.item.id);
    let new = NewItemChange {
        target: match item {
            Some(item) => ChangeTarget::Item { item },
            None => ChangeTarget::Href {
                collection: stored.id,
                href: located.href.clone(),
            },
        },
        op: ChangeOp::Delete,
        class: ChangeClass::Delete,
        content: None,
        patch: None,
        base_version: item.and(effective.latest.as_ref().map(|v| v.number)),
        origin: ChangeOrigin::Dav,
        user_id: Some(caller.user.id),
        device: Some(caller.device.to_string()),
        at: Timestamp::now(),
        not_before: None,
        messages: delivery
            .as_ref()
            .map(|d| d.queued.clone())
            .unwrap_or_default(),
        schedule_tag: None,
    };
    match record(dav, &new, expect).await {
        Ok(change) => {
            confirm(dav, caller, delivery.as_ref()).await;
            after_change(dav, caller, &located, &change).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(response) => {
            withdraw(dav, caller, delivery.as_ref()).await;
            *response
        }
    }
}

/// `PROPPATCH` of a collection's display properties, kept for the signed-in user and never
/// sent upstream. All or nothing: one property that cannot be set fails the others with 424.
pub(crate) async fn proppatch(dav: &Dav, caller: &Caller, target: &Target, body: Body) -> Response {
    let Target::Collection(home, _, key) = target else {
        return text(
            StatusCode::FORBIDDEN,
            "only collections have display properties",
        );
    };
    let Some(bytes) = body_bytes(body, MAX_XML).await else {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
    };
    let Some(root) = xml::parse(&bytes).filter(|r| r.is(DAV, "propertyupdate")) else {
        return text(StatusCode::BAD_REQUEST, "expected propertyupdate");
    };
    let shared = dav.shared();
    let run = async {
        let Some(coll) = shared.collection(caller, *home, key).await? else {
            return Ok(text(StatusCode::NOT_FOUND, "not found"));
        };
        let mut updates: Vec<(bool, &Element)> = Vec::new();
        for op in &root.children {
            let set = op.is(DAV, "set");
            if !set && !op.is(DAV, "remove") {
                continue;
            }
            for prop in op.children(DAV, "prop").flat_map(|p| p.children.iter()) {
                updates.push((set, prop));
            }
        }
        let allowed = |p: &Element| {
            DISPLAY.iter().any(|(ns, name)| p.is(ns, name)) && p.text.len() <= MAX_DISPLAY
        };
        let ok = updates.iter().all(|(_, p)| allowed(p));
        let mut good = String::new();
        let mut bad = String::new();
        for (_, prop) in &updates {
            let element = xml::element(&prop.ns, &prop.name, "");
            if allowed(prop) {
                good.push_str(&element);
            } else {
                bad.push_str(&element);
            }
        }
        if ok {
            let mut props: BTreeMap<String, String> =
                shared.display_props(caller, &coll.key).await?;
            for (set, prop) in &updates {
                let k = props::key(&prop.ns, &prop.name);
                match set {
                    true => props.insert(k, prop.text.trim().to_owned()),
                    false => props.remove(&k),
                };
            }
            shared.set_display_props(caller, &coll.key, &props).await?;
        }
        let href = crate::path::collection(*home, caller.user.id, &coll.key);
        let mut out = format!(
            "{}<d:response><d:href>{}</d:href>",
            xml::open("multistatus"),
            xml::escape(&href)
        );
        let good_status = if ok {
            "200 OK"
        } else {
            "424 Failed Dependency"
        };
        if !good.is_empty() {
            out.push_str(&format!(
                "<d:propstat><d:prop>{good}</d:prop><d:status>HTTP/1.1 {good_status}</d:status></d:propstat>"
            ));
        }
        if !bad.is_empty() {
            out.push_str(&format!(
                "<d:propstat><d:prop>{bad}</d:prop><d:status>HTTP/1.1 403 Forbidden</d:status></d:propstat>"
            ));
        }
        out.push_str("</d:response></d:multistatus>");
        R::Ok(xml_response(StatusCode::MULTI_STATUS, out))
    };
    run.await.unwrap_or_else(|Unavailable| unavailable())
}
