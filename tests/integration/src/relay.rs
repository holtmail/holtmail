//! A TCP relay in the test process between Holtmail and a server, which can pass traffic, stall
//! it or cut connections at a chosen point. TLS passes through untouched, so a client that names
//! `localhost` still checks the server's certificate.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// What the relay does with the server's answers on each connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Pass everything both ways.
    Pass,
    /// Forward the first `n` bytes the server sends on a connection, then hold the connection
    /// open without forwarding anything more, like a server that stopped answering.
    StallAfter(u64),
    /// Forward the first `n` bytes the server sends on a connection, then close it, like a
    /// server that died.
    CutAfter(u64),
}

struct Shared {
    upstream: String,
    mode: Mutex<Mode>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    accepted: Mutex<u64>,
}

/// A running relay; stops with every connection when dropped.
pub struct Relay {
    port: u16,
    shared: Arc<Shared>,
    listener: JoinHandle<()>,
}

impl Relay {
    /// Listens on a free port of `127.0.0.1` and relays every connection to `upstream`
    /// (`host:port`) in [`Mode::Pass`].
    pub async fn start(upstream: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("relay binds");
        let port = listener.local_addr().expect("relay address").port();
        let shared = Arc::new(Shared {
            upstream: upstream.to_owned(),
            mode: Mutex::new(Mode::Pass),
            tasks: Mutex::new(Vec::new()),
            accepted: Mutex::new(0),
        });
        let accepting = shared.clone();
        let listener = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                *accepting.accepted.lock().expect("relay lock") += 1;
                let mode = *accepting.mode.lock().expect("relay lock");
                let upstream = accepting.upstream.clone();
                let task = tokio::spawn(relay(client, upstream, mode));
                accepting.tasks.lock().expect("relay lock").push(task);
            }
        });
        Self {
            port,
            shared,
            listener,
        }
    }

    /// The port to give Holtmail instead of the server's.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Sets the mode of connections accepted from now on; open ones keep theirs.
    pub fn set(&self, mode: Mode) {
        *self.shared.mode.lock().expect("relay lock") = mode;
    }

    /// Closes every open connection at once.
    pub fn cut_all(&self) {
        for task in self.shared.tasks.lock().expect("relay lock").drain(..) {
            task.abort();
        }
    }

    /// How many connections the relay has accepted so far.
    pub fn accepted(&self) -> u64 {
        *self.shared.accepted.lock().expect("relay lock")
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.listener.abort();
        self.cut_all();
    }
}

async fn relay(client: TcpStream, upstream: String, mode: Mode) {
    let Ok(server) = TcpStream::connect(&upstream).await else {
        return;
    };
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let _up = AbortOnDrop(tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
    }));
    let limit = match mode {
        Mode::Pass => u64::MAX,
        Mode::StallAfter(n) | Mode::CutAfter(n) => n,
    };
    let mut sent = 0u64;
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match server_read.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let allowed = usize::try_from(limit.saturating_sub(sent)).unwrap_or(usize::MAX);
        let take = read.min(allowed);
        if take > 0 && client_write.write_all(&buffer[..take]).await.is_err() {
            break;
        }
        sent += take as u64;
        if take < read || sent >= limit {
            match mode {
                Mode::StallAfter(_) => {
                    std::future::pending::<()>().await;
                }
                Mode::CutAfter(_) | Mode::Pass => break,
            }
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
