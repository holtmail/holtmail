//! IMAP serving through the real binary, with third-party clients and a second Holtmail.
//!
//! Reading: `doveadm backup -R imapc:` in the Dovecot container mirrors the served account over
//! TLS with QRESYNC and leaves the mail unread, curl lists and fetches over `imaps://`, a second
//! Holtmail archives the first through `source-imap` while the first refuses itself as an
//! account, and `IDLE` wakes when a delivery to the upstream is synced.
//!
//! Writing: a held move on the Dovecot upstream keeps its served UID after writeback and sync
//! confirm it, `\Seen` reaches Dovecot, a `QRESYNC` select after expunges reports the UIDs that
//! left, and a client's Sent copy of mail sent through Holtmail answers with the outbox's own
//! message instead of archiving a second copy.

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use holtmail_integration::api::{OWNER_EMAIL, id};
use holtmail_integration::data::Message;
use holtmail_integration::imap::{Seeder, quote};
use holtmail_integration::services::{DOVECOT, GREENMAIL_SMTP, compose, local};
use holtmail_integration::{Api, Holtmail, Options, ca_file, require_it, state_dir, unique};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const WAIT: Duration = Duration::from_secs(60);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
        .port()
}

struct Served {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    account: String,
    secret: String,
    tls: u16,
    plain: u16,
}

