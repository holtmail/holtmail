//! Device credentials through the real binary: an app password shown once and never stored in
//! clear, a new device read-only unless asked, an old sign-in refused for making a device and
//! raising its rights, renaming and lowering allowed, revocation permanent, audit entries, the
//! `[serve] members` rule, `X-Forwarded-For` believed only from `trusted_proxies`, and a broken
//! `[serve]` section refused at start. Over SMTP: only the app password of a device that may send
//! signs in, repeated failures are limited, and revoking or lowering a device ends its open
//! session. Over IMAP, JMAP and CalDAV/CardDAV: only a device with that protocol signs in (never
//! the web password, another protocol's device or a browser session), and revoking one ends its
//! IMAP session and refuses its next HTTP request. Client setup: a one-time profile link gives a
//! fresh app password that signs in over IMAP and DAV while the old one stops, Thunderbird
//! autoconfig answers only for `[serve] mail_domains`, and `holtmail dns` prints their records.

use std::net::TcpListener;
use std::time::Duration;

use base64::Engine;
use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD, id};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, state_dir, unique};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, ORIGIN};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const MEMBER_EMAIL: &str = "devices-member@example.com";
const MEMBER_PASSWORD: &str = "it-devices-member-1";
const WRONG: &str = "it-devices-wrong-1";

async fn with_member(holtmail: &Holtmail, owner: &Api, archives: &[&str]) -> Api {
    owner
        .post(
            "/api/users",
            json!({ "email": MEMBER_EMAIL, "name": "Member Example", "password": MEMBER_PASSWORD, "archives": archives }),
        )
        .await
        .ok();
    Api::sign_in(holtmail, MEMBER_EMAIL, MEMBER_PASSWORD).await
}

async fn age_sessions(holtmail: &Holtmail) {
    assert!(
        holtmail
            .execute("UPDATE sessions SET created_at = created_at - 600000", &[])
            .await
            > 0
    );
}

