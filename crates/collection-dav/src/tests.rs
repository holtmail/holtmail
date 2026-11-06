use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes as Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use holtmail_collection::testing::{Remote, conformance};

use super::*;

#[derive(Default)]
struct Coll {
    kind: Option<CollectionKind>,
    name: String,
    color: Option<String>,
    items: BTreeMap<String, (u64, Vec<u8>)>,
    log: Vec<(u64, String)>,
}

#[derive(Default)]
struct DavState {
    collections: BTreeMap<String, Coll>,
    seq: u64,
    generation: u64,
    no_sync: bool,
    no_multiget: bool,
    page: Option<usize>,
    stuck: bool,
    busy: u32,
    redirect_root: Option<String>,
    requests: Vec<(String, String, bool)>,
}

#[derive(Clone, Default)]
struct FakeDav(Arc<Mutex<DavState>>);

const BOOK: &str = "/dav/addressbooks/ada/contacts/";
const CAL: &str = "/dav/calendars/ada/work/";

impl FakeDav {
    fn standard() -> Self {
        let fake = Self::default();
        {
            let mut s = fake.0.lock().unwrap();
            s.collections.insert(
                BOOK.into(),
                Coll {
                    kind: Some(CollectionKind::Contacts),
                    name: "Contacts".into(),
                    ..Coll::default()
                },
            );
            s.collections.insert(
                CAL.into(),
                Coll {
                    kind: Some(CollectionKind::Calendar),
                    name: "Work & Life".into(),
                    color: Some("#336699FF".into()),
                    ..Coll::default()
                },
            );
        }
        fake
    }

    fn put_item(&self, collection: &str, name: &str, raw: &[u8]) {
        let mut s = self.0.lock().unwrap();
        s.seq += 1;
        let seq = s.seq;
        let coll = s.collections.get_mut(collection).unwrap();
        coll.items.insert(name.into(), (seq, raw.to_vec()));
        coll.log.push((seq, name.into()));
    }

    fn requests(&self) -> Vec<(String, String, bool)> {
        self.0.lock().unwrap().requests.clone()
    }
}

#[async_trait]
impl Remote for FakeDav {
    async fn put(&self, collection: &str, name: &str, raw: &[u8]) {
        self.put_item(collection, name, raw);
    }

    async fn delete(&self, collection: &str, name: &str) {
        let mut s = self.0.lock().unwrap();
        s.seq += 1;
        let seq = s.seq;
        let coll = s.collections.get_mut(collection).unwrap();
        coll.items.remove(name);
        coll.log.push((seq, name.into()));
    }

    async fn expire_tokens(&self) {
        self.0.lock().unwrap().generation += 1;
    }
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn multistatus(inner: String) -> Response {
    (
        StatusCode::from_u16(207).unwrap(),
        [("content-type", "application/xml; charset=utf-8")],
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\" \
             xmlns:C=\"urn:ietf:params:xml:ns:carddav\" xmlns:K=\"urn:ietf:params:xml:ns:caldav\" \
             xmlns:CS=\"http://calendarserver.org/ns/\" xmlns:A=\"http://apple.com/ns/ical/\">\
             {inner}</D:multistatus>"
        ),
    )
        .into_response()
}

fn ok(href: &str, props: &str) -> String {
    format!(
        "<D:response><D:href>{}</D:href><D:propstat><D:prop>{props}</D:prop>\
         <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        path::encode(href)
    )
}

fn token(s: &DavState, seq: u64) -> String {
    format!("http://example.com/ns/sync/{}-{seq}", s.generation)
}

fn authorized(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("ada@example.com:app-secret")
    );
    value == basic || value == "Bearer token-1"
}

fn coll_props(s: &DavState, path: &str, coll: &Coll) -> String {
    let last = coll.log.last().map_or(0, |(seq, _)| *seq);
    let kind = match coll.kind {
        Some(CollectionKind::Contacts) => "<C:addressbook/>",
        _ => "<K:calendar/>",
    };
    let mut props = format!(
        "<D:resourcetype><D:collection/>{kind}</D:resourcetype>\
         <D:displayname>{}</D:displayname><CS:getctag>ctag-{last}</CS:getctag>\
         <D:sync-token>{}</D:sync-token>",
        esc(&coll.name),
        token(s, last)
    );
    if let Some(color) = &coll.color {
        props.push_str(&format!("<A:calendar-color>{color}</A:calendar-color>"));
    }
    if coll.kind == Some(CollectionKind::Calendar) {
        props.push_str(
            "<K:supported-calendar-component-set><K:comp name=\"VEVENT\"/>\
             <K:comp name=\"vtodo\"/></K:supported-calendar-component-set>",
        );
    }
    let _ = path;
    props
}

