//! Rules against real mail servers: live mail delivered to the binary's SMTP listener runs an
//! auto-reply flow whose reply leaves through GreenMail's SMTP. Each sender gets one reply
//! (RFC 3834, once per sender in the node's days) marked `Auto-Submitted: auto-replied`; mail
//! from a list gets none; mail to an own alias address is answered from that alias; the command
//! line lists, exports and dry-runs the flow.

use std::time::{Duration, Instant};

use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{GREENMAIL, GREENMAIL_SMTP, local};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, submit, unique};
use serde_json::{Value, json};

const LISTEN: u16 = 42527;

async fn inbox(address: &str) -> Vec<Vec<u8>> {
    let mut imap = Seeder::login(&local(GREENMAIL.port), address, GREENMAIL.password).await;
    let bodies = imap.bodies("INBOX").await;
    imap.logout().await;
    bodies
}

async fn wait_for_inbox(address: &str, count: usize) -> Vec<Vec<u8>> {
    let started = Instant::now();
    loop {
        let bodies = inbox(address).await;
        if bodies.len() >= count {
            return bodies;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{count} messages did not arrive"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_runs(api: &Api, flow: &str, count: usize) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let runs = api.get(&format!("/api/flows/{flow}/runs")).await.ok();
        let runs = runs.as_array().cloned().unwrap_or_default();
        if runs.len() >= count && runs.iter().all(|r| r["status"] != "running") {
            return runs;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{count} runs did not happen: {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_outbox(api: &Api, archive: &str) {
    let started = Instant::now();
    loop {
        let entries = api
            .get(&format!("/api/archives/{archive}/outbox"))
            .await
            .ok();
        let busy = entries.as_array().unwrap().iter().any(|e| {
            ["queued", "sending", "retrying"].contains(&e["state"].as_str().unwrap_or_default())
        });
        if !busy {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the outbox did not empty: {entries}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn header<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    let head = raw.split("\r\n\r\n").next().unwrap_or_default();
    head.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn mail(from: &str, to: &str, subject: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{}@example.org>\r\nDate: Wed, 11 Nov 2026 08:00:00 +0000\r\n{extra}\r\nHello, is anyone there?\r\n",
        unique("m")
    )
    .into_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_replies_once_per_sender_never_to_lists_and_through_an_alias() {
    require_it!();
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            r#"
[write]
undo_seconds = 1

[smtp]
listen = "127.0.0.1:{LISTEN}"
hostname = "localhost"
allow = ["0.0.0.0/0", "::/0"]
[[smtp.route]]
recipient = "ada@example.com"
archive = "Rules"
folder = "Inbox"
[[smtp.route]]
recipient = "shop@example.com"
archive = "Rules"
folder = "Inbox"
"#
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.archive_named("Rules").await;

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
    let check = api
        .post(&format!("/api/transports/{transport}/check"), json!({}))
        .await
        .ok();
    assert_eq!(check["ok"], true, "{check}");
    for (address, name) in [
        ("ada@example.com", "Ada"),
        ("shop@example.com", "Ada's shop"),
    ] {
        let identity = api
            .post(
                &format!("/api/archives/{archive}/identities"),
                json!({ "address": address, "name": name, "transport": transport, "active": true }),
            )
            .await
            .ok();
        assert_eq!(identity["active"], true, "{identity}");
    }

    let created = api
        .post(
            &format!("/api/archives/{archive}/flows"),
            json!({
                "name": "Away",
                "flow": {
                    "trigger": { "type": "message.archived", "next": "reply" },
                    "nodes": [{
                        "id": "reply", "type": "reply",
                        "params": { "identity": "auto", "body": "Away until Monday, {{ sender.name }}." }
                    }]
                }
            }),
        )
        .await
        .ok();
    assert_eq!(created["mode"], "test", "{created}");
    let flow = created["id"].as_str().unwrap().to_owned();
    api.patch(&format!("/api/flows/{flow}"), json!({ "mode": "on" }))
        .await
        .ok();

    let carol = format!("{}@example.org", unique("carol"));
    let first = unique("Lunch");
    let second = unique("Lunch again");
    for subject in [&first, &second] {
        let answer = submit::send(
            LISTEN,
            &carol,
            "ada@example.com",
            &mail(&format!("Carol <{carol}>"), "ada@example.com", subject, ""),
        )
        .await;
        assert!(answer.starts_with("250"), "{answer}");
    }
    let dave = format!("{}@example.org", unique("dave"));
    let picnic = unique("Picnic");
    let answer = submit::send(
        LISTEN,
        &dave,
        "ada@example.com",
        &mail(
            &format!("Dave <{dave}>"),
            "friends@lists.example.org",
            &picnic,
            "List-Id: <friends.lists.example.org>\r\n",
        ),
    )
    .await;
    assert!(answer.starts_with("250"), "{answer}");
    let frank = format!("{}@example.org", unique("frank"));
    let order = unique("Order");
    let answer = submit::send(
        LISTEN,
        &frank,
        "shop@example.com",
        &mail(&format!("Frank <{frank}>"), "shop@example.com", &order, ""),
    )
    .await;
    assert!(answer.starts_with("250"), "{answer}");

    let runs = wait_for_runs(&api, &flow, 4).await;
    wait_for_outbox(&api, &archive).await;
    let skipped: Vec<&str> = runs
        .iter()
        .filter_map(|r| r["plan"]["path"].as_array())
        .flatten()
        .filter_map(|s| s["reason"]["code"].as_str())
        .collect();
    assert!(skipped.contains(&"mailing_list"), "{runs:?}");
    assert!(skipped.contains(&"replied_recently"), "{runs:?}");

    let replies = wait_for_inbox(&carol, 1).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let replies_again = inbox(&carol).await;
    assert_eq!(replies_again.len(), 1, "one reply per sender");
    let reply = String::from_utf8_lossy(&replies[0]).into_owned();
    assert_eq!(
        header(&reply, "Auto-Submitted"),
        Some("auto-replied"),
        "{reply}"
    );
    assert!(
        header(&reply, "From").is_some_and(|f| f.contains("ada@example.com")),
        "{reply}"
    );
    assert!(
        header(&reply, "Subject").is_some_and(|s| s.contains(&first) || s.contains(&second)),
        "{reply}"
    );
    assert!(reply.contains("Away until Monday, Carol."), "{reply}");
    assert!(inbox(&dave).await.is_empty(), "no reply to a list");

    let through_alias = wait_for_inbox(&frank, 1).await;
    let through_alias = String::from_utf8_lossy(&through_alias[0]).into_owned();
    assert!(
        header(&through_alias, "From").is_some_and(|f| f.contains("shop@example.com")),
        "the reply leaves from the alias the mail came to: {through_alias}"
    );
    assert!(
        !header(&through_alias, "From").is_some_and(|f| f.contains("ada@example.com")),
        "{through_alias}"
    );

    let listed = holtmail.command(&["rules", "list"], "");
    assert!(listed.status.success(), "{listed:?}");
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        listed.contains(&flow) && listed.contains("message.archived"),
        "{listed}"
    );
    let exported = holtmail.command(&["rules", "export", &flow], "");
    assert!(exported.status.success(), "{exported:?}");
    let exported: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(exported["nodes"][0]["type"], "reply");
    let message = runs
        .iter()
        .find_map(|r| r["message"].as_str())
        .unwrap()
        .to_owned();
    let tested = holtmail.command(&["rules", "test", &flow, "--message", &message], "");
    assert!(tested.status.success(), "{tested:?}");
    let plan: Value = serde_json::from_slice(&tested.stdout).unwrap();
    assert_eq!(plan["ran"], true, "{plan}");

    LogScanner::new()
        .secret("carol's address", &carol)
        .secret("dave's address", &dave)
        .secret("frank's address", &frank)
        .secret("a subject", &first)
        .secret("a subject", &second)
        .secret("a subject", &picnic)
        .secret("a subject", &order)
        .check(&holtmail.logs());
}
