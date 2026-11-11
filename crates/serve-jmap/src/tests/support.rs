use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use holtmail_blob::{BlobError, BlobId, BlobStore};
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, OutboxId, UserId};
use holtmail_device::{DeviceAuth, DeviceAuthError, NewSecret, Principal, RevocationFeed};
use holtmail_mailview::testing::{FakeMailView, Scenario, Setup, World};
use holtmail_submission::testing::{Driver as SendDriver, FakeSubmission, Setup as SendSetup};
use holtmail_submission::{
    Actor, SendIdentity, Submission, SubmissionError, SubmissionLimits, Submit, Submitted,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tower::ServiceExt;

use crate::{Hub, JmapServer, Services, Settings, Wake};

pub const USING: [&str; 3] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
];

#[derive(Default)]
pub struct TestAuth {
    devices: Mutex<HashMap<(String, String), Principal>>,
    revoked: Mutex<HashSet<DeviceId>>,
    feeds: Mutex<Vec<mpsc::UnboundedSender<DeviceId>>>,
}

impl TestAuth {
    fn add(&self, login: &str, secret: &str, principal: Principal) {
        self.devices
            .lock()
            .unwrap()
            .insert((login.to_string(), secret.to_string()), principal);
    }
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
impl DeviceAuth for TestAuth {
    async fn verify(
        &self,
        login: &str,
        secret: &str,
        _peer: IpAddr,
        protocol: DeviceProtocol,
    ) -> Result<Principal, DeviceAuthError> {
        let found = self
            .devices
            .lock()
            .unwrap()
            .get(&(login.to_ascii_lowercase(), secret.to_string()))
            .cloned();
        match found {
            Some(p) if self.revoked.lock().unwrap().contains(&p.device) => {
                Err(DeviceAuthError::Revoked)
            }
            Some(p) if protocol == DeviceProtocol::Jmap => Ok(p),
            _ => Err(DeviceAuthError::Invalid),
        }
    }

    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError> {
        let fresh = self.revoked.lock().unwrap().insert(device);
        self.end_sessions(device);
        Ok(fresh)
    }

    async fn revoke_user(&self, _user: UserId) -> Result<u32, DeviceAuthError> {
        Ok(0)
    }

    fn end_sessions(&self, device: DeviceId) {
        self.feeds
            .lock()
            .unwrap()
            .retain(|feed| feed.send(device).is_ok());
    }

    fn new_secret(&self) -> Result<NewSecret, DeviceAuthError> {
        Err(DeviceAuthError::Unavailable("not in tests".into()))
    }

    fn revocations(&self) -> Box<dyn RevocationFeed> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.feeds.lock().unwrap().push(tx);
        Box::new(Feed(rx))
    }
}

/// The fake submission speaks for its own user and archive; this maps the mail view's owner
/// and archive onto them.
struct Mapped {
    inner: Arc<dyn Submission>,
    user: UserId,
    archive: ArchiveId,
    sender: UserId,
    target: ArchiveId,
}

impl Mapped {
    fn actor(&self, actor: &Actor) -> Actor {
        let mut out = actor.clone();
        if actor.user == self.user {
            out.user = self.sender;
        }
        out
    }

    fn archive(&self, archive: ArchiveId) -> ArchiveId {
        if archive == self.archive {
            self.target
        } else {
            archive
        }
    }
}

#[async_trait]
impl Submission for Mapped {
    async fn identities(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<SendIdentity>, SubmissionError> {
        self.inner
            .identities(&self.actor(actor), self.archive(archive))
            .await
    }

    async fn limits(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<SubmissionLimits, SubmissionError> {
        self.inner
            .limits(&self.actor(actor), self.archive(archive))
            .await
    }

    async fn sender(
        &self,
        actor: &Actor,
        archive: ArchiveId,
        address: &str,
    ) -> Result<SendIdentity, SubmissionError> {
        self.inner
            .sender(&self.actor(actor), self.archive(archive), address)
            .await
    }

    async fn queue(&self, mut request: Submit) -> Result<Submitted, SubmissionError> {
        request.actor = self.actor(&request.actor);
        request.archive = self.archive(request.archive);
        self.inner.queue(request).await
    }

    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.undo(&self.actor(actor), id).await
    }

    async fn release(
        &self,
        actor: &Actor,
        id: OutboxId,
        undo_window: Option<std::time::Duration>,
    ) -> Result<Submitted, SubmissionError> {
        self.inner
            .release(&self.actor(actor), id, undo_window)
            .await
    }

    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.status(&self.actor(actor), id).await
    }
}

#[derive(Default)]
pub struct MemoryBlobs(Mutex<HashMap<BlobId, Bytes>>);

#[async_trait]
impl BlobStore for MemoryBlobs {
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError> {
        let id = BlobId::of(&bytes);
        self.0.lock().unwrap().insert(id, bytes);
        Ok(id)
    }

    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }

    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError> {
        Ok(self.0.lock().unwrap().contains_key(id))
    }

    async fn delete(&self, id: &BlobId) -> Result<(), BlobError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

#[derive(Default)]
pub struct TestHub(Mutex<HashMap<ArchiveId, watch::Sender<u64>>>);

impl TestHub {
    fn sender(&self, archive: ArchiveId) -> watch::Sender<u64> {
        self.0
            .lock()
            .unwrap()
            .entry(archive)
            .or_insert_with(|| watch::channel(0).0)
            .clone()
    }

