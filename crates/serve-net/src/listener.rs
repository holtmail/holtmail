use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_proxy_protocol::{Command, Parsed, parse};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use crate::connection::{Guarded, Stream};
use crate::limits::{ConnectionSlot, LimitPlace};
use crate::{
    ClientSource, Connection, ConnectionInfo, ConnectionLimiter, TlsCertificates, Transport,
    TrustedProxies,
};

/// What a listener does about TLS.
#[derive(Debug, Clone)]
pub enum TlsMode {
    /// Never encrypts. For tests and for traffic a proxy has already decrypted.
    Off,
    /// Every connection starts with a TLS handshake.
    Implicit(TlsCertificates),
    /// Connections start in the clear and may call [`Connection::start_tls`].
    Starttls(TlsCertificates),
}

/// How a listener treats new connections.
#[derive(Debug, Clone)]
pub struct ListenerOptions {
    /// TLS on this listener.
    pub tls: TlsMode,
    /// Expect a PROXY v2 header from peers in `trusted_proxies`.
    pub proxy_protocol: bool,
    /// The peers allowed to send a PROXY header.
    pub trusted_proxies: TrustedProxies,
    /// How long a trusted proxy may take to send its header. Default 5 s.
    pub proxy_header_timeout: Duration,
    /// How long a TLS handshake may take. Default 10 s.
    pub tls_handshake_timeout: Duration,
}

impl ListenerOptions {
    /// Options with `tls`, no PROXY protocol, and the default waits.
    pub fn new(tls: TlsMode) -> Self {
        Self {
            tls,
            proxy_protocol: false,
            trusted_proxies: TrustedProxies::default(),
            proxy_header_timeout: Duration::from_secs(5),
            tls_handshake_timeout: Duration::from_secs(10),
        }
    }
}

/// What a protocol front end provides.
#[async_trait]
pub trait ConnectionHandler: Send + Sync {
    /// Runs one connection to its end. Called once per connection, concurrently with others;
    /// the connection's limit places are freed when it is dropped, even by a panic.
    async fn handle(&self, connection: Connection);
}

enum Refused {
    Limit,
    Proxy,
    Handshake,
    Io,
}

impl Refused {
    fn reason(&self) -> &'static str {
        match self {
            Self::Limit => "address limit",
            Self::Proxy => "PROXY header",
            Self::Handshake => "TLS handshake",
            Self::Io => "socket error",
        }
    }
}

/// Accepts connections on `listener` until the runtime stops, and hands each one that passes
/// its checks to `handler` on its own task.
///
/// Checks, in order:
/// 1. [`Limits::total`](crate::Limits::total); over it the socket is closed at once.
/// 2. With `proxy_protocol`, from a trusted peer: a PROXY v2 header is required. A connection
///    that does not send a valid one in time is closed, and so is a relayed one that names no
///    IP client (the limits and the loopback rule would have nothing to go by).
/// 3. [`Limits::per_address`](crate::Limits::per_address) for the client, as reported by the
///    proxy when there is one.
/// 4. On an implicit-TLS listener, the handshake, within its timeout.
///
/// Refused connections get no reply, since this layer speaks no protocol. A peer that is not
/// trusted is served directly; if its bytes start with a PROXY header, the read that completes
/// the signature fails, which ends the connection. Logs carry reasons and counts, never
/// addresses or content.
pub async fn serve(
    listener: TcpListener,
    options: ListenerOptions,
    limiter: ConnectionLimiter,
    handler: Arc<dyn ConnectionHandler>,
) {
    let options = Arc::new(options);
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(slot) = limiter.acquire() else {
            tracing::debug!(open = limiter.open(), "connection refused: total limit");
            continue;
        };
        let options = options.clone();
        let handler = handler.clone();
        tokio::spawn(async move {
            match admit(stream, peer, slot, &options).await {
                Ok(connection) => handler.handle(connection).await,
                Err(refused) => {
                    tracing::debug!(reason = refused.reason(), "connection refused");
                }
            }
        });
    }
}

fn canonical(address: SocketAddr) -> SocketAddr {
    SocketAddr::new(address.ip().to_canonical(), address.port())
}

struct Origin {
    client: SocketAddr,
    server: SocketAddr,
    source: ClientSource,
    proxy_tls: bool,
}

async fn admit(
    mut stream: TcpStream,
    peer: SocketAddr,
    mut slot: ConnectionSlot,
    options: &ListenerOptions,
) -> Result<Connection, Refused> {
    let _ = stream.set_nodelay(true);
    let peer = canonical(peer);
    let server = canonical(stream.local_addr().map_err(|_| Refused::Io)?);
    let origin = if options.proxy_protocol && options.trusted_proxies.contains(peer.ip()) {
        tokio::time::timeout(
            options.proxy_header_timeout,
            read_header(&mut stream, peer, server),
        )
        .await
        .map_err(|_| Refused::Proxy)??
    } else {
        Origin {
            client: peer,
            server,
            source: ClientSource::Direct,
            proxy_tls: false,
        }
    };
    slot.bind_address(origin.client.ip())
        .map_err(|_| Refused::Limit)?;

    let plain = Guarded::new(stream);
    let reported = if origin.proxy_tls {
        Transport::ProxyTls
    } else {
        Transport::Plain
    };
    let (stream, transport, starttls) = match &options.tls {
        TlsMode::Off => (Stream::Plain(plain), reported, None),
        TlsMode::Implicit(certificates) => {
            let accept = certificates.acceptor().accept(plain);
            let tls = tokio::time::timeout(options.tls_handshake_timeout, accept)
                .await
                .map_err(|_| Refused::Handshake)?
                .map_err(|_| Refused::Handshake)?;
            (Stream::Tls(Box::new(tls)), Transport::Tls, None)
        }
        TlsMode::Starttls(certificates) => {
            let acceptor = (!origin.proxy_tls).then(|| certificates.acceptor());
            (Stream::Plain(plain), reported, acceptor)
        }
    };
    Ok(Connection {
        stream,
        info: ConnectionInfo {
            client: origin.client,
            server: origin.server,
            peer,
            source: origin.source,
            transport,
        },
        slot: LimitPlace(Arc::new(Mutex::new(slot))),
        starttls,
        handshake_timeout: options.tls_handshake_timeout,
    })
}

async fn read_header(
    stream: &mut TcpStream,
    peer: SocketAddr,
    server: SocketAddr,
) -> Result<Origin, Refused> {
    let mut bytes = Vec::new();
    let mut wanted = 16;
    let header = loop {
        let start = bytes.len();
        bytes.resize(wanted, 0);
        read_exact(stream, &mut bytes[start..]).await?;
        match parse(&bytes).map_err(|_| Refused::Proxy)? {
            Parsed::Incomplete { needed } => wanted = needed,
            Parsed::Complete { header, .. } => break header,
        }
    };
    if header.command == Command::Local {
        return Ok(Origin {
            client: peer,
            server,
            source: ClientSource::ProxyLocal,
            proxy_tls: false,
        });
    }
    let addresses = header.addresses.ok_or(Refused::Proxy)?;
    Ok(Origin {
        client: canonical(addresses.source),
        server: canonical(addresses.destination),
        source: ClientSource::Proxy,
        proxy_tls: header.ssl.is_some_and(|ssl| ssl.verified_tls()),
    })
}

async fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> Result<(), Refused> {
    stream.read_exact(buf).await.map(drop).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Refused::Proxy
        } else {
            Refused::Io
        }
    })
}
