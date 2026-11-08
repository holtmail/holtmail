//! Replies through an alias service against a real SimpleLogin (profile `aliases`): mail from
//! `ada@example.com` reaches Holtmail through a SimpleLogin alias, the reply is routed to the
//! sender's reverse alias from the alias's mailbox and handed to SimpleLogin's own SMTP, and it
//! reaches Ada through `sl-mx` with the alias as `From`, never the mailbox.

use std::time::{Duration, Instant};

use holtmail_integration::{Api, Holtmail, LogScanner, Options, profile, require_it, unique};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const LISTEN: u16 = 42526;
const SL_API: &str = "http://127.0.0.1:47777";
const SL_SMTP: u16 = 47381;
const KEY: &str = "it-sl-api-key";

async fn reply(io: &mut BufReader<TcpStream>) -> String {
    loop {
        let mut line = String::new();
        io.read_line(&mut line).await.unwrap();
        if line.as_bytes().get(3) != Some(&b'-') {
            return line.trim_end().to_owned();
        }
    }
}

async fn send(port: u16, from: &str, to: &str, data: &str) -> String {
    let mut io = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
    reply(&mut io).await;
    for command in [
        "EHLO client.example.com\r\n".to_owned(),
        format!("MAIL FROM:<{from}>\r\n"),
        format!("RCPT TO:<{to}>\r\n"),
        "DATA\r\n".to_owned(),
    ] {
        io.get_mut().write_all(command.as_bytes()).await.unwrap();
        let answer = reply(&mut io).await;
        assert!(
            answer.starts_with('2') || answer.starts_with('3'),
            "{command:?}: {answer}"
        );
    }
    let body = data.replace("\r\n", "\n").replace('\n', "\r\n");
    io.get_mut().write_all(body.as_bytes()).await.unwrap();
    io.get_mut().write_all(b"\r\n.\r\n").await.unwrap();
    let answer = reply(&mut io).await;
    io.get_mut().write_all(b"QUIT\r\n").await.unwrap();
    answer
}