async fn handle(
    State(fake): State<FakeDav>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let path = path::id(&Url::parse(&format!("http://x{}", uri.path())).unwrap());
    let mut s = fake.0.lock().unwrap();
    s.requests.push((
        method.to_string(),
        path.clone(),
        headers.contains_key("authorization"),
    ));
    if path.starts_with("/.well-known/") {
        return (StatusCode::MOVED_PERMANENTLY, [("location", "/dav/")]).into_response();
    }
    if s.busy > 0 {
        s.busy -= 1;
        return (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "0")]).into_response();
    }
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Basic realm=\"dav\"")],
        )
            .into_response();
    }
    let depth = headers
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("infinity")
        .to_owned();
    match (method.as_str(), path.as_str()) {
        ("PROPFIND", "/dav/") => {
            if let Some(to) = s.redirect_root.clone() {
                return (StatusCode::TEMPORARY_REDIRECT, [("location", to)]).into_response();
            }
            multistatus(ok(
                "/dav/",
                "<D:current-user-principal><D:href>/dav/principals/ada/</D:href>\
                 </D:current-user-principal><D:resourcetype><D:collection/></D:resourcetype>",
            ))
        }
        ("PROPFIND", "/dav/principals/ada/") => multistatus(ok(
            "/dav/principals/ada/",
            "<C:addressbook-home-set><D:href>/dav/addressbooks/ada/</D:href></C:addressbook-home-set>\
             <K:calendar-home-set><D:href>http://ignored.example/dav/calendars/ada/</D:href></K:calendar-home-set>",
        )
        .replace("http://ignored.example", "")),
        ("PROPFIND", home @ ("/dav/addressbooks/ada/" | "/dav/calendars/ada/")) => {
            let mut inner = ok(home, "<D:resourcetype><D:collection/></D:resourcetype>");
            if depth == "1" {
                for (p, coll) in s.collections.iter().filter(|(p, _)| p.starts_with(home)) {
                    inner.push_str(&ok(p, &coll_props(&s, p, coll)));
                }
            }
            multistatus(inner)
        }
        ("PROPFIND", p) if s.collections.contains_key(p) => {
            let coll = &s.collections[p];
            let mut inner = ok(p, &coll_props(&s, p, coll));
            if depth == "1" {
                for (name, (etag, _)) in &coll.items {
                    inner.push_str(&ok(
                        &format!("{p}{name}"),
                        &format!(
                            "<D:getetag>\"{etag}\"</D:getetag><D:resourcetype/>\
                             <D:getcontenttype>text/plain</D:getcontenttype>"
                        ),
                    ));
                }
            }
            multistatus(inner)
        }
        ("REPORT", p) if s.collections.contains_key(p) => {
            let request = xml::parse(&body).unwrap();
            if request.is(DAV, "sync-collection") {
                if s.no_sync {
                    return StatusCode::NOT_IMPLEMENTED.into_response();
                }
                let raw = request
                    .child(DAV, "sync-token")
                    .and_then(Element::trimmed)
                    .unwrap_or("")
                    .to_owned();
                let since = if raw.is_empty() {
                    None
                } else {
                    let parsed = raw
                        .rsplit('/')
                        .next()
                        .and_then(|t| t.split_once('-'))
                        .and_then(|(g, q)| Some((g.parse::<u64>().ok()?, q.parse::<u64>().ok()?)));
                    match parsed {
                        Some((generation, seq)) if generation == s.generation => Some(seq),
                        _ => {
                            return (
                                StatusCode::FORBIDDEN,
                                "<?xml version=\"1.0\"?><d:error xmlns:d=\"DAV:\"><d:valid-sync-token/></d:error>",
                            )
                                .into_response();
                        }
                    }
                };
                let coll = &s.collections[p];
                let mut latest: BTreeMap<&String, u64> = BTreeMap::new();
                for (seq, name) in &coll.log {
                    if since.is_none_or(|since| *seq > since) {
                        latest.insert(name, *seq);
                    }
                }
                let mut touched: Vec<(u64, &String)> =
                    latest.into_iter().map(|(n, q)| (q, n)).collect();
                touched.sort();
                if since.is_none() {
                    touched.retain(|(_, n)| coll.items.contains_key(*n));
                }
                let mut upto = coll.log.last().map_or(0, |(q, _)| *q).max(since.unwrap_or(0));
                let mut truncated = false;
                if let Some(page) = s.page
                    && touched.len() > page
                {
                    touched.truncate(page);
                    upto = if s.stuck {
                        since.unwrap_or(0)
                    } else {
                        touched.last().unwrap().0
                    };
                    truncated = true;
                }
                let mut inner = String::new();
                for (_, name) in &touched {
                    let href = format!("{p}{name}");
                    match coll.items.get(*name) {
                        Some((etag, _)) => inner.push_str(&ok(
                            &href,
                            &format!("<D:getetag>\"{etag}\"</D:getetag>"),
                        )),
                        None => inner.push_str(&format!(
                            "<D:response><D:href>{}</D:href>\
                             <D:status>HTTP/1.1 404 Not Found</D:status></D:response>",
                            path::encode(&href)
                        )),
                    }
                }
                if truncated {
                    inner.push_str(&format!(
                        "<D:response><D:href>{p}</D:href>\
                         <D:status>HTTP/1.1 507 Insufficient Storage</D:status></D:response>"
                    ));
                }
                inner.push_str(&format!("<D:sync-token>{}</D:sync-token>", token(&s, upto)));
                return multistatus(inner);
            }
            if s.no_multiget {
                return StatusCode::NOT_IMPLEMENTED.into_response();
            }
            let coll = &s.collections[p];
            let tag = match coll.kind {
                Some(CollectionKind::Contacts) => "C:address-data",
                _ => "K:calendar-data",
            };
            let mut inner = String::new();
            for href in request.children(DAV, "href").filter_map(Element::trimmed) {
                let id = path::id(&Url::parse(&format!("http://x{href}")).unwrap());
                match id.strip_prefix(p).and_then(|n| coll.items.get(n)) {
                    Some((etag, raw)) => inner.push_str(&ok(
                        &id,
                        &format!(
                            "<D:getetag>\"{etag}\"</D:getetag><{tag}>{}</{tag}>",
                            esc(&String::from_utf8_lossy(raw))
                        ),
                    )),
                    None => inner.push_str(&format!(
                        "<D:response><D:href>{href}</D:href>\
                         <D:status>HTTP/1.1 404 Not Found</D:status></D:response>"
                    )),
                }
            }
            multistatus(inner)
        }
        ("GET", p) => {
            let found = s.collections.iter().find_map(|(c, coll)| {
                p.strip_prefix(c.as_str())
                    .and_then(|n| coll.items.get(n))
                    .map(|(_, raw)| raw.clone())
            });
            match found {
                Some(raw) => raw.into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve(fake: FakeDav) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().fallback(handle).with_state(fake);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

fn password() -> Credentials {
    Credentials::Password {
        username: "ada@example.com".into(),
        password: "app-secret".into(),
    }
}

#[tokio::test]
async fn keeps_the_contract_through_discovery() {
    let fake = FakeDav::standard();
    let base = serve(fake.clone()).await;
    let source = DavSource::new(DavConfig::at(&base, password())).unwrap();
    conformance(&source, &fake).await;
    let requests = fake.requests();
    assert!(
        requests
            .iter()
            .filter(|(_, p, _)| p.starts_with("/.well-known/"))
            .all(|(_, _, auth)| !auth),
        "the well-known lookup goes without credentials"
    );
}

#[tokio::test]
async fn keeps_the_contract_without_sync_or_multiget() {
    let fake = FakeDav::standard();
    {
        let mut s = fake.0.lock().unwrap();
        s.no_sync = true;
        s.no_multiget = true;
    }
    let base = serve(fake.clone()).await;
    let source = DavSource::new(DavConfig::at(&format!("{base}/dav/"), password())).unwrap();
    conformance(&source, &fake).await;
    assert!(fake.requests().iter().any(|(m, _, _)| m == "GET"));
}

#[tokio::test]
async fn lists_collections_with_names_colors_and_components() {
    let fake = FakeDav::standard();
    let base = serve(fake.clone()).await;
    let source = DavSource::new(DavConfig {
        contacts: Some(base.clone()),
        calendars: Some(format!("{base}/.well-known/caldav")),
        credentials: Credentials::Bearer("token-1".into()),
        timeout: Duration::from_secs(10),
    })
    .unwrap();
    let listed = source.collections().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, BOOK);
    assert_eq!(listed[0].kind, CollectionKind::Contacts);
    assert_eq!(listed[0].name, "Contacts");
    assert!(listed[0].components.is_empty());
    assert_eq!(listed[1].id, CAL);
    assert_eq!(listed[1].name, "Work & Life");
    assert_eq!(listed[1].color.as_deref(), Some("#336699FF"));
    assert_eq!(listed[1].components, ["VEVENT", "VTODO"]);
    assert_eq!(listed[1].tag.as_deref(), Some("ctag-0"));
}

#[tokio::test]
async fn a_single_collection_address_is_that_collection() {
    let fake = FakeDav::standard();
    let base = serve(fake.clone()).await;
    let source = DavSource::new(DavConfig {
        contacts: None,
        calendars: Some(format!("{base}{CAL}")),
        credentials: password(),
        timeout: Duration::from_secs(10),
    })
    .unwrap();
    let listed = source.collections().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, CAL);
}

#[tokio::test]
async fn a_sync_token_that_does_not_move_on_is_refused() {
    let fake = FakeDav::standard();
    for name in ["a.vcf", "b.vcf", "c.vcf"] {
        fake.put_item(
            BOOK,
            name,
            b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Someone\r\nEND:VCARD\r\n",
        );
    }
    {
        let mut s = fake.0.lock().unwrap();
        s.page = Some(1);
        s.stuck = true;
    }
    let base = serve(fake.clone()).await;
    let source = DavSource::new(DavConfig::at(&base, password())).unwrap();
    source.collections().await.unwrap();
    let error = source.changes(BOOK, None).await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    let reports = fake
        .requests()
        .iter()
        .filter(|(m, _, _)| m == "REPORT")
        .count();
    assert_eq!(reports, 2, "stops at the first repeat");
}

#[tokio::test]
async fn refused_credentials_are_an_auth_error() {
    let fake = FakeDav::standard();
    let base = serve(fake).await;
    let source = DavSource::new(DavConfig::at(
        &base,
        Credentials::Password {
            username: "ada@example.com".into(),
            password: "wrong".into(),
        },
    ))
    .unwrap();
    assert!(matches!(
        source.collections().await,
        Err(CollectionError::Auth(_))
    ));
}

#[tokio::test]
async fn credentials_never_follow_a_redirect_to_another_server() {
    let elsewhere = FakeDav::standard();
    let other = serve(elsewhere.clone()).await;
    let fake = FakeDav::standard();
    fake.0.lock().unwrap().redirect_root = Some(format!("{other}/dav/"));
    let base = serve(fake).await;
    let source = DavSource::new(DavConfig::at(&format!("{base}/dav/"), password())).unwrap();
    let error = source.collections().await.unwrap_err();
    assert!(matches!(error, CollectionError::Protocol(_)), "{error}");
    assert!(
        elsewhere.requests().is_empty(),
        "nothing reached the other server"
    );
}

#[tokio::test]
async fn busy_servers_are_waited_out_and_long_change_lists_paged() {
    let fake = FakeDav::standard();
    for n in 0..7 {
        fake.put_item(
            BOOK,
            &format!("p{n}.vcf"),
            format!("BEGIN:VCARD\r\nFN:P{n}\r\nEND:VCARD\r\n").as_bytes(),
        );
    }
    {
        let mut s = fake.0.lock().unwrap();
        s.busy = 2;
        s.page = Some(3);
    }
    let base = serve(fake.clone()).await;
    let source = DavSource::new(DavConfig::at(&base, password())).unwrap();
    source.collections().await.unwrap();
    let all = source.changes(BOOK, None).await.unwrap();
    assert_eq!(all.changed.len(), 7);
    let reports = fake
        .requests()
        .iter()
        .filter(|(m, _, _)| m == "REPORT")
        .count();
    assert_eq!(reports, 3, "three pages of at most three");
    let again = source.changes(BOOK, Some(&all.token)).await.unwrap();
    assert!(again.changed.is_empty());
    let fetched = source
        .fetch(
            BOOK,
            &all.changed.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    assert_eq!(fetched.len(), 7);
}

#[test]
fn only_https_or_loopback_addresses() {
    let config = |url: &str| DavSource::new(DavConfig::at(url, password()));
    assert!(config("http://dav.example.com").is_err());
    assert!(config("ftp://dav.example.com").is_err());
    assert!(config("https://ada:pw@dav.example.com").is_err());
    assert!(config("dav.example.com").is_ok(), "https is assumed");
    assert!(config("http://127.0.0.1:5232/").is_ok());
    assert!(
        DavSource::new(DavConfig {
            contacts: None,
            calendars: None,
            credentials: password(),
            timeout: Duration::from_secs(1),
        })
        .is_err()
    );
}
