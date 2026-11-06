use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::BytesMut;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

use crate::{Envelope, Handler, Options, Recipient, Refusal, peer_ip};

const MAX_COMMAND: usize = 2_048;
const MAX_ERRORS: u32 = 10;
const MAX_AUTH_FAILURES: u32 = 3;

enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    Gone,
}

fn gone() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "the connection was handed to TLS",
    )
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
            Self::Gone => Poll::Ready(Err(gone())),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
            Self::Gone => Poll::Ready(Err(gone())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s).poll_flush(cx),
            Self::Gone => Poll::Ready(Err(gone())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
            Self::Gone => Poll::Ready(Ok(())),
        }
    }
}

enum Read {
    Line(Vec<u8>),
    TooLong,
    Closed,
    TimedOut,
}

struct Session<'a> {
    io: BufReader<Stream>,
    options: &'a Options,
    handler: &'a dyn Handler,
    peer: IpAddr,
    helo: Option<String>,
    from: Option<String>,
    recipients: Vec<String>,
    tls: bool,
    authenticated: bool,
    errors: u32,
    auth_failures: u32,
}

pub(crate) async fn run(
    stream: TcpStream,
    peer: SocketAddr,
    options: &Options,
    handler: &dyn Handler,
    allowed: bool,
) {
    let peer = peer_ip(peer);
    let mut session = Session {
        io: BufReader::new(Stream::Plain(stream)),
        options,
        handler,
        peer,
        helo: None,
        from: None,
        recipients: Vec::new(),
        tls: false,
        authenticated: false,
        errors: 0,
        auth_failures: 0,
    };
    if !allowed {
        tracing::info!(%peer, "SMTP client refused");
        let _ = session
            .reply("554 5.7.1 Not accepted from this address")
            .await;
        return;
    }
    if session
        .reply(&format!("220 {} Holtmail ESMTP", options.hostname))
        .await
        .is_err()
    {
        return;
    }
    let _ = session.serve().await;
}

