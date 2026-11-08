//! Sending through Stalwart's JMAP submission: an account transport of a JMAP account hands the
//! draft to `EmailSubmission/set`, the recipient gets it, the provider keeps the sent copy in
//! Sent (moved there by `onSuccessUpdateEmail`, without `$draft`), and the next sync archives that
//! copy as the same message, once. A delivery status notification that later reaches the
//! sender's inbox, read by a live sync, marks the outbox message bounced for that recipient.

use std::time::{Duration, Instant};

use holtmail_integration::stalwart::{self, STALWART_PASSWORD, Seeder};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use reqwest::Method;
use serde_json::{Value, json};

async fn wait_for_state(api: &Api, outbox: &str, wanted: &[&str]) -> Value {
    let started = Instant::now();
    loop {
        let entry = api.get(&format!("/api/outbox/{outbox}")).await.ok();
        let state = entry["state"].as_str().unwrap_or_default().to_owned();
        if wanted.contains(&state.as_str()) {
            return entry;
        }
        assert!(
            !["failed", "uncertain", "cancelled"].contains(&state.as_str()),
            "ended {state}: {entry}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "still {state}: {entry}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn emails_with(seeder: &Seeder, role: &str, message_id: &str, at_least: usize) -> Vec<Value> {
    let mailbox = seeder.role(role).await;
    let started = Instant::now();
    loop {
        let found: Vec<Value> = seeder
            .emails(&mailbox)
            .await
            .into_iter()
            .filter(|e| e["messageId"][0] == message_id)
            .collect();
        if found.len() >= at_least || started.elapsed() > Duration::from_secs(30) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn delivery_report(sender: &str, recipient: &str, message_id: &str) -> Vec<u8> {
    format!(
        "From: Mail Delivery System <MAILER-DAEMON@mx.example.org>\r\n\
         To: {sender}\r\n\
         Subject: Undelivered Mail Returned to Sender\r\n\
         Message-ID: <{}@mx.example.org>\r\n\
         Date: Sat, 7 Nov 2026 10:05:00 +0000\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/report; report-type=delivery-status; boundary=\"report\"\r\n\
         \r\n\
         --report\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         Your message could not be delivered to one or more recipients.\r\n\
         --report\r\n\
         Content-Type: message/delivery-status\r\n\
         \r\n\
         Reporting-MTA: dns; mx.example.org\r\n\
         \r\n\
         Final-Recipient: rfc822; {recipient}\r\n\
         Action: failed\r\n\
         Status: 5.1.1\r\n\
         Diagnostic-Code: smtp; 550 5.1.1 mailbox unavailable\r\n\
         \r\n\
         --report\r\n\
         Content-Type: text/rfc822-headers\r\n\
         \r\n\
         From: {sender}\r\n\
         To: {recipient}\r\n\
         Message-ID: <{message_id}>\r\n\
         Subject: (removed)\r\n\
         \r\n\
         --report--\r\n",
        unique("dsn")
    )
    .into_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_through_stalwart_jmap_and_reads_the_bounce() {
    require_it!();
    let holtmail = Holtmail::start(Options {
        extra_config: "[write]\nundo_seconds = 1\n".into(),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique("send-jmap")).await;
    let sender = format!("{}@example.com", unique("ada"));
    let rcpt = format!("{}@example.com", unique("bob"));
    let sender_box = Seeder::new(&sender).await;
    let rcpt_box = Seeder::new(&rcpt).await;

    let added = api
        .add_jmap_account(
            &archive,
            json!({ "url": stalwart::base_url(), "username": sender, "password": STALWART_PASSWORD }),
        )
        .await
        .ok();
    let account = added["id"].as_str().unwrap().to_owned();
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    api.request(
        Method::PUT,
        &format!("/api/accounts/{account}/capabilities"),
        Some(&json!({ "capabilities": ["mail.send"] })),
    )
    .await
    .ok();
    let transport = api
        .post(
            &format!("/api/archives/{archive}/transports"),
            json!({ "name": "Stalwart JMAP", "kind": "account", "account": account }),
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
            json!({ "address": sender, "name": "Ada Example", "transport": transport, "active": true }),
        )
        .await
        .ok();
    assert_eq!(identity["sent_copy"], "provider", "{identity}");

    let subject = unique("Thursday lunch");
    let draft = api
        .post(
            &format!("/api/archives/{archive}/drafts"),
            json!({ "payload": {
                "to": [{ "address": rcpt }],
                "subject": subject,
                "text": "Noon at the usual place?\nAda",
            } }),
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
    let sent = wait_for_state(&api, &outbox, &["sent"]).await;
    assert_eq!(sent["recipients"][0]["state"], "accepted", "{sent}");
    let message_id = sent["rfc_message_id"].as_str().unwrap().to_owned();
    let archived = api.raw(sent["message"].as_str().expect("archived")).await;

    let received = emails_with(&rcpt_box, "inbox", &message_id, 1).await;
    assert_eq!(received.len(), 1, "the recipient did not get it");
    let copies = emails_with(&sender_box, "sent", &message_id, 1).await;
    assert_eq!(copies.len(), 1, "no sent copy in Sent");
    assert_eq!(copies[0]["keywords"]["$draft"], Value::Null, "{copies:?}");
    assert_eq!(
        copies[0]["mailboxIds"].as_object().unwrap().len(),
        1,
        "the sent copy stayed in Drafts: {copies:?}"
    );
    assert_eq!(copies[0]["size"].as_u64(), Some(archived.len() as u64));

    let synced = api.sync(&archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    let matching: Vec<Value> = api
        .messages(&archive)
        .await
        .into_iter()
        .filter(|m| m["subject"] == subject.as_str())
        .collect();
    assert_eq!(matching.len(), 1, "the sent copy was archived twice");
    let after = api.get(&format!("/api/outbox/{outbox}")).await.ok();
    assert_eq!(
        after["synced_copy"],
        Value::Null,
        "identical bytes were linked"
    );

    let inbox = sender_box.role("inbox").await;
    sender_box
        .import(
            &delivery_report(&sender, &rcpt, &message_id),
            &[&inbox],
            &[],
        )
        .await;
    let synced = api.sync(&archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    let bounced = wait_for_state(&api, &outbox, &["bounced"]).await;
    let recipient = &bounced["recipients"][0];
    assert_eq!(recipient["state"], "bounced", "{bounced}");
    assert_eq!(recipient["status"], "5.1.1");
    let audit = api.get("/api/audit").await.ok();
    assert!(
        audit
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "outbox_changed" && e["targets"] == json!([outbox, "bounced"])),
        "the bounce was not audited: {audit}"
    );

    LogScanner::new()
        .secret("the password", STALWART_PASSWORD)
        .secrets("an address", [sender.as_str(), rcpt.as_str()])
        .secret("the subject", &subject)
        .check(&holtmail.logs());
}
