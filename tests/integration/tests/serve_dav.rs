//! CalDAV and CardDAV serving through the real binary, with curl and plain DAV requests (the
//! requests vdirsyncer and phone clients send), over a Radicale account the instance syncs.
//!
//! Reading: the well-known redirects, `401` without a device or with the web password,
//! `PROPFIND /` finding the principal, the homes listing People first and the Radicale
//! collections, `GET` byte for byte with a quoted ETag, vCard 3.0 on request, calendar and
//! address book queries, `sync-collection` reporting an upstream addition, `valid-sync-token`
//! after the change log is pruned, display names kept per user, and a read-only device refused.
//!
//! Writing: a held create listed at once and never sent, `412` on a taken href and on a stale
//! ETag of Holtmail's own, a held edit served and kept from Radicale until a conflicting
//! upstream edit turns it into a conflict, a write-through edit, create and delete reaching
//! Radicale under hrefs that never change, and the web app's held create served the same way.

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use holtmail_integration::api::OWNER_EMAIL;
use holtmail_integration::data::{icalendar_event, vcard};
use holtmail_integration::dav::{Dav, RADICALE, hrefs_in};
use holtmail_integration::pim::PimRun;
use holtmail_integration::{Options, require_it, unique};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(60);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
        .port()
}

fn at(day: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(day)
        .expect("a time")
        .with_timezone(&chrono::Utc)
}

