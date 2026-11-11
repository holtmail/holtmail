use holtmail_core::ArchiveId;
use holtmail_store::{Capabilities, Capability, CollectionKind, NewCollection};

use super::*;

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:standup@example.com\r\nDTSTAMP:20260101T090000Z\r\nSUMMARY:Standup\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T093000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

pub(super) struct Pim {
    book: String,
    pub(super) calendar: String,
    event: String,
}

pub(super) async fn pim(f: &Fixture) -> Pim {
    let archive: ArchiveId = f.archive.parse().unwrap();
    let account = server_account(f).await;
    let collection = |kind, remote: &str| NewCollection {
        archive_id: archive,
        account_id: Some(account),
        kind,
        remote_id: remote.into(),
        name: remote.into(),
        color: None,
    };
    let book = f
        .store
        .upsert_collection(collection(CollectionKind::Contacts, "/books/main/"))
        .await
        .unwrap();
    let calendar = f
        .store
        .upsert_collection(collection(CollectionKind::Calendar, "/cals/work/"))
        .await
        .unwrap();
    let event = f
        .ingest
        .ingest_item(holtmail_ingest::IncomingItem {
            collection: calendar.id,
            remote_id: "/cals/work/standup.ics".into(),
            kind: holtmail_pim::ItemKind::Event,
            format: holtmail_pim::Format::ICalendar,
            raw: Bytes::from_static(EVENT.as_bytes()),
            etag: Some("\"e1\"".into()),
            seen_at: holtmail_core::Timestamp::from_secs(1_700_000_000),
        })
        .await
        .unwrap();
    Pim {
        book: book.id.to_string(),
        calendar: calendar.id.to_string(),
        event: event.item_id.to_string(),
    }
}

