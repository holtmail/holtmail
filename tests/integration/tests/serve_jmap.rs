//! JMAP serving through the real binary, with curl, `jmap-client` and plain JMAP requests.
//!
//! Reading: the well-known redirect, `401` without a device and CORS preflights over curl;
//! `jmap-client` lists the same inbox Dovecot holds and downloads bytes whose BLAKE3 hash is the
//! blob id; an EventSource stream gets a `StateChange` when mail arrives over SMTP.
//!
//! Writing: `$seen` reaches Dovecot and Stalwart through writeback, a move and a destroy reach
//! Dovecot (a destroy expunges), an `ifInState` from before a change is a `stateMismatch`, and `EmailSubmission`
//! sends a draft through GreenMail (the sent copy archived, the draft destroyed) while an undone
//! one never leaves.

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use holtmail_integration::api::{OWNER_EMAIL, id};
use holtmail_integration::data::Message;
use holtmail_integration::imap::{Seeder, quote};
use holtmail_integration::jmap::Jmap;
use holtmail_integration::services::{DOVECOT, GREENMAIL, GREENMAIL_SMTP, local};
use holtmail_integration::stalwart::{self, STALWART_PASSWORD};
use holtmail_integration::{Api, Holtmail, Options, require_it, unique};
use reqwest::StatusCode;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(60);
const USING: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
];

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
        .port()
}

struct Served {
    _holtmail: Holtmail,
    api: Api,
    archive: String,
    secret: String,
    base: String,
}