fn scanner() -> LogScanner {
    LogScanner::new()
        .secret("the owner's email", OWNER_EMAIL)
        .secret("the owner's password", OWNER_PASSWORD)
        .secret("the member's email", MEMBER_EMAIL)
        .secret("the member's password", MEMBER_PASSWORD)
        .secret("a wrong password", WRONG)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_shows_its_secret_once_and_can_be_revoked() {
    require_it!();
    let holtmail = Holtmail::start_default().await;
    let owner = Api::setup_owner(&holtmail).await;
    let archive = owner.create_archive("Device mail").await;
    let member = with_member(&holtmail, &owner, &[&archive]).await;

    let made = member
        .post(
            "/api/devices",
            json!({ "name": "Member's phone", "protocols": ["imap", "dav"], "mail_archive": archive }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let secret = made.body["secret"].as_str().expect("a secret").to_owned();
    assert!(secret.starts_with("hm_") && secret.len() == 35);
    let device = id(&made.body["device"]);
    assert_eq!(
        made.body["device"]["scopes"],
        json!(["read"]),
        "read-only by default"
    );
    assert_eq!(made.body["device"]["mail_archive"], json!(archive));

    let listed = member.get("/api/devices").await.ok();
    assert_eq!(listed.as_array().expect("a list").len(), 1, "{listed}");
    assert!(listed[0].get("secret").is_none());
    assert!(
        !listed.to_string().contains(&secret),
        "the secret came back"
    );
    assert_eq!(
        owner.get("/api/devices").await.ok(),
        json!([]),
        "the owner sees only their own devices"
    );

    let stored = holtmail
        .query(
            "SELECT lower(hex(secret_hash)) FROM devices WHERE id = ?",
            &[&device],
        )
        .await;
    assert_eq!(stored.len(), 1);
    assert_ne!(stored[0][0], "", "no hash stored");
    assert!(
        !stored[0][0].contains(&secret),
        "the secret was stored in clear"
    );

    let uri = format!("/api/devices/{device}");
    let raised = member
        .patch(
            &uri,
            json!({ "scopes": ["read", "write:mail"], "name": "Phone" }),
        )
        .await
        .ok();
    assert_eq!(raised["scopes"], json!(["read", "write:mail"]));
    assert_eq!(raised["name"], "Phone");

    let foreign = owner.patch(&uri, json!({ "name": "Taken" })).await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.body);
    let foreign = owner.delete(&uri).await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);

    let revoked = member.delete(&uri).await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    let listed = member.get("/api/devices").await.ok();
    assert!(listed[0]["revoked_at"].is_number(), "{listed}");
    let again = member.delete(&uri).await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    let dead = member.patch(&uri, json!({ "name": "Back" })).await;
    assert_eq!(dead.status, StatusCode::BAD_REQUEST, "{}", dead.body);

    let audit = owner.get("/api/audit").await.ok();
    let kinds: Vec<&str> = audit
        .as_array()
        .expect("audit entries")
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .filter(|k| k.starts_with("device_"))
        .collect();
    assert_eq!(
        kinds,
        ["device_revoked", "device_changed", "device_created"],
        "{audit}"
    );
    assert!(
        !audit.to_string().contains(&secret),
        "the secret was audited"
    );
    scanner()
        .secret("the app password", &secret)
        .check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_sign_in_cannot_make_a_device_or_raise_its_rights() {
    require_it!();
    let holtmail = Holtmail::start_default().await;
    let owner = Api::setup_owner(&holtmail).await;
    let made = owner
        .post(
            "/api/devices",
            json!({ "name": "Tablet", "protocols": ["jmap", "dav"] }),
        )
        .await
        .ok();
    let uri = format!("/api/devices/{}", id(&made["device"]));

    age_sessions(&holtmail).await;
    let refused = owner
        .post(
            "/api/devices",
            json!({ "name": "Phone", "protocols": ["jmap"] }),
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::PRECONDITION_REQUIRED,
        "{}",
        refused.body
    );
    let raise = owner
        .patch(&uri, json!({ "scopes": ["read", "send:mail"] }))
        .await;
    assert_eq!(
        raise.status,
        StatusCode::PRECONDITION_REQUIRED,
        "{}",
        raise.body
    );
    let lower = owner
        .patch(&uri, json!({ "name": "Old tablet", "protocols": ["jmap"] }))
        .await;
    assert_eq!(lower.status, StatusCode::OK, "{}", lower.body);
    let revoked = owner.delete(&uri).await;
    assert_eq!(
        revoked.status,
        StatusCode::NO_CONTENT,
        "revoking needs no fresh sign-in"
    );

    let fresh = Api::sign_in(&holtmail, OWNER_EMAIL, OWNER_PASSWORD).await;
    let made = fresh
        .post(
            "/api/devices",
            json!({ "name": "Phone", "protocols": ["jmap"] }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    scanner()
        .secret(
            "the app password",
            made.body["secret"].as_str().expect("a secret"),
        )
        .check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_config_can_keep_devices_to_owners_and_admins() {
    require_it!();
    let holtmail = Holtmail::start(Options {
        extra_config: "[serve]\nmembers = false\n".into(),
        ..Options::default()
    })
    .await;
    let owner = Api::setup_owner(&holtmail).await;
    let archive = owner.create_archive("Device mail").await;
    let member = with_member(&holtmail, &owner, &[&archive]).await;
    let refused = member
        .post(
            "/api/devices",
            json!({ "name": "Phone", "protocols": ["jmap"] }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(member.get("/api/devices").await.ok(), json!([]));
    let made = owner
        .post(
            "/api/devices",
            json!({ "name": "Ipad", "protocols": ["jmap"] }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    scanner().check(&holtmail.logs());
}

async fn login_from(holtmail: &Holtmail, password: &str, forwarded: &str) -> StatusCode {
    reqwest::Client::new()
        .post(format!("{}/api/login", holtmail.url()))
        .header(ORIGIN, holtmail.url())
        .header(CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", forwarded)
        .body(json!({ "email": OWNER_EMAIL, "password": password }).to_string())
        .send()
        .await
        .expect("an answer")
        .status()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sign_in_limit_counts_forwarded_clients_of_a_trusted_proxy() {
    require_it!();
    let holtmail = Holtmail::start(Options {
        extra_config: "[serve]\ntrusted_proxies = [\"127.0.0.1\"]\n".into(),
        ..Options::default()
    })
    .await;
    let _owner = Api::setup_owner(&holtmail).await;
    for attempt in 1..=10 {
        assert_eq!(
            login_from(&holtmail, WRONG, "203.0.113.9").await,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}"
        );
    }
    assert_eq!(
        login_from(&holtmail, OWNER_PASSWORD, "203.0.113.9").await,
        StatusCode::TOO_MANY_REQUESTS,
        "the client that failed is limited"
    );
    assert_eq!(
        login_from(&holtmail, OWNER_PASSWORD, "203.0.113.10").await,
        StatusCode::OK,
        "another client behind the same proxy is not"
    );
    scanner().check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn certificates_load_and_a_broken_serve_section_refuses_to_start() {
    require_it!();
    let cert = state_dir().join("server.crt");
    let key = state_dir().join("server.key");
    let serve = format!(
        "[serve]\ntls_cert = {:?}\ntls_key = {:?}\n",
        cert.display().to_string(),
        key.display().to_string()
    );
    let mut holtmail = Holtmail::start(Options {
        extra_config: serve,
        ..Options::default()
    })
    .await;
    holtmail.stop().await;

    let config = std::fs::read_to_string(holtmail.config_path()).expect("the config");
    let base = &config[..config.find("[serve]").expect("a serve section")];
    let broken: [(String, &str); 4] = [
        (
            format!(
                "{base}[serve]\ntls_cert = \"/nonexistent/c.pem\"\ntls_key = \"/nonexistent/k.pem\"\n"
            ),
            "tls_cert",
        ),
        (
            format!("{base}[serve]\ntls_cert = \"c.pem\"\n"),
            "go together",
        ),
        (
            format!("{base}[serve]\nproxy_protocol = true\n"),
            "trusted_proxies",
        ),
        (
            format!("{base}[serve]\ntrusted_proxies = [\"proxy.example.org\"]\n"),
            "trusted_proxies",
        ),
    ];
    for (text, expected) in broken {
        std::fs::write(holtmail.config_path(), &text).expect("the config");
        let output = holtmail.command(&["serve"], "");
        assert!(!output.status.success(), "{text}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{expected}: {stderr}");
    }
}

struct Smtp(BufReader<TcpStream>);

impl Smtp {
    async fn hello(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("the listener");
        let mut smtp = Self(BufReader::new(stream));
        assert_eq!(smtp.reply().await, "220");
        assert_eq!(smtp.say("EHLO client.example.com").await, "250");
        smtp
    }

    async fn reply(&mut self) -> String {
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout(Duration::from_secs(5), self.0.read_line(&mut line))
                .await
                .expect("an answer within five seconds")
                .expect("a line");
            if read == 0 {
                return "closed".into();
            }
            if line.as_bytes().get(3) != Some(&b'-') {
                return line.get(..3).unwrap_or_default().to_owned();
            }
        }
    }

    async fn say(&mut self, command: &str) -> String {
        self.0
            .get_mut()
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .expect("written");
        self.reply().await
    }

    async fn sign_in(&mut self, login: &str, secret: &str) -> String {
        let plain =
            base64::engine::general_purpose::STANDARD.encode(format!("\0{login}\0{secret}"));
        self.say(&format!("AUTH PLAIN {plain}")).await
    }
}

async fn sign_in_code(port: u16, secret: &str) -> String {
    Smtp::hello(port).await.sign_in(OWNER_EMAIL, secret).await
}

async fn smtp_device(
    owner: &Api,
    archive: &str,
    protocols: &[&str],
    scopes: &[&str],
) -> (String, String) {
    let made = owner
        .post(
            "/api/devices",
            json!({
                "name": unique("Mail client"),
                "protocols": protocols,
                "scopes": scopes,
                "mail_archive": archive,
            }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    (
        id(&made.body["device"]),
        made.body["secret"].as_str().expect("a secret").to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_takes_only_a_sending_devices_app_password() {
    require_it!();
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
        .port();
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            "[serve.submit]\nlisten_starttls = [\"127.0.0.1:{port}\"]\nhostname = \"localhost\"\n"
        ),
        ..Options::default()
    })
    .await;
    let owner = Api::setup_owner(&holtmail).await;
    let archive = owner.create_archive(&unique("sign-in")).await;
    let sends = ["read", "send:mail"];
    let setup = owner.get("/api/devices/setup").await.ok();
    assert_eq!(setup["can_create"], true);
    assert_eq!(
        setup["servers"],
        json!([{ "protocol": "smtp", "host": "localhost", "port": port, "security": "starttls", "url": null }])
    );

    let (revoked_id, revoked_secret) = smtp_device(&owner, &archive, &["smtp"], &sends).await;
    let (lowered_id, lowered_secret) = smtp_device(&owner, &archive, &["smtp"], &sends).await;
    let (renamed_id, renamed_secret) = smtp_device(&owner, &archive, &["smtp"], &sends).await;
    let (_, read_only) = smtp_device(&owner, &archive, &["smtp"], &["read"]).await;
    let (_, jmap_only) = smtp_device(&owner, &archive, &["jmap"], &sends).await;

    assert_eq!(
        sign_in_code(port, OWNER_PASSWORD).await,
        "535",
        "the web password"
    );
    assert_eq!(sign_in_code(port, &read_only).await, "535", "no send:mail");
    assert_eq!(sign_in_code(port, &jmap_only).await, "535", "smtp not on");

    let mut revoked = Smtp::hello(port).await;
    assert_eq!(revoked.sign_in(OWNER_EMAIL, &revoked_secret).await, "235");
    let answer = owner.delete(&format!("/api/devices/{revoked_id}")).await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT, "{}", answer.body);
    assert_eq!(revoked.reply().await, "421", "revoking ends the session");
    assert_eq!(sign_in_code(port, &revoked_secret).await, "535");

    let mut lowered = Smtp::hello(port).await;
    assert_eq!(lowered.sign_in(OWNER_EMAIL, &lowered_secret).await, "235");
    owner
        .patch(
            &format!("/api/devices/{lowered_id}"),
            json!({ "scopes": ["read"] }),
        )
        .await
        .ok();
    assert_eq!(
        lowered.reply().await,
        "421",
        "losing a right ends the session"
    );
    assert_eq!(sign_in_code(port, &lowered_secret).await, "535");

    let mut renamed = Smtp::hello(port).await;
    assert_eq!(renamed.sign_in(OWNER_EMAIL, &renamed_secret).await, "235");
    owner
        .patch(
            &format!("/api/devices/{renamed_id}"),
            json!({ "name": "Still the same rights" }),
        )
        .await
        .ok();
    assert_eq!(renamed.say("NOOP").await, "250", "a rename ends nothing");

    let mut attempts = 0;
    while sign_in_code(port, "hm_wrong").await == "535" {
        attempts += 1;
        assert!(attempts <= 10, "no limit after {attempts} failures");
    }
    assert_eq!(
        sign_in_code(port, &renamed_secret).await,
        "421",
        "a blocked client is refused even with the right secret"
    );

    let scanner = scanner()
        .secret("an app password", &renamed_secret)
        .secret("an app password", &revoked_secret);
    assert_eq!(scanner.findings(&holtmail.logs()), Vec::new());
}

struct Imap(BufReader<TcpStream>);

impl Imap {
    async fn greeted(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("the listener");
        let mut imap = Self(BufReader::new(stream));
        assert!(imap.line().await.starts_with("* OK"));
        imap
    }

    async fn line(&mut self) -> String {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(5), self.0.read_line(&mut line))
            .await
            .expect("an answer within five seconds")
            .expect("a line");
        if read == 0 { "closed".into() } else { line }
    }

    /// Sends `command` tagged `a` and returns the tagged answer's status, or `closed`.
    async fn tagged(&mut self, command: &str) -> String {
        self.0
            .get_mut()
            .write_all(format!("a {command}\r\n").as_bytes())
            .await
            .expect("written");
        loop {
            let line = self.line().await;
            if line == "closed" {
                return line;
            }
            if let Some(rest) = line.strip_prefix("a ") {
                return rest
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
            }
        }
    }

    async fn login(&mut self, secret: &str) -> String {
        self.tagged(&format!("LOGIN \"{OWNER_EMAIL}\" \"{secret}\""))
            .await
    }

    /// Lines until the server closes the connection or `wait` passes; whether it closed.
    async fn closes_within(&mut self, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let mut line = String::new();
            match tokio::time::timeout_at(deadline, self.0.read_line(&mut line)).await {
                Err(_) => return false,
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) if line.starts_with("* BYE") => return true,
                Ok(Ok(_)) => {}
            }
        }
    }
}

async fn http_status(
    client: &reqwest::Client,
    method: &str,
    url: &str,
    credentials: Option<&str>,
) -> StatusCode {
    let mut request = client
        .request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), url)
        .header("Depth", "0");
    if let Some(secret) = credentials {
        request = request.basic_auth(OWNER_EMAIL, Some(secret));
    }
    request.send().await.expect("an answer").status()
}

#[tokio::test(flavor = "multi_thread")]
async fn imap_jmap_and_dav_take_only_their_devices_app_password() {
    require_it!();
    let free = || {
        TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("an address")
            .port()
    };
    let (http, imap) = (free(), free());
    let base = format!("http://127.0.0.1:{http}");
    let holtmail = Holtmail::start(Options {
        port: Some(http),
        extra_config: format!(
            "public_url = \"{base}\"\n[serve.imap]\nlisten_starttls = [\"127.0.0.1:{imap}\"]\nhostname = \"localhost\"\n[serve.jmap]\n[serve.dav]\n"
        ),
        ..Options::default()
    })
    .await;
    let owner = Api::setup_owner(&holtmail).await;
    let archive = owner.create_archive(&unique("protocols")).await;
    let setup = owner.get("/api/devices/setup").await.ok();
    assert!(
        setup["servers"].as_array().is_some_and(|s| s
            .iter()
            .any(|r| r["protocol"] == "imap" && r["port"] == imap)),
        "{setup}"
    );

    let (imap_id, imap_secret) = smtp_device(&owner, &archive, &["imap"], &["read"]).await;
    let (_, smtp_only) = smtp_device(&owner, &archive, &["smtp"], &["read"]).await;
    let (jmap_id, jmap_secret) = smtp_device(&owner, &archive, &["jmap"], &["read"]).await;
    let (dav_id, dav_secret) = smtp_device(&owner, &archive, &["dav"], &["read"]).await;

    assert_eq!(Imap::greeted(imap).await.login(OWNER_PASSWORD).await, "NO");
    assert_eq!(Imap::greeted(imap).await.login(&smtp_only).await, "NO");
    assert_eq!(Imap::greeted(imap).await.login(&dav_secret).await, "NO");
    let mut session = Imap::greeted(imap).await;
    assert_eq!(session.login(&imap_secret).await, "OK");
    assert_eq!(session.tagged("LIST \"\" \"*\"").await, "OK");
    let answer = owner.delete(&format!("/api/devices/{imap_id}")).await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT, "{}", answer.body);
    assert!(
        session.closes_within(Duration::from_secs(5)).await,
        "revoking ends the IMAP session"
    );
    assert_eq!(Imap::greeted(imap).await.login(&imap_secret).await, "NO");

    let (idle_id, idle_secret) = smtp_device(&owner, &archive, &["imap"], &["read"]).await;
    let mut idling = Imap::greeted(imap).await;
    assert_eq!(idling.login(&idle_secret).await, "OK");
    idling
        .0
        .get_mut()
        .write_all(b"i IDLE\r\n")
        .await
        .expect("written");
    let go = idling.line().await;
    assert!(go.starts_with('+'), "{go}");
    let answer = owner.delete(&format!("/api/devices/{idle_id}")).await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT, "{}", answer.body);
    assert!(
        idling.closes_within(Duration::from_secs(5)).await,
        "revoking ends a session inside IDLE"
    );

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a client");
    let session = format!("{base}/jmap/session");
    let dav = format!("{base}/dav/");
    for (url, method, own, other) in [
        (&session, "GET", &jmap_secret, &dav_secret),
        (&dav, "PROPFIND", &dav_secret, &jmap_secret),
    ] {
        assert_eq!(
            http_status(&client, method, url, None).await,
            StatusCode::UNAUTHORIZED,
            "{url} without a device"
        );
        assert_eq!(
            http_status(&client, method, url, Some(OWNER_PASSWORD)).await,
            StatusCode::UNAUTHORIZED,
            "{url} with the web password"
        );
        assert_eq!(
            http_status(&client, method, url, Some(other)).await,
            StatusCode::UNAUTHORIZED,
            "{url} with another protocol's device"
        );
        assert!(
            http_status(&client, method, url, Some(own))
                .await
                .is_success(),
            "{url} with its own device"
        );
    }
    let cookie = owner
        .request(reqwest::Method::GET, "/jmap/session", None)
        .await;
    assert_eq!(
        cookie.status,
        StatusCode::UNAUTHORIZED,
        "a browser session never signs in to JMAP"
    );
    let mut stream = client
        .get(format!(
            "{base}/jmap/eventsource?types=*&closeafter=no&ping=0"
        ))
        .basic_auth(OWNER_EMAIL, Some(&jmap_secret))
        .send()
        .await
        .expect("the event stream opens");
    assert_eq!(stream.status(), StatusCode::OK);
    let mut seen = String::new();
    while !seen.contains(": ok") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.chunk())
            .await
            .expect("the stream starts")
            .expect("a chunk")
            .expect("not closed yet");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    for (id, url, method, secret) in [
        (&jmap_id, &session, "GET", &jmap_secret),
        (&dav_id, &dav, "PROPFIND", &dav_secret),
    ] {
        let answer = owner.delete(&format!("/api/devices/{id}")).await;
        assert_eq!(answer.status, StatusCode::NO_CONTENT, "{}", answer.body);
        assert_eq!(
            http_status(&client, method, url, Some(secret)).await,
            StatusCode::UNAUTHORIZED,
            "{url} after revoking"
        );
    }
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await;
    assert!(ended.is_ok(), "revoking ends an open JMAP event stream");

    let scanner = scanner()
        .secret("an app password", &imap_secret)
        .secret("an app password", &idle_secret)
        .secret("an app password", &jmap_secret)
        .secret("an app password", &dav_secret);
    assert_eq!(scanner.findings(&holtmail.logs()), Vec::new());
}

/// The value after `<key>{key}</key>` in a property list, the first time the key appears.
fn plist_value(plist: &str, key: &str) -> String {
    let after = &plist[plist
        .find(&format!("<key>{key}</key>"))
        .unwrap_or_else(|| panic!("{key} in the profile"))..];
    let start = after.find("</key>").expect("a key") + "</key>".len();
    let value = after[start..].trim_start();
    let open = value.find('>').expect("a value element") + 1;
    let close = value.find("</").expect("a closed value");
    value[open..close].to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_and_autoconfig_set_up_a_mail_client() {
    require_it!();
    let free = || {
        TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("an address")
            .port()
    };
    let (http, imap, smtp) = (free(), free(), free());
    let base = format!("http://localhost:{http}");
    let holtmail = Holtmail::start(Options {
        port: Some(http),
        extra_config: format!(
            "public_url = \"{base}\"\n[serve]\nmail_domains = [\"Example.com\"]\n\
             [serve.imap]\nlisten_starttls = [\"127.0.0.1:{imap}\"]\nhostname = \"localhost\"\n\
             [serve.submit]\nlisten_starttls = [\"127.0.0.1:{smtp}\"]\n[serve.dav]\n"
        ),
        ..Options::default()
    })
    .await;
    let owner = Api::setup_owner(&holtmail).await;
    let archive = owner.create_archive(&unique("profile")).await;
    let setup = owner.get("/api/devices/setup").await.ok();
    assert_eq!(setup["apple_profile"], true, "{setup}");
    assert_eq!(setup["davx5"], json!(null), "no DAVx5 link over http");

    let made = owner
        .post(
            "/api/devices",
            json!({
                "name": "Owner's iPhone",
                "protocols": ["imap", "smtp", "dav"],
                "scopes": ["read", "send:mail"],
                "mail_archive": archive,
            }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let device = id(&made.body["device"]);
    let old = made.body["secret"].as_str().expect("a secret").to_owned();
    let jmap_only = owner
        .post(
            "/api/devices",
            json!({ "name": "Script", "protocols": ["jmap"] }),
        )
        .await;
    let refused = owner
        .post(
            &format!("/api/devices/{}/profile", id(&jmap_only.body["device"])),
            json!({}),
        )
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);

    let link = owner
        .post(&format!("/api/devices/{device}/profile"), json!({}))
        .await;
    assert_eq!(link.status, StatusCode::CREATED, "{}", link.body);
    let url = link.body["url"].as_str().expect("a link").to_owned();
    assert!(
        url.starts_with(&format!("{base}/api/devices/profile/")),
        "{url}"
    );
    let client = reqwest::Client::new();
    let page = client.get(&url).send().await.expect("an answer");
    assert_eq!(page.status(), StatusCode::OK);
    assert!(
        page.text()
            .await
            .expect("the page")
            .contains("<form method=\"post\">"),
        "opening the link shows a confirmation page"
    );
    let answer = client.post(&url).send().await.expect("an answer");
    assert_eq!(answer.status(), StatusCode::OK);
    assert_eq!(
        answer.headers()[CONTENT_TYPE],
        "application/x-apple-aspen-config"
    );
    assert_eq!(answer.headers()["cache-control"], "no-store");
    let profile = answer.text().await.expect("the profile");
    assert!(profile.starts_with("<?xml"), "{profile}");
    assert_eq!(plist_value(&profile, "EmailAddress"), OWNER_EMAIL);
    assert_eq!(
        plist_value(&profile, "IncomingMailServerHostName"),
        "localhost"
    );
    assert_eq!(
        plist_value(&profile, "IncomingMailServerPortNumber"),
        imap.to_string()
    );
    assert_eq!(
        plist_value(&profile, "OutgoingMailServerPortNumber"),
        smtp.to_string()
    );
    assert_eq!(plist_value(&profile, "CalDAVHostName"), "localhost");
    assert_eq!(plist_value(&profile, "CardDAVAccountDescription"), "People");
    assert_eq!(plist_value(&profile, "CalDAVPort"), http.to_string());
    let fresh = plist_value(&profile, "IncomingPassword");
    assert!(
        fresh.starts_with("hm_") && fresh != old,
        "a fresh app password"
    );
    assert_eq!(plist_value(&profile, "CalDAVPassword"), fresh);

    assert_eq!(Imap::greeted(imap).await.login(&fresh).await, "OK");
    assert_eq!(
        Imap::greeted(imap).await.login(&old).await,
        "NO",
        "the old app password stopped working"
    );
    let dav = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("{base}/dav/"),
        )
        .header("Depth", "0")
        .basic_auth(OWNER_EMAIL, Some(&fresh))
        .send()
        .await
        .expect("an answer");
    assert!(dav.status().is_success(), "{}", dav.status());
    let again = client.post(&url).send().await.expect("an answer");
    assert_eq!(again.status(), StatusCode::NOT_FOUND, "the link works once");
    let audit = owner.get("/api/audit").await.ok();
    assert_eq!(audit[0]["kind"], "device_secret_issued", "{audit}");
    assert!(!audit.to_string().contains(&fresh));

    let autoconfig = |path: &str, host: &str| {
        client
            .get(format!("http://127.0.0.1:{http}{path}"))
            .header(reqwest::header::HOST, host)
            .send()
    };
    let well_known = "/.well-known/autoconfig/mail/config-v1.1.xml";
    for (path, host) in [
        (
            format!("{well_known}?emailaddress=someone%40example.com"),
            "example.com",
        ),
        ("/mail/config-v1.1.xml".to_owned(), "autoconfig.example.com"),
    ] {
        let answer = autoconfig(&path, host).await.expect("an answer");
        assert_eq!(answer.status(), StatusCode::OK, "{path} on {host}");
        let xml = answer.text().await.expect("the config");
        assert!(xml.contains("<domain>example.com</domain>"), "{xml}");
        assert!(xml.contains("<hostname>localhost</hostname>"), "{xml}");
        assert!(xml.contains(&format!("<port>{imap}</port>")), "{xml}");
        assert!(xml.contains(&format!("<port>{smtp}</port>")), "{xml}");
        assert!(xml.contains("<socketType>STARTTLS</socketType>"), "{xml}");
        assert!(xml.contains(&format!("{base}/dav/")), "{xml}");
        assert!(!xml.contains("hm_"));
    }
    for (path, host) in [
        (
            format!("{well_known}?emailaddress=someone%40example.org"),
            "example.com",
        ),
        ("/mail/config-v1.1.xml".to_owned(), "autoconfig.example.org"),
    ] {
        let answer = autoconfig(&path, host).await.expect("an answer");
        assert_eq!(
            answer.status(),
            StatusCode::NOT_FOUND,
            "{path} on {host}: not an own mail domain"
        );
    }

    let records = holtmail.command(&["dns"], "");
    assert!(
        records.status.success(),
        "{}",
        String::from_utf8_lossy(&records.stderr)
    );
    let zone = String::from_utf8_lossy(&records.stdout);
    assert!(zone.contains("; example.com"), "{zone}");
    assert!(
        zone.contains("_imap._tcp.example.com.") && zone.contains(&format!("{imap} localhost.")),
        "{zone}"
    );
    assert!(zone.contains("_submission._tcp.example.com."), "{zone}");
    assert!(
        !zone.contains("_jmap._tcp"),
        "no JMAP record over http: {zone}"
    );

    let scanner = scanner()
        .secret("an app password", &old)
        .secret("an app password", &fresh);
    assert_eq!(scanner.findings(&holtmail.logs()), Vec::new());
}
