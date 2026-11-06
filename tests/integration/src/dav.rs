//! CardDAV and CalDAV servers of the stack (Radicale, Baïkal, Stalwart, and Nextcloud with its
//! profile) and a client that seeds them: users, address books, calendars, items.

use std::process::Command;

use reqwest::{Method, StatusCode};

use crate::services::{DOVECOT, ImapServer, STALWART};

/// Radicale over https, with a certificate from the stack's CA for `localhost`.
pub const RADICALE_PORT: u16 = 44232;
/// Baïkal over https (nginx in its container listens on the same port, so its absolute
/// redirects name it).
pub const BAIKAL_PORT: u16 = 45443;
/// Nextcloud over https, only with `IT_PROFILES=nextcloud`.
pub const NEXTCLOUD_PORT: u16 = 46443;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Radicale,
    Baikal,
    Stalwart,
    Nextcloud,
}

/// A CardDAV and CalDAV server of the stack, with the IMAP server whose accounts carry its
/// contacts and calendars (the same user name and password sign in to both).
#[derive(Debug, Clone, Copy)]
pub struct DavServer {
    /// A short name for messages and collection names.
    pub name: &'static str,
    backend: Backend,
    /// The https port on this machine.
    pub port: u16,
    contacts_root: &'static str,
    calendars_root: &'static str,
    /// The mail server of the same users.
    pub imap: ImapServer,
}

/// Radicale 3: file storage, `sync-collection`, a well-known redirect to `/`; users from
/// `.state/radicale-users`, read on every sign-in.
pub const RADICALE: DavServer = DavServer {
    name: "radicale",
    backend: Backend::Radicale,
    port: RADICALE_PORT,
    contacts_root: "/",
    calendars_root: "/",
    imap: DOVECOT,
};

/// Baïkal 0.10 (sabre/dav) with SQLite and Basic sign-in under `/dav.php/`; the well-known
/// addresses redirect there.
pub const BAIKAL: DavServer = DavServer {
    name: "baikal",
    backend: Backend::Baikal,
    port: BAIKAL_PORT,
    contacts_root: "/dav.php/",
    calendars_root: "/dav.php/",
    imap: DOVECOT,
};

/// Stalwart's CardDAV (`/dav/card`) and CalDAV (`/dav/cal`) on its HTTPS port.
pub const STALWART_DAV: DavServer = DavServer {
    name: "stalwart",
    backend: Backend::Stalwart,
    port: crate::stalwart::STALWART_HTTPS,
    contacts_root: "/dav/card/",
    calendars_root: "/dav/cal/",
    imap: STALWART,
};

/// Nextcloud (sabre/dav under `/remote.php/dav/`), only with the `nextcloud` profile.
pub const NEXTCLOUD: DavServer = DavServer {
    name: "nextcloud",
    backend: Backend::Nextcloud,
    port: NEXTCLOUD_PORT,
    contacts_root: "/remote.php/dav/",
    calendars_root: "/remote.php/dav/",
    imap: DOVECOT,
};

/// The address book and calendar homes of one user, as absolute addresses.
#[derive(Debug, Clone)]
pub struct Homes {
    /// `addressbook-home-set`.
    pub contacts: String,
    /// `calendar-home-set`.
    pub calendars: String,
}

impl DavServer {
    /// `https://localhost:<port>`, the address the well-known lookup starts from.
    pub fn url(&self) -> String {
        format!("https://localhost:{}", self.port)
    }

