//! Invitations answered and sent through the outbox: an invitation delivered to a Dovecot
//! mailbox is archived, and answering it through the API sends an iMIP `REPLY` through GreenMail's
//! SMTP to the organizer's mailbox (read back over IMAP). An event with an invitee created in a
//! `write_through` Radicale calendar (Radicale does not schedule) mails a `REQUEST`; the
//! invitee's `REPLY` arriving in the organizer's mailbox lands on the event in Radicale as their
//! `PARTSTAT` (its DKIM pass stamped by the receiving server) while a forged `REPLY` does not;
//! deleting the event mails a `CANCEL`. On Stalwart,
//! a JMAP calendar's organizer mails the `REQUEST` itself (writes never ask the server to
//! schedule), and a CalDAV calendar leaves it to the server, whose attendee finds the event.

use std::time::{Duration, Instant};

use holtmail_integration::dav::{Dav, RADICALE, STALWART_DAV};
use holtmail_integration::imap::Seeder;
use holtmail_integration::pim::PimRun;
use holtmail_integration::services::{DOVECOT, GREENMAIL, GREENMAIL_SMTP, local};
use holtmail_integration::stalwart::{self, CALENDARS, STALWART_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use reqwest::Method;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(60);

fn unfolded(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).replace("\r\n ", "")
}

fn invitation(uid: &str, organizer: &str, attendee: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: Bob Example <{organizer}>\r\nTo: <{attendee}>\r\nSubject: {subject}\r\nDate: Mon, 02 Nov 2026 09:00:00 +0000\r\nMessage-ID: <{uid}>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nA walk along the harbour.\r\n--alt\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\nDTSTAMP:20261102T085900Z\r\nDTSTART:20261204T150000Z\r\nDTEND:20261204T160000Z\r\nSUMMARY:{subject}\r\nORGANIZER;CN=Bob Example:mailto:{organizer}\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--alt--\r\n"
    )
    .into_bytes()
}

/// An attendee's answer as the organizer's receiving server stamps it: DKIM passed for the
/// attendee's domain, under the authserv-id `trust_receiving_server` trusts, so the scheduler
/// applies it.
fn reply(uid: &str, organizer: &str, attendee: &str) -> Vec<u8> {
    let domain = attendee.rsplit('@').next().unwrap_or_default();
    let mut signed =
        format!("Authentication-Results: mx.example.com; dkim=pass header.d={domain}\r\n")
            .into_bytes();
    signed.extend(reply_from(attendee, uid, organizer, attendee));
    signed
}