async fn wait_for(api: &Api, archive: &str, test: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(found) = api.messages(archive).await.into_iter().find(&test) {
            return api.message(found["id"].as_str().unwrap()).await;
        }
        assert!(Instant::now() < deadline, "the message did not arrive");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn subject_has(message: &Value, marker: &str) -> bool {
    message["subject"]
        .as_str()
        .is_some_and(|s| s.contains(marker))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_leaves_through_the_reverse_alias() {
    require_it!();
    if !profile("aliases") {
        eprintln!("skipped: run with IT_PROFILES=aliases");
        return;
    }
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            r#"
[write]
undo_seconds = 1

[aliases]
sync_interval_minutes = 0
simplelogin_signing_domains = ["sl.example.org"]

[smtp]
listen = "127.0.0.1:{LISTEN}"
hostname = "localhost"
allow = ["0.0.0.0/0", "::/0"]
[[smtp.route]]
recipient = "owner@example.org"
archive = "Aliases"
folder = "Owner"
[[smtp.route]]
recipient = "ada@example.com"
archive = "Contact"
folder = "Ada"
"#
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.archive_named("Aliases").await;
    let contact = api.archive_named("Contact").await;

    let service = api
        .post(
            &format!("/api/archives/{archive}/alias-services"),
            json!({ "provider": "simplelogin", "base_url": SL_API, "key": KEY }),
        )
        .await
        .ok();
    let service_id = service["id"].as_str().unwrap().to_owned();
    let options = api
        .get(&format!("/api/alias-services/{service_id}/options"))
        .await
        .ok();
    let prefix = unique("reply");
    let made = api
        .post(
            &format!("/api/alias-services/{service_id}/aliases"),
            json!({ "prefix": prefix, "domain": options["domains"][0]["id"] }),
        )
        .await
        .ok();
    let alias = format!("{prefix}@sl.example.org");
    assert_eq!(made["address"], alias.as_str(), "{made}");
    assert_eq!(made["mailboxes"], json!(["owner@example.org"]));

    let transport = api
        .post(
            &format!("/api/archives/{archive}/transports"),
            json!({
                "name": "Mailbox SMTP",
                "kind": "smtp",
                "host": "127.0.0.1",
                "port": SL_SMTP,
                "security": "none",
                "allowed_from": ["owner@example.org"],
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
    let identity = api
        .post(
            &format!("/api/archives/{archive}/identities"),
            json!({
                "address": "owner@example.org",
                "name": "Owner",
                "transport": transport,
                "active": true,
            }),
        )
        .await
        .ok();
    assert_eq!(identity["active"], true, "{identity}");

    let marker = unique("lamp");
    let answer = send(
        SL_SMTP,
        "ada@example.com",
        &alias,
        &format!(
            "From: Ada Lovelace <ada@example.com>\nTo: {alias}\nSubject: Desk lamp {marker}\nMessage-ID: <{marker}@example.com>\nDate: Fri, 06 Nov 2026 16:00:00 +0000\n\nIs the lamp still for sale?\n"
        ),
    )
    .await;
    assert!(answer.starts_with("250"), "{answer}");
    let received = wait_for(&api, &archive, |m| subject_has(m, &marker)).await;
    assert_eq!(received["alias"]["signed"], true, "{received}");
    let reverse = received["alias"]["reverse_alias"]
        .as_str()
        .unwrap()
        .to_owned();

    let draft = api
        .post(
            &format!("/api/messages/{}/reply", received["id"].as_str().unwrap()),
            json!({}),
        )
        .await
        .ok();
    assert_eq!(draft["route"]["strategy"], "reverse_alias", "{draft}");
    assert_eq!(draft["route"]["strategy_source"], "default");
    assert_eq!(draft["route"]["from"], "owner@example.org");
    assert_eq!(draft["route"]["to"], reverse.as_str());
    assert_eq!(draft["route"]["alias"], alias.as_str());
    assert_eq!(draft["route"]["reasons"][0]["code"], "alias_mailbox");
    assert_eq!(draft["payload"]["to"][0]["address"], reverse.as_str());
    assert_eq!(draft["leaks"], json!([]));

    let mut leaky = draft["payload"].clone();
    leaky["cc"] = json!([{ "address": "carol@example.org" }]);
    let leaky = api
        .request(
            reqwest::Method::PUT,
            &format!("/api/drafts/{}", draft["id"].as_str().unwrap()),
            Some(&json!({ "version": draft["version"], "payload": leaky })),
        )
        .await
        .ok();
    assert_eq!(leaky["leaks"][0]["recipient"], "carol@example.org");
    assert_eq!(leaky["leaks"][0]["fix"], "use_reverse_alias");
    let refused = api
        .post(
            &format!("/api/drafts/{}/send", draft["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(refused.status.as_u16(), 409, "{}", refused.body);
    assert_eq!(refused.body["leaks"][0]["reason"], "cc_not_reverse_alias");
    let mut fixed = leaky["payload"].clone();
    fixed["cc"] = json!([]);
    api.request(
        reqwest::Method::PUT,
        &format!("/api/drafts/{}", draft["id"].as_str().unwrap()),
        Some(&json!({ "version": leaky["version"], "payload": fixed })),
    )
    .await
    .ok();

    let queued = api
        .post(
            &format!("/api/drafts/{}/send", draft["id"].as_str().unwrap()),
            json!({}),
        )
        .await
        .ok();
    let outbox = queued["id"].as_str().unwrap().to_owned();
    let started = Instant::now();
    let sent = loop {
        let entry = api.get(&format!("/api/outbox/{outbox}")).await.ok();
        let state = entry["state"].as_str().unwrap_or_default().to_owned();
        if !["queued", "sending", "retrying"].contains(&state.as_str()) {
            break entry;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "still {state}: {entry}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(sent["state"], "sent", "{sent}");
    assert_eq!(sent["recipients"][0]["address"], reverse.as_str());

    let delivered = wait_for(&api, &contact, |m| subject_has(m, &marker)).await;
    assert_eq!(delivered["from"][0]["email"], alias.as_str(), "{delivered}");
    assert_eq!(delivered["to"][0]["email"], "ada@example.com");
    let raw =
        String::from_utf8_lossy(&api.raw(delivered["id"].as_str().unwrap()).await).into_owned();
    let head = raw.split("\r\n\r\n").next().unwrap_or_default();
    assert!(
        !head
            .to_ascii_lowercase()
            .contains("from: owner@example.org")
            && !head.contains("\"Owner\" <owner@example.org>"),
        "the mailbox leaked into the delivered header:\n{head}"
    );

    LogScanner::new()
        .secret("the SimpleLogin key", KEY)
        .secret("the alias", &alias)
        .secret("the reverse alias", &reverse)
        .secret("the subject", &marker)
        .check(&holtmail.logs());
}
