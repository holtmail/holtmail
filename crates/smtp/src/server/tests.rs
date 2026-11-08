use std::net::SocketAddr;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use holtmail_core::tls::testing::TestCa;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

use super::*;

const SECRET: &str = "hm_invented";

#[derive(Debug, PartialEq, Eq)]
struct Device(&'static str);

#[derive(Default)]
struct Auth {
    outcome: Mutex<Option<AuthError>>,
    ended: AsyncMutex<Option<oneshot::Receiver<()>>>,
    calls: Mutex<u32>,
}

#[async_trait]
impl SmtpAuth for Auth {
    async fn verify(
        &self,
        login: &str,
        secret: &str,
        _peer: IpAddr,
    ) -> Result<SignedIn, AuthError> {
        *self.calls.lock().unwrap() += 1;
        if let Some(error) = *self.outcome.lock().unwrap() {
            return Err(error);
        }
        if login != "ada@example.com" || secret != SECRET {
            return Err(AuthError::Invalid);
        }
        let mut signed_in = SignedIn::new(Principal::new(Device("phone")));
        if let Some(receiver) = self.ended.lock().await.take() {
            signed_in.ended = Some(Box::pin(async move {
                let _ = receiver.await;
            }));
        }
        Ok(signed_in)
    }
}

#[derive(Default)]
struct Recorder {
    limits: Mutex<Option<Limits>>,
    refuse_from: Mutex<Option<Refused>>,
    froms: Mutex<Vec<String>>,
    principals: Mutex<Vec<bool>>,
    delivered: Mutex<Vec<(Envelope, Bytes)>>,
}

impl Recorder {
    fn saw(&self, session: &Session) {
        let device = session.principal.as_ref().and_then(|p| p.get::<Device>());
        self.principals
            .lock()
            .unwrap()
            .push(device == Some(&Device("phone")));
    }
}

#[async_trait]
impl Handler for Recorder {
    async fn limits(&self, _session: &Session) -> Option<Limits> {
        *self.limits.lock().unwrap()
    }

    async fn mail_from(&self, session: &Session, address: &str) -> Result<(), Refused> {
        self.saw(session);
        self.froms.lock().unwrap().push(address.to_owned());
        match self.refuse_from.lock().unwrap().clone() {
            Some(refused) => Err(refused),
            None => Ok(()),
        }
    }

    async fn recipient(&self, session: &Session, address: &str) -> Result<(), Refused> {
        self.saw(session);
        if address.starts_with("nobody") {
            return Err(Refused::new(550, "5.1.1", "no\r\nsuch"));
        }
        Ok(())
    }

    async fn deliver(
        &self,
        session: &Session,
        envelope: &Envelope,
        data: Bytes,
    ) -> Result<(), Refused> {
        self.saw(session);
        self.delivered
            .lock()
            .unwrap()
            .push((envelope.clone(), data));
        Ok(())
    }
}

async fn start(options: Options) -> (SocketAddr, Arc<Recorder>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let recorder = Arc::new(Recorder::default());
    tokio::spawn(serve(
        listener,
        Arc::new(options),
        recorder.clone(),
        Arc::new(|_| true),
    ));
    (address, recorder)
}

fn options() -> Options {
    Options::new("smtp.example.org")
}

fn with_auth() -> (Options, Arc<Auth>) {
    let auth = Arc::new(Auth::default());
    let mut options = options();
    options.auth = Some(auth.clone());
    (options, auth)
}

fn plain(authorize: &str, login: &str, secret: &str) -> String {
    format!(
        "AUTH PLAIN {}",
        STANDARD.encode(format!("{authorize}\0{login}\0{secret}"))
    )
}

struct Client<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    async fn reply(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            if !matches!(self.io.read_line(&mut line).await, Ok(n) if n > 0) {
                return lines;
            }
            let line = line.trim_end().to_owned();
            let last = line.as_bytes().get(3) != Some(&b'-');
            lines.push(line);
            if last {
                return lines;
            }
        }
    }

    async fn send(&mut self, raw: &[u8]) {
        self.io.get_mut().write_all(raw).await.unwrap();
    }

    async fn say(&mut self, command: &str) -> Vec<String> {
        self.send(format!("{command}\r\n").as_bytes()).await;
        self.reply().await
    }

    async fn code(&mut self, command: &str) -> String {
        self.say(command).await.last().unwrap()[..3].to_owned()
    }
}

