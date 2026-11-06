//! A plain HTTP server in the test process on `127.0.0.1` that answers every request with a
//! redirect or with one JSON body, and remembers what it was asked.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One request the server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The request target, such as `/.well-known/jmap`.
    pub path: String,
    /// Whether it carried an `Authorization` header.
    pub authorized: bool,
}

/// Answers every request the same way, until dropped.
pub struct Redirect {
    port: u16,
    asked: Arc<Mutex<Vec<Asked>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Redirect {
    /// Starts a server on a free port that answers `307` to `target`.
    pub async fn start(target: &str) -> Self {
        Self::answering(format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ))
        .await
    }

    /// Starts a server on a free port that answers `200` with `body` as JSON.
    pub async fn json(body: &serde_json::Value) -> Self {
        let body = body.to_string();
        Self::answering(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ))
        .await
    }

    async fn answering(answer: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
        let port = listener.local_addr().expect("an address").port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let seen = seen.clone();
                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buffer = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let path = text
                        .lines()
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .unwrap_or_default()
                        .to_owned();
                    let authorized = text
                        .lines()
                        .any(|l| l.to_ascii_lowercase().starts_with("authorization:"));
                    seen.lock()
                        .expect("not poisoned")
                        .push(Asked { path, authorized });
                    let _ = stream.write_all(answer.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { port, asked, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Every request so far, in order.
    pub fn asked(&self) -> Vec<Asked> {
        self.asked.lock().expect("not poisoned").clone()
    }
}

impl Drop for Redirect {
    fn drop(&mut self) {
        self.task.abort();
    }
}
