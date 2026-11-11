use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_pim::ItemKind;

use crate::model::{Caller, DavCollection, R, Res, Unavailable};
use crate::path::{self, Home, Target};
use crate::props::{self, CollectionInfo, Node, Wanted};
use crate::xml::{self, CALDAV, CARDDAV, DAV, Element};
use crate::{Dav, body_bytes, dav_error, text, unavailable, xml_response};

/// Largest XML request body read.
pub(crate) const MAX_XML: usize = 1 << 20;

enum Depth {
    Zero,
    One,
}

fn depth(headers: &HeaderMap) -> Option<Depth> {
    match headers
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        None | Some("0") => Some(Depth::Zero),
        Some("1") => Some(Depth::One),
        _ => None,
    }
}

async fn read_xml(body: Body) -> Result<Option<Element>, Box<Response>> {
    let Some(bytes) = body_bytes(body, MAX_XML).await else {
        return Err(Box::new(text(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request too large",
        )));
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    match xml::parse(&bytes) {
        Some(root) => Ok(Some(root)),
        None => Err(Box::new(text(StatusCode::BAD_REQUEST, "malformed XML"))),
    }
}

fn multistatus(responses: String, tail: &str) -> Response {
    xml_response(
        StatusCode::MULTI_STATUS,
        format!(
            "{}{responses}{tail}</d:multistatus>",
            xml::open("multistatus")
        ),
    )
}

pub(crate) async fn info(dav: &Dav, caller: &Caller, coll: &DavCollection) -> R<CollectionInfo> {
    let shared = dav.shared();
    let token = shared.token(caller, coll).await?;
    let ctag = token.rsplit(':').next().unwrap_or_default().to_owned();
    Ok(CollectionInfo {
        token,
        ctag,
        writable: shared.writable(caller, coll).await?.advertised(),
        props: shared.display_props(caller, &coll.key).await?,
        components: shared.components(coll).await?,
    })
}

async fn collection_of(
    dav: &Dav,
    caller: &Caller,
    home: Home,
    key: &path::CollectionKey,
) -> Result<DavCollection, Box<Response>> {
    match dav.shared().collection(caller, home, key).await {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(Box::new(text(StatusCode::NOT_FOUND, "not found"))),
        Err(Unavailable) => Err(Box::new(unavailable())),
    }
}

fn resource_response(
    caller: &Caller,
    coll: &DavCollection,
    res: &Res,
    writable: bool,
    wanted: &Wanted,
) -> String {
    let target = path::resource(coll.home, caller.user.id, &coll.key, &res.href);
    props::response(caller, &target, &Node::Resource(res, writable), wanted)
}

pub(crate) async fn propfind(
    dav: &Dav,
    caller: &Caller,
    target: &Target,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let Some(depth) = depth(headers) else {
        return dav_error(StatusCode::FORBIDDEN, DAV, "propfind-finite-depth");
    };
    let wanted = match read_xml(body).await {
        Err(response) => return *response,
        Ok(None) => Wanted::All,
        Ok(Some(root)) if root.is(DAV, "propfind") => Wanted::of(&root),
        Ok(Some(_)) => return text(StatusCode::BAD_REQUEST, "expected propfind"),
    };
    match propfind_inner(dav, caller, target, depth, &wanted).await {
        Ok(response) => response,
        Err(Unavailable) => unavailable(),
    }
}

async fn propfind_inner(
    dav: &Dav,
    caller: &Caller,
    target: &Target,
    depth: Depth,
    wanted: &Wanted,
) -> R<Response> {
    let user = caller.user.id;
    let shared = dav.shared();
    let mut out = String::new();
    match target {
        Target::Root => {
            let here = path::BASE.to_owned() + "/";
            out.push_str(&props::response(caller, &here, &Node::Root, wanted));
        }
        Target::Principals => {
            out.push_str(&props::response(
                caller,
                &path::principals(),
                &Node::Principals,
                wanted,
            ));
            if matches!(depth, Depth::One) {
                out.push_str(&props::response(
                    caller,
                    &path::principal(user),
                    &Node::Principal,
                    wanted,
                ));
            }
        }
        Target::Principal(_) => out.push_str(&props::response(
            caller,
            &path::principal(user),
            &Node::Principal,
            wanted,
        )),
        Target::Home(home, _) => {
            out.push_str(&props::response(
                caller,
                &path::home(*home, user),
                &Node::Home(*home),
                wanted,
            ));
            if matches!(depth, Depth::One) {
                for coll in shared.collections(caller, *home).await? {
                    let info = info(dav, caller, &coll).await?;
                    out.push_str(&props::response(
                        caller,
                        &path::collection(*home, user, &coll.key),
                        &Node::Collection(&coll, &info),
                        wanted,
                    ));
                }
            }
        }
        Target::Collection(home, _, key) => {
            let Some(coll) = shared.collection(caller, *home, key).await? else {
                return Ok(text(StatusCode::NOT_FOUND, "not found"));
            };
            let info = info(dav, caller, &coll).await?;
            out.push_str(&props::response(
                caller,
                &path::collection(*home, user, key),
                &Node::Collection(&coll, &info),
                wanted,
            ));
            if matches!(depth, Depth::One) {
                for res in shared.members(caller, &coll, wanted.needs_bytes()).await? {
                    out.push_str(&resource_response(
                        caller,
                        &coll,
                        &res,
                        info.writable,
                        wanted,
                    ));
                }
            }
        }
        Target::Resource(home, _, key, href) => {
            let Some(coll) = shared.collection(caller, *home, key).await? else {
                return Ok(text(StatusCode::NOT_FOUND, "not found"));
            };
            let Some(res) = shared
                .resource(caller, &coll, href, wanted.needs_bytes())
                .await?
            else {
                return Ok(text(StatusCode::NOT_FOUND, "not found"));
            };
            let writable = shared.writable(caller, &coll).await?.advertised();
            out.push_str(&resource_response(caller, &coll, &res, writable, wanted));
        }
        Target::Inbox(_) | Target::Outbox(_) | Target::InboxItem(..) => {
            return Ok(text(StatusCode::NOT_FOUND, "not found"));
        }
    }
    Ok(multistatus(out, ""))
}

/// `PROPFIND` on the schedule inbox, the outbox or an inbox resource.
pub(crate) async fn propfind_box(
    dav: &Dav,
    caller: &Caller,
    which: crate::sched::Box_,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let Some(depth) = depth(headers) else {
        return dav_error(StatusCode::FORBIDDEN, DAV, "propfind-finite-depth");
    };
    let wanted = match read_xml(body).await {
        Err(response) => return *response,
        Ok(None) => Wanted::All,
        Ok(Some(root)) if root.is(DAV, "propfind") => Wanted::of(&root),
        Ok(Some(_)) => return text(StatusCode::BAD_REQUEST, "expected propfind"),
    };
    crate::sched::propfind(dav, caller, which, matches!(depth, Depth::One), &wanted)
        .await
        .unwrap_or_else(|Unavailable| unavailable())
}

/// `REPORT` on the schedule inbox: `calendar-multiget` only.
pub(crate) async fn report_inbox(dav: &Dav, caller: &Caller, body: Body) -> Response {
    match read_xml(body).await {
        Err(response) => *response,
        Ok(Some(root)) if root.is(CALDAV, "calendar-multiget") => {
            crate::sched::multiget(dav, caller, &root).await
        }
        Ok(_) => dav_error(StatusCode::FORBIDDEN, DAV, "supported-report"),
    }
}

pub(crate) async fn get(
    dav: &Dav,
    caller: &Caller,
    target: &Target,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let Target::Resource(home, _, key, href) = target else {
        return text(StatusCode::METHOD_NOT_ALLOWED, "not a resource");
    };
    let coll = match collection_of(dav, caller, *home, key).await {
        Ok(c) => c,
        Err(response) => return *response,
    };
    let res = match dav.shared().resource(caller, &coll, href, true).await {
        Ok(Some(res)) => res,
        Ok(None) => return text(StatusCode::NOT_FOUND, "not found"),
        Err(Unavailable) => return unavailable(),
    };
    let Some(mut raw) = res.bytes.clone() else {
        return unavailable();
    };
    let mut etag = res.quoted_etag();
    let wants_v3 = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| {
            a.to_ascii_lowercase()
                .replace(' ', "")
                .contains("version=3.0")
        });
    if res.kind == ItemKind::Contact
        && wants_v3
        && let Some(v3) = props::vcard3(&raw)
    {
        if v3 != raw {
            etag = format!("\"{}\"", BlobId::of(&v3));
        }
        raw = v3;
    }
    let length = raw.len();
    let body = if head { Body::empty() } else { Body::from(raw) };
    let mut response = (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                format!("{}; charset=utf-8", res.kind.content_type()),
            ),
            (header::ETAG, etag),
        ],
        body,
    )
        .into_response();
    if head && let Ok(value) = length.to_string().parse() {
        response.headers_mut().insert(header::CONTENT_LENGTH, value);
    }
    if caller.sched.is_some()
        && let Some(tag) = res.quoted_schedule_tag()
        && let Ok(value) = tag.parse()
    {
        response.headers_mut().insert("schedule-tag", value);
    }
    response
}

