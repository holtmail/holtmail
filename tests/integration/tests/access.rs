//! Who may do what, route by route: every operation of the OpenAPI description (read from
//! `holtmail openapi`, so a new route without an expectation fails this test) asked by the owner,
//! an admin, a member for an archive granted to them and for one that is not, and without a
//! session, against an instance with mail, attachments, remote images, threads, contacts, events,
//! people and overrides in both archives. Each answer must be the expected one and one the
//! OpenAPI description declares for that operation.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use base64::Engine;
use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD, id};
use holtmail_integration::data::{Message, mbox, vcard};
use holtmail_integration::images::{self, Images, SOCKS_PORT, Served};
use holtmail_integration::services::{DOVECOT, DOVECOT_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, stalwart, unique};
use holtmail_sanitize::remote_key;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const ADMIN_EMAIL: &str = "admin-access@example.com";
const MEMBER_EMAIL: &str = "member-access@example.com";
const PASSWORD: &str = "it-access-password-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    Owner,
    Admin,
    Granted,
    Ungranted,
    Anonymous,
}

const EVERYONE: [Who; 5] = [
    Who::Owner,
    Who::Admin,
    Who::Granted,
    Who::Ungranted,
    Who::Anonymous,
];

impl Who {
    fn credentials(self) -> Option<(&'static str, &'static str)> {
        match self {
            Who::Owner => Some((OWNER_EMAIL, OWNER_PASSWORD)),
            Who::Admin => Some((ADMIN_EMAIL, PASSWORD)),
            Who::Granted | Who::Ungranted => Some((MEMBER_EMAIL, PASSWORD)),
            Who::Anonymous => None,
        }
    }
}

enum Body {
    Empty,
    /// No body and no content type, as `DELETE`s go.
    NoneAsJson,
    Json(Value),
    Octet(Vec<u8>),
}

/// A client that keeps its session, sends the instance's `Origin` and follows no redirects.
struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    async fn new(base: &str, who: Who) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_str(base).expect("origin"));
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .expect("client");
        let client = Self {
            base: base.to_owned(),
            http,
        };
        if let Some((email, password)) = who.credentials() {
            let (status, body) = client
                .send(
                    Method::POST,
                    "/api/login",
                    Body::Json(json!({ "email": email, "password": password })),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{who:?} signs in: {body}");
        }
        client
    }

    async fn send(&self, method: Method, path: &str, body: Body) -> (StatusCode, Value) {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        request = match body {
            Body::Empty => request,
            Body::NoneAsJson => request,
            Body::Json(value) => request
                .header(CONTENT_TYPE, "application/json")
                .body(value.to_string()),
            Body::Octet(bytes) => request
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(bytes),
        };
        let response = request.send().await.expect("the instance answers");
        let status = response.status();
        let bytes = response.bytes().await.expect("a body");
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(format!("{} bytes", bytes.len()))),
        )
    }
}

/// The ids one archive's routes are asked with.
#[derive(Debug, Clone, Default)]
struct Seeded {
    archive: String,
    message: String,
    ordinal: u64,
    remote: String,
    thread: String,
    contact: String,
    event_item: String,
    event_group: String,
    person: String,
    split: BTreeMap<&'static str, String>,
    join: BTreeMap<&'static str, Vec<String>>,
    undo: BTreeMap<&'static str, Vec<String>>,
    import_account: String,
    imap_account: String,
    subjects: Vec<String>,
}

struct World {
    holtmail: Holtmail,
    owner: Api,
    granted: Seeded,
    other: Seeded,
    users: BTreeMap<String, String>,
    dovecot_users: Vec<String>,
}

fn multipart(message: Message, image_url: &str, png: &[u8]) -> Message {
    let boundary = "holtmail-access-boundary";
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).expect("base64"))
        .collect();
    let body = format!(
        "--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{}\r\n--{boundary}\r\n\
         Content-Type: image/png\r\nContent-Disposition: attachment; filename=\"chart.png\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n",
        images::html(&[image_url.to_owned()]),
        lines.join("\r\n")
    );
    let mut message = message.body(&body);
    message.content_type = format!("multipart/mixed; boundary=\"{boundary}\"");
    message
}

fn card(uid: &str, name: &str, email: &str) -> String {
    vcard(uid, name, email, None)
}

const PEOPLE: [(&str, &str); 12] = [
    ("split-owner-1", "Split Owner"),
    ("split-owner-2", "Split Owner"),
    ("split-admin-1", "Split Admin"),
    ("split-admin-2", "Split Admin"),
    ("join-owner-a", "Join Owner A"),
    ("join-owner-b", "Join Owner B"),
    ("join-admin-a", "Join Admin A"),
    ("join-admin-b", "Join Admin B"),
    ("undo-owner-a", "Undo Owner A"),
    ("undo-owner-b", "Undo Owner B"),
    ("undo-admin-a", "Undo Admin A"),
    ("undo-admin-b", "Undo Admin B"),
];