async fn open(address: SocketAddr) -> (Client<TcpStream>, String) {
    let mut client = Client {
        io: BufReader::new(TcpStream::connect(address).await.unwrap()),
    };
    let greeting = client.reply().await.into_iter().next().unwrap_or_default();
    (client, greeting)
}

async fn hello(address: SocketAddr) -> Client<TcpStream> {
    let (mut client, _) = open(address).await;
    assert_eq!(client.code("EHLO client.example.com").await, "250");
    client
}

async fn tls_over(ca: &TestCa, tcp: TcpStream) -> TlsStream<TcpStream> {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    for der in ca.roots().iter() {
        roots.add(der.to_vec().into()).unwrap();
    }
    let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("smtp.example.org").unwrap(), tcp)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_principal_reaches_every_step() {
    let (options, _) = with_auth();
    let (address, recorder) = start(options).await;
    let mut c = hello(address).await;
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "530");
    assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "235");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(c.code("RCPT TO:<bo@example.org>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"Subject: hi\r\n\r\nhello\r\n.\r\n").await;
    assert_eq!(c.reply().await[0], "250 2.0.0 OK: queued");

    assert_eq!(*recorder.principals.lock().unwrap(), [true, true, true]);
    let delivered = recorder.delivered.lock().unwrap();
    let (envelope, data) = &delivered[0];
    assert_eq!(
        envelope.principal.as_ref().unwrap().get::<Device>(),
        Some(&Device("phone"))
    );
    assert_eq!(envelope.mail_from, "ada@example.com");
    assert_eq!(&data[..], b"Subject: hi\r\n\r\nhello\r\n");
}

#[tokio::test]
async fn mail_from_can_refuse_with_its_own_code() {
    let (address, recorder) = start(options()).await;
    let mut c = hello(address).await;
    *recorder.refuse_from.lock().unwrap() = Some(Refused::new(553, "5.7.1", "not your address"));
    assert_eq!(c.say("MAIL FROM:<>").await, ["553 5.7.1 not your address"]);
    assert_eq!(c.code("RCPT TO:<bo@example.org>").await, "503");
    *recorder.refuse_from.lock().unwrap() = Some(Refused::new(451, "4.7.1", "later"));
    assert_eq!(
        c.say("MAIL FROM:<ada@example.com>").await,
        ["451 4.7.1 later"]
    );
    *recorder.refuse_from.lock().unwrap() = None;
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(
        *recorder.froms.lock().unwrap(),
        ["", "ada@example.com", "ada@example.com"]
    );
}

#[tokio::test]
async fn a_non_ascii_path_is_a_syntax_error() {
    let (address, _) = start(options()).await;
    let mut c = hello(address).await;
    assert_eq!(c.code("MAIL ééé").await, "501");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(c.code("RCPT ééé").await, "501");
    assert_eq!(c.code("RCPT TO:<bo@example.org>").await, "250");
}

#[test]
fn a_refusal_never_reads_as_success() {
    assert_eq!(Refused::new(250, "2.0.0", "fine").line(), "451 4.3.0 fine");
    assert_eq!(
        Refused::new(550, "5.1.1", "a\r\n250 b").line(),
        "550 5.1.1 a  250 b"
    );
}

#[tokio::test]
async fn implicit_tls_starts_with_the_handshake() {
    let ca = TestCa::new(&["smtp.example.org"]);
    let (mut options, _) = with_auth();
    options.tls = Some(ca.acceptor());
    options.implicit_tls = true;
    let (address, recorder) = start(options).await;

    let tcp = TcpStream::connect(address).await.unwrap();
    let mut c = Client {
        io: BufReader::new(tls_over(&ca, tcp).await),
    };
    assert!(c.reply().await[0].starts_with("220 smtp.example.org"));
    let ehlo = c.say("EHLO client.example.com").await;
    assert!(ehlo.iter().all(|l| !l.contains("STARTTLS")));
    assert!(ehlo.iter().any(|l| l.ends_with("AUTH PLAIN LOGIN")));
    assert_eq!(c.code("STARTTLS").await, "503");
    assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "235");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(c.code("RCPT TO:<bo@example.org>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"Subject: tls\r\n\r\nhi\r\n.\r\n").await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "250");
    assert!(recorder.delivered.lock().unwrap()[0].0.tls);
}

#[tokio::test]
async fn implicit_tls_without_a_certificate_serves_nothing() {
    let mut options = options();
    options.implicit_tls = true;
    let (address, _) = start(options).await;
    assert_eq!(open(address).await.1, "");
}

#[tokio::test]
async fn starttls_forgets_the_sign_in() {
    let ca = TestCa::new(&["smtp.example.org"]);
    let (mut options, _) = with_auth();
    options.tls = Some(ca.acceptor());
    let (address, _) = start(options).await;
    let mut c = hello(address).await;
    assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "235");
    assert_eq!(c.code("STARTTLS").await, "220");
    let mut c = Client {
        io: BufReader::new(tls_over(&ca, c.io.into_inner()).await),
    };
    assert_eq!(c.code("EHLO client.example.com").await, "250");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "530");
}