    pub fn poke(&self, archive: ArchiveId) {
        self.sender(archive).send_modify(|seq| *seq += 1);
    }
}

struct TestWake(watch::Receiver<u64>);

#[async_trait]
impl Wake for TestWake {
    async fn changed(&mut self) -> Option<u64> {
        self.0.changed().await.ok()?;
        Some(*self.0.borrow_and_update())
    }
}

#[async_trait]
impl Hub for TestHub {
    fn listen(&self, archive: ArchiveId, _user: UserId) -> Box<dyn Wake> {
        Box::new(TestWake(self.sender(archive).subscribe()))
    }

    async fn archive_name(&self, _archive: ArchiveId) -> Option<String> {
        Some("Ada's mail".into())
    }
}

#[derive(Debug, Clone)]
pub struct Creds {
    pub login: String,
    pub secret: String,
    pub device: DeviceId,
}

impl Creds {
    pub fn basic(&self) -> String {
        format!(
            "Basic {}",
            STANDARD.encode(format!("{}:{}", self.login, self.secret))
        )
    }
}

pub struct Harness {
    pub server: JmapServer,
    pub router: Router,
    pub world: World,
    pub auth: Arc<TestAuth>,
    pub hub: Arc<TestHub>,
    pub send: Arc<dyn SendDriver>,
    pub archive: ArchiveId,
    pub editor: Creds,
    pub reader: Creds,
}

pub const TOKEN: &str = "instance-token-for-tests";

pub fn harness() -> Harness {
    harness_with(|_| {})
}

pub fn harness_with(configure: impl FnOnce(&mut Settings)) -> Harness {
    let world = FakeMailView::world(&Scenario::two_accounts(), Setup::default());
    let archive = world.editor.archive;
    let sending = FakeSubmission::world(SendSetup::default());
    let submission = Arc::new(Mapped {
        inner: sending.submission.clone(),
        user: world.editor.user,
        archive,
        sender: sending.sender.user,
        target: sending.archive,
    });
    let auth = Arc::new(TestAuth::default());
    let principal = |scopes: &[DeviceScope]| Principal {
        user: world.editor.user,
        device: DeviceId::new(),
        mail_archive: Some(archive),
        archives: vec![archive],
        scopes: scopes.iter().copied().collect::<BTreeSet<_>>(),
        implicit_seen: false,
    };
    let editor_principal = principal(&[
        DeviceScope::Read,
        DeviceScope::WriteMail,
        DeviceScope::SendMail,
    ]);
    let reader_principal = principal(&[DeviceScope::Read]);
    let editor = Creds {
        login: "ada@example.com".into(),
        secret: "hm_editor".into(),
        device: editor_principal.device,
    };
    let reader = Creds {
        login: "ada@example.com".into(),
        secret: "hm_reader".into(),
        device: reader_principal.device,
    };
    auth.add(&editor.login, &editor.secret, editor_principal);
    auth.add(&reader.login, &reader.secret, reader_principal);
    let hub = Arc::new(TestHub::default());
    let mut settings = Settings::new("https://mail.example.com");
    settings.instance_token = Some(TOKEN.into());
    configure(&mut settings);
    let server = JmapServer::new(
        settings,
        Services {
            view: world.view.clone(),
            actions: world.actions.clone(),
            submission,
            devices: auth.clone(),
            blobs: Arc::new(MemoryBlobs::default()),
            hub: hub.clone(),
        },
    );
    Harness {
        router: server.router(),
        server,
        world,
        auth,
        hub,
        send: sending.driver,
        archive,
        editor,
        reader,
    }
}

pub struct Answer {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Bytes,
}

impl Answer {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

impl Harness {
    pub fn account(&self) -> String {
        self.archive.to_string()
    }

    pub async fn send(&self, request: Request<Body>) -> Answer {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        Answer {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, path: &str, creds: Option<&Creds>) -> Answer {
        let mut request = Request::get(path);
        if let Some(creds) = creds {
            request = request.header(header::AUTHORIZATION, creds.basic());
        }
        self.send(request.body(Body::empty()).unwrap()).await
    }

    pub async fn post(
        &self,
        path: &str,
        creds: &Creds,
        body: impl Into<Body>,
        kind: &str,
    ) -> Answer {
        let mut request = Request::post(path)
            .header(header::AUTHORIZATION, creds.basic())
            .header(header::CONTENT_TYPE, kind)
            .body(body.into())
            .unwrap();
        request.extensions_mut().insert(loopback());
        self.send(request).await
    }

    /// Runs method calls and returns the method responses.
    pub async fn calls(&self, creds: &Creds, calls: Value) -> Vec<Value> {
        let body = json!({"using": USING, "methodCalls": calls});
        let answer = self
            .post("/jmap/api", creds, body.to_string(), "application/json")
            .await;
        assert_eq!(
            answer.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&answer.body)
        );
        answer.json()["methodResponses"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Runs one method call and returns its arguments, panicking on a method error.
    pub async fn call(&self, creds: &Creds, name: &str, args: Value) -> Value {
        let responses = self.calls(creds, json!([[name, args, "c"]])).await;
        assert_eq!(responses[0][0], json!(name), "{}", responses[0]);
        responses[0][1].clone()
    }

    pub fn email(&self, name: &str) -> String {
        crate::ids::email(&self.world.email(name))
    }
}

pub fn loopback() -> ConnectInfo<SocketAddr> {
    ConnectInfo("127.0.0.1:50000".parse().unwrap())
}

pub async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("timed out")
}