fn contacts_file(tag: &str) -> String {
    PEOPLE
        .iter()
        .map(|(uid, name)| {
            let mailbox = uid.trim_end_matches(|c: char| c == '-' || c.is_ascii_digit());
            card(
                &format!("{tag}-{uid}"),
                name,
                &format!("{mailbox}-{tag}@example.org"),
            )
        })
        .collect()
}

fn calendar_file(tag: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Integration tests//EN\r\n\
         BEGIN:VEVENT\r\nUID:access-{tag}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260310T090000Z\r\n\
         DTEND:20260310T100000Z\r\nSUMMARY:Access review {tag}\r\n\
         ORGANIZER:mailto:split-owner-{tag}@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

async fn wait_people(api: &Api, archive: &str, cards: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let people = api.people(archive).await;
        let with_cards: usize = people
            .iter()
            .map(|p| p["cards"].as_u64().unwrap_or(0) as usize)
            .sum();
        if with_cards >= cards {
            return people;
        }
        assert!(
            Instant::now() < deadline,
            "people were not built: {people:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn seed(owner: &Api, name: &str, tag: &str, images: &Images) -> (Seeded, String) {
    let archive = owner.create_archive(name).await;
    owner
        .patch(
            &format!("/api/archives/{archive}"),
            json!({ "save_remote_images": true }),
        )
        .await
        .ok();
    let png = images::png(images::seed(tag));
    let served = images.put(Served::Plain, &format!("{tag}.png"), &png);
    images.wait_served(&served).await;
    let url = Images::by_name(&served);
    let to = format!("viewer-{tag}@example.com");
    let first = multipart(Message::new(&to), &url, &png);
    let reply = Message::new(&to)
        .subject(&format!("Re: {}", first.subject))
        .header(&format!("In-Reply-To: <{}>", first.message_id))
        .header(&format!("References: <{}>", first.message_id));
    let (import_account, task) = owner
        .import(&archive, "mail.mbox", mbox(&[first.clone(), reply.clone()]))
        .await;
    assert_eq!(task["state"], "done", "{task}");
    let (_, task) = owner
        .import(&archive, "contacts.vcf", contacts_file(tag).into_bytes())
        .await;
    assert_eq!(task["state"], "done", "{task}");
    let (_, task) = owner
        .import(&archive, "calendar.ics", calendar_file(tag).into_bytes())
        .await;
    assert_eq!(task["state"], "done", "{task}");

    let messages = owner.messages(&archive).await;
    let message = messages
        .iter()
        .find(|m| m["subject"] == first.subject.as_str())
        .map(id)
        .expect("the first message");
    let detail = owner.message(&message).await;
    let ordinal = detail["attachments"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["content_type"] == "image/png"))
        .and_then(|a| a["ordinal"].as_u64())
        .unwrap_or_else(|| panic!("no PNG attachment: {detail}"));
    let thread = detail["thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no thread: {detail}"))
        .to_owned();

    let people = wait_people(owner, &archive, PEOPLE.len()).await;
    let person = |name: &str| -> String {
        people
            .iter()
            .find(|p| p["name"] == name)
            .map(id)
            .unwrap_or_else(|| panic!("no person {name}: {people:?}"))
    };
    let mut split = BTreeMap::new();
    split.insert("owner", person("Split Owner"));
    split.insert("admin", person("Split Admin"));
    let mut join = BTreeMap::new();
    join.insert(
        "owner",
        vec![person("Join Owner A"), person("Join Owner B")],
    );
    join.insert(
        "admin",
        vec![person("Join Admin A"), person("Join Admin B")],
    );
    let mut undo = BTreeMap::new();
    undo.insert(
        "owner",
        vec![person("Undo Owner A"), person("Undo Owner B")],
    );
    undo.insert(
        "admin",
        vec![person("Undo Admin A"), person("Undo Admin B")],
    );

    let contact = owner
        .items(&archive, "contacts")
        .await
        .first()
        .map(id)
        .expect("a contact");
    let event_item = owner
        .items(&archive, "events")
        .await
        .first()
        .map(id)
        .expect("an event");
    let calendar = owner
        .get(&format!(
            "/api/archives/{archive}/calendar?from=1772323200000&to=1775001600000"
        ))
        .await
        .ok();
    let event_group = calendar["occurrences"][0]["group_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no occurrence: {calendar}"))
        .to_owned();

    let user = DOVECOT.user("access");
    let seeder = DOVECOT.seeder(&user).await;
    seeder.logout().await;
    let imap_account = owner.add_imap(&archive, DOVECOT.account(&user)).await;
    owner.settle(&archive, &imap_account).await;

    (
        Seeded {
            archive,
            message,
            ordinal,
            remote: remote_key(&url),
            thread,
            contact,
            event_item,
            event_group,
            person: split["owner"].clone(),
            split,
            join,
            undo,
            import_account,
            imap_account,
            subjects: vec![first.subject, reply.subject],
        },
        user,
    )
}

