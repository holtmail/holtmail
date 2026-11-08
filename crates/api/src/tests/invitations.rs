use holtmail_collection::WriteCaps;
use holtmail_core::ArchiveId;
use holtmail_pim_edit::Host;
use holtmail_store::{Capabilities, Capability, CollectionKind, NewCollection, WriteMode};

use super::pim_edit::server_account;
use super::*;

const INVITE: &str = "From: Bob Stone <bob@example.org>\r\nTo: Ada Lovelace <ada@example.com>\r\nSubject: Invitation: Harbour walk\r\nDate: Mon, 02 Nov 2026 09:00:00 +0000\r\nMessage-ID: <harbour-invite@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nA walk along the harbour.\r\n--alt\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:harbour@example.org\r\nSEQUENCE:1\r\nDTSTAMP:20261102T085900Z\r\nDTSTART:20261204T150000Z\r\nDTEND:20261204T160000Z\r\nSUMMARY:Harbour walk\r\nORGANIZER;CN=Bob Stone:mailto:bob@example.org\r\nATTENDEE;CN=Ada Lovelace;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:cy@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--alt--\r\n";

const COPY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:harbour@example.org\r\nSEQUENCE:1\r\nDTSTAMP:20261102T085900Z\r\nDTSTART:20261204T150000Z\r\nDTEND:20261204T160000Z\r\nSUMMARY:Harbour walk\r\nORGANIZER;CN=Bob Stone:mailto:bob@example.org\r\nATTENDEE;CN=Ada Lovelace;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const ORGANIZED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:lunch@example.com\r\nSEQUENCE:0\r\nDTSTAMP:20261102T085900Z\r\nDTSTART:20261210T120000Z\r\nDTEND:20261210T130000Z\r\nSUMMARY:Lunch\r\nORGANIZER;CN=Ada Lovelace:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:cy@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

impl Fixture {
    async fn deliver(&self, raw: &'static str, uid: u64) {
        self.ingest
            .ingest(
                self.archive.parse().unwrap(),
                Bytes::from_static(raw.as_bytes()),
                &Placement {
                    folder: Some(self.folder.parse().unwrap()),
                    uid: Some(uid),
                    ..Placement::default()
                },
            )
            .await
            .unwrap();
        self.ingest.commit().await.unwrap();
    }

    async fn capabilities(&self, account: holtmail_core::AccountId, caps: &[Capability]) {
        self.store
            .set_account_capabilities(account, &caps.iter().copied().collect::<Capabilities>())
            .await
            .unwrap();
    }

    async fn import_account(&self) -> holtmail_core::AccountId {
        let archive: ArchiveId = self.archive.parse().unwrap();
        self.store
            .list_accounts(archive)
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.kind == AccountKind::Import)
            .unwrap()
            .id
    }

    async fn calendar(
        &self,
        raw: &'static str,
        name: &str,
    ) -> (holtmail_store::Collection, String) {
        let archive: ArchiveId = self.archive.parse().unwrap();
        let account = server_account(self).await;
        let calendar = self
            .store
            .upsert_collection(NewCollection {
                archive_id: archive,
                account_id: Some(account),
                kind: CollectionKind::Calendar,
                remote_id: "/cals/work/".into(),
                name: "Work".into(),
                color: None,
            })
            .await
            .unwrap();
        let item = self
            .ingest
            .ingest_item(holtmail_ingest::IncomingItem {
                collection: calendar.id,
                remote_id: format!("/cals/work/{name}"),
                kind: holtmail_pim::ItemKind::Event,
                format: holtmail_pim::Format::ICalendar,
                raw: Bytes::from_static(raw.as_bytes()),
                etag: Some("\"c1\"".into()),
                seen_at: holtmail_core::Timestamp::from_secs(1_800_000_000),
            })
            .await
            .unwrap();
        (calendar, item.item_id.to_string())
    }

    fn schedules(&self, host: Option<Host>) {
        *self.mailboxes.scheduling.lock().unwrap() = host.map(|host| {
            crate::SchedulingProbe::Known(crate::Scheduling {
                host,
                caps: WriteCaps {
                    create: true,
                    update: true,
                    delete: true,
                    respond: true,
                    tags: true,
                    keeps_uid: true,
                    ..WriteCaps::default()
                },
            })
        });
    }

    async fn sent(&self) -> Vec<(Vec<String>, String)> {
        let sending = self.state.sending.clone().unwrap();
        while sending.outbox.send_due().await.unwrap() > 0 {}
        self.submitted
            .lock()
            .unwrap()
            .drain(..)
            .map(|(envelope, bytes)| {
                (
                    envelope.recipients,
                    String::from_utf8_lossy(&bytes).into_owned(),
                )
            })
            .collect()
    }
}

