use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, OutboxId, UserId};
use holtmail_device::{NewSecret, RevocationFeed};
use holtmail_proxy_protocol::testing::encode;
use holtmail_proxy_protocol::{Addresses, Command, ProxyHeader};
use holtmail_serve_net::Limits as NetLimits;
use holtmail_submission::testing::{FakeSubmission, IDENTITY_ADDRESS, Setup, World};
use holtmail_submission::{SendIdentity, SubmissionLimits, Submitted};
use holtmail_submit::{Envelope as SubmitEnvelope, RecipientOutcome, SubmitError, Submitter};
use holtmail_submit_smtp::{Secret, Security, SmtpConfig, SmtpSubmitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::*;

const SECRET: &str = "hm_invented";
const TOKEN: &str = "test-instance";

struct Devices {
    principal: Device,
    feeds: Mutex<Vec<mpsc::UnboundedSender<DeviceId>>>,
    protocols: Mutex<Vec<DeviceProtocol>>,
}

struct Feed(mpsc::UnboundedReceiver<DeviceId>);

#[async_trait]
impl RevocationFeed for Feed {
    async fn next(&mut self) -> DeviceId {
        match self.0.recv().await {
            Some(id) => id,
            None => std::future::pending().await,
        }
    }
}

#[async_trait]
impl DeviceAuth for Devices {
    async fn verify(
        &self,
        login: &str,
        secret: &str,
        _peer: IpAddr,
        protocol: DeviceProtocol,
    ) -> Result<Device, DeviceAuthError> {
        self.protocols.lock().unwrap().push(protocol);
        if login != IDENTITY_ADDRESS || secret != SECRET {
            return Err(DeviceAuthError::Invalid);
        }
        Ok(self.principal.clone())
    }

    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError> {
        for feed in self.feeds.lock().unwrap().iter() {
            let _ = feed.send(device);
        }
        Ok(true)
    }

    async fn revoke_user(&self, _: UserId) -> Result<u32, DeviceAuthError> {
        Ok(0)
    }

    fn new_secret(&self) -> Result<NewSecret, DeviceAuthError> {
        Err(DeviceAuthError::Unavailable("not in tests".into()))
    }

    fn end_sessions(&self, device: DeviceId) {
        for feed in self.feeds.lock().unwrap().iter() {
            let _ = feed.send(device);
        }
    }

    fn revocations(&self) -> Box<dyn RevocationFeed> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.feeds.lock().unwrap().push(sender);
        Box::new(Feed(receiver))
    }
}

#[derive(Default)]
struct Seen {
    requests: Vec<Submit>,
    results: Vec<Submitted>,
}

struct Recording {
    inner: Arc<dyn Submission>,
    seen: Arc<Mutex<Seen>>,
}

#[async_trait]
impl Submission for Recording {
    async fn identities(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<SendIdentity>, SubmissionError> {
        self.inner.identities(actor, archive).await
    }

    async fn limits(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<SubmissionLimits, SubmissionError> {
        self.inner.limits(actor, archive).await
    }

    async fn sender(
        &self,
        actor: &Actor,
        archive: ArchiveId,
        address: &str,
    ) -> Result<SendIdentity, SubmissionError> {
        self.inner.sender(actor, archive, address).await
    }

    async fn queue(&self, request: Submit) -> Result<Submitted, SubmissionError> {
        self.seen.lock().unwrap().requests.push(request.clone());
        let result = self.inner.queue(request).await;
        if let Ok(submitted) = &result {
            self.seen.lock().unwrap().results.push(submitted.clone());
        }
        result
    }

    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.undo(actor, id).await
    }

    async fn release(
        &self,
        actor: &Actor,
        id: OutboxId,
        undo_window: Option<std::time::Duration>,
    ) -> Result<Submitted, SubmissionError> {
        self.inner.release(actor, id, undo_window).await
    }

    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.status(actor, id).await
    }
}

struct Server {
    port: u16,
    devices: Arc<Devices>,
    seen: Arc<Mutex<Seen>>,
    world: World,
}