/// A DAV client of the instance, signed in with a device's app password.
struct Client {
    http: reqwest::Client,
    login: String,
    secret: String,
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Answer {
    fn etag(&self) -> Option<String> {
        self.headers
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }
}

impl Client {
    async fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: impl Into<String>,
    ) -> Answer {
        let mut request = self
            .http
            .request(
                Method::from_bytes(method.as_bytes()).expect("a method"),
                url,
            )
            .basic_auth(&self.login, Some(&self.secret))
            .body(body.into());
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
             xmlns:card=\"urn:ietf:params:xml:ns:carddav\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\" \
             xmlns:cs=\"http://calendarserver.org/ns/\"><d:prop>{props}</d:prop></d:propfind>"
        );
        let answer = self
            .send(
                "PROPFIND",
                url,
                &[("Depth", depth), ("Content-Type", "application/xml")],
                body,
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

    async fn report(&self, url: &str, body: String) -> Answer {
        self.send(
            "REPORT",
            url,
            &[("Depth", "1"), ("Content-Type", "application/xml")],
            body,
        )
        .await
    }

    async fn get(&self, url: &str) -> Answer {
        self.send("GET", url, &[], String::new()).await
    }

    async fn put(&self, url: &str, kind: &str, headers: &[(&str, &str)], body: &str) -> Answer {
        let mut all = vec![("Content-Type", kind)];
        all.extend_from_slice(headers);
        self.send("PUT", url, &all, body).await
    }

    /// The hrefs of the resources in the collection at `url`, absolute.
    async fn resources(&self, url: &str) -> Vec<String> {
        let listing = self.propfind(url, "1", "<d:getetag/>").await;
        let base = reqwest::Url::parse(url).expect("an address");
        hrefs_in(&listing, "multistatus")
            .into_iter()
            .map(|h| base.join(&h).expect("an href").to_string())
            .filter(|h| h.trim_end_matches('/') != url.trim_end_matches('/'))
            .collect()
    }

    /// The resource of the collection at `url` whose text holds `needle`, with its text.
    async fn find(&self, url: &str, needle: &str) -> Option<(String, String)> {
        for href in self.resources(url).await {
            let answer = self.get(&href).await;
            if answer.status.is_success() && answer.body.contains(needle) {
                return Some((href, answer.body));
            }
        }
        None
    }
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

fn sync_body(token: &str) -> String {
    format!(
        "<d:sync-collection xmlns:d=\"DAV:\"><d:sync-token>{token}</d:sync-token>\
         <d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"
    )
}

fn curl(args: &[&str]) -> String {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--max-time", "30", "--include"])
        .args(args)
        .output()
        .expect("curl runs");
    assert!(
        output.status.success(),
        "curl {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_lowercase()
}

struct Served {
    run: PimRun,
    base: String,
    client: Client,
    secret: String,
    upstream: Dav,
    account: String,
    upstream_book: String,
    upstream_plans: String,
    planning: String,
    ada: String,
    book_id: String,
    plans_id: String,
}

/// Radicale holds a user with the address book "Friends" (Ada) and the calendar "Plans" (a
/// planning meeting); an instance with `[serve.dav]` on `http://127.0.0.1:<port>` syncs it, and
/// a DAV device with `scopes` signs in.
async fn start(prefix: &str, scopes: &[&str]) -> Served {
    let user = RADICALE.user(prefix).await;
    let upstream = RADICALE.client(&user);
    let homes = RADICALE.homes(&upstream).await;
    let upstream_book = upstream
        .create_address_book(&homes.contacts, "friends", "Friends")
        .await;
    let upstream_plans = upstream
        .create_calendar(&homes.calendars, "plans", "Plans")
        .await;
    let ada_uid = unique("ada");
    let ada = upstream
        .put_card(
            &upstream_book,
            &ada_uid,
            &vcard(&ada_uid, "Ada Example", "ada.example@example.org", None),
        )
        .await;
    let planning_uid = unique("planning");
    let planning = upstream
        .put_event(
            &upstream_plans,
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

    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let mut run = PimRun::start_with(
        &format!("Served {prefix}"),
        Options {
            port: Some(port),
            extra_config: format!("public_url = \"{base}\"\n[serve.dav]\n"),
            ..Options::default()
        },
    )
    .await;
    run.secrets(
        "an item",
        &[
            "Ada Example",
            "ada.example@example.org",
            "Planning meeting",
            "Bea Example",
            "Held create",
        ],
    );
    let account = run.add_dav(&RADICALE, &user, None).await;
    run.api
        .request(
            Method::PUT,
            &format!("/api/accounts/{account}/capabilities"),
            Some(&json!({ "capabilities": ["pim.edit"] })),
        )
        .await
        .ok();
    let collections = run.collections().await;
    let id_of = |name: &str| {
        collections
            .get(name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", collections.keys()))["id"]
            .as_str()
            .expect("an id")
            .to_owned()
    };
    let book_id = id_of("Friends");
    let plans_id = id_of("Plans");
    let made = run
        .api
        .post(
            "/api/devices",
            json!({ "name": "Calendar app", "protocols": ["dav"], "scopes": scopes }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let secret = made.body["secret"].as_str().expect("a secret").to_owned();
    run.secrets("an app password", &[secret.as_str()]);
    let client = Client {
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client"),
        login: OWNER_EMAIL.to_owned(),
        secret: secret.clone(),
    };
    Served {
        run,
        base,
        client,
        secret,
        upstream,
        account,
        upstream_book,
        upstream_plans,
        planning,
        ada,
        book_id,
        plans_id,
    }
}

impl Served {
    /// The calendar and address book homes, found from `PROPFIND /` as a client that skips the
    /// well-known address does.
    async fn homes(&self) -> (String, String) {
        let root = self
            .client
            .propfind(
                &format!("{}/", self.base),
                "0",
                "<d:current-user-principal/>",
            )
            .await;
        let principal = hrefs_in(&root, "current-user-principal")
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("no principal: {root}"));
        assert!(principal.starts_with("/dav/principals/"), "{principal}");
        let url = format!("{}{principal}", self.base);
        let props = self
            .client
            .propfind(
                &url,
                "0",
                "<cal:calendar-home-set/><card:addressbook-home-set/>",
            )
            .await;
        let calendars = hrefs_in(&props, "calendar-home-set")
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("no calendar home: {props}"));
        let books = hrefs_in(&props, "addressbook-home-set")
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("no address book home: {props}"));
        (
            format!("{}{calendars}", self.base),
            format!("{}{books}", self.base),
        )
    }

    async fn set_mode(&self, collection: &str, mode: &str) {
        let answer = self
            .run
            .api
            .request(
                Method::PUT,
                &format!("/api/collections/{collection}/write-mode"),
                Some(&json!({ "mode": mode, "confirm": true })),
            )
            .await
            .ok();
        assert_eq!(answer["mode"], mode, "{answer}");
    }

    async fn open_changes(&self, collection: &str) -> Vec<Value> {
        self.run
            .api
            .get(&format!("/api/pim/changes?collection={collection}"))
            .await
            .ok()
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Waits until the collection has no open change left, failing on a conflict or refusal.
    async fn settled(&self, collection: &str) {
        let started = Instant::now();
        loop {
            let open = self.open_changes(collection).await;
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

    async fn upstream_text(&self, url: &str) -> String {
        let (status, text) = self.upstream.get(url).await;
        assert!(status.is_success(), "GET {url}: {status}");
        text
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_radicale_collections_to_dav_clients() {
    require_it!();
    let served = start("sdr", &["read"]).await;
    let base = &served.base;

    for well_known in ["caldav", "carddav"] {
        let head = curl(&[
            "-X",
            "PROPFIND",
            &format!("{base}/.well-known/{well_known}"),
        ]);
        assert!(head.starts_with("http/1.1 301"), "{head}");
        assert!(head.contains(&format!("location: {base}/dav/")), "{head}");
    }
    let anonymous = curl(&["-X", "PROPFIND", "-H", "Depth: 0", &format!("{base}/dav/")]);
    assert!(anonymous.starts_with("http/1.1 401"), "{anonymous}");
    assert!(anonymous.contains("www-authenticate: basic"), "{anonymous}");
    let web_password = curl(&[
        "-X",
        "PROPFIND",
        "-H",
        "Depth: 0",
        "-u",
        &format!(
            "{OWNER_EMAIL}:{}",
            holtmail_integration::api::OWNER_PASSWORD
        ),
        &format!("{base}/dav/"),
    ]);
    assert!(web_password.starts_with("http/1.1 401"), "{web_password}");
    let device = curl(&[
        "-X",
        "PROPFIND",
        "-H",
        "Depth: 0",
        "-H",
        "Content-Type: application/xml",
        "--data",
        "<d:propfind xmlns:d=\"DAV:\"><d:prop><d:current-user-principal/></d:prop></d:propfind>",
        "-u",
        &format!("{OWNER_EMAIL}:{}", served.secret),
        &format!("{base}/"),
    ]);
    assert!(device.starts_with("http/1.1 207"), "{device}");
    assert!(device.contains("current-user-principal"), "{device}");
    let web_root = curl(&[&format!("{base}/")]);
    assert!(
        !web_root.starts_with("http/1.1 207"),
        "a GET of / stays the web app's: {web_root}"
    );

    let (calendars, books) = served.homes().await;
    let book_listing = served
        .client
        .propfind(&books, "1", "<d:displayname/><d:resourcetype/>")
        .await;
    let book_hrefs: Vec<String> = hrefs_in(&book_listing, "multistatus")
        .into_iter()
        .filter(|h| format!("{base}{h}") != books)
        .collect();
    assert!(
        book_hrefs.first().is_some_and(|h| h
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .starts_with("people-")),
        "People first: {book_hrefs:?}"
    );
    assert!(
        book_hrefs.iter().any(|h| h.contains(&served.book_id)),
        "{book_listing}"
    );
    let calendar_listing = served
        .client
        .propfind(&calendars, "1", "<d:displayname/>")
        .await;
    assert!(
        calendar_listing.contains(&served.plans_id),
        "{calendar_listing}"
    );
    let plans = format!("{calendars}{}/", served.plans_id);
    let book = format!("{books}{}/", served.book_id);

    let (event, text) = served
        .client
        .find(&plans, "Planning meeting")
        .await
        .expect("the planning meeting is served");
    assert_eq!(
        text,
        served.upstream_text(&served.planning).await,
        "byte for byte"
    );
    let got = served.client.get(&event).await;
    let etag = got.etag().expect("an ETag");
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && !etag.starts_with("W/"),
        "{etag}"
    );
    let listing = served.client.propfind(&plans, "1", "<d:getetag/>").await;
    assert!(listing.contains(&etag), "{listing}");

    let (card, card_text) = served
        .client
        .find(&book, "Ada Example")
        .await
        .expect("Ada is served");
    assert_eq!(card_text, served.upstream_text(&served.ada).await);
    let old = served
        .client
        .send("GET", &card, &[("Accept", "text/vcard; version=3.0")], "")
        .await;
    assert!(old.status.is_success(), "{}", old.body);
    assert!(old.body.contains("VERSION:3.0"), "{}", old.body);
    assert!(old.body.contains("Ada Example"), "{}", old.body);
    assert_ne!(old.etag(), Some(etag.clone()));

    let query = |start: &str, end: &str| {
        format!(
            "<c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:getetag/></d:prop>\
             <c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\">\
             <c:time-range start=\"{start}\" end=\"{end}\"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"
        )
    };
    let path = reqwest::Url::parse(&event).unwrap().path().to_owned();
    let inside = served
        .client
        .report(&plans, query("20261201T000000Z", "20261202T000000Z"))
        .await;
    assert_eq!(inside.status.as_u16(), 207, "{}", inside.body);
    assert!(inside.body.contains(&path), "{}", inside.body);
    let outside = served
        .client
        .report(&plans, query("20270101T000000Z", "20270102T000000Z"))
        .await;
    assert!(!outside.body.contains(&path), "{}", outside.body);
    let contacts = served
        .client
        .report(
            &book,
            "<card:addressbook-query xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">\
             <d:prop><d:getetag/></d:prop><card:filter><card:prop-filter name=\"FN\">\
             <card:text-match match-type=\"contains\">ada</card:text-match></card:prop-filter></card:filter>\
             </card:addressbook-query>"
                .to_owned(),
        )
        .await;
    assert_eq!(contacts.status.as_u16(), 207, "{}", contacts.body);
    assert!(
        contacts
            .body
            .contains(reqwest::Url::parse(&card).unwrap().path()),
        "{}",
        contacts.body
    );

    let first = served.client.report(&plans, sync_body("")).await;
    assert_eq!(first.status.as_u16(), 207, "{}", first.body);
    let token = between(&first.body, "<d:sync-token>", "</d:sync-token>")
        .expect("a sync token")
        .to_owned();
    let ctag = between(
        &served.client.propfind(&plans, "0", "<cs:getctag/>").await,
        "getctag>",
        "</",
    )
    .map(str::to_owned);
    let review_uid = unique("review");
    served
        .upstream
        .put_event(
            &served.upstream_plans,
            &review_uid,
            &icalendar_event(
                &review_uid,
                "Quarterly review",
                at("2026-12-04T09:00:00Z"),
                at("2026-12-04T10:00:00Z"),
                None,
            ),
        )
        .await;
    served.run.sync(&served.account).await;
    let next = served.client.report(&plans, sync_body(&token)).await;
    assert_eq!(next.status.as_u16(), 207, "{}", next.body);
    let changed: Vec<String> = hrefs_in(&next.body, "multistatus");
    assert_eq!(changed.len(), 1, "only the new event: {}", next.body);
    let new = served.client.get(&format!("{base}{}", changed[0])).await;
    assert!(
        new.body.contains("SUMMARY:Quarterly review"),
        "{}",
        new.body
    );
    let moved = between(
        &served.client.propfind(&plans, "0", "<cs:getctag/>").await,
        "getctag>",
        "</",
    )
    .map(str::to_owned);
    assert_ne!(ctag, moved, "getctag moves with the change feed");

    assert!(
        served
            .run
            .holtmail
            .execute(
                "INSERT INTO settings (key, value) VALUES ('change_log_pruned_through', \
                 (SELECT CAST(MAX(seq) AS TEXT) FROM changes)) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                &[],
            )
            .await
            > 0
    );
    let pruned = served.client.report(&plans, sync_body(&token)).await;
    assert_eq!(pruned.status.as_u16(), 403, "{}", pruned.body);
    assert!(pruned.body.contains("valid-sync-token"), "{}", pruned.body);
    let fresh = served.client.report(&plans, sync_body("")).await;
    assert_eq!(fresh.status.as_u16(), 207, "a full listing still works");

    let patched = served
        .client
        .send(
            "PROPPATCH",
            &plans,
            &[("Content-Type", "application/xml")],
            "<d:propertyupdate xmlns:d=\"DAV:\"><d:set><d:prop><d:displayname>Team plans</d:displayname>\
             </d:prop></d:set></d:propertyupdate>",
        )
        .await;
    assert_eq!(patched.status.as_u16(), 207, "{}", patched.body);
    assert!(!patched.body.contains("403"), "{}", patched.body);
    let named = served
        .client
        .propfind(&plans, "0", "<d:displayname/>")
        .await;
    assert!(named.contains("Team plans"), "{named}");
    let upstream_name = served
        .upstream
        .propfind(&served.upstream_plans, "0", "<d:displayname/>")
        .await;
    assert!(
        upstream_name.contains("Plans") && !upstream_name.contains("Team plans"),
        "{upstream_name}"
    );

    let refused = served
        .client
        .put(
            &format!("{plans}refused.ics"),
            "text/calendar; charset=utf-8",
            &[],
            &icalendar_event(
                &unique("refused"),
                "Refused",
                at("2026-12-05T09:00:00Z"),
                at("2026-12-05T10:00:00Z"),
                None,
            ),
        )
        .await;
    assert_eq!(refused.status.as_u16(), 403, "{}", refused.body);
    assert!(refused.body.contains("need-privileges"), "{}", refused.body);

    served.run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn client_writes_are_held_or_written_through() {
    require_it!();
    let served = start("sdw", &["read", "write:pim"]).await;
    served.set_mode(&served.book_id, "write_through").await;
    let (calendars, books) = served.homes().await;
    let plans = format!("{calendars}{}/", served.plans_id);
    let book = format!("{books}{}/", served.book_id);
    let ical = "text/calendar; charset=utf-8";
    let vcf = "text/vcard; charset=utf-8";

    let held_uid = unique("held");
    let held_href = format!("{plans}{held_uid}.ics");
    let held_body = icalendar_event(
        &held_uid,
        "Held create",
        at("2026-12-02T09:00:00Z"),
        at("2026-12-02T10:00:00Z"),
        None,
    );
    let created = served
        .client
        .put(&held_href, ical, &[("If-None-Match", "*")], &held_body)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert!(
        served.client.resources(&plans).await.contains(&held_href),
        "listed at once"
    );
    let got = served.client.get(&held_href).await;
    assert_eq!(got.body, held_body);
    let again = served
        .client
        .put(&held_href, ical, &[("If-None-Match", "*")], &held_body)
        .await;
    assert_eq!(
        again.status,
        StatusCode::PRECONDITION_FAILED,
        "a taken href"
    );
    let open = served.open_changes(&served.plans_id).await;
    assert!(
        open.iter()
            .any(|c| c["op"] == "create" && c["state"] == "held"),
        "{open:?}"
    );

    let (event, text) = served
        .client
        .find(&plans, "Planning meeting")
        .await
        .expect("the planning meeting is served");
    let etag = served.client.get(&event).await.etag().expect("an ETag");
    let edited = text
        .replace("DTSTART:20261201T090000Z", "DTSTART:20261201T150000Z")
        .replace("DTEND:20261201T100000Z", "DTEND:20261201T160000Z");
    let saved = served
        .client
        .put(&event, ical, &[("If-Match", &etag)], &edited)
        .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.body);
    let now = served.client.get(&event).await;
    assert!(
        now.body.contains("DTSTART:20261201T150000Z"),
        "{}",
        now.body
    );
    let stale = served
        .client
        .put(&event, ical, &[("If-Match", &etag)], &edited)
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "a stale ETag of Holtmail's own"
    );
    let current = now.etag().expect("an ETag");
    let same = served
        .client
        .put(&event, ical, &[("If-Match", &current)], &now.body)
        .await;
    assert_eq!(same.status, StatusCode::NO_CONTENT, "{}", same.body);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let upstream = served.upstream_text(&served.planning).await;
    assert!(
        upstream.contains("DTSTART:20261201T090000Z"),
        "a held edit stays: {upstream}"
    );
    let (status, _) = served
        .upstream
        .get(&format!("{}{held_uid}.ics", served.upstream_plans))
        .await;
    assert_eq!(status.as_u16(), 404, "a held create stays");

    served
        .upstream
        .put(
            &served.planning,
            ical,
            &upstream
                .replace("DTSTART:20261201T090000Z", "DTSTART:20261201T120000Z")
                .replace("DTEND:20261201T100000Z", "DTEND:20261201T130000Z"),
        )
        .await;
    served.run.sync(&served.account).await;
    let conflicts = served
        .run
        .api
        .get(&format!(
            "/api/pim/changes?state=conflict&collection={}",
            served.plans_id
        ))
        .await
        .ok();
    let conflict = conflicts
        .as_array()
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or_else(|| panic!("no conflict: {conflicts}"));
    assert_eq!(conflict["reason"], "overlap", "{conflict}");
    assert!(
        served.client.get(&event).await.status.is_success(),
        "a conflicted item is still served"
    );

    let (card, card_text) = served
        .client
        .find(&book, "Ada Example")
        .await
        .expect("Ada is served");
    let card_etag = served.client.get(&card).await.etag().expect("an ETag");
    let renamed = card_text
        .replace("FN:Ada Example", "FN:Ada Example-Smith")
        .replace("N:Example;Ada", "N:Example-Smith;Ada");
    let saved = served
        .client
        .put(&card, vcf, &[("If-Match", &card_etag)], &renamed)
        .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.body);
    served.settled(&served.book_id).await;
    let upstream = served.upstream_text(&served.ada).await;
    assert!(upstream.contains("FN:Ada Example-Smith"), "{upstream}");

    let bea_uid = unique("bea");
    let bea = format!("{book}{bea_uid}.vcf");
    let created = served
        .client
        .put(
            &bea,
            vcf,
            &[("If-None-Match", "*")],
            &vcard(&bea_uid, "Bea Example", "bea.example@example.org", None),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    served.settled(&served.book_id).await;
    let (status, text) = served
        .upstream
        .get(&format!("{}{bea_uid}.vcf", served.upstream_book))
        .await;
    let reached = status.is_success() && text.contains("FN:Bea Example") || {
        let listing = served
            .upstream
            .propfind(&served.upstream_book, "1", "<d:getetag/>")
            .await;
        let mut found = false;
        for href in hrefs_in(&listing, "multistatus") {
            let url = reqwest::Url::parse(&served.upstream_book)
                .unwrap()
                .join(&href)
                .unwrap()
                .to_string();
            let (_, body) = served.upstream.get(&url).await;
            found |= body.contains("FN:Bea Example");
        }
        found
    };
    assert!(reached, "the new contact is in Radicale");
    served.run.sync(&served.account).await;
    let listed = served.client.resources(&book).await;
    assert_eq!(
        listed.iter().filter(|h| **h == bea).count(),
        1,
        "the href stays after writeback and sync: {listed:?}"
    );
    assert!(served.client.get(&bea).await.body.contains("Bea Example"));

    let deleted = served.client.send("DELETE", &bea, &[], "").await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert_eq!(served.client.get(&bea).await.status, StatusCode::NOT_FOUND);
    served.settled(&served.book_id).await;
    served.run.sync(&served.account).await;
    let listing = served
        .upstream
        .propfind(&served.upstream_book, "1", "<d:getetag/>")
        .await;
    for href in hrefs_in(&listing, "multistatus") {
        let url = reqwest::Url::parse(&served.upstream_book)
            .unwrap()
            .join(&href)
            .unwrap()
            .to_string();
        let (_, body) = served.upstream.get(&url).await;
        assert!(!body.contains("FN:Bea Example"), "deleted in Radicale");
    }

    let web = served
        .run
        .api
        .post(
            &format!("/api/collections/{}/items", served.plans_id),
            json!({ "event": { "title": "Made in the web app", "start": "2026-12-06T10:00:00", "end": "2026-12-06T11:00:00", "time_zone": "UTC" } }),
        )
        .await
        .ok();
    assert_eq!(web["state"], "held", "{web}");
    assert!(
        served
            .client
            .find(&plans, "SUMMARY:Made in the web app")
            .await
            .is_some(),
        "the web app's held create is served"
    );

    served.run.check_logs();
}