pub(crate) async fn report(
    dav: &Dav,
    caller: &Caller,
    target: &Target,
    _headers: &HeaderMap,
    body: Body,
) -> Response {
    let root = match read_xml(body).await {
        Err(response) => return *response,
        Ok(None) => return text(StatusCode::BAD_REQUEST, "a report needs a body"),
        Ok(Some(root)) => root,
    };
    let result = if root.is(DAV, "principal-property-search") {
        Ok(principal_search(caller, &root))
    } else if root.is(DAV, "sync-collection") {
        on_collection(dav, caller, target, |coll| sync(dav, caller, coll, &root)).await
    } else if root.is(CALDAV, "calendar-multiget") || root.is(CARDDAV, "addressbook-multiget") {
        on_collection(dav, caller, target, |coll| {
            multiget(dav, caller, coll, &root)
        })
        .await
    } else if root.is(CALDAV, "calendar-query") || root.is(CARDDAV, "addressbook-query") {
        on_collection(dav, caller, target, |coll| query(dav, caller, coll, &root)).await
    } else if root.is(CALDAV, "free-busy-query") {
        on_collection(dav, caller, target, |coll| {
            free_busy(dav, caller, coll, &root)
        })
        .await
    } else {
        return dav_error(StatusCode::FORBIDDEN, DAV, "supported-report");
    };
    result.unwrap_or_else(|Unavailable| unavailable())
}