pub(super) async fn server_account(f: &Fixture) -> holtmail_core::AccountId {
    let archive: ArchiveId = f.archive.parse().unwrap();
    if let Some(account) = f
        .store
        .list_accounts(archive)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.kind == holtmail_store::AccountKind::Imap)
    {
        return account.id;
    }
    f.store
        .create_account(holtmail_store::NewAccount {
            archive_id: archive,
            kind: holtmail_store::AccountKind::Imap,
            name: "ada@example.com".into(),
            address: Some("ada@example.com".into()),
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id
}

impl Fixture {
    pub(super) async fn allow_pim(&self) {
        let account = server_account(self).await;
        let caps: Capabilities = [Capability::PimEdit].into_iter().collect();
        self.store
            .set_account_capabilities(account, &caps)
            .await
            .unwrap();
    }

    async fn write(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, _, body) = self.send(request(method, uri, body)).await;
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_contacts_and_events_as_held_changes() {
    let f = fixture().await;
    let p = pim(&f).await;
    let create = format!("/api/collections/{}/items", p.book);
    let contact = json!({ "contact": { "full_name": "Bea Example", "emails": [{ "value": "bea@example.org" }] } });
    let (status, _) = f.write("POST", &create, contact.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pim.edit is off");

    f.allow_pim().await;
    let (status, change) = f.write("POST", &create, contact).await;
    assert_eq!(status, StatusCode::CREATED, "{change}");
    assert_eq!(change["op"], "create");
    assert_eq!(change["class"], "create");
    assert_eq!(change["state"], "held");
    assert!(change["href"].as_str().unwrap().ends_with(".vcf"));
    let (status, _) = f
        .write(
            "POST",
            &create,
            json!({ "event": { "title": "Wrong", "start": "2026-01-05" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = f
        .write(
            "POST",
            &create,
            json!({ "raw": "BEGIN:VCARD\r\nEND:VCARD\r\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, clash) = f
        .write(
            "POST",
            &format!("/api/collections/{}/items", p.calendar),
            json!({ "raw": EVENT }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{clash}");
    assert!(clash.to_string().contains("no-uid-conflict"));

    let edit = f.json(&format!("/api/items/{}/edit", p.event)).await;
    assert_eq!(edit["event"]["title"], "Standup");
    assert_eq!(edit["writable"], true);
    let mut form = edit["event"].clone();
    form["title"] = json!("Daily standup");
    let content = format!("/api/items/{}/content", p.event);
    let (status, change) = f
        .write(
            "PUT",
            &content,
            json!({ "event": form, "expect": edit["state"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{change}");
    assert_eq!(change["class"], "edit");
    let (status, _) = f
        .write(
            "PUT",
            &content,
            json!({ "event": form, "expect": edit["state"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "a stale expect");

    let detail = f.json(&format!("/api/items/{}", p.event)).await;
    assert_eq!(detail["item"]["local_edit"]["state"], "held");
    assert_eq!(detail["item"]["local_edit"]["write_mode"], "held");
    assert_eq!(detail["item"]["title"], "Daily standup");
    let kinds: Vec<&str> = detail["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["version", "change"]);
    let events = f.json(&format!("/api/archives/{}/events", f.archive)).await;
    assert_eq!(events["items"][0]["local_edit"]["state"], "held");

    let changes = f.json("/api/pim/changes?state=held").await;
    assert_eq!(changes.as_array().unwrap().len(), 2);
    let (status, _) = f
        .write("GET", "/api/pim/changes?state=applied", json!({}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mode = format!("/api/collections/{}/write-mode", p.calendar);
    let shown = f.json(&mode).await;
    assert_eq!(
        (shown["mode"].clone(), shown["open_changes"].clone()),
        (json!("held"), json!(1))
    );
    assert_eq!(
        shown["allowed_modes"],
        json!(["read_only", "held", "write_through"])
    );
    let (status, _) = f
        .write("PUT", &mode, json!({ "mode": "write_through" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "open changes need confirm");
    let (status, shown) = f
        .write(
            "PUT",
            &mode,
            json!({ "mode": "write_through", "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["mode"], "write_through");
    let (status, _) = f.write("PUT", &mode, json!({ "mode": "held" })).await;
    assert_eq!(status, StatusCode::OK);

    let id = change["id"].as_str().unwrap();
    let resolve = format!("/api/pim/changes/{id}/resolve");
    let (status, _) = f
        .write("POST", &resolve, json!({ "action": "retry" }))
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "only a refused change is retried"
    );
    let (status, decided) = f
        .write("POST", &resolve, json!({ "action": "discard" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{decided}");
    assert_eq!(decided["state"], "discarded");
    let detail = f.json(&format!("/api/items/{}", p.event)).await;
    assert!(detail["item"]["local_edit"].is_null());
    assert_eq!(
        detail["item"]["title"], "Standup",
        "derived data is upstream's again"
    );

    let (status, deleted) = f
        .write("DELETE", &format!("/api/items/{}", p.event), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["op"], "delete");

    let audit = f.json(&format!("/api/audit?archive={}", f.archive)).await;
    let kinds: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "pim_edit").count(),
        4,
        "{kinds:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn makes_contacts_from_senders_and_routes_people_to_their_card() {
    let f = fixture().await;
    let p = pim(&f).await;
    f.allow_pim().await;
    let page = f
        .json(&format!(
            "/api/archives/{}/messages?folder={}",
            f.archive, f.folder
        ))
        .await;
    let id: holtmail_core::MessageId = page["items"][0]["id"].as_str().unwrap().parse().unwrap();
    let message = f.store.get_message(id).await.unwrap().unwrap();
    let (status, change) = f
        .write(
            "POST",
            &format!("/api/collections/{}/items", p.book),
            json!({ "from_message": message.id.to_string() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{change}");
    let raw = f
        .blobs
        .get(
            &f.store
                .get_change(change["id"].as_str().unwrap().parse().unwrap())
                .await
                .unwrap()
                .unwrap()
                .content
                .unwrap()
                .blob_id,
        )
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&raw).to_lowercase();
    assert!(
        text.contains(&message.from[0].email.to_lowercase()),
        "{text}"
    );

    let (status, _) = f
        .write(
            "POST",
            &format!("/api/collections/{}/items", p.calendar),
            json!({ "from_message": message.id.to_string() }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a calendar holds events");

    let archive: ArchiveId = f.archive.parse().unwrap();
    let book: holtmail_core::CollectionId = p.book.parse().unwrap();
    let card = f
        .ingest
        .ingest_item(holtmail_ingest::IncomingItem {
            collection: book,
            remote_id: "/books/main/ada.vcf".into(),
            kind: holtmail_pim::ItemKind::Contact,
            format: holtmail_pim::Format::VCard,
            raw: Bytes::from_static(CARD_V2.as_bytes()),
            etag: None,
            seen_at: holtmail_core::Timestamp::from_secs(1_700_000_000),
        })
        .await
        .unwrap();
    let read_only = f
        .store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: None,
            kind: CollectionKind::Contacts,
            remote_id: "people.vcf".into(),
            name: "Imported".into(),
            color: None,
        })
        .await
        .unwrap();
    f.ingest
        .ingest_item(holtmail_ingest::IncomingItem {
            collection: read_only.id,
            remote_id: "ada".into(),
            kind: holtmail_pim::ItemKind::Contact,
            format: holtmail_pim::Format::VCard,
            raw: Bytes::from_static(CARD_V2.as_bytes()),
            etag: None,
            seen_at: holtmail_core::Timestamp::from_secs(1_700_000_000),
        })
        .await
        .unwrap();
    holtmail_people::rebuild(&*f.store, archive).await.unwrap();
    let people = f.json(&format!("/api/archives/{}/people", f.archive)).await;
    let person = people["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["cards"] == 2)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let detail = f.json(&format!("/api/people/{person}")).await;
    assert_eq!(detail["primary_card"], card.item_id.to_string());
    let (status, change) = f
        .write(
            "PUT",
            &format!("/api/people/{person}/card"),
            json!({ "contact": { "full_name": "Ada Quill", "emails": [{ "value": "ada@example.com" }], "phones": [{ "value": "+1 555 0123" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{change}");
    assert_eq!(change["item_id"], card.item_id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn readers_and_read_only_collections_cannot_edit() {
    let f = fixture().await;
    let p = pim(&f).await;
    f.allow_pim().await;
    let (grace, member) = f.add_user("grace@example.org", "member").await;
    let (status, _, _) = f
        .send(request(
            "PUT",
            &format!("/api/users/{grace}/grants/{}", f.archive),
            json!({ "role": "reader" }),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let body = json!({ "contact": { "full_name": "Bea Example" } });
    let (status, _, _) = f
        .send_as(
            &member,
            request(
                "POST",
                &format!("/api/collections/{}/items", p.book),
                body.clone(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .send_as(
            &member,
            request("DELETE", &format!("/api/items/{}", p.event), json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .send_as(
            &member,
            request(
                "PUT",
                &format!("/api/collections/{}/write-mode", p.book),
                json!({ "mode": "read_only" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = f
        .write(
            "PUT",
            &format!("/api/collections/{}/write-mode", p.book),
            json!({ "mode": "read_only" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = f
        .write("POST", &format!("/api/collections/{}/items", p.book), body)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a read-only collection");
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_editable_collections_and_change_diffs() {
    let f = fixture().await;
    let p = pim(&f).await;
    let editable = |collections: &Value, id: &str| {
        collections
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .map(|c| (c["write_mode"].clone(), c["editable"].clone()))
            .unwrap()
    };
    let collections = f
        .json(&format!("/api/archives/{}/collections", f.archive))
        .await;
    assert_eq!(
        editable(&collections, &p.calendar),
        (json!("held"), json!(false)),
        "pim.edit is off"
    );
    f.allow_pim().await;
    let collections = f
        .json(&format!("/api/archives/{}/collections", f.archive))
        .await;
    assert_eq!(
        editable(&collections, &p.calendar),
        (json!("held"), json!(true))
    );

    let edit = f.json(&format!("/api/items/{}/edit", p.event)).await;
    let mut form = edit["event"].clone();
    form["title"] = json!("Daily standup");
    let (status, change) = f
        .write(
            "PUT",
            &format!("/api/items/{}/content", p.event),
            json!({ "event": form }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{change}");
    let open = f.json("/api/pim/changes").await;
    assert_eq!(open[0]["title"], "Daily standup");
    let diff = f
        .json(&format!(
            "/api/pim/changes/{}/diff",
            change["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(diff["base_version"], 1);
    assert_eq!(diff["latest_version"], 1);
    assert_eq!(diff["content_type"], "text/calendar");
    assert!(diff["mine"].as_str().unwrap().contains("Daily standup"));
    let summary = diff["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["field"] == "summary")
        .expect("the title changed");
    assert_eq!(summary["base"], json!(["Standup"]));
    assert_eq!(summary["mine"], json!(["Daily standup"]));
    assert_eq!(summary["mine_changed"], true);
    assert_eq!(summary["theirs_changed"], false);
    assert_eq!(summary["conflict"], false);

    let (status, _) = f
        .write(
            "GET",
            &format!(
                "/api/pim/changes/{}/diff",
                holtmail_core::ItemChangeId::new()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
