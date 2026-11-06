use std::sync::Mutex;

use holtmail_core::tls::testing::TestCa;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;

use super::*;

#[derive(Default)]
struct Recorder {
    delivered: Mutex<Vec<(Envelope, Bytes)>>,
    refuse: Mutex<Option<Refusal>>,
}

#[async_trait]
impl Handler for Recorder {
    async fn recipient(&self, address: &str) -> Recipient {
        if address
            .to_ascii_lowercase()
            .ends_with("@archive.example.com")
        {
            Recipient::Accept
        } else {
            Recipient::Reject("no such mailbox here".into())
        }
    }

    async fn deliver(&self, envelope: &Envelope, data: Bytes) -> Result<(), Refusal> {
        if let Some(refusal) = self.refuse.lock().unwrap().clone() {
            return Err(refusal);
        }
        self.delivered
            .lock()
            .unwrap()
            .push((envelope.clone(), data));
        Ok(())
    }
}

async fn start(options: Options, allowed: bool) -> (SocketAddr, Arc<Recorder>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let recorder = Arc::new(Recorder::default());
    tokio::spawn(serve(
        listener,
        Arc::new(options),
        recorder.clone(),
        Arc::new(move |_| allowed),
    ));
    (address, recorder)
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
        let _ = self.io.get_mut().write_all(raw).await;
    }

    async fn say(&mut self, command: &str) -> Vec<String> {
        self.send(format!("{command}\r\n").as_bytes()).await;
        self.reply().await
    }

    async fn code(&mut self, command: &str) -> String {
        self.say(command).await.last().cloned().unwrap_or_default()[..3].to_owned()
    }
}

async fn connect(address: SocketAddr) -> Client<TcpStream> {
    let mut client = Client {
        io: BufReader::new(TcpStream::connect(address).await.unwrap()),
    };
    assert!(client.reply().await[0].starts_with("220 mx.example.org"));
    client
}

#[tokio::test]
async fn takes_mail_byte_for_byte() {
    let (address, recorder) = start(Options::new("mx.example.org"), true).await;
    let mut c = connect(address).await;
    let ehlo = c.say("EHLO sender.example.com").await;
    assert_eq!(ehlo[0], "250-mx.example.org Hello sender.example.com");
    assert!(ehlo.contains(&format!("250-SIZE {}", 50 << 20)));
    assert!(
        ehlo.iter()
            .all(|l| !l.contains("STARTTLS") && !l.contains("AUTH"))
    );
    assert_eq!(c.code("RCPT TO:<a@archive.example.com>").await, "503");
    assert_eq!(
        c.code("MAIL FROM:<ada@example.com> BODY=8BITMIME").await,
        "250"
    );
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "503");
    assert_eq!(c.code("RCPT TO:<journal@archive.example.com>").await, "250");
    assert_eq!(
        c.code("RCPT TO:<someone@elsewhere.example.net>").await,
        "550"
    );
    assert_eq!(c.code("RCPT TO:<Copy@Archive.example.com>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"Subject: Dots\r\n\r\n..leading dot\r\nsmuggle\n.\nnot the end\r\n.\r\n")
        .await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "250");
    assert_eq!(c.code("QUIT").await, "221");

    let delivered = recorder.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    let (envelope, data) = &delivered[0];
    assert_eq!(
        &data[..],
        b"Subject: Dots\r\n\r\n.leading dot\r\nsmuggle\n.\nnot the end\r\n"
    );
    assert_eq!(envelope.mail_from, "ada@example.com");
    assert_eq!(envelope.helo, "sender.example.com");
    assert_eq!(
        envelope.recipients,
        ["journal@archive.example.com", "Copy@Archive.example.com"]
    );
    assert!(!envelope.tls && !envelope.authenticated);
}

