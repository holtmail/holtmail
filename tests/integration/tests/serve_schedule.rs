//! calendar-auto-schedule through the real binary: a calendar app (plain CalDAV requests with a
//! device's app password) organizes an event in a `write_through` Radicale calendar, and
//! Holtmail delivers the invitation through the outbox into GreenMail, where the attendee reads
//! it. The served copy carries `SCHEDULE-STATUS:1.1` and a `Schedule-Tag`, and no ETag comes
//! back because the scheduler rewrote it. The attendee's `REPLY` arriving by mail without a DKIM
//! or DMARC pass stays in the schedule inbox unapplied until "Apply anyway"; then the attendee's
//! `PARTSTAT` lands on the event without changing its `Schedule-Tag`. A client mailing its own
//! copy of the same invitation over SMTP submission is accepted and dropped. An incoming
//! `REQUEST` shows in the schedule inbox and in "From mail", and an invitation sent from one of
//! the user's own addresses (to an own alias) is never taken in.
//!
//! Stalwart as the attendee's server and Apple's CalDAVTester are not part of this suite; see
//! docs/VERIFY.md.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use holtmail_core::TlsRoots;
use holtmail_integration::api::OWNER_EMAIL;
use holtmail_integration::dav::{RADICALE, hrefs_in};
use holtmail_integration::imap::Seeder;
use holtmail_integration::pim::PimRun;
use holtmail_integration::services::{DOVECOT, GREENMAIL, GREENMAIL_SMTP, local};
use holtmail_integration::{Options, ca_file, require_it, state_dir, unique};
use holtmail_submit::{Envelope, Submitter};
use holtmail_submit_smtp::{Secret, Security, SmtpConfig, SmtpSubmitter};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(60);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
        .port()
}

fn unfolded(text: &str) -> String {
    text.replace("\r\n ", "")
}

fn answer_of(text: &str, attendee: &str) -> Option<String> {
    text.lines()
        .filter(|l| l.starts_with("ATTENDEE") && l.contains(&format!("mailto:{attendee}")))
        .find_map(|l| {
            l.split([';', ':'])
                .find_map(|p| p.strip_prefix("PARTSTAT="))
                .map(str::to_owned)
        })
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }
}

struct Client {
    http: reqwest::Client,
    secret: String,
}

impl Client {
    async fn send(&self, method: &str, url: &str, headers: &[(&str, &str)], body: &str) -> Answer {
        let mut request = self
            .http
            .request(
                Method::from_bytes(method.as_bytes()).expect("a method"),
                url,
            )
            .basic_auth(OWNER_EMAIL, Some(&self.secret))
            .body(body.to_owned());
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|e| panic!("{method} {url}: {e}"));
        Answer {
            status: response.status(),
            headers: response.headers().clone(),
            body: response.text().await.unwrap_or_default(),
        }
    }

    async fn propfind(&self, url: &str, depth: &str, props: &str) -> String {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:propfind xmlns:d=\"DAV:\" \
             xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop>{props}</d:prop></d:propfind>"
        );
        let answer = self
            .send(
                "PROPFIND",
                url,
                &[("Depth", depth), ("Content-Type", "application/xml")],
                &body,
            )
            .await;
        assert_eq!(
            answer.status.as_u16(),
            207,
            "PROPFIND {url}: {}",
            answer.body
        );
        answer.body
    }
}