#[tokio::test]
async fn pipelined_commands_answer_in_order() {
    let (address, recorder) = start(options()).await;
    let (mut c, _) = open(address).await;
    let ehlo = c.say("EHLO client.example.com").await;
    for keyword in ["PIPELINING", "8BITMIME", "ENHANCEDSTATUSCODES", "SMTPUTF8"] {
        assert!(ehlo.iter().any(|l| l.ends_with(keyword)), "{keyword}");
    }
    c.send(
        b"MAIL FROM:<ada@example.com> BODY=8BITMIME\r\n\
          RCPT TO:<bo@example.org>\r\n\
          RCPT TO:<nobody@example.org>\r\n\
          RCPT TO:<cy@example.org>\r\n\
          DATA\r\n",
    )
    .await;
    assert_eq!(c.reply().await, ["250 2.1.0 OK"]);
    assert_eq!(c.reply().await, ["250 2.1.5 OK"]);
    assert_eq!(c.reply().await, ["550 5.1.1 no  such"]);
    assert_eq!(c.reply().await, ["250 2.1.5 OK"]);
    assert_eq!(c.reply().await[0][..3].to_owned(), "354");
    c.send(b"Subject: batch\r\n\r\n8\xc3\xa9\r\n.\r\nRSET\r\nNOOP\r\n")
        .await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "250");
    assert_eq!(c.reply().await, ["250 2.0.0 OK"]);
    assert_eq!(c.reply().await, ["250 2.0.0 OK"]);

    let delivered = recorder.delivered.lock().unwrap();
    assert_eq!(
        delivered[0].0.recipients,
        ["bo@example.org", "cy@example.org"]
    );
    assert_eq!(&delivered[0].1[..], b"Subject: batch\r\n\r\n8\xc3\xa9\r\n");
}

#[tokio::test]
async fn pipelined_data_with_no_good_recipient_gets_no_go_ahead() {
    let (address, _) = start(options()).await;
    let mut c = hello(address).await;
    c.send(b"MAIL FROM:<ada@example.com>\r\nRCPT TO:<nobody@example.org>\r\nDATA\r\n")
        .await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "250");
    assert_eq!(c.reply().await[0][..3].to_owned(), "550");
    assert_eq!(c.reply().await[0][..3].to_owned(), "554");
}

