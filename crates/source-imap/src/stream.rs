use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use holtmail_source::SourceError;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::Security;

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
    ) -> Result<(Self, bool), SourceError> {
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| SourceError::Connection(format!("{host}:{port}: {e}")))?;
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
        .map_err(|e| SourceError::Connection(format!("TLS with {host}: {e}")))
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
