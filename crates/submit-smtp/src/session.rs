use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use holtmail_submit::{
    Envelope, Limits, RecipientOutcome, RecipientResult, Refusal, SubmitError, SubmitReport,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::{Secret, Security, SmtpConfig};

const MAX_LINE: u64 = 4096;
const MAX_LINES: usize = 128;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

fn greeted_by(greeting: &str, token: &str) -> bool {
    greeting
        .split_whitespace()
        .filter_map(|word| word.strip_prefix("holtmail-instance="))
        .any(|found| found == token)
}

struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn text(&self) -> String {
        self.lines.join(" ")
    }

    fn ok(&self) -> bool {
        (200..300).contains(&self.code)
    }

    fn refusal(&self) -> Refusal {
        Refusal {
            permanent: self.code >= 500,
            code: self.code,
            text: self.text(),
        }
    }

    fn refused(&self) -> SubmitError {
        SubmitError::Refused {
            permanent: self.code >= 500,
            code: self.code,
            text: self.text(),
        }
    }

    fn describe(&self) -> String {
        format!("{} {}", self.code, self.text())
    }
}

#[derive(Default)]
struct Extensions {
    size: Option<u64>,
    eight_bit: bool,
    smtputf8: bool,
    starttls: bool,
    auth: Vec<String>,
}

impl Extensions {
    fn parse(lines: &[String]) -> Self {
        let mut ext = Self::default();
        for line in lines.iter().skip(1) {
            let mut words = line.split_whitespace();
            let Some(keyword) = words.next() else {
                continue;
            };
            match keyword.to_ascii_uppercase().as_str() {
                "SIZE" => ext.size = words.next().and_then(|n| n.parse().ok()),
                "8BITMIME" => ext.eight_bit = true,
                "SMTPUTF8" => ext.smtputf8 = true,
                "STARTTLS" => ext.starttls = true,
                "AUTH" => ext.auth = words.map(str::to_ascii_uppercase).collect(),
                _ => {}
            }
        }
        ext
    }
}

pub(crate) struct Session<'a> {
    config: &'a SmtpConfig,
    io: BufReader<Box<dyn Io>>,
    ext: Extensions,
}

impl<'a> Session<'a> {
    pub(crate) async fn open(
        config: &'a SmtpConfig,
        tls: Arc<ClientConfig>,
        own_instance: Option<&str>,
    ) -> Result<Self, SubmitError> {
        let unavailable = SubmitError::Unavailable;
        let tcp = within(
            config.timeout,
            TcpStream::connect((config.host.as_str(), config.port)),
        )
        .await
        .map_err(|e| {
            unavailable(format!(
                "connecting to {}:{}: {e}",
                config.host, config.port
            ))
        })?;
        let io: Box<dyn Io> = match config.security {
            Security::Tls => Box::new(handshake(config, tls.clone(), tcp).await?),
            Security::Starttls | Security::None => Box::new(tcp),
        };
        let mut session = Self {
            config,
            io: BufReader::new(io),
            ext: Extensions::default(),
        };
        let greeting = session.reply(config.timeout).await.map_err(unavailable)?;
        if !greeting.ok() {
            return Err(unavailable(format!(
                "the server greeted with {}",
                greeting.describe()
            )));
        }
        if own_instance.is_some_and(|token| greeted_by(&greeting.text(), token)) {
            return Err(SubmitError::Invalid(
                "this server is this Holtmail instance; a transport cannot submit to itself".into(),
            ));
        }
        session.hello().await?;
        if config.security == Security::Starttls {
            session.starttls(tls).await?;
        }
        session.authenticate().await?;
        Ok(session)
    }

    pub(crate) fn limits(&self) -> Limits {
        Limits {
            max_size: self.ext.size.filter(|n| *n > 0),
            smtputf8: self.ext.smtputf8,
        }
    }