#[tokio::test]
async fn a_held_reply_goes_out_when_a_command_is_incomplete() {
    let (address, _) = start(options()).await;
    let mut c = hello(address).await;
    c.send(b"NOOP\r\nNO").await;
    assert_eq!(c.reply().await, ["250 2.0.0 OK"]);
    c.send(b"OP\r\n").await;
    assert_eq!(c.reply().await, ["250 2.0.0 OK"]);
}

#[tokio::test]
async fn the_greeting_carries_the_instance_token() {
    let mut with_token = options();
    with_token.instance_token = Some("a1-b_2 \r\nx".into());
    let (address, _) = start(with_token).await;
    assert_eq!(
        open(address).await.1,
        "220 smtp.example.org Holtmail ESMTP holtmail-instance=a1-b_2x"
    );
    let (address, _) = start(options()).await;
    assert_eq!(open(address).await.1, "220 smtp.example.org Holtmail ESMTP");
}

#[tokio::test]
async fn limits_follow_the_handler_and_the_options_cap_them() {
    let (address, recorder) = start(options()).await;
    *recorder.limits.lock().unwrap() = Some(Limits {
        max_size: 40,
        smtputf8: false,
    });
    let (mut c, _) = open(address).await;
    let ehlo = c.say("EHLO client.example.com").await;
    assert!(ehlo.contains(&"250-SIZE 40".to_owned()));
    assert!(ehlo.iter().all(|l| !l.contains("SMTPUTF8")));
    assert_eq!(c.code("MAIL FROM:<ada@example.com> SIZE=41").await, "552");
    assert_eq!(c.code("MAIL FROM:<ada@example.com> SMTPUTF8").await, "555");
    assert_eq!(
        c.code("MAIL FROM:<ada@example.com> BODY=BINARYMIME").await,
        "555"
    );
    assert_eq!(c.code("MAIL FROM:<zoë@example.com>").await, "553");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(c.code("RCPT TO:<zoë@example.org>").await, "553");
    assert_eq!(c.code("RCPT TO:<bo@example.org>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(format!("Subject: big\r\n\r\n{}\r\n.\r\n", "x".repeat(60)).as_bytes())
        .await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "552");
    assert!(recorder.delivered.lock().unwrap().is_empty());

    *recorder.limits.lock().unwrap() = Some(Limits {
        max_size: usize::MAX,
        smtputf8: true,
    });
    let ehlo = c.say("EHLO client.example.com").await;
    assert!(ehlo.contains(&format!("250-SIZE {}", 50 << 20)));
    assert!(ehlo.iter().any(|l| l.ends_with("SMTPUTF8")));
}

#[tokio::test]
async fn plain_refuses_acting_as_someone_else() {
    let (options, auth) = with_auth();
    let (address, _) = start(options).await;
    let mut c = hello(address).await;
    assert_eq!(
        c.code(&plain("bo@example.org", "ada@example.com", SECRET))
            .await,
        "535"
    );
    assert_eq!(*auth.calls.lock().unwrap(), 0);
    assert_eq!(
        c.code(&plain("ada@example.com", "ada@example.com", SECRET))
            .await,
        "235"
    );
}

#[tokio::test]
async fn cancelling_a_sign_in_gets_one_answer_and_no_failure() {
    let (options, _) = with_auth();
    let (address, _) = start(options).await;
    let mut c = hello(address).await;
    for _ in 0..3 {
        assert_eq!(c.code("AUTH LOGIN").await, "334");
        assert_eq!(c.code("*").await, "501");
    }
    assert_eq!(c.code("NOOP").await, "250");
}

#[tokio::test]
async fn sign_in_failures_map_to_their_replies() {
    let (options, auth) = with_auth();
    let (address, _) = start(options).await;
    let mut c = hello(address).await;
    *auth.outcome.lock().unwrap() = Some(AuthError::Unavailable);
    for _ in 0..4 {
        assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "454");
    }
    *auth.outcome.lock().unwrap() = Some(AuthError::TooMany);
    assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "421");
    assert!(c.reply().await.is_empty());

    *auth.outcome.lock().unwrap() = None;
    let mut c = hello(address).await;
    assert_eq!(c.code(&plain("", "ada@example.com", "wrong")).await, "535");
    assert_eq!(c.code(&plain("", "ada@example.com", "wrong")).await, "535");
    assert_eq!(c.code(&plain("", "ada@example.com", "wrong")).await, "421");
    assert!(c.reply().await.is_empty());
}

