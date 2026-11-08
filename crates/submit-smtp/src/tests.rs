use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use holtmail_core::tls::testing::TestCa;
use holtmail_smtp::{Credentials, Handler, Options, Recipient, Refusal};
use holtmail_submit::testing::{Behavior, Deliveries, Delivery, Fake, Receiver, conformance};
use holtmail_submit::{Envelope, RecipientOutcome, SubmitError, Submitter};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::{Secret, Security, SmtpConfig, SmtpSubmitter};

const MESSAGE: &[u8] =
    b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Hi\r\n\r\nHello\r\n";

fn envelope() -> Envelope {
    Envelope {
        mail_from: "ada@example.com".into(),
        recipients: vec!["bob@example.org".into()],
    }
}

fn plain(port: u16) -> SmtpConfig {
    SmtpConfig {
        port,
        security: Security::None,
        timeout: Duration::from_secs(5),
        data_timeout: Duration::from_secs(5),
        ..SmtpConfig::new("127.0.0.1")
    }
}

fn submitter(config: SmtpConfig) -> SmtpSubmitter {
    SmtpSubmitter::new(config).expect("config")
}

struct Inbox {
    behavior: Behavior,
    deliveries: Deliveries,
}

#[async_trait]
impl Handler for Inbox {
    async fn recipient(&self, address: &str) -> Recipient {
        match &self.behavior {
            Behavior::RefuseRecipient(refused) if refused == address => {
                Recipient::Reject("no such user".into())
            }
            _ => Recipient::Accept,
        }
    }

    async fn deliver(
        &self,
        envelope: &holtmail_smtp::Envelope,
        data: Bytes,
    ) -> Result<(), Refusal> {
        match self.behavior {
            Behavior::TemporaryFailure => return Err(Refusal::Temporary("try later".into())),
            Behavior::PermanentFailure => return Err(Refusal::Permanent("rejected".into())),
            _ => {}
        }
        self.deliveries.push(Delivery {
            mail_from: envelope.mail_from.clone(),
            recipients: envelope.recipients.clone(),
            bytes: data.to_vec(),
        });
        Ok(())
    }
}

async fn serve_smtp(options: Options, handler: Arc<dyn Handler>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    tokio::spawn(holtmail_smtp::serve(
        listener,
        Arc::new(options),
        handler,
        Arc::new(|_: IpAddr| true),
    ));
    port
}

async fn serve_inbox(options: Options, behavior: Behavior) -> (u16, Deliveries) {
    let deliveries = Deliveries::default();
    let handler = Arc::new(Inbox {
        behavior,
        deliveries: deliveries.clone(),
    });
    (serve_smtp(options, handler).await, deliveries)
}

#[derive(Clone, Copy)]
enum OnDot {
    Reply(&'static str),
    Drop,
    Hang,
}

#[derive(Clone)]
struct Script {
    greeting: &'static str,
    extensions: Vec<&'static str>,
    login: Option<(&'static str, &'static str)>,
    xoauth2: Option<String>,
    close_on_data: bool,
    on_dot: OnDot,
    tls: Option<TlsAcceptor>,
}

impl Script {
    fn new() -> Self {
        Self {
            greeting: "220 fake.example.org ESMTP",
            extensions: vec!["8BITMIME"],
            login: None,
            xoauth2: None,
            close_on_data: false,
            on_dot: OnDot::Reply("250 2.0.0 queued"),
            tls: None,
        }
    }
}

#[derive(Clone, Default)]
struct Seen {
    deliveries: Deliveries,
    commands: Arc<Mutex<Vec<String>>>,
}

impl Seen {
    fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("commands").clone()
    }
}

async fn serve_script(script: Script) -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    let seen = Seen::default();
    let shared = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let script = script.clone();
            let seen = shared.clone();
            tokio::spawn(async move {
                match script.tls.clone() {
                    Some(acceptor) => {
                        if let Ok(tls) = acceptor.accept(tcp).await {
                            run_script(tls, &script, &seen).await;
                        }
                    }
                    None => run_script(tcp, &script, &seen).await,
                }
            });
        }
    });
    (port, seen)
}

