use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{DeviceId, DeviceProtocol, UserId};
use holtmail_device::{DeviceAuth, DeviceAuthError, NewSecret, Principal, RevocationFeed};
use holtmail_mailview::Viewer;
use holtmail_mailview::testing::{FakeMailView, Scenario, Setup, World};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream, ReadBuf,
    ReadHalf, WriteHalf,
};
use tokio::sync::{Notify, mpsc};

use crate::conn::Wire;
use crate::{ChangeListener, ChangeSignal, Settings, Shared};

pub(super) const EDITOR: &str = "ada@example.com";
pub(super) const READER: &str = "ada+phone@example.com";
pub(super) const SECRET: &str = "hm_invented";
pub(super) const TOKEN: &str = "instance-token";

pub(super) struct TestWire {
    inner: DuplexStream,
    pub(super) tls: bool,
    pub(super) can_tls: bool,
    pub(super) loopback: bool,
}

impl AsyncRead for TestWire {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TestWire {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[async_trait]
impl Wire for TestWire {
    fn can_start_tls(&self) -> bool {
        self.can_tls && !self.tls
    }

    async fn start_tls(&mut self) -> bool {
        self.tls = true;
        self.can_tls = false;
        true
    }

    fn sign_in_allowed(&self) -> bool {
        self.tls || self.loopback
    }

    fn client(&self) -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    fn authenticated(&mut self, _user: &UserId) -> bool {
        true
    }
}

pub(super) struct Devices {
    principals: Vec<(&'static str, Principal)>,
    feeds: Mutex<Vec<mpsc::UnboundedSender<DeviceId>>>,
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
    ) -> Result<Principal, DeviceAuthError> {
        assert_eq!(protocol, DeviceProtocol::Imap);
        if secret != SECRET {
            return Err(DeviceAuthError::Invalid);
        }
        self.principals
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(login))
            .map(|(_, principal)| principal.clone())
            .ok_or(DeviceAuthError::Invalid)
    }

    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError> {
        self.end_sessions(device);
        Ok(true)
    }

    async fn revoke_user(&self, _: UserId) -> Result<u32, DeviceAuthError> {
        Ok(0)
    }

    fn end_sessions(&self, device: DeviceId) {
        for feed in self.feeds.lock().unwrap().iter() {
            let _ = feed.send(device);
        }
    }

    fn new_secret(&self) -> Result<NewSecret, DeviceAuthError> {
        Err(DeviceAuthError::Unavailable("not in tests".into()))
    }

    fn revocations(&self) -> Box<dyn RevocationFeed> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.feeds.lock().unwrap().push(sender);
        Box::new(Feed(receiver))
    }
}

#[derive(Default)]
pub(super) struct Signal {
    notify: Arc<Notify>,
}

impl Signal {
    pub(super) fn wake(&self) {
        self.notify.notify_waiters();
    }
}

struct Listener(Arc<Notify>);

#[async_trait]
impl ChangeListener for Listener {
    async fn changed(&mut self) -> Option<u64> {
        self.0.notified().await;
        Some(0)
    }
}

impl ChangeSignal for Signal {
    fn listen(&self, _archive: holtmail_core::ArchiveId, _user: UserId) -> Box<dyn ChangeListener> {
        Box::new(Listener(self.notify.clone()))
    }
}

pub(super) fn principal(viewer: &Viewer, implicit_seen: bool) -> Principal {
    Principal {
        user: viewer.user,
        device: viewer.device.expect("a device"),
        mail_archive: Some(viewer.archive),
        archives: vec![viewer.archive],
        scopes: viewer.scopes.clone(),
        implicit_seen,
    }
}

/// An instance over the fake mail view, and how to reach it.
pub(super) struct Instance {
    pub(super) world: World,
    pub(super) shared: Arc<Shared>,
    pub(super) devices: Arc<Devices>,
    pub(super) signal: Arc<Signal>,
}

pub(super) struct Options {
    pub(super) scenario: Scenario,
    pub(super) implicit_seen: bool,
    pub(super) settings: Settings,
}

impl Default for Options {
    fn default() -> Self {
        let settings = Settings {
            instance_token: Some(TOKEN.into()),
            ..Settings::default()
        };
        Self {
            scenario: Scenario::two_accounts(),
            implicit_seen: false,
            settings,
        }
    }
}