async fn on_collection<'a, F, Fut>(
    dav: &'a Dav,
    caller: &'a Caller,
    target: &'a Target,
    run: F,
) -> R<Response>
where
    F: FnOnce(DavCollection) -> Fut,
    Fut: Future<Output = R<Response>>,
{
    let (home, key) = match target {
        Target::Collection(home, _, key) | Target::Resource(home, _, key, _) => (*home, key),
        _ => return Ok(dav_error(StatusCode::FORBIDDEN, DAV, "supported-report")),
    };
    match collection_of(dav, caller, home, key).await {
        Ok(coll) => run(coll).await,
        Err(response) => Ok(*response),
    }
}

async fn sync(dav: &Dav, caller: &Caller, coll: DavCollection, root: &Element) -> R<Response> {
    let wanted = Wanted::of(root);
    let token = root
        .child(DAV, "sync-token")
        .map(|t| t.trimmed().to_owned());
    let shared = dav.shared();
    let Some(synced) = shared
        .sync(caller, &coll, token.as_deref(), wanted.needs_bytes())
        .await?
    else {
        return Ok(dav_error(StatusCode::FORBIDDEN, DAV, "valid-sync-token"));
    };
    let writable = shared.writable(caller, &coll).await?.advertised();
    let mut out = String::new();
    for res in &synced.present {
        out.push_str(&resource_response(caller, &coll, res, writable, &wanted));
    }
    for href in &synced.removed {
        let target = path::resource(coll.home, caller.user.id, &coll.key, href);
        out.push_str(&props::status_response(&target, "404 Not Found"));
    }
    if synced.truncated {
        let here = path::collection(coll.home, caller.user.id, &coll.key);
        out.push_str(&props::status_response(&here, "507 Insufficient Storage"));
    }
    let tail = format!(
        "<d:sync-token>{}</d:sync-token>",
        xml::escape(&synced.token)
    );
    Ok(multistatus(out, &tail))
}