    /// The password of the users [`DavServer::user`] makes, the same as on [`DavServer::imap`].
    pub fn password(&self) -> &'static str {
        self.imap.password
    }

    /// A fresh user on this server and its mail server, with [`DavServer::password`].
    pub async fn user(&self, prefix: &str) -> String {
        let user = self.imap.user(prefix);
        self.add_user(&user, self.password()).await;
        user
    }

    /// Creates `user` with `password` on the DAV server only.
    pub async fn add_user(&self, user: &str, password: &str) {
        match self.backend {
            Backend::Radicale => {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(crate::state_dir().join("radicale-users"))
                    .expect("the Radicale users file");
                file.write_all(format!("{user}:{password}\n").as_bytes())
                    .expect("written");
                drop(file);
                let dav = Dav::new(user, password);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    let (status, _) = dav
                        .request_with(
                            "PROPFIND",
                            &self.url(),
                            "application/xml",
                            Some("0"),
                            String::new(),
                        )
                        .await;
                    if status != StatusCode::UNAUTHORIZED {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Radicale does not sign {user} in"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
            Backend::Baikal => {
                baikal_php(
                    "$u=$argv[1];$p=$argv[2];\
                     $db->prepare('INSERT INTO users (username, digesta1) VALUES (?, ?)')\
                       ->execute([$u, md5($u.':BaikalDAV:'.$p)]);\
                     $db->prepare('INSERT INTO principals (uri, email, displayname) VALUES (?, ?, ?)')\
                       ->execute(['principals/'.$u, $u, $u]);",
                    &[user, password],
                );
            }
            Backend::Stalwart => {
                assert_eq!(
                    password,
                    self.password(),
                    "Stalwart users share one password"
                );
                crate::stalwart::create_user(user).await;
            }
            Backend::Nextcloud => {
                let output = Command::new("docker")
                    .args(["compose", "-f"])
                    .arg(compose_file())
                    .args(["exec", "-T", "-u", "www-data", "-e"])
                    .arg(format!("OC_PASS={password}"))
                    .args([
                        "nextcloud",
                        "php",
                        "occ",
                        "user:add",
                        "--password-from-env",
                        user,
                    ])
                    .output()
                    .expect("docker runs");
                assert!(
                    output.status.success(),
                    "occ user:add: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    /// A seeding client for `user` with [`DavServer::password`].
    pub fn client(&self, user: &str) -> Dav {
        Dav::new(user, self.password())
    }

    /// The homes of the signed-in user of `dav`, found from the server's DAV root through the
    /// current principal.
    pub async fn homes(&self, dav: &Dav) -> Homes {
        Homes {
            contacts: dav
                .home(
                    &format!("{}{}", self.url(), self.contacts_root),
                    "addressbook-home-set",
                )
                .await,
            calendars: dav
                .home(
                    &format!("{}{}", self.url(), self.calendars_root),
                    "calendar-home-set",
                )
                .await,
        }
    }

    /// Whether this is Baïkal, whose database the tests may change behind its back.
    pub fn is_baikal(&self) -> bool {
        self.backend == Backend::Baikal
    }

    /// Whether this is Stalwart.
    pub fn is_stalwart(&self) -> bool {
        self.backend == Backend::Stalwart
    }
}

fn compose_file() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("compose.yaml")
}

/// Runs PHP `code` in the Baïkal container with `$db` open on its SQLite database and `args`
/// as `$argv[1..]`; returns what it prints.
pub fn baikal_php(code: &str, args: &[&str]) -> String {
    let script = format!(
        "$db = new PDO('sqlite:/var/www/baikal/Specific/db/db.sqlite');\
         $db->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);\
         $db->exec('PRAGMA busy_timeout = 10000');{code}"
    );
    let output = Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose_file())
        .args([
            "exec", "-T", "-u", "nginx", "baikal", "php", "-r", &script, "--",
        ])
        .args(args)
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "PHP in Baïkal: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Puts `body` into Baïkal's database as the item `name` of the address book or calendar at
/// `collection` (its address), with a change record, without the server checking it: for
/// objects Baïkal would refuse over DAV.
pub fn baikal_insert(collection: &str, name: &str, body: &str) {
    let path = reqwest::Url::parse(collection)
        .expect("an address")
        .path()
        .trim_end_matches('/')
        .to_owned();
    let mut parts = path.rsplitn(3, '/');
    let uri = parts.next().expect("a collection").to_owned();
    let user = decode(parts.next().expect("a user"));
    let kind = parts.next().expect("a home");
    let code = if kind.ends_with("addressbooks") {
        "[$user, $uri, $name, $data] = array_slice($argv, 1);\
         $s = $db->prepare('SELECT id, synctoken FROM addressbooks WHERE principaluri = ? AND uri = ?');\
         $s->execute(['principals/'.$user, $uri]); [$id, $token] = $s->fetch(PDO::FETCH_NUM);\
         $db->prepare('INSERT INTO cards (addressbookid, carddata, uri, lastmodified, etag, size) VALUES (?, ?, ?, ?, ?, ?)')\
           ->execute([$id, $data, $name, time(), md5($data), strlen($data)]);\
         $db->prepare('INSERT INTO addressbookchanges (uri, synctoken, addressbookid, operation) VALUES (?, ?, ?, 1)')\
           ->execute([$name, $token, $id]);\
         $db->prepare('UPDATE addressbooks SET synctoken = synctoken + 1 WHERE id = ?')->execute([$id]);"
    } else {
        "[$user, $uri, $name, $data] = array_slice($argv, 1);\
         $s = $db->prepare('SELECT calendarid FROM calendarinstances WHERE principaluri = ? AND uri = ?');\
         $s->execute(['principals/'.$user, $uri]); $id = $s->fetchColumn();\
         $token = $db->query('SELECT synctoken FROM calendars WHERE id = '.intval($id))->fetchColumn();\
         $db->prepare('INSERT INTO calendarobjects (calendardata, uri, calendarid, lastmodified, etag, size, componenttype) VALUES (?, ?, ?, ?, ?, ?, ?)')\
           ->execute([$data, $name, $id, time(), md5($data), strlen($data), 'VEVENT']);\
         $db->prepare('INSERT INTO calendarchanges (uri, synctoken, calendarid, operation) VALUES (?, ?, ?, 1)')\
           ->execute([$name, $token, $id]);\
         $db->prepare('UPDATE calendars SET synctoken = synctoken + 1 WHERE id = ?')->execute([$id]);"
    };
    baikal_php(code, &[&user, &uri, name, body]);
}

fn decode(raw: &str) -> String {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("UTF-8")
}

/// The texts of the `href`s inside the first element named `local` (any namespace prefix) in
/// `xml`.
pub fn hrefs_in(xml: &str, local: &str) -> Vec<String> {
    let mut at = 0;
    while let Some(found) = xml[at..].find(local) {
        let start = at + found;
        at = start + local.len();
        let Some(lt) = xml[..start].rfind('<') else {
            continue;
        };
        let name = &xml[lt + 1..start];
        let after = xml[at..].chars().next();
        let opening = !name.starts_with('/')
            && !name.contains(['>', ' '])
            && (name.is_empty() || name.ends_with(':'))
            && matches!(after, Some('>' | ' ' | '/'));
        if !opening {
            continue;
        }
        let Some(open_end) = xml[at..].find('>') else {
            return Vec::new();
        };
        if xml[..at + open_end].ends_with('/') {
            return Vec::new();
        }
        let body_start = at + open_end + 1;
        let close = xml[body_start..]
            .find(&format!("{local}>"))
            .map_or(xml.len(), |e| body_start + e);
        let body = &xml[body_start..close];
        let mut out = Vec::new();
        let mut rest = body;
        while let Some(h) = rest.find("href>") {
            let text_start = h + "href>".len();
            let text_end = rest[text_start..]
                .find('<')
                .map_or(rest.len(), |e| text_start + e);
            let text = rest[text_start..text_end].trim();
            if !text.is_empty() {
                out.push(text.to_owned());
            }
            rest = &rest[text_end..];
        }
        return out;
    }
    Vec::new()
}

/// A DAV client signed in with HTTP Basic, trusting the stack's CA.
#[derive(Clone)]
pub struct Dav {
    client: reqwest::Client,
    user: String,
    password: String,
}

impl Dav {
    /// A client for `user` with `password`.
    pub fn new(user: &str, password: &str) -> Self {
        Self {
            client: crate::services::http_client(),
            user: user.to_owned(),
            password: password.to_owned(),
        }
    }

    /// Sends `method` (such as `MKCOL`, `MKCALENDAR`, `PROPFIND`) to `url` with a body and
    /// returns the status and body.
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        content_type: &str,
        body: String,
    ) -> (StatusCode, String) {
        self.request_with(method, url, content_type, None, body)
            .await
    }

    async fn request_with(
        &self,
        method: &str,
        url: &str,
        content_type: &str,
        depth: Option<&str>,
        body: String,
    ) -> (StatusCode, String) {
        let method = Method::from_bytes(method.as_bytes()).expect("a method");
        let mut request = self
            .client
            .request(method, url)
            .basic_auth(&self.user, Some(&self.password))
            .header("Content-Type", content_type)
            .body(body);
        if let Some(depth) = depth {
            request = request.header("Depth", depth);
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|e| panic!("DAV at {url}: {e}"));
        let status = response.status();
        (status, response.text().await.unwrap_or_default())
    }

    /// `PROPFIND` with `Depth` and the `prop` children in `props` (with the prefixes `d`,
    /// `card`, `cal`); the multistatus text.
    pub async fn propfind(&self, url: &str, depth: &str, props: &str) -> String {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:propfind xmlns:d=\"DAV:\" \
             xmlns:card=\"urn:ietf:params:xml:ns:carddav\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">\
             <d:prop>{props}</d:prop></d:propfind>"
        );
        let (status, text) = self
            .request_with(
                "PROPFIND",
                url,
                "application/xml; charset=utf-8",
                Some(depth),
                body,
            )
            .await;
        assert_eq!(status.as_u16(), 207, "PROPFIND {url}: {status} {text}");
        text
    }

    async fn home(&self, root: &str, home_set: &str) -> String {
        let prefix = if home_set.starts_with("addressbook") {
            "card"
        } else {
            "cal"
        };
        let props = format!("<d:current-user-principal/><{prefix}:{home_set}/>");
        let text = self.propfind(root, "0", &props).await;
        let base = reqwest::Url::parse(root).expect("an address");
        if let Some(home) = hrefs_in(&text, home_set).first() {
            return base.join(home).expect("a home").to_string();
        }
        let principal = hrefs_in(&text, "current-user-principal")
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("no principal at {root}: {text}"));
        let principal = base.join(&principal).expect("a principal").to_string();
        let text = self
            .propfind(&principal, "0", &format!("<{prefix}:{home_set}/>"))
            .await;
        let home = hrefs_in(&text, home_set)
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("no {home_set} at {principal}: {text}"));
        base.join(&home).expect("a home").to_string()
    }

    /// Creates the address book `slug` named `name` in `home` and returns its address.
    pub async fn create_address_book(&self, home: &str, slug: &str, name: &str) -> String {
        let url = format!("{home}{slug}/");
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:mkcol xmlns:d=\"DAV:\" \
             xmlns:card=\"urn:ietf:params:xml:ns:carddav\"><d:set><d:prop><d:resourcetype>\
             <d:collection/><card:addressbook/></d:resourcetype><d:displayname>{name}</d:displayname>\
             </d:prop></d:set></d:mkcol>"
        );
        let (status, text) = self
            .request("MKCOL", &url, "application/xml; charset=utf-8", body)
            .await;
        assert!(status.is_success(), "MKCOL {url}: {status} {text}");
        url
    }

    /// Creates the calendar `slug` named `name` for events in `home` and returns its address.
    pub async fn create_calendar(&self, home: &str, slug: &str, name: &str) -> String {
        let url = format!("{home}{slug}/");
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><cal:mkcalendar xmlns:d=\"DAV:\" \
             xmlns:cal=\"urn:ietf:params:xml:ns:caldav\"><d:set><d:prop>\
             <d:displayname>{name}</d:displayname><cal:supported-calendar-component-set>\
             <cal:comp name=\"VEVENT\"/></cal:supported-calendar-component-set></d:prop></d:set>\
             </cal:mkcalendar>"
        );
        let (status, text) = self
            .request("MKCALENDAR", &url, "application/xml; charset=utf-8", body)
            .await;
        assert!(status.is_success(), "MKCALENDAR {url}: {status} {text}");
        url
    }

    /// Puts a vCard as `<name>.vcf` into the address book at `collection`; returns its address.
    pub async fn put_card(&self, collection: &str, name: &str, card: &str) -> String {
        let url = format!("{collection}{name}.vcf");
        self.put(&url, "text/vcard; charset=utf-8", card).await;
        url
    }

    /// Puts an iCalendar object as `<name>.ics` into the calendar at `collection`; returns its
    /// address.
    pub async fn put_event(&self, collection: &str, name: &str, calendar: &str) -> String {
        let url = format!("{collection}{name}.ics");
        self.put(&url, "text/calendar; charset=utf-8", calendar)
            .await;
        url
    }

    /// Puts a vCard or iCalendar object at `url`, checking that the server stored it.
    pub async fn put(&self, url: &str, content_type: &str, body: &str) {
        let (status, text) = self
            .request("PUT", url, content_type, body.to_owned())
            .await;
        assert!(status.is_success(), "PUT {url}: {status} {text}");
    }

    /// `GET url`: the status and body.
    pub async fn get(&self, url: &str) -> (StatusCode, String) {
        self.request("GET", url, "text/plain", String::new()).await
    }

    /// Deletes the item or collection at `url`, checking that it is gone.
    pub async fn delete(&self, url: &str) {
        let (status, text) = self
            .request("DELETE", url, "text/plain", String::new())
            .await;
        assert!(
            status.is_success() || status == StatusCode::NOT_FOUND,
            "DELETE {url}: {status} {text}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_hrefs_by_local_name() {
        let xml = "<D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>/dav/card/</D:href>\
                   <D:propstat><D:prop><D:current-user-principal><D:href>/dav/pal/a%40example.com/</D:href>\
                   </D:current-user-principal><B:addressbook-home-set><D:href>/dav/card/a%40example.com/</D:href>\
                   </B:addressbook-home-set></D:prop></D:propstat></D:response></D:multistatus>";
        assert_eq!(
            hrefs_in(xml, "current-user-principal"),
            vec!["/dav/pal/a%40example.com/"]
        );
        assert_eq!(
            hrefs_in(xml, "addressbook-home-set"),
            vec!["/dav/card/a%40example.com/"]
        );
        let empty = "<multistatus><prop><CR:addressbook-home-set /><current-user-principal>\
                     <href>/a/</href></current-user-principal></prop></multistatus>";
        assert!(hrefs_in(empty, "addressbook-home-set").is_empty());
        assert_eq!(hrefs_in(empty, "current-user-principal"), vec!["/a/"]);
        assert_eq!(decode("a%40example.com"), "a@example.com");
    }
}