impl Instance {
    pub(super) fn new() -> Self {
        Self::with(Options::default())
    }

    pub(super) fn with(options: Options) -> Self {
        let world = FakeMailView::world(&options.scenario, Setup::default());
        let devices = Arc::new(Devices {
            principals: vec![
                (EDITOR, principal(&world.editor, options.implicit_seen)),
                (READER, principal(&world.reader, options.implicit_seen)),
            ],
            feeds: Mutex::new(Vec::new()),
        });
        let signal = Arc::new(Signal::default());
        let permits = options.settings.response_bytes;
        let shared = Arc::new(Shared {
            view: world.view.clone(),
            actions: world.actions.clone(),
            devices: devices.clone(),
            signal: signal.clone(),
            settings: options.settings,
            bytes: Arc::new(tokio::sync::Semaphore::new(permits)),
        });
        Self {
            world,
            shared,
            devices,
            signal,
        }
    }

    /// A client on a loopback connection, after the greeting.
    pub(super) async fn connect(&self) -> Client {
        self.connect_with(true, false).await
    }

    pub(super) async fn connect_with(&self, loopback: bool, can_tls: bool) -> Client {
        let (client, server) = tokio::io::duplex(1 << 20);
        let wire = TestWire {
            inner: server,
            tls: false,
            can_tls,
            loopback,
        };
        tokio::spawn(crate::session::run(self.shared.clone(), wire));
        let (read, write) = tokio::io::split(client);
        let mut client = Client {
            read: BufReader::new(read),
            write,
            greeting: String::new(),
        };
        client.greeting = client.line().await;
        client
    }
}

pub(super) struct Client {
    read: BufReader<ReadHalf<DuplexStream>>,
    write: WriteHalf<DuplexStream>,
    pub(super) greeting: String,
}

const WAIT: Duration = Duration::from_secs(5);

impl Client {
    pub(super) async fn line(&mut self) -> String {
        let mut line = String::new();
        tokio::time::timeout(WAIT, self.read.read_line(&mut line))
            .await
            .expect("the server answers in time")
            .expect("readable");
        line
    }

    pub(super) async fn send(&mut self, text: &str) {
        self.write.write_all(text.as_bytes()).await.unwrap();
        self.write.write_all(b"\r\n").await.unwrap();
    }

    pub(super) async fn send_raw(&mut self, bytes: &[u8]) {
        self.write.write_all(bytes).await.unwrap();
    }

    /// Reads until the tagged line of `tag`, and returns everything read.
    pub(super) async fn until(&mut self, tag: &str) -> String {
        let mut all = String::new();
        loop {
            let line = self.line().await;
            assert!(!line.is_empty(), "connection closed; got so far:\n{all}");
            all.push_str(&line);
            if line.starts_with(&format!("{tag} ")) {
                return all;
            }
        }
    }

    /// Sends a command (`tag` first) and returns the whole answer.
    pub(super) async fn run(&mut self, command: &str) -> String {
        let tag = command.split(' ').next().unwrap().to_string();
        self.send(command).await;
        self.until(&tag).await
    }

    pub(super) async fn login(&mut self, login: &str) {
        let answer = self.run(&format!("l1 LOGIN {login} {SECRET}")).await;
        assert!(answer.contains("l1 OK"), "{answer}");
    }

    /// Whether the server closed the connection, reading what is left.
    pub(super) async fn closed(&mut self) -> String {
        let mut all = String::new();
        loop {
            let line = self.line().await;
            if line.is_empty() {
                return all;
            }
            all.push_str(&line);
        }
    }
}

/// The tagged status of an answer.
pub(super) fn status(answer: &str) -> &str {
    answer.lines().last().unwrap_or_default()
}

/// The UID the answer gave in `[APPENDUID v uid]` or as the last number of `[COPYUID ...]`.
pub(super) fn uid_code(answer: &str, code: &str) -> u32 {
    let start = answer.find(&format!("[{code} ")).expect(code) + code.len() + 2;
    let end = start + answer[start..].find(']').unwrap();
    answer[start..end]
        .split(' ')
        .next_back()
        .unwrap()
        .split([',', ':'])
        .next_back()
        .unwrap()
        .parse()
        .unwrap()
}