/// An instance with `[serve.jmap]` on `http://127.0.0.1:<port>`, an archive named `name` holding
/// the Dovecot account of `user`, and a JMAP device with `scopes`.
async fn start(name: &str, user: &str, scopes: &[&str], extra: &str) -> Served {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let holtmail = Holtmail::start(Options {
        port: Some(port),
        extra_config: format!(
            "public_url = \"{base}\"\n[write]\nundo_seconds = 3\n[serve.jmap]\n{extra}"
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = match api.get("/api/archives").await.ok().as_array() {
        Some(list) if list.iter().any(|a| a["name"] == name) => api.archive_named(name).await,
        _ => api.create_archive(name).await,
    };
    let account = api.add_imap(&archive, DOVECOT.account(user)).await;
    api.request(
        reqwest::Method::PUT,
        &format!("/api/accounts/{account}/capabilities"),
        Some(&json!({ "capabilities": ["mail.flags", "mail.move", "mail.delete", "mail.expunge", "mail.append"] })),
    )
    .await
    .ok();
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let made = api
        .post(
            "/api/devices",
            json!({
                "name": "JMAP client",
                "protocols": ["jmap"],
                "scopes": scopes,
                "mail_archive": archive,
            }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let secret = made.body["secret"].as_str().expect("a secret").to_owned();
    Served {
        _holtmail: holtmail,
        api,
        archive,
        secret,
        base,
    }
}

impl Served {
    async fn jmap(&self) -> Jmap {
        Jmap::connect(
            &format!("{}/jmap/session", self.base),
            OWNER_EMAIL,
            &self.secret,
        )
        .await
    }

    /// `curl -i` with `args`, returning the status line and headers, lowercased.
    fn curl(&self, args: &[&str]) -> String {
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
}

/// Method calls in one request, the answers in order.
async fn calls(jmap: &Jmap, method_calls: Value) -> Vec<Value> {
    jmap.call(USING, method_calls).await
}

/// One method call that must not fail.
async fn ok(jmap: &Jmap, method: &str, arguments: Value) -> Value {
    jmap.ok(USING, method, arguments).await
}

/// Email id by subject, over every email the account shows.
async fn emails(jmap: &Jmap, account: &str) -> Vec<(String, String, Value)> {
    let found = ok(
        jmap,
        "Email/query",
        json!({ "accountId": account, "limit": 500 }),
    )
    .await;
    let got = ok(
        jmap,
        "Email/get",
        json!({ "accountId": account, "ids": found["ids"], "properties": ["subject", "mailboxIds", "keywords"] }),
    )
    .await;
    got["list"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap_or_default().to_owned(),
                e["subject"].as_str().unwrap_or_default().to_owned(),
                e,
            )
        })
        .collect()
}

fn email_id(list: &[(String, String, Value)], subject: &str) -> String {
    list.iter()
        .find(|(_, s, _)| s == subject)
        .map(|(id, _, _)| id.clone())
        .unwrap_or_else(|| panic!("no {subject} in {list:?}"))
}

async fn mailbox_named(jmap: &Jmap, account: &str, name: &str) -> String {
    let got = ok(jmap, "Mailbox/get", json!({ "accountId": account })).await;
    got["list"]
        .as_array()
        .and_then(|l| l.iter().find(|m| m["name"] == name || m["role"] == name))
        .and_then(|m| m["id"].as_str())
        .unwrap_or_else(|| panic!("no {name} in {got}"))
        .to_owned()
}

/// Whether Dovecot shows the message with `subject` in `folder` of `user` with `want` among its
/// flags, within the wait.
async fn in_dovecot(user: &str, folder: &str, subject: &str, want: &str) -> bool {
    let started = Instant::now();
    loop {
        let mut imap = DOVECOT.seeder(user).await;
        imap.ok(&format!("SELECT {}", quote(folder))).await;
        let (_, lines) = imap
            .command("FETCH 1:* (FLAGS BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
            .await;
        imap.logout().await;
        let mut flags = None;
        for line in &lines {
            if line.contains("FLAGS (") {
                flags = Some(line.clone());
            }
            if line == &format!("Subject: {subject}")
                && flags.as_ref().is_some_and(|f| f.contains(want))
            {
                return true;
            }
        }
        if started.elapsed() > WAIT {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn subjects_in_dovecot(user: &str, folder: &str) -> Vec<String> {
    let mut imap = DOVECOT.seeder(user).await;
    imap.ok(&format!("SELECT {}", quote(folder))).await;
    let (_, lines) = imap
        .command("FETCH 1:* (BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
        .await;
    imap.logout().await;
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("Subject: ").map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn clients_discover_read_download_and_get_pushes() {
    require_it!();
    let user = DOVECOT.user("jmap-read");
    let welcome = Message::new(&user).subject(&unique("Welcome"));
    let numbers = Message::new(&user).subject(&unique("Numbers"));
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &welcome.bytes()).await;
    seeder.append("INBOX", &[], None, &numbers.bytes()).await;
    seeder.logout().await;
    let smtp = free_port();
    let name = unique("jmap-read");
    let served = start(
        &name,
        &user,
        &["read"],
        &format!(
            "[smtp]\nlisten = \"127.0.0.1:{smtp}\"\nhostname = \"localhost\"\n\
             allow = [\"127.0.0.0/8\"]\n\
             [[smtp.route]]\nrecipient = \"*@inbound.example.com\"\narchive = \"{name}\"\n"
        ),
    )
    .await;

    let redirect = served.curl(&[&format!("{}/.well-known/jmap", served.base)]);
    assert!(redirect.starts_with("http/1.1 307"), "{redirect}");
    assert!(
        redirect.contains(&format!("location: {}/jmap/session", served.base)),
        "{redirect}"
    );
    let anonymous = served.curl(&[&format!("{}/jmap/session", served.base)]);
    assert!(anonymous.starts_with("http/1.1 401"), "{anonymous}");
    assert!(anonymous.contains("www-authenticate: basic"), "{anonymous}");
    let wrong = served.curl(&[
        "--user",
        &format!("{OWNER_EMAIL}:not-the-secret"),
        &format!("{}/jmap/session", served.base),
    ]);
    assert!(wrong.starts_with("http/1.1 401"), "{wrong}");
    let preflight = served.curl(&[
        "--request",
        "OPTIONS",
        "--header",
        "Origin: https://webmail.example.org",
        "--header",
        "Access-Control-Request-Method: POST",
        "--header",
        "Access-Control-Request-Headers: authorization, content-type",
        &format!("{}/jmap/api", served.base),
    ]);
    assert!(preflight.starts_with("http/1.1 204"), "{preflight}");
    assert!(
        preflight.contains("access-control-allow-origin: *"),
        "{preflight}"
    );
    assert!(
        !preflight.contains("access-control-allow-credentials: true"),
        "{preflight}"
    );

    let jmap = served.jmap().await;
    let account = served.archive.clone();
    assert_eq!(
        jmap.session()["capabilities"]["urn:holtmail:params:jmap:instance"]["token"]
            .as_str()
            .map(str::len),
        Some(64),
        "the session carries the instance token"
    );
    assert!(jmap.session()["accounts"][&account].is_object());
    let inbox = mailbox_named(&jmap, &account, "inbox").await;

    use jmap_client::client::{Client, Credentials};
    use jmap_client::email;
    let client = Client::new()
        .credentials(Credentials::basic(OWNER_EMAIL, &served.secret))
        .follow_redirects(["127.0.0.1"])
        .connect(&served.base)
        .await
        .expect("jmap-client connects");
    let found = client
        .email_query(
            Some(email::query::Filter::in_mailbox(&inbox)),
            Some([email::query::Comparator::received_at()]),
        )
        .await
        .expect("Email/query");
    let ids = found.ids().to_vec();
    assert_eq!(ids.len(), 2, "the inbox Dovecot holds");
    let mut subjects = Vec::new();
    for id in &ids {
        let email = client
            .email_get(id, None::<Vec<email::Property>>)
            .await
            .expect("Email/get")
            .expect("the email");
        subjects.push(email.subject().unwrap_or_default().to_owned());
        let blob = email.blob_id().expect("a blob id").to_owned();
        let bytes = client.download(&blob).await.expect("the download");
        assert_eq!(
            format!("B{}", blake3::hash(&bytes).to_hex()),
            blob,
            "the blob id is the hash of the bytes"
        );
        let message = id.strip_prefix('M').expect("an archived message");
        assert_eq!(bytes, served.api.raw(message).await, "the archived bytes");
    }
    subjects.sort();
    let mut expected = vec![welcome.subject.clone(), numbers.subject.clone()];
    expected.sort();
    assert_eq!(subjects, expected);

    let mut stream = holtmail_integration::services::http_client()
        .get(format!(
            "{}/jmap/eventsource?types=*&closeafter=state&ping=0",
            served.base
        ))
        .basic_auth(OWNER_EMAIL, Some(&served.secret))
        .send()
        .await
        .expect("the event stream opens");
    assert_eq!(stream.status(), StatusCode::OK);
    let mut seen = String::new();
    while !seen.contains(": ok") {
        let chunk = tokio::time::timeout(WAIT, stream.chunk())
            .await
            .expect("the stream starts")
            .expect("a chunk")
            .expect("not closed yet");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    let late = unique("Late");
    let answer = holtmail_integration::submit::send(
        smtp,
        "carol@example.org",
        "ada@inbound.example.com",
        format!(
            "From: Carol <carol@example.org>\r\nTo: ada@inbound.example.com\r\nSubject: {late}\r\n\
             Message-ID: <{late}@example.org>\r\nDate: Sat, 7 Nov 2026 10:00:00 +0000\r\n\r\nHello.\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(answer.starts_with("250"), "{answer}");
    let pushed = tokio::time::timeout(WAIT, async {
        while let Some(chunk) = stream.chunk().await.expect("a chunk") {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        seen
    })
    .await
    .expect("a StateChange within the wait");
    assert!(pushed.contains("event: state"), "{pushed}");
    let data: Value = pushed
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .and_then(|d| serde_json::from_str(d).ok())
        .unwrap_or_else(|| panic!("a StateChange in {pushed}"));
    assert_eq!(data["@type"], "StateChange");
    assert!(data["changed"][&account]["Email"].is_string(), "{data}");
    let all = emails(&jmap, &account).await;
    email_id(&all, &late);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_writes_reach_upstreams_and_submissions_leave_through_the_outbox() {
    require_it!();
    let user = DOVECOT.user("jmap-write");
    let reading = Message::new(&user).subject(&unique("Reading"));
    let moving = Message::new(&user).subject(&unique("Moving"));
    let doomed = Message::new(&user).subject(&unique("Doomed"));
    let mut seeder = DOVECOT.seeder(&user).await;
    for message in [&reading, &moving, &doomed] {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    for folder in ["Projects", "Sent", "Trash"] {
        seeder.create(folder).await;
    }
    seeder.logout().await;
    let served = start(
        &unique("jmap-write"),
        &user,
        &["read", "write:mail", "send:mail"],
        "",
    )
    .await;

    let remote_user = format!("{}@example.com", unique("remote"));
    let remote_box = stalwart::Seeder::new(&remote_user).await;
    let remote_subject = unique("Remote");
    let remote_inbox = remote_box.role("inbox").await;
    remote_box
        .import(
            &Message::new(&remote_user).subject(&remote_subject).bytes(),
            &[&remote_inbox],
            &[],
        )
        .await;
    let added = served
        .api
        .add_jmap_account(
            &served.archive,
            json!({ "url": stalwart::base_url(), "username": remote_user, "password": STALWART_PASSWORD }),
        )
        .await
        .ok();
    let remote = id(&added);
    served
        .api
        .request(
            reqwest::Method::PUT,
            &format!("/api/accounts/{remote}/capabilities"),
            Some(&json!({ "capabilities": ["mail.flags"] })),
        )
        .await
        .ok();
    let settled = served.api.settle(&served.archive, &remote).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");

    let jmap = served.jmap().await;
    let account = served.archive.clone();
    let all = emails(&jmap, &account).await;
    let before = ok(
        &jmap,
        "Email/get",
        json!({ "accountId": account, "ids": [] }),
    )
    .await["state"]
        .clone();
    let seen = ok(
        &jmap,
        "Email/set",
        json!({ "accountId": account, "update": {
            email_id(&all, &reading.subject): { "keywords/$seen": true },
            email_id(&all, &remote_subject): { "keywords/$seen": true },
        } }),
    )
    .await;
    assert_ne!(seen["newState"], before);
    assert!(
        in_dovecot(&user, "INBOX", &reading.subject, "\\Seen").await,
        "$seen reached Dovecot"
    );
    let started = Instant::now();
    loop {
        let read = remote_box
            .emails(&remote_inbox)
            .await
            .iter()
            .any(|e| e["subject"] == remote_subject.as_str() && e["keywords"]["$seen"] == true);
        if read {
            break;
        }
        assert!(started.elapsed() < WAIT, "$seen never reached Stalwart");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let stale = calls(
        &jmap,
        json!([["Email/set", {
            "accountId": account,
            "ifInState": before,
            "update": { email_id(&all, &moving.subject): { "keywords/$flagged": true } },
        }, "0"]]),
    )
    .await;
    assert_eq!(stale[0][0], "error", "{stale:?}");
    assert_eq!(stale[0][1]["type"], "stateMismatch", "{stale:?}");

    let projects = mailbox_named(&jmap, &account, "Projects").await;
    ok(
        &jmap,
        "Email/set",
        json!({ "accountId": account, "update": {
            email_id(&all, &moving.subject): { "mailboxIds": { projects: true } },
        } }),
    )
    .await;
    assert!(
        in_dovecot(&user, "Projects", &moving.subject, "").await,
        "the move reached Dovecot"
    );
    ok(
        &jmap,
        "Email/set",
        json!({ "accountId": account, "destroy": [email_id(&all, &doomed.subject)] }),
    )
    .await;
    let started = Instant::now();
    while subjects_in_dovecot(&user, "INBOX")
        .await
        .contains(&doomed.subject)
    {
        assert!(
            started.elapsed() < WAIT,
            "the destroy never reached Dovecot"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        !subjects_in_dovecot(&user, "Trash")
            .await
            .contains(&doomed.subject),
        "a JMAP destroy expunges instead of moving to the trash"
    );

    let transport = served
        .api
        .post(
            &format!("/api/archives/{}/transports", served.archive),
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
    let sender = format!("{}@example.com", unique("ada"));
    served
        .api
        .post(
            &format!("/api/archives/{}/identities", served.archive),
            json!({ "address": sender, "name": "Ada Example", "transport": id(&transport), "active": true }),
        )
        .await
        .ok();
    let identities = ok(&jmap, "Identity/get", json!({ "accountId": account })).await;
    let identity = identities["list"]
        .as_array()
        .and_then(|l| l.iter().find(|i| i["email"] == sender.as_str()))
        .unwrap_or_else(|| panic!("the identity in {identities}"))["id"]
        .clone();

    let draft_and_send = |to: &str, subject: &str| {
        json!([
            ["Email/set", { "accountId": account, "create": { "d": {
                "mailboxIds": { "drafts": true },
                "keywords": { "$draft": true, "$seen": true },
                "from": [{ "name": "Ada Example", "email": sender }],
                "to": [{ "email": to }],
                "subject": subject,
                "bodyValues": { "t": { "value": "Noon on Thursday?" } },
                "textBody": [{ "partId": "t", "type": "text/plain" }],
            } } }, "0"],
            ["EmailSubmission/set", {
                "accountId": account,
                "create": { "s": { "identityId": identity, "emailId": "#d" } },
                "onSuccessDestroyEmail": ["#s"],
            }, "1"],
        ])
    };

    let carol = format!("{}@example.org", unique("carol"));
    let undone = calls(&jmap, draft_and_send(&carol, &unique("Never mind"))).await;
    let created = &undone[1][1]["created"]["s"];
    assert_eq!(created["undoStatus"], "pending", "{undone:?}");
    let undone_id = created["id"].as_str().expect("a submission id").to_owned();
    let cancel = ok(
        &jmap,
        "EmailSubmission/set",
        json!({ "accountId": account, "update": { &undone_id: { "undoStatus": "canceled" } } }),
    )
    .await;
    assert!(
        cancel["updated"]
            .as_object()
            .is_some_and(|m| m.contains_key(&undone_id)),
        "{cancel}"
    );

    let bob = format!("{}@example.org", unique("bob"));
    let subject = unique("Lunch");
    let sent = calls(&jmap, draft_and_send(&bob, &subject)).await;
    let draft = sent[0][1]["created"]["d"]["id"].clone();
    assert_eq!(sent[2][0], "Email/set", "{sent:?}");
    assert_eq!(sent[2][1]["destroyed"], json!([draft]), "{sent:?}");
    let submission = sent[1][1]["created"]["s"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a submission in {sent:?}"))
        .to_owned();
    let started = Instant::now();
    let final_state = loop {
        let got = ok(
            &jmap,
            "EmailSubmission/get",
            json!({ "accountId": account, "ids": [submission, undone_id] }),
        )
        .await;
        let list = got["list"].as_array().cloned().unwrap_or_default();
        let of = |id: &str| {
            list.iter()
                .find(|s| s["id"] == id)
                .cloned()
                .unwrap_or_default()
        };
        if of(&submission)["undoStatus"] == "final" {
            assert_eq!(of(&undone_id)["undoStatus"], "canceled", "{got}");
            break of(&submission);
        }
        assert!(started.elapsed() < WAIT, "never final: {got}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(
        final_state["envelope"]["rcptTo"][0]["email"],
        bob.as_str(),
        "{final_state}"
    );

    let started = Instant::now();
    let bodies = loop {
        let mut imap = Seeder::login(&local(GREENMAIL.port), &bob, GREENMAIL.password).await;
        let bodies = imap.bodies("INBOX").await;
        imap.logout().await;
        if !bodies.is_empty() || started.elapsed() > WAIT {
            break bodies;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(bodies.len(), 1, "GreenMail got the message");
    assert!(String::from_utf8_lossy(&bodies[0]).contains(&subject));
    let mut imap = Seeder::login(&local(GREENMAIL.port), &carol, GREENMAIL.password).await;
    assert!(
        imap.bodies("INBOX").await.is_empty(),
        "the undone submission never left"
    );
    imap.logout().await;

    let outbox = submission.strip_prefix('S').expect("an outbox id");
    let started = Instant::now();
    loop {
        let entry = served.api.get(&format!("/api/outbox/{outbox}")).await.ok();
        if entry["state"] == "sent" && entry["message"].is_string() {
            break;
        }
        assert!(started.elapsed() < WAIT, "not archived: {entry}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let started = Instant::now();
    loop {
        let found = ok(
            &jmap,
            "Email/query",
            json!({ "accountId": account, "filter": { "subject": subject } }),
        )
        .await;
        if found["ids"].as_array().is_some_and(|ids| {
            ids.iter()
                .any(|i| i.as_str().is_some_and(|i| i.starts_with('M')))
        }) {
            break;
        }
        assert!(
            started.elapsed() < WAIT,
            "the sent copy never showed: {found}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
