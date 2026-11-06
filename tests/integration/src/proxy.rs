//! A plain HTTP reverse proxy in the test process on `127.0.0.1` in front of an https server of
//! the stack, which can pass requests on, answer every request with one status, refuse
//! `sync-collection` reports as a server without RFC 6578 would, or change what one collection
//! sends.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the proxy does with each request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Pass every request on.
    Pass,
    /// Answer `REPORT` requests whose body is a `sync-collection` with `501 Not Implemented`;
    /// pass the rest on.
    NoSync,
    /// Answer every request with this status and an empty body.
    Status(u16),
    /// Pass every request on, and in the answers to requests whose path contains `path`
    /// replace every `from` with `to`, as a server that sends broken data would.
    Rewrite {
        /// Part of the request path.
        path: String,
        /// Text to replace in the answer.
        from: String,
        /// Its replacement.
        to: String,
    },
}

struct Shared {
    upstream: String,
    mode: Mutex<Mode>,
    seen: Mutex<Vec<(String, bool)>>,
    client: reqwest::Client,
}

/// A running proxy; stops when dropped.
pub struct Proxy {
    port: u16,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    /// Listens on a free port of `127.0.0.1` and passes requests to `upstream` (such as
    /// `https://localhost:45443`), trusting the stack's CA and following no redirects.
    pub async fn start(upstream: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
        let port = listener.local_addr().expect("an address").port();
        let pem = std::fs::read(crate::ca_file()).expect("the stack's CA");
        let client = reqwest::Client::builder()
            .tls_certs_merge(reqwest::Certificate::from_pem_bundle(&pem).expect("a PEM CA"))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let shared = Arc::new(Shared {
            upstream: upstream.trim_end_matches('/').to_owned(),
            mode: Mutex::new(Mode::Pass),
            seen: Mutex::new(Vec::new()),
            client,
        });
        let serving = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, serving.clone()));
            }
        });
        Self { port, shared, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Sets the mode for requests from now on.
    pub fn set(&self, mode: Mode) {
        *self.shared.mode.lock().expect("not poisoned") = mode;
    }

    /// Every request so far as `METHOD path`, with whether the proxy passed it on.
    pub fn seen(&self) -> Vec<(String, bool)> {
        self.shared.seen.lock().expect("not poisoned").clone()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, shared: Arc<Shared>) {
    let mut data = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let head_end = loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => data.extend_from_slice(&buffer[..n]),
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_owned();
    let path = first.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut length = 0usize;
    for line in lines.filter(|l| !l.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            let (name, value) = (name.trim().to_owned(), value.trim().to_owned());
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    let mut body = data[head_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => body.extend_from_slice(&buffer[..n]),
        }
    }
    let mode = shared.mode.lock().expect("not poisoned").clone();
    let refused = match &mode {
        Mode::Pass | Mode::Rewrite { .. } => None,
        Mode::Status(status) => Some(*status),
        Mode::NoSync => (method == "REPORT"
            && String::from_utf8_lossy(&body).contains("sync-collection"))
        .then_some(501),
    };
    shared
        .seen
        .lock()
        .expect("not poisoned")
        .push((format!("{method} {path}"), refused.is_none()));
    let answer = match refused {
        Some(status) => format!(
            "HTTP/1.1 {status} Refused by the test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
        None => {
            let rewrite = match &mode {
                Mode::Rewrite { path: part, from, to } if path.contains(part.as_str()) => {
                    Some((from.as_str(), to.as_str()))
                }
                _ => None,
            };
            forward(&shared, &method, &path, &headers, body, rewrite).await
        }
    };
    let _ = stream.write_all(&answer).await;
    let _ = stream.shutdown().await;
}

async fn forward(
    shared: &Shared,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
    rewrite: Option<(&str, &str)>,
) -> Vec<u8> {
    let method = reqwest::Method::from_bytes(method.as_bytes()).expect("a method");
    let mut request = shared
        .client
        .request(method, format!("{}{path}", shared.upstream))
        .body(body);
    for (name, value) in headers {
        if !["host", "connection", "content-length"]
            .iter()
            .any(|h| name.eq_ignore_ascii_case(h))
        {
            request = request.header(name, value);
        }
    }
    let Ok(response) = request.send().await else {
        return b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
    };
    let status = response.status();
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    for (name, value) in response.headers() {
        if ["connection", "content-length", "transfer-encoding"]
            .iter()
            .any(|h| name.as_str().eq_ignore_ascii_case(h))
        {
            continue;
        }
        out.push_str(&format!(
            "{}: {}\r\n",
            name,
            value.to_str().unwrap_or_default()
        ));
    }
    let mut body = response.bytes().await.unwrap_or_default().to_vec();
    if let Some((from, to)) = rewrite {
        body = String::from_utf8_lossy(&body)
            .replace(from, to)
            .into_bytes();
    }
    out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&body);
    bytes
}
