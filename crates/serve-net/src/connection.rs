use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use holtmail_core::UserId;
use holtmail_proxy_protocol::SIGNATURE;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::limits::LimitPlace;
use crate::{LimitExceeded, NetError};

/// How a connection reached this listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSource {
    /// Straight from the client.
    Direct,
    /// Relayed by a trusted proxy for the client it reported.
    Proxy,
    /// A trusted proxy's own connection (a `LOCAL` header), such as a health check. There is no
    /// client behind it.
    ProxyLocal,
}

/// How the client's bytes are protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Not encrypted.
    Plain,
    /// Holtmail terminated TLS.
    Tls,
    /// A trusted proxy terminated TLS and reported a verified connection in its SSL TLV.
    ProxyTls,
}

/// What is known about a connection. Addresses are canonical: an IPv4-mapped IPv6 address
/// appears as IPv4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// The client: the address a trusted proxy reported, otherwise the socket's peer.
    pub client: SocketAddr,
    /// The address the client connected to, by the same rule.
    pub server: SocketAddr,
    /// The socket's peer, which is the proxy on a relayed connection.
    pub peer: SocketAddr,
    /// How the connection got here.
    pub source: ClientSource,
    /// The protection in force now; it changes after [`Connection::start_tls`].
    pub transport: Transport,
}

impl ConnectionInfo {
    /// Whether the connection is encrypted, here or at a trusted proxy.
    pub fn encrypted(&self) -> bool {
        self.transport != Transport::Plain
    }

    /// Whether the client is on this machine. Judged by the real client, so never `true` for
    /// a proxy's own address standing in for one.
    pub fn loopback(&self) -> bool {
        self.source != ClientSource::ProxyLocal && self.client.ip().is_loopback()
    }

    /// Whether a password may be sent on this connection: it is encrypted, or the client is
    /// on this machine.
    pub fn sign_in_allowed(&self) -> bool {
        self.encrypted() || self.loopback()
    }
}

/// A TCP stream that fails as soon as its bytes turn out to start with a PROXY header. The
/// listener reads headers it trusts straight from the socket, so what arrives here is the
/// client's own.
#[derive(Debug)]
pub(crate) struct Guarded {
    inner: TcpStream,
    matched: Option<usize>,
}

impl Guarded {
    pub(crate) fn new(inner: TcpStream) -> Self {
        Self {
            inner,
            matched: Some(0),
        }
    }

    fn inspect(&mut self, new: &[u8]) -> io::Result<()> {
        let Some(matched) = self.matched else {
            return Ok(());
        };
        let take = new.len().min(SIGNATURE.len() - matched);
        if new[..take] != SIGNATURE[matched..matched + take] {
            self.matched = None;
            return Ok(());
        }
        if matched + take == SIGNATURE.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a PROXY header from a peer that may not send one",
            ));
        }
        self.matched = Some(matched + take);
        Ok(())
    }
}

impl AsyncRead for Guarded {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => Poll::Ready(this.inspect(&buf.filled()[before..])),
            other => other,
        }
    }
}

impl AsyncWrite for Guarded {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub(crate) enum Stream {
    Plain(Guarded),
    Tls(Box<TlsStream<Guarded>>),
    Broken,
}

fn broken() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "the TLS handshake failed")
}

/// A connection that passed the listener's checks, ready for a protocol. Reads and writes the
/// client's bytes, in the clear or through TLS as the case may be. Dropping it closes the socket
/// and frees its places in the limits.
pub struct Connection {
    pub(crate) stream: Stream,
    pub(crate) info: ConnectionInfo,
    pub(crate) slot: LimitPlace,
    pub(crate) starttls: Option<TlsAcceptor>,
    pub(crate) handshake_timeout: Duration,
}

impl Connection {
    /// What is known about this connection.
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Whether [`start_tls`](Self::start_tls) can succeed; a protocol offers `STARTTLS` only
    /// when this is `true`. `false` once the connection is encrypted.
    pub fn can_start_tls(&self) -> bool {
        self.starttls.is_some()
    }

    /// Turns a plain connection into a TLS one, in place. A front end sends its "go ahead"
    /// first, and discards whatever it had buffered: this type never reads ahead, so bytes in
    /// the front end's own buffer are the only plaintext that could survive the upgrade.
    ///
    /// [`NetError::TlsUnavailable`] leaves the connection as it was. [`NetError::Handshake`],
    /// also on timeout, leaves it unusable; every later read or write fails.
    pub async fn start_tls(&mut self) -> Result<(), NetError> {
        let acceptor = self.starttls.take().ok_or(NetError::TlsUnavailable)?;
        let Stream::Plain(plain) = std::mem::replace(&mut self.stream, Stream::Broken) else {
            return Err(NetError::TlsUnavailable);
        };
        let tls = tokio::time::timeout(self.handshake_timeout, acceptor.accept(plain))
            .await
            .map_err(|_| NetError::Handshake)?
            .map_err(|_| NetError::Handshake)?;
        self.stream = Stream::Tls(Box::new(tls));
        self.info.transport = Transport::Tls;
        Ok(())
    }

    /// Records that `user` signed in on this connection: its place moves from the client
    /// address's count to the user's. [`LimitExceeded::PerUser`] changes nothing; the front end
    /// refuses the sign-in. Signing in again as the same user counts once.
    pub fn authenticated(&mut self, user: &UserId) -> Result<(), LimitExceeded> {
        self.slot.authenticated(user)
    }

    /// This connection's place in the limits, to keep after the connection itself was handed on.
    pub fn place(&self) -> LimitPlace {
        self.slot.clone()
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().stream {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Broken => Poll::Ready(Err(broken())),
        }
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().stream {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Broken => Poll::Ready(Err(broken())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().stream {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s).poll_flush(cx),
            Stream::Broken => Poll::Ready(Err(broken())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().stream {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Broken => Poll::Ready(Err(broken())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn pair() -> (TcpStream, Guarded) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, Guarded::new(server))
    }

    #[tokio::test]
    async fn a_signature_in_pieces_is_caught() {
        let (mut client, mut guarded) = pair().await;
        let mut buf = [0u8; 64];
        client.write_all(&SIGNATURE[..5]).await.unwrap();
        assert_eq!(guarded.read(&mut buf).await.unwrap(), 5);
        client.write_all(&SIGNATURE[5..]).await.unwrap();
        let error = guarded.read(&mut buf).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn ordinary_bytes_pass_and_the_guard_stands_down() {
        let (mut client, mut guarded) = pair().await;
        let mut buf = [0u8; 64];
        client.write_all(b"\r\nA").await.unwrap();
        let n = guarded.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"\r\nA");
        client.write_all(&SIGNATURE).await.unwrap();
        let n = guarded.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &SIGNATURE);
    }

    #[tokio::test]
    async fn end_of_stream_is_not_an_error() {
        let (client, mut guarded) = pair().await;
        drop(client);
        let mut buf = [0u8; 8];
        assert_eq!(guarded.read(&mut buf).await.unwrap(), 0);
    }
}
