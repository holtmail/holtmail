use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use holtmail_core::ArchiveId;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::auth::{self, Authed};
use crate::request;
use crate::{Inner, ids};

const TYPES: [&str; 4] = ["Email", "Mailbox", "Thread", "EmailSubmission"];
const MIN_PING: u64 = 10;

type States = HashMap<ArchiveId, HashMap<&'static str, String>>;

async fn current(
    inner: &Inner,
    authed: &Authed,
    archive: ArchiveId,
) -> HashMap<&'static str, String> {
    let mut out = HashMap::new();
    if let Ok(state) = inner.services.view.state(&authed.viewer(archive)).await {
        out.insert("Email", state.email.to_string());
        out.insert("Mailbox", state.mailbox.to_string());
        out.insert("Thread", state.thread.to_string());
    }
    inner
        .sent
        .refresh(&*inner.services.submission, authed, archive)
        .await;
    out.insert(
        "EmailSubmission",
        inner.sent.state(authed.principal.user, archive),
    );
    out
}

async fn snapshot(inner: &Inner, authed: &Authed) -> States {
    let mut out = HashMap::new();
    for archive in &authed.principal.archives {
        out.insert(*archive, current(inner, authed, *archive).await);
    }
    out
}

/// The types of `archive` whose state moved since `known`, as a `StateChange`'s `changed`
/// member; `known` is brought up to date.
async fn changed(
    inner: &Inner,
    authed: &Authed,
    archive: ArchiveId,
    known: &mut States,
    types: Option<&[String]>,
) -> Option<Value> {
    let now = current(inner, authed, archive).await;
    let before = known.entry(archive).or_default();
    let mut moved = Map::new();
    for (kind, state) in &now {
        let wanted = types.is_none_or(|t| t.iter().any(|x| x == kind));
        if wanted && before.get(kind) != Some(state) {
            moved.insert(kind.to_string(), json!(state));
        }
    }
    *before = now;
    if moved.is_empty() {
        return None;
    }
    let mut changed = Map::new();
    changed.insert(ids::account(archive), Value::Object(moved));
    Some(Value::Object(changed))
}