/// Waits until `user`'s GreenMail inbox holds at least `count` messages and returns them.
async fn greenmail(user: &str, count: usize) -> Vec<String> {
    let started = Instant::now();
    loop {
        let mut imap = Seeder::login(&local(GREENMAIL.port), user, GREENMAIL.password).await;
        let bodies = imap.bodies("INBOX").await;
        imap.logout().await;
        if bodies.len() >= count {
            return bodies
                .iter()
                .map(|b| unfolded(&String::from_utf8_lossy(b)))
                .collect();
        }
        assert!(
            started.elapsed() < WAIT,
            "{user} has {} of {count} messages",
            bodies.len()
        );
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

async fn message_with_subject(run: &PimRun, subject: &str) -> String {
    let started = Instant::now();
    loop {
        if let Some(found) = run
            .api
            .messages(&run.archive)
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

fn reply(from: &str, uid: &str, organizer: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: <{from}>\r\nTo: <{organizer}>\r\nSubject: {subject}\r\nDate: Tue, 03 Nov 2026 09:00:00 +0000\r\nMessage-ID: <{}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; charset=utf-8; method=REPLY\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\nDTSTAMP:20261103T090000Z\r\nORGANIZER:mailto:{organizer}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{from}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        unique("reply")
    )
    .into_bytes()
}

fn invitation(from: &str, uid: &str, organizer: &str, attendee: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: <{from}>\r\nTo: <{attendee}>\r\nSubject: {subject}\r\nDate: Mon, 02 Nov 2026 09:00:00 +0000\r\nMessage-ID: <{}@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\nDTSTAMP:20261102T085900Z\r\nDTSTART:20261218T150000Z\r\nDTEND:20261218T160000Z\r\nSUMMARY:{subject}\r\nORGANIZER:mailto:{organizer}\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        unique("invite")
    )
    .into_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_calendar_app_organizes_and_holtmail_schedules() {
    require_it!();
    let user = RADICALE.user("scheduler").await;
    let upstream = RADICALE.client(&user);
    let homes = RADICALE.homes(&upstream).await;
    upstream
        .create_calendar(&homes.calendars, "team", "Team")
        .await;
    let bob = format!("{}@example.org", unique("bob"));
    let carol = format!("{}@example.org", unique("carol"));
    let title = unique("Review");

    let (port, submit_port) = (free_port(), free_port());
    let base = format!("http://127.0.0.1:{port}");
    let state = state_dir();
    let mut run = PimRun::start_with(
        "Scheduling",
        Options {
            port: Some(port),
            extra_config: format!(
                "public_url = \"{base}\"\n[write]\nundo_seconds = 1\n\
                 [serve]\ntls_cert = {cert:?}\ntls_key = {key:?}\n\
                 [serve.dav]\n\
                 [serve.submit]\nlisten_starttls = [\"127.0.0.1:{submit_port}\"]\nhostname = \"localhost\"\n",
                cert = state.join("server.crt"),
                key = state.join("server.key"),
            ),
            ..Options::default()
        },
    )
    .await;
    run.secrets("an item", &[bob.as_str(), carol.as_str(), title.as_str()]);
    let account = run.add_dav(&RADICALE, &user, None).await;
    run.api
        .request(
            Method::PUT,
            &format!("/api/accounts/{account}/capabilities"),
            Some(&json!({ "capabilities": ["pim.edit", "pim.respond"] })),
        )
        .await
        .ok();
    let transport = run
        .api
        .post(
            &format!("/api/archives/{}/transports", run.archive),
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
    run.api
        .post(
            &format!("/api/archives/{}/identities", run.archive),
            json!({
                "address": user,
                "name": "Ada Example",
                "transport": transport["id"],
                "active": true,
            }),
        )
        .await
        .ok();
    let team_id = run.collections().await["Team"]["id"]
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
    let made = run
        .api
        .post(
            "/api/devices",
            json!({
                "name": "Calendar app",
                "protocols": ["dav", "smtp"],
                "scopes": ["read", "write:pim", "send:mail"],
                "mail_archive": run.archive,
            }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let secret = made.body["secret"].as_str().expect("a secret").to_owned();
    run.secrets("an app password", &[secret.as_str()]);
    let client = Client {
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("a client"),
        secret: secret.clone(),
    };

    let options = client
        .send("OPTIONS", &format!("{base}/dav/"), &[], "")
        .await;
    let dav_header = options.header("dav").unwrap_or_default();
    assert!(
        dav_header.contains("calendar-auto-schedule"),
        "{dav_header}"
    );
    let root = client
        .propfind(&format!("{base}/dav/"), "0", "<d:current-user-principal/>")
        .await;
    let principal = hrefs_in(&root, "current-user-principal")
        .first()
        .cloned()
        .expect("a principal");
    let found = client
        .propfind(
            &format!("{base}{principal}"),
            "0",
            "<c:schedule-inbox-URL/><c:schedule-outbox-URL/><c:calendar-user-address-set/>",
        )
        .await;
    assert!(found.contains(&format!("mailto:{user}")), "{found}");
    let inbox = hrefs_in(&found, "schedule-inbox-URL")
        .first()
        .cloned()
        .expect("an inbox");
    let user_id = principal
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .expect("a user id")
        .to_owned();
    let calendar = format!("{base}/dav/calendars/{user_id}/{team_id}/");

    let uid = format!("{}@example.com", unique("review"));
    let event = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//macOS//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261101T080000Z\r\nSEQUENCE:0\r\nSUMMARY:{title}\r\nDTSTART:20261216T100000Z\r\nDTEND:20261216T110000Z\r\nORGANIZER;CN=Ada Example:mailto:{user}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{user}\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{bob}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let url = format!("{calendar}review.ics");
    let created = client
        .send(
            "PUT",
            &url,
            &[("Content-Type", "text/calendar"), ("If-None-Match", "*")],
            &event,
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        created.header("etag"),
        None,
        "the scheduler rewrote the event"
    );
    assert!(created.header("schedule-tag").is_some());
    let request = greenmail(&bob, 1).await;
    assert!(request[0].contains("METHOD:REQUEST"), "{}", request[0]);
    assert!(request[0].contains(&format!("UID:{uid}")), "{}", request[0]);
    let served = unfolded(&client.send("GET", &url, &[], "").await.body);
    assert!(
        served.contains(&format!("SCHEDULE-STATUS=1.1:mailto:{bob}")),
        "{served}"
    );
    settled(&run, &team_id).await;

    let pem = std::fs::read(ca_file()).expect("the stack's CA");
    let submitter = SmtpSubmitter::new(SmtpConfig {
        port: submit_port,
        security: Security::Starttls,
        username: Some(OWNER_EMAIL.to_owned()),
        secret: Some(Secret::Password(secret.clone())),
        helo: "client.example.com".into(),
        timeout: Duration::from_secs(20),
        ..SmtpConfig::new("localhost")
    })
    .expect("a client")
    .with_roots(&TlsRoots::from_pem(&pem).expect("a CA"))
    .expect("a CA");
    let outbox_before = run
        .api
        .get(&format!("/api/archives/{}/outbox", run.archive))
        .await
        .ok();
    let own_copy = format!(
        "From: Ada Example <{user}>\r\nTo: <{bob}>\r\nSubject: Invitation: {title}\r\nDate: Mon, 02 Nov 2026 10:00:00 +0000\r\nMessage-ID: <{}@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Mozilla//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSEQUENCE:0\r\nDTSTAMP:20261102T100000Z\r\nDTSTART:20261216T100000Z\r\nORGANIZER:mailto:{user}\r\nATTENDEE:mailto:{bob}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        unique("client-imip")
    );
    submitter
        .submit(
            &Envelope {
                mail_from: user.clone(),
                recipients: vec![bob.clone()],
            },
            own_copy.as_bytes(),
        )
        .await
        .expect("accepted");
    let outbox_after = run
        .api
        .get(&format!("/api/archives/{}/outbox", run.archive))
        .await
        .ok();
    assert_eq!(
        outbox_after.as_array().map(Vec::len),
        outbox_before.as_array().map(Vec::len),
        "the client's own iMIP copy is dropped: {outbox_after}"
    );

    let tag = client
        .send("GET", &url, &[], "")
        .await
        .header("schedule-tag")
        .expect("a schedule tag");
    let answer_subject = unique("Accepted");
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append(
            "INBOX",
            &[],
            None,
            &reply(&bob, &uid, &user, &answer_subject),
        )
        .await;
    seeder.logout().await;
    run.api.sync(&run.archive, &account).await;
    let message = message_with_subject(&run, &answer_subject).await;
    let started = Instant::now();
    let detail = loop {
        let detail = run.api.get(&format!("/api/messages/{message}")).await.ok();
        if detail["invitations"][0]["schedule_note"].is_string() {
            break detail;
        }
        assert!(started.elapsed() < WAIT, "{detail}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(
        detail["invitations"][0]["schedule_note"], "unauthenticated",
        "{detail}"
    );
    let listed = client
        .propfind(&format!("{base}{inbox}"), "1", "<d:getetag/>")
        .await;
    assert!(listed.contains(&message), "{listed}");
    let still = unfolded(&client.send("GET", &url, &[], "").await.body);
    assert_ne!(
        answer_of(&still, &bob).as_deref(),
        Some("ACCEPTED"),
        "an unverified answer is not applied: {still}"
    );

    let applied = run
        .api
        .post(&format!("/api/messages/{message}/itip/apply"), json!({}))
        .await
        .ok();
    assert_eq!(applied["notes"], json!(["applied"]), "{applied}");
    let merged = client.send("GET", &url, &[], "").await;
    assert_eq!(
        answer_of(&unfolded(&merged.body), &bob).as_deref(),
        Some("ACCEPTED"),
        "{}",
        merged.body
    );
    assert_eq!(
        merged.header("schedule-tag").as_deref(),
        Some(tag.as_str()),
        "a merged answer keeps the Schedule-Tag"
    );
    settled(&run, &team_id).await;

    let walk = unique("Walk");
    let walk_uid = format!("{}@example.org", unique("walk"));
    let own_alias = unique("Own alias");
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append(
            "INBOX",
            &[],
            None,
            &invitation(&carol, &walk_uid, &carol, &user, &walk),
        )
        .await;
    seeder
        .append(
            "INBOX",
            &[],
            None,
            &invitation(&user, &unique("alias-event"), &user, &user, &own_alias),
        )
        .await;
    seeder.logout().await;
    run.api.sync(&run.archive, &account).await;
    let walk_message = message_with_subject(&run, &walk).await;
    let own_message = message_with_subject(&run, &own_alias).await;
    let started = Instant::now();
    loop {
        let listed = client
            .propfind(&format!("{base}{inbox}"), "1", "<d:getetag/>")
            .await;
        if listed.contains(&walk_message) {
            assert!(
                !listed.contains(&own_message),
                "an invitation from an own address is never taken in: {listed}"
            );
            break;
        }
        assert!(started.elapsed() < WAIT, "{listed}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let home = client
        .propfind(
            &format!("{base}/dav/calendars/{user_id}/"),
            "1",
            "<d:displayname/>",
        )
        .await;
    let from_mail = home
        .split("<d:response>")
        .find(|r| r.contains("From mail"))
        .and_then(|r| {
            let start = r.find("<d:href>")? + 8;
            let end = r[start..].find("</d:href>")? + start;
            Some(r[start..end].to_owned())
        })
        .expect("the From mail calendar");
    let listing = client
        .propfind(&format!("{base}{from_mail}"), "1", "<c:calendar-data/>")
        .await;
    assert!(listing.contains(&walk_uid), "{listing}");
    run.check_logs();
}
