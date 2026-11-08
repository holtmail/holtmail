use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use holtmail_submit::testing::{
    Behavior, Deliveries, Delivery, Fake, Profile, Receiver, conformance,
};
use holtmail_submit::{Envelope, RecipientOutcome, SubmitError, Submitter};
use mail_parser::MessageParser;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::{GraphConfig, GraphSubmitter, MAX_SIZE, StaticToken, TokenProvider};

const TOKEN: &str = "graph-send-token";
const ADA: &str = "ada@example.com";
const BOB: &str = "bob@example.org";
const CAROL: &str = "carol@example.org";

struct Request {
    method: String,
    path: String,
    authorization: String,
    content_type: String,
    body: Vec<u8>,
}

enum Action {
    Respond {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: String,
    },
    Close,
}

fn respond(status: u16, body: &str) -> Action {
    Action::Respond {
        status,
        headers: Vec::new(),
        body: body.into(),
    }
}

fn graph_error(status: u16, code: &str) -> Action {
    respond(
        status,
        &format!(r#"{{"error":{{"code":"{code}","message":"refused by the fake"}}}}"#),
    )
}

type Expect = fn(&SubmitError) -> bool;

type Handler = Arc<dyn Fn(&Request) -> Action + Send + Sync>;

struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn sent(&self) -> Vec<Vec<u8>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .map(|r| BASE64.decode(&r.body).expect("base64 body"))
            .collect()
    }

    fn config(&self) -> GraphConfig {
        GraphConfig {
            api_url: format!("http://127.0.0.1:{}/v1.0", self.port),
            mailbox: Some(ADA.into()),
            timeout: Duration::from_secs(10),
            ..GraphConfig::default()
        }
    }

    fn submitter(&self) -> GraphSubmitter {
        GraphSubmitter::new(self.config(), Arc::new(StaticToken::new(TOKEN))).expect("config")
    }
}

async fn read_request(stream: &mut BufReader<tokio::net::TcpStream>) -> Option<Request> {
    let mut line = String::new();
    stream.read_line(&mut line).await.ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let (mut length, mut authorization, mut content_type) = (0usize, String::new(), String::new());
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).await.ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        let value = value.trim().to_owned();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().ok()?,
            "authorization" => authorization = value,
            "content-type" => content_type = value,
            _ => {}
        }
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    Some(Request {
        method,
        path,
        authorization,
        content_type,
        body,
    })
}

async fn serve(handler: Handler) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let handler = handler.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let Some(request) = read_request(&mut stream).await else {
                    return;
                };
                let action = if request.authorization == format!("Bearer {TOKEN}") {
                    handler(&request)
                } else {
                    graph_error(401, "InvalidAuthenticationToken")
                };
                log.lock().unwrap().push(request);
                let Action::Respond {
                    status,
                    headers,
                    body,
                } = action
                else {
                    return;
                };
                let mut head = format!(
                    "HTTP/1.1 {status} Fake\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                head.push_str(&body);
                let _ = stream.get_mut().write_all(head.as_bytes()).await;
                let _ = stream.get_mut().shutdown().await;
            });
        }
    });
    Server { port, requests }
}

fn strip_bcc(mime: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(mime.len());
    let mut in_header = true;
    let mut skipping = false;
    for line in mime.split_inclusive(|&b| b == b'\n') {
        if in_header {
            if line == b"\r\n" {
                in_header = false;
            } else if line.starts_with(b" ") || line.starts_with(b"\t") {
                if skipping {
                    continue;
                }
            } else {
                skipping = line.len() >= 4 && line[..4].eq_ignore_ascii_case(b"bcc:");
                if skipping {
                    continue;
                }
            }
        }
        out.extend_from_slice(line);
    }
    out
}

