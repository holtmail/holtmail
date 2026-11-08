//! Real sockets on loopback through the listener.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::UserId;
use holtmail_proxy_protocol::testing::encode;
use holtmail_proxy_protocol::{Addresses, Command, ProxyHeader, SIGNATURE, Ssl};
use holtmail_serve_net::{
    ClientSource, Connection, ConnectionHandler, ConnectionInfo, ConnectionLimiter, LimitExceeded,
    Limits, ListenerOptions, NetError, TlsCertificates, TlsMode, Transport, TrustedProxies, serve,
};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::crypto::ring;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, SupportedProtocolVersion, version};

const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Behaviour {
    Echo,
    StartTlsOnStart,
    SignIn(UserId),
}

struct Handler {
    seen: mpsc::UnboundedSender<ConnectionInfo>,
    outcomes: mpsc::UnboundedSender<Outcome>,
    behaviour: Behaviour,
}

#[derive(Debug)]
enum Outcome {
    ReadFailed(std::io::ErrorKind),
    SignIn(Result<(), LimitExceeded>),
}

#[async_trait]
impl ConnectionHandler for Handler {
    async fn handle(&self, mut connection: Connection) {
        self.seen.send(*connection.info()).unwrap();
        let mut buf = [0u8; 256];
        match self.behaviour {
            Behaviour::Echo => {}
            Behaviour::StartTlsOnStart => {
                assert!(connection.can_start_tls());
                connection.write_all(b"go\n").await.unwrap();
                connection.start_tls().await.unwrap();
                assert!(!connection.can_start_tls());
                assert!(matches!(
                    connection.start_tls().await,
                    Err(NetError::TlsUnavailable)
                ));
                self.seen.send(*connection.info()).unwrap();
            }
            Behaviour::SignIn(user) => {
                let result = connection.authenticated(&user);
                self.outcomes.send(Outcome::SignIn(result)).unwrap();
            }
        }
        loop {
            match connection.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => connection.write_all(&buf[..n]).await.unwrap(),
                Err(error) => {
                    let _ = self.outcomes.send(Outcome::ReadFailed(error.kind()));
                    return;
                }
            }
        }
    }
}

struct Server {
    address: SocketAddr,
    seen: mpsc::UnboundedReceiver<ConnectionInfo>,
    outcomes: mpsc::UnboundedReceiver<Outcome>,
    limiter: ConnectionLimiter,
}

