//! Sending through real submission servers: the binary hands drafts to GreenMail's SMTP and to
//! Stalwart's submission port (an account transport signing in with the account's own
//! password), the message reaches the recipient's mailbox, read back over IMAP, ending in exactly
//! the archived bytes; a refused recipient is reported per address; an undo inside the window
//! sends nothing; `holtmail send --wait` sends from the command line.

use std::time::{Duration, Instant};

use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{GREENMAIL, GREENMAIL_SMTP, local};
use holtmail_integration::stalwart::{self, STALWART_IMAP, STALWART_PASSWORD, STALWART_SUBMISSION};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use reqwest::Method;
use serde_json::{Value, json};

async fn start() -> (Holtmail, Api, String) {
    let holtmail = Holtmail::start(Options {
        extra_config: "[write]\nundo_seconds = 2\n".into(),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique("send")).await;
    (holtmail, api, archive)
}

async fn identity(api: &Api, archive: &str, transport: &str, address: &str) -> String {
    let check = api
        .post(&format!("/api/transports/{transport}/check"), json!({}))
        .await
        .ok();
    assert_eq!(check["ok"], true, "{check}");
    let identity = api
        .post(
            &format!("/api/archives/{archive}/identities"),
            json!({
                "address": address,
                "name": "Ada Example",
                "transport": transport,
                "active": true,
            }),
        )
        .await
        .ok();
    assert_eq!(identity["active"], true, "{identity}");
    identity["id"].as_str().unwrap().to_owned()
}

async fn draft(api: &Api, archive: &str, to: &[&str], subject: &str) -> String {
    let to: Vec<Value> = to.iter().map(|a| json!({ "address": a })).collect();
    let draft = api
        .post(
            &format!("/api/archives/{archive}/drafts"),
            json!({ "payload": {
                "to": to,
                "subject": subject,
                "text": "Shall we meet on Thursday at noon?\nAda",
            } }),
        )
        .await
        .ok();
    assert!(draft["route"].is_object(), "no route: {draft}");
    draft["id"].as_str().unwrap().to_owned()
}

async fn send(api: &Api, draft: &str) -> Value {
    let queued = api
        .post(&format!("/api/drafts/{draft}/send"), json!({}))
        .await
        .ok();
    assert_eq!(queued["state"], "queued", "{queued}");
    queued
}

async fn wait_until_done(api: &Api, outbox: &str) -> Value {
    let started = Instant::now();
    loop {
        let entry = api.get(&format!("/api/outbox/{outbox}")).await.ok();
        let state = entry["state"].as_str().unwrap_or_default().to_owned();
        if !["queued", "sending", "retrying"].contains(&state.as_str()) {
            return entry;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "still {state}: {entry}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn delivered(address: &str, user: &str, password: &str, count: usize) -> Vec<Vec<u8>> {
    let started = Instant::now();
    loop {
        let mut imap = Seeder::login(address, user, password).await;
        let bodies = imap.bodies("INBOX").await;
        imap.logout().await;
        if bodies.len() >= count || started.elapsed() > Duration::from_secs(30) {
            return bodies;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_through_greenmail_and_undoes_inside_the_window() {
    require_it!();
    let (holtmail, api, archive) = start().await;
    let sender = format!("{}@example.com", unique("ada"));
    let bob = format!("{}@example.org", unique("bob"));
    let carol = format!("{}@example.org", unique("carol"));
    let dave = format!("{}@example.org", unique("dave"));
    let subject = unique("Thursday plans");
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
            }),
        )
        .await
        .ok();
    let transport = transport["id"].as_str().unwrap().to_owned();
    identity(&api, &archive, &transport, &sender).await;

    let first = draft(&api, &archive, &[&bob], &subject).await;
    let queued = send(&api, &first).await;
    let sent = wait_until_done(&api, queued["id"].as_str().unwrap()).await;
    assert_eq!(sent["state"], "sent", "{sent}");
    assert_eq!(sent["recipients"][0]["state"], "accepted");
    let message = sent["message"].as_str().expect("archived");
    let archived = api.raw(message).await;
    let bodies = delivered(&local(GREENMAIL.port), &bob, GREENMAIL.password, 1).await;
    assert_eq!(bodies.len(), 1);
    // GreenMail stores the DATA without the CRLF that ends the last line.
    let stored = archived.strip_suffix(b"\r\n").expect("ends with CRLF");
    assert!(
        bodies[0].ends_with(stored),
        "the delivered message does not end with the archived bytes:\n{}\n---\n{}",
        String::from_utf8_lossy(&bodies[0]),
        String::from_utf8_lossy(&archived)
    );

    let undone = draft(&api, &archive, &[&carol], &unique("Never mind")).await;
    let queued = send(&api, &undone).await;
    let answer = api
        .post(
            &format!("/api/outbox/{}/undo", queued["id"].as_str().unwrap()),
            json!({}),
        )
        .await
        .ok();
    assert_eq!(answer["outbox"]["state"], "cancelled", "{answer}");
    assert_eq!(answer["draft"]["payload"]["to"][0]["address"], carol);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let mut imap = Seeder::login(&local(GREENMAIL.port), &carol, GREENMAIL.password).await;
    assert!(
        imap.bodies("INBOX").await.is_empty(),
        "the undone message left"
    );
    imap.logout().await;

    let cli_subject = unique("From the shell");
    let output = holtmail.command(
        &[
            "send",
            "--archive",
            &archive,
            "--from",
            &sender,
            "--to",
            &dave,
            "--subject",
            &cli_subject,
            "--body",
            "Sent by a script.",
            "--wait",
        ],
        "",
    );
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{printed}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(printed.contains(&format!("{dave}  accepted")), "{printed}");
    let bodies = delivered(&local(GREENMAIL.port), &dave, GREENMAIL.password, 1).await;
    assert!(String::from_utf8_lossy(&bodies[0]).contains(&cli_subject));
    let listed = holtmail.command(
        &["outbox", "list", "--archive", &archive, "--state", "sent"],
        "",
    );
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter(|l| l.contains("  sent "))
            .count(),
        2
    );

    LogScanner::new()
        .secrets(
            "an address",
            [sender.as_str(), bob.as_str(), carol.as_str(), dave.as_str()],
        )
        .secrets("a subject", [subject.as_str(), cli_subject.as_str()])
        .check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_through_stalwart_submission_with_a_refused_recipient() {
    require_it!();
    let (holtmail, api, archive) = start().await;
    let sender = format!("{}@example.com", unique("ada"));
    let rcpt = format!("{}@example.com", unique("bob"));
    let nobody = format!("{}@example.com", unique("nobody"));
    stalwart::create_user(&sender).await;
    stalwart::create_user(&rcpt).await;
    let account = api
        .add_imap(
            &archive,
            json!({
                "host": "localhost",
                "port": STALWART_IMAP,
                "security": "starttls",
                "username": sender,
                "password": STALWART_PASSWORD,
            }),
        )
        .await;
    let refused = api
        .post(
            &format!("/api/archives/{archive}/transports"),
            json!({
                "name": "Stalwart",
                "kind": "account",
                "account": account,
                "host": "localhost",
                "port": STALWART_SUBMISSION,
                "security": "starttls",
            }),
        )
        .await
        .ok();
    let transport = refused["id"].as_str().unwrap().to_owned();
    assert_eq!(refused["allowed_from"], json!([sender]));
    let check = api
        .post(&format!("/api/transports/{transport}/check"), json!({}))
        .await
        .ok();
    assert_eq!(check["ok"], false, "sent without mail.send: {check}");
    api.request(
        Method::PUT,
        &format!("/api/accounts/{account}/capabilities"),
        Some(&json!({ "capabilities": ["mail.send"] })),
    )
    .await
    .ok();
    identity(&api, &archive, &transport, &sender).await;

    let subject = unique("Quarterly figures");
    let draft = draft(&api, &archive, &[&rcpt, &nobody], &subject).await;
    let queued = send(&api, &draft).await;
    let sent = wait_until_done(&api, queued["id"].as_str().unwrap()).await;
    assert_eq!(sent["state"], "sent", "{sent}");
    let recipients = sent["recipients"].as_array().unwrap();
    assert_eq!(recipients[0]["address"], rcpt);
    assert_eq!(recipients[0]["state"], "accepted");
    assert_eq!(recipients[1]["address"], nobody);
    assert_eq!(recipients[1]["state"], "refused", "{sent}");
    assert_eq!(recipients[1]["permanent"], true);
    assert!(recipients[1]["code"].as_u64().unwrap() >= 500);

    let archived = api.raw(sent["message"].as_str().expect("archived")).await;
    let bodies = delivered(&local(STALWART_IMAP), &rcpt, STALWART_PASSWORD, 1).await;
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].ends_with(&archived),
        "the delivered message does not end with the archived bytes:\n{}\n---\n{}",
        String::from_utf8_lossy(&bodies[0]),
        String::from_utf8_lossy(&archived)
    );

    let audit = api.get("/api/audit").await.ok();
    let kinds: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    for kind in [
        "capabilities_changed",
        "transport_changed",
        "identity_changed",
        "outbox_changed",
        "mail_send",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
    LogScanner::new()
        .secret("the IMAP password", STALWART_PASSWORD)
        .secrets(
            "an address",
            [sender.as_str(), rcpt.as_str(), nobody.as_str()],
        )
        .secret("the subject", &subject)
        .check(&holtmail.logs());
}