fn header_recipients(mime: &[u8]) -> Vec<String> {
    let parsed = MessageParser::new().parse_headers(mime).expect("headers");
    let headers = parsed.headers();
    headers
        .all_to()
        .chain(headers.all_cc())
        .chain(headers.all_bcc())
        .flat_map(|list| {
            list.mailboxes()
                .filter_map(|m| m.address().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A fake `sendMail` that behaves like Graph: recipients from the header fields, the mailbox in
/// the path as the sender, and the `Bcc` field removed from what recipients get.
fn graph_handler(behavior: Behavior, deliveries: Deliveries) -> Handler {
    Arc::new(move |request: &Request| {
        if request.method == "GET" {
            return respond(200, r#"{"id":"sentitems-id"}"#);
        }
        let Some(mailbox) = request
            .path
            .strip_prefix("/v1.0/users/")
            .and_then(|rest| rest.strip_suffix("/sendMail"))
        else {
            return graph_error(404, "ResourceNotFound");
        };
        if request.content_type != "text/plain" {
            return graph_error(400, "ErrorMimeContentInvalid");
        }
        let Ok(mime) = BASE64.decode(&request.body) else {
            return graph_error(400, "ErrorMimeContentInvalidBase64String");
        };
        let recipients = header_recipients(&mime);
        let delivery = Delivery {
            mail_from: mailbox.to_owned(),
            recipients: recipients.clone(),
            bytes: strip_bcc(&mime),
        };
        match &behavior {
            Behavior::AcceptAll => {}
            Behavior::RefuseRecipient(refused) => {
                if recipients.iter().any(|r| r.eq_ignore_ascii_case(refused)) {
                    return graph_error(400, "ErrorInvalidRecipients");
                }
            }
            Behavior::TemporaryFailure => {
                return Action::Respond {
                    status: 503,
                    headers: vec![("Retry-After", "7".into())],
                    body: String::new(),
                };
            }
            Behavior::PermanentFailure => return graph_error(403, "ErrorSendAsDenied"),
            Behavior::DropAfterData => {
                deliveries.push(delivery);
                return Action::Close;
            }
            Behavior::SizeLimit(limit) => {
                if mime.len() as u64 > *limit {
                    return graph_error(413, "ErrorMessageSizeExceeded");
                }
            }
        }
        deliveries.push(delivery);
        respond(202, "")
    })
}

struct GraphReceiver;

#[async_trait]
impl Receiver for GraphReceiver {
    async fn start(&self, behavior: Behavior) -> Fake {
        let deliveries = Deliveries::default();
        let max_size = match behavior {
            Behavior::SizeLimit(limit) => limit,
            _ => MAX_SIZE,
        };
        let server = serve(graph_handler(behavior, deliveries.clone())).await;
        let config = GraphConfig {
            max_size,
            ..server.config()
        };
        Fake {
            submitter: Box::new(
                GraphSubmitter::new(config, Arc::new(StaticToken::new(TOKEN))).expect("config"),
            ),
            deliveries,
        }
    }

    fn profile(&self) -> Profile {
        Profile {
            return_path: false,
            per_recipient: false,
        }
    }
}

fn envelope(recipients: &[&str]) -> Envelope {
    Envelope {
        mail_from: ADA.into(),
        recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
    }
}

fn message() -> Vec<u8> {
    let mut bytes = b"From: Ada Example <ada@example.com>\r\n\
To: Bob Example <bob@example.org>\r\n\
Subject: Graph\r\n\
Message-ID: <graph@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\
\r\n\
.leading dot\r\n\
.\r\n\
 trailing space \r\n\
Gr\xc3\xbc\xc3\x9fe\r\n"
        .to_vec();
    for line in 0..200 {
        bytes.extend_from_slice(format!("line {line} of the body\r\n").as_bytes());
    }
    bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_submit_contract() {
    conformance(&GraphReceiver).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_base64_mime_and_keeps_the_bytes() {
    let fake = GraphReceiver.start(Behavior::AcceptAll).await;
    assert_eq!(
        fake.submitter.check().await.unwrap().max_size,
        Some(MAX_SIZE)
    );
    assert!(fake.deliveries.all().is_empty(), "check sent a message");

    let bytes = message();
    let report = fake
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap();
    assert_eq!(report.recipients.len(), 1);
    assert_eq!(report.recipients[0].outcome, RecipientOutcome::Accepted);
    let delivered = fake.deliveries.all();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].mail_from, ADA);
    assert_eq!(delivered[0].recipients, vec![BOB]);
    assert!(delivered[0].bytes == bytes, "bytes changed on the way");
}

#[tokio::test(flavor = "multi_thread")]
async fn hands_graph_envelope_only_recipients_as_bcc() {
    let deliveries = Deliveries::default();
    let server = serve(graph_handler(Behavior::AcceptAll, deliveries.clone())).await;
    let bytes = message();
    let report = server
        .submitter()
        .submit(&envelope(&[BOB, CAROL]), &bytes)
        .await
        .unwrap();
    assert_eq!(report.recipients[0].address, BOB);
    assert_eq!(report.recipients[1].address, CAROL);
    assert!(report.refused().next().is_none());

    let mut expected = format!("Bcc: {CAROL}\r\n").into_bytes();
    expected.extend_from_slice(&bytes);
    assert_eq!(server.sent(), vec![expected], "only a Bcc field is added");
    let delivered = deliveries.all();
    assert_eq!(delivered[0].recipients, vec![BOB, CAROL]);
    assert!(delivered[0].bytes == bytes);

    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0].path, "/v1.0/users/ada@example.com/sendMail");
    assert_eq!(requests[0].content_type, "text/plain");
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_from_me_without_a_mailbox() {
    let server = serve(Arc::new(|_: &Request| respond(202, ""))).await;
    let config = GraphConfig {
        mailbox: None,
        ..server.config()
    };
    let submitter = GraphSubmitter::new(config, Arc::new(StaticToken::new(TOKEN))).unwrap();
    submitter.check().await.unwrap();
    submitter
        .submit(&envelope(&[BOB]), &message())
        .await
        .unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests[0].path,
        "/v1.0/me/mailFolders/sentitems?$select=id"
    );
    assert_eq!(requests[1].path, "/v1.0/me/sendMail");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_invalid_input_before_sending() {
    let server = serve(Arc::new(|_: &Request| respond(202, ""))).await;
    let submitter = server.submitter();
    let bytes = message();
    let results = [
        submitter.submit(&envelope(&[]), &bytes).await,
        submitter.submit(&envelope(&["bob"]), &bytes).await,
        submitter
            .submit(&envelope(&[BOB]), b"Subject: x\n\nbare LF\n")
            .await,
        submitter.submit(&envelope(&[CAROL]), &bytes).await,
    ];
    for (at, result) in results.into_iter().enumerate() {
        assert!(
            matches!(result, Err(SubmitError::Invalid(_))),
            "input {at} gave {result:?}"
        );
    }
    assert_eq!(server.count(), 0, "invalid input was sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn maps_graph_refusals() {
    let cases: Vec<(Action, Expect)> = vec![
        (graph_error(401, "InvalidAuthenticationToken"), |e| {
            matches!(e, SubmitError::Auth(_))
        }),
        (graph_error(403, "ErrorAccessDenied"), |e| {
            matches!(e, SubmitError::Auth(_))
        }),
        (graph_error(403, "ErrorSendAsDenied"), |e| {
            matches!(
                e,
                SubmitError::Refused {
                    permanent: true,
                    code: 550,
                    ..
                }
            )
        }),
        (graph_error(413, "RequestEntityTooLarge"), |e| {
            *e == SubmitError::TooLarge { limit: None }
        }),
        (graph_error(400, "ErrorMessageSizeExceeded"), |e| {
            *e == SubmitError::TooLarge { limit: None }
        }),
        (graph_error(400, "ErrorInvalidRecipients"), |e| {
            matches!(
                e,
                SubmitError::Refused {
                    permanent: true,
                    code: 550,
                    ..
                }
            )
        }),
        (graph_error(400, "ErrorMimeContentInvalid"), |e| {
            matches!(
                e,
                SubmitError::Refused {
                    permanent: true,
                    code: 554,
                    ..
                }
            )
        }),
        (
            Action::Respond {
                status: 429,
                headers: vec![("Retry-After", "30".into())],
                body: String::new(),
            },
            |e| matches!(e, SubmitError::Temporary(text) if text.contains("retry after 30 s")),
        ),
        (graph_error(503, "ServiceUnavailable"), |e| {
            matches!(e, SubmitError::Temporary(_))
        }),
        (graph_error(500, "InternalServerError"), |e| {
            matches!(e, SubmitError::Uncertain(_))
        }),
        (graph_error(504, "GatewayTimeout"), |e| {
            matches!(e, SubmitError::Uncertain(_))
        }),
        (Action::Close, |e| matches!(e, SubmitError::Uncertain(_))),
    ];
    for (at, (action, expected)) in cases.into_iter().enumerate() {
        let action = Mutex::new(Some(action));
        let server = serve(Arc::new(move |_: &Request| {
            action
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| respond(500, ""))
        }))
        .await;
        let error = server
            .submitter()
            .submit(&envelope(&[BOB]), &message())
            .await
            .unwrap_err();
        assert!(expected(&error), "case {at} gave {error:?}");
        assert_eq!(server.count(), 1, "case {at} was retried");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn contract_mappings_for_the_suite_behaviors() {
    let bytes = message();
    let temporary = GraphReceiver.start(Behavior::TemporaryFailure).await;
    let error = temporary
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap_err();
    assert!(
        error.is_retryable() && !error.may_have_been_sent(),
        "{error:?}"
    );

    let permanent = GraphReceiver.start(Behavior::PermanentFailure).await;
    let error = permanent
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SubmitError::Refused {
            permanent: true,
            code: 500..600,
            ..
        }
    ));

    let refused = GraphReceiver
        .start(Behavior::RefuseRecipient(CAROL.into()))
        .await;
    let error = refused
        .submitter
        .submit(&envelope(&[BOB, CAROL]), &bytes)
        .await
        .unwrap_err();
    assert!(
        !error.is_retryable() && !error.may_have_been_sent(),
        "{error:?}"
    );
    assert!(refused.deliveries.all().is_empty());

    let dropped = GraphReceiver.start(Behavior::DropAfterData).await;
    let error = dropped
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap_err();
    assert!(
        error.may_have_been_sent() && !error.is_retryable(),
        "{error:?}"
    );
    let delivered = dropped.deliveries.all();
    assert_eq!(delivered.len(), 1, "the submitter retried on its own");
    assert!(delivered[0].bytes == bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn enforces_the_size_limit_before_sending() {
    let bytes = message();
    let limit = bytes.len() as u64 - 1;
    let fake = GraphReceiver.start(Behavior::SizeLimit(limit)).await;
    let limits = fake.submitter.check().await.unwrap();
    assert_eq!(limits.max_size, Some(limit));
    assert_eq!(fake.submitter.limits(), limits);
    let error = fake
        .submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap_err();
    assert_eq!(error, SubmitError::TooLarge { limit: Some(limit) });
    assert!(fake.deliveries.all().is_empty());
    fake.submitter
        .submit(&envelope(&[BOB]), b"Subject: Small\r\n\r\nHi\r\n")
        .await
        .unwrap();
    assert_eq!(fake.deliveries.all().len(), 1);

    let server = serve(Arc::new(|_: &Request| respond(202, ""))).await;
    let config = GraphConfig {
        max_size: u64::MAX,
        ..server.config()
    };
    let capped = GraphSubmitter::new(config, Arc::new(StaticToken::new(TOKEN))).unwrap();
    assert_eq!(capped.limits().max_size, Some(MAX_SIZE));
}

struct Rotating {
    calls: AtomicUsize,
}

#[async_trait]
impl TokenProvider for Rotating {
    async fn token(&self) -> Result<String, SubmitError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok("stale-token".into()),
            1 => Ok(TOKEN.into()),
            _ => Err(SubmitError::Auth("the sign-in was revoked".into())),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn asks_the_token_provider_on_every_call() {
    let server = serve(Arc::new(|_: &Request| respond(202, ""))).await;
    let tokens = Arc::new(Rotating {
        calls: AtomicUsize::new(0),
    });
    let submitter = GraphSubmitter::new(server.config(), tokens).unwrap();
    let bytes = message();
    let first = submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap_err();
    assert!(matches!(first, SubmitError::Auth(_)), "{first:?}");
    submitter.submit(&envelope(&[BOB]), &bytes).await.unwrap();
    let revoked = submitter
        .submit(&envelope(&[BOB]), &bytes)
        .await
        .unwrap_err();
    assert_eq!(revoked, SubmitError::Auth("the sign-in was revoked".into()));
    assert_eq!(server.count(), 2, "a request went out without a token");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_maps_sign_in_and_reachability() {
    let server = serve(Arc::new(|_: &Request| {
        graph_error(403, "ErrorAccessDenied")
    }))
    .await;
    assert!(matches!(
        server.submitter().check().await,
        Err(SubmitError::Auth(_))
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = GraphConfig {
        api_url: format!("http://127.0.0.1:{port}/v1.0/"),
        timeout: Duration::from_secs(5),
        ..GraphConfig::default()
    };
    let submitter = GraphSubmitter::new(config, Arc::new(StaticToken::new(TOKEN))).unwrap();
    assert!(matches!(
        submitter.check().await,
        Err(SubmitError::Unavailable(_))
    ));
    let error = submitter
        .submit(&envelope(&[BOB]), &message())
        .await
        .unwrap_err();
    assert!(matches!(error, SubmitError::Unavailable(_)), "{error:?}");
}

#[test]
fn refuses_unsafe_configuration() {
    let tokens = || -> Arc<dyn TokenProvider> { Arc::new(StaticToken::new(TOKEN)) };
    for (api_url, mailbox) in [
        ("http://graph.example.com/v1.0/", None),
        ("ftp://127.0.0.1/", None),
        ("not a url", None),
        ("https://graph.example.com/v1.0/", Some("")),
        ("https://graph.example.com/v1.0/", Some("ada @example.com")),
    ] {
        let config = GraphConfig {
            api_url: api_url.into(),
            mailbox: mailbox.map(str::to_owned),
            ..GraphConfig::default()
        };
        assert!(
            matches!(
                GraphSubmitter::new(config, tokens()),
                Err(SubmitError::Invalid(_))
            ),
            "{api_url} {mailbox:?} was accepted"
        );
    }
    assert!(GraphSubmitter::new(GraphConfig::default(), tokens()).is_ok());
    assert_eq!(format!("{:?}", StaticToken::new(TOKEN)), "StaticToken(..)");
}