impl Session<'_> {
    async fn reply(&mut self, line: &str) -> io::Result<()> {
        let stream = self.io.get_mut();
        stream.write_all(line.as_bytes()).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await
    }

    async fn read(&mut self, max: usize) -> Read {
        let mut line = Vec::new();
        let work = async {
            loop {
                let buf = match self.io.fill_buf().await {
                    Ok(buf) => buf,
                    Err(_) => return Read::Closed,
                };
                if buf.is_empty() {
                    return Read::Closed;
                }
                let (take, done) = match buf.iter().position(|b| *b == b'\n') {
                    Some(at) => (at + 1, true),
                    None => (buf.len(), false),
                };
                if line.len() + take > max {
                    return Read::TooLong;
                }
                line.extend_from_slice(&buf[..take]);
                self.io.consume(take);
                if done {
                    return Read::Line(std::mem::take(&mut line));
                }
            }
        };
        tokio::time::timeout(self.options.idle_timeout, work)
            .await
            .unwrap_or(Read::TimedOut)
    }

    fn may_sign_in(&self) -> bool {
        self.options.credentials.is_some() && (self.tls || self.peer.is_loopback())
    }

    fn reset(&mut self) {
        self.from = None;
        self.recipients.clear();
    }

    async fn serve(&mut self) -> io::Result<()> {
        loop {
            let line = match self.read(MAX_COMMAND).await {
                Read::Line(line) => line,
                Read::TooLong => {
                    self.reply("500 5.5.6 Line too long").await?;
                    return Ok(());
                }
                Read::Closed => return Ok(()),
                Read::TimedOut => {
                    self.reply("421 4.4.2 Timeout, closing").await?;
                    return Ok(());
                }
            };
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\r', '\n']);
            let (verb, rest) = text.split_once(' ').unwrap_or((text, ""));
            let verb = verb.to_ascii_uppercase();
            let rest = rest.trim();
            let good = match verb.as_str() {
                "EHLO" => self.ehlo(rest).await?,
                "HELO" => self.helo(rest).await?,
                "STARTTLS" => match self.starttls().await? {
                    Some(good) => good,
                    None => return Ok(()),
                },
                "AUTH" => match self.auth(rest).await? {
                    Some(good) => good,
                    None => return Ok(()),
                },
                "MAIL" => self.mail(rest).await?,
                "RCPT" => self.rcpt(rest).await?,
                "DATA" => match self.data().await? {
                    Some(good) => good,
                    None => return Ok(()),
                },
                "RSET" => {
                    self.reset();
                    self.reply("250 2.0.0 OK").await?;
                    true
                }
                "NOOP" => {
                    self.reply("250 2.0.0 OK").await?;
                    true
                }
                "VRFY" => {
                    self.reply("252 2.1.5 Cannot verify, but will try").await?;
                    true
                }
                "HELP" => {
                    self.reply("214 2.0.0 See RFC 5321").await?;
                    true
                }
                "QUIT" => {
                    self.reply("221 2.0.0 Bye").await?;
                    return Ok(());
                }
                _ => {
                    self.reply("500 5.5.2 Command not recognized").await?;
                    false
                }
            };
            if !good {
                self.errors += 1;
                if self.errors >= MAX_ERRORS {
                    self.reply("421 4.7.0 Too many errors, closing").await?;
                    return Ok(());
                }
            }
        }
    }

    async fn ehlo(&mut self, name: &str) -> io::Result<bool> {
        if name.is_empty() {
            self.reply("501 5.5.4 EHLO needs a name").await?;
            return Ok(false);
        }
        self.reset();
        self.helo = Some(name.to_owned());
        let mut lines = vec![
            format!("{} Hello {name}", self.options.hostname),
            format!("SIZE {}", self.options.max_size),
            "8BITMIME".to_owned(),
            "SMTPUTF8".to_owned(),
            "ENHANCEDSTATUSCODES".to_owned(),
        ];
        if self.options.tls.is_some() && !self.tls {
            lines.push("STARTTLS".to_owned());
        }
        if self.may_sign_in() && !self.authenticated {
            lines.push("AUTH PLAIN LOGIN".to_owned());
        }
        let last = lines.len() - 1;
        for (at, line) in lines.iter().enumerate() {
            let sep = if at == last { ' ' } else { '-' };
            self.reply(&format!("250{sep}{line}")).await?;
        }
        Ok(true)
    }

    async fn helo(&mut self, name: &str) -> io::Result<bool> {
        if name.is_empty() {
            self.reply("501 5.5.4 HELO needs a name").await?;
            return Ok(false);
        }
        self.reset();
        self.helo = Some(name.to_owned());
        self.reply(&format!("250 {}", self.options.hostname))
            .await?;
        Ok(true)
    }

    async fn starttls(&mut self) -> io::Result<Option<bool>> {
        let Some(acceptor) = self.options.tls.clone() else {
            self.reply("502 5.5.1 STARTTLS is not offered").await?;
            return Ok(Some(false));
        };
        if self.tls {
            self.reply("503 5.5.1 TLS is already on").await?;
            return Ok(Some(false));
        }
        if !self.io.buffer().is_empty() {
            self.reply("503 5.5.1 Nothing may follow STARTTLS before the handshake")
                .await?;
            return Ok(None);
        }
        self.reply("220 2.0.0 Ready to start TLS").await?;
        let placeholder = BufReader::new(Stream::Gone);
        let Stream::Plain(tcp) = std::mem::replace(&mut self.io, placeholder).into_inner() else {
            return Ok(None);
        };
        match tokio::time::timeout(self.options.idle_timeout, acceptor.accept(tcp)).await {
            Ok(Ok(tls)) => {
                self.io = BufReader::new(Stream::Tls(Box::new(tls)));
                self.tls = true;
                self.helo = None;
                self.authenticated = false;
                self.reset();
                Ok(Some(true))
            }
            _ => {
                tracing::info!(peer = %self.peer, "SMTP TLS handshake failed");
                Ok(None)
            }
        }
    }

    async fn auth(&mut self, rest: &str) -> io::Result<Option<bool>> {
        let Some(expected) = self.options.credentials.clone() else {
            self.reply("502 5.5.1 AUTH is not offered").await?;
            return Ok(Some(false));
        };
        if !self.may_sign_in() {
            self.reply("538 5.7.11 Encryption required to sign in")
                .await?;
            return Ok(Some(false));
        }
        if self.helo.is_none() || self.authenticated || self.from.is_some() {
            self.reply("503 5.5.1 Bad sequence of commands").await?;
            return Ok(Some(false));
        }
        let (mechanism, initial) = rest.split_once(' ').unwrap_or((rest, ""));
        let given = match mechanism.to_ascii_uppercase().as_str() {
            "PLAIN" => {
                let answer = if initial.is_empty() {
                    self.reply("334 ").await?;
                    match self.challenge().await? {
                        Some(answer) => answer,
                        None => return Ok(None),
                    }
                } else {
                    initial.to_owned()
                };
                decode(&answer).and_then(|bytes| {
                    let mut parts = bytes.split(|b| *b == 0);
                    let _authorize = parts.next()?;
                    let user = String::from_utf8(parts.next()?.to_vec()).ok()?;
                    let pass = String::from_utf8(parts.next()?.to_vec()).ok()?;
                    Some((user, pass))
                })
            }
            "LOGIN" => {
                let user = if initial.is_empty() {
                    self.reply("334 VXNlcm5hbWU6").await?;
                    match self.challenge().await? {
                        Some(answer) => answer,
                        None => return Ok(None),
                    }
                } else {
                    initial.to_owned()
                };
                self.reply("334 UGFzc3dvcmQ6").await?;
                let pass = match self.challenge().await? {
                    Some(answer) => answer,
                    None => return Ok(None),
                };
                decode(&user)
                    .and_then(|u| String::from_utf8(u).ok())
                    .zip(decode(&pass).and_then(|p| String::from_utf8(p).ok()))
            }
            _ => {
                self.reply("504 5.5.4 Unknown AUTH mechanism").await?;
                return Ok(Some(false));
            }
        };
        let good = given.is_some_and(|(user, pass)| {
            equal(user.as_bytes(), expected.username.as_bytes())
                & equal(pass.as_bytes(), expected.password.as_bytes())
        });
        if good {
            self.authenticated = true;
            self.reply("235 2.7.0 Authentication succeeded").await?;
            return Ok(Some(true));
        }
        self.auth_failures += 1;
        tracing::info!(peer = %self.peer, failures = self.auth_failures, "SMTP sign-in refused");
        if self.auth_failures >= MAX_AUTH_FAILURES {
            self.reply("421 4.7.0 Too many failed sign-ins, closing")
                .await?;
            return Ok(None);
        }
        self.reply("535 5.7.8 Authentication credentials invalid")
            .await?;
        Ok(Some(false))
    }

    async fn challenge(&mut self) -> io::Result<Option<String>> {
        match self.read(MAX_COMMAND).await {
            Read::Line(line) => {
                let text = String::from_utf8_lossy(&line).trim().to_owned();
                if text == "*" {
                    self.reply("501 5.7.0 Authentication cancelled").await?;
                    return Ok(Some(String::new()));
                }
                Ok(Some(text))
            }
            _ => Ok(None),
        }
    }

    async fn mail(&mut self, rest: &str) -> io::Result<bool> {
        if self.helo.is_none() {
            self.reply("503 5.5.1 Say EHLO first").await?;
            return Ok(false);
        }
        if self.options.require_tls && !self.tls {
            self.reply("530 5.7.0 Must issue a STARTTLS command first")
                .await?;
            return Ok(false);
        }
        if self.options.credentials.is_some() && !self.authenticated {
            self.reply("530 5.7.0 Authentication required").await?;
            return Ok(false);
        }
        if self.from.is_some() {
            self.reply("503 5.5.1 Already in a transaction").await?;
            return Ok(false);
        }
        let Some((address, params)) = path(rest, "FROM:") else {
            self.reply("501 5.5.4 Syntax: MAIL FROM:<address>").await?;
            return Ok(false);
        };
        for param in params.split_whitespace() {
            if let Some(size) = param
                .split_once('=')
                .filter(|(k, _)| k.eq_ignore_ascii_case("SIZE"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                && size > self.options.max_size
            {
                self.reply("552 5.3.4 Message too big").await?;
                return Ok(false);
            }
        }
        self.from = Some(address);
        self.reply("250 2.1.0 OK").await?;
        Ok(true)
    }

    async fn rcpt(&mut self, rest: &str) -> io::Result<bool> {
        if self.from.is_none() {
            self.reply("503 5.5.1 Need MAIL first").await?;
            return Ok(false);
        }
        let Some((address, _)) = path(rest, "TO:").filter(|(a, _)| !a.is_empty()) else {
            self.reply("501 5.5.4 Syntax: RCPT TO:<address>").await?;
            return Ok(false);
        };
        if self.recipients.len() >= self.options.max_recipients {
            self.reply("452 4.5.3 Too many recipients").await?;
            return Ok(true);
        }
        match self.handler.recipient(&address).await {
            Recipient::Accept => {
                self.recipients.push(address);
                self.reply("250 2.1.5 OK").await?;
                Ok(true)
            }
            Recipient::Reject(text) => {
                self.reply(&format!("550 5.1.1 {}", one_line(&text)))
                    .await?;
                Ok(false)
            }
        }
    }

    async fn data(&mut self) -> io::Result<Option<bool>> {
        if self.from.is_none() {
            self.reply("503 5.5.1 Need MAIL first").await?;
            return Ok(Some(false));
        }
        if self.recipients.is_empty() {
            self.reply("554 5.5.1 No valid recipients").await?;
            return Ok(Some(false));
        }
        self.reply("354 End data with <CR><LF>.<CR><LF>").await?;
        let mut data = BytesMut::new();
        let mut too_big = false;
        let mut line_start = true;
        let max_line = self.options.max_size.saturating_add(1_024).max(MAX_COMMAND);
        loop {
            let line = match self.read(max_line).await {
                Read::Line(line) => line,
                Read::TooLong => {
                    self.reply("552 5.3.4 Message too big").await?;
                    return Ok(None);
                }
                Read::Closed => return Ok(None),
                Read::TimedOut => {
                    self.reply("421 4.4.2 Timeout, closing").await?;
                    return Ok(None);
                }
            };
            let starts = line_start;
            line_start = line.ends_with(b"\r\n");
            if starts && line == b".\r\n" {
                break;
            }
            let line = match line.strip_prefix(b".") {
                Some(rest) if starts => rest,
                _ => &line,
            };
            if data.len() + line.len() > self.options.max_size {
                too_big = true;
            }
            if !too_big {
                data.extend_from_slice(line);
            }
        }
        let envelope = Envelope {
            peer: self.peer,
            helo: self.helo.clone().unwrap_or_default(),
            mail_from: self.from.clone().unwrap_or_default(),
            recipients: self.recipients.clone(),
            tls: self.tls,
            authenticated: self.authenticated,
        };
        self.reset();
        if too_big {
            self.reply("552 5.3.4 Message too big").await?;
            return Ok(Some(false));
        }
        match self.handler.deliver(&envelope, data.freeze()).await {
            Ok(()) => self.reply("250 2.0.0 OK: queued").await?,
            Err(Refusal::Temporary(text)) => {
                self.reply(&format!("451 4.3.0 {}", one_line(&text)))
                    .await?
            }
            Err(Refusal::Permanent(text)) => {
                self.reply(&format!("554 5.6.0 {}", one_line(&text)))
                    .await?
            }
        }
        Ok(Some(true))
    }
}

fn path(rest: &str, keyword: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    if rest.len() < keyword.len() || !rest[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = rest[keyword.len()..].trim_start();
    let rest = rest.strip_prefix('<')?;
    let end = rest.find('>')?;
    let address = rest[..end].trim().to_owned();
    if address.contains(['\r', '\n', '<']) {
        return None;
    }
    Some((address, rest[end + 1..].trim().to_owned()))
}

fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
}

fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect()
}
