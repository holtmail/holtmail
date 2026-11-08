//! SMTP submission through the real binary: `submit-smtp` and curl as mail clients, GreenMail as
//! the server the outbox hands mail to. A Bcc-only recipient gets the message without a `Bcc`
//! header, the sent copy is archived, a wrong `MAIL FROM` is `553`, a wrong `From` header `554`,
//! oversized mail `552`, a client's retry is one outbox row, and a transport pointing at this
//! very instance fails with a report instead of going round in a circle.

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use holtmail_core::TlsRoots;
use holtmail_integration::api::{OWNER_EMAIL, id};
use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{GREENMAIL, GREENMAIL_SMTP, local};
use holtmail_integration::{
    Api, Holtmail, LogScanner, Options, ca_file, require_it, state_dir, unique,
};
use holtmail_submit::{Envelope, SubmitError, Submitter};
use holtmail_submit_smtp::{Secret, Security, SmtpConfig, SmtpSubmitter};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
        .port()
}

struct Server {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    transport: String,
    address: String,
    secret: String,
    starttls: u16,
    implicit: u16,
}

async fn start(max_size: Option<u64>) -> Server {
    let (starttls, implicit) = (free_port(), free_port());
    let state = state_dir();
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            "[serve]\ntls_cert = {cert:?}\ntls_key = {key:?}\n\
             [serve.submit]\nlisten_starttls = [\"127.0.0.1:{starttls}\"]\n\
             listen_tls = [\"127.0.0.1:{implicit}\"]\nhostname = \"localhost\"\n",
            cert = state.join("server.crt"),
            key = state.join("server.key"),
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique("submit")).await;
    let address = format!("{}@example.com", unique("ada"));
    let transport = api
        .post(
            &format!("/api/archives/{archive}/transports"),
            json!({
                "name": "GreenMail",
                "kind": "smtp",
                "host": "127.0.0.1",
                "port": GREENMAIL_SMTP,
                "security": "none",
                "allowed_from": ["*@example.com"],
                "limits": { "max_size": max_size },
            }),
        )
        .await
        .ok();
    let transport = id(&transport);
    api.post(
        &format!("/api/archives/{archive}/identities"),
        json!({ "address": address, "name": "Ada Example", "transport": transport, "active": true }),
    )
    .await
    .ok();
    let made = api
        .post(
            "/api/devices",
            json!({
                "name": "Mail client",
                "protocols": ["smtp"],
                "scopes": ["read", "send:mail"],
                "mail_archive": archive,
            }),
        )
        .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let secret = made.body["secret"].as_str().expect("a secret").to_owned();
    Server {
        holtmail,
        api,
        archive,
        transport,
        address,
        secret,
        starttls,
        implicit,
    }
}

impl Server {
    fn client(&self, port: u16, security: Security, secret: &str) -> SmtpSubmitter {
        let pem = std::fs::read(ca_file()).expect("the stack's CA");
        SmtpSubmitter::new(SmtpConfig {
            port,
            security,
            username: Some(OWNER_EMAIL.to_owned()),
            secret: Some(Secret::Password(secret.to_owned())),
            helo: "client.example.com".into(),
            timeout: Duration::from_secs(20),
            ..SmtpConfig::new("localhost")
        })
        .expect("a client")
        .with_roots(&TlsRoots::from_pem(&pem).expect("a CA"))
        .expect("a CA")
    }

    fn starttls_client(&self) -> SmtpSubmitter {
        self.client(self.starttls, Security::Starttls, &self.secret)
    }

    async fn outbox(&self) -> Vec<Value> {
        self.api
            .get(&format!("/api/archives/{}/outbox", self.archive))
            .await
            .ok()
            .as_array()
            .expect("a list")
            .clone()
    }

