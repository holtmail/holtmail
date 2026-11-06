//! Proves the harness: seed Dovecot, archive it over TLS with the stack's CA through the real
//! binary and its API, and find nothing seeded in the logs.

use holtmail_integration::data::Message;
use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{DOVECOT_IMAP, DOVECOT_IMAPS, DOVECOT_PASSWORD, local};
use holtmail_integration::{Api, Holtmail, LogScanner, require_it, unique};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn archives_a_dovecot_mailbox_over_tls() {
    require_it!();
    let user = format!("{}@example.com", unique("smoke"));
    let messages = [Message::new(&user), Message::new(&user)];
    let mut seeder = Seeder::login(&local(DOVECOT_IMAP), &user, DOVECOT_PASSWORD).await;
    for message in &messages {
        seeder
            .append("INBOX", &["\\Seen"], None, &message.bytes())
            .await;
    }
    seeder.logout().await;

    let holtmail = Holtmail::start_default().await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Smoke").await;
    let account = api
        .add_imap_account(
            &archive,
            json!({
                "host": "localhost",
                "port": DOVECOT_IMAPS,
                "security": "tls",
                "username": user,
                "password": DOVECOT_PASSWORD,
            }),
        )
        .await
        .ok();
    let account = holtmail_integration::api::id(&account);

    let synced = api.sync(&archive, &account).await;
    assert_eq!(
        synced["status"]["error"],
        serde_json::Value::Null,
        "{synced}"
    );

    let listed = api.messages(&archive).await;
    let mut subjects: Vec<_> = listed
        .iter()
        .map(|m| m["subject"].as_str().unwrap_or_default().to_owned())
        .collect();
    subjects.sort();
    let mut seeded: Vec<_> = messages.iter().map(|m| m.subject.clone()).collect();
    seeded.sort();
    assert_eq!(subjects, seeded);

    LogScanner::new()
        .secret("the IMAP password", DOVECOT_PASSWORD)
        .secret("the IMAP user", &user)
        .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
        .secrets("a sender", messages.iter().map(|m| m.from.as_str()))
        .check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_the_stack_certificate_without_the_ca() {
    require_it!();
    let holtmail = Holtmail::start(holtmail_integration::Options {
        trust_stack_ca: false,
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Untrusted").await;
    let refused = api
        .add_imap_account(
            &archive,
            json!({
                "host": "localhost",
                "port": DOVECOT_IMAPS,
                "security": "tls",
                "username": format!("{}@example.com", unique("untrusted")),
                "password": DOVECOT_PASSWORD,
            }),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    let reason = refused.body["error"].as_str().unwrap_or_default();
    assert!(reason.contains("UnknownIssuer"), "{reason}");
    LogScanner::new()
        .secret("the IMAP password", DOVECOT_PASSWORD)
        .check(&holtmail.logs());
}
