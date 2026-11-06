use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use holtmail_source::SourceError;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::Security;

/// How long a plain or STARTTLS connection waits for the server to speak first. A TLS port says
/// nothing until the client starts TLS, so a longer silence most likely means one.
pub(crate) const GREETING_WAIT: Duration = Duration::from_secs(15);

const TLS_HINT: &str = "the port may expect TLS from the start (security = tls, usually port 993)";
const PLAIN_HINT: &str =
    "the server answered in plain text; the port may be plain or STARTTLS (usually port 143)";

#[derive(Debug)]
pub(crate) enum ImapStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl ImapStream {
    /// Connects and, for `starttls`, upgrades. Returns whether the server's greeting is still
    /// unread: after `STARTTLS` there is no new greeting.
    pub(crate) async fn connect(
        host: &str,
        port: u16,
        security: Security,
        tls: Arc<ClientConfig>,
        greeting_wait: Duration,
    ) -> Result<(Self, bool), SourceError> {
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| SourceError::Connection(format!("{host}:{port}: {e}")))?;
        if security != Security::Tls {
            await_greeting(&tcp, host, port, greeting_wait).await?;
        }
        match security {
            Security::Plain => Ok((Self::Plain(tcp), true)),
            Security::Tls => Ok((Self::Tls(Box::new(handshake(tcp, host, tls).await?)), true)),
            Security::Starttls => {
                let tcp = starttls(tcp).await?;
                let tls = handshake(tcp, host, tls).await?;
                Ok((Self::Tls(Box::new(tls)), false))
            }
        }
    }
}

async fn await_greeting(
    tcp: &TcpStream,
    host: &str,
    port: u16,
    wait: Duration,
) -> Result<(), SourceError> {
    let mut first = [0u8; 1];
    match tokio::time::timeout(wait, tcp.peek(&mut first)).await {
        Err(_) => Err(SourceError::Connection(format!(
            "{host}:{port} sent no greeting within {} s; {TLS_HINT}",
            wait.as_secs_f32()
        ))),
        Ok(Err(e)) => Err(SourceError::Connection(format!("{host}:{port}: {e}"))),
        Ok(Ok(0)) => Err(SourceError::Connection(format!(
            "{host}:{port} closed the connection before the greeting"
        ))),
        Ok(Ok(_)) if matches!(first[0], 0x15 | 0x16) => Err(SourceError::Connection(format!(
            "{host}:{port} answered with TLS; {TLS_HINT}"
        ))),
        Ok(Ok(_)) => Ok(()),
    }
}

async fn handshake(
    tcp: TcpStream,
    host: &str,
    tls: Arc<ClientConfig>,
) -> Result<TlsStream<TcpStream>, SourceError> {
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| SourceError::Config(format!("{host} is not a valid host name")))?;
    TlsConnector::from(tls)
        .connect(name, tcp)
        .await
        .map_err(|e| {
            let plain = e
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<tokio_rustls::rustls::Error>())
                .is_some_and(|tls| matches!(tls, tokio_rustls::rustls::Error::InvalidMessage(_)));
            if plain {
                SourceError::Connection(format!("TLS with {host}: {e}; {PLAIN_HINT}"))
            } else {
                SourceError::Connection(format!("TLS with {host}: {e}"))
            }
        })
}

async fn starttls(tcp: TcpStream) -> Result<TcpStream, SourceError> {
    let broken = |e: io::Error| SourceError::Connection(e.to_string());
    let mut reader = BufReader::new(tcp);
    let mut line = String::new();
    reader.read_line(&mut line).await.map_err(broken)?;
    if !line.starts_with("* OK") {
        return Err(SourceError::Protocol(format!(
            "unexpected greeting {}",
            line.trim()
        )));
    }
    reader
        .get_mut()
        .write_all(b"a0 STARTTLS\r\n")
        .await
        .map_err(broken)?;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.map_err(broken)? == 0 {
            return Err(SourceError::Connection("closed during STARTTLS".into()));
        }
        if line.starts_with("a0 OK") {
            return Ok(reader.into_inner());
        }
        if line.starts_with("a0 ") {
            return Err(SourceError::Protocol(format!(
                "STARTTLS refused: {}",
                line.trim()
            )));
        }
    }
}

impl AsyncRead for ImapStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ImapStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