    async fn settled(&self, count: usize) -> Vec<Value> {
        let started = Instant::now();
        loop {
            let rows = self.outbox().await;
            let busy = rows.iter().any(|row| {
                ["queued", "sending", "retrying"]
                    .contains(&row["state"].as_str().unwrap_or_default())
            });
            if rows.len() >= count && !busy {
                return rows;
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "still busy: {rows:?}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn message(&self, subject: &str, extra: &str) -> Vec<u8> {
        format!(
            "From: Ada Example <{}>\r\nTo: bob@example.org\r\nSubject: {subject}\r\n\
             Date: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
             Message-ID: <{}@example.com>\r\n{extra}\r\nShall we meet on Thursday?\r\n",
            self.address,
            unique("message"),
        )
        .into_bytes()
    }

    fn envelope(&self, to: &[&str]) -> Envelope {
        Envelope {
            mail_from: self.address.clone(),
            recipients: to.iter().map(|a| (*a).to_owned()).collect(),
        }
    }
}

async fn delivered(address: &str, count: usize) -> Vec<Vec<u8>> {
    let started = Instant::now();
    loop {
        let mut imap = Seeder::login(&local(GREENMAIL.port), address, GREENMAIL.password).await;
        let bodies = imap.bodies("INBOX").await;
        imap.logout().await;
        if bodies.len() >= count || started.elapsed() > Duration::from_secs(30) {
            return bodies;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bcc_only_recipient_gets_the_mail_without_the_header() {
    require_it!();
    let server = start(None).await;
    let bob = format!("{}@example.org", unique("bob"));
    let hidden = format!("{}@example.org", unique("hidden"));
    let subject = unique("Thursday");
    let bytes = server.message(&subject, &format!("Bcc: {hidden}\r\n"));
    server
        .starttls_client()
        .submit(&server.envelope(&[&bob, &hidden]), &bytes)
        .await
        .expect("taken");
    let rows = server.settled(1).await;
    assert_eq!(rows[0]["state"], "sent", "{}", rows[0]);
    let archived = server
        .api
        .raw(rows[0]["message"].as_str().expect("archived"))
        .await;
    let text = String::from_utf8_lossy(&archived).into_owned();
    assert!(text.contains(&subject), "{text}");
    assert!(!text.to_ascii_lowercase().contains("bcc:"), "{text}");
    for person in [&bob, &hidden] {
        let bodies = delivered(person, 1).await;
        assert_eq!(bodies.len(), 1, "{person}");
        let body = String::from_utf8_lossy(&bodies[0]).to_ascii_lowercase();
        assert!(body.contains(&subject.to_ascii_lowercase()));
        assert!(!body.contains("bcc:"), "{body}");
    }

    let scanner = LogScanner::new()
        .secret("the app password", &server.secret)
        .secret("the recipient", &bob)
        .secret("the hidden recipient", &hidden)
        .secret("the subject", &subject);
    assert_eq!(scanner.findings(&server.holtmail.logs()), Vec::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_wrong_sender_and_oversized_mail() {
    require_it!();
    let server = start(Some(4096)).await;
    let bob = format!("{}@example.org", unique("bob"));
    let client = server.starttls_client();

    let mut wrong = server.envelope(&[&bob]);
    wrong.mail_from = "eve@example.org".into();
    let refused = client.submit(&wrong, &server.message("Wrong", "")).await;
    assert!(
        matches!(refused, Err(SubmitError::Refused { code: 553, .. })),
        "{refused:?}"
    );

    let forged = format!(
        "From: eve@example.org\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
         Message-ID: <{}@example.com>\r\n\r\nBody\r\n",
        unique("forged")
    );
    let refused = client
        .submit(&server.envelope(&[&bob]), forged.as_bytes())
        .await;
    assert!(
        matches!(refused, Err(SubmitError::Refused { code: 554, .. })),
        "{refused:?}"
    );

    let two_fields = format!(
        "From: eve@example.org\r\nFrom: {}\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
         Message-ID: <{}@example.com>\r\n\r\nBody\r\n",
        server.address,
        unique("two-fields")
    );
    let refused = client
        .submit(&server.envelope(&[&bob]), two_fields.as_bytes())
        .await;
    assert!(
        matches!(refused, Err(SubmitError::Refused { code: 554, .. })),
        "{refused:?}"
    );

    let padding = format!("X-Padding: {}\r\n", "x".repeat(8192));
    let refused = client
        .submit(&server.envelope(&[&bob]), &server.message("Big", &padding))
        .await;
    assert!(
        matches!(refused, Err(SubmitError::TooLarge { .. })),
        "{refused:?}"
    );

    assert!(server.outbox().await.is_empty(), "nothing was queued");

    let wrong_secret = server.client(server.starttls, Security::Starttls, "hm_not-the-secret");
    assert!(matches!(
        wrong_secret.check().await,
        Err(SubmitError::Auth(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_a_lost_reply_is_one_outbox_row() {
    require_it!();
    let server = start(None).await;
    let bob = format!("{}@example.org", unique("bob"));
    let bytes = server.message(&unique("Retried"), "");
    let client = server.starttls_client();
    client
        .submit(&server.envelope(&[&bob]), &bytes)
        .await
        .expect("taken");
    client
        .submit(&server.envelope(&[&bob]), &bytes)
        .await
        .expect("taken again");
    let rows = server.settled(1).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(delivered(&bob, 1).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn curl_sends_over_starttls_and_implicit_tls() {
    require_it!();
    let server = start(None).await;
    let dir = tempfile::tempdir().expect("a directory");
    for (name, url, port) in [
        ("starttls", "smtp", server.starttls),
        ("implicit", "smtps", server.implicit),
    ] {
        let bob = format!("{}@example.org", unique(name));
        let file = dir.path().join(name);
        std::fs::write(&file, server.message(&unique(name), "")).expect("written");
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--ssl-reqd", "--url"])
            .arg(format!("{url}://localhost:{port}"))
            .arg("--cacert")
            .arg(ca_file())
            .args(["--user", &format!("{OWNER_EMAIL}:{}", server.secret)])
            .args(["--mail-from", &server.address, "--mail-rcpt", &bob])
            .arg("--upload-file")
            .arg(&file)
            .output()
            .expect("curl runs");
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(delivered(&bob, 1).await.len(), 1, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transport_pointing_at_itself_fails_with_a_report() {
    require_it!();
    let server = start(None).await;
    let changed = server
        .api
        .patch(
            &format!("/api/transports/{}", server.transport),
            json!({ "port": server.starttls }),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    let check = server
        .api
        .post(
            &format!("/api/transports/{}/check", server.transport),
            json!({}),
        )
        .await
        .ok();
    assert_eq!(check["ok"], false, "{check}");

    let bob = format!("{}@example.org", unique("bob"));
    let subject = unique("Circle");
    server
        .starttls_client()
        .submit(&server.envelope(&[&bob]), &server.message(&subject, ""))
        .await
        .expect("taken");
    let rows = server.settled(1).await;
    assert_eq!(rows.len(), 1, "no second message: {rows:?}");
    assert_eq!(rows[0]["state"], "failed", "{}", rows[0]);
    assert_eq!(rows[0]["attempts"], 1, "not retried");
    let error = rows[0]["last_error"].as_str().unwrap_or_default();
    assert!(error.contains("Holtmail instance"), "{error}");
    let started = Instant::now();
    let report = loop {
        let messages = server.api.messages(&server.archive).await;
        let found = messages.into_iter().find(|m| {
            m["subject"]
                .as_str()
                .is_some_and(|s| s.starts_with("Undelivered"))
        });
        if let Some(found) = found {
            break found;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "no report");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let raw = server.api.raw(report["id"].as_str().expect("an id")).await;
    let parsed = holtmail_mime::parse_dsn(&raw).expect("a delivery status report");
    assert_eq!(
        parsed.recipients[0].final_recipient.as_deref(),
        Some(bob.as_str())
    );
    let started = Instant::now();
    loop {
        let row = server.outbox().await.remove(0);
        if row["note"].as_str().unwrap_or_default().contains("report") {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no note: {row}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        parsed.recipients[0].action,
        Some(holtmail_mime::DsnAction::Failed)
    );
}
