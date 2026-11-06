//! Webhooks from the real binary to a receiver in the test: signatures, a retry after a refusal,
//! a new secret, turning a webhook off, and payloads without mail.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use holtmail_integration::data::Message;
use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{DOVECOT_IMAP, DOVECOT_IMAPS, DOVECOT_PASSWORD, local};
use holtmail_integration::{Api, Holtmail, require_it, unique};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Receiver {
    got: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
    refuse_next: Arc<Mutex<u32>>,
}

async fn receive(State(receiver): State<Receiver>, headers: HeaderMap, body: Bytes) -> StatusCode {
    receiver.got.lock().unwrap().push((headers, body));
    let mut refuse = receiver.refuse_next.lock().unwrap();
    if *refuse > 0 {
        *refuse -= 1;
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::NO_CONTENT
    }
}

async fn listen(receiver: Receiver) -> String {
    let app = Router::new()
        .route("/hook", post(receive))
        .with_state(receiver);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

impl Receiver {
    async fn wait(&self, count: usize, within: Duration) -> Vec<(HeaderMap, Bytes)> {
        let deadline = Instant::now() + within;
        loop {
            let got = self.got.lock().unwrap().clone();
            if got.len() >= count {
                return got;
            }
            assert!(
                Instant::now() < deadline,
                "{} of {count} deliveries arrived",
                got.len()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

fn signed(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    let now = chrono::Utc::now().timestamp();
    holtmail_hooks::verify(
        secret.as_bytes(),
        header(headers, "holtmail-signature"),
        body,
        now,
        Duration::from_secs(120),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn tells_a_receiver_with_signed_payloads() {
    require_it!();
    let receiver = Receiver::default();
    let url = listen(receiver.clone()).await;
    let holtmail = Holtmail::start_default().await;
    let api = Api::setup_owner(&holtmail).await;

    let created = api
        .post(
            "/api/webhooks",
            json!({ "url": url, "events": ["task.finished", "messages.deleted"] }),
        )
        .await
        .ok();
    let hook = created["webhook"]["id"].as_str().unwrap().to_owned();
    let secret = created["secret"].as_str().unwrap().to_owned();

    *receiver.refuse_next.lock().unwrap() = 1;
    api.post(&format!("/api/webhooks/{hook}/test"), json!({}))
        .await
        .ok();
    let got = receiver.wait(1, Duration::from_secs(15)).await;
    assert_eq!(header(&got[0].0, "holtmail-event"), "webhook.test");
    assert!(signed(&secret, &got[0].0, &got[0].1), "bad signature");
    let deliveries = api
        .get(&format!("/api/webhooks/{hook}/deliveries"))
        .await
        .ok();
    let waiting = &deliveries[0];
    assert_eq!(
        (
            waiting["attempts"].as_u64(),
            waiting["last_status"].as_u64()
        ),
        (Some(1), Some(503))
    );
    assert!(waiting["next_at"].is_i64(), "{waiting}");
    let got = receiver.wait(2, Duration::from_secs(60)).await;
    assert_eq!(
        header(&got[0].0, "holtmail-delivery"),
        header(&got[1].0, "holtmail-delivery"),
        "a retry is a new delivery"
    );
    let deliveries = api
        .get(&format!("/api/webhooks/{hook}/deliveries"))
        .await
        .ok();
    assert!(deliveries[0]["delivered_at"].is_i64(), "{deliveries}");

    let user = format!("{}@example.com", unique("hooks"));
    let message = Message::new(&user);
    let mut seeder = Seeder::login(&local(DOVECOT_IMAP), &user, DOVECOT_PASSWORD).await;
    seeder.append("INBOX", &[], None, &message.bytes()).await;
    seeder.logout().await;
    let archive = api.create_archive("Hooks").await;
    let account = api
        .add_imap(
            &archive,
            json!({
                "host": "localhost",
                "port": DOVECOT_IMAPS,
                "security": "tls",
                "username": user,
                "password": DOVECOT_PASSWORD,
            }),
        )
        .await;
    api.settle(&archive, &account).await;
    let got = receiver.wait(3, Duration::from_secs(20)).await;
    let (headers, body) = got.last().unwrap();
    assert_eq!(header(headers, "holtmail-event"), "task.finished");
    assert!(signed(&secret, headers, body));
    let payload: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(payload["data"]["kind"], "sync");
    assert_eq!(payload["data"]["account_id"], account.as_str());
    let text = String::from_utf8_lossy(body);
    assert!(
        !text.contains(&user) && !text.contains(&message.subject),
        "mail in a payload: {text}"
    );

    let rotated = api
        .post(&format!("/api/webhooks/{hook}/secret"), json!({}))
        .await
        .ok()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let listed = api.messages(&archive).await;
    let id = listed[0]["id"].as_str().unwrap().to_owned();
    let review = api
        .post(
            &format!("/api/archives/{archive}/deletions"),
            json!({ "message_ids": [id] }),
        )
        .await
        .ok();
    api.post(
        &format!("/api/deletions/{}/confirm", review["id"].as_str().unwrap()),
        json!({ "count": 1 }),
    )
    .await
    .ok();
    let got = receiver.wait(5, Duration::from_secs(20)).await;
    let deleted = got
        .iter()
        .find(|(h, _)| header(h, "holtmail-event") == "messages.deleted")
        .expect("messages.deleted");
    assert!(
        signed(&rotated, &deleted.0, &deleted.1),
        "the new secret is not used"
    );
    assert!(
        !signed(&secret, &deleted.0, &deleted.1),
        "the old secret still signs"
    );
    let payload: Value = serde_json::from_slice(&deleted.1).unwrap();
    assert_eq!(payload["data"]["messages"], 1);
    assert_eq!(
        payload["data"]["ledger_hash"].as_str().map(str::len),
        Some(64)
    );

    api.patch(
        &format!("/api/webhooks/{hook}"),
        json!({ "enabled": false }),
    )
    .await
    .ok();
    let before = receiver.got.lock().unwrap().len();
    api.sync(&archive, &account).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(
        receiver.got.lock().unwrap().len(),
        before,
        "a turned-off webhook was told"
    );
    assert_eq!(
        api.delete(&format!("/api/webhooks/{hook}"))
            .await
            .status
            .as_u16(),
        204
    );
}
