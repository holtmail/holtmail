//! Email aliases against a real SimpleLogin (profile `aliases`): the service is connected with
//! its API key, an alias is created, changed, switched and deleted through Holtmail, and mail sent
//! to it travels SimpleLogin → Postfix with OpenDKIM (which checks SimpleLogin's signature
//! against the stack's DNS) → Holtmail's SMTP listener. Forged forwarding fields sent straight
//! to Postfix are archived as not verified.

use std::time::{Duration, Instant};

use holtmail_integration::api::Reply;
use holtmail_integration::{Api, Holtmail, LogScanner, Options, profile, require_it, unique};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const LISTEN: u16 = 42526;
const SL_API: &str = "http://127.0.0.1:47777";
const SL_SMTP: u16 = 47381;
const MX: u16 = 47025;
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

/// Sends one message over plain SMTP and returns the server's answer to the data.
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

async fn sl(path: &str) -> Value {
    reqwest::Client::new()
        .get(format!("{SL_API}{path}"))
        .header("Authentication", KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn sl_alias(address: &str) -> Option<Value> {
    let mut page = 0;
    loop {
        let answer = sl(&format!("/api/v2/aliases?page_id={page}")).await;
        let aliases = answer["aliases"].as_array().cloned().unwrap_or_default();
        if let Some(found) = aliases.iter().find(|a| a["email"] == address) {
            return Some(found.clone());
        }
        if aliases.len() < 20 {
            return None;
        }
        page += 1;
    }
}

async fn wait_for(api: &Api, archive: &str, marker: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(found) = api
            .messages(archive)
            .await
            .into_iter()
            .find(|m| m["subject"].as_str().is_some_and(|s| s.contains(marker)))
        {
            return api.message(found["id"].as_str().unwrap()).await;
        }
        assert!(Instant::now() < deadline, "{marker} did not arrive");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn alias_in(list: &Value, address: &str) -> Value {
    list["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["address"] == address)
        .cloned()
        .unwrap_or_else(|| panic!("{address} is not listed: {list}"))
}

#[track_caller]
fn refused(reply: Reply, words: &str) {
    assert_eq!(reply.status.as_u16(), 400, "{}", reply.body);
    assert!(
        reply.body["error"]
            .as_str()
            .is_some_and(|e| e.contains(words)),
        "{}",
        reply.body
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn simplelogin_end_to_end() {
    require_it!();
    if !profile("aliases") {
        eprintln!("skipped: run with IT_PROFILES=aliases");
        return;
    }
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            r#"
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
recipient = "other@example.org"
archive = "Aliases"
folder = "Other"
"#
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.archive_named("Aliases").await;
    let services = format!("/api/archives/{archive}/alias-services");

    refused(
        api.post(
            &services,
            json!({ "provider": "simplelogin", "base_url": SL_API, "key": "wrong-key" }),
        )
        .await,
        "refused the key",
    );
    let service = api
        .post(
            &services,
            json!({ "provider": "simplelogin", "name": "Self-hosted", "base_url": SL_API, "key": KEY }),
        )
        .await
        .ok();
    let service_id = service["id"].as_str().unwrap().to_owned();
    assert_eq!(service["sync_error"], Value::Null, "{service}");
    assert!(service["synced_at"].is_i64());

    let list_path = format!("/api/archives/{archive}/aliases");
    let listed = api.get(&list_path).await.ok();
    let newsletter = listed["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| {
            a["address"]
                .as_str()
                .unwrap()
                .starts_with("simplelogin-newsletter")
        })
        .cloned()
        .expect("the seeded alias is listed");
    assert_eq!(newsletter["source"], "service");
    assert_eq!(newsletter["mailboxes"], json!(["owner@example.org"]));

    let options = api
        .get(&format!("/api/alias-services/{service_id}/options"))
        .await
        .ok();
    assert_eq!(options["domains"][0]["label"], "@sl.example.org");
    assert_eq!(options["mailboxes"][0]["email"], "owner@example.org");
    let other_box = options["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["email"] == "other@example.org")
        .expect("the second mailbox")["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let prefix = unique("shop");
    let made = api
        .post(
            &format!("/api/alias-services/{service_id}/aliases"),
            json!({ "prefix": prefix, "domain": options["domains"][0]["id"], "note": "furniture" }),
        )
        .await
        .ok();
    let address = format!("{prefix}@sl.example.org");
    assert_eq!(made["address"], address.as_str(), "{made}");
    assert_eq!(made["note"], "furniture");
    refused(
        api.post(
            &format!("/api/alias-services/{service_id}/aliases"),
            json!({ "prefix": prefix, "domain": options["domains"][0]["id"] }),
        )
        .await,
        "already",
    );
    let at_service = sl_alias(&address).await.expect("created at SimpleLogin");
    assert_eq!(at_service["note"], "furniture");

    let alias_path = format!("/api/archives/{archive}/aliases/{address}");
    let moved = api
        .patch(
            &alias_path,
            json!({ "mailbox_ids": [other_box], "name": "Shop", "note": "" }),
        )
        .await
        .ok();
    assert_eq!(moved["mailboxes"], json!(["other@example.org"]));
    assert_eq!(moved["name"], "Shop");
    assert_eq!(moved["note"], Value::Null);
    let at_service = sl_alias(&address).await.unwrap();
    assert_eq!(at_service["mailboxes"][0]["email"], "other@example.org");
    assert_eq!(at_service["name"], "Shop");

    let marker = unique("parcel");
    let answer = send(
        SL_SMTP,
        "ada@example.com",
        &address,
        &format!(
            "From: Ada Lovelace <ada@example.com>\nTo: {address}\nSubject: Parcel {marker}\nMessage-ID: <{marker}@example.com>\nDate: Fri, 06 Nov 2026 16:00:00 +0000\n\nOn its way.\n"
        ),
    )
    .await;
    assert!(answer.starts_with("250"), "{answer}");
    let message = wait_for(&api, &archive, &marker).await;
    let alias = &message["alias"];
    assert_eq!(alias["provider"], "simplelogin", "{message}");
    assert_eq!(alias["alias"], address.as_str());
    assert_eq!(alias["sender"]["email"], "ada@example.com");
    assert_eq!(alias["sender"]["name"], "Ada Lovelace");
    let raw = api.raw(message["id"].as_str().unwrap()).await;
    let head = String::from_utf8_lossy(&raw);
    let head = head.split("\r\n\r\n").next().unwrap_or_default();
    assert_eq!(
        alias["signed"], true,
        "OpenDKIM did not pass: {alias}\n{head}"
    );
    assert_eq!(alias["known"], true);
    let reverse = alias["reverse_alias"].as_str().unwrap().to_owned();
    assert!(reverse.ends_with("@sl.example.org"), "{reverse}");
    assert_eq!(message["from"][0]["email"], reverse.as_str());
    assert_eq!(message["to"][0]["email"], address.as_str());

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let hits = api
            .get(&format!(
                "/api/archives/{archive}/search?q=from%3Aada%40example.com"
            ))
            .await
            .ok();
        if hits["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == message["id"])
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "from: does not find the hidden sender: {hits}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let second = unique("parcel");
    send(
        SL_SMTP,
        "ada@example.com",
        &address,
        &format!(
            "From: Ada Lovelace <ada@example.com>\nTo: {address}\nSubject: Parcel {second}\nMessage-ID: <{second}@example.com>\n\nDelivered.\n"
        ),
    )
    .await;
    let again = wait_for(&api, &archive, &second).await;
    assert_eq!(
        again["alias"]["reverse_alias"],
        reverse.as_str(),
        "one reverse alias per sender"
    );

    let listed = api.get(&list_path).await.ok();
    let shop = alias_in(&listed, &address);
    assert_eq!(shop["messages"], 2);
    assert_eq!(shop["senders"], 1);
    assert_eq!(shop["signed"], 2);
    assert_eq!(shop["accounts"][0]["messages"], 2, "{shop}");

    api.post(&format!("/api/alias-services/{service_id}/sync"), json!({}))
        .await
        .ok();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let people = api.people(&archive).await;
        let emails: Vec<String> = people
            .iter()
            .flat_map(|p| p["emails"].as_array().cloned().unwrap_or_default())
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect();
        assert!(
            !emails.iter().any(|e| e == &reverse || e == &address),
            "a reverse alias or the alias became a person: {emails:?}"
        );
        if emails.iter().any(|e| e == "ada@example.com") {
            break;
        }
        assert!(Instant::now() < deadline, "Ada is not a person: {emails:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let off = api
        .patch(&alias_path, json!({ "enabled": false }))
        .await
        .ok();
    assert_eq!(off["enabled"], false);
    assert_eq!(sl_alias(&address).await.unwrap()["enabled"], false);
    let blocked = unique("blocked");
    send(
        SL_SMTP,
        "ada@example.com",
        &address,
        &format!("From: Ada <ada@example.com>\nTo: {address}\nSubject: Blocked {blocked}\n\nNot forwarded.\n"),
    )
    .await;
    let on = api
        .patch(&alias_path, json!({ "enabled": true }))
        .await
        .ok();
    assert_eq!(on["enabled"], true);

    let forged = unique("forged");
    let answer = send(
        MX,
        "someone@example.com",
        "owner@example.org",
        &format!(
            "X-SimpleLogin-Type: Forward\nX-SimpleLogin-Envelope-To: nobody@sl.example.org\nX-SimpleLogin-Original-From: Bank <security@example.com>\nFrom: \"Bank - security at example.com\" <someone@example.com>\nTo: owner@example.org\nSubject: Verify {forged}\nMessage-ID: <{forged}@example.com>\n\nClick here.\n"
        ),
    )
    .await;
    assert!(answer.starts_with("250"), "{answer}");
    let fake = wait_for(&api, &archive, &forged).await;
    assert_eq!(fake["alias"]["alias"], "nobody@sl.example.org");
    assert_eq!(fake["alias"]["signed"], false, "{fake}");
    assert_eq!(fake["alias"]["known"], false);
    assert!(
        !api.messages(&archive)
            .await
            .iter()
            .any(|m| m["subject"].as_str().is_some_and(|s| s.contains(&blocked))),
        "SimpleLogin forwarded mail to a switched-off alias"
    );
    let listed = api.get(&list_path).await.ok();
    let nobody = alias_in(&listed, "nobody@sl.example.org");
    assert_eq!(nobody["source"], "mail");
    assert_eq!(nobody["signed"], 0);

    let deleted = api.delete(&format!("{alias_path}?at_service=true")).await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.body);
    assert!(sl_alias(&address).await.is_none(), "still at SimpleLogin");
    let listed = api.get(&list_path).await.ok();
    assert_eq!(alias_in(&listed, &address)["source"], "mail");

    let gone = api
        .delete(&format!(
            "/api/alias-services/{service_id}?keep_aliases=false"
        ))
        .await;
    assert_eq!(gone.status.as_u16(), 204);
    let listed = api.get(&list_path).await.ok();
    assert_eq!(listed["services"], json!([]));
    assert!(
        !listed["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["source"] == "service"),
        "{listed}"
    );

    LogScanner::new()
        .secret("the SimpleLogin key", KEY)
        .secret("the alias", &address)
        .secret("the subject", &marker)
        .check(&holtmail.logs());
}