    pub(crate) async fn send(
        &mut self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError> {
        let limit = self.limits().max_size;
        if limit.is_some_and(|limit| message.len() as u64 > limit) {
            return Err(SubmitError::TooLarge { limit });
        }
        let international = !envelope.mail_from.is_ascii()
            || envelope.recipients.iter().any(|r| !r.is_ascii())
            || !header_section(message).is_ascii();
        let eight_bit = !message.is_ascii();
        if international && !self.ext.smtputf8 {
            return Err(SubmitError::Invalid(
                "the server does not take internationalized mail (SMTPUTF8)".into(),
            ));
        }
        if eight_bit && !self.ext.eight_bit && !international {
            return Err(SubmitError::Invalid(
                "the server does not take 8-bit mail (8BITMIME)".into(),
            ));
        }

        let mut mail = format!("MAIL FROM:<{}>", envelope.mail_from);
        if self.ext.size.is_some() {
            mail.push_str(&format!(" SIZE={}", message.len()));
        }
        if eight_bit && self.ext.eight_bit {
            mail.push_str(" BODY=8BITMIME");
        }
        if international {
            mail.push_str(" SMTPUTF8");
        }
        let reply = self.command(&mail).await?;
        if !reply.ok() {
            return Err(match reply.code {
                552 => SubmitError::TooLarge { limit },
                530 | 535 => SubmitError::Auth(reply.text()),
                _ => reply.refused(),
            });
        }

        let mut recipients = Vec::with_capacity(envelope.recipients.len());
        for address in &envelope.recipients {
            let reply = self.command(&format!("RCPT TO:<{address}>")).await?;
            if reply.code == 421 {
                return Err(reply.refused());
            }
            let outcome = if reply.ok() {
                RecipientOutcome::Accepted
            } else {
                RecipientOutcome::Refused(reply.refusal())
            };
            recipients.push(RecipientResult {
                address: address.clone(),
                outcome,
            });
        }
        if !recipients
            .iter()
            .any(|r| r.outcome == RecipientOutcome::Accepted)
        {
            let _ = self.command("RSET").await;
            return Err(SubmitError::RecipientsRefused(recipients));
        }

        let reply = self.command("DATA").await?;
        if reply.code != 354 {
            return Err(match reply.code {
                552 => SubmitError::TooLarge { limit },
                200..400 => SubmitError::Temporary(format!(
                    "unexpected reply to DATA: {}",
                    reply.describe()
                )),
                _ => reply.refused(),
            });
        }
        let stuffed = dot_stuff(message);
        within(self.config.data_timeout, self.io.write_all(&stuffed))
            .await
            .map_err(|e| SubmitError::Temporary(format!("sending the message: {e}")))?;
        within(self.config.timeout, async {
            self.io.write_all(b".\r\n").await?;
            self.io.flush().await
        })
        .await
        .map_err(|e| SubmitError::Uncertain(format!("ending the message: {e}")))?;
        let reply = self
            .reply(self.config.data_timeout)
            .await
            .map_err(|e| SubmitError::Uncertain(format!("waiting for the reply: {e}")))?;
        match reply.code {
            200..300 => Ok(SubmitReport { recipients }),
            552 => Err(SubmitError::TooLarge { limit }),
            400..600 => Err(reply.refused()),
            _ => Err(SubmitError::Uncertain(format!(
                "unexpected reply to the message: {}",
                reply.describe()
            ))),
        }
    }

    pub(crate) async fn quit(mut self) {
        if self.write_line("QUIT").await.is_ok() {
            let _ = self.reply(self.config.timeout).await;
        }
        let _ = within(self.config.timeout, self.io.get_mut().shutdown()).await;
    }

    async fn hello(&mut self) -> Result<(), SubmitError> {
        let reply = self
            .open_command(&format!("EHLO {}", self.config.helo))
            .await?;
        if reply.ok() {
            self.ext = Extensions::parse(&reply.lines);
            return Ok(());
        }
        let reply = self
            .open_command(&format!("HELO {}", self.config.helo))
            .await?;
        if !reply.ok() {
            return Err(SubmitError::Unavailable(format!(
                "the server refused EHLO and HELO: {}",
                reply.describe()
            )));
        }
        self.ext = Extensions::default();
        Ok(())
    }

    async fn starttls(&mut self, tls: Arc<ClientConfig>) -> Result<(), SubmitError> {
        if !self.ext.starttls {
            return Err(SubmitError::Unavailable(
                "the server does not offer STARTTLS".into(),
            ));
        }
        let reply = self.open_command("STARTTLS").await?;
        if reply.code != 220 {
            return Err(SubmitError::Unavailable(format!(
                "the server refused STARTTLS: {}",
                reply.describe()
            )));
        }
        if !self.io.buffer().is_empty() {
            return Err(SubmitError::Unavailable(
                "the server sent data before the TLS handshake".into(),
            ));
        }
        let plain = std::mem::replace(
            &mut self.io,
            BufReader::new(Box::new(tokio::io::empty()) as Box<dyn Io>),
        )
        .into_inner();
        let secured = handshake(self.config, tls, plain).await?;
        self.io = BufReader::new(Box::new(secured));
        self.hello().await
    }

    async fn authenticate(&mut self) -> Result<(), SubmitError> {
        let (Some(username), Some(secret)) = (&self.config.username, &self.config.secret) else {
            return Ok(());
        };
        let offers = |mechanism: &str| self.ext.auth.iter().any(|m| m == mechanism);
        let (plain, login, xoauth2) = (offers("PLAIN"), offers("LOGIN"), offers("XOAUTH2"));
        let reply = match secret {
            Secret::OAuthToken(token) => {
                if !xoauth2 {
                    return Err(SubmitError::Auth(
                        "the server does not offer AUTH XOAUTH2".into(),
                    ));
                }
                let initial =
                    BASE64.encode(format!("user={username}\x01auth=Bearer {token}\x01\x01"));
                let reply = self
                    .open_command(&format!("AUTH XOAUTH2 {initial}"))
                    .await?;
                if reply.code == 334 {
                    self.open_command("").await?
                } else {
                    reply
                }
            }
            Secret::Password(password) if plain => {
                let initial = BASE64.encode(format!("\0{username}\0{password}"));
                self.open_command(&format!("AUTH PLAIN {initial}")).await?
            }
            Secret::Password(password) if login => {
                let mut reply = self.open_command("AUTH LOGIN").await?;
                if reply.code == 334 {
                    reply = self.open_command(&BASE64.encode(username)).await?;
                }
                if reply.code == 334 {
                    reply = self.open_command(&BASE64.encode(password)).await?;
                }
                reply
            }
            Secret::Password(_) => {
                return Err(SubmitError::Auth(
                    "the server offers neither AUTH PLAIN nor AUTH LOGIN".into(),
                ));
            }
        };
        match reply.code {
            200..300 => Ok(()),
            400..500 => Err(SubmitError::Temporary(format!(
                "signing in: {}",
                reply.describe()
            ))),
            334 => {
                let _ = self.open_command("*").await;
                Err(SubmitError::Auth(
                    "the server asked for more than the mechanism sends".into(),
                ))
            }
            _ => Err(SubmitError::Auth(reply.describe())),
        }
    }

    async fn open_command(&mut self, line: &str) -> Result<Reply, SubmitError> {
        self.write_line(line)
            .await
            .map_err(SubmitError::Unavailable)?;
        self.reply(self.config.timeout)
            .await
            .map_err(SubmitError::Unavailable)
    }

    async fn command(&mut self, line: &str) -> Result<Reply, SubmitError> {
        self.write_line(line)
            .await
            .map_err(SubmitError::Temporary)?;
        self.reply(self.config.timeout)
            .await
            .map_err(SubmitError::Temporary)
    }

    async fn write_line(&mut self, line: &str) -> Result<(), String> {
        within(self.config.timeout, async {
            self.io.write_all(line.as_bytes()).await?;
            self.io.write_all(b"\r\n").await?;
            self.io.flush().await
        })
        .await
        .map_err(|e| format!("sending a command: {e}"))
    }

    async fn reply(&mut self, limit: Duration) -> Result<Reply, String> {
        within(limit, async {
            let mut code = None;
            let mut lines = Vec::new();
            loop {
                let mut raw = Vec::new();
                (&mut self.io)
                    .take(MAX_LINE)
                    .read_until(b'\n', &mut raw)
                    .await?;
                if raw.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the server closed the connection",
                    ));
                }
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches(['\r', '\n']);
                let malformed = || std::io::Error::other("a malformed reply");
                let this: u16 = line
                    .get(..3)
                    .filter(|c| c.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|c| c.parse().ok())
                    .ok_or_else(malformed)?;
                if code.is_some_and(|c| c != this) || !raw.ends_with(b"\n") {
                    return Err(malformed());
                }
                code = Some(this);
                lines.push(line.get(4..).unwrap_or("").to_owned());
                match line.as_bytes().get(3) {
                    Some(b'-') if lines.len() < MAX_LINES => continue,
                    Some(b'-') => return Err(std::io::Error::other("a reply that never ends")),
                    None | Some(b' ') => return Ok(Reply { code: this, lines }),
                    Some(_) => return Err(malformed()),
                }
            }
        })
        .await
        .map_err(|e| e.to_string())
    }
}

