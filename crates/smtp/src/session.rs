use std::any::Any;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::BytesMut;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::server::TlsStream;

use crate::server::{
    AuthError, ClientStream, Connection, Ended, Envelope, Handler, Limits, Options, Session,
};

const MAX_COMMAND: usize = 2_048;
const MAX_ERRORS: u32 = 10;
const MAX_AUTH_FAILURES: u32 = 3;
const NOT_ASCII: &str = "553 5.6.7 This address needs SMTPUTF8";

enum Stream {
    Plain(Box<dyn ClientStream>),
    Tls(Box<TlsStream<Box<dyn ClientStream>>>),
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
    Ended,
}

enum Given {
    Login(String, String),
    Malformed,
    Cancelled,
    UnknownMechanism,
    Closed,
}

struct Conversation<'a> {
    io: BufReader<Stream>,
    out: Vec<u8>,
    options: &'a Options,
    handler: &'a dyn Handler,
    session: Session,
    ended: Option<Ended>,
    until_signed_in: Option<Box<dyn Any + Send>>,
    from: Option<String>,
    recipients: Vec<String>,
    limits: Limits,
    errors: u32,
    auth_failures: u32,
}

pub(crate) async fn refuse(mut stream: TcpStream, peer: IpAddr, options: &Options) {
    tracing::info!(peer = %peer.to_canonical(), "SMTP client refused");
    if !options.implicit_tls {
        let _ = stream
            .write_all(b"554 5.7.1 Not accepted from this address\r\n")
            .await;
        let _ = stream.flush().await;
    }
}

pub(crate) async fn run(connection: Connection, options: &Options, handler: &dyn Handler) {
    let Connection {
        stream,
        peer,
        tls_by_proxy,
        until_signed_in,
    } = connection;
    let peer = peer.to_canonical();
    let stream = if options.implicit_tls && !tls_by_proxy {
        let Some(acceptor) = options.tls.clone() else {
            tracing::error!("SMTP implicit TLS has no certificate, closing");
            return;
        };
        let wait = options.sign_in_timeout.unwrap_or(options.idle_timeout);
        match timeout(wait, acceptor.accept(stream)).await {
            Ok(Ok(tls)) => Stream::Tls(Box::new(tls)),
            _ => {
                tracing::info!(%peer, "SMTP TLS handshake failed");
                return;
            }
        }
    } else {
        Stream::Plain(stream)
    };
    let mut talk = Conversation {
        io: BufReader::new(stream),
        out: Vec::new(),
        options,
        handler,
        session: Session {
            peer,
            helo: None,
            tls: tls_by_proxy || options.implicit_tls,
            principal: None,
        },
        ended: None,
        until_signed_in,
        from: None,
        recipients: Vec::new(),
        limits: Limits {
            max_size: options.max_size,
            smtputf8: true,
        },
        errors: 0,
        auth_failures: 0,
    };
    if talk.reply(&greeting(options)).await.is_err() {
        return;
    }
    if talk.serve().await.is_ok() {
        let _ = talk.flush().await;
    }
}

async fn limits_for(options: &Options, handler: &dyn Handler, session: &Session) -> Limits {
    match handler.limits(session).await {
        Some(limits) => Limits {
            max_size: limits.max_size.min(options.max_size),
            smtputf8: limits.smtputf8,
        },
        None => Limits {
            max_size: options.max_size,
            smtputf8: true,
        },
    }
}

fn greeting(options: &Options) -> String {
    let mut line = format!("220 {} Holtmail ESMTP", options.hostname);
    if let Some(token) = &options.instance_token {
        line.push_str(" holtmail-instance=");
        line.extend(
            token
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        );
    }
    line
}