#[tokio::test]
async fn an_ended_sign_in_closes_the_session() {
    let (options, auth) = with_auth();
    let (end, ended) = oneshot::channel();
    *auth.ended.lock().await = Some(ended);
    let (address, _) = start(options).await;
    let mut c = hello(address).await;
    assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "235");
    assert_eq!(c.code("NOOP").await, "250");
    end.send(()).unwrap();
    assert_eq!(c.reply().await, ["421 4.7.0 Signed out, closing"]);
    assert!(c.reply().await.is_empty());
}

#[tokio::test]
async fn waiting_to_sign_in_has_its_own_timeout() {
    let (mut options, _) = with_auth();
    options.sign_in_timeout = Some(Duration::from_millis(100));
    let (address, _) = start(options).await;
    let mut c = hello(address).await;
    assert!(c.reply().await[0].starts_with("421 4.4.2"));
}

#[tokio::test]
async fn a_connection_from_a_proxy_keeps_its_address_and_tls() {
    let (options, _) = with_auth();
    let options = Arc::new(options);
    let recorder = Arc::new(Recorder::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (dropped, slot_gone) = oneshot::channel::<()>();
    let server = {
        let (options, recorder) = (options.clone(), recorder.clone());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let connection = Connection {
                stream: Box::new(stream),
                peer: "::ffff:203.0.113.9".parse().unwrap(),
                tls_by_proxy: true,
                until_signed_in: Some(Box::new(dropped)),
            };
            handle(connection, &options, &*recorder).await;
        })
    };
    let mut c = hello(address).await;
    let mut slot_gone = slot_gone;
    assert!(slot_gone.try_recv().is_err());
    assert_eq!(c.code(&plain("", "ada@example.com", SECRET)).await, "235");
    assert!(slot_gone.await.is_err(), "released at sign-in");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(c.code("RCPT TO:<bo@example.org>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"Subject: proxied\r\n\r\nhi\r\n.\r\n").await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "250");
    assert_eq!(c.code("QUIT").await, "221");
    server.await.unwrap();
    let delivered = recorder.delivered.lock().unwrap();
    assert!(delivered[0].0.tls);
    assert_eq!(
        delivered[0].0.peer,
        "203.0.113.9".parse::<IpAddr>().unwrap()
    );
}

#[tokio::test]
async fn the_simple_receiver_gets_pipelining_too() {
    #[derive(Default)]
    struct Taker;
    #[async_trait]
    impl crate::Handler for Taker {
        async fn recipient(&self, _address: &str) -> crate::Recipient {
            crate::Recipient::Accept
        }
        async fn deliver(
            &self,
            _envelope: &crate::Envelope,
            _data: Bytes,
        ) -> Result<(), crate::Refusal> {
            Err(crate::Refusal::Permanent("no".into()))
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(crate::serve(
        listener,
        Arc::new(crate::Options::new("smtp.example.org")),
        Arc::new(Taker),
        Arc::new(|_| true),
    ));
    let (mut c, _) = open(address).await;
    let ehlo = c.say("EHLO client.example.com").await;
    assert!(ehlo.iter().any(|l| l.ends_with("PIPELINING")));
    assert_eq!(c.code("MAIL FROM:<>").await, "250");
    assert_eq!(c.code("RCPT TO:<a@example.org>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"\r\n.\r\n").await;
    assert_eq!(c.reply().await, ["554 5.6.0 no"]);
}