async fn world() -> World {
    let holtmail = Holtmail::start(Options {
        sync_interval_minutes: None,
        ..Default::default()
    })
    .await;
    let owner = Api::setup_owner(&holtmail).await;
    owner
        .request(
            Method::PUT,
            "/api/image-proxy",
            Some(&json!({ "proxy": format!("socks5h://127.0.0.1:{SOCKS_PORT}") })),
        )
        .await
        .ok();
    let images = Images::new("access");
    let (granted, user_a) = seed(&owner, "Granted", &unique("a"), &images).await;
    let (other, user_b) = seed(&owner, "Not granted", &unique("b"), &images).await;
    let mut users = BTreeMap::new();
    for (email, role, archives) in [
        (ADMIN_EMAIL, "admin", vec![]),
        (MEMBER_EMAIL, "member", vec![granted.archive.clone()]),
    ] {
        let created = owner
            .post(
                "/api/users",
                json!({ "email": email, "name": "Access Example", "password": PASSWORD, "role": role, "archives": archives }),
            )
            .await
            .ok();
        users.insert(email.to_owned(), id(&created["user"]));
    }
    World {
        holtmail,
        owner,
        granted,
        other,
        users,
        dovecot_users: vec![user_a, user_b],
    }
}

fn mbox_upload() -> Vec<u8> {
    mbox(&[Message::new("viewer@example.com")])
}

impl World {
    fn ids(&self, who: Who) -> &Seeded {
        if who == Who::Ungranted {
            &self.other
        } else {
            &self.granted
        }
    }

