use std::sync::Mutex;

use async_trait::async_trait;
use holtmail_secret_local::LocalSecretBox;
use holtmail_store_sqlite::SqliteStore;

use super::*;

#[derive(Default)]
struct Scripted {
    answers: Mutex<Vec<Result<u16, WebhookError>>>,
    sent: Mutex<Vec<WebhookRequest>>,
}

#[async_trait]
impl WebhookSender for Scripted {
    async fn send(&self, request: &WebhookRequest) -> Result<u16, WebhookError> {
        self.sent.lock().unwrap().push(request.clone());
        let mut answers = self.answers.lock().unwrap();
        if answers.is_empty() {
            Ok(200)
        } else {
            answers.remove(0)
        }
    }
}

fn header<'a>(request: &'a WebhookRequest, name: &str) -> &'a str {
    request
        .headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
        .unwrap()
}

fn days(n: i64) -> Timestamp {
    Timestamp::from_millis(Timestamp::now().as_millis() + n * 86_400_000)
}

#[tokio::test]
async fn delivers_signed_events_and_tries_again() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let sender = Arc::new(Scripted::default());
    let hooks = Hooks::new(
        store.clone(),
        Arc::new(LocalSecretBox::from_key([7; 32])),
        sender.clone(),
    );
    let wanted = vec!["task.finished".to_owned()];
    let (hook, secret) = hooks
        .create("https://hooks.example.com/a", &wanted)
        .await
        .unwrap();
    let (off, _) = hooks
        .create("https://hooks.example.com/off", &wanted)
        .await
        .unwrap();
    let mut off = store.get_webhook(off.id).await.unwrap().unwrap();
    off.enabled = false;
    store.update_webhook(&off).await.unwrap();
    hooks
        .create(
            "https://hooks.example.com/other",
            &["ledger.verified".to_owned()],
        )
        .await
        .unwrap();

    let event = Event::TaskFinished {
        task_id: "t1".into(),
        kind: "sync".into(),
        state: "done".into(),
        archive_id: None,
        account_id: None,
        done: 3,
        total: Some(3),
    };
    hooks.emit(event).await;
    sender.answers.lock().unwrap().push(Ok(500));
    assert_eq!(hooks.deliver_due().await.unwrap(), 1);
    assert_eq!(hooks.deliver_due().await.unwrap(), 0, "retried at once");
    let first = store.recent_deliveries(hook.id, 10).await.unwrap()[0].clone();
    assert_eq!((first.attempts, first.last_status), (1, Some(500)));
    let wait = first.next_at.unwrap().as_millis() - Timestamp::now().as_millis();
    assert!((25_000..=30_000).contains(&wait), "{wait}");

    assert_eq!(hooks.deliver_due_at(days(1)).await.unwrap(), 1);
    let done = store.recent_deliveries(hook.id, 10).await.unwrap()[0].clone();
    assert_eq!((done.attempts, done.next_at), (2, None));
    assert!(done.delivered_at.is_some());

    let sent = sender.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|r| r.url == "https://hooks.example.com/a"));
    let body: serde_json::Value = serde_json::from_slice(&sent[1].body).unwrap();
    assert_eq!(body["event"], "task.finished");
    assert_eq!(
        header(&sent[1], DELIVERY_HEADER),
        body["id"].as_str().unwrap()
    );
    assert_eq!(
        header(&sent[0], DELIVERY_HEADER),
        header(&sent[1], DELIVERY_HEADER)
    );
    assert!(holtmail_hooks::verify(
        secret.as_bytes(),
        header(&sent[1], SIGNATURE_HEADER),
        &sent[1].body,
        days(1).as_millis() / 1000,
        Duration::from_secs(60)
    ));

    let rotated = hooks.rotate(hook.id).await.unwrap();
    assert_ne!(rotated, secret);
    hooks.test(hook.id).await.unwrap();
    for _ in 0..holtmail_hooks::MAX_ATTEMPTS {
        sender
            .answers
            .lock()
            .unwrap()
            .push(Err(WebhookError::Unreachable("refused".into())));
    }
    hooks.deliver_due().await.unwrap();
    let sent = sender.sent.lock().unwrap().clone();
    let test = sent.last().unwrap();
    let now = Timestamp::now().as_millis() / 1000;
    assert_eq!(header(test, EVENT_HEADER), "webhook.test");
    assert!(holtmail_hooks::verify(
        rotated.as_bytes(),
        header(test, SIGNATURE_HEADER),
        &test.body,
        now,
        Duration::from_secs(60)
    ));
    assert!(!holtmail_hooks::verify(
        secret.as_bytes(),
        header(test, SIGNATURE_HEADER),
        &test.body,
        now,
        Duration::from_secs(60)
    ));
    for n in 1..holtmail_hooks::MAX_ATTEMPTS {
        hooks.deliver_due_at(days(2 * i64::from(n))).await.unwrap();
    }
    let given_up = store.recent_deliveries(hook.id, 10).await.unwrap()[0].clone();
    assert_eq!(given_up.attempts, holtmail_hooks::MAX_ATTEMPTS);
    assert_eq!((given_up.next_at, given_up.delivered_at), (None, None));
    assert_eq!(given_up.last_error.as_deref(), Some("unreachable: refused"));
}

#[test]
fn refuses_bad_addresses_and_events() {
    let events = vec!["task.finished".to_owned()];
    for url in [
        "",
        "hooks.example.com",
        "ftp://hooks.example.com/",
        "https://user:pass@hooks.example.com/",
        &format!("https://hooks.example.com/{}", "a".repeat(2_000)),
    ] {
        assert!(check(url, &events).is_err(), "{url}");
    }
    assert!(check("https://hooks.example.com", &["mail.read".to_owned()]).is_err());
    let (url, events) = check(
        " http://10.0.0.5:8080/in ",
        &[
            "task.finished".into(),
            "task.finished".into(),
            "messages.deleted".into(),
        ],
    )
    .unwrap();
    assert_eq!(url, "http://10.0.0.5:8080/in");
    assert_eq!(events, ["messages.deleted", "task.finished"]);
}