impl Conversation<'_> {
    fn command_waiting(&self) -> bool {
        self.io.buffer().contains(&b'\n')
    }

    async fn flush(&mut self) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let out = std::mem::take(&mut self.out);
        let stream = self.io.get_mut();
        let work = async {
            stream.write_all(&out).await?;
            stream.flush().await
        };
        timeout(self.options.idle_timeout, work)
            .await
            .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
    }

    async fn reply(&mut self, line: &str) -> io::Result<()> {
        self.out.extend_from_slice(line.as_bytes());
        self.out.extend_from_slice(b"\r\n");
        if self.command_waiting() {
            return Ok(());
        }
        self.flush().await
    }

    fn wait(&self) -> Duration {
        match self.options.sign_in_timeout {
            Some(wait) if self.options.auth.is_some() && self.session.principal.is_none() => wait,
            _ => self.options.idle_timeout,
        }
    }

    async fn read(&mut self, max: usize) -> Read {
        if !self.command_waiting() && self.flush().await.is_err() {
            return Read::Closed;
        }
        let wait = self.wait();
        let Self { io, ended, .. } = self;
        let mut line = Vec::new();
        let work = async {
            loop {
                let buf = match io.fill_buf().await {
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
                io.consume(take);
                if done {
                    return Read::Line(std::mem::take(&mut line));
                }
            }
        };
        let work = async { timeout(wait, work).await.unwrap_or(Read::TimedOut) };
        match ended {
            Some(ended) => tokio::select! {
                read = work => read,
                () = ended.as_mut() => Read::Ended,
            },
            None => work.await,
        }
    }

    fn may_sign_in(&self) -> bool {
        self.options.auth.is_some() && (self.session.tls || self.session.peer.is_loopback())
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
                Read::Ended => {
                    self.reply("421 4.7.0 Signed out, closing").await?;
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
        self.session.helo = Some(name.to_owned());
        let limits = limits_for(self.options, self.handler, &self.session).await;
        let mut lines = vec![
            format!("{} Hello {name}", self.options.hostname),
            format!("SIZE {}", limits.max_size),
            "PIPELINING".to_owned(),
            "8BITMIME".to_owned(),
        ];
        if limits.smtputf8 {
            lines.push("SMTPUTF8".to_owned());
        }
        lines.push("ENHANCEDSTATUSCODES".to_owned());
        if self.options.tls.is_some() && !self.session.tls {
            lines.push("STARTTLS".to_owned());
        }
        if self.may_sign_in() && self.session.principal.is_none() {
            lines.push("AUTH PLAIN LOGIN".to_owned());
        }
        let last = lines.len() - 1;
        let answer = lines
            .iter()
            .enumerate()
            .map(|(at, line)| format!("250{}{line}", if at == last { ' ' } else { '-' }))
            .collect::<Vec<_>>()
            .join("\r\n");
        self.reply(&answer).await?;
        Ok(true)
    }

    async fn helo(&mut self, name: &str) -> io::Result<bool> {
        if name.is_empty() {
            self.reply("501 5.5.4 HELO needs a name").await?;
            return Ok(false);
        }
        self.reset();
        self.session.helo = Some(name.to_owned());
        self.reply(&format!("250 {}", self.options.hostname))
            .await?;
        Ok(true)
    }

    async fn starttls(&mut self) -> io::Result<Option<bool>> {
        let Some(acceptor) = self.options.tls.clone() else {
            self.reply("502 5.5.1 STARTTLS is not offered").await?;
            return Ok(Some(false));
        };
        if self.session.tls {
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
        match timeout(self.wait(), acceptor.accept(tcp)).await {
            Ok(Ok(tls)) => {
                self.io = BufReader::new(Stream::Tls(Box::new(tls)));
                self.session.tls = true;
                self.session.helo = None;
                self.session.principal = None;
                self.ended = None;
                self.reset();
                Ok(Some(true))
            }
            _ => {
                tracing::info!(peer = %self.session.peer, "SMTP TLS handshake failed");
                Ok(None)
            }
        }
    }

    async fn auth(&mut self, rest: &str) -> io::Result<Option<bool>> {
        let Some(auth) = self.options.auth.clone() else {
            self.reply("502 5.5.1 AUTH is not offered").await?;
            return Ok(Some(false));
        };
        if !self.may_sign_in() {
            self.reply("538 5.7.11 Encryption required to sign in")
                .await?;
            return Ok(Some(false));
        }
        if self.session.helo.is_none() || self.session.principal.is_some() || self.from.is_some() {
            self.reply("503 5.5.1 Bad sequence of commands").await?;
            return Ok(Some(false));
        }
        let verdict = match self.given(rest).await? {
            Given::Closed => return Ok(None),
            Given::Cancelled => return Ok(Some(false)),
            Given::UnknownMechanism => {
                self.reply("504 5.5.4 Unknown AUTH mechanism").await?;
                return Ok(Some(false));
            }
            Given::Malformed => Err(AuthError::Invalid),
            Given::Login(login, secret) => auth.verify(&login, &secret, self.session.peer).await,
        };
        match verdict {
            Ok(signed_in) => {
                self.session.principal = Some(signed_in.principal);
                self.ended = signed_in.ended;
                self.until_signed_in = None;
                self.reply("235 2.7.0 Authentication succeeded").await?;
                Ok(Some(true))
            }
            Err(AuthError::Unavailable) => {
                self.reply("454 4.7.0 Temporary authentication failure")
                    .await?;
                Ok(Some(false))
            }
            Err(AuthError::TooMany) => self.too_many_sign_ins().await,
            Err(AuthError::Invalid) => {
                self.auth_failures += 1;
                tracing::info!(
                    peer = %self.session.peer,
                    failures = self.auth_failures,
                    "SMTP sign-in refused"
                );
                if self.auth_failures >= MAX_AUTH_FAILURES {
                    return self.too_many_sign_ins().await;
                }
                self.reply("535 5.7.8 Authentication credentials invalid")
                    .await?;
                Ok(Some(false))
            }
        }
    }

    async fn too_many_sign_ins(&mut self) -> io::Result<Option<bool>> {
        self.reply("421 4.7.0 Too many failed sign-ins, closing")
            .await?;
        Ok(None)
    }

    async fn given(&mut self, rest: &str) -> io::Result<Given> {
        let (mechanism, initial) = rest.split_once(' ').unwrap_or((rest, ""));
        let pair = match mechanism.to_ascii_uppercase().as_str() {
            "PLAIN" => {
                let answer = if initial.is_empty() {
                    self.reply("334 ").await?;
                    match self.challenge().await? {
                        Ok(answer) => answer,
                        Err(given) => return Ok(given),
                    }
                } else {
                    initial.to_owned()
                };
                decode(&answer).and_then(|bytes| plain(&bytes))
            }
            "LOGIN" => {
                let user = if initial.is_empty() {
                    self.reply("334 VXNlcm5hbWU6").await?;
                    match self.challenge().await? {
                        Ok(answer) => answer,
                        Err(given) => return Ok(given),
                    }
                } else {
                    initial.to_owned()
                };
                self.reply("334 UGFzc3dvcmQ6").await?;
                let pass = match self.challenge().await? {
                    Ok(answer) => answer,
                    Err(given) => return Ok(given),
                };
                decode(&user)
                    .and_then(|u| String::from_utf8(u).ok())
                    .zip(decode(&pass).and_then(|p| String::from_utf8(p).ok()))
            }
            _ => return Ok(Given::UnknownMechanism),
        };
        Ok(pair.map_or(Given::Malformed, |(login, secret)| {
            Given::Login(login, secret)
        }))
    }

    async fn challenge(&mut self) -> io::Result<Result<String, Given>> {
        match self.read(MAX_COMMAND).await {
            Read::Line(line) => {
                let text = String::from_utf8_lossy(&line).trim().to_owned();
                if text == "*" {
                    self.reply("501 5.7.0 Authentication cancelled").await?;
                    return Ok(Err(Given::Cancelled));
                }
                Ok(Ok(text))
            }
            _ => Ok(Err(Given::Closed)),
        }
    }

    async fn mail(&mut self, rest: &str) -> io::Result<bool> {
        if self.session.helo.is_none() {
            self.reply("503 5.5.1 Say EHLO first").await?;
            return Ok(false);
        }
        if self.options.require_tls && !self.session.tls {
            self.reply("530 5.7.0 Must issue a STARTTLS command first")
                .await?;
            return Ok(false);
        }
        if self.options.auth.is_some() && self.session.principal.is_none() {
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
        let limits = limits_for(self.options, self.handler, &self.session).await;
        for param in params.split_whitespace() {
            let (key, value) = param.split_once('=').unwrap_or((param, ""));
            let refusal = match key.to_ascii_uppercase().as_str() {
                "SIZE" if value.parse::<usize>().is_ok_and(|s| s > limits.max_size) => {
                    Some("552 5.3.4 Message too big")
                }
                "BODY"
                    if !value.eq_ignore_ascii_case("7BIT")
                        && !value.eq_ignore_ascii_case("8BITMIME") =>
                {
                    Some("555 5.5.4 BODY type not supported")
                }
                "SMTPUTF8" if !limits.smtputf8 => Some("555 5.5.4 SMTPUTF8 is not offered"),
                _ => None,
            };
            if let Some(refusal) = refusal {
                self.reply(refusal).await?;
                return Ok(false);
            }
        }
        if !limits.smtputf8 && !address.is_ascii() {
            self.reply(NOT_ASCII).await?;
            return Ok(false);
        }
        if let Err(refused) = self.handler.mail_from(&self.session, &address).await {
            self.reply(&refused.line()).await?;
            return Ok(false);
        }
        self.limits = limits;
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
        if !self.limits.smtputf8 && !address.is_ascii() {
            self.reply(NOT_ASCII).await?;
            return Ok(false);
        }
        match self.handler.recipient(&self.session, &address).await {
            Ok(()) => {
                self.recipients.push(address);
                self.reply("250 2.1.5 OK").await?;
                Ok(true)
            }
            Err(refused) => {
                self.reply(&refused.line()).await?;
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
        let max_size = self.limits.max_size;
        let max_line = max_size.saturating_add(1_024).max(MAX_COMMAND);
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
                Read::Ended => {
                    self.reply("421 4.7.0 Signed out, closing").await?;
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
            if data.len() + line.len() > max_size {
                too_big = true;
            }
            if !too_big {
                data.extend_from_slice(line);
            }
        }
        let envelope = Envelope {
            peer: self.session.peer,
            helo: self.session.helo.clone().unwrap_or_default(),
            mail_from: self.from.clone().unwrap_or_default(),
            recipients: self.recipients.clone(),
            tls: self.session.tls,
            principal: self.session.principal.clone(),
        };
        self.reset();
        if too_big {
            self.reply("552 5.3.4 Message too big").await?;
            return Ok(Some(false));
        }
        match self
            .handler
            .deliver(&self.session, &envelope, data.freeze())
            .await
        {
            Ok(()) => self.reply("250 2.0.0 OK: queued").await?,
            Err(refused) => self.reply(&refused.line()).await?,
        }
        Ok(Some(true))
    }
}

fn path(rest: &str, keyword: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    if !rest
        .get(..keyword.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(keyword))
    {
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

fn plain(bytes: &[u8]) -> Option<(String, String)> {
    let mut parts = bytes.split(|b| *b == 0);
    let authorize = parts.next()?;
    let user = parts.next()?;
    let pass = parts.next()?;
    if parts.next().is_some() || !(authorize.is_empty() || authorize == user) {
        return None;
    }
    Some((
        String::from_utf8(user.to_vec()).ok()?,
        String::from_utf8(pass.to_vec()).ok()?,
    ))
}

fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
}