async fn multiget(dav: &Dav, caller: &Caller, coll: DavCollection, root: &Element) -> R<Response> {
    let wanted = Wanted::of(root);
    let shared = dav.shared();
    let writable = shared.writable(caller, &coll).await?.advertised();
    let mut out = String::new();
    for href in root.children(DAV, "href") {
        let raw = path::href_path(&href.text);
        let found = match path::parse(raw) {
            Some(Target::Resource(home, user, key, name))
                if home == coll.home && user == caller.user.id && key == coll.key =>
            {
                shared.resource(caller, &coll, &name, true).await?
            }
            _ => None,
        };
        match found {
            Some(res) => out.push_str(&resource_response(caller, &coll, &res, writable, &wanted)),
            None => out.push_str(&props::status_response(raw, "404 Not Found")),
        }
    }
    Ok(multistatus(out, ""))
}

async fn query(dav: &Dav, caller: &Caller, coll: DavCollection, root: &Element) -> R<Response> {
    let wanted = Wanted::of(root);
    let shared = dav.shared();
    let writable = shared.writable(caller, &coll).await?.advertised();
    let calendar = root.ns == CALDAV;
    let ns = if calendar { CALDAV } else { CARDDAV };
    let filter = root.child(ns, "filter");
    let limit = root
        .child(CARDDAV, "limit")
        .and_then(|l| l.child(CARDDAV, "nresults"))
        .and_then(|n| n.trimmed().parse::<usize>().ok());
    let mut out = String::new();
    let mut count = 0;
    let mut cut = false;
    for res in shared.members(caller, &coll, true).await? {
        let Some(raw) = &res.bytes else { continue };
        let hit = match calendar {
            true => crate::filter::calendar(filter, res.kind, raw),
            false => crate::filter::contact(filter, raw),
        };
        if !hit {
            continue;
        }
        if limit.is_some_and(|l| count >= l) {
            cut = true;
            break;
        }
        count += 1;
        out.push_str(&resource_response(caller, &coll, &res, writable, &wanted));
    }
    if cut {
        let here = path::collection(coll.home, caller.user.id, &coll.key);
        out.push_str(&props::status_response(&here, "507 Insufficient Storage"));
    }
    Ok(multistatus(out, ""))
}

async fn free_busy(dav: &Dav, caller: &Caller, coll: DavCollection, root: &Element) -> R<Response> {
    let Some(window) = root
        .child(CALDAV, "time-range")
        .and_then(crate::filter::window)
    else {
        return Ok(text(
            StatusCode::BAD_REQUEST,
            "free-busy-query needs a time-range",
        ));
    };
    if coll.home != Home::Calendars {
        return Ok(dav_error(StatusCode::FORBIDDEN, DAV, "supported-report"));
    }
    let mut periods: Vec<(i64, i64)> = Vec::new();
    for res in dav.shared().members(caller, &coll, true).await? {
        if let Some(raw) = &res.bytes {
            periods.extend(crate::filter::busy(res.kind, raw, window));
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
    let fmt = crate::filter::format_utc;
    let mut body = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//DAV//EN\r\nBEGIN:VFREEBUSY\r\n",
    );
    body.push_str(&format!(
        "DTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\n",
        fmt(holtmail_core::Timestamp::now().as_millis()),
        fmt(window.from),
        fmt(window.to)
    ));
    for (start, end) in merged {
        body.push_str(&format!("FREEBUSY:{}/{}\r\n", fmt(start), fmt(end)));
    }
    body.push_str("END:VFREEBUSY\r\nEND:VCALENDAR\r\n");
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/calendar; charset=utf-8")],
        Bytes::from(body),
    )
        .into_response())
}

/// Only the caller's own principal exists: it is listed when the search matches its name or
/// address.
fn principal_search(caller: &Caller, root: &Element) -> Response {
    let haystack = format!("{}\n{}", caller.user.name, caller.user.email).to_lowercase();
    let searches: Vec<bool> = root
        .children(DAV, "property-search")
        .map(|s| {
            s.child(DAV, "match")
                .is_some_and(|m| haystack.contains(&m.trimmed().to_lowercase()))
        })
        .collect();
    let hit = match root.attr("test") {
        Some("anyof") => searches.iter().any(|h| *h),
        _ => !searches.is_empty() && searches.iter().all(|h| *h),
    };
    let wanted = match root.child(DAV, "prop") {
        Some(prop) => Wanted::Props(prop.children.clone()),
        None => Wanted::All,
    };
    let out = match hit {
        true => props::response(
            caller,
            &path::principal(caller.user.id),
            &Node::Principal,
            &wanted,
        ),
        false => String::new(),
    };
    multistatus(out, "")
}
