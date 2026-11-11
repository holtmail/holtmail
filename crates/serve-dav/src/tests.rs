use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use base64::Engine;
use bytes::Bytes;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{ArchiveId, DeviceProtocol, DeviceScope, PersonId, Timestamp, UserId};
use holtmail_device_local::{LocalDeviceAuth, Options, new_app_password, secret_hash};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{IncomingItem, Ingest};
use holtmail_store::{
    AccountKind, AuditKind, Capabilities, Capability, ChangeOrigin, Collection, CollectionKind,
    DeviceSettings, InstanceRole, ItemChangeQuery, NewAccount, NewAuditEntry, NewCollection,
    NewDevice, NewUser, Person, PersonCard, Store, WriteMode,
};
use holtmail_store_sqlite::SqliteStore;
use tempfile::TempDir;
use tower::ServiceExt;

use super::*;

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:standup@example.com\r\nDTSTAMP:20260101T090000Z\r\nSUMMARY:Standup\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T093000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const CARD: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:bea\r\nFN:Bea Example\r\nEMAIL:bea@example.org\r\nEND:VCARD\r\n";
const NEW_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//macOS//EN\r\nBEGIN:VEVENT\r\nUID:lunch@example.com\r\nDTSTAMP:20260201T080000Z\r\nSUMMARY:Lunch\r\nDTSTART:20260203T120000Z\r\nDTEND:20260203T130000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

struct Fixture {
    _dir: TempDir,
    store: Arc<dyn Store>,
    ingest: Ingest,
    devices: Arc<LocalDeviceAuth>,
    archive: ArchiveId,
    user: UserId,
    calendar: Collection,
    book: Collection,
    auth: String,
    secret: String,
    router: Router,
}

async fn fixture() -> Fixture {
    fixture_with(
        DavOptions::new("https://mail.example.com"),
        &[DeviceScope::Read, DeviceScope::WritePim],
    )
    .await
}

async fn fixture_with(options: DavOptions, scopes: &[DeviceScope]) -> Fixture {
    let dir = TempDir::new().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        SqliteStore::open(dir.path().join("db.sqlite"))
            .await
            .unwrap(),
    );
    let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
    let index = Arc::new(TantivyIndex::open(dir.path().join("index")).unwrap());
    let ingest = Ingest::new(blobs, store.clone(), index);
    let archive = store.create_archive("Home").await.unwrap().id;
    let user = store
        .create_user(NewUser {
            email: "ada@example.com".into(),
            name: "Ada Example".into(),
            role: InstanceRole::Owner,
            password_hash: None,
        })
        .await
        .unwrap();
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "ada@example.com".into(),
            address: Some("ada@example.com".into()),
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    let caps: Capabilities = [Capability::PimEdit].into_iter().collect();
    store
        .set_account_capabilities(account.id, &caps)
        .await
        .unwrap();
    let collection = |kind, remote: &str| NewCollection {
        archive_id: archive,
        account_id: Some(account.id),
        kind,
        remote_id: remote.into(),
        name: remote.trim_matches('/').into(),
        color: Some("#3366ccff".into()),
    };
    let calendar = store
        .upsert_collection(collection(CollectionKind::Calendar, "/cals/work/"))
        .await
        .unwrap();
    let book = store
        .upsert_collection(collection(CollectionKind::Contacts, "/books/main/"))
        .await
        .unwrap();
    for (coll, remote, kind, format, raw) in [
        (
            calendar.id,
            "/cals/work/standup.ics",
            holtmail_pim::ItemKind::Event,
            holtmail_pim::Format::ICalendar,
            EVENT,
        ),
        (
            book.id,
            "/books/main/bea.vcf",
            holtmail_pim::ItemKind::Contact,
            holtmail_pim::Format::VCard,
            CARD,
        ),
    ] {
        ingest
            .ingest_item(IncomingItem {
                collection: coll,
                remote_id: remote.into(),
                kind,
                format,
                raw: Bytes::from_static(raw.as_bytes()),
                etag: Some("\"e1\"".into()),
                seen_at: Timestamp::from_secs(1_700_000_000),
            })
            .await
            .unwrap();
    }
    let secret = new_app_password().unwrap();
    store
        .create_device(NewDevice {
            user_id: user.id,
            settings: DeviceSettings {
                name: "Phone".into(),
                mail_archive: None,
                archives: None,
                protocols: BTreeSet::from([DeviceProtocol::Dav]),
                scopes: scopes.iter().copied().collect(),
                implicit_seen: false,
            },
            secret_hash: Some(secret_hash(&secret)),
            at: Timestamp::now(),
        })
        .await
        .unwrap();
    let devices = Arc::new(LocalDeviceAuth::new(store.clone(), Options::default()));
    let dav = Dav::new(store.clone(), ingest.clone(), devices.clone(), options);
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("ada@example.com:{secret}"))
    );
    Fixture {
        _dir: dir,
        router: dav.router(),
        store,
        ingest,
        devices,
        archive,
        user: user.id,
        calendar,
        book,
        auth,
        secret,
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Fixture {
    fn calendar_path(&self) -> String {
        format!("/dav/calendars/{}/{}/", self.user, self.calendar.id)
    }

    fn book_path(&self) -> String {
        format!("/dav/addressbooks/{}/{}/", self.user, self.book.id)
    }

    async fn send(&self, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Reply {
        let mut request = Request::builder()
            .method(Method::from_bytes(method.as_bytes()).unwrap())
            .uri(uri)
            .header("authorization", &self.auth);
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let mut request = request.body(Body::from(body.to_owned())).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 5], 50000))));
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    async fn propfind(&self, uri: &str, depth: &str, props: &str) -> Reply {
        let body = format!(
            "<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\" xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:ical=\"http://apple.com/ns/ical/\"><d:prop>{props}</d:prop></d:propfind>"
        );
        self.send("PROPFIND", uri, &[("depth", depth)], &body).await
    }

    async fn sync(&self, uri: &str, token: &str) -> Reply {
        let body = format!(
            "<d:sync-collection xmlns:d=\"DAV:\"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"
        );
        self.send("REPORT", uri, &[("depth", "0")], &body).await
    }
}