async fn within<T>(
    limit: Duration,
    work: impl Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ))
    })
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin + Send>(
    config: &SmtpConfig,
    tls: Arc<ClientConfig>,
    stream: S,
) -> Result<tokio_rustls::client::TlsStream<S>, SubmitError> {
    let host = config.host.trim_start_matches('[').trim_end_matches(']');
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| SubmitError::Invalid(format!("{host} is not a valid host name")))?;
    within(
        config.timeout,
        TlsConnector::from(tls).connect(name, stream),
    )
    .await
    .map_err(|e| SubmitError::Unavailable(format!("TLS with {host}: {e}")))
}

fn header_section(message: &[u8]) -> &[u8] {
    message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(message, |end| &message[..end])
}

fn dot_stuff(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + message.len() / 64);
    for line in message.split_inclusive(|b| *b == b'\n') {
        if line.first() == Some(&b'.') {
            out.push(b'.');
        }
        out.extend_from_slice(line);
    }
    out
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn stuffs_dots_at_line_starts_only() {
        assert_eq!(
            dot_stuff(b".a\r\nb.c\r\n.\r\n..\r\n"),
            b"..a\r\nb.c\r\n..\r\n...\r\n"
        );
    }

    #[test]
    fn reads_extensions() {
        let lines: Vec<String> = [
            "mx.example.org Hello",
            "SIZE 1000",
            "8BITMIME",
            "AUTH plain LOGIN XOAUTH2",
            "STARTTLS",
        ]
        .map(String::from)
        .into();
        let ext = Extensions::parse(&lines);
        assert_eq!(ext.size, Some(1000));
        assert!(ext.eight_bit && ext.starttls && !ext.smtputf8);
        assert_eq!(ext.auth, ["PLAIN", "LOGIN", "XOAUTH2"]);
    }

    #[test]
    fn finds_the_header_section() {
        assert_eq!(header_section(b"A: b\r\n\r\nbody\r\n"), b"A: b");
        assert_eq!(header_section(b"A: b\r\n"), b"A: b\r\n");
    }
}
