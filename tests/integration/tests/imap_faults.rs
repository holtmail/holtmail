//! IMAP failures: refused and locked sign-ins, a server that dies or stalls in the middle of a
//! fetch, malformed and very large messages, and folders that cannot be selected.

use std::time::{Duration, Instant};

use holtmail_integration::api::id;
use holtmail_integration::data::Message;
use holtmail_integration::imap::quote;
use holtmail_integration::relay::{Mode, Relay};
use holtmail_integration::services::{DOVECOT, DOVECOT_IMAPS, local, lock_dovecot_user};
use holtmail_integration::{Api, Holtmail, LogScanner, require_it};
use holtmail_source::{Credentials, Source, SourceError};
use holtmail_source_imap::{ImapSettings, ImapSource, Security};
use serde_json::{Value, json};

async fn start(user: &str) -> (Holtmail, Api, String, LogScanner) {
    let holtmail = Holtmail::start_default().await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Faults").await;
    let scanner = LogScanner::new()
        .secret("the IMAP password", DOVECOT.password)
        .secret("the IMAP user", user);
    (holtmail, api, archive, scanner)
}

fn seeded(scanner: LogScanner, messages: &[Message]) -> LogScanner {
    scanner
        .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
        .secrets("a sender", messages.iter().map(|m| m.from.as_str()))
}

