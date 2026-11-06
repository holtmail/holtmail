use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use holtmail_webhook::testing::{Received, Receiver, conformance};

use super::*;

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Received>>>);

struct Local {
    base: String,
    closed: String,
    log: Log,
}

async fn record(
    State(log): State<Log>,
    Path((status, path)): Path<(u16, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    log.0.lock().unwrap().push(Received {
        path: format!("/{path}"),
        headers: headers
            .iter()
            .map(|(n, v)| {
                (
                    n.as_str().to_owned(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
        body,
    });
    StatusCode::from_u16(status).unwrap()
}

async fn moved(Path(target): Path<String>) -> impl IntoResponse {
    (
        StatusCode::TEMPORARY_REDIRECT,
        [(header::LOCATION, format!("/200/{target}"))],
    )
}

async fn local() -> Local {
    let log = Log::default();
    let app = Router::new()
        .route("/{status}/{path}", post(record))
        .route("/moved/{target}", post(moved))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let unused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed = format!("http://{}/hook", unused.local_addr().unwrap());
    drop(unused);
    Local { base, closed, log }
}

#[async_trait]
impl Receiver for Local {
    fn url(&self, path: &str, status: u16) -> String {
        format!("{}/{status}/{path}", self.base)
    }

    fn redirect(&self, _path: &str, target: &str) -> String {
        format!("{}/moved/{target}", self.base)
    }

    fn closed(&self) -> String {
        self.closed.clone()
    }

    async fn received(&self) -> Vec<Received> {
        self.log.0.lock().unwrap().clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_webhook_contract() {
    let receiver = local().await;
    let sender = HttpWebhookSender::new(Duration::from_secs(5)).unwrap();
    conformance(&sender, &receiver).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_on_a_receiver_that_never_answers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            open.push(socket);
        }
    });
    let sender = HttpWebhookSender::new(Duration::from_millis(300)).unwrap();
    let answer = sender
        .send(&WebhookRequest {
            url,
            headers: Vec::new(),
            body: "{}".into(),
        })
        .await;
    assert!(matches!(answer, Err(WebhookError::TimedOut)), "{answer:?}");
}