async fn start(user: &str, scopes: &[&str], capabilities: &[&str]) -> Served {
    let (tls, plain) = (free_port(), free_port());
    let state = state_dir();
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            "[serve]\ntls_cert = {cert:?}\ntls_key = {key:?}\n\
             [serve.imap]\nlisten_tls = [\"127.0.0.1:{tls}\"]\n\
             listen_starttls = [\"127.0.0.1:{plain}\"]\nhostname = \"localhost\"\n",
            cert = state.join("server.crt"),
            key = state.join("server.key"),
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique("served")).await;
    let account = api.add_imap(&archive, DOVECOT.account(user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    if !capabilities.is_empty() {
        api.request(
            reqwest::Method::PUT,
            &format!("/api/accounts/{account}/capabilities"),
            Some(&json!({ "capabilities": capabilities })),
        )
        .await
        .ok();
    }
    let made = api
        .post(
            "/api/devices",
            json!({
                "name": "Mail client",
                "protocols": ["imap"],
                "scopes": scopes,
                "mail_archive": archive,
            }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let secret = made.body["secret"].as_str().expect("a secret").to_owned();
    Served {
        holtmail,
        api,
        archive,
        account,
        secret,
        tls,
        plain,
    }
}

impl Served {
    /// `doveadm backup -R imapc:` from this server into Dovecot's `mirror` user, over TLS
    /// through the host's gateway; QRESYNC is used because the server offers it.
    fn mirror(&self, mirror: &str) {
        let password = format!("imapc_password={}", self.secret);
        let port = format!("imapc_port={}", self.tls);
        let user = format!("imapc_user={OWNER_EMAIL}");
        compose(&[
            "exec",
            "-T",
            "dovecot",
            "doveadm",
            "-o",
            "imapc_host=host.docker.internal",
            "-o",
            &port,
            "-o",
            "imapc_ssl=imaps",
            "-o",
            "ssl_client_require_valid_cert=no",
            "-o",
            &user,
            "-o",
            &password,
            "backup",
            "-R",
            "-u",
            mirror,
            "imapc:",
        ]);
    }

    async fn client(&self) -> Seeder {
        Seeder::login(&local(self.plain), OWNER_EMAIL, &self.secret).await
    }

    fn curl(&self, path: &str, request: Option<&str>) -> String {
        let mut command = Command::new("curl");
        command
            .args(["--silent", "--show-error", "--max-time", "30", "--cacert"])
            .arg(ca_file())
            .arg("--user")
            .arg(format!("{OWNER_EMAIL}:{}", self.secret));
        if let Some(request) = request {
            command.args(["--request", request]);
        }
        let output = command
            .arg(format!("imaps://localhost:{}/{path}", self.tls))
            .output()
            .expect("curl runs");
        assert!(
            output.status.success(),
            "curl {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

/// UID by subject for every message in the selected mailbox.
async fn uids(client: &mut Seeder) -> Vec<(u32, String)> {
    let lines = client
        .ok("UID FETCH 1:* (FLAGS BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
        .await;
    let mut out = Vec::new();
    let mut current = None;
    for line in lines {
        if let Some(at) = line.find("UID ") {
            current = line[at + 4..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok());
        }
        if let (Some(subject), Some(uid)) = (line.strip_prefix("Subject: "), current) {
            out.push((uid, subject.to_owned()));
        }
    }
    out
}

fn uid_of(list: &[(u32, String)], subject: &str) -> u32 {
    list.iter()
        .find(|(_, s)| s == subject)
        .map(|(u, _)| *u)
        .unwrap_or_else(|| panic!("no {subject} in {list:?}"))
}

fn number_after(lines: &[String], word: &str) -> u64 {
    lines
        .iter()
        .find_map(|l| {
            let at = l.find(word)?;
            l[at + word.len()..]
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or_else(|| panic!("no {word} in {lines:?}"))
}

/// The flags Dovecot shows for the message with `subject` in `folder` of `user`, once it is
/// there; `None` if it never shows within the wait.
async fn dovecot_flags(user: &str, folder: &str, subject: &str, want: &str) -> Option<String> {
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
                && let Some(flags) = &flags
                && flags.contains(want)
            {
                return Some(flags.clone());
            }
        }
        if started.elapsed() > WAIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn clients_mirror_list_and_sync_the_served_account_without_reading_it() {
    require_it!();
    let user = DOVECOT.user("served-read");
    let welcome = Message::new(&user).subject(&unique("Welcome"));
    let numbers = Message::new(&user).subject(&unique("Numbers"));
    let report = Message::new(&user).subject(&unique("Report"));
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &welcome.bytes()).await;
    seeder.append("INBOX", &[], None, &numbers.bytes()).await;
    seeder.create("Projects").await;
    seeder.append("Projects", &[], None, &report.bytes()).await;
    seeder.logout().await;
    let served = start(&user, &["read"], &[]).await;

    let listed = served.curl("", None);
    assert!(listed.contains("INBOX"), "{listed}");
    assert!(listed.contains("Projects"), "{listed}");
    let search = served.curl("INBOX?ALL", None);
    let first: u32 = search
        .split_whitespace()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a UID in {search:?}"));
    let body = served.curl(&format!("INBOX/;UID={first}"), None);
    assert!(
        body.contains(&welcome.subject) || body.contains(&numbers.subject),
        "{body}"
    );

    let mirror = DOVECOT.user("mirror");
    served.mirror(&mirror);
    let mut mirrored = DOVECOT.seeder(&mirror).await;
    assert_eq!(mirrored.status("INBOX", "MESSAGES").await, 2);
    assert_eq!(
        mirrored.status("INBOX", "UNSEEN").await,
        2,
        "the mirror is unread"
    );
    assert_eq!(mirrored.status("Projects", "MESSAGES").await, 1);
    mirrored.logout().await;
    let mut client = served.client().await;
    client.ok("EXAMINE INBOX").await;
    let flags = client.ok("FETCH 1:* (FLAGS)").await;
    assert!(
        flags.iter().all(|l| !l.contains("\\Seen")),
        "the backup marked nothing read: {flags:?}"
    );
    client.logout().await;

    let own = served
        .api
        .add_imap_account(
            &served.archive,
            json!({
                "host": "localhost",
                "port": served.tls,
                "security": "tls",
                "username": OWNER_EMAIL,
                "password": served.secret,
            }),
        )
        .await;
    assert!(
        !own.status.is_success(),
        "the instance refuses itself as an account: {}",
        own.body
    );

    let second = Holtmail::start(Options::default()).await;
    let other = Api::setup_owner(&second).await;
    let copy = other.create_archive(&unique("copy")).await;
    let upstream = other
        .add_imap(
            &copy,
            json!({
                "host": "localhost",
                "port": served.tls,
                "security": "tls",
                "username": OWNER_EMAIL,
                "password": served.secret,
            }),
        )
        .await;
    let settled = other.settle(&copy, &upstream).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let subjects: Vec<String> = other
        .messages(&copy)
        .await
        .iter()
        .filter_map(|m| m["subject"].as_str().map(str::to_owned))
        .collect();
    for subject in [&welcome.subject, &numbers.subject, &report.subject] {
        assert!(subjects.contains(subject), "{subject} in {subjects:?}");
    }

    let stream = TcpStream::connect(local(served.plain)).await.unwrap();
    let mut idle = BufReader::new(stream);
    let mut line = String::new();
    idle.read_line(&mut line).await.unwrap();
    for command in [
        format!(
            "a LOGIN {} {}\r\n",
            quote(OWNER_EMAIL),
            quote(&served.secret)
        ),
        "b SELECT INBOX\r\n".to_owned(),
    ] {
        idle.get_mut().write_all(command.as_bytes()).await.unwrap();
        loop {
            line.clear();
            idle.read_line(&mut line).await.unwrap();
            if line.starts_with(&command[..2]) {
                assert!(line.contains(" OK"), "{command}: {line}");
                break;
            }
        }
    }
    idle.get_mut().write_all(b"c IDLE\r\n").await.unwrap();
    line.clear();
    idle.read_line(&mut line).await.unwrap();
    assert!(line.starts_with('+'), "IDLE: {line}");
    let late = Message::new(&user).subject(&unique("Late"));
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &late.bytes()).await;
    seeder.logout().await;
    served.api.sync(&served.archive, &served.account).await;
    let woke = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            line.clear();
            idle.read_line(&mut line).await.unwrap();
            if line.trim_end() == "* 3 EXISTS" {
                return;
            }
        }
    })
    .await;
    assert!(woke.is_ok(), "IDLE never told the new message");
    idle.get_mut().write_all(b"DONE\r\n").await.unwrap();

    served.mirror(&mirror);
    let mut mirrored = DOVECOT.seeder(&mirror).await;
    assert_eq!(
        mirrored.status("INBOX", "MESSAGES").await,
        3,
        "a second backup brings the new message"
    );
    assert_eq!(mirrored.status("INBOX", "UNSEEN").await, 3);
    mirrored.logout().await;
    served
        .holtmail
        .logs()
        .contains("IMAP")
        .then_some(())
        .expect("the listener is named in the logs");
}

#[tokio::test(flavor = "multi_thread")]
async fn client_writes_reach_dovecot_and_keep_served_uids() {
    require_it!();
    let user = DOVECOT.user("served-write");
    let moving = Message::new(&user).subject(&unique("Moving"));
    let reading = Message::new(&user).subject(&unique("Reading"));
    let doomed = Message::new(&user).subject(&unique("Doomed"));
    let mut seeder = DOVECOT.seeder(&user).await;
    for message in [&moving, &reading, &doomed] {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    for folder in ["Projects", "Sent", "Trash"] {
        seeder.create(folder).await;
    }
    seeder.logout().await;
    let served = start(
        &user,
        &["read", "write:mail"],
        &["mail.flags", "mail.move", "mail.delete", "mail.append"],
    )
    .await;

    let mut client = served.client().await;
    client.ok("ENABLE QRESYNC").await;
    let selected = client.ok("SELECT INBOX").await;
    let validity = number_after(&selected, "UIDVALIDITY");
    let modseq = number_after(&selected, "HIGHESTMODSEQ");
    let inbox = uids(&mut client).await;
    let (moving_uid, reading_uid, doomed_uid) = (
        uid_of(&inbox, &moving.subject),
        uid_of(&inbox, &reading.subject),
        uid_of(&inbox, &doomed.subject),
    );

    client
        .ok(&format!("UID STORE {reading_uid} +FLAGS (\\Seen)"))
        .await;
    assert!(
        dovecot_flags(&user, "INBOX", &reading.subject, "\\Seen")
            .await
            .is_some(),
        "\\Seen reached Dovecot"
    );

    let (answer, lines) = client
        .command(&format!("UID MOVE {moving_uid} Projects"))
        .await;
    assert!(answer.contains(" OK"), "MOVE: {answer}");
    let copyuid = lines
        .iter()
        .chain(std::iter::once(&answer))
        .find(|l| l.contains("COPYUID"))
        .unwrap_or_else(|| panic!("COPYUID in {lines:?} {answer}"))
        .clone();
    let held: u32 = copyuid
        .split(']')
        .next()
        .and_then(|l| l.split_whitespace().last())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a target UID in {copyuid}"));
    assert!(
        dovecot_flags(&user, "Projects", &moving.subject, "")
            .await
            .is_some(),
        "the move reached Dovecot"
    );
    served.api.sync(&served.archive, &served.account).await;
    let mut other = served.client().await;
    other.ok("EXAMINE Projects").await;
    assert_eq!(
        uid_of(&uids(&mut other).await, &moving.subject),
        held,
        "the served UID survives the confirmed move"
    );
    other.logout().await;

    client
        .ok(&format!("UID STORE {doomed_uid} +FLAGS (\\Deleted)"))
        .await;
    client.ok(&format!("UID EXPUNGE {doomed_uid}")).await;
    client.logout().await;
    let mut resync = served.client().await;
    resync.ok("ENABLE QRESYNC").await;
    let lines = resync
        .ok(&format!("SELECT INBOX (QRESYNC ({validity} {modseq}))"))
        .await;
    let vanished = lines
        .iter()
        .find(|l| l.starts_with("* VANISHED (EARLIER)"))
        .unwrap_or_else(|| panic!("VANISHED in {lines:?}"));
    for uid in [moving_uid, doomed_uid] {
        assert!(
            vanished
                .rsplit(' ')
                .next()
                .unwrap()
                .split(',')
                .any(|range| match range.split_once(':') {
                    Some((a, b)) => (a.parse::<u32>().unwrap()..=b.parse().unwrap()).contains(&uid),
                    None => range.parse::<u32>().unwrap() == uid,
                }),
            "{uid} in {vanished}"
        );
    }

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
    let subject = unique("Lunch");
    let draft = served
        .api
        .post(
            &format!("/api/archives/{}/drafts", served.archive),
            json!({ "payload": {
                "to": [{ "address": format!("{}@example.org", unique("bob")) }],
                "subject": subject,
                "text": "Noon on Thursday?",
            } }),
        )
        .await
        .ok();
    let queued = served
        .api
        .post(&format!("/api/drafts/{}/send", id(&draft)), json!({}))
        .await
        .ok();
    let outbox = id(&queued);
    let started = Instant::now();
    let entry = loop {
        let entry = served.api.get(&format!("/api/outbox/{outbox}")).await.ok();
        if entry["state"] == "sent" {
            break entry;
        }
        assert!(started.elapsed() < WAIT, "not sent: {entry}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let message = entry["message"]
        .as_str()
        .unwrap_or_else(|| panic!("an archived copy: {entry}"))
        .to_owned();
    let sent = served.api.raw(&message).await;
    let before = served.api.messages(&served.archive).await.len();
    let mut copy = b"X-Client-Copy: yes\r\n".to_vec();
    copy.extend_from_slice(&sent);
    let mut client = served.client().await;
    let answer = client.append("Sent", &["\\Seen"], None, &copy).await;
    let appended: u32 = answer
        .split("APPENDUID ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|n| n.trim_end_matches(']').parse().ok())
        .unwrap_or_else(|| panic!("APPENDUID in {answer}"));
    client.ok("EXAMINE Sent").await;
    let header = client
        .ok(&format!(
            "UID FETCH {appended} (BODY.PEEK[HEADER.FIELDS (X-Client-Copy SUBJECT)])"
        ))
        .await;
    assert!(
        header.iter().any(|l| l == &format!("Subject: {subject}")),
        "the APPENDUID is the sent message: {header:?}"
    );
    assert!(
        !header.iter().any(|l| l.starts_with("X-Client-Copy")),
        "answered with the outbox's own bytes: {header:?}"
    );
    client.logout().await;
    assert_eq!(
        served.api.messages(&served.archive).await.len(),
        before,
        "the client's copy is not archived a second time"
    );
}