impl Fixture {
    /// Compares a whole answer, with this fixture's ids replaced by names and one element per
    /// line, to `tests/golden/<name>.xml`; `HOLTMAIL_BLESS=1` writes the file instead.
    fn golden(&self, name: &str, body: &str) {
        let normalized = body
            .replace(&self.user.to_string(), "{user}")
            .replace(&self.archive.to_string(), "{archive}")
            .replace(&self.calendar.id.to_string(), "{calendar}")
            .replace(&self.book.id.to_string(), "{book}")
            .replace("><", ">\n<");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("{name}.xml"));
        if std::env::var_os("HOLTMAIL_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("{normalized}\n")).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{} is missing; run with HOLTMAIL_BLESS=1", path.display()));
        assert_eq!(
            normalized,
            expected.trim_end_matches('\n'),
            "the answer differs from {}",
            path.display()
        );
    }
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_follows_the_well_known_path_to_the_homes() {
    let f = fixture().await;
    let well_known = f.send("PROPFIND", "/.well-known/caldav", &[], "").await;
    assert_eq!(well_known.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        well_known.headers["location"],
        "https://mail.example.com/dav/"
    );

    let root = f
        .propfind("/dav/", "0", "<d:current-user-principal/><d:resourcetype/>")
        .await;
    assert_eq!(root.status, StatusCode::MULTI_STATUS);
    let principal = format!("/dav/principals/{}/", f.user);
    assert!(
        root.body.contains(&format!("<d:href>{principal}</d:href>")),
        "{}",
        root.body
    );

    let homes = f
        .propfind(
            &principal,
            "0",
            "<c:calendar-home-set/><card:addressbook-home-set/><c:calendar-user-address-set/><d:displayname/><c:schedule-inbox-URL/>",
        )
        .await;
    assert!(homes.body.contains(&format!("/dav/calendars/{}/", f.user)));
    assert!(
        homes
            .body
            .contains(&format!("/dav/addressbooks/{}/", f.user))
    );
    assert!(homes.body.contains("mailto:ada@example.com"));
    assert!(homes.body.contains("Ada Example"));
    assert!(
        homes.body.contains("404 Not Found"),
        "unknown props are 404"
    );

    let calendars = f
        .propfind(
            &format!("/dav/calendars/{}/", f.user),
            "1",
            "<d:resourcetype/><d:displayname/><cs:getctag/><d:sync-token/><c:supported-calendar-component-set/><d:current-user-privilege-set/><ical:calendar-color/>",
        )
        .await;
    assert!(
        calendars.body.contains(&f.calendar_path()),
        "{}",
        calendars.body
    );
    assert!(calendars.body.contains("<c:calendar/>"));
    assert!(calendars.body.contains("<c:comp name=\"VEVENT\"/>"));
    assert!(!calendars.body.contains("VTODO"));
    assert!(
        calendars.body.contains("<d:bind/>"),
        "writable: {}",
        calendars.body
    );
    assert!(calendars.body.contains("#3366ccff"));
    assert!(calendars.body.contains("urn:holtmail:sync:"));

    let books = f
        .propfind(
            &format!("/dav/addressbooks/{}/", f.user),
            "1",
            "<d:displayname/><d:resourcetype/>",
        )
        .await;
    let people = books.body.find(&format!("people-{}", f.archive)).unwrap();
    let main = books.body.find(&f.book.id.to_string()).unwrap();
    assert!(people < main, "People comes first");

    let deep = f.propfind(&principal, "infinity", "<d:displayname/>").await;
    assert_eq!(deep.status, StatusCode::FORBIDDEN);
    assert!(deep.body.contains("propfind-finite-depth"));

    let other = f
        .propfind(
            &format!("/dav/principals/{}/", UserId::new()),
            "0",
            "<d:displayname/>",
        )
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_needs_an_app_password_over_https() {
    let f = fixture().await;
    let mut anonymous = f.send("PROPFIND", "/dav/", &[("depth", "0")], "").await;
    assert_eq!(anonymous.status, StatusCode::MULTI_STATUS);
    let wrong = Fixture {
        auth: format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("ada@example.com:hm_wrong")
        ),
        ..f
    };
    anonymous = wrong.send("PROPFIND", "/dav/", &[("depth", "0")], "").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(
        anonymous.headers["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );

    let plain = fixture_with(
        DavOptions::new("http://mail.example.com"),
        &[DeviceScope::Read],
    )
    .await;
    let refused = plain.send("PROPFIND", "/dav/", &[("depth", "0")], "").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let options = plain.send("OPTIONS", "/dav/", &[], "").await;
    assert!(
        options.headers["dav"]
            .to_str()
            .unwrap()
            .contains("calendar-access")
    );
    assert!(
        !options.headers["dav"]
            .to_str()
            .unwrap()
            .contains("auto-schedule")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_are_served_byte_for_byte_with_their_hash_as_etag() {
    let f = fixture().await;
    let listing = f
        .propfind(&f.calendar_path(), "1", "<d:getetag/><d:getcontenttype/>")
        .await;
    let href = format!(
        "{}{}",
        f.calendar_path(),
        path::name_of("/cals/work/standup.ics")
    );
    assert!(listing.body.contains(&href), "{}", listing.body);
    let hash = holtmail_blob::BlobId::of(EVENT.as_bytes()).to_string();
    assert!(listing.body.contains(&format!("\"{hash}\"")));

    let get = f.send("GET", &href, &[], "").await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(get.body, EVENT);
    assert_eq!(get.headers["etag"], format!("\"{hash}\"").as_str());
    let head = f.send("HEAD", &href, &[], "").await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());

    let card = format!("{}{}", f.book_path(), path::name_of("/books/main/bea.vcf"));
    let v4 = f.send("GET", &card, &[], "").await;
    assert_eq!(v4.body, CARD);
    let v3 = f
        .send("GET", &card, &[("accept", "text/vcard; version=3.0")], "")
        .await;
    assert!(v3.body.contains("VERSION:3.0"), "{}", v3.body);
    assert_ne!(v3.headers["etag"], v4.headers["etag"]);

    let missing = f
        .send("GET", &format!("{}nope.ics", f.calendar_path()), &[], "")
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_answer_multiget_and_queries() {
    let f = fixture().await;
    let href = format!(
        "{}{}",
        f.calendar_path(),
        path::name_of("/cals/work/standup.ics")
    );
    let multiget = format!(
        "<c:calendar-multiget xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:getetag/><c:calendar-data/></d:prop><d:href>{href}</d:href><d:href>{}missing.ics</d:href></c:calendar-multiget>",
        f.calendar_path()
    );
    let reply = f
        .send("REPORT", &f.calendar_path(), &[("depth", "1")], &multiget)
        .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("SUMMARY:Standup"), "{}", reply.body);
    assert!(reply.body.contains("404 Not Found"));

    let query = |start: &str, end: &str| {
        format!(
            "<c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:getetag/></d:prop><c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:time-range start=\"{start}\" end=\"{end}\"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"
        )
    };
    let hit = f
        .send(
            "REPORT",
            &f.calendar_path(),
            &[("depth", "1")],
            &query("20260107T000000Z", "20260108T000000Z"),
        )
        .await;
    assert!(hit.body.contains(&path::name_of("/cals/work/standup.ics")));
    let miss = f
        .send(
            "REPORT",
            &f.calendar_path(),
            &[("depth", "1")],
            &query("20260201T000000Z", "20260301T000000Z"),
        )
        .await;
    assert!(!miss.body.contains(&path::name_of("/cals/work/standup.ics")));

    let contacts = "<card:addressbook-query xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\"><d:prop><d:getetag/><card:address-data version=\"3.0\"/></d:prop><card:filter><card:prop-filter name=\"EMAIL\"><card:text-match match-type=\"contains\">example.org</card:text-match></card:prop-filter></card:filter></card:addressbook-query>";
    let found = f
        .send("REPORT", &f.book_path(), &[("depth", "1")], contacts)
        .await;
    assert!(found.body.contains("VERSION:3.0"), "{}", found.body);

    let busy = "<c:free-busy-query xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><c:time-range start=\"20260105T000000Z\" end=\"20260107T000000Z\"/></c:free-busy-query>";
    let fb = f
        .send("REPORT", &f.calendar_path(), &[("depth", "1")], busy)
        .await;
    assert_eq!(fb.status, StatusCode::OK);
    assert!(
        fb.body
            .contains("FREEBUSY:20260105T090000Z/20260105T093000Z"),
        "{}",
        fb.body
    );
    assert!(
        fb.body
            .contains("FREEBUSY:20260106T090000Z/20260106T093000Z")
    );

    let search = "<d:principal-property-search xmlns:d=\"DAV:\"><d:property-search><d:prop><d:displayname/></d:prop><d:match>ada</d:match></d:property-search><d:prop><d:displayname/></d:prop></d:principal-property-search>";
    let principal = f
        .send("REPORT", "/dav/principals/", &[("depth", "0")], search)
        .await;
    assert!(
        principal
            .body
            .contains(&format!("/dav/principals/{}/", f.user))
    );
    let nobody = search.replace(">ada<", ">grace<");
    let empty = f
        .send("REPORT", "/dav/principals/", &[("depth", "0")], &nobody)
        .await;
    assert!(!empty.body.contains("<d:response>"));

    let unknown = "<d:expand-property xmlns:d=\"DAV:\"/>";
    let refused = f.send("REPORT", &f.calendar_path(), &[], unknown).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn held_creates_are_listed_at_once_and_412_only_on_our_own_etag() {
    let f = fixture().await;
    let start = f.sync(&f.calendar_path(), "").await;
    let token = between(&start.body, "<d:sync-token>", "</d:sync-token>")
        .unwrap()
        .to_owned();
    let ctag = f.propfind(&f.calendar_path(), "0", "<cs:getctag/>").await;

    let href = format!("{}lunch.ics", f.calendar_path());
    let created = f
        .send(
            "PUT",
            &href,
            &[
                ("if-none-match", "*"),
                ("content-type", "text/calendar; charset=utf-8"),
            ],
            NEW_EVENT,
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let etag = created.headers["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        etag,
        format!("\"{}\"", holtmail_blob::BlobId::of(NEW_EVENT.as_bytes()))
    );

    let listing = f.propfind(&f.calendar_path(), "1", "<d:getetag/>").await;
    assert!(listing.body.contains("lunch.ics"), "{}", listing.body);
    let get = f.send("GET", &href, &[], "").await;
    assert_eq!(get.body, NEW_EVENT);
    assert_eq!(get.headers["etag"].to_str().unwrap(), etag);
    let after = f.propfind(&f.calendar_path(), "0", "<cs:getctag/>").await;
    assert_ne!(
        between(&after.body, "<cs:getctag>", "<"),
        between(&ctag.body, "<cs:getctag>", "<")
    );
    let delta = f.sync(&f.calendar_path(), &token).await;
    assert!(delta.body.contains("lunch.ics"), "{}", delta.body);
    assert!(
        !delta
            .body
            .contains(&path::name_of("/cals/work/standup.ics"))
    );

    let again = f
        .send("PUT", &href, &[("if-none-match", "*")], NEW_EVENT)
        .await;
    assert_eq!(again.status, StatusCode::PRECONDITION_FAILED);
    let stale = f
        .send(
            "PUT",
            &href,
            &[("if-match", "\"upstream-etag\"")],
            NEW_EVENT,
        )
        .await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let moved = NEW_EVENT.replace("T120000Z", "T123000Z");
    let edited = f.send("PUT", &href, &[("if-match", &etag)], &moved).await;
    assert_eq!(edited.status, StatusCode::NO_CONTENT, "{}", edited.body);
    assert!(edited.headers.contains_key("etag"));
    assert_eq!(f.send("GET", &href, &[], "").await.body, moved);

    let changes = f
        .store
        .open_changes(&ItemChangeQuery {
            archive_id: f.archive,
            collection_id: Some(f.calendar.id),
            states: Vec::new(),
            flagged_only: false,
            after: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(changes.len(), 1, "the edit superseded the create");
    assert_eq!(changes[0].origin, ChangeOrigin::Dav);
    assert!(changes[0].device.is_some());
    assert_eq!(changes[0].user_id, Some(f.user));

    let deleted = f.send("DELETE", &href, &[], "").await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        f.send("GET", &href, &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    let gone = f.sync(&f.calendar_path(), &token).await;
    assert!(gone.body.contains("lunch.ics"));
    assert!(gone.body.contains("404 Not Found"), "{}", gone.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_of_upstream_items_are_held_and_validated() {
    let f = fixture().await;
    let href = format!(
        "{}{}",
        f.calendar_path(),
        path::name_of("/cals/work/standup.ics")
    );
    let renamed = EVENT.replace("SUMMARY:Standup", "SUMMARY:Daily standup");
    let ok = f.send("PUT", &href, &[], &renamed).await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT, "{}", ok.body);
    let changes = f
        .store
        .open_changes(&ItemChangeQuery {
            archive_id: f.archive,
            collection_id: Some(f.calendar.id),
            states: Vec::new(),
            flagged_only: false,
            after: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(changes[0].class.as_str(), "edit");
    assert_eq!(f.send("GET", &href, &[], "").await.body, renamed);

    let other_uid = EVENT.replace("standup@example.com", "other@example.com");
    let refused = f.send("PUT", &href, &[], &other_uid).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.body.contains("no-uid-conflict"), "{}", refused.body);

    let clash = f
        .send("PUT", &format!("{}copy.ics", f.calendar_path()), &[], EVENT)
        .await;
    assert_eq!(clash.status, StatusCode::FORBIDDEN);
    assert!(clash.body.contains("no-uid-conflict"));

    let broken = f
        .send(
            "PUT",
            &format!("{}broken.ics", f.calendar_path()),
            &[],
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:No UID\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .await;
    assert_eq!(broken.status, StatusCode::FORBIDDEN);
    assert!(
        broken.body.contains("valid-calendar-data"),
        "{}",
        broken.body
    );

    let wrong_type = f
        .send(
            "PUT",
            &format!("{}x.ics", f.calendar_path()),
            &[("content-type", "text/vcard")],
            CARD,
        )
        .await;
    assert_eq!(wrong_type.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let mkcol = f
        .send(
            "MKCALENDAR",
            &format!("/dav/calendars/{}/new/", f.user),
            &[],
            "",
        )
        .await;
    assert_eq!(mkcol.status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_devices_and_collections_are_refused_up_front() {
    let f = fixture_with(
        DavOptions::new("https://mail.example.com"),
        &[DeviceScope::Read],
    )
    .await;
    let privileges = f
        .propfind(&f.calendar_path(), "0", "<d:current-user-privilege-set/>")
        .await;
    assert!(privileges.body.contains("<d:read/>"));
    assert!(!privileges.body.contains("<d:bind/>"));
    let put = f
        .send(
            "PUT",
            &format!("{}lunch.ics", f.calendar_path()),
            &[],
            NEW_EVENT,
        )
        .await;
    assert_eq!(put.status, StatusCode::FORBIDDEN);
    assert!(put.body.contains("need-privileges"));

    let mut options = DavOptions::new("https://mail.example.com");
    options.edits = false;
    let off = fixture_with(options, &[DeviceScope::Read, DeviceScope::WritePim]).await;
    let put = off
        .send(
            "PUT",
            &format!("{}lunch.ics", off.calendar_path()),
            &[],
            NEW_EVENT,
        )
        .await;
    assert_eq!(put.status, StatusCode::FORBIDDEN);

    let g = fixture().await;
    g.store
        .set_write_mode(g.calendar.id, WriteMode::WriteThrough)
        .await
        .unwrap();
    g.store
        .set_write_mode(g.calendar.id, WriteMode::ReadOnly)
        .await
        .unwrap();
    let privileges = g
        .propfind(&g.calendar_path(), "0", "<d:current-user-privilege-set/>")
        .await;
    assert!(
        !privileges.body.contains("<d:bind/>"),
        "read_only is advertised at once"
    );
    let refused = g
        .send(
            "PUT",
            &format!("{}lunch.ics", g.calendar_path()),
            &[],
            NEW_EVENT,
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::FORBIDDEN,
        "only a collection that was held gets the grace day"
    );
    g.store
        .set_write_mode(g.calendar.id, WriteMode::Held)
        .await
        .unwrap();
    g.store
        .set_write_mode(g.calendar.id, WriteMode::ReadOnly)
        .await
        .unwrap();
    for n in 0..600 {
        g.store
            .append_audit(&NewAuditEntry {
                at: Timestamp::now(),
                user: Some(g.user),
                kind: AuditKind::MailFlags,
                archive: Some(g.archive),
                account: g.calendar.account_id,
                targets: vec![format!("busy-{n}")],
                count: 1,
            })
            .await
            .unwrap();
    }
    let grace = g
        .send(
            "PUT",
            &format!("{}lunch.ics", g.calendar_path()),
            &[],
            NEW_EVENT,
        )
        .await;
    assert_eq!(
        grace.status,
        StatusCode::CREATED,
        "held within the grace day, however busy the archive's audit log"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn display_properties_are_kept_per_user_all_or_nothing() {
    let f = fixture().await;
    let set = "<d:propertyupdate xmlns:d=\"DAV:\" xmlns:ical=\"http://apple.com/ns/ical/\"><d:set><d:prop><d:displayname>Work stuff</d:displayname><ical:calendar-color>#ff0000ff</ical:calendar-color><ical:calendar-order>2</ical:calendar-order></d:prop></d:set></d:propertyupdate>";
    let reply = f.send("PROPPATCH", &f.calendar_path(), &[], set).await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    assert!(reply.body.contains("200 OK"), "{}", reply.body);
    let shown = f
        .propfind(
            &f.calendar_path(),
            "0",
            "<d:displayname/><ical:calendar-color/><ical:calendar-order/>",
        )
        .await;
    assert!(shown.body.contains("Work stuff"));
    assert!(shown.body.contains("#ff0000ff"));
    assert!(shown.body.contains(">2<"));
    assert_eq!(
        f.store
            .get_collection(f.calendar.id)
            .await
            .unwrap()
            .unwrap()
            .name,
        "cals/work"
    );

    let mixed = "<d:propertyupdate xmlns:d=\"DAV:\" xmlns:x=\"urn:example\"><d:set><d:prop><d:displayname>Other</d:displayname><x:secret>1</x:secret></d:prop></d:set></d:propertyupdate>";
    let reply = f.send("PROPPATCH", &f.calendar_path(), &[], mixed).await;
    assert!(
        reply.body.contains("424 Failed Dependency"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains("403 Forbidden"));
    let shown = f
        .propfind(&f.calendar_path(), "0", "<d:displayname/>")
        .await;
    assert!(shown.body.contains("Work stuff"));

    let remove = "<d:propertyupdate xmlns:d=\"DAV:\"><d:remove><d:prop><d:displayname/></d:prop></d:remove></d:propertyupdate>";
    f.send("PROPPATCH", &f.calendar_path(), &[], remove).await;
    let shown = f
        .propfind(&f.calendar_path(), "0", "<d:displayname/>")
        .await;
    assert!(shown.body.contains("cals/work"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_tokens_are_checked() {
    let f = fixture().await;
    let start = f.sync(&f.book_path(), "").await;
    assert!(start.body.contains(&path::name_of("/books/main/bea.vcf")));
    let token = between(&start.body, "<d:sync-token>", "</d:sync-token>")
        .unwrap()
        .to_owned();
    let nothing = f.sync(&f.book_path(), &token).await;
    assert!(!nothing.body.contains("<d:response>"), "{}", nothing.body);
    let foreign = f.sync(&f.calendar_path(), &token).await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert!(foreign.body.contains("valid-sync-token"));
    let future = token.rsplit_once(':').unwrap().0.to_owned() + ":999999";
    let refused = f.sync(&f.book_path(), &future).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn people_and_from_mail_are_served_read_only() {
    let f = fixture().await;
    let items = f
        .store
        .list_items(&holtmail_store::ItemQuery {
            archive_id: f.archive,
            kind: None,
            collection_id: Some(f.book.id),
            removed: holtmail_store::RemovedFilter::Hide,
            order: holtmail_store::ItemOrder::Title,
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    let person = Person {
        id: PersonId::from_hash([9; 16]),
        archive_id: f.archive,
        name: Some("Bea Example".into()),
        email: Some("bea@example.org".into()),
        phone: None,
        emails: vec!["bea@example.org".into()],
        mail_emails: vec!["bea@example.org".into()],
        phones: Vec::new(),
        cards: vec![PersonCard {
            item_id: items.items[0].item.id,
            collection_id: f.book.id,
            removed: false,
        }],
        links: Vec::new(),
        mail: None,
        differences: false,
        removed: false,
    };
    f.store
        .replace_people(f.archive, std::slice::from_ref(&person))
        .await
        .unwrap();
    let people = format!("/dav/addressbooks/{}/people-{}/", f.user, f.archive);
    let listing = f
        .propfind(&people, "1", "<d:getetag/><d:current-user-privilege-set/>")
        .await;
    let href = format!("{people}{}.vcf", person.id);
    assert!(listing.body.contains(&href), "{}", listing.body);
    assert!(!listing.body.contains("<d:bind/>"));
    let card = f.send("GET", &href, &[], "").await;
    assert!(card.body.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"));
    let put = f.send("PUT", &href, &[], &card.body).await;
    assert_eq!(put.status, StatusCode::FORBIDDEN);
    let first = f.sync(&people, "").await;
    let token = between(&first.body, "<d:sync-token>", "</d:sync-token>")
        .unwrap()
        .to_owned();
    assert!(!f.sync(&people, &token).await.body.contains("<d:response>"));

    let mail = f
        .store
        .upsert_collection(NewCollection {
            archive_id: f.archive,
            account_id: None,
            kind: CollectionKind::Calendar,
            remote_id: "mail".into(),
            name: "From mail".into(),
            color: None,
        })
        .await
        .unwrap();
    for (remote, raw) in [("m1", EVENT.to_owned()), ("m2", NEW_EVENT.to_owned())] {
        f.ingest
            .ingest_item(IncomingItem {
                collection: mail.id,
                remote_id: remote.into(),
                kind: holtmail_pim::ItemKind::Event,
                format: holtmail_pim::Format::ICalendar,
                raw: Bytes::from(raw),
                etag: None,
                seen_at: Timestamp::from_secs(1_700_000_000),
            })
            .await
            .unwrap();
    }
    let path = format!("/dav/calendars/{}/{}/", f.user, mail.id);
    let before = f.sync(&path, "").await;
    let token = between(&before.body, "<d:sync-token>", "</d:sync-token>")
        .unwrap()
        .to_owned();
    let listing = f.propfind(&path, "1", "<d:getetag/>").await;
    assert!(
        listing.body.contains(&format!("{path}m2")),
        "{}",
        listing.body
    );
    assert!(
        !listing.body.contains(&format!("{path}m1")),
        "the work calendar holds that UID"
    );
    assert_eq!(
        f.send("GET", &format!("{path}m1"), &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    let put = f
        .send("PUT", &format!("{path}m3.ics"), &[], NEW_EVENT)
        .await;
    assert_eq!(put.status, StatusCode::FORBIDDEN);

    f.ingest
        .ingest_item(IncomingItem {
            collection: f.calendar.id,
            remote_id: "/cals/work/lunch.ics".into(),
            kind: holtmail_pim::ItemKind::Event,
            format: holtmail_pim::Format::ICalendar,
            raw: Bytes::from_static(NEW_EVENT.as_bytes()),
            etag: Some("\"l1\"".into()),
            seen_at: Timestamp::from_secs(1_700_000_100),
        })
        .await
        .unwrap();
    let hidden = f.sync(&path, &token).await;
    let gone = format!("<d:href>{path}m2</d:href>");
    let at = hidden
        .body
        .find(&gone)
        .unwrap_or_else(|| panic!("{}", hidden.body));
    assert!(
        hidden.body[at..].contains("404 Not Found"),
        "another calendar took the UID: {}",
        hidden.body
    );
    let token = between(&hidden.body, "<d:sync-token>", "</d:sync-token>")
        .unwrap()
        .to_owned();
    f.store
        .mark_items_removed(
            f.calendar.id,
            &["/cals/work/lunch.ics".to_owned()],
            Timestamp::from_secs(1_700_000_200),
        )
        .await
        .unwrap();
    let back = f.sync(&path, &token).await;
    let at = back
        .body
        .find(&gone)
        .unwrap_or_else(|| panic!("{}", back.body));
    assert!(
        back.body[at..].contains("<d:getetag>"),
        "the UID left the other calendar: {}",
        back.body
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_deleted_with_its_mail_is_reported_removed() {
    let f = fixture().await;
    let mail = f
        .store
        .upsert_collection(NewCollection {
            archive_id: f.archive,
            account_id: None,
            kind: CollectionKind::Calendar,
            remote_id: "mail".into(),
            name: "From mail".into(),
            color: None,
        })
        .await
        .unwrap();
    let message = f
        .store
        .insert_message(holtmail_store::testing::message(
            f.archive,
            1,
            1_700_000_000,
        ))
        .await
        .unwrap()
        .message_id;
    let raw = NEW_EVENT.replace("lunch@example.com", "party@example.com");
    let read = holtmail_pim::read_as(
        holtmail_pim::Format::ICalendar,
        holtmail_pim::ItemKind::Event,
        raw.as_bytes(),
    );
    f.store
        .record_mail_version(holtmail_store::NewMailVersion {
            version: holtmail_store::NewItemVersion {
                collection_id: mail.id,
                remote_id: "party".into(),
                kind: holtmail_store::ItemKind::Event,
                key: Some("party@example.com".into()),
                blob_id: holtmail_blob::BlobId::of(raw.as_bytes()),
                content_type: "text/calendar".into(),
                etag: None,
                seen_at: Timestamp::from_secs(1_700_000_000),
                photo_blob_id: None,
                index: holtmail_ingest::pim_index(&read.normalized, 0),
            },
            message_id: message,
            mail: holtmail_store::MailVersionInfo {
                method: Some("REQUEST".into()),
                sequence: Some(0),
                dtstamp: None,
                sent_at: None,
            },
        })
        .await
        .unwrap();
    let path = format!("/dav/calendars/{}/{}/", f.user, mail.id);
    let first = f.sync(&path, "").await;
    let href = format!("<d:href>{path}party</d:href>");
    assert!(first.body.contains(&href), "{}", first.body);
    let token = between(&first.body, "<d:sync-token>", "</d:sync-token>")
        .unwrap()
        .to_owned();
    f.store
        .delete_messages(f.archive, &[message], None)
        .await
        .unwrap();
    let next = f.sync(&path, &token).await;
    let at = next
        .body
        .find(&href)
        .unwrap_or_else(|| panic!("{}", next.body));
    assert!(next.body[at..].contains("404 Not Found"), "{}", next.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_held_create_is_listed_past_one_page_of_open_changes() {
    let f = fixture().await;
    for n in 0..510 {
        let body = NEW_EVENT.replace("lunch@example.com", &format!("e{n}@example.com"));
        let put = f
            .send("PUT", &format!("{}e{n}.ics", f.calendar_path()), &[], &body)
            .await;
        assert_eq!(put.status, StatusCode::CREATED, "{}", put.body);
    }
    let listing = f.propfind(&f.calendar_path(), "1", "<d:getetag/>").await;
    let held = (0..510)
        .filter(|n| {
            listing
                .body
                .contains(&format!("{}e{n}.ics<", f.calendar_path()))
        })
        .count();
    assert_eq!(held, 510);
}

#[tokio::test(flavor = "multi_thread")]
async fn root_propfind_leaves_the_web_app_alone() {
    let f = fixture().await;
    let dav = Dav::new(
        f.store.clone(),
        f.ingest.clone(),
        f.devices.clone(),
        DavOptions::new("https://mail.example.com"),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let dav = dav.with_hook(Arc::new(move |_| {
        hook_calls.fetch_add(1, Ordering::SeqCst);
    }));
    let web = Router::new()
        .route("/", axum::routing::get(|| async { "web app" }))
        .layer(axum::middleware::from_fn_with_state(
            dav.clone(),
            root_propfind,
        ))
        .merge(dav.router());
    let f = Fixture { router: web, ..f };
    let page = f.send("GET", "/", &[], "").await;
    assert_eq!(page.body, "web app");
    let probe = f.propfind("/", "0", "<d:current-user-principal/>").await;
    assert_eq!(probe.status, StatusCode::MULTI_STATUS);
    assert!(probe.body.contains("/dav/principals/"));
    let created = f
        .send(
            "PUT",
            &format!("{}lunch.ics", f.calendar_path()),
            &[],
            NEW_EVENT,
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_with_collection_dav() {
    use holtmail_collection::{
        CollectionSource, CollectionWriter, Credentials, ItemPatch, Precondition,
    };
    use holtmail_collection_dav::{DavConfig, DavSource};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let f = fixture_with(
        DavOptions::new(&base),
        &[DeviceScope::Read, DeviceScope::WritePim],
    )
    .await;
    let app = f.router.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let source = DavSource::new(DavConfig::at(
        &base,
        Credentials::Password {
            username: "ada@example.com".into(),
            password: f.secret.clone(),
        },
    ))
    .unwrap();
    let collections = source.collections().await.unwrap();
    assert_eq!(collections.len(), 3, "{collections:?}");
    let calendar = collections
        .iter()
        .find(|c| c.id.contains(&f.calendar.id.to_string()))
        .unwrap();
    assert_eq!(calendar.color.as_deref(), Some("#3366ccff"));
    let first = source.changes(&calendar.id, None).await.unwrap();
    assert_eq!(first.changed.len(), 1);
    let ids: Vec<String> = first.changed.iter().map(|t| t.id.clone()).collect();
    let items = source.fetch(&calendar.id, &ids).await.unwrap();
    assert_eq!(items[0].raw, EVENT.as_bytes());

    let written = source
        .put(
            &calendar.id,
            None,
            &ItemPatch {
                content_type: "text/calendar".into(),
                raw: Bytes::from_static(NEW_EVENT.as_bytes()),
                uid: Some("lunch@example.com".into()),
                fields: None,
            },
            Precondition::Absent,
            "loopback-1",
        )
        .await
        .unwrap();
    let later = source
        .changes(&calendar.id, Some(&first.token))
        .await
        .unwrap();
    assert!(
        later.changed.iter().any(|t| t.id == written.id),
        "{later:?} {written:?}"
    );
    let again = source
        .changes(&calendar.id, Some(&later.token))
        .await
        .unwrap();
    assert!(again.changed.is_empty() && again.removed.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn client_request_shapes_are_answered() {
    let f = fixture().await;
    let apple = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<A:propfind xmlns:A=\"DAV:\"><A:prop><A:add-member/><C:allowed-sharing-modes xmlns:C=\"http://calendarserver.org/ns/\"/><D:autoprovisioned xmlns:D=\"http://apple.com/ns/ical/\"/><E:bulk-requests xmlns:E=\"http://me.com/_namespace/\"/><B:calendar-color xmlns:B=\"http://apple.com/ns/ical/\"/><F:calendar-description xmlns:F=\"urn:ietf:params:xml:ns:caldav\"/><B:calendar-order xmlns:B=\"http://apple.com/ns/ical/\"/><C:getctag xmlns:C=\"http://calendarserver.org/ns/\"/><A:current-user-privilege-set/><A:displayname/><A:owner/><A:resourcetype/><F:supported-calendar-component-set xmlns:F=\"urn:ietf:params:xml:ns:caldav\"/><A:supported-report-set/><A:sync-token/></A:prop></A:propfind>";
    let reply = f
        .send(
            "PROPFIND",
            &format!("/dav/calendars/{}/", f.user),
            &[("depth", "1")],
            apple,
        )
        .await;
    assert_eq!(reply.status, StatusCode::MULTI_STATUS);
    let doc = xml::parse(reply.body.as_bytes()).expect("well-formed answer");
    let responses: Vec<_> = doc.children(xml::DAV, "response").collect();
    assert_eq!(responses.len(), 2, "the home and its calendar");
    assert!(
        reply
            .body
            .contains("<x:bulk-requests xmlns:x=\"http://me.com/_namespace/\"/>")
    );
    assert!(reply.body.contains("<c:calendar-query/>"));
    f.golden("apple-calendar-home", &reply.body);

    let davx5 = "<propfind xmlns=\"DAV:\" xmlns:CARD=\"urn:ietf:params:xml:ns:carddav\"><prop><CARD:addressbook-home-set/><current-user-principal/></prop></propfind>";
    let reply = f
        .send(
            "PROPFIND",
            &format!("/dav/principals/{}/", f.user),
            &[("depth", "0")],
            davx5,
        )
        .await;
    assert!(
        reply
            .body
            .contains(&format!("<d:href>/dav/addressbooks/{}/</d:href>", f.user))
    );
    f.golden("davx5-principal", &reply.body);

    let thunderbird = "<D:propfind xmlns:D=\"DAV:\" xmlns:CS=\"http://calendarserver.org/ns/\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><D:prop><D:resourcetype/><D:owner/><D:current-user-principal/><D:current-user-privilege-set/><D:supported-report-set/><C:supported-calendar-component-set/><CS:getctag/></D:prop></D:propfind>";
    let reply = f
        .send(
            "PROPFIND",
            &f.calendar_path(),
            &[("depth", "0")],
            thunderbird,
        )
        .await;
    let doc = xml::parse(reply.body.as_bytes()).unwrap();
    let ok = doc
        .child(xml::DAV, "response")
        .and_then(|r| r.child(xml::DAV, "propstat"))
        .and_then(|p| p.child(xml::DAV, "prop"))
        .unwrap();
    assert_eq!(ok.children.len(), 7, "{}", reply.body);
    f.golden("thunderbird-calendar", &reply.body);

    let empty = f
        .send("PROPFIND", &f.calendar_path(), &[("depth", "0")], "")
        .await;
    assert!(
        empty
            .body
            .contains("<d:displayname>cals/work</d:displayname>")
    );
}

mod scheduling {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use holtmail_core::{IdentityId, OutboxId, TransportId};
    use holtmail_schedule::testing::FakeScheduler;
    use holtmail_schedule::{Host, HostCaps};
    use holtmail_store::{
        ChangeClass, ChangeContent, ChangeOp, ChangeTarget, Expect, InboxNote, NewIdentity,
        NewItemChange, ScheduleInboxEntry, SentCopyPolicy, Transport, TransportKind,
        TransportLimits,
    };
    use holtmail_submission::{
        Actor, SendIdentity, Submission, SubmissionError, SubmissionLimits, SubmissionState,
        Submit, Submitted,
    };

    use super::*;

    #[derive(Default)]
    struct Outbox(Mutex<Vec<Submit>>);

    impl Outbox {
        fn methods(&self) -> Vec<(String, Vec<String>)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .map(|s| {
                    let text = String::from_utf8_lossy(&s.message).replace("\r\n ", "");
                    let method = text
                        .lines()
                        .find_map(|l| l.strip_prefix("METHOD:"))
                        .unwrap_or_default()
                        .to_owned();
                    (method, s.recipients.clone())
                })
                .collect()
        }
    }

    #[async_trait]
    impl Submission for Outbox {
        async fn identities(
            &self,
            _: &Actor,
            _: ArchiveId,
        ) -> Result<Vec<SendIdentity>, SubmissionError> {
            Ok(Vec::new())
        }

        async fn limits(
            &self,
            _: &Actor,
            _: ArchiveId,
        ) -> Result<SubmissionLimits, SubmissionError> {
            Ok(SubmissionLimits {
                max_size: 1 << 20,
                smtputf8: true,
            })
        }

        async fn sender(
            &self,
            _: &Actor,
            _: ArchiveId,
            address: &str,
        ) -> Result<SendIdentity, SubmissionError> {
            match address.eq_ignore_ascii_case("ada@example.com") {
                true => Ok(SendIdentity {
                    id: IdentityId::new(),
                    address: address.into(),
                    name: None,
                }),
                false => Err(SubmissionError::Forbidden("no identity".into())),
            }
        }

        async fn queue(&self, request: Submit) -> Result<Submitted, SubmissionError> {
            let submitted = Submitted {
                id: OutboxId::new(),
                archive: request.archive,
                identity: request.identity,
                state: SubmissionState::Queued,
                send_at: Timestamp::now(),
                recipients: Vec::new(),
                rfc_message_id: "itip@example.com".into(),
            };
            self.0.lock().unwrap().push(request);
            Ok(submitted)
        }

        async fn undo(&self, _: &Actor, _: OutboxId) -> Result<Submitted, SubmissionError> {
            Err(SubmissionError::NotFound("submission"))
        }

        async fn release(
            &self,
            _: &Actor,
            _: OutboxId,
            _: Option<std::time::Duration>,
        ) -> Result<Submitted, SubmissionError> {
            Err(SubmissionError::NotFound("submission"))
        }

        async fn status(&self, _: &Actor, _: OutboxId) -> Result<Submitted, SubmissionError> {
            Err(SubmissionError::NotFound("submission"))
        }
    }

    struct Hosts {
        known: bool,
    }

    #[async_trait]
    impl CalendarHosts for Hosts {
        async fn host(&self, _: &Collection) -> Option<(Host, HostCaps)> {
            if !self.known {
                return None;
            }
            Some((
                Host::CalDav {
                    auto_schedule: false,
                },
                HostCaps {
                    create: true,
                    update: true,
                    delete: true,
                    respond: true,
                },
            ))
        }
    }

    const ORGANIZED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//macOS//EN\r\nBEGIN:VEVENT\r\nUID:review@example.com\r\nDTSTAMP:20260201T080000Z\r\nSEQUENCE:0\r\nSUMMARY:Review\r\nDTSTART:20260210T100000Z\r\nDTEND:20260210T110000Z\r\nORGANIZER;CN=Ada:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:ada@example.com\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;SCHEDULE-STATUS=2.0:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    const INVITE: &str = "From: Bob <bob@example.org>\r\nTo: Ada <ada@example.com>\r\nSubject: Invitation\r\nDate: Mon, 02 Feb 2026 09:00:00 +0000\r\nMessage-ID: <walk-invite@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:walk@example.org\r\nSEQUENCE:0\r\nDTSTAMP:20260202T090000Z\r\nDTSTART:20260212T150000Z\r\nDTEND:20260212T160000Z\r\nSUMMARY:Walk\r\nORGANIZER:mailto:bob@example.org\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:ada@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    async fn scheduling(scopes: &[DeviceScope]) -> (Fixture, Arc<Outbox>) {
        scheduling_with(scopes, true).await
    }

    async fn scheduling_with(scopes: &[DeviceScope], known: bool) -> (Fixture, Arc<Outbox>) {
        let mut options = DavOptions::new("https://mail.example.com");
        options.scheduling = true;
        let mut f = fixture_with(options.clone(), scopes).await;
        let transport = Transport {
            id: TransportId::new(),
            archive_id: Some(f.archive),
            name: "Relay".into(),
            kind: TransportKind::Smtp,
            account_id: None,
            settings: serde_json::json!({ "host": "smtp.example.com", "port": 587 }),
            sealed_secret: None,
            allowed_from: vec!["*@example.com".into()],
            limits: TransportLimits::default(),
            enabled: true,
            smtputf8: false,
            created_at: Timestamp::now(),
        };
        f.store.create_transport(&transport).await.unwrap();
        f.store
            .create_identity(NewIdentity {
                archive_id: f.archive,
                address: "ada@example.com".into(),
                name: Some("Ada".into()),
                signature_html: None,
                signature_text: None,
                reply_to: None,
                transport_id: transport.id,
                sent_copy: SentCopyPolicy::None,
                bcc_self: false,
                account_id: None,
                alias: Some("ada.alias@example.org".into()),
                plus_addresses: false,
                active: true,
                created_by: Some(f.user),
            })
            .await
            .unwrap();
        let outbox = Arc::new(Outbox::default());
        let scheduler = Arc::new(FakeScheduler::new(
            outbox.clone(),
            holtmail_schedule::system_clock(),
        ));
        f.router = Dav::new(
            f.store.clone(),
            f.ingest.clone(),
            f.devices.clone(),
            options,
        )
        .with_scheduler(scheduler, Arc::new(Hosts { known }))
        .router();
        (f, outbox)
    }

    const ALL: [DeviceScope; 3] = [
        DeviceScope::Read,
        DeviceScope::WritePim,
        DeviceScope::SendMail,
    ];

    #[tokio::test(flavor = "multi_thread")]
    async fn advertised_only_when_every_condition_holds() {
        let (f, _) = scheduling(&ALL).await;
        let options = f.send("OPTIONS", "/dav/", &[], "").await;
        let dav = options.headers["dav"].to_str().unwrap();
        assert!(dav.contains("calendar-auto-schedule"), "{dav}");
        let principal = f
            .propfind(
                &format!("/dav/principals/{}/", f.user),
                "0",
                "<c:schedule-inbox-URL/><c:schedule-outbox-URL/><c:calendar-user-address-set/><c:schedule-default-calendar-URL/>",
            )
            .await;
        assert!(principal.body.contains("/inbox/"), "{}", principal.body);
        assert!(principal.body.contains("/outbox/"), "{}", principal.body);
        assert!(principal.body.contains("mailto:ada@example.com"));
        assert!(principal.body.contains("mailto:ada.alias@example.org"));
        assert!(principal.body.contains(&f.calendar.id.to_string()));

        let (read_only, _) = scheduling(&[DeviceScope::Read, DeviceScope::WritePim]).await;
        let options = read_only.send("OPTIONS", "/dav/", &[], "").await;
        assert!(
            !options.headers["dav"]
                .to_str()
                .unwrap()
                .contains("calendar-auto-schedule"),
            "a device without send:mail"
        );
        let inbox = read_only
            .propfind(
                &format!("/dav/calendars/{}/inbox/", read_only.user),
                "0",
                "<d:resourcetype/>",
            )
            .await;
        assert_eq!(inbox.status, StatusCode::NOT_FOUND);

        let unsigned = Request::builder()
            .method(Method::OPTIONS)
            .uri("/dav/")
            .body(Body::empty())
            .unwrap();
        let answer = f.router.clone().oneshot(unsigned).await.unwrap();
        assert!(
            !answer.headers()["dav"]
                .to_str()
                .unwrap()
                .contains("calendar-auto-schedule"),
            "never without credentials"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn organizer_puts_are_delivered_with_a_schedule_tag() {
        let (f, outbox) = scheduling(&ALL).await;
        let url = format!("{}review.ics", f.calendar_path());
        let created = f
            .send(
                "PUT",
                &url,
                &[("content-type", "text/calendar"), ("if-none-match", "*")],
                ORGANIZED,
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert!(
            created.headers.get("etag").is_none(),
            "the scheduler rewrote the bytes, so no ETag"
        );
        let tag = created.headers["schedule-tag"].to_str().unwrap().to_owned();
        assert_eq!(
            outbox.methods(),
            [("REQUEST".to_owned(), vec!["bob@example.org".to_owned()])]
        );
        let got = f.send("GET", &url, &[], "").await;
        let text = got.body.replace("\r\n ", "");
        assert!(
            text.contains("SCHEDULE-STATUS=1.1:mailto:bob@example.org"),
            "{text}"
        );
        assert!(
            !text.contains("SCHEDULE-STATUS=2.0"),
            "client status stripped: {text}"
        );
        assert_eq!(got.headers["schedule-tag"].to_str().unwrap(), tag);

        let stale = f
            .send(
                "PUT",
                &url,
                &[
                    ("content-type", "text/calendar"),
                    ("if-schedule-tag-match", "\"elsewhere\""),
                ],
                &got.body,
            )
            .await;
        assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
        let moved = got
            .body
            .replace("DTSTART:20260210T100000Z", "DTSTART:20260210T090000Z");
        let changed = f
            .send(
                "PUT",
                &url,
                &[
                    ("content-type", "text/calendar"),
                    ("if-schedule-tag-match", tag.as_str()),
                ],
                &moved,
            )
            .await;
        assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.body);
        assert_ne!(changed.headers["schedule-tag"].to_str().unwrap(), tag);
        assert_eq!(outbox.methods().len(), 2, "the moved event is sent again");

        let deleted = f.send("DELETE", &url, &[], "").await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        assert_eq!(outbox.methods().last().unwrap().0, "CANCEL");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_merged_reply_keeps_the_schedule_tag() {
        let (f, _) = scheduling(&ALL).await;
        let url = format!(
            "{}{}",
            f.calendar_path(),
            path::name_of("/cals/work/standup.ics")
        );
        let before = f.send("GET", &url, &[], "").await;
        let tag = before.headers["schedule-tag"].to_str().unwrap().to_owned();
        let effective = f
            .store
            .effective(&ChangeTarget::Href {
                collection: f.calendar.id,
                href: "/cals/work/standup.ics".into(),
            })
            .await
            .unwrap()
            .unwrap();
        let item = effective.item.as_ref().unwrap().item.id;
        f.store
            .set_schedule_tag(item, Some(tag.trim_matches('"')))
            .await
            .unwrap();
        let merged = EVENT.replace("SUMMARY:Standup", "SUMMARY:Standup\r\nCOMMENT:answered");
        let blob = f
            .ingest
            .blobs()
            .put(Bytes::from(merged.clone()))
            .await
            .unwrap();
        f.store
            .record_change(
                &NewItemChange {
                    target: ChangeTarget::Item { item },
                    op: ChangeOp::Update,
                    class: ChangeClass::Edit,
                    content: Some(ChangeContent {
                        blob_id: blob,
                        content_type: "text/calendar".into(),
                    }),
                    patch: None,
                    base_version: Some(1),
                    origin: ChangeOrigin::Itip,
                    user_id: None,
                    device: None,
                    at: Timestamp::now(),
                    not_before: None,
                    messages: Vec::new(),
                    schedule_tag: None,
                },
                Expect::Any,
            )
            .await
            .unwrap();
        let after = f.send("GET", &url, &[], "").await;
        assert_ne!(after.headers["etag"], before.headers["etag"]);
        assert_eq!(after.headers["schedule-tag"].to_str().unwrap(), tag);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_organizer_put_keeps_a_merged_answer() {
        let (f, _) = scheduling(&ALL).await;
        f.ingest
            .ingest_item(IncomingItem {
                collection: f.calendar.id,
                remote_id: "/cals/work/review.ics".into(),
                kind: holtmail_pim::ItemKind::Event,
                format: holtmail_pim::Format::ICalendar,
                raw: Bytes::from_static(ORGANIZED.as_bytes()),
                etag: Some("\"r1\"".into()),
                seen_at: Timestamp::from_secs(1_700_000_000),
            })
            .await
            .unwrap();
        let url = format!(
            "{}{}",
            f.calendar_path(),
            path::name_of("/cals/work/review.ics")
        );
        let before = f.send("GET", &url, &[], "").await;
        let tag = before.headers["schedule-tag"].to_str().unwrap().to_owned();
        let effective = f
            .store
            .effective(&ChangeTarget::Href {
                collection: f.calendar.id,
                href: "/cals/work/review.ics".into(),
            })
            .await
            .unwrap()
            .unwrap();
        let item = effective.item.as_ref().unwrap().item.id;
        let merged = ORGANIZED.replace(
            "ATTENDEE;PARTSTAT=NEEDS-ACTION;SCHEDULE-STATUS=2.0:mailto:bob@example.org",
            "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org",
        );
        let blob = f.ingest.blobs().put(Bytes::from(merged)).await.unwrap();
        f.store
            .record_change(
                &NewItemChange {
                    target: ChangeTarget::Item { item },
                    op: ChangeOp::Update,
                    class: ChangeClass::Reply,
                    content: Some(ChangeContent {
                        blob_id: blob,
                        content_type: "text/calendar".into(),
                    }),
                    patch: None,
                    base_version: Some(1),
                    origin: ChangeOrigin::Itip,
                    user_id: None,
                    device: None,
                    at: Timestamp::now(),
                    not_before: None,
                    messages: Vec::new(),
                    schedule_tag: Some(tag.trim_matches('"').to_owned()),
                },
                Expect::Any,
            )
            .await
            .unwrap();
        let after = f.send("GET", &url, &[], "").await;
        assert_eq!(after.headers["schedule-tag"].to_str().unwrap(), tag);
        let edited = before
            .body
            .replace("SUMMARY:Review", "SUMMARY:Review with notes");
        let put = f
            .send(
                "PUT",
                &url,
                &[
                    ("content-type", "text/calendar"),
                    ("if-schedule-tag-match", tag.as_str()),
                ],
                &edited,
            )
            .await;
        assert_eq!(put.status, StatusCode::NO_CONTENT, "{}", put.body);
        let got = f.send("GET", &url, &[], "").await;
        let text = got.body.replace("\r\n ", "");
        assert!(text.contains("SUMMARY:Review with notes"), "{text}");
        assert!(
            text.contains("PARTSTAT=ACCEPTED") && !text.contains("PARTSTAT=NEEDS-ACTION"),
            "Bob's merged answer survives the stale copy: {text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_calendar_server_that_cannot_be_asked_is_503_not_unscheduled() {
        let (f, outbox) = scheduling_with(&ALL, false).await;
        let options = f.send("OPTIONS", "/dav/", &[], "").await;
        assert!(
            options.headers["dav"]
                .to_str()
                .unwrap()
                .contains("calendar-auto-schedule"),
            "the capability does not flap with the calendar server"
        );
        let url = format!("{}review.ics", f.calendar_path());
        let put = f
            .send(
                "PUT",
                &url,
                &[("content-type", "text/calendar"), ("if-none-match", "*")],
                ORGANIZED,
            )
            .await;
        assert_eq!(put.status, StatusCode::SERVICE_UNAVAILABLE, "{}", put.body);
        assert!(outbox.methods().is_empty(), "nothing was sent");
        let got = f.send("GET", &url, &[], "").await;
        assert_eq!(got.status, StatusCode::NOT_FOUND, "nothing was recorded");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_inbox_lists_incoming_itip_and_deleting_hides_it() {
        let (f, _) = scheduling(&ALL).await;
        let message = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(INVITE.as_bytes()),
                &holtmail_ingest::Placement::default(),
            )
            .await
            .unwrap()
            .message_id;
        f.store
            .put_inbox_entry(&ScheduleInboxEntry {
                archive_id: f.archive,
                message_id: message,
                part: 0,
                method: "REQUEST".into(),
                uid: "walk@example.org".into(),
                note: InboxNote::Update,
                received_at: Timestamp::now(),
            })
            .await
            .unwrap();
        let inbox = format!("/dav/calendars/{}/inbox/", f.user);
        let listed = f
            .propfind(&inbox, "1", "<d:resourcetype/><d:getetag/>")
            .await;
        assert!(
            listed.body.contains("<c:schedule-inbox/>"),
            "{}",
            listed.body
        );
        let name = format!("{message}-0.ics");
        assert!(listed.body.contains(&name), "{}", listed.body);
        let got = f.send("GET", &format!("{inbox}{name}"), &[], "").await;
        assert!(got.body.contains("METHOD:REQUEST"), "{}", got.body);
        let gone = f.send("DELETE", &format!("{inbox}{name}"), &[], "").await;
        assert_eq!(gone.status, StatusCode::NO_CONTENT);
        let listed = f.propfind(&inbox, "1", "<d:getetag/>").await;
        assert!(!listed.body.contains(&name), "{}", listed.body);
        assert!(f.store.get_message(message).await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn free_busy_answers_own_addresses_only() {
        let (f, _) = scheduling(&ALL).await;
        let request = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VFREEBUSY\r\nUID:fb-1@example.com\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260105T000000Z\r\nDTEND:20260107T000000Z\r\nORGANIZER:mailto:ada@example.com\r\nATTENDEE:mailto:ada@example.com\r\nATTENDEE:mailto:bob@example.org\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n";
        let answer = f
            .send(
                "POST",
                &format!("/dav/calendars/{}/outbox/", f.user),
                &[("content-type", "text/calendar")],
                request,
            )
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        assert!(answer.body.contains("2.0;Success"), "{}", answer.body);
        assert!(
            answer
                .body
                .contains("FREEBUSY:20260105T090000Z/20260105T093000Z"),
            "{}",
            answer.body
        );
        assert!(answer.body.contains("3.7;Invalid calendar user"));
        let stranger = request.replace("ORGANIZER:mailto:ada@", "ORGANIZER:mailto:eve@");
        let refused = f
            .send(
                "POST",
                &format!("/dav/calendars/{}/outbox/", f.user),
                &[("content-type", "text/calendar")],
                &stranger,
            )
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
    }
}