fn limiter() -> ConnectionLimiter {
    ConnectionLimiter::new(NetLimits {
        total: 20,
        per_user: 5,
        per_address: 10,
    })
}

async fn start_on(
    setup: Setup,
    tls: PortTls,
    proxy: bool,
    options: impl FnOnce(&mut Settings),
) -> Server {
    start_with(setup, tls, proxy, options, None).await
}

async fn start_with(
    setup: Setup,
    tls: PortTls,
    proxy: bool,
    options: impl FnOnce(&mut Settings),
    scheduler: Option<Arc<dyn Scheduler>>,
) -> Server {
    let world = FakeSubmission::world(setup);
    let devices = Arc::new(Devices {
        principal: Device {
            user: world.sender.user,
            device: world.sender.device.unwrap(),
            mail_archive: Some(world.archive),
            archives: vec![world.archive],
            scopes: world.sender.scopes.clone(),
            implicit_seen: false,
        },
        feeds: Mutex::default(),
        protocols: Mutex::default(),
    });
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let submission = Arc::new(Recording {
        inner: world.submission.clone(),
        seen: seen.clone(),
    });
    let mut settings = Settings::new("mail.example.com");
    settings.instance_token = Some(TOKEN.into());
    options(&mut settings);
    let server = match scheduler {
        Some(scheduler) => {
            SubmitServer::with_scheduler(submission, devices.clone(), settings, scheduler)
        }
        None => SubmitServer::new(submission, devices.clone(), settings),
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let trusted = if proxy {
        TrustedProxies::parse(&["127.0.0.1"]).unwrap()
    } else {
        TrustedProxies::default()
    };
    tokio::spawn(server.serve(listener, tls, proxy, trusted, limiter()));
    Server {
        port,
        devices,
        seen,
        world,
    }
}

async fn start(setup: Setup) -> Server {
    start_on(setup, PortTls::Starttls(None), false, |_| {}).await
}

struct Client(BufReader<TcpStream>);

impl Client {
    async fn connect(port: u16) -> Self {
        let mut client = Self(BufReader::new(
            TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap(),
        ));
        let greeting = client.reply().await;
        assert!(greeting.starts_with("220 "), "{greeting}");
        client
    }

    async fn reply(&mut self) -> String {
        let mut all = String::new();
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout(Duration::from_secs(10), self.0.read_line(&mut line))
                .await
                .expect("a reply")
                .unwrap();
            if read == 0 {
                return all;
            }
            all.push_str(&line);
            if line.as_bytes().get(3) != Some(&b'-') {
                return all;
            }
        }
    }

    async fn send(&mut self, command: &str) -> String {
        self.0
            .get_mut()
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .unwrap();
        self.reply().await
    }

    async fn signed_in(port: u16) -> Self {
        let mut client = Self::connect(port).await;
        assert!(
            client
                .send("EHLO client.example.com")
                .await
                .contains("AUTH")
        );
        let plain = base64_plain();
        let answer = client.send(&format!("AUTH PLAIN {plain}")).await;
        assert!(answer.starts_with("235"), "{answer}");
        client
    }

    async fn data(&mut self, body: &str) -> String {
        let answer = self.send("DATA").await;
        assert!(answer.starts_with("354"), "{answer}");
        self.0.get_mut().write_all(body.as_bytes()).await.unwrap();
        self.send(".").await
    }

    async fn transaction(&mut self, to: &[&str], body: &str) -> String {
        let answer = self.send(&format!("MAIL FROM:<{IDENTITY_ADDRESS}>")).await;
        assert!(answer.starts_with("250"), "{answer}");
        for recipient in to {
            let answer = self.send(&format!("RCPT TO:<{recipient}>")).await;
            assert!(answer.starts_with("250"), "{answer}");
        }
        self.data(body).await
    }
}

