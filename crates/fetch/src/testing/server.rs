use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use holtmail_core::tls::testing::TestCa;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(super) struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

pub(super) struct Server {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Arc<Request>>>>,
}

impl Server {
    pub async fn start(ca: &TestCa, slow: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let port = address.port();
        let acceptor = ca.acceptor();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (count, log) = (connections.clone(), requests.clone());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let (acceptor, log) = (acceptor.clone(), log.clone());
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Some(request) = read_request(&mut stream).await else {
                        return;
                    };
                    let request = Arc::new(request);
                    log.lock().expect("log").push(request.clone());
                    let reply = answer(&request, port);
                    if request.path == "/slow" {
                        tokio::time::sleep(slow).await;
                    }
                    let length = match reply.length {
                        Length::Exact => format!("Content-Length: {}\r\n", reply.body.len()),
                        Length::Missing => String::new(),
                        Length::Stall => format!("Content-Length: {}\r\n", reply.body.len() * 2),
                    };
                    let head = format!(
                        "HTTP/1.1 {} Test\r\nContent-Type: text/plain\r\n{length}{}Connection: close\r\n\r\n",
                        reply.status, reply.extra
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&reply.body).await;
                    let _ = stream.flush().await;
                    if matches!(reply.length, Length::Stall) {
                        tokio::time::sleep(slow).await;
                    }
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            address,
            connections,
            requests,
        }
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn requests(&self, path: &str) -> Vec<Arc<Request>> {
        self.requests
            .lock()
            .expect("log")
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }

    pub fn requests_to(&self, host: &str) -> Vec<Arc<Request>> {
        let prefix = format!("{host}:");
        self.requests
            .lock()
            .expect("log")
            .iter()
            .filter(|r| r.header("host").is_some_and(|h| h.starts_with(&prefix)))
            .cloned()
            .collect()
    }

    pub fn all_requests(&self) -> Vec<Arc<Request>> {
        self.requests.lock().expect("log").clone()
    }
}

pub(super) const DATA_ETAG: &str = "\"v1-5f0c9e2a\"";
pub(super) const DATA_BODY: &[u8] = b"#holtmail-categories 1 test 1\n";

enum Length {
    Exact,
    Missing,
    Stall,
}

struct Reply {
    status: u16,
    extra: String,
    body: Vec<u8>,
    length: Length,
}

impl Reply {
    fn new(status: u16, extra: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            extra: extra.into(),
            body: body.into(),
            length: Length::Exact,
        }
    }
}

fn answer(request: &Request, port: u16) -> Reply {
    let path = request.path.as_str();
    if let Some(rest) = path.strip_prefix("/data/") {
        return data_answer(request, rest, port);
    }
    match path {
        "/redirect" => Reply::new(
            302,
            "Location: https://news.example.com/followed\r\n",
            Vec::new(),
        ),
        "/large" => Reply::new(200, "", vec![b'x'; 1024 * 1024]),
        _ => {
            let status = path
                .strip_prefix("/status/")
                .and_then(|s| s.parse().ok())
                .unwrap_or(200);
            Reply::new(status, "", b"ok".to_vec())
        }
    }
}

fn data_answer(request: &Request, rest: &str, port: u16) -> Reply {
    let mut parts = rest.splitn(4, '/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(scheme @ ("redirect" | "insecure")), Some(code), Some(host), Some(target)) => {
            let scheme = if scheme == "redirect" {
                "https"
            } else {
                "http"
            };
            Reply::new(
                code.parse().unwrap_or(302),
                format!(
                    "Location: {scheme}://{host}:{port}/data/{target}\r\nSet-Cookie: session=c2VjcmV0; Path=/\r\n"
                ),
                Vec::new(),
            )
        }
        (Some("file"), None, ..) => {
            let tag = format!("ETag: {DATA_ETAG}\r\n");
            if request.header("if-none-match") == Some(DATA_ETAG) {
                Reply::new(304, tag, Vec::new())
            } else {
                Reply::new(200, tag, DATA_BODY)
            }
        }
        (Some("untagged"), None, ..) => Reply::new(200, "", DATA_BODY),
        (Some("odd-tag"), None, ..) => Reply::new(200, "ETag: \"caf\u{e9}\"\r\n", DATA_BODY),
        (Some("unconditional-304"), None, ..) => Reply::new(304, "", Vec::new()),
        (Some("large"), None, ..) => Reply::new(200, "", vec![b'x'; 1024 * 1024]),
        (Some("large-unsized"), None, ..) => Reply {
            length: Length::Missing,
            ..Reply::new(200, "", vec![b'x'; 1024 * 1024])
        },
        (Some("stalled-body"), None, ..) => Reply {
            length: Length::Stall,
            ..Reply::new(200, "", DATA_BODY)
        },
        (Some("status"), Some(code), None, _) => {
            Reply::new(code.parse().unwrap_or(500), "", b"no".to_vec())
        }
        _ => Reply::new(404, "", b"no".to_vec()),
    }
}

async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Option<Request> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let body_start = loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        let n = stream.read(&mut buf).await.ok().filter(|n| *n > 0)?;
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..body_start]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let find = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let mut rest = data.split_off(body_start);
    let chunked = find("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    let body = if chunked {
        loop {
            if let Some(body) = dechunk(&rest) {
                break body;
            }
            let n = stream.read(&mut buf).await.ok().filter(|n| *n > 0)?;
            rest.extend_from_slice(&buf[..n]);
        }
    } else {
        let length: usize = find("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while rest.len() < length {
            let n = stream.read(&mut buf).await.ok().filter(|n| *n > 0)?;
            rest.extend_from_slice(&buf[..n]);
        }
        rest.truncate(length);
        rest
    };
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        data = &data[line_end + 2..];
        if size == 0 {
            return data.windows(2).any(|w| w == b"\r\n").then_some(body);
        }
        if data.len() < size + 2 {
            return None;
        }
        body.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}