    fn role(who: Who) -> &'static str {
        match who {
            Who::Admin => "admin",
            _ => "owner",
        }
    }

    /// The path and body `who` asks `template` with, creating what a destructive request needs.
    async fn prepare(&self, method: &Method, template: &str, who: Who) -> (String, Body) {
        let s = self.ids(who);
        let a = &s.archive;
        let m = &s.message;
        let fill = |t: &str| {
            t.replace("{archive}", a)
                .replace("{message}", m)
                .replace("{ordinal}", &s.ordinal.to_string())
                .replace("{remote}", &s.remote)
                .replace("{thread}", &s.thread)
                .replace("{contact}", &s.contact)
                .replace("{event_item}", &s.event_item)
                .replace("{event_group}", &s.event_group)
                .replace("{person}", &s.person)
        };
        let manages = matches!(who, Who::Owner | Who::Admin);
        match (method.as_str(), template) {
            ("GET", "/api/archives/{id}/calendar") => (
                fill("/api/archives/{archive}/calendar?from=1772323200000&to=1775001600000"),
                Body::Empty,
            ),
            ("GET", "/api/archives/{id}/calendar/export") => (
                fill("/api/archives/{archive}/calendar/export?format=ics"),
                Body::Empty,
            ),
            ("GET", "/api/archives/{id}/contacts/export") => (
                fill("/api/archives/{archive}/contacts/export?format=vcf"),
                Body::Empty,
            ),
            ("GET", "/api/archives/{id}/export") => (
                fill("/api/archives/{archive}/export?format=mbox"),
                Body::Empty,
            ),
            ("GET", "/api/archives/{id}/search") => (
                fill("/api/archives/{archive}/search?q=invented"),
                Body::Empty,
            ),
            ("GET", "/api/archives/{id}/search/items") => (
                fill("/api/archives/{archive}/search/items?q=Access"),
                Body::Empty,
            ),
            ("GET", t) if t.starts_with("/api/archives/{id}") => {
                (t.replace("{id}", a), Body::Empty)
            }
            ("GET", "/api/messages/{id}/attachments/{ordinal}") => (
                fill("/api/messages/{message}/attachments/{ordinal}"),
                Body::Empty,
            ),
            ("GET", "/api/messages/{id}/attachments/{ordinal}/thumbnail") => (
                fill("/api/messages/{message}/attachments/{ordinal}/thumbnail"),
                Body::Empty,
            ),
            ("GET", "/api/messages/{id}/live/{key}") => {
                (fill("/api/messages/{message}/live/{remote}"), Body::Empty)
            }
            ("GET", "/api/messages/{id}/remote/{key}") => {
                (fill("/api/messages/{message}/remote/{remote}"), Body::Empty)
            }
            ("GET", t) if t.starts_with("/api/messages/{id}") => {
                (t.replace("{id}", m), Body::Empty)
            }
            ("GET", "/api/threads/{id}") => (fill("/api/threads/{thread}"), Body::Empty),
            ("GET", "/api/items/{id}") => (fill("/api/items/{contact}"), Body::Empty),
            ("GET", "/api/items/{id}/versions/{number}/raw") => {
                (fill("/api/items/{event_item}/versions/1/raw"), Body::Empty)
            }
            ("GET", "/api/events/{id}") => (fill("/api/events/{event_group}"), Body::Empty),
            ("GET", "/api/people/{id}") => (fill("/api/people/{person}"), Body::Empty),
            ("GET", "/api/oauth/callback") => (
                "/api/oauth/callback?state=unknown&code=unknown".to_owned(),
                Body::Empty,
            ),
            ("GET", "/api/webhooks/{id}/deliveries") => (
                format!(
                    "/api/webhooks/{}/deliveries",
                    self.throwaway_webhook().await
                ),
                Body::Empty,
            ),
            ("GET", t) => (t.to_owned(), Body::Empty),
            ("POST", "/api/setup") => (
                "/api/setup".to_owned(),
                Body::Json(
                    json!({ "token": "not-the-token", "email": "late@example.com", "name": "Late Example", "password": "late-password-1" }),
                ),
            ),
            ("POST", "/api/login") => {
                let (email, password) = who
                    .credentials()
                    .unwrap_or((OWNER_EMAIL, "it-wrong-password-1"));
                (
                    "/api/login".to_owned(),
                    Body::Json(json!({ "email": email, "password": password })),
                )
            }
            ("POST", "/api/logout") => ("/api/logout".to_owned(), Body::Json(json!({}))),
            ("POST", "/api/archives") => (
                "/api/archives".to_owned(),
                Body::Json(json!({ "name": unique("archive") })),
            ),
            ("PATCH", "/api/archives/{id}") => (
                format!("/api/archives/{a}"),
                Body::Json(json!({ "name": format!("Renamed {}", unique("x")) })),
            ),
            ("DELETE", "/api/archives/{id}") => {
                let doomed = if manages {
                    self.owner.create_archive(&unique("doomed")).await
                } else {
                    a.clone()
                };
                (format!("/api/archives/{doomed}"), Body::NoneAsJson)
            }
            ("POST", "/api/archives/{id}/accounts") => {
                let user = DOVECOT.user("access-add");
                (
                    format!("/api/archives/{a}/accounts"),
                    Body::Json(DOVECOT.account(&user)),
                )
            }
            ("POST", "/api/archives/{id}/accounts/jmap") => {
                let user = format!("{}@example.com", unique("access-jmap"));
                if manages {
                    stalwart::create_user(&user).await;
                }
                (
                    format!("/api/archives/{a}/accounts/jmap"),
                    Body::Json(json!({
                        "url": stalwart::base_url(),
                        "username": user,
                        "password": stalwart::STALWART_PASSWORD,
                    })),
                )
            }
            ("POST", "/api/archives/{id}/imports") => (
                format!("/api/archives/{a}/imports?name={}.mbox", unique("upload")),
                Body::Octet(mbox_upload()),
            ),
            ("POST", "/api/archives/{id}/remote-images") => (
                format!("/api/archives/{a}/remote-images"),
                Body::Json(json!({ "job": "failed" })),
            ),
            ("PUT", "/api/archives/{id}/category-overrides") => (
                format!("/api/archives/{a}/category-overrides"),
                Body::Json(json!({ "scope": "domain", "key": "example.com", "kind": "bulk" })),
            ),
            ("DELETE", "/api/archives/{id}/category-overrides/{override}") if !manages => (
                format!(
                    "/api/archives/{a}/category-overrides/{}",
                    uuid::Uuid::now_v7()
                ),
                Body::NoneAsJson,
            ),
            ("DELETE", "/api/archives/{id}/category-overrides/{override}") => {
                let made = self
                    .owner
                    .request(
                        Method::PUT,
                        &format!("/api/archives/{a}/category-overrides"),
                        Some(&json!({
                            "scope": "address",
                            "key": format!("{}@example.org", unique("sender")),
                            "topics": ["travel"],
                        })),
                    )
                    .await
                    .ok();
                (
                    format!("/api/archives/{a}/category-overrides/{}", id(&made)),
                    Body::NoneAsJson,
                )
            }
            ("POST", "/api/messages/{id}/unsubscribe") => (
                format!("/api/messages/{m}/unsubscribe"),
                Body::Json(json!({ "confirm": true })),
            ),
            ("PATCH", "/api/accounts/{id}") => (
                format!("/api/accounts/{}", s.import_account),
                Body::Json(json!({ "name": "Imported mail" })),
            ),
            ("DELETE", "/api/accounts/{id}") => {
                let account = if manages {
                    let (account, task) = self.owner.import(a, "doomed.mbox", mbox_upload()).await;
                    assert_eq!(task["state"], "done", "{task}");
                    account
                } else {
                    s.import_account.clone()
                };
                (format!("/api/accounts/{account}"), Body::NoneAsJson)
            }
            ("POST", "/api/accounts/{id}/sync") => {
                if manages {
                    self.owner.settle(a, &s.imap_account).await;
                }
                (
                    format!("/api/accounts/{}/sync", s.imap_account),
                    Body::Json(json!({})),
                )
            }
            ("POST", "/api/oauth/start") => (
                "/api/oauth/start".to_owned(),
                Body::Json(
                    json!({ "provider": "google", "email": "someone@example.com", "archive_id": a }),
                ),
            ),
            ("POST", "/api/oauth/finish") => (
                "/api/oauth/finish".to_owned(),
                Body::Json(
                    json!({ "pasted_url": "https://example.com/?code=unknown&state=unknown" }),
                ),
            ),
            ("POST", "/api/users") => (
                "/api/users".to_owned(),
                Body::Json(json!({
                    "email": format!("{}@example.com", unique("access-user")),
                    "name": "Added Example",
                    "password": PASSWORD,
                })),
            ),
            ("PATCH", "/api/users/{id}") => {
                let target = self.throwaway_user().await;
                (
                    format!("/api/users/{target}"),
                    Body::Json(json!({ "name": "Changed Example" })),
                )
            }
            ("DELETE", "/api/users/{id}") => {
                let target = self.throwaway_user().await;
                (format!("/api/users/{target}"), Body::NoneAsJson)
            }
            ("PATCH", "/api/me") => (
                "/api/me".to_owned(),
                Body::Json(json!({ "name": "Access Example" })),
            ),
            ("POST", "/api/me/password") => {
                let current = who.credentials().map(|c| c.1).unwrap_or(PASSWORD);
                (
                    "/api/me/password".to_owned(),
                    Body::Json(json!({ "current": current, "new": current })),
                )
            }
            ("PUT", "/api/image-proxy") => (
                "/api/image-proxy".to_owned(),
                Body::Json(json!({ "proxy": format!("socks5h://127.0.0.1:{SOCKS_PORT}") })),
            ),
            ("PUT", "/api/sync-schedule") => (
                "/api/sync-schedule".to_owned(),
                Body::Json(json!({ "minutes": 0 })),
            ),
            ("POST", "/api/tasks/{id}/cancel") => {
                let task = self
                    .owner
                    .tasks()
                    .await
                    .into_iter()
                    .find(|t| t["state"] == "done")
                    .map(|t| id(&t))
                    .expect("a finished task");
                (format!("/api/tasks/{task}/cancel"), Body::Json(json!({})))
            }
            ("POST", "/api/people/{id}/split") if !manages => (
                fill("/api/people/{person}/split"),
                Body::Json(json!({ "cards": [s.contact] })),
            ),
            ("POST", "/api/people/{id}/split") => {
                let person = s.split.get(Self::role(who)).expect("a person to split");
                let detail = self.owner.get(&format!("/api/people/{person}")).await.ok();
                let card = detail["cards"][0]["item_id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("no card: {detail}"))
                    .to_owned();
                (
                    format!("/api/people/{person}/split"),
                    Body::Json(json!({ "cards": [card] })),
                )
            }
            ("POST", "/api/people/join") => (
                "/api/people/join".to_owned(),
                Body::Json(json!({ "people": s.join[Self::role(who)] })),
            ),
            ("DELETE", "/api/people/overrides/{id}") if !manages => (
                format!("/api/people/overrides/{}", uuid::Uuid::now_v7()),
                Body::NoneAsJson,
            ),
            ("DELETE", "/api/people/overrides/{id}") => {
                let pair = &s.undo[Self::role(who)];
                let joined = self
                    .owner
                    .post("/api/people/join", json!({ "people": pair }))
                    .await
                    .ok();
                (
                    format!("/api/people/overrides/{}", id(&joined)),
                    Body::NoneAsJson,
                )
            }
            ("POST", "/api/ledger/verify") => {
                ("/api/ledger/verify".to_owned(), Body::Json(json!({})))
            }
            ("POST", "/api/archives/{id}/deletions") => (
                fill("/api/archives/{archive}/deletions"),
                Body::Json(json!({ "message_ids": [m] })),
            ),
            ("POST", "/api/deletions/{id}/confirm") => (
                format!("/api/deletions/{}/confirm", uuid::Uuid::now_v7().simple()),
                Body::Json(json!({ "count": 1 })),
            ),
            ("POST", "/api/webhooks") => (
                "/api/webhooks".to_owned(),
                Body::Json(json!({ "url": "https://hooks.example.com/access", "events": [] })),
            ),
            ("PATCH", "/api/webhooks/{id}") => (
                format!("/api/webhooks/{}", self.throwaway_webhook().await),
                Body::Json(json!({ "enabled": false })),
            ),
            ("DELETE", "/api/webhooks/{id}") => (
                format!("/api/webhooks/{}", self.throwaway_webhook().await),
                Body::NoneAsJson,
            ),
            ("POST", "/api/webhooks/{id}/secret") => (
                format!("/api/webhooks/{}/secret", self.throwaway_webhook().await),
                Body::Json(json!({})),
            ),
            ("POST", "/api/webhooks/{id}/test") => (
                format!("/api/webhooks/{}/test", self.throwaway_webhook().await),
                Body::Json(json!({})),
            ),
            (method, template) => panic!("no request for {method} {template}"),
        }
    }

    async fn throwaway_webhook(&self) -> String {
        let created = self
            .owner
            .post(
                "/api/webhooks",
                json!({ "url": "https://hooks.example.com/target", "events": [] }),
            )
            .await
            .ok();
        id(&created["webhook"])
    }

    async fn throwaway_user(&self) -> String {
        let created = self
            .owner
            .post(
                "/api/users",
                json!({
                    "email": format!("{}@example.com", unique("access-target")),
                    "name": "Target Example",
                    "password": PASSWORD,
                }),
            )
            .await
            .ok();
        id(&created["user"])
    }
}