fn padded(user: &str, kib: usize) -> Message {
    let line = "The quick brown fox jumps over the lazy dog, again and again and again.\n";
    Message::new(user).body(&line.repeat(kib * 1024 / line.len()))
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_wrong_password_and_a_locked_account() {
    require_it!();
    let user = DOVECOT.user("locked");
    let (holtmail, api, archive, scanner) = start(&user).await;
    let scanner = scanner.secret("the wrong password", "it-wrong-password");

    let mut wrong = DOVECOT.account(&user);
    wrong["password"] = json!("it-wrong-password");
    let refused = api.add_imap_account(&archive, wrong).await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert_eq!(
        refused.body["error"],
        "The server refused this user name and password. Some providers need an app password."
    );
    assert!(api.accounts(&archive).await.is_empty());

    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");

    lock_dovecot_user(&user).await;
    let synced = api.sync(&archive, &account).await;
    let error = synced["status"]["error"].as_str().unwrap_or_default();
    assert!(error.contains("AUTHENTICATIONFAILED"), "{synced}");
    assert_eq!(api.accounts(&archive).await.len(), 1);
    let log = api.sync_log(&account).await;
    assert!(
        log.iter().any(|l| l.contains("AUTHENTICATIONFAILED")),
        "{log:?}"
    );

    let again = api.add_imap_account(&archive, DOVECOT.account(&user)).await;
    assert_eq!(again.status, 400, "{}", again.body);
    scanner.check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_after_the_server_dies_mid_fetch() {
    require_it!();
    let user = DOVECOT.user("cut");
    let messages: Vec<Message> = (0..120).map(|_| padded(&user, 10)).collect();
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append_many(
            "INBOX",
            &messages.iter().map(Message::bytes).collect::<Vec<_>>(),
        )
        .await;
    seeder.logout().await;
    let relay = Relay::start(&local(DOVECOT.port)).await;
    relay.set(Mode::CutAfter(800 * 1024));

    let (holtmail, api, archive, scanner) = start(&user).await;
    let scanner = seeded(scanner, &messages);
    let mut account = DOVECOT.account(&user);
    account["port"] = json!(relay.port());
    let account = api.add_imap(&archive, account).await;
    let settled = api.settle(&archive, &account).await;
    let error = settled["status"]["error"].as_str().unwrap_or_default();
    assert!(!error.is_empty(), "the cut went unnoticed: {settled}");
    assert_eq!(api.messages(&archive).await.len(), 50);
    let log = api.sync_log(&account).await;
    assert!(
        log.iter()
            .any(|l| l.starts_with("INBOX: 50 new, 0 known") && l.contains(error)),
        "{log:?}"
    );

    relay.set(Mode::Pass);
    let synced = api.sync(&archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    let log = api.sync_log(&account).await;
    assert!(
        log.iter().any(|l| l.starts_with("INBOX: 70 new, 0 known")),
        "{log:?}"
    );
    assert_eq!(api.messages(&archive).await.len(), 120);
    scanner.check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_on_a_server_that_stalls_mid_fetch() {
    require_it!();
    let user = DOVECOT.user("stall");
    let messages: Vec<Message> = (0..20).map(|_| padded(&user, 10)).collect();
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append_many(
            "INBOX",
            &messages.iter().map(Message::bytes).collect::<Vec<_>>(),
        )
        .await;
    seeder.logout().await;
    let relay = Relay::start(&local(DOVECOT.port)).await;
    relay.set(Mode::StallAfter(60 * 1024));

    let source = ImapSource::new(
        ImapSettings {
            host: "127.0.0.1".into(),
            port: Some(relay.port()),
            security: Security::Plain,
            username: user.clone(),
        },
        Credentials::Password(DOVECOT.password.into()),
    )
    .with_timeout(Duration::from_secs(5));
    let mut session = source.connect().await.expect("signs in");
    session.select("INBOX").await.expect("selects");
    let uids = session.uids().await.expect("lists");
    assert_eq!(uids.len(), 20);
    let started = Instant::now();
    let Err(error) = session.fetch(&uids).await else {
        panic!("the stalled fetch succeeded");
    };
    let waited = started.elapsed();
    assert!(
        matches!(&error, SourceError::Connection(reason) if reason.contains("in time")),
        "{error}"
    );
    assert!(
        waited >= Duration::from_secs(5) && waited < Duration::from_secs(30),
        "{waited:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_malformed_mail_raw_and_skips_what_is_not_mail() {
    require_it!();
    let user = DOVECOT.user("malformed");
    let broken = format!(
        "From: Broken Sender <broken-{}@example.org>\r\n\
         To: {user}\r\n\
         Subject: Broken =?utf-8?B?not base64 at all?=\r\n\
         X-Raw: caf\u{e9} na\u{ef}ve\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"edge\"\r\n\
         \r\n\
         --edge\r\n\
         Content-Type: text/plain; charset=x-unknown-charset\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         !!!! this is not base64 ****\r\n\
         --edge\r\n\
         Content-Type: application/pdf; name=\"half.pdf\"\r\n\
         Content-Transfer-Encoding: quoted-printable\r\n\
         \r\n\
         =ZZ=4\r\n",
        holtmail_integration::unique("b")
    )
    .into_bytes();
    let mut latin1 = broken.clone();
    latin1.extend_from_slice(b"\xff\xfe raw bytes in the body\r\n");
    let not_mail = b"this line has no header field at all\r\n".to_vec();
    let after = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &latin1).await;
    seeder.append("INBOX", &[], None, &not_mail).await;
    seeder.append("INBOX", &[], None, &after.bytes()).await;
    seeder.logout().await;

    let (holtmail, api, archive, scanner) = start(&user).await;
    let scanner = seeded(scanner, std::slice::from_ref(&after));
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let log = api.sync_log(&account).await;
    assert!(
        log.iter()
            .any(|l| l.starts_with("INBOX: 2 new, 0 known, 0 removed, 0 changed, 1 failed")),
        "{log:?}"
    );
    let listed = api.messages(&archive).await;
    assert_eq!(listed.len(), 2);
    let broken_id = listed
        .iter()
        .find(|m| m["subject"].as_str() != Some(after.subject.as_str()))
        .map(id)
        .expect("the broken message is listed");
    assert_eq!(api.raw(&broken_id).await, latin1);
    scanner.check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_a_twenty_megabyte_message() {
    require_it!();
    let user = DOVECOT.user("large");
    let message = padded(&user, 20 * 1024);
    let bytes = message.bytes();
    assert!(bytes.len() >= 20 * 1024 * 1024 - 1024);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &bytes).await;
    seeder.logout().await;

    let (holtmail, api, archive, scanner) = start(&user).await;
    let scanner = seeded(scanner, std::slice::from_ref(&message));
    let started = Instant::now();
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    eprintln!("20 MB message archived in {:?}", started.elapsed());
    let listed = api.messages(&archive).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["size"], bytes.len());
    assert!(api.raw(&id(&listed[0])).await == bytes);
    scanner.check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn skips_folders_that_cannot_be_selected() {
    require_it!();
    let user = DOVECOT.user("noselect");
    let inbox = Message::new(&user);
    let child = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.create("Parent/Child").await;
    seeder.create("Unreadable").await;
    seeder.append("INBOX", &[], None, &inbox.bytes()).await;
    seeder
        .append("Parent/Child", &[], None, &child.bytes())
        .await;
    seeder
        .ok(&format!("SETACL {} owner la", quote("Unreadable")))
        .await;
    let (answer, _) = seeder.command("EXAMINE Unreadable").await;
    assert!(answer.contains("NOPERM"), "{answer}");
    seeder.logout().await;

    let (holtmail, api, archive, scanner) = start(&user).await;
    let scanner = seeded(scanner, &[inbox.clone(), child.clone()]);
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    let error = settled["status"]["error"].as_str().unwrap_or_default();
    assert!(error.contains("NOPERM"), "{settled}");
    assert_eq!(api.messages(&archive).await.len(), 2);
    let paths: Vec<String> = api
        .folders(&archive)
        .await
        .iter()
        .map(|f| f["path"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(paths.contains(&"Parent/Child".to_owned()), "{paths:?}");
    assert!(!paths.contains(&"Parent".to_owned()), "{paths:?}");
    let log = api.sync_log(&account).await;
    assert!(
        log.iter()
            .any(|l| l.starts_with("Unreadable: 0 new") && l.contains("NOPERM")),
        "{log:?}"
    );

    let excluded = api
        .update_account(
            &account,
            json!({
                "connection": {
                    "host": "127.0.0.1",
                    "port": DOVECOT.port,
                    "security": "plain",
                    "username": user,
                    "exclude": ["Unreadable"],
                }
            }),
        )
        .await
        .ok();
    assert_eq!(excluded["connection"]["exclude"], json!(["Unreadable"]));
    let synced = api.sync(&archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    scanner.check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn names_a_port_mixed_up_with_its_security() {
    require_it!();
    let source = |port, security| {
        ImapSource::new(
            ImapSettings {
                host: "127.0.0.1".into(),
                port: Some(port),
                security,
                username: "ada@example.com".into(),
            },
            Credentials::Password(DOVECOT.password.into()),
        )
        .with_timeout(Duration::from_secs(60))
    };
    let started = Instant::now();
    let Err(error) = source(DOVECOT_IMAPS, Security::Plain).connect().await else {
        panic!("signed in without TLS on the TLS port");
    };
    assert!(error.to_string().contains("security = tls"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );

    let started = Instant::now();
    let Err(error) = source(DOVECOT.port, Security::Tls).connect().await else {
        panic!("TLS on the plain port");
    };
    assert!(error.to_string().contains("plain or STARTTLS"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