impl Server {
    async fn start(options: ListenerOptions, limits: Limits, behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen) = mpsc::unbounded_channel();
        let (outcomes_tx, outcomes) = mpsc::unbounded_channel();
        let limiter = ConnectionLimiter::new(limits);
        tokio::spawn(serve(
            listener,
            options,
            limiter.clone(),
            Arc::new(Handler {
                seen: seen_tx,
                outcomes: outcomes_tx,
                behaviour,
            }),
        ));
        Self {
            address,
            seen,
            outcomes,
            limiter,
        }
    }

    async fn echo(options: ListenerOptions) -> Self {
        Self::start(options, Limits::default(), Behaviour::Echo).await
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.address).await.unwrap()
    }

    async fn next(&mut self) -> ConnectionInfo {
        timeout(WAIT, self.seen.recv()).await.unwrap().unwrap()
    }

    async fn outcome(&mut self) -> Outcome {
        timeout(WAIT, self.outcomes.recv()).await.unwrap().unwrap()
    }

    fn handler_not_called(&mut self) {
        assert!(self.seen.try_recv().is_err(), "the handler ran");
    }

    async fn wait_for_open(&self, wanted: usize) {
        timeout(WAIT, async {
            while self.limiter.open() != wanted {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

async fn assert_closed<S: AsyncRead + Unpin>(stream: &mut S) {
    let mut buf = [0u8; 16];
    match timeout(WAIT, stream.read(&mut buf)).await.unwrap() {
        Ok(0) | Err(_) => {}
        Ok(n) => panic!("got {n} bytes instead of a close"),
    }
}

async fn assert_echoes<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) {
    stream.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    timeout(WAIT, stream.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf, b"ping");
}

struct Files {
    _dir: TempDir,
    cert: PathBuf,
    key: PathBuf,
}

fn generate() -> (String, String, Vec<u8>) {
    let made = rcgen::generate_simple_self_signed(vec!["mail.example.com".to_owned()]).unwrap();
    (
        made.cert.pem(),
        made.signing_key.serialize_pem(),
        made.cert.der().to_vec(),
    )
}

fn write_pair(cert: &Path, key: &Path, pair: &(String, String, Vec<u8>)) {
    std::fs::write(cert, &pair.0).unwrap();
    std::fs::write(key, &pair.1).unwrap();
}

fn files(pair: &(String, String, Vec<u8>)) -> Files {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("fullchain.pem");
    let key = dir.path().join("privkey.pem");
    write_pair(&cert, &key, pair);
    Files {
        _dir: dir,
        cert,
        key,
    }
}

fn connector(der: &[u8], versions: &[&'static SupportedProtocolVersion]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(der.to_vec())).unwrap();
    let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn tls_client<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    der: &[u8],
    versions: &[&'static SupportedProtocolVersion],
) -> std::io::Result<TlsStream<S>> {
    let name = ServerName::try_from("mail.example.com").unwrap();
    connector(der, versions).connect(name, stream).await
}

fn trusting_loopback(tls: TlsMode) -> ListenerOptions {
    let mut options = ListenerOptions::new(tls);
    options.proxy_protocol = true;
    options.trusted_proxies = TrustedProxies::parse(&["127.0.0.0/8"]).unwrap();
    options
}

fn relayed(ssl: Option<Ssl>) -> Vec<u8> {
    relayed_from(Ipv4Addr::new(203, 0, 113, 7), ssl)
}

fn relayed_from(client: Ipv4Addr, ssl: Option<Ssl>) -> Vec<u8> {
    encode(&ProxyHeader {
        command: Command::Proxy,
        addresses: Some(Addresses {
            source: SocketAddr::from((client, 50000)),
            destination: SocketAddr::from((Ipv4Addr::new(198, 51, 100, 1), 993)),
        }),
        ssl,
    })
}

fn tls_verified() -> Ssl {
    Ssl {
        client_tls: true,
        cert_in_connection: false,
        cert_in_session: false,
        verify: 0,
        version: Some("TLSv1.3".to_owned()),
        cipher: None,
    }
}

#[tokio::test]
async fn a_direct_plain_connection() {
    let mut server = Server::echo(ListenerOptions::new(TlsMode::Off)).await;
    let mut client = server.connect().await;
    assert_echoes(&mut client).await;
    let info = server.next().await;
    assert_eq!(info.client, client.local_addr().unwrap());
    assert_eq!(info.peer, info.client);
    assert_eq!(info.server, server.address);
    assert_eq!(info.source, ClientSource::Direct);
    assert_eq!(info.transport, Transport::Plain);
    assert!(!info.encrypted() && info.loopback() && info.sign_in_allowed());
}

#[tokio::test]
async fn implicit_tls_speaks_1_3_and_1_2_and_nothing_in_the_clear() {
    let pair = generate();
    let files = files(&pair);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let mut server = Server::echo(ListenerOptions::new(TlsMode::Implicit(certificates))).await;

    let mut tls13 = tls_client(server.connect().await, &pair.2, &[&version::TLS13])
        .await
        .unwrap();
    assert_echoes(&mut tls13).await;
    assert_eq!(server.next().await.transport, Transport::Tls);

    let mut tls12 = tls_client(server.connect().await, &pair.2, &[&version::TLS12])
        .await
        .unwrap();
    assert_echoes(&mut tls12).await;
    assert_eq!(server.next().await.transport, Transport::Tls);

    let mut clear = server.connect().await;
    clear.write_all(b"a1 NOOP\r\n\r\n\r\n").await.unwrap();
    let mut alert = Vec::new();
    let _ = timeout(WAIT, clear.read_to_end(&mut alert)).await.unwrap();
    assert_eq!(alert.first(), Some(&0x15), "only a TLS alert comes back");
    server.handler_not_called();
}

#[tokio::test]
async fn a_silent_client_is_dropped_after_the_handshake_timeout() {
    let pair = generate();
    let files = files(&pair);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let mut options = ListenerOptions::new(TlsMode::Implicit(certificates));
    options.tls_handshake_timeout = Duration::from_millis(50);
    let mut server = Server::echo(options).await;
    let mut client = server.connect().await;
    assert_closed(&mut client).await;
    server.wait_for_open(0).await;
    server.handler_not_called();
}

#[tokio::test]
async fn starttls_upgrades_in_place() {
    let pair = generate();
    let files = files(&pair);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let mut server = Server::start(
        ListenerOptions::new(TlsMode::Starttls(certificates)),
        Limits::default(),
        Behaviour::StartTlsOnStart,
    )
    .await;
    let mut client = server.connect().await;
    let mut go = [0u8; 3];
    client.read_exact(&mut go).await.unwrap();
    assert_eq!(&go, b"go\n");
    assert_eq!(server.next().await.transport, Transport::Plain);
    let mut tls = tls_client(client, &pair.2, &[&version::TLS13])
        .await
        .unwrap();
    assert_echoes(&mut tls).await;
    assert_eq!(server.next().await.transport, Transport::Tls);
}

#[tokio::test]
async fn without_a_certificate_there_is_no_starttls() {
    struct NoTls(mpsc::UnboundedSender<Result<(), bool>>);
    #[async_trait]
    impl ConnectionHandler for NoTls {
        async fn handle(&self, mut connection: Connection) {
            assert!(!connection.can_start_tls());
            let result = connection.start_tls().await;
            self.0
                .send(match result {
                    Err(NetError::TlsUnavailable) => Err(true),
                    _ => Err(false),
                })
                .unwrap();
            connection.write_all(b"ok").await.unwrap();
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(serve(
        listener,
        ListenerOptions::new(TlsMode::Off),
        ConnectionLimiter::new(Limits::default()),
        Arc::new(NoTls(tx)),
    ));
    let mut client = TcpStream::connect(address).await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), Err(true));
    let mut ok = [0u8; 2];
    client.read_exact(&mut ok).await.unwrap();
}

#[tokio::test]
async fn a_trusted_proxy_reports_the_client() {
    let mut server = Server::echo(trusting_loopback(TlsMode::Off)).await;
    let mut client = server.connect().await;
    client.write_all(&relayed(None)).await.unwrap();
    assert_echoes(&mut client).await;
    let info = server.next().await;
    assert_eq!(info.client.to_string(), "203.0.113.7:50000");
    assert_eq!(info.server.to_string(), "198.51.100.1:993");
    assert_eq!(info.peer.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(info.source, ClientSource::Proxy);
    assert_eq!(info.transport, Transport::Plain);
    assert!(
        !info.loopback() && !info.sign_in_allowed(),
        "the proxy's own loopback address must not count"
    );
}

#[tokio::test]
async fn the_header_and_the_first_bytes_may_share_a_segment() {
    let server = Server::echo(trusting_loopback(TlsMode::Off)).await;
    let mut client = server.connect().await;
    let mut bytes = relayed(None);
    bytes.extend_from_slice(b"ping");
    client.write_all(&bytes).await.unwrap();
    let mut buf = [0u8; 4];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");
}

#[tokio::test]
async fn the_ssl_tlv_counts_as_tls_only_when_verified() {
    let pair = generate();
    let files = files(&pair);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let mut server = Server::echo(trusting_loopback(TlsMode::Off)).await;

    let mut client = server.connect().await;
    client
        .write_all(&relayed(Some(tls_verified())))
        .await
        .unwrap();
    let info = server.next().await;
    assert_eq!(info.transport, Transport::ProxyTls);
    assert!(info.encrypted() && info.sign_in_allowed());

    let failed = Ssl {
        verify: 1,
        cert_in_session: true,
        ..tls_verified()
    };
    let mut client2 = server.connect().await;
    client2.write_all(&relayed(Some(failed))).await.unwrap();
    assert_eq!(server.next().await.transport, Transport::Plain);

    let not_tls = Ssl {
        client_tls: false,
        ..tls_verified()
    };
    let mut client3 = server.connect().await;
    client3.write_all(&relayed(Some(not_tls))).await.unwrap();
    assert_eq!(server.next().await.transport, Transport::Plain);

    struct Probe(mpsc::UnboundedSender<bool>);
    #[async_trait]
    impl ConnectionHandler for Probe {
        async fn handle(&self, connection: Connection) {
            self.0.send(connection.can_start_tls()).unwrap();
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(serve(
        listener,
        trusting_loopback(TlsMode::Starttls(certificates)),
        ConnectionLimiter::new(Limits::default()),
        Arc::new(Probe(tx)),
    ));
    let mut encrypted = TcpStream::connect(address).await.unwrap();
    encrypted
        .write_all(&relayed(Some(tls_verified())))
        .await
        .unwrap();
    assert!(!rx.recv().await.unwrap(), "already encrypted at the proxy");
    let mut plain = TcpStream::connect(address).await.unwrap();
    plain.write_all(&relayed(None)).await.unwrap();
    assert!(rx.recv().await.unwrap());
}

#[tokio::test]
async fn a_proxy_may_pass_tls_through() {
    let pair = generate();
    let files = files(&pair);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let mut server = Server::echo(trusting_loopback(TlsMode::Implicit(certificates))).await;
    let mut client = server.connect().await;
    client.write_all(&relayed(None)).await.unwrap();
    let mut tls = tls_client(client, &pair.2, &[&version::TLS13])
        .await
        .unwrap();
    assert_echoes(&mut tls).await;
    let info = server.next().await;
    assert_eq!(info.transport, Transport::Tls);
    assert_eq!(info.client.to_string(), "203.0.113.7:50000");
}

#[tokio::test]
async fn a_local_header_keeps_the_socket_address_without_trusting_it() {
    let mut server = Server::echo(trusting_loopback(TlsMode::Off)).await;
    let mut client = server.connect().await;
    let header = encode(&ProxyHeader {
        command: Command::Local,
        addresses: None,
        ssl: None,
    });
    client.write_all(&header).await.unwrap();
    let info = server.next().await;
    assert_eq!(info.source, ClientSource::ProxyLocal);
    assert_eq!(info.client, client.local_addr().unwrap());
    assert!(!info.loopback() && !info.sign_in_allowed());
}

#[tokio::test]
async fn a_relayed_connection_without_an_ip_client_is_closed() {
    let mut server = Server::echo(trusting_loopback(TlsMode::Off)).await;
    let mut client = server.connect().await;
    let header = encode(&ProxyHeader {
        command: Command::Proxy,
        addresses: None,
        ssl: None,
    });
    client.write_all(&header).await.unwrap();
    assert_closed(&mut client).await;
    server.wait_for_open(0).await;
    server.handler_not_called();
}

#[tokio::test]
async fn a_trusted_peer_must_send_a_valid_header() {
    let mut server = Server::echo(trusting_loopback(TlsMode::Off)).await;
    let mut garbage = server.connect().await;
    garbage
        .write_all(b"a1 LOGIN nobody nothing\r\n")
        .await
        .unwrap();
    assert_closed(&mut garbage).await;
    server.wait_for_open(0).await;
    server.handler_not_called();

    let mut short = server.connect().await;
    short.write_all(&SIGNATURE).await.unwrap();
    short.shutdown().await.unwrap();
    assert_closed(&mut short).await;
    server.wait_for_open(0).await;
    server.handler_not_called();
}

#[tokio::test]
async fn a_trusted_peer_that_stays_silent_times_out() {
    let mut options = trusting_loopback(TlsMode::Off);
    options.proxy_header_timeout = Duration::from_millis(50);
    let mut server = Server::echo(options).await;
    let mut client = server.connect().await;
    assert_closed(&mut client).await;
    server.wait_for_open(0).await;
    server.handler_not_called();
}

#[tokio::test]
async fn a_header_from_an_untrusted_peer_ends_the_connection() {
    for proxy_protocol in [true, false] {
        let mut options = ListenerOptions::new(TlsMode::Off);
        options.proxy_protocol = proxy_protocol;
        options.trusted_proxies = TrustedProxies::parse(&["198.51.100.0/24"]).unwrap();
        let mut server = Server::echo(options).await;
        let mut client = server.connect().await;
        client.write_all(&relayed(None)).await.unwrap();
        let info = server.next().await;
        assert_eq!(info.source, ClientSource::Direct);
        assert_eq!(info.client, client.local_addr().unwrap());
        assert!(matches!(
            server.outcome().await,
            Outcome::ReadFailed(std::io::ErrorKind::InvalidData)
        ));
        assert_closed(&mut client).await;
        server.wait_for_open(0).await;
    }
}

#[tokio::test]
async fn an_untrusted_peer_needs_no_first_bytes() {
    let mut options = ListenerOptions::new(TlsMode::Off);
    options.proxy_protocol = true;
    options.proxy_header_timeout = Duration::from_millis(1);
    let mut server = Server::echo(options).await;
    let _client = server.connect().await;
    assert_eq!(server.next().await.source, ClientSource::Direct);
}

#[tokio::test]
async fn trusted_proxies_are_needed_for_a_header_even_with_proxy_protocol_off() {
    let mut options = trusting_loopback(TlsMode::Off);
    options.proxy_protocol = false;
    let mut server = Server::echo(options).await;
    let mut client = server.connect().await;
    client.write_all(&relayed(None)).await.unwrap();
    server.next().await;
    assert!(matches!(server.outcome().await, Outcome::ReadFailed(_)));
}

#[tokio::test]
async fn the_total_limit_closes_extra_connections() {
    let limits = Limits {
        total: 2,
        ..Limits::default()
    };
    let mut server =
        Server::start(ListenerOptions::new(TlsMode::Off), limits, Behaviour::Echo).await;
    let mut first = server.connect().await;
    let mut second = server.connect().await;
    assert_echoes(&mut first).await;
    assert_echoes(&mut second).await;
    server.next().await;
    server.next().await;
    let mut third = server.connect().await;
    assert_closed(&mut third).await;
    server.handler_not_called();
    drop(first);
    server.wait_for_open(1).await;
    let mut fourth = server.connect().await;
    assert_echoes(&mut fourth).await;
}

#[tokio::test]
async fn the_address_limit_uses_the_reported_client() {
    let limits = Limits {
        per_address: 1,
        ..Limits::default()
    };
    let mut server = Server::start(trusting_loopback(TlsMode::Off), limits, Behaviour::Echo).await;
    let mut first = server.connect().await;
    first.write_all(&relayed(None)).await.unwrap();
    server.next().await;
    let mut same = server.connect().await;
    same.write_all(&relayed(None)).await.unwrap();
    assert_closed(&mut same).await;
    server.handler_not_called();
    let mut other = server.connect().await;
    other
        .write_all(&relayed_from(Ipv4Addr::new(203, 0, 113, 8), None))
        .await
        .unwrap();
    assert_echoes(&mut other).await;
    assert_eq!(server.next().await.client.ip().to_string(), "203.0.113.8");
}

#[tokio::test]
async fn the_user_limit_is_reported_to_the_front_end() {
    let limits = Limits {
        per_user: 1,
        ..Limits::default()
    };
    let user = UserId::new();
    let mut server = Server::start(
        ListenerOptions::new(TlsMode::Off),
        limits,
        Behaviour::SignIn(user),
    )
    .await;
    let first = server.connect().await;
    assert!(matches!(server.outcome().await, Outcome::SignIn(Ok(()))));
    let _second = server.connect().await;
    assert!(matches!(
        server.outcome().await,
        Outcome::SignIn(Err(LimitExceeded::PerUser))
    ));
    drop(first);
    server.wait_for_open(1).await;
    let _third = server.connect().await;
    assert!(matches!(server.outcome().await, Outcome::SignIn(Ok(()))));
}

#[tokio::test]
async fn a_handler_panic_frees_the_place() {
    struct Panics;
    #[async_trait]
    impl ConnectionHandler for Panics {
        async fn handle(&self, _connection: Connection) {
            panic!("on purpose");
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let limiter = ConnectionLimiter::new(Limits::default());
    tokio::spawn(serve(
        listener,
        ListenerOptions::new(TlsMode::Off),
        limiter.clone(),
        Arc::new(Panics),
    ));
    let mut client = TcpStream::connect(address).await.unwrap();
    assert_closed(&mut client).await;
    timeout(WAIT, async {
        while limiter.open() != 0 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn presented<S>(tls: &TlsStream<S>) -> Vec<u8> {
    tls.get_ref().1.peer_certificates().unwrap()[0].to_vec()
}

#[tokio::test]
async fn reload_replaces_the_certificate_for_new_connections_only() {
    let old = generate();
    let new = generate();
    let files = files(&old);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    assert!(!certificates.reload().await.unwrap(), "nothing changed");
    let server = Server::echo(ListenerOptions::new(TlsMode::Implicit(
        certificates.clone(),
    )))
    .await;

    let mut before = tls_client(server.connect().await, &old.2, &[&version::TLS13])
        .await
        .unwrap();
    assert_echoes(&mut before).await;

    write_pair(&files.cert, &files.key, &new);
    assert!(certificates.reload().await.unwrap());
    assert!(!certificates.reload().await.unwrap());

    let after = tls_client(server.connect().await, &new.2, &[&version::TLS13])
        .await
        .unwrap();
    assert_eq!(presented(&after).await, new.2);
    assert!(
        tls_client(server.connect().await, &old.2, &[&version::TLS13])
            .await
            .is_err(),
        "the old certificate is gone for new connections"
    );
    assert_echoes(&mut before).await;
    assert_eq!(presented(&before).await, old.2);
}

#[tokio::test]
async fn a_bad_reload_keeps_the_certificate_in_use() {
    let first = generate();
    let second = generate();
    let files = files(&first);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let server = Server::echo(ListenerOptions::new(TlsMode::Implicit(
        certificates.clone(),
    )))
    .await;

    std::fs::write(&files.cert, &second.0).unwrap();
    assert!(matches!(
        certificates.reload().await,
        Err(NetError::Certificate(_))
    ));
    std::fs::write(&files.cert, "not a certificate").unwrap();
    assert!(certificates.reload().await.is_err());
    std::fs::remove_file(&files.key).unwrap();
    assert!(certificates.reload().await.is_err());

    let tls = tls_client(server.connect().await, &first.2, &[&version::TLS13])
        .await
        .unwrap();
    assert_eq!(presented(&tls).await, first.2);

    write_pair(&files.cert, &files.key, &second);
    assert!(certificates.reload().await.unwrap());
}

#[tokio::test]
async fn the_watcher_notices_new_files() {
    let old = generate();
    let new = generate();
    let files = files(&old);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let watcher = certificates.watch(Duration::from_millis(10));
    let server = Server::echo(ListenerOptions::new(TlsMode::Implicit(
        certificates.clone(),
    )))
    .await;

    write_pair(&files.cert, &files.key, &new);
    timeout(WAIT, async {
        loop {
            if tls_client(server.connect().await, &new.2, &[&version::TLS13])
                .await
                .is_ok()
            {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    watcher.abort();
}

#[tokio::test]
async fn the_watcher_ends_with_the_last_clone() {
    let pair = generate();
    let files = files(&pair);
    let certificates = TlsCertificates::load(&files.cert, &files.key)
        .await
        .unwrap();
    let watcher = certificates.watch(Duration::from_millis(5));
    drop(certificates);
    timeout(WAIT, watcher).await.unwrap().unwrap();
}

#[tokio::test]
async fn loading_refuses_bad_files() {
    let pair = generate();
    let other = generate();
    let files = files(&pair);
    assert!(matches!(
        TlsCertificates::load(&files.cert, files.key.with_extension("missing")).await,
        Err(NetError::Certificate(_))
    ));
    std::fs::write(&files.key, &other.1).unwrap();
    assert!(
        TlsCertificates::load(&files.cert, &files.key)
            .await
            .is_err()
    );
    std::fs::write(&files.cert, "").unwrap();
    assert!(
        TlsCertificates::load(&files.cert, &files.key)
            .await
            .is_err()
    );
}