fn base64_plain() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = format!("\0{IDENTITY_ADDRESS}\0{SECRET}").into_bytes();
    let mut out = String::new();
    for chunk in raw.chunks(3) {
        let a = u32::from(chunk[0]);
        let b = u32::from(*chunk.get(1).unwrap_or(&0));
        let c = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (a << 16) | (b << 8) | c;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn mail(id: &str, extra: &str) -> String {
    format!(
        "From: Ada <{IDENTITY_ADDRESS}>\r\nTo: bob@example.org\r\n\
         Date: Sun, 08 Nov 2026 10:00:00 +0000\r\nMessage-ID: <{id}@example.com>\r\n\
         Subject: Hello\r\n{extra}\r\nBody\r\n"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_with_bcc_removed_and_every_recipient_kept() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let answer = client
        .transaction(
            &["bob@example.org", "hidden@example.org"],
            &mail("first", "Bcc: hidden@example.org\r\n"),
        )
        .await;
    assert!(answer.starts_with("250"), "{answer}");
    let seen = server.seen.lock().unwrap();
    let request = &seen.requests[0];
    assert_eq!(
        request.recipients,
        ["bob@example.org", "hidden@example.org"]
    );
    assert_eq!(request.identity, server.world.identity);
    assert_eq!(request.undo_window, Some(Duration::ZERO));
    assert!(
        request
            .idempotency_key
            .as_deref()
            .unwrap()
            .starts_with("smtp:")
    );
    assert!(!String::from_utf8_lossy(&request.message).contains("hidden@"));
    assert_eq!(
        server.devices.protocols.lock().unwrap().as_slice(),
        [DeviceProtocol::Smtp]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adds_what_rfc_6409_lets_a_server_add() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let answer = client
        .transaction(
            &["bob@example.org"],
            &format!("From: {IDENTITY_ADDRESS}\r\nSubject: Bare\r\n\r\nBody\r\n"),
        )
        .await;
    assert!(answer.starts_with("250"), "{answer}");
    let seen = server.seen.lock().unwrap();
    let text = String::from_utf8_lossy(&seen.requests[0].message).into_owned();
    assert!(text.contains("\r\nDate: "), "{text}");
    assert!(text.contains("@example.com>\r\n"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("received:"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_a_lost_reply_is_one_message() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let body = mail("retried", "");
    assert!(
        client
            .transaction(&["bob@example.org"], &body)
            .await
            .starts_with("250")
    );
    let mut again = Client::signed_in(server.port).await;
    assert!(
        again
            .transaction(&["bob@example.org"], &body)
            .await
            .starts_with("250")
    );
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.results.len(), 2);
    assert_eq!(seen.results[0].id, seen.results[1].id);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_of_a_cancelled_message_is_refused_not_accepted() {
    let server = start(Setup::default()).await;
    let body = mail("cancelled", "");
    let mut client = Client::signed_in(server.port).await;
    assert!(
        client
            .transaction(&["bob@example.org"], &body)
            .await
            .starts_with("250")
    );
    let (id, actor) = {
        let seen = server.seen.lock().unwrap();
        (seen.results[0].id, seen.requests[0].actor.clone())
    };
    server.world.submission.undo(&actor, id).await.unwrap();
    let mut again = Client::signed_in(server.port).await;
    assert!(
        again
            .transaction(&["bob@example.org"], &body)
            .await
            .starts_with("554")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_message_to_other_recipients_is_another_transaction() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let body = mail("split", "");
    client.transaction(&["bob@example.org"], &body).await;
    client.transaction(&["carol@example.org"], &body).await;
    let seen = server.seen.lock().unwrap();
    assert_ne!(seen.results[0].id, seen.results[1].id);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_what_the_identity_does_not_allow() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let answer = client.send("MAIL FROM:<eve@example.org>").await;
    assert!(answer.starts_with("553 5.7.1"), "{answer}");
    let answer = client.send("MAIL FROM:<>").await;
    assert!(answer.starts_with("553 5.7.1"), "{answer}");
    let answer = client
        .transaction(
            &["bob@example.org"],
            "From: eve@example.org\r\nDate: Sun, 08 Nov 2026 10:00:00 +0000\r\n\
             Message-ID: <wrong@example.com>\r\n\r\nBody\r\n",
        )
        .await;
    assert!(answer.starts_with("554 5.7.1"), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_without_sign_in() {
    let server = start(Setup::default()).await;
    let mut client = Client::connect(server.port).await;
    client.send("EHLO client.example.com").await;
    let answer = client
        .send(&format!("MAIL FROM:<{IDENTITY_ADDRESS}>"))
        .await;
    assert!(answer.starts_with("530"), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn advertises_the_transports_limits_and_refuses_larger_mail() {
    let server = start(Setup {
        max_size: 2048,
        smtputf8: false,
        ..Setup::default()
    })
    .await;
    let mut client = Client::signed_in(server.port).await;
    let ehlo = client.send("EHLO client.example.com").await;
    assert!(ehlo.contains("SIZE 2048"), "{ehlo}");
    assert!(!ehlo.contains("SMTPUTF8"), "{ehlo}");
    let padding = format!("X-Padding: {}\r\n", "x".repeat(4096));
    let answer = client
        .transaction(&["bob@example.org"], &mail("big", &padding))
        .await;
    assert!(answer.starts_with("552"), "{answer}");
    assert!(server.seen.lock().unwrap().requests.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn offers_smtputf8_only_when_the_transports_do() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let ehlo = client.send("EHLO client.example.com").await;
    assert!(ehlo.contains("SMTPUTF8"), "{ehlo}");
    let mut before = Client::connect(server.port).await;
    let ehlo = before.send("EHLO client.example.com").await;
    assert!(!ehlo.contains("SMTPUTF8"), "before sign-in: {ehlo}");
    assert!(ehlo.contains(&format!("SIZE {MAX_SIZE}")), "{ehlo}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_used_up_limit_is_a_temporary_refusal() {
    let server = start(Setup {
        messages_per_hour: 1,
        ..Setup::default()
    })
    .await;
    let mut client = Client::signed_in(server.port).await;
    assert!(
        client
            .transaction(&["bob@example.org"], &mail("one", ""))
            .await
            .starts_with("250")
    );
    let answer = client
        .send(&format!("MAIL FROM:<{IDENTITY_ADDRESS}>"))
        .await;
    assert!(answer.starts_with("451 4.7.1"), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_leak_guard_refuses_and_names_nobody() {
    let server = start(Setup::default()).await;
    let reply = mail(
        "leak",
        &format!("In-Reply-To: <{}>\r\n", server.world.alias_thread),
    );
    let mut client = Client::signed_in(server.port).await;
    let recipient = server.world.leaking_recipient.clone();
    let answer = client.transaction(&[&recipient], &reply).await;
    assert!(answer.starts_with("550 5.7.1"), "{answer}");
    assert!(
        !answer.contains(&recipient) && !answer.contains(IDENTITY_ADDRESS),
        "{answer}"
    );
    assert!(!server.seen.lock().unwrap().requests[0].allow_leaks);
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_the_device_ends_the_session() {
    let server = start(Setup::default()).await;
    let mut client = Client::signed_in(server.port).await;
    let mut other = Client::signed_in(server.port).await;
    server
        .devices
        .revoke(server.world.sender.device.unwrap())
        .await
        .unwrap();
    let answer = client.reply().await;
    assert!(answer.starts_with("421"), "{answer}");
    let answer = other.reply().await;
    assert!(answer.starts_with("421"), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_per_user_connection_limit_applies_at_sign_in() {
    let server = start(Setup::default()).await;
    let mut clients = Vec::new();
    for _ in 0..5 {
        clients.push(Client::signed_in(server.port).await);
    }
    let mut extra = Client::connect(server.port).await;
    extra.send("EHLO client.example.com").await;
    let answer = extra.send(&format!("AUTH PLAIN {}", base64_plain())).await;
    assert!(answer.starts_with("421"), "{answer}");
}

fn proxy_header(port: u16) -> Vec<u8> {
    encode(&ProxyHeader {
        command: Command::Proxy,
        addresses: Some(Addresses {
            source: SocketAddr::from((Ipv4Addr::new(203, 0, 113, 9), 50000)),
            destination: SocketAddr::from((Ipv4Addr::new(198, 51, 100, 1), port)),
        }),
        ssl: None,
    })
}

async fn certificates(dir: &std::path::Path) -> (TlsCertificates, holtmail_core::TlsRoots) {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert, made.cert.pem()).unwrap();
    std::fs::write(&key, made.signing_key.serialize_pem()).unwrap();
    let roots = holtmail_core::TlsRoots::from_pem(made.cert.pem().as_bytes()).unwrap();
    (TlsCertificates::load(cert, key).await.unwrap(), roots)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_client_must_use_starttls_before_anything() {
    let dir = tempfile::tempdir().unwrap();
    let (certs, _) = certificates(dir.path()).await;
    let server = start_on(
        Setup::default(),
        PortTls::Starttls(Some(certs)),
        true,
        |_| {},
    )
    .await;
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port))
        .await
        .unwrap();
    stream.write_all(&proxy_header(587)).await.unwrap();
    let mut client = Client(BufReader::new(stream));
    assert!(client.reply().await.starts_with("220 "));
    let ehlo = client.send("EHLO client.example.com").await;
    assert!(ehlo.contains("STARTTLS"), "{ehlo}");
    assert!(!ehlo.contains("AUTH"), "{ehlo}");
    let answer = client
        .send(&format!("MAIL FROM:<{IDENTITY_ADDRESS}>"))
        .await;
    assert!(answer.starts_with("530"), "{answer}");
}

fn submitter(port: u16, security: Security) -> SmtpSubmitter {
    SmtpSubmitter::new(SmtpConfig {
        port,
        security,
        username: Some(IDENTITY_ADDRESS.into()),
        secret: Some(Secret::Password(SECRET.into())),
        helo: "client.example.com".into(),
        ..SmtpConfig::new("localhost")
    })
    .unwrap()
}

fn outgoing() -> (SubmitEnvelope, Vec<u8>) {
    (
        SubmitEnvelope {
            mail_from: IDENTITY_ADDRESS.into(),
            recipients: vec!["bob@example.org".into(), "carol@example.org".into()],
        },
        mail("loopback", "").into_bytes(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_smtp_sends_through_it_over_each_kind_of_port() {
    let dir = tempfile::tempdir().unwrap();
    let (certs, roots) = certificates(dir.path()).await;
    for (tls, security) in [
        (PortTls::Starttls(None), Security::None),
        (PortTls::Starttls(Some(certs.clone())), Security::Starttls),
        (PortTls::Implicit(certs.clone()), Security::Tls),
    ] {
        let server = start_on(Setup::default(), tls, false, |_| {}).await;
        let client = submitter(server.port, security).with_roots(&roots).unwrap();
        client.check().await.expect("a sign-in");
        let (envelope, bytes) = outgoing();
        let report = client.submit(&envelope, &bytes).await.expect("taken");
        assert!(
            report
                .recipients
                .iter()
                .all(|r| r.outcome == RecipientOutcome::Accepted)
        );
        assert_eq!(server.seen.lock().unwrap().results.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_smtp_refuses_its_own_instance() {
    let server = start(Setup::default()).await;
    let (envelope, bytes) = outgoing();
    let looping = submitter(server.port, Security::None).refusing_instance(TOKEN);
    let refused = looping.submit(&envelope, &bytes).await;
    assert!(
        matches!(refused, Err(SubmitError::Invalid(_))),
        "{refused:?}"
    );
    assert!(matches!(
        looping.check().await,
        Err(SubmitError::Invalid(_))
    ));
    assert!(
        server.seen.lock().unwrap().requests.is_empty(),
        "nothing was sent"
    );

    let another = submitter(server.port, Security::None).refusing_instance("another-instance");
    another
        .submit(&envelope, &bytes)
        .await
        .expect("a second instance is fine");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_undo_window_comes_from_the_settings() {
    let server = start_on(Setup::default(), PortTls::Starttls(None), false, |s| {
        s.undo_window = Duration::from_secs(30);
    })
    .await;
    let mut client = Client::signed_in(server.port).await;
    client
        .transaction(&["bob@example.org"], &mail("undo", ""))
        .await;
    assert_eq!(
        server.seen.lock().unwrap().requests[0].undo_window,
        Some(Duration::from_secs(30))
    );
}

struct JustSent(holtmail_schedule::ItipKey);

#[async_trait]
impl Scheduler for JustSent {
    async fn prepare(
        &self,
        _: &holtmail_schedule::ClientPut,
    ) -> Result<holtmail_schedule::Prepared, holtmail_schedule::ScheduleError> {
        Err(holtmail_schedule::ScheduleError::Invalid("unused".into()))
    }

    async fn deliver(
        &self,
        _: &holtmail_schedule::Change,
    ) -> Result<holtmail_schedule::Delivery, holtmail_schedule::ScheduleError> {
        Err(holtmail_schedule::ScheduleError::Invalid("unused".into()))
    }

    async fn confirm(
        &self,
        _: &Actor,
        _: &holtmail_schedule::Delivery,
    ) -> Result<(), holtmail_schedule::ScheduleError> {
        Ok(())
    }

    async fn withdraw(
        &self,
        _: &Actor,
        _: &holtmail_schedule::Delivery,
    ) -> Result<(), holtmail_schedule::ScheduleError> {
        Ok(())
    }

    async fn incoming(
        &self,
        _: &holtmail_schedule::IncomingItip,
    ) -> Result<holtmail_schedule::Incoming, holtmail_schedule::ScheduleError> {
        Err(holtmail_schedule::ScheduleError::Invalid("unused".into()))
    }

    async fn just_sent(
        &self,
        _: ArchiveId,
        key: &holtmail_schedule::ItipKey,
        recipients: &[String],
    ) -> Result<bool, holtmail_schedule::ScheduleError> {
        Ok(*key == self.0
            && !recipients.is_empty()
            && recipients
                .iter()
                .all(|r| r.eq_ignore_ascii_case("bob@example.org")))
    }
}

fn imip(id: &str, sequence: u32) -> String {
    mail(
        id,
        &format!(
            "MIME-Version: 1.0\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:lunch@example.com\r\nSEQUENCE:{sequence}\r\nDTSTAMP:20261108T100000Z\r\nDTSTART:20261110T120000Z\r\nORGANIZER:mailto:{IDENTITY_ADDRESS}\r\nATTENDEE:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR"
        ),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn drops_an_imip_message_the_scheduler_just_sent() {
    let scheduler = Arc::new(JustSent(holtmail_schedule::ItipKey {
        uid: "lunch@example.com".into(),
        sequence: 1,
        method: holtmail_schedule::ItipMethod::Request,
        recurrence_id: None,
        partstat: None,
    }));
    let server = start_with(
        Setup::default(),
        PortTls::Starttls(None),
        false,
        |_| {},
        Some(scheduler),
    )
    .await;
    let mut client = Client::signed_in(server.port).await;
    let answer = client
        .transaction(&["bob@example.org"], &imip("dup", 1))
        .await;
    assert!(answer.starts_with("250"), "{answer}");
    assert!(
        server.seen.lock().unwrap().requests.is_empty(),
        "the duplicate is accepted and dropped"
    );
    let answer = client
        .transaction(&["bob@example.org"], &imip("newer", 2))
        .await;
    assert!(answer.starts_with("250"), "{answer}");
    assert_eq!(server.seen.lock().unwrap().requests.len(), 1);
    let answer = client
        .transaction(&["carol@example.org"], &imip("carol", 1))
        .await;
    assert!(answer.starts_with("250"), "{answer}");
    assert_eq!(
        server.seen.lock().unwrap().requests.len(),
        2,
        "a recipient the scheduler did not tell gets the device's message"
    );
}