/// Owner, admin, member with the archive granted, member without it, anonymous.
type Expected = [u16; 5];

const READ: Expected = [200, 200, 200, 404, 401];
const MANAGE_READ: Expected = [200, 200, 403, 403, 401];
const PUBLIC: Expected = [200, 200, 200, 200, 200];

fn expectations() -> BTreeMap<(String, String), Expected> {
    let rows: &[(&str, &str, Expected)] = &[
        ("GET", "/api/session", [200, 200, 200, 200, 401]),
        ("GET", "/api/setup", PUBLIC),
        ("POST", "/api/setup", [403, 403, 403, 403, 403]),
        ("POST", "/api/login", [200, 200, 200, 200, 401]),
        ("POST", "/api/logout", [204, 204, 204, 204, 204]),
        ("GET", "/api/oauth/callback", [303, 303, 303, 303, 303]),
        ("GET", "/api/archives", [200, 200, 200, 200, 401]),
        ("POST", "/api/archives", [201, 201, 403, 403, 401]),
        ("GET", "/api/archives/{id}", READ),
        ("PATCH", "/api/archives/{id}", [200, 200, 403, 403, 401]),
        ("DELETE", "/api/archives/{id}", [204, 204, 403, 403, 401]),
        ("GET", "/api/archives/{id}/accounts", READ),
        (
            "POST",
            "/api/archives/{id}/accounts",
            [201, 201, 403, 403, 401],
        ),
        (
            "POST",
            "/api/archives/{id}/accounts/jmap",
            [201, 201, 403, 403, 401],
        ),
        ("GET", "/api/archives/{id}/calendar", READ),
        ("GET", "/api/archives/{id}/calendar/export", READ),
        ("GET", "/api/archives/{id}/collections", READ),
        ("GET", "/api/archives/{id}/contacts", READ),
        ("GET", "/api/archives/{id}/contacts/export", READ),
        ("GET", "/api/archives/{id}/events", READ),
        ("GET", "/api/archives/{id}/export", READ),
        ("GET", "/api/archives/{id}/folders", READ),
        (
            "POST",
            "/api/archives/{id}/imports",
            [202, 202, 403, 403, 401],
        ),
        ("GET", "/api/archives/{id}/insights/status", READ),
        ("GET", "/api/archives/{id}/categories", READ),
        ("GET", "/api/archives/{id}/category-overrides", READ),
        (
            "PUT",
            "/api/archives/{id}/category-overrides",
            [200, 200, 403, 403, 401],
        ),
        (
            "DELETE",
            "/api/archives/{id}/category-overrides/{override}",
            [204, 204, 403, 403, 401],
        ),
        ("GET", "/api/archives/{id}/labels", READ),
        ("GET", "/api/archives/{id}/messages", READ),
        ("GET", "/api/archives/{id}/people", READ),
        ("GET", "/api/archives/{id}/people/overrides", READ),
        (
            "POST",
            "/api/archives/{id}/remote-images",
            [202, 202, 403, 403, 401],
        ),
        ("GET", "/api/archives/{id}/search", READ),
        ("GET", "/api/archives/{id}/search/items", READ),
        ("PATCH", "/api/accounts/{id}", [200, 200, 403, 403, 401]),
        ("DELETE", "/api/accounts/{id}", [204, 204, 403, 403, 401]),
        ("POST", "/api/accounts/{id}/sync", [202, 202, 403, 403, 401]),
        ("GET", "/api/events/{id}", READ),
        ("GET", "/api/image-proxy", MANAGE_READ),
        ("PUT", "/api/image-proxy", MANAGE_READ),
        ("GET", "/api/insights/data", [200, 200, 200, 200, 401]),
        ("GET", "/api/items/{id}", READ),
        ("GET", "/api/items/{id}/versions/{number}/raw", READ),
        ("PATCH", "/api/me", [200, 200, 200, 200, 401]),
        ("POST", "/api/me/password", [200, 200, 200, 200, 401]),
        ("GET", "/api/messages/{id}", READ),
        ("GET", "/api/messages/{id}/attachments/{ordinal}", READ),
        (
            "GET",
            "/api/messages/{id}/attachments/{ordinal}/thumbnail",
            READ,
        ),
        ("GET", "/api/messages/{id}/body", READ),
        ("GET", "/api/messages/{id}/live/{key}", READ),
        ("GET", "/api/messages/{id}/raw", READ),
        ("GET", "/api/messages/{id}/remote/{key}", READ),
        (
            "POST",
            "/api/messages/{id}/unsubscribe",
            [400, 400, 403, 403, 401],
        ),
        ("POST", "/api/oauth/start", [200, 200, 403, 403, 401]),
        ("POST", "/api/oauth/finish", [400, 400, 403, 403, 401]),
        ("POST", "/api/people/join", [201, 201, 403, 403, 401]),
        (
            "DELETE",
            "/api/people/overrides/{id}",
            [200, 200, 403, 403, 401],
        ),
        ("GET", "/api/people/{id}", READ),
        ("POST", "/api/people/{id}/split", [201, 201, 403, 403, 401]),
        ("GET", "/api/sync-schedule", MANAGE_READ),
        ("PUT", "/api/sync-schedule", MANAGE_READ),
        ("GET", "/api/tasks", MANAGE_READ),
        ("POST", "/api/tasks/{id}/cancel", [409, 409, 403, 403, 401]),
        ("GET", "/api/threads/{id}", READ),
        ("GET", "/api/users", MANAGE_READ),
        ("GET", "/api/oauth/providers", MANAGE_READ),
        ("GET", "/api/messages/{id}/pdf", READ),
        ("GET", "/api/ledger", MANAGE_READ),
        ("POST", "/api/ledger/verify", [202, 202, 403, 403, 401]),
        (
            "POST",
            "/api/archives/{id}/deletions",
            [200, 200, 403, 403, 401],
        ),
        (
            "POST",
            "/api/deletions/{id}/confirm",
            [400, 400, 403, 403, 401],
        ),
        ("GET", "/api/smtp", MANAGE_READ),
        ("GET", "/api/webhooks", MANAGE_READ),
        ("POST", "/api/webhooks", [201, 201, 403, 403, 401]),
        ("PATCH", "/api/webhooks/{id}", [200, 200, 403, 403, 401]),
        ("DELETE", "/api/webhooks/{id}", [204, 204, 403, 403, 401]),
        (
            "POST",
            "/api/webhooks/{id}/secret",
            [200, 200, 403, 403, 401],
        ),
        ("POST", "/api/webhooks/{id}/test", [202, 202, 403, 403, 401]),
        ("GET", "/api/webhooks/{id}/deliveries", MANAGE_READ),
        ("POST", "/api/users", [201, 201, 403, 403, 401]),
        ("PATCH", "/api/users/{id}", [200, 200, 403, 403, 401]),
        ("DELETE", "/api/users/{id}", [204, 204, 403, 403, 401]),
    ];
    rows.iter()
        .map(|(m, p, e)| ((m.to_string(), p.to_string()), *e))
        .collect()
}