fn reply_from(from: &str, uid: &str, organizer: &str, attendee: &str) -> Vec<u8> {
    format!(
        "From: <{from}>\r\nTo: <{organizer}>\r\nSubject: Accepted\r\nDate: Tue, 03 Nov 2026 09:00:00 +0000\r\nMessage-ID: <reply-{from}-{uid}>\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; charset=utf-8; method=REPLY\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\nDTSTAMP:20261103T090000Z\r\nORGANIZER:mailto:{organizer}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{attendee}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
    .into_bytes()
}

/// Trusts the authserv-id `reply` stamps for the account, as an operator does once the account's
/// server is known to strip forged `Authentication-Results` fields.
async fn trust_receiving_server(api: &Api, account: &str) {
    let trusted = api
        .request(
            Method::PATCH,
            &format!("/api/accounts/{account}"),
            Some(&json!({ "trusted_authserv_ids": ["mx.example.com"] })),
        )
        .await
        .ok();
    assert_eq!(
        trusted["trusted_authserv_ids"],
        json!(["mx.example.com"]),
        "{trusted}"
    );
}

async fn sending_identity(api: &Api, archive: &str, address: &str) {
    let transport = api
        .post(
            &format!("/api/archives/{archive}/transports"),
            json!({
                "name": "GreenMail",
                "kind": "smtp",
                "host": "127.0.0.1",
                "port": GREENMAIL_SMTP,
                "security": "none",
                "allowed_from": ["*@example.com"],
            }),
        )
        .await
        .ok();
    let identity = api
        .post(
            &format!("/api/archives/{archive}/identities"),
            json!({
                "address": address,
                "name": "Ada Example",
                "transport": transport["id"],
                "active": true,
            }),
        )
        .await
        .ok();
    assert_eq!(identity["active"], true, "{identity}");
}

async fn capabilities(api: &Api, account: &str, caps: &[&str]) {
    api.request(
        Method::PUT,
        &format!("/api/accounts/{account}/capabilities"),
        Some(&json!({ "capabilities": caps })),
    )
    .await
    .ok();
}

/// Waits until `user`'s GreenMail inbox holds at least `count` messages and returns them.
async fn inbox(user: &str, count: usize) -> Vec<Vec<u8>> {
    let started = Instant::now();
    loop {
        let mut imap = Seeder::login(&local(GREENMAIL.port), user, GREENMAIL.password).await;
        let bodies = imap.bodies("INBOX").await;
        imap.logout().await;
        if bodies.len() >= count {
            return bodies;
        }
        assert!(
            started.elapsed() < WAIT,
            "{user} has {} of {count} messages",
            bodies.len()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn message_with_subject(api: &Api, archive: &str, subject: &str) -> String {
    let started = Instant::now();
    loop {
        if let Some(found) = api
            .messages(archive)
            .await
            .into_iter()
            .find(|m| m["subject"] == subject)
        {
            return found["id"].as_str().expect("an id").to_owned();
        }
        assert!(started.elapsed() < WAIT, "no message {subject}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_an_invitation_from_mail_with_an_imip_reply() {
    require_it!();
    let holtmail = Holtmail::start(Options {
        extra_config: "[write]\nundo_seconds = 1\n".into(),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique("rsvp")).await;
    let ada = DOVECOT.user("ada");
    let bob = format!("{}@example.org", unique("bob"));
    let subject = unique("Harbour walk");
    let uid = format!("{}@example.org", unique("harbour"));
    let mut seeder = DOVECOT.seeder(&ada).await;
    seeder
        .append("INBOX", &[], None, &invitation(&uid, &bob, &ada, &subject))
        .await;
    seeder.logout().await;
    let account = api.add_imap(&archive, DOVECOT.account(&ada)).await;
    api.settle(&archive, &account).await;
    sending_identity(&api, &archive, &ada).await;
    let message = message_with_subject(&api, &archive, &subject).await;
    let detail = api.message(&message).await;
    assert_eq!(
        detail["invitations"][0]["my_status"], "needs-action",
        "{detail}"
    );

    let uri = format!("/api/messages/{message}/rsvp");
    let refused = api.post(&uri, json!({ "answer": "accepted" })).await;
    assert_eq!(refused.status.as_u16(), 403, "pim.respond is off");
    capabilities(&api, &account, &["pim.respond"]).await;
    let answered = api
        .post(
            &uri,
            json!({ "answer": "accepted", "comment": "See you there" }),
        )
        .await
        .ok();
    assert_eq!(answered["route"], "imip", "{answered}");
    assert_eq!(answered["outbox"]["state"], "queued", "{answered}");

    let bodies = inbox(&bob, 1).await;
    let text = unfolded(&bodies[0]);
    assert!(text.contains("METHOD:REPLY"), "{text}");
    assert!(text.contains("method=\"REPLY\""), "{text}");
    assert!(
        text.contains(&format!("PARTSTAT=ACCEPTED:mailto:{ada}")),
        "{text}"
    );
    assert!(text.contains(&format!("<{ada}>")), "{text}");
    assert!(text.contains(&format!("In-Reply-To: <{uid}>")), "{text}");

    let started = Instant::now();
    loop {
        let detail = api.message(&message).await;
        if detail["invitations"][0]["my_status"] == "accepted" {
            break;
        }
        assert!(
            started.elapsed() < WAIT,
            "the answer never showed: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    LogScanner::new()
        .secrets("an address", [ada.as_str(), bob.as_str()])
        .secrets("a title", [subject.as_str()])
        .check(&holtmail.logs());
}

/// The `PARTSTAT` of `attendee` in the event text, unfolded.
fn answer_of(text: &str, attendee: &str) -> Option<String> {
    text.lines()
        .filter(|l| l.starts_with("ATTENDEE") && l.contains(&format!("mailto:{attendee}")))
        .find_map(|l| {
            l.split([';', ':'])
                .find_map(|p| p.strip_prefix("PARTSTAT="))
                .map(str::to_owned)
        })
}

/// Waits until the item at `url` reads as `check` wants; a missing item is the empty text.
async fn wait_for(dav: &Dav, url: &str, check: impl Fn(&str) -> bool) -> String {
    let started = Instant::now();
    loop {
        let (status, text) = dav.get(url).await;
        let unfolded = if status.is_success() {
            text.replace("\r\n ", "")
        } else {
            String::new()
        };
        if check(&unfolded) {
            return unfolded;
        }
        assert!(started.elapsed() < WAIT, "{url}: {status}\n{unfolded}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

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
        assert!(started.elapsed() < WAIT, "changes still open: {open:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn organizes_an_event_in_a_radicale_calendar_by_mail() {
    require_it!();
    let user = RADICALE.user("organizer").await;
    let dav = RADICALE.client(&user);
    let homes = RADICALE.homes(&dav).await;
    let team = dav.create_calendar(&homes.calendars, "team", "Team").await;
    let bob = format!("{}@example.org", unique("bob"));
    let title = unique("Planning");

    let mut run = PimRun::start_with(
        "Invitations on radicale",
        Options {
            extra_config: "[write]\nundo_seconds = 1\n".into(),
            ..Options::default()
        },
    )
    .await;
    run.secrets("an item", &[bob.as_str(), title.as_str()]);
    let account = run.add_dav(&RADICALE, &user, None).await;
    capabilities(&run.api, &account, &["pim.edit", "pim.respond"]).await;
    trust_receiving_server(&run.api, &account).await;
    sending_identity(&run.api, &run.archive, &user).await;
    let collections = run.collections().await;
    let team_id = collections["Team"]["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    let mode = run
        .api
        .request(
            Method::PUT,
            &format!("/api/collections/{team_id}/write-mode"),
            Some(&json!({ "mode": "write_through", "confirm": true })),
        )
        .await
        .ok();
    assert_eq!(mode["mode"], "write_through", "{mode}");

    run.api
        .post(
            &format!("/api/collections/{team_id}/items"),
            json!({ "event": {
                "title": title,
                "start": "2026-12-14T10:00:00",
                "end": "2026-12-14T11:00:00",
                "time_zone": "UTC",
                "attendees": [{ "email": bob }],
            } }),
        )
        .await
        .ok();
    let request = unfolded(&inbox(&bob, 1).await[0]);
    assert!(request.contains("METHOD:REQUEST"), "{request}");
    assert!(
        request.contains(&format!("ORGANIZER;CN=Ada Example:mailto:{user}")),
        "{request}"
    );
    assert!(request.contains(&format!("mailto:{bob}")), "{request}");
    settled(&run, &team_id).await;
    run.sync(&account).await;
    let item = run
        .api
        .items(&run.archive, "events")
        .await
        .into_iter()
        .find(|i| i["title"] == title.as_str() && i["collection_id"] == team_id.as_str())
        .expect("the event in Team")["id"]
        .as_str()
        .expect("an item")
        .to_owned();
    let edit = run.api.get(&format!("/api/items/{item}/edit")).await.ok();
    let uid = edit["event"]["uid"].as_str().expect("a UID").to_owned();
    let listing = dav.propfind(&team, "1", "<d:getetag/>").await;
    let href = holtmail_integration::dav::hrefs_in(&listing, "multistatus")
        .into_iter()
        .find(|h| h.ends_with(".ics"))
        .expect("the event upstream");
    let url = reqwest::Url::parse(&team)
        .unwrap()
        .join(&href)
        .unwrap()
        .to_string();
    let before = wait_for(&dav, &url, |t| t.contains(&format!("mailto:{bob}"))).await;
    assert_ne!(
        answer_of(&before, &bob).as_deref(),
        Some("ACCEPTED"),
        "{before}"
    );

    let eve = format!("{}@example.org", unique("eve"));
    run.secrets("an item", &[eve.as_str()]);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append("INBOX", &[], None, &reply_from(&eve, &uid, &user, &bob))
        .await;
    run.api.sync(&run.archive, &account).await;
    settled(&run, &team_id).await;
    let (_, forged) = dav.get(&url).await;
    assert_ne!(
        answer_of(&forged.replace("\r\n ", ""), &bob).as_deref(),
        Some("ACCEPTED"),
        "a stranger answered for the attendee: {forged}"
    );
    seeder
        .append("INBOX", &[], None, &reply(&uid, &user, &bob))
        .await;
    seeder.logout().await;
    run.api.sync(&run.archive, &account).await;
    let answered = wait_for(&dav, &url, |t| {
        answer_of(t, &bob).as_deref() == Some("ACCEPTED")
    })
    .await;
    assert!(answered.contains(&title), "{answered}");
    settled(&run, &team_id).await;

    let deleted = run
        .api
        .request(Method::DELETE, &format!("/api/items/{item}"), None)
        .await
        .ok();
    assert_eq!(deleted["op"], "delete", "{deleted}");
    let bodies = inbox(&bob, 2).await;
    let cancel = bodies
        .iter()
        .map(|b| unfolded(b))
        .find(|t| t.contains("METHOD:CANCEL"))
        .expect("a cancellation");
    assert!(cancel.contains("STATUS:CANCELLED"), "{cancel}");
    assert!(cancel.contains(&format!("UID:{uid}")), "{cancel}");
    wait_for(&dav, &url, |t| !t.contains("BEGIN:VEVENT")).await;
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jmap_calendar_mails_its_invitations_through_the_outbox() {
    require_it!();
    let user = STALWART_DAV.user("jorganizer").await;
    let bob = format!("{}@example.org", unique("bob"));
    let title = unique("Budget");
    let mut run = PimRun::start_with(
        "Invitations on JMAP",
        Options {
            extra_config: "[write]\nundo_seconds = 1\n".into(),
            ..Options::default()
        },
    )
    .await;
    run.secrets("an item", &[bob.as_str(), title.as_str(), user.as_str()]);
    let added = run
        .api
        .add_jmap_account(
            &run.archive,
            json!({
                "url": stalwart::session_url(),
                "username": user,
                "password": STALWART_PASSWORD,
            }),
        )
        .await
        .ok();
    let account = added["id"].as_str().expect("an id").to_owned();
    run.api.settle(&run.archive, &account).await;
    run.api.enable_contacts(&account, None).await.ok();
    run.sync(&account).await;
    capabilities(&run.api, &account, &["pim.edit", "pim.respond"]).await;
    sending_identity(&run.api, &run.archive, &user).await;
    let collections = run.collections().await;
    let calendar = collections
        .values()
        .find(|c| c["kind"] == "calendar")
        .expect("a calendar")["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    run.api
        .request(
            Method::PUT,
            &format!("/api/collections/{calendar}/write-mode"),
            Some(&json!({ "mode": "write_through", "confirm": true })),
        )
        .await
        .ok();
    let organizing = run
        .api
        .get(&format!("/api/collections/{calendar}/organizing"))
        .await
        .ok();
    assert_eq!(organizing["route"], "imip", "{organizing}");

    run.api
        .post(
            &format!("/api/collections/{calendar}/items"),
            json!({ "event": {
                "title": title,
                "start": "2026-12-15T10:00:00",
                "end": "2026-12-15T11:00:00",
                "time_zone": "UTC",
                "attendees": [{ "email": bob }],
            } }),
        )
        .await
        .ok();
    let request = unfolded(&inbox(&bob, 1).await[0]);
    assert!(request.contains("METHOD:REQUEST"), "{request}");
    assert!(request.contains(&format!("mailto:{bob}")), "{request}");
    settled(&run, &calendar).await;
    run.sync(&account).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(inbox(&bob, 1).await.len(), 1, "the attendee was told twice");
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caldav_server_that_schedules_tells_the_attendee_itself() {
    require_it!();
    let user = STALWART_DAV.user("dorganizer").await;
    let attendee = STALWART_DAV.user("dattendee").await;
    let title = unique("Review");
    let mut run = PimRun::start_with("Invitations on Stalwart CalDAV", Options::default()).await;
    run.secrets("an item", &[attendee.as_str(), title.as_str()]);
    let account = run.add_dav(&STALWART_DAV, &user, None).await;
    capabilities(&run.api, &account, &["pim.edit"]).await;
    let collections = run.collections().await;
    let calendar = collections
        .values()
        .find(|c| c["kind"] == "calendar")
        .expect("a calendar")["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    run.api
        .request(
            Method::PUT,
            &format!("/api/collections/{calendar}/write-mode"),
            Some(&json!({ "mode": "write_through", "confirm": true })),
        )
        .await
        .ok();
    let organizing = run
        .api
        .get(&format!("/api/collections/{calendar}/organizing"))
        .await
        .ok();
    assert_eq!(organizing["route"], "server", "{organizing}");

    run.api
        .post(
            &format!("/api/collections/{calendar}/items"),
            json!({ "event": {
                "title": title,
                "start": "2026-12-16T10:00:00",
                "end": "2026-12-16T11:00:00",
                "time_zone": "UTC",
                "attendees": [{ "email": attendee }],
                "organizer": { "email": user },
            } }),
        )
        .await
        .ok();
    settled(&run, &calendar).await;

    let jmap = stalwart::Seeder::new(&attendee).await.jmap;
    let account_id = jmap.account("urn:ietf:params:jmap:calendars");
    let started = Instant::now();
    loop {
        let found = jmap
            .ok(
                CALENDARS,
                "CalendarEvent/query",
                json!({ "accountId": account_id }),
            )
            .await;
        let ids = found["ids"].clone();
        if ids.as_array().is_some_and(|i| !i.is_empty()) {
            let events = jmap
                .ok(
                    CALENDARS,
                    "CalendarEvent/get",
                    json!({ "accountId": account_id, "ids": ids }),
                )
                .await;
            if events["list"]
                .as_array()
                .is_some_and(|l| l.iter().any(|e| e["title"] == title.as_str()))
            {
                break;
            }
        }
        assert!(
            started.elapsed() < WAIT,
            "the server did not tell the attendee"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    run.check_logs();
}
