//! Contacts and calendars written through to real servers: against Radicale and Stalwart
//! (CardDAV and CalDAV) the binary creates, edits and deletes an event and a contact through
//! the API with the collections in `write_through` mode, and the suite reads the server itself
//! to see each change land. An edit made in `held` mode never leaves; when the event is moved
//! upstream meanwhile to another time, the next sync turns the held edit into a conflict, and
//! "keep mine" overwrites upstream once the calendar writes through again.

use std::time::{Duration, Instant};

use holtmail_integration::data::{icalendar_event, vcard};
use holtmail_integration::dav::{Dav, DavServer, RADICALE, STALWART_DAV, hrefs_in};
use holtmail_integration::pim::PimRun;
use holtmail_integration::{require_it, unique};
use reqwest::Method;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(60);

fn at(day: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(day)
        .expect("a time")
        .with_timezone(&chrono::Utc)
}

async fn put(run: &PimRun, path: &str, body: Value) -> Value {
    run.api.request(Method::PUT, path, Some(&body)).await.ok()
}

/// Waits until the collection has no open change left, failing on a conflict or refusal.
async fn settled(run: &PimRun, collection: &str) {
    let started = Instant::now();
    loop {
        let open = run
            .api
            .get(&format!("/api/pim/changes?collection={collection}"))
            .await
            .ok();
        let open = open.as_array().cloned().unwrap_or_default();
        if open.is_empty() {
            return;
        }
        if let Some(stuck) = open
            .iter()
            .find(|c| c["state"] == "conflict" || c["state"] == "rejected")
        {
            panic!("a change did not go through: {stuck}");
        }
        assert!(
            started.elapsed() < WAIT,
            "changes still open after {WAIT:?}: {open:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Every item of the collection at `url` as the server holds it: address and text.
async fn upstream(dav: &Dav, url: &str) -> Vec<(String, String)> {
    let listing = dav.propfind(url, "1", "<d:getetag/>").await;
    let base = reqwest::Url::parse(url).expect("an address");
    let mut out = Vec::new();
    for href in hrefs_in(&listing, "multistatus") {
        let address = base.join(&href).expect("an href").to_string();
        if address.trim_end_matches('/') == url.trim_end_matches('/') {
            continue;
        }
        let (status, text) = dav.get(&address).await;
        if status.is_success() {
            out.push((address, text));
        }
    }
    out
}

async fn upstream_text(dav: &Dav, url: &str) -> String {
    let (status, text) = dav.get(url).await;
    assert!(status.is_success(), "GET {url}: {status}");
    text
}

fn id_of(map: &std::collections::BTreeMap<String, Value>, name: &str) -> String {
    map.get(name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", map.keys()))["id"]
        .as_str()
        .expect("an id")
        .to_owned()
}

async fn writes_through(server: DavServer) {
    let user = server.user("wb").await;
    let dav = server.client(&user);
    let homes = server.homes(&dav).await;
    let book = dav
        .create_address_book(&homes.contacts, "people", "People")
        .await;
    let plans = dav
        .create_calendar(&homes.calendars, "plans", "Plans")
        .await;
    let ada_uid = unique("ada");
    let ada = dav
        .put_card(
            &book,
            &ada_uid,
            &vcard(&ada_uid, "Ada Example", "ada.example@example.org", None),
        )
        .await;
    let planning_uid = unique("planning");
    let planning = dav
        .put_event(
            &plans,
            &planning_uid,
            &icalendar_event(
                &planning_uid,
                "Planning meeting",
                at("2026-12-01T09:00:00Z"),
                at("2026-12-01T10:00:00Z"),
                None,
            ),
        )
        .await;

    let mut run = PimRun::start(&format!("Writeback on {}", server.name)).await;
    run.secrets(
        "an item",
        &[
            "Ada Example",
            "ada.example@example.org",
            "Bea Example",
            "bea.example@example.org",
            "Planning meeting",
            "Planning review",
            "Created in Holtmail",
        ],
    );
    let account = run.add_dav(&server, &user, None).await;
    run.api
        .request(
            Method::PUT,
            &format!("/api/accounts/{account}/capabilities"),
            Some(&json!({ "capabilities": ["pim.edit"] })),
        )
        .await
        .ok();
    let collections = run.collections().await;
    let book_id = id_of(&collections, "People");
    let plans_id = id_of(&collections, "Plans");
    for collection in [&book_id, &plans_id] {
        let mode = put(
            &run,
            &format!("/api/collections/{collection}/write-mode"),
            json!({ "mode": "write_through", "confirm": true }),
        )
        .await;
        assert_eq!(mode["mode"], "write_through", "{mode}");
    }

    let created = run
        .api
        .post(
            &format!("/api/collections/{plans_id}/items"),
            json!({ "event": { "title": "Created in Holtmail", "start": "2026-12-03T10:00:00", "end": "2026-12-03T11:00:00", "time_zone": "UTC" } }),
        )
        .await
        .ok();
    assert_eq!(created["state"], "held", "{created}");
    run.api
        .post(
            &format!("/api/collections/{book_id}/items"),
            json!({ "contact": { "full_name": "Bea Example", "emails": [{ "value": "bea.example@example.org" }] } }),
        )
        .await
        .ok();
    settled(&run, &plans_id).await;
    settled(&run, &book_id).await;
    let events = upstream(&dav, &plans).await;
    assert!(
        events
            .iter()
            .any(|(_, t)| t.contains("SUMMARY:Created in Holtmail")),
        "the new event is upstream: {events:?}"
    );
    let cards = upstream(&dav, &book).await;
    assert!(
        cards.iter().any(|(_, t)| t.contains("FN:Bea Example")),
        "the new contact is upstream: {cards:?}"
    );

    let event = id_of(&run.items("events").await, "Planning meeting");
    let edit = run.api.get(&format!("/api/items/{event}/edit")).await.ok();
    assert_eq!(edit["writable"], true, "{edit}");
    let mut form = edit["event"].clone();
    form["title"] = json!("Planning review");
    put(
        &run,
        &format!("/api/items/{event}/content"),
        json!({ "event": form, "expect": edit["state"] }),
    )
    .await;
    settled(&run, &plans_id).await;
    let text = upstream_text(&dav, &planning).await;
    assert!(text.contains("SUMMARY:Planning review"), "{text}");
    let detail = run.api.get(&format!("/api/items/{event}")).await.ok();
    assert!(
        detail["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["origin"] == "writeback"),
        "a writeback version: {detail}"
    );

    let contact = id_of(&run.items("contacts").await, "Ada Example");
    let deleted = run.api.delete(&format!("/api/items/{contact}")).await.ok();
    assert_eq!(deleted["op"], "delete", "{deleted}");
    settled(&run, &book_id).await;
    let (status, _) = dav.get(&ada).await;
    assert_eq!(status.as_u16(), 404, "the contact is gone upstream");

    put(
        &run,
        &format!("/api/collections/{plans_id}/write-mode"),
        json!({ "mode": "held" }),
    )
    .await;
    let edit = run.api.get(&format!("/api/items/{event}/edit")).await.ok();
    let mut form = edit["event"].clone();
    form["start"] = json!("2026-12-01T15:00:00");
    form["end"] = json!("2026-12-01T16:00:00");
    let held = put(
        &run,
        &format!("/api/items/{event}/content"),
        json!({ "event": form, "expect": edit["state"] }),
    )
    .await;
    assert_eq!(held["state"], "held", "{held}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let text = upstream_text(&dav, &planning).await;
    assert!(
        text.contains("DTSTART:20261201T090000Z"),
        "a held edit left: {text}"
    );

    let moved = text
        .replace("DTSTART:20261201T090000Z", "DTSTART:20261201T120000Z")
        .replace("DTEND:20261201T100000Z", "DTEND:20261201T130000Z");
    dav.put(&planning, "text/calendar; charset=utf-8", &moved)
        .await;
    run.sync(&account).await;
    let conflicts = run
        .api
        .get(&format!(
            "/api/pim/changes?state=conflict&collection={plans_id}"
        ))
        .await
        .ok();
    let conflict = conflicts
        .as_array()
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or_else(|| panic!("no conflict: {conflicts}"));
    assert_eq!(conflict["reason"], "overlap", "{conflict}");
    let detail = run.api.get(&format!("/api/items/{event}")).await.ok();
    assert_eq!(
        detail["item"]["local_edit"]["state"], "conflict",
        "{detail}"
    );

    let kept = run
        .api
        .post(
            &format!(
                "/api/pim/changes/{}/resolve",
                conflict["id"].as_str().unwrap()
            ),
            json!({ "action": "keep_mine" }),
        )
        .await
        .ok();
    assert_eq!(kept["state"], "held", "{kept}");
    put(
        &run,
        &format!("/api/collections/{plans_id}/write-mode"),
        json!({ "mode": "write_through", "confirm": true }),
    )
    .await;
    settled(&run, &plans_id).await;
    let text = upstream_text(&dav, &planning).await;
    assert!(
        text.contains("DTSTART:20261201T150000Z"),
        "keep mine overwrote upstream: {text}"
    );

    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_through_to_radicale() {
    require_it!();
    writes_through(RADICALE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_through_to_stalwart() {
    require_it!();
    writes_through(STALWART_DAV).await;
}