#[tokio::test]
async fn refuses_out_of_order_too_big_and_unknown() {
    let mut options = Options::new("mx.example.org");
    options.max_size = 100;
    let (address, recorder) = start(options, true).await;
    let mut c = connect(address).await;
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "503");
    assert_eq!(c.code("HELO sender.example.com").await, "250");
    assert_eq!(c.code("DATA").await, "503");
    assert_eq!(c.code("MAIL FROM:<ada@example.com> SIZE=5000").await, "552");
    assert_eq!(c.code("MAIL FROM:ada@example.com").await, "501");
    assert_eq!(c.code("MAIL FROM:<>").await, "250");
    assert_eq!(c.code("DATA").await, "554");
    assert_eq!(c.code("RCPT TO:<a@archive.example.com>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(format!("Subject: big\r\n\r\n{}\r\n.\r\n", "x".repeat(300)).as_bytes())
        .await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "552");
    assert_eq!(c.code("FROB").await, "500");
    assert_eq!(c.code("VRFY ada").await, "252");
    assert_eq!(c.code("RSET").await, "250");
    assert_eq!(c.code("MAIL FROM:<>").await, "250");
    assert_eq!(c.code("RCPT TO:<a@archive.example.com>").await, "250");
    *recorder.refuse.lock().unwrap() = Some(Refusal::Temporary("store busy".into()));
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"Subject: small\r\n\r\nhi\r\n.\r\n").await;
    assert_eq!(c.reply().await[0], "451 4.3.0 store busy");
    assert!(recorder.delivered.lock().unwrap().is_empty());
    for _ in 0..8 {
        c.say("FROB").await;
    }
    assert_eq!(
        c.reply().await,
        Vec::<String>::new(),
        "still open after ten errors"
    );
}

#[tokio::test]
async fn closes_on_refused_clients_and_silence() {
    let (address, _) = start(Options::new("mx.example.org"), false).await;
    let mut client = Client {
        io: BufReader::new(TcpStream::connect(address).await.unwrap()),
    };
    assert!(client.reply().await[0].starts_with("554 5.7.1"));
    assert!(client.reply().await.is_empty());

    let mut options = Options::new("mx.example.org");
    options.idle_timeout = Duration::from_millis(200);
    let (address, _) = start(options, true).await;
    let mut c = connect(address).await;
    assert!(c.reply().await[0].starts_with("421 4.4.2"));
}

#[tokio::test]
async fn signs_in_over_starttls() {
    let ca = TestCa::new(&["mx.example.org"]);
    let mut options = Options::new("mx.example.org");
    options.tls = Some(ca.acceptor());
    options.require_tls = true;
    options.credentials = Some(Credentials {
        username: "journal".into(),
        password: "harbour lights".into(),
    });
    let (address, recorder) = start(options, true).await;

    let mut c = connect(address).await;
    let ehlo = c.say("EHLO sender.example.com").await;
    assert!(ehlo.iter().any(|l| l.ends_with("STARTTLS")));
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "530");
    c.send(b"STARTTLS\r\nEHLO injected\r\n").await;
    assert_eq!(
        c.reply().await[0][..3].to_owned(),
        "503",
        "pipelined after STARTTLS"
    );

    let mut c = connect(address).await;
    c.say("EHLO sender.example.com").await;
    assert_eq!(c.code("STARTTLS").await, "220");
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
    let tls = TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("mx.example.org").unwrap(),
            c.io.into_inner(),
        )
        .await
        .unwrap();
    let mut c = Client {
        io: BufReader::new(tls),
    };
    let ehlo = c.say("EHLO sender.example.com").await;
    assert!(ehlo.iter().all(|l| !l.contains("STARTTLS")));
    assert!(ehlo.iter().any(|l| l.ends_with("AUTH PLAIN LOGIN")));
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "530");
    let wrong = STANDARD_B64.encode(b"\0journal\0wrong");
    assert_eq!(c.code(&format!("AUTH PLAIN {wrong}")).await, "535");
    assert_eq!(c.code("AUTH LOGIN").await, "334");
    assert_eq!(c.code(&STANDARD_B64.encode("journal")).await, "334");
    assert_eq!(c.code(&STANDARD_B64.encode("harbour lights")).await, "235");
    assert_eq!(c.code("MAIL FROM:<ada@example.com>").await, "250");
    assert_eq!(c.code("RCPT TO:<j@archive.example.com>").await, "250");
    assert_eq!(c.code("DATA").await, "354");
    c.send(b"Subject: secure\r\n\r\nhi\r\n.\r\n").await;
    assert_eq!(c.reply().await[0][..3].to_owned(), "250");
    let delivered = recorder.delivered.lock().unwrap().clone();
    assert!(delivered[0].0.tls && delivered[0].0.authenticated);
}

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as STANDARD_B64;
