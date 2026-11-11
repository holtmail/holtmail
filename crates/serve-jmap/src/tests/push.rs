use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures::StreamExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::support::*;

async fn open(h: &Harness, creds: &Creds, query: &str) -> axum::body::BodyDataStream {
    let request = Request::get(format!("/jmap/eventsource?{query}"))
        .header(header::AUTHORIZATION, creds.basic())
        .body(Body::empty())
        .unwrap();
    let response = h.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    response.into_body().into_data_stream()
}

async fn next_event(stream: &mut axum::body::BodyDataStream) -> Option<(String, Value)> {
    loop {
        let chunk = within(stream.next()).await?.ok()?;
        let text = String::from_utf8_lossy(&chunk).to_string();
        if text.starts_with(':') {
            continue;
        }
        let name = text
            .lines()
            .find_map(|l| l.strip_prefix("event: "))?
            .to_string();
        let data = text.lines().find_map(|l| l.strip_prefix("data: "))?;
        return Some((name, serde_json::from_str(data).ok()?));
    }
}

#[tokio::test]
async fn a_change_is_pushed_and_revocation_ends_the_stream() {
    let h = harness();
    let account = h.account();
    let mut stream = open(&h, &h.editor, "types=Email&closeafter=no&ping=0").await;
    let ok = within(stream.next()).await.unwrap().unwrap();
    assert_eq!(&ok[..], b": ok\n\n");
    let set = h
        .call(
            &h.editor,
            "Email/set",
            json!({"accountId": account, "update": {h.email("welcome"): {"keywords/$seen": true}}}),
        )
        .await;
    h.hub.poke(h.archive);
    let (name, data) = next_event(&mut stream).await.expect("a state event");
    assert_eq!(name, "state");
    assert_eq!(data["@type"], "StateChange");
    assert_eq!(data["changed"][&account]["Email"], set["newState"]);
    assert!(
        data["changed"][&account].get("Mailbox").is_none(),
        "types filter"
    );

    h.hub.poke(h.archive);
    use holtmail_device::DeviceAuth;
    h.auth.revoke(h.editor.device).await.unwrap();
    let rest = within(async {
        let mut frames = Vec::new();
        while let Some(frame) = stream.next().await {
            frames.push(frame);
        }
        frames
    })
    .await;
    assert!(rest.iter().all(|f| f.is_ok()));
    assert_eq!(
        h.get("/jmap/session", Some(&h.editor)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn closeafter_state_ends_after_one_change() {
    let h = harness();
    let account = h.account();
    let mut stream = open(&h, &h.reader, "types=*&closeafter=state&ping=0").await;
    within(stream.next()).await.unwrap().unwrap();
    h.call(
        &h.reader,
        "Mailbox/set",
        json!({"accountId": account, "update": {"kept": {"isSubscribed": true}}}),
    )
    .await;
    h.hub.poke(h.archive);
    let (_, data) = next_event(&mut stream).await.expect("a state event");
    assert!(data["changed"][&account]["Mailbox"].is_string());
    assert!(within(stream.next()).await.is_none());
}

#[tokio::test]
async fn nothing_is_pushed_when_nothing_moved() {
    let h = harness();
    let mut stream = open(&h, &h.editor, "types=*&closeafter=no&ping=0").await;
    within(stream.next()).await.unwrap().unwrap();
    h.hub.poke(h.archive);
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;
    assert!(quiet.is_err(), "no event without a change");
}