async fn run_script<S: AsyncRead + AsyncWrite + Unpin>(stream: S, script: &Script, seen: &Seen) {
    let mut io = BufReader::new(stream);
    let _ = run_session(&mut io, script, seen).await;
}

async fn read_line<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut BufReader<S>,
) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    if io.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()))
}

async fn say<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut BufReader<S>,
    line: &str,
) -> std::io::Result<()> {
    io.get_mut()
        .write_all(format!("{line}\r\n").as_bytes())
        .await?;
    io.get_mut().flush().await
}

async fn run_session<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut BufReader<S>,
    script: &Script,
    seen: &Seen,
) -> std::io::Result<()> {
    say(io, script.greeting).await?;
    let mut from = String::new();
    let mut recipients = Vec::new();
    while let Some(line) = read_line(io).await? {
        let upper = line.to_ascii_uppercase();
        seen.commands.lock().expect("commands").push(line.clone());
        if upper.starts_with("EHLO") {
            let mut lines = vec!["fake.example.org".to_owned()];
            lines.extend(script.extensions.iter().map(|e| (*e).to_owned()));
            let last = lines.len() - 1;
            for (at, text) in lines.iter().enumerate() {
                let sep = if at == last { ' ' } else { '-' };
                say(io, &format!("250{sep}{text}")).await?;
            }
        } else if upper == "AUTH LOGIN" {
            let (user, pass) = script.login.unwrap_or_default();
            say(io, "334 VXNlcm5hbWU6").await?;
            let given_user = read_line(io).await?.unwrap_or_default();
            say(io, "334 UGFzc3dvcmQ6").await?;
            let given_pass = read_line(io).await?.unwrap_or_default();
            if given_user == BASE64.encode(user) && given_pass == BASE64.encode(pass) {
                say(io, "235 2.7.0 OK").await?;
            } else {
                say(io, "535 5.7.8 bad credentials").await?;
            }
        } else if let Some(initial) = line.strip_prefix("AUTH XOAUTH2 ") {
            if script.xoauth2.as_deref() == Some(initial) {
                say(io, "235 2.7.0 OK").await?;
            } else {
                say(io, "334 eyJzdGF0dXMiOiI0MDEifQ==").await?;
                read_line(io).await?;
                say(io, "535 5.7.8 bad token").await?;
            }
        } else if upper.starts_with("MAIL FROM:") {
            from = line[11..line.find('>').unwrap_or(11)].to_owned();
            say(io, "250 2.1.0 OK").await?;
        } else if upper.starts_with("RCPT TO:") {
            recipients.push(line[9..line.find('>').unwrap_or(9)].to_owned());
            say(io, "250 2.1.5 OK").await?;
        } else if upper == "DATA" {
            if script.close_on_data {
                return Ok(());
            }
            say(io, "354 go ahead").await?;
            let mut bytes = Vec::new();
            loop {
                let mut raw = Vec::new();
                if io.read_until(b'\n', &mut raw).await? == 0 {
                    return Ok(());
                }
                if raw == b".\r\n" {
                    break;
                }
                bytes.extend_from_slice(raw.strip_prefix(b".").unwrap_or(&raw));
            }
            seen.deliveries.push(Delivery {
                mail_from: std::mem::take(&mut from),
                recipients: std::mem::take(&mut recipients),
                bytes,
            });
            match script.on_dot {
                OnDot::Reply(text) => say(io, text).await?,
                OnDot::Drop => return Ok(()),
                OnDot::Hang => {
                    let mut rest = Vec::new();
                    io.read_to_end(&mut rest).await?;
                    return Ok(());
                }
            }
        } else if upper == "QUIT" {
            say(io, "221 2.0.0 bye").await?;
            return Ok(());
        } else if upper == "RSET" {
            say(io, "250 2.0.0 OK").await?;
        } else {
            say(io, "500 5.5.2 what").await?;
        }
    }
    Ok(())
}