fn unfolded(text: &str) -> String {
    text.replace("\r\n ", "")
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_an_invitation_from_mail_with_an_imip_reply() {
    let f = fixture().await;
    f.sending_identity().await;
    f.deliver(INVITE, 10).await;
    let message = f.message_with_subject("Invitation: Harbour walk").await;
    let uri = format!("/api/messages/{message}/rsvp");

    let (status, body) = f.call("POST", &uri, json!({ "answer": "accepted" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pim.respond is off: {body}");
    f.capabilities(f.import_account().await, &[Capability::PimRespond])
        .await;
    let (status, _) = f.call("POST", &uri, json!({ "answer": "maybe" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let answered = f
        .ok(
            "POST",
            &uri,
            json!({ "answer": "accepted", "comment": "See you there" }),
        )
        .await;
    assert_eq!(answered["route"], "imip", "{answered}");
    assert_eq!(answered["copy"], "none");
    assert_eq!(answered["outbox"]["state"], "queued");
    assert!(answered["change"].is_null());

    let sent = f.sent().await;
    assert_eq!(sent.len(), 1);
    let (recipients, bytes) = &sent[0];
    assert_eq!(recipients, &vec!["bob@example.org".to_owned()]);
    let text = unfolded(bytes);
    assert!(text.contains("METHOD:REPLY"), "{text}");
    assert!(text.contains("method=\"REPLY\""), "{text}");
    assert!(
        text.contains("PARTSTAT=ACCEPTED:mailto:ada@example.com"),
        "{text}"
    );
    assert!(!text.contains("cy@example.org"), "{text}");
    assert!(text.contains("COMMENT:See you there"), "{text}");
    assert!(
        text.contains("In-Reply-To: <harbour-invite@example.org>"),
        "{text}"
    );
    assert!(text.contains("Subject: Accepted: Harbour walk"), "{text}");
    assert!(text.contains("ada@example.com>"), "{text}");

    let detail = f.json(&format!("/api/messages/{message}")).await;
    assert_eq!(
        detail["invitations"][0]["my_status"], "accepted",
        "{detail}"
    );
    let kinds = f.audit_kinds().await;
    assert!(kinds.iter().any(|k| k == "pim_respond"), "{kinds:?}");
    assert!(kinds.iter().any(|k| k == "mail_send"), "{kinds:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_own_events_and_strangers() {
    let f = fixture().await;
    f.sending_identity().await;
    f.capabilities(f.import_account().await, &[Capability::PimRespond])
        .await;
    let own: &'static str = Box::leak(
        INVITE
            .replace("Subject: Invitation: Harbour walk", "Subject: Mine")
            .replace("UID:harbour@example.org", "UID:mine@example.com")
            .replace(
                "ORGANIZER;CN=Bob Stone:mailto:bob@example.org",
                "ORGANIZER:mailto:ada@example.com",
            )
            .into_boxed_str(),
    );
    f.deliver(own, 11).await;
    let message = f.message_with_subject("Mine").await;
    let (status, body) = f
        .call(
            "POST",
            &format!("/api/messages/{message}/rsvp"),
            json!({ "answer": "accepted" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let stranger: &'static str = Box::leak(
        INVITE
            .replace("Subject: Invitation: Harbour walk", "Subject: Not mine")
            .replace("UID:harbour@example.org", "UID:other@example.org")
            .replace("mailto:ada@example.com", "mailto:dee@example.net")
            .into_boxed_str(),
    );
    f.deliver(stranger, 12).await;
    let message = f.message_with_subject("Not mine").await;
    let (status, body) = f
        .call(
            "POST",
            &format!("/api/messages/{message}/rsvp"),
            json!({ "answer": "declined" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(f.sent().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_an_account_copy_through_its_server_or_by_mail() {
    let f = fixture().await;
    f.sending_identity().await;
    let (calendar, item) = f.calendar(COPY, "harbour.ics").await;
    let account = server_account(&f).await;
    let uri = format!("/api/items/{item}/rsvp");
    let (status, _) = f.call("POST", &uri, json!({ "answer": "tentative" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    f.capabilities(account, &[Capability::PimRespond]).await;
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();

    f.schedules(Some(Host::Graph));
    let answered = f.ok("POST", &uri, json!({ "answer": "tentative" })).await;
    assert_eq!(answered["route"], "server", "{answered}");
    assert_eq!(answered["copy"], "respond");
    assert_eq!(answered["change"]["class"], "reply", "{answered}");
    assert!(answered["outbox"].is_null());
    let change = f
        .store
        .get_change(answered["change"]["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let patch = change.patch.unwrap().to_string();
    assert!(patch.contains("\"notify\":true"), "{patch}");
    assert!(patch.contains("TENTATIVE"), "{patch}");
    assert!(f.sent().await.is_empty(), "the server tells the organizer");
    let edit = f.json(&format!("/api/items/{item}/edit")).await;
    assert!(
        edit["raw"]
            .as_str()
            .unwrap()
            .contains("PARTSTAT=TENTATIVE:mailto:ada@example.com"),
        "{edit}"
    );

    f.schedules(None);
    f.store
        .set_write_mode(calendar.id, WriteMode::Held)
        .await
        .unwrap();
    let answered = f.ok("POST", &uri, json!({ "answer": "declined" })).await;
    assert_eq!(answered["route"], "imip", "{answered}");
    assert_eq!(answered["copy"], "held");
    let change = f
        .store
        .get_change(answered["change"]["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(
        change
            .patch
            .unwrap()
            .to_string()
            .contains("\"notify\":false"),
        "a later write must not tell the organizer again"
    );
    let sent = f.sent().await;
    assert_eq!(sent.len(), 1);
    assert!(unfolded(&sent[0].1).contains("PARTSTAT=DECLINED:mailto:ada@example.com"));
    assert_eq!(sent[0].0, vec!["bob@example.org".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn organizers_mail_invitations_unless_the_server_schedules() {
    let f = fixture().await;
    f.sending_identity().await;
    let (calendar, item) = f.calendar(ORGANIZED, "lunch.ics").await;
    f.allow_pim().await;

    let created = f
        .ok(
            "POST",
            &format!("/api/collections/{}/items", calendar.id),
            json!({ "event": {
                "title": "Planning",
                "start": "2026-12-14T10:00",
                "end": "2026-12-14T11:00",
                "time_zone": "Europe/Berlin",
                "attendees": [{ "email": "bob@example.org", "name": "Bob Stone" }],
            } }),
        )
        .await;
    assert_eq!(created["state"], "held", "{created}");
    let sent = f.sent().await;
    assert_eq!(sent.len(), 1, "one invitation");
    let text = unfolded(&sent[0].1);
    assert_eq!(sent[0].0, vec!["bob@example.org".to_owned()]);
    assert!(text.contains("METHOD:REQUEST"), "{text}");
    assert!(text.contains("ORGANIZER"), "{text}");
    assert!(text.contains("mailto:ada@example.com"), "{text}");
    assert!(text.contains("Subject: Invitation: Planning"), "{text}");

    let edit = f.json(&format!("/api/items/{item}/edit")).await;
    let mut form = edit["event"].clone();
    form["start"] = json!("2026-12-10T13:00");
    form["end"] = json!("2026-12-10T14:00");
    form["attendees"] = json!([{ "email": "bob@example.org" }]);
    f.ok(
        "PUT",
        &format!("/api/items/{item}/content"),
        json!({ "event": form, "expect": edit["state"] }),
    )
    .await;
    let sent = f.sent().await;
    assert_eq!(sent.len(), 2, "a cancellation and an update: {sent:?}");
    let cancel = sent
        .iter()
        .find(|(_, t)| unfolded(t).contains("METHOD:CANCEL"))
        .unwrap();
    assert_eq!(cancel.0, vec!["cy@example.org".to_owned()]);
    let update = sent
        .iter()
        .find(|(_, t)| unfolded(t).contains("METHOD:REQUEST"))
        .unwrap();
    assert_eq!(update.0, vec!["bob@example.org".to_owned()]);
    assert!(unfolded(&update.1).contains("SEQUENCE:1"), "{}", update.1);

    f.ok(
        "PUT",
        &format!("/api/items/{item}/content"),
        json!({ "raw": ORGANIZED.replace("SUMMARY:Lunch", "SUMMARY:Long lunch"), "invitations": false }),
    )
    .await;
    assert!(f.sent().await.is_empty(), "invitations: false");

    f.schedules(Some(Host::Graph));
    f.ok(
        "POST",
        &format!("/api/collections/{}/items", calendar.id),
        json!({ "event": {
            "title": "Deferred",
            "start": "2026-12-15T10:00",
            "attendees": [{ "email": "bob@example.org" }],
        } }),
    )
    .await;
    assert!(
        f.sent().await.is_empty(),
        "a held change on a scheduling server waits for the server"
    );
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    f.ok("DELETE", &format!("/api/items/{item}"), json!({}))
        .await;
    assert!(f.sent().await.is_empty(), "the server cancels");

    f.schedules(None);
    let (other, other_item) = f
        .calendar(
            Box::leak(
                ORGANIZED
                    .replace("UID:lunch@example.com", "UID:dinner@example.com")
                    .into_boxed_str(),
            ),
            "dinner.ics",
        )
        .await;
    assert_eq!(other.id, calendar.id);
    f.store
        .set_write_mode(calendar.id, WriteMode::Held)
        .await
        .unwrap();
    f.ok("DELETE", &format!("/api/items/{other_item}"), json!({}))
        .await;
    let sent = f.sent().await;
    assert_eq!(sent.len(), 1);
    assert!(unfolded(&sent[0].1).contains("METHOD:CANCEL"));
    assert_eq!(
        sent[0].0,
        vec!["bob@example.org".to_owned(), "cy@example.org".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn organizers_without_sending_rights_save_only_without_invitations() {
    let f = fixture().await;
    let (calendar, _) = f.calendar(ORGANIZED, "lunch.ics").await;
    f.allow_pim().await;
    let body = json!({ "event": {
        "title": "Planning",
        "start": "2026-12-14T10:00",
        "organizer": { "email": "ada@example.com" },
        "attendees": [{ "email": "bob@example.org" }],
    } });
    let uri = format!("/api/collections/{}/items", calendar.id);
    let (status, answer) = f.call("POST", &uri, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    let mut quiet = body;
    quiet["invitations"] = json!(false);
    f.ok("POST", &uri, quiet).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn options_explain_the_answer_before_it_is_given() {
    let f = fixture().await;
    f.deliver(INVITE, 10).await;
    let message = f.message_with_subject("Invitation: Harbour walk").await;
    let options = f.json(&format!("/api/messages/{message}/rsvp")).await;
    assert_eq!(
        options["reason"], "not_invited",
        "no own address yet: {options}"
    );

    f.sending_identity().await;
    let uri = format!("/api/messages/{message}/rsvp");

    let options = f.json(&uri).await;
    assert_eq!(options["allowed"], false, "{options}");
    assert_eq!(options["reason"], "locked");
    assert_eq!(options["my_status"], "needs-action");
    assert_eq!(options["recurring"], false);

    f.capabilities(f.import_account().await, &[Capability::PimRespond])
        .await;
    let options = f.json(&uri).await;
    assert_eq!(options["allowed"], true, "{options}");
    assert_eq!(options["route"], "imip");
    assert_eq!(options["copy"], "none");
    assert_eq!(options["from"], "ada@example.com");
    assert_eq!(options["attendee"], "ada@example.com");
    assert!(options["item_id"].is_null());
    assert!(f.sent().await.is_empty(), "options send nothing");
    assert!(
        !f.audit_kinds().await.iter().any(|k| k == "pim_respond"),
        "options record nothing"
    );

    let (calendar, item) = f.calendar(COPY, "harbour.ics").await;
    f.capabilities(server_account(&f).await, &[Capability::PimRespond])
        .await;
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    f.schedules(Some(Host::Graph));
    let options = f.json(&format!("/api/items/{item}/rsvp")).await;
    assert_eq!(options["allowed"], true, "{options}");
    assert_eq!(options["route"], "server");
    assert_eq!(options["copy"], "respond");
    assert_eq!(options["item_id"], item.as_str());
    assert_eq!(options["collection_name"], "Work");
    assert!(options["from"].is_null());
    assert!(options["local_edit"].is_null());

    f.ok(
        "POST",
        &format!("/api/items/{item}/rsvp"),
        json!({ "answer": "accepted" }),
    )
    .await;
    let options = f.json(&format!("/api/items/{item}/rsvp")).await;
    assert_eq!(options["my_status"], "accepted", "{options}");
    assert_eq!(options["local_edit"]["class"], "reply", "{options}");
}

#[tokio::test(flavor = "multi_thread")]
async fn options_name_organizers_and_who_sends_their_invitations() {
    let f = fixture().await;
    let (calendar, item) = f.calendar(ORGANIZED, "lunch.ics").await;
    let options = f.json(&format!("/api/items/{item}/rsvp")).await;
    assert_eq!(options["reason"], "organizer", "{options}");

    let uri = format!("/api/collections/{}/organizing", calendar.id);
    let organizing = f.json(&uri).await;
    assert_eq!(organizing["route"], "imip", "{organizing}");
    assert_eq!(organizing["from"], "ada@example.com");
    assert_eq!(organizing["allowed"], false, "no identity sends as ada yet");

    f.sending_identity().await;
    let organizing = f.json(&format!("{uri}?item={item}")).await;
    assert_eq!(organizing["route"], "imip", "{organizing}");
    assert_eq!(organizing["allowed"], true, "{organizing}");

    f.schedules(Some(Host::Graph));
    assert_eq!(f.json(&uri).await["route"], "deferred");
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    f.allow_pim().await;
    let organizing = f.json(&uri).await;
    assert_eq!(organizing["route"], "server", "{organizing}");
    assert!(organizing["from"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn options_list_occurrences_as_the_series_writes_them() {
    let f = fixture().await;
    f.sending_identity().await;
    let weekly: &'static str = Box::leak(
        COPY.replace(
            "DTSTART:20261204T150000Z",
            "DTSTART;TZID=Europe/Berlin:20261204T160000",
        )
        .replace(
            "DTEND:20261204T160000Z",
            "DTEND;TZID=Europe/Berlin:20261204T170000\r\nRRULE:FREQ=WEEKLY;COUNT=3",
        )
        .into_boxed_str(),
    );
    let (_, item) = f.calendar(weekly, "weekly.ics").await;
    f.capabilities(server_account(&f).await, &[Capability::PimRespond])
        .await;
    let options = f.json(&format!("/api/items/{item}/rsvp")).await;
    assert_eq!(options["recurring"], true, "{options}");
    let occurrences = options["occurrences"].as_array().unwrap();
    assert_eq!(occurrences.len(), 3, "{options}");
    assert_eq!(occurrences[1]["recurrence_id"], "20261211T160000");
    assert_eq!(occurrences[1]["key"], "2026-12-11T15:00:00Z");
    assert_eq!(occurrences[1]["start_local"], "2026-12-11T16:00:00");

    let answered = f
        .ok(
            "POST",
            &format!("/api/items/{item}/rsvp"),
            json!({ "answer": "declined", "recurrence_id": occurrences[1]["recurrence_id"] }),
        )
        .await;
    assert_eq!(answered["route"], "imip", "{answered}");
    let sent = f.sent().await;
    let text = unfolded(&sent[0].1);
    assert!(
        text.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261211T160000"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_can_be_taken_back_with_only_the_respond_capability() {
    let f = fixture().await;
    f.sending_identity().await;
    let (_, item) = f.calendar(COPY, "harbour.ics").await;
    f.capabilities(server_account(&f).await, &[Capability::PimRespond])
        .await;
    let answered = f
        .ok(
            "POST",
            &format!("/api/items/{item}/rsvp"),
            json!({ "answer": "accepted" }),
        )
        .await;
    let change = answered["change"]["id"].as_str().unwrap();
    let uri = format!("/api/pim/changes/{change}/resolve");
    let (status, _) = f.call("POST", &uri, json!({ "action": "retry" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only discarding the answer");
    f.ok("POST", &uri, json!({ "action": "discard" })).await;
    let options = f.json(&format!("/api/items/{item}/rsvp")).await;
    assert!(options["local_edit"].is_null(), "{options}");
    assert_eq!(options["my_status"], "needs-action");
}

fn leaked(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn edited(raw: &str) -> Value {
    json!({ "raw": raw.replace("SUMMARY:Lunch", "SUMMARY:Long lunch"), "expect": null })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_cannot_be_asked_mails_nobody() {
    let f = fixture().await;
    f.sending_identity().await;
    let (calendar, item) = f.calendar(ORGANIZED, "lunch.ics").await;
    f.capabilities(
        server_account(&f).await,
        &[Capability::PimEdit, Capability::MailSend],
    )
    .await;
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    *f.mailboxes.scheduling.lock().unwrap() = Some(crate::SchedulingProbe::Unknown);
    let organizing = f
        .json(&format!(
            "/api/collections/{}/organizing?item={item}",
            calendar.id
        ))
        .await;
    assert_eq!(organizing["route"], "unknown", "{organizing}");
    assert_eq!(organizing["allowed"], false);
    let uri = format!("/api/items/{item}/content");
    let (status, body) = f.call("PUT", &uri, edited(ORGANIZED)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(f.sent().await.is_empty());
    let mut quiet = edited(ORGANIZED);
    quiet["invitations"] = json!(false);
    f.ok("PUT", &uri, quiet).await;
    assert!(f.sent().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_change_on_a_scheduling_server_still_mails_client_scheduled_attendees() {
    let f = fixture().await;
    f.sending_identity().await;
    let raw = leaked(ORGANIZED.replace(
        "ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:cy@example.org",
        "ATTENDEE;SCHEDULE-AGENT=CLIENT;PARTSTAT=NEEDS-ACTION:mailto:cy@example.org",
    ));
    let (_, item) = f.calendar(raw, "lunch.ics").await;
    f.capabilities(
        server_account(&f).await,
        &[Capability::PimEdit, Capability::MailSend],
    )
    .await;
    f.schedules(Some(Host::Graph));
    f.ok("PUT", &format!("/api/items/{item}/content"), edited(raw))
        .await;
    let sent = f.sent().await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, vec!["cy@example.org".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_emailed_answer_stays_off_the_server_until_the_undo_window_ends() {
    let f = fixture().await;
    f.sending_identity().await;
    let (calendar, item) = f.calendar(COPY, "harbour.ics").await;
    f.capabilities(
        server_account(&f).await,
        &[Capability::PimRespond, Capability::MailSend],
    )
    .await;
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    f.schedules(Some(Host::CalDav {
        auto_schedule: false,
    }));
    let answered = f
        .ok(
            "POST",
            &format!("/api/items/{item}/rsvp"),
            json!({ "answer": "accepted" }),
        )
        .await;
    assert_eq!(answered["route"], "imip", "{answered}");
    let change = f
        .store
        .get_change(answered["change"]["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let outbox: holtmail_core::OutboxId =
        answered["outbox"]["id"].as_str().unwrap().parse().unwrap();
    let entry = f.store.get_outbox(outbox).await.unwrap().unwrap();
    assert!(
        change
            .next_attempt_at
            .is_some_and(|at| at > entry.new.send_at),
        "{change:?}"
    );
    let due = f
        .store
        .claim_change(calendar.id, entry.new.send_at)
        .await
        .unwrap();
    assert!(due.is_none(), "written while it can still be undone");

    f.store
        .change_outbox(
            outbox,
            &[holtmail_store::OutboxState::Queued],
            &holtmail_store::OutboxChange::to(
                holtmail_store::OutboxState::Draft,
                holtmail_core::Timestamp::now(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
    let sending = f.state.sending.clone().unwrap();
    assert_eq!(sending.outbox.recover_held().await.unwrap(), [outbox]);
    assert_eq!(
        f.store.get_outbox(outbox).await.unwrap().unwrap().state,
        holtmail_store::OutboxState::Queued,
        "the reply of a recorded answer leaves after a restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_waits_for_an_open_edit() {
    let f = fixture().await;
    f.sending_identity().await;
    let (calendar, item) = f.calendar(COPY, "harbour.ics").await;
    f.capabilities(
        server_account(&f).await,
        &[Capability::PimRespond, Capability::PimEdit],
    )
    .await;
    f.store
        .set_write_mode(calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    f.schedules(Some(Host::Graph));
    f.ok(
        "PUT",
        &format!("/api/items/{item}/content"),
        json!({ "raw": COPY.replace("Harbour walk", "Pier walk"), "expect": null, "invitations": false }),
    )
    .await;
    let uri = format!("/api/items/{item}/rsvp");
    let options = f.json(&uri).await;
    assert_eq!(options["allowed"], false, "{options}");
    assert_eq!(options["reason"], "open_edit");
    let (status, body) = f.call("POST", &uri, json!({ "answer": "accepted" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_answers_as_someone_else_or_shows_the_mailbox_behind_an_alias() {
    let f = fixture().await;
    let (transport, _) = f.sending_identity().await;
    let archive = &f.archive;
    let linked = f
        .ok(
            "POST",
            &format!("/api/archives/{archive}/identities"),
            json!({
                "address": "ada.real@example.com",
                "transport": transport,
                "alias": "shade@example.net",
                "active": true,
            }),
        )
        .await;
    assert_eq!(linked["alias"], "shade@example.net", "{linked}");
    let colleague = f
        .ok(
            "POST",
            &format!("/api/archives/{archive}/identities"),
            json!({ "address": "cy@example.com", "transport": transport, "active": true }),
        )
        .await;
    f.store
        .set_identity_users(colleague["id"].as_str().unwrap().parse().unwrap(), &[])
        .await
        .unwrap();
    let account = server_account(&f).await;
    f.capabilities(account, &[Capability::PimRespond, Capability::MailSend])
        .await;

    let as_alias = leaked(COPY.replace("mailto:ada@example.com", "mailto:shade@example.net"));
    f.store
        .put_alias(
            archive.parse().unwrap(),
            &holtmail_store::KnownAlias {
                address: "shade@example.net".into(),
                provider: "simplelogin".into(),
                service_id: None,
                remote_id: None,
                enabled: Some(true),
                note: None,
                name: None,
                for_domain: None,
                mailboxes: vec!["ada.real@example.com".into()],
                created_at: None,
                forwarded: None,
                blocked: None,
                replied: None,
            },
        )
        .await
        .unwrap();
    let (_, item) = f.calendar(as_alias, "alias.ics").await;
    let uri = format!("/api/items/{item}/rsvp");
    let options = f.json(&uri).await;
    assert_eq!(options["reason"], "cannot_send", "{options}");
    let (status, body) = f.call("POST", &uri, json!({ "answer": "accepted" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let as_colleague = leaked(COPY.replace("mailto:ada@example.com", "mailto:cy@example.com"));
    let (_, item) = f.calendar(as_colleague, "colleague.ics").await;
    let uri = format!("/api/items/{item}/rsvp");
    let (status, body) = f.call("POST", &uri, json!({ "answer": "accepted" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(f.sent().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_instance_only_update_keeps_the_series_answerable() {
    let f = fixture().await;
    f.sending_identity().await;
    let series = leaked(INVITE.replace(
        "DTEND:20261204T160000Z\r\n",
        "DTEND:20261204T160000Z\r\nRRULE:FREQ=DAILY;COUNT=400\r\n",
    ));
    let update = leaked(
        series
            .replace(
                "Message-ID: <harbour-invite@",
                "Message-ID: <harbour-update@",
            )
            .replace("RRULE:FREQ=DAILY;COUNT=400\r\n", "")
            .replace(
                "DTSTART:20261204T150000Z",
                "RECURRENCE-ID:20261205T150000Z\r\nDTSTART:20261205T170000Z",
            )
            .replace("DTEND:20261204T160000Z", "DTEND:20261205T180000Z")
            .replace("SEQUENCE:1", "SEQUENCE:2"),
    );
    f.deliver(series, 20).await;
    f.deliver(update, 21).await;
    f.capabilities(f.import_account().await, &[Capability::PimRespond])
        .await;
    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let latest = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == "Invitation: Harbour walk")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let options = f.json(&format!("/api/messages/{latest}/rsvp")).await;
    assert_eq!(options["recurring"], true, "{options}");
    assert!(!options["occurrences"].as_array().unwrap().is_empty());
}