/// `(method, path) -> declared statuses` of every operation in `holtmail openapi`.
fn operations(holtmail: &Holtmail) -> BTreeMap<(String, String), BTreeSet<u16>> {
    let output = holtmail.command(&["openapi"], "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: Value = serde_json::from_slice(&output.stdout).expect("OpenAPI JSON");
    let mut out = BTreeMap::new();
    for (path, item) in doc["paths"].as_object().expect("paths") {
        for (method, operation) in item.as_object().expect("a path item") {
            if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                continue;
            }
            let declared = operation["responses"]
                .as_object()
                .map(|r| r.keys().filter_map(|k| k.parse().ok()).collect())
                .unwrap_or_default();
            out.insert((method.to_uppercase(), path.clone()), declared);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn every_route_answers_each_role_as_documented() {
    require_it!();
    let world = world().await;
    let operations = operations(&world.holtmail);
    let expected = expectations();
    let documented: BTreeSet<_> = operations.keys().cloned().collect();
    let covered: BTreeSet<_> = expected.keys().cloned().collect();
    assert_eq!(
        documented.difference(&covered).collect::<Vec<_>>(),
        Vec::<&(String, String)>::new(),
        "routes in the OpenAPI description without an expectation here"
    );
    assert_eq!(
        covered.difference(&documented).collect::<Vec<_>>(),
        Vec::<&(String, String)>::new(),
        "expectations for routes the OpenAPI description does not have"
    );

    let base = world.holtmail.url();
    let mut clients = Vec::new();
    for who in EVERYONE {
        clients.push((who, Client::new(&base, who).await));
    }
    let mut failures = Vec::new();
    let mut undeclared = Vec::new();
    let mut asked = 0;
    let ends_sessions = |p: &str| p == "/api/logout" || p == "/api/me/password";
    let ordered: Vec<_> = expected
        .iter()
        .filter(|((_, p), _)| !ends_sessions(p))
        .chain(expected.iter().filter(|((_, p), _)| ends_sessions(p)))
        .collect();
    for ((method, template), statuses) in ordered {
        let method: Method = method.parse().expect("a method");
        for (index, (who, client)) in clients.iter().enumerate() {
            let fresh;
            let client = if ends_sessions(template) && *who != Who::Anonymous {
                fresh = Client::new(&base, *who).await;
                &fresh
            } else {
                client
            };
            let (path, body) = world.prepare(&method, template, *who).await;
            let (status, answer) = client.send(method.clone(), &path, body).await;
            asked += 1;
            if status.as_u16() != statuses[index] {
                failures.push(format!(
                    "{who:?} {method} {path}: {status}, expected {}: {answer}",
                    statuses[index]
                ));
            }
            let declared = &operations[&(method.to_string(), template.clone())];
            if !declared.contains(&status.as_u16()) {
                undeclared.push(format!("{method} {template}: {status} for {who:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {asked} answers differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        undeclared.is_empty(),
        "answers the OpenAPI description does not declare:\n{}",
        undeclared.join("\n")
    );

    let mut clients = Vec::new();
    for who in EVERYONE {
        clients.push((who, Client::new(&base, who).await));
    }
    let owner = Api::sign_in(&world.holtmail, OWNER_EMAIL, OWNER_PASSWORD).await;
    for (who, client) in &clients {
        let (status, _) = client
            .send(Method::GET, "/api/openapi.json", Body::Empty)
            .await;
        assert_eq!(status, 200, "{who:?} reads the OpenAPI description");
        let (status, _) = client
            .send(Method::GET, "/api/no-such-route", Body::Empty)
            .await;
        let expected = if *who == Who::Anonymous { 401 } else { 404 };
        assert_eq!(status.as_u16(), expected, "{who:?} on an unknown route");
    }

    let member = &clients[2].1;
    let (_, listed) = member.send(Method::GET, "/api/archives", Body::Empty).await;
    let ids: Vec<&str> = listed
        .as_array()
        .expect("archives")
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![world.granted.archive.as_str()],
        "a member lists only granted archives"
    );
    let (_, accounts) = member
        .send(
            Method::GET,
            &format!("/api/archives/{}/accounts", world.granted.archive),
            Body::Empty,
        )
        .await;
    for account in accounts.as_array().expect("accounts") {
        assert_eq!(
            account["connection"],
            Value::Null,
            "a member sees a connection: {account}"
        );
    }

    let admin = &clients[1].1;
    let owner_id = owner.get("/api/session").await.ok()["user"]["id"]
        .as_str()
        .expect("the owner's id")
        .to_owned();
    let (status, body) = admin
        .send(
            Method::POST,
            "/api/users",
            Body::Json(json!({ "email": "second-owner@example.com", "name": "Second Owner", "password": PASSWORD, "role": "owner" })),
        )
        .await;
    assert_eq!(status, 403, "an admin adds an owner: {body}");
    let (status, body) = admin
        .send(
            Method::PATCH,
            &format!("/api/users/{owner_id}"),
            Body::Json(json!({ "name": "Renamed Owner" })),
        )
        .await;
    assert_eq!(status, 403, "an admin changes the owner: {body}");
    let admin_id = &world.users[ADMIN_EMAIL];
    let (status, body) = admin
        .send(
            Method::PATCH,
            &format!("/api/users/{admin_id}"),
            Body::Json(json!({ "role": "owner" })),
        )
        .await;
    assert_eq!(status, 403, "an admin makes themselves owner: {body}");
    let (status, body) = admin
        .send(
            Method::PATCH,
            &format!("/api/users/{admin_id}"),
            Body::Json(json!({ "role": "member" })),
        )
        .await;
    assert_eq!(status, 400, "an admin changes their own role: {body}");

    let mut scanner = LogScanner::new()
        .secret("a user's password", PASSWORD)
        .secret("the IMAP password", DOVECOT_PASSWORD)
        .secret("the admin's email", ADMIN_EMAIL)
        .secret("the member's email", MEMBER_EMAIL);
    for seeded in [&world.granted, &world.other] {
        scanner = scanner.secrets("a subject", seeded.subjects.iter().map(String::as_str));
    }
    for user in &world.dovecot_users {
        scanner = scanner.secret("an IMAP user", user);
    }
    scanner.check(&world.holtmail.logs());
}