struct SmtpReceiver;

#[async_trait]
impl Receiver for SmtpReceiver {
    async fn start(&self, behavior: Behavior) -> Fake {
        if behavior == Behavior::DropAfterData {
            let (port, seen) = serve_script(Script {
                on_dot: OnDot::Drop,
                ..Script::new()
            })
            .await;
            return Fake {
                submitter: Box::new(submitter(plain(port))),
                deliveries: seen.deliveries,
            };
        }
        let mut options = Options::new("mx.example.org");
        if let Behavior::SizeLimit(limit) = behavior {
            options.max_size = limit as usize;
        }
        let (port, deliveries) = serve_inbox(options, behavior).await;
        Fake {
            submitter: Box::new(submitter(plain(port))),
            deliveries,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_submit_contract() {
    conformance(&SmtpReceiver).await;
}

#[tokio::test]
async fn starttls_and_auth_plain_with_a_test_ca() {
    let ca = TestCa::new(&["localhost"]);
    let mut options = Options::new("mx.example.org");
    options.tls = Some(ca.acceptor());
    options.require_tls = true;
    options.credentials = Some(Credentials {
        username: "ada".into(),
        password: "correct horse".into(),
    });
    let (port, deliveries) = serve_inbox(options, Behavior::AcceptAll).await;
    let config = |password: &str| SmtpConfig {
        port,
        security: Security::Starttls,
        username: Some("ada".into()),
        secret: Some(Secret::Password(password.into())),
        timeout: Duration::from_secs(5),
        ..SmtpConfig::new("localhost")
    };

    let trusted = submitter(config("correct horse"))
        .with_roots(&ca.roots())
        .unwrap();
    trusted.check().await.expect("check");
    trusted.submit(&envelope(), MESSAGE).await.expect("submit");
    assert_eq!(deliveries.all()[0].bytes, MESSAGE);

    let wrong = submitter(config("wrong")).with_roots(&ca.roots()).unwrap();
    assert!(matches!(wrong.check().await, Err(SubmitError::Auth(_))));

    let untrusted = submitter(config("correct horse"));
    assert!(matches!(
        untrusted.check().await,
        Err(SubmitError::Unavailable(_))
    ));
    let other = TestCa::new(&["localhost"]);
    let wrong_ca = submitter(config("correct horse"))
        .with_roots(&other.roots())
        .unwrap();
    assert!(matches!(
        wrong_ca.submit(&envelope(), MESSAGE).await,
        Err(SubmitError::Unavailable(_))
    ));
    assert_eq!(deliveries.all().len(), 1);
}

#[tokio::test]
async fn refuses_a_server_without_starttls() {
    let (port, _) = serve_inbox(Options::new("mx.example.org"), Behavior::AcceptAll).await;
    let config = SmtpConfig {
        port,
        security: Security::Starttls,
        timeout: Duration::from_secs(5),
        ..SmtpConfig::new("localhost")
    };
    assert!(matches!(
        submitter(config).check().await,
        Err(SubmitError::Unavailable(_))
    ));
}

#[tokio::test]
async fn implicit_tls() {
    let ca = TestCa::new(&["localhost"]);
    let (port, seen) = serve_script(Script {
        tls: Some(ca.acceptor()),
        ..Script::new()
    })
    .await;
    let config = SmtpConfig {
        port,
        security: Security::Tls,
        timeout: Duration::from_secs(5),
        ..SmtpConfig::new("localhost")
    };
    let client = submitter(config).with_roots(&ca.roots()).unwrap();
    client.submit(&envelope(), MESSAGE).await.expect("submit");
    assert_eq!(seen.deliveries.all()[0].bytes, MESSAGE);
}

#[tokio::test]
async fn auth_login_when_plain_is_not_offered() {
    let (port, seen) = serve_script(Script {
        extensions: vec!["AUTH LOGIN"],
        login: Some(("ada", "secret")),
        ..Script::new()
    })
    .await;
    let config = |password: &str| SmtpConfig {
        username: Some("ada".into()),
        secret: Some(Secret::Password(password.into())),
        ..plain(port)
    };
    submitter(config("secret")).check().await.expect("login");
    assert!(matches!(
        submitter(config("nope")).check().await,
        Err(SubmitError::Auth(_))
    ));
    assert!(seen.commands().iter().any(|c| c == "AUTH LOGIN"));
}

#[tokio::test]
async fn auth_xoauth2() {
    let expected = BASE64.encode("user=ada@example.com\x01auth=Bearer token-1\x01\x01");
    let (port, _) = serve_script(Script {
        extensions: vec!["AUTH PLAIN XOAUTH2"],
        xoauth2: Some(expected),
        ..Script::new()
    })
    .await;
    let config = |token: &str| SmtpConfig {
        username: Some("ada@example.com".into()),
        secret: Some(Secret::OAuthToken(token.into())),
        ..plain(port)
    };
    submitter(config("token-1"))
        .submit(&envelope(), MESSAGE)
        .await
        .expect("xoauth2");
    assert!(matches!(
        submitter(config("expired")).check().await,
        Err(SubmitError::Auth(_))
    ));

    let (port, _) = serve_script(Script {
        extensions: vec!["AUTH PLAIN"],
        ..Script::new()
    })
    .await;
    let config = SmtpConfig {
        username: Some("ada@example.com".into()),
        secret: Some(Secret::OAuthToken("token-1".into())),
        ..plain(port)
    };
    assert!(matches!(
        submitter(config).check().await,
        Err(SubmitError::Auth(_))
    ));
}

#[tokio::test]
async fn maps_failures_around_the_session() {
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = unused.local_addr().unwrap().port();
    drop(unused);
    assert!(matches!(
        submitter(plain(port)).check().await,
        Err(SubmitError::Unavailable(_))
    ));

    let (port, _) = serve_script(Script {
        greeting: "421 4.3.2 shutting down",
        ..Script::new()
    })
    .await;
    assert!(matches!(
        submitter(plain(port)).submit(&envelope(), MESSAGE).await,
        Err(SubmitError::Unavailable(_))
    ));

    let (port, seen) = serve_script(Script {
        close_on_data: true,
        ..Script::new()
    })
    .await;
    let error = submitter(plain(port))
        .submit(&envelope(), MESSAGE)
        .await
        .unwrap_err();
    assert!(matches!(error, SubmitError::Temporary(_)), "{error:?}");
    assert!(seen.deliveries.all().is_empty());
}

#[tokio::test]
async fn silence_after_the_message_is_uncertain() {
    let (port, seen) = serve_script(Script {
        on_dot: OnDot::Hang,
        ..Script::new()
    })
    .await;
    let config = SmtpConfig {
        data_timeout: Duration::from_millis(200),
        ..plain(port)
    };
    let error = submitter(config)
        .submit(&envelope(), MESSAGE)
        .await
        .unwrap_err();
    assert!(matches!(error, SubmitError::Uncertain(_)), "{error:?}");
    assert_eq!(seen.deliveries.all().len(), 1);
}

#[tokio::test]
async fn too_large_after_the_message_without_size() {
    let (port, _) = serve_script(Script {
        on_dot: OnDot::Reply("552 5.3.4 too big"),
        ..Script::new()
    })
    .await;
    assert_eq!(
        submitter(plain(port)).submit(&envelope(), MESSAGE).await,
        Err(SubmitError::TooLarge { limit: None })
    );
}

#[tokio::test]
async fn needs_8bitmime_and_smtputf8_for_such_mail() {
    let eight_bit = "Subject: Hi\r\n\r\nGr\u{fc}\u{df}e\r\n".as_bytes();
    let international = "Subject: Gr\u{fc}\u{df}e\r\n\r\nHi\r\n".as_bytes();

    let (port, _) = serve_script(Script {
        extensions: vec![],
        ..Script::new()
    })
    .await;
    let bare = submitter(plain(port));
    assert!(matches!(
        bare.submit(&envelope(), eight_bit).await,
        Err(SubmitError::Invalid(_))
    ));
    assert!(matches!(
        bare.submit(&envelope(), international).await,
        Err(SubmitError::Invalid(_))
    ));
    bare.submit(&envelope(), MESSAGE).await.expect("7-bit");

    let (port, seen) = serve_script(Script {
        extensions: vec!["8BITMIME", "SMTPUTF8", "SIZE 100000"],
        ..Script::new()
    })
    .await;
    let full = submitter(plain(port));
    full.submit(&envelope(), eight_bit).await.expect("8-bit");
    let utf8 = Envelope {
        recipients: vec!["j\u{f6}rg@example.org".into()],
        ..envelope()
    };
    full.submit(&utf8, international).await.expect("SMTPUTF8");
    let mails: Vec<_> = seen
        .commands()
        .into_iter()
        .filter(|c| c.starts_with("MAIL"))
        .collect();
    assert_eq!(
        mails[0],
        format!(
            "MAIL FROM:<ada@example.com> SIZE={} BODY=8BITMIME",
            eight_bit.len()
        )
    );
    assert!(mails[1].ends_with(" SMTPUTF8"), "{}", mails[1]);
    assert_eq!(
        seen.deliveries.all()[1].recipients,
        ["j\u{f6}rg@example.org"]
    );
}

#[tokio::test]
async fn reports_each_recipient_and_limits() {
    let mut options = Options::new("mx.example.org");
    options.max_size = 4096;
    let (port, _) = serve_inbox(
        options,
        Behavior::RefuseRecipient("nobody@example.org".into()),
    )
    .await;
    let client = submitter(plain(port));
    assert_eq!(client.limits().max_size, None);
    let both = Envelope {
        recipients: vec!["nobody@example.org".into(), "bob@example.org".into()],
        ..envelope()
    };
    let report = client.submit(&both, MESSAGE).await.expect("submit");
    assert_eq!(client.limits().max_size, Some(4096));
    let RecipientOutcome::Refused(refusal) = &report.recipients[0].outcome else {
        panic!("not refused");
    };
    assert_eq!(refusal.code, 550);
    assert!(refusal.permanent && refusal.text.starts_with("5.1.1"));
    assert_eq!(report.recipients[1].outcome, RecipientOutcome::Accepted);
}

#[test]
fn refuses_unsafe_configs_and_hides_secrets() {
    let open = SmtpConfig {
        security: Security::None,
        ..SmtpConfig::new("smtp.example.org")
    };
    assert!(matches!(
        SmtpSubmitter::new(open),
        Err(SubmitError::Invalid(_))
    ));
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let local = SmtpConfig {
            security: Security::None,
            ..SmtpConfig::new(host)
        };
        assert!(SmtpSubmitter::new(local).is_ok(), "{host}");
    }
    let half = SmtpConfig {
        username: Some("ada".into()),
        ..SmtpConfig::new("smtp.example.org")
    };
    assert!(SmtpSubmitter::new(half).is_err());
    let bad_helo = SmtpConfig {
        helo: "a b".into(),
        ..SmtpConfig::new("smtp.example.org")
    };
    assert!(SmtpSubmitter::new(bad_helo).is_err());

    let config = SmtpConfig {
        username: Some("ada".into()),
        secret: Some(Secret::Password("hunter2".into())),
        ..SmtpConfig::new("smtp.example.org")
    };
    assert!(!format!("{config:?}").contains("hunter2"));
    let token = Secret::OAuthToken("ya29.token".into());
    assert!(!format!("{token:?}").contains("ya29"));
}