struct Listeners {
    rx: mpsc::Receiver<ArchiveId>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Listeners {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn listen(inner: &Inner, authed: &Authed) -> Listeners {
    let (tx, rx) = mpsc::channel(64);
    let tasks = authed
        .principal
        .archives
        .iter()
        .map(|archive| {
            let mut wake = inner.services.hub.listen(*archive, authed.principal.user);
            let tx = tx.clone();
            let archive = *archive;
            tokio::spawn(async move {
                while wake.changed().await.is_some() {
                    if tx.send(archive).await.is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    Listeners { rx, tasks }
}

fn types_of(list: &str) -> Option<Vec<String>> {
    if list.is_empty() || list == "*" {
        return None;
    }
    Some(
        list.split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| TYPES.contains(&t.as_str()))
            .collect(),
    )
}

#[derive(Deserialize)]
pub(crate) struct EventQuery {
    types: Option<String>,
    closeafter: Option<String>,
    ping: Option<u64>,
}

fn event(name: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
}

/// `GET /jmap/eventsource` (RFC 8620 §7.3): `state` events with a `StateChange` when a type of
/// an account moved, `ping` events every `ping` seconds (at least 10; none for 0), closed
/// after the first change with `closeafter=state`, and closed at once when the device is
/// revoked or loses a right.
pub(crate) async fn event_source(
    State(inner): State<Arc<Inner>>,
    Query(query): Query<EventQuery>,
    request: Request,
) -> Response {
    let authed =
        match auth::authenticate(&inner, request.headers(), auth::peer(request.extensions())).await
        {
            Ok(authed) => authed,
            Err(response) => return *response,
        };
    let types = types_of(query.types.as_deref().unwrap_or("*"));
    let close_after = query.closeafter.as_deref() == Some("state");
    let ping = query.ping.filter(|p| *p > 0).map(|p| p.max(MIN_PING));
    let (tx, rx) = mpsc::channel::<Bytes>(16);
    let revoked = inner.revoked.subscribe();
    tokio::spawn(stream(
        inner.clone(),
        authed,
        types,
        close_after,
        ping,
        tx,
        revoked,
    ));
    let body = Body::from_stream(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<_, Infallible>(b), rx))
    }));
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

async fn stream(
    inner: Arc<Inner>,
    authed: Authed,
    types: Option<Vec<String>>,
    close_after: bool,
    ping: Option<u64>,
    tx: mpsc::Sender<Bytes>,
    mut revoked: broadcast::Receiver<holtmail_core::DeviceId>,
) {
    let mut listeners = listen(&inner, &authed);
    let mut known = snapshot(&inner, &authed).await;
    if tx.send(Bytes::from_static(b": ok\n\n")).await.is_err() {
        return;
    }
    let period = Duration::from_secs(ping.unwrap_or(24 * 60 * 60));
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    loop {
        tokio::select! {
            archive = listeners.rx.recv() => {
                let Some(archive) = archive else { break };
                if let Some(change) = changed(&inner, &authed, archive, &mut known, types.as_deref()).await {
                    let data = json!({"@type": "StateChange", "changed": change});
                    if tx.send(event("state", &data)).await.is_err() || close_after {
                        break;
                    }
                }
            }
            _ = ticker.tick(), if ping.is_some() => {
                let data = json!({"interval": ping.unwrap_or_default()});
                if tx.send(event("ping", &data)).await.is_err() {
                    break;
                }
            }
            device = revoked.recv() => {
                match device {
                    Ok(device) if device == authed.principal.device => break,
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
            _ = tx.closed() => break,
        }
    }
}

/// `GET /jmap/ws` (RFC 8887): requests and responses over a WebSocket with the `jmap`
/// subprotocol, and `StateChange` pushes after `WebSocketPushEnable`.
pub(crate) async fn websocket(State(inner): State<Arc<Inner>>, request: Request) -> Response {
    if auth::foreign_origin(request.headers()) {
        return auth::forbidden("a WebSocket from another site's page is refused");
    }
    let authed =
        match auth::authenticate(&inner, request.headers(), auth::peer(request.extensions())).await
        {
            Ok(authed) => authed,
            Err(response) => return *response,
        };
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let revoked = inner.revoked.subscribe();
    upgrade
        .protocols(["jmap"])
        .max_message_size(inner.settings.limits.max_size_request)
        .on_upgrade(move |socket| session(inner, authed, socket, revoked))
}

fn problem_message(request_id: Option<&Value>, problem: &crate::error::Problem) -> Value {
    let mut out = problem.json();
    out["@type"] = json!("RequestError");
    if let Some(id) = request_id {
        out["requestId"] = id.clone();
    }
    out
}

async fn session(
    inner: Arc<Inner>,
    authed: Authed,
    mut socket: WebSocket,
    mut revoked: broadcast::Receiver<holtmail_core::DeviceId>,
) {
    let mut listeners = listen(&inner, &authed);
    let mut push: Option<Option<Vec<String>>> = None;
    let mut known = States::new();
    loop {
        tokio::select! {
            message = socket.recv() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => break,
                };
                let reply = match serde_json::from_str::<Value>(&text) {
                    Err(_) => problem_message(None, &crate::error::Problem::new("notJSON", "the message is not JSON")),
                    Ok(value) => match value.get("@type").and_then(Value::as_str) {
                        Some("Request") => {
                            let id = value.get("id").cloned();
                            let answer = match request::slot(&inner, &authed) {
                                Err(problem) => Err(problem),
                                Ok(_slot) => request::process(&inner, &authed, value).await,
                            };
                            match answer {
                                Ok(mut response) => {
                                    response["@type"] = json!("Response");
                                    if let Some(id) = id {
                                        response["requestId"] = id;
                                    }
                                    response
                                }
                                Err(problem) => problem_message(id.as_ref(), &problem),
                            }
                        }
                        Some("WebSocketPushEnable") => {
                            let types = value.get("dataTypes").and_then(Value::as_array).map(|list| {
                                list.iter().filter_map(|t| t.as_str().map(str::to_string)).collect::<Vec<_>>()
                            });
                            known = snapshot(&inner, &authed).await;
                            push = Some(types);
                            continue;
                        }
                        Some("WebSocketPushDisable") => {
                            push = None;
                            continue;
                        }
                        _ => problem_message(value.get("id"), &crate::error::Problem::new("notRequest", "unknown message type")),
                    },
                };
                if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                    break;
                }
            }
            archive = listeners.rx.recv() => {
                let Some(archive) = archive else { break };
                let Some(types) = &push else { continue };
                if let Some(change) = changed(&inner, &authed, archive, &mut known, types.as_deref()).await {
                    let data = json!({"@type": "StateChange", "changed": change});
                    if socket.send(Message::Text(data.to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
            device = revoked.recv() => {
                match device {
                    Ok(device) if device == authed.principal.device => break,
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_types() {
        assert_eq!(types_of("*"), None);
        assert_eq!(
            types_of("Email,Nope,Mailbox"),
            Some(vec!["Email".into(), "Mailbox".into()])
        );
        assert_eq!(
            &event("ping", &json!({"interval": 30}))[..],
            b"event: ping\ndata: {\"interval\":30}\n\n"
        );
    }
}
