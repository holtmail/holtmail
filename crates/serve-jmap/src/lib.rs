//! JMAP (RFC 8620, RFC 8621, RFC 8887) for the user's own mail clients.
//!
//! A [`JmapServer`] is an axum router over the served mail view ([`MailView`], [`MailActions`]),
//! [`Submission`], [`DeviceAuth`] and a [`BlobStore`] for uploads. A client signs in with a
//! device's app password; every read goes through the mail view as that device sees it, and
//! every write is the same held change, local flag, draft or outbox row the web app makes, so it
//! meets the same five locks. It holds no writer, transport or store handle that writes.
//!
//! Push comes from a [`Hub`] that says when an archive changed; the server never polls.
//! The design is in SERVE.md, "JMAP".

mod auth;
mod blob;
mod body;
mod email;
mod email_set;
mod error;
mod ids;
mod json;
mod mailbox;
mod push;
mod request;
mod session;
mod submit;
mod time;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::routing::{get, post};
use holtmail_blob::BlobStore;
use holtmail_core::{ArchiveId, DeviceId, UserId};
use holtmail_device::DeviceAuth;
use holtmail_mailview::{MailActions, MailView};
use holtmail_serve_net::TrustedProxies;
use holtmail_submission::Submission;
use tokio::sync::{Semaphore, broadcast};

/// The capability of the instance token in the session resource. Its value is
/// `{"token": "<instance token>"}`; a Holtmail source refuses an upstream that carries its own.
pub const INSTANCE_CAPABILITY: &str = "urn:holtmail:params:jmap:instance";

const UPLOAD_SWEEP: Duration = Duration::from_secs(60 * 60);

/// How large and how many, as the session resource advertises them (RFC 8620 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest upload in bytes. The session advertises the smaller of this and what the
    /// mail view takes.
    pub max_size_upload: u64,
    /// Uploads one user may run at once.
    pub max_concurrent_upload: u32,
    /// The largest request body in bytes.
    pub max_size_request: usize,
    /// Requests one user may run at once; more are refused with the `limit` error.
    pub max_concurrent_requests: u32,
    /// Method calls in one request.
    pub max_calls_in_request: usize,
    /// Ids in one `/get`.
    pub max_objects_in_get: usize,
    /// Creates, updates and destroys in one `/set`.
    pub max_objects_in_set: usize,
    /// Ids one `Email/query` answers at most per call.
    pub max_query_limit: usize,
    /// Bytes of uploads one user may hold at once within their lifetime; not advertised.
    pub upload_budget: u64,
}

impl Default for Limits {
    /// SERVE.md's defaults: 50 MiB uploads, 4 concurrent requests and uploads, 32 calls, 500
    /// objects, 10 MB requests, 1 000 ids per query page, and 200 MiB of uploads per user.
    fn default() -> Self {
        Self {
            max_size_upload: 50 << 20,
            max_concurrent_upload: 4,
            max_size_request: 10_000_000,
            max_concurrent_requests: 4,
            max_calls_in_request: 32,
            max_objects_in_get: 500,
            max_objects_in_set: 500,
            max_query_limit: 1_000,
            upload_budget: 200 << 20,
        }
    }
}

/// How one server behaves.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The address clients reach Holtmail at, e.g. `https://mail.example.com`; every URL in the
    /// session resource starts with it. Sign-in is refused unless it is `https` or the client is
    /// on a loopback address.
    pub public_url: String,
    /// Sent in the session resource under [`INSTANCE_CAPABILITY`].
    pub instance_token: Option<String>,
    /// Peers whose `X-Forwarded-For` names the client.
    pub trusted_proxies: TrustedProxies,
    /// How long a submission stays undoable; `None` for the instance's default.
    pub undo_window: Option<Duration>,
    /// The limits.
    pub limits: Limits,
}

impl Settings {
    /// Default limits, no instance token, no trusted proxies.
    pub fn new(public_url: impl Into<String>) -> Self {
        Self {
            public_url: public_url.into().trim_end_matches('/').to_string(),
            instance_token: None,
            trusted_proxies: TrustedProxies::default(),
            undo_window: None,
            limits: Limits::default(),
        }
    }
}

/// Wakes a push stream when its archive changed.
#[async_trait]
pub trait Wake: Send {
    /// Waits until something after the last wake-up changed in the archive and returns the
    /// highest change `seq` seen; `None` once no more wake-ups will come.
    async fn changed(&mut self) -> Option<u64>;
}

/// What the server needs from the instance besides the contracts: change wake-ups and names.
#[async_trait]
pub trait Hub: Send + Sync {
    /// Wake-ups for changes of `archive` that `user` may see.
    fn listen(&self, archive: ArchiveId, user: UserId) -> Box<dyn Wake>;

    /// The archive's display name, used as the JMAP account name.
    async fn archive_name(&self, archive: ArchiveId) -> Option<String>;
}

/// The contracts the server works through.
#[derive(Clone)]
pub struct Services {
    /// Reads the mail view.
    pub view: Arc<dyn MailView>,
    /// Changes it.
    pub actions: Arc<dyn MailActions>,
    /// Takes mail to send.
    pub submission: Arc<dyn Submission>,
    /// Signs devices in.
    pub devices: Arc<dyn DeviceAuth>,
    /// Keeps uploads and nothing else: the server deletes what it put there once the upload
    /// expires, and keeps no record across restarts, so the instance gives it a store of its own
    /// that it empties at start.
    pub blobs: Arc<dyn BlobStore>,
    /// Wakes push streams.
    pub hub: Arc<dyn Hub>,
}

pub(crate) struct Inner {
    settings: Settings,
    services: Services,
    auth: auth::Cache,
    revoked: broadcast::Sender<DeviceId>,
    requests: Mutex<HashMap<UserId, Arc<Semaphore>>>,
    uploads: blob::Uploads,
    queries: email::QueryCache,
    sent: submit::Registry,
}

/// The JMAP server: build it once, mount [`JmapServer::router`].
#[derive(Clone)]
pub struct JmapServer {
    inner: Arc<Inner>,
}

impl JmapServer {
    /// A server over `services`. Starts a task that follows the device revocation feed and one
    /// that deletes expired uploads every hour, so it must be called inside a Tokio runtime; the
    /// tasks end once the server is dropped.
    pub fn new(settings: Settings, services: Services) -> Self {
        let (revoked, _) = broadcast::channel(256);
        let inner = Arc::new(Inner {
            settings,
            services,
            auth: auth::Cache::default(),
            revoked,
            requests: Mutex::new(HashMap::new()),
            uploads: blob::Uploads::default(),
            queries: email::QueryCache::default(),
            sent: submit::Registry::default(),
        });
        let sweeper: Weak<Inner> = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(UPLOAD_SWEEP);
            every.tick().await;
            loop {
                every.tick().await;
                let Some(inner) = sweeper.upgrade() else {
                    break;
                };
                let gone = inner.uploads.sweep();
                blob::delete_expired(&inner, gone).await;
            }
        });
        let mut feed = inner.services.devices.revocations();
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        tokio::spawn(async move {
            loop {
                let device = feed.next().await;
                let Some(inner) = weak.upgrade() else { break };
                inner.auth.forget(device);
                let _ = inner.revoked.send(device);
            }
        });
        Self { inner }
    }

    /// The routes: `GET /.well-known/jmap` (a 307 to the session), and under `/jmap`:
    /// `session`, `api`, `download/{account}/{blob}/{name}`, `upload/{account}`, `eventsource`
    /// and `ws`. Every `/jmap` route answers CORS preflights and allows any origin without
    /// cookies: credentials come only from the `Authorization` header.
    pub fn router(&self) -> Router {
        let jmap = Router::new()
            .route("/jmap/session", get(session::handle))
            .route("/jmap/api", post(request::handle))
            .route(
                "/jmap/download/{account}/{blob}/{name}",
                get(blob::download),
            )
            .route("/jmap/upload/{account}", post(blob::upload))
            .route("/jmap/eventsource", get(push::event_source))
            .route("/jmap/ws", get(push::websocket))
            .layer(axum::middleware::from_fn(auth::cors))
            .layer(axum::extract::DefaultBodyLimit::disable());
        Router::new()
            .route("/.well-known/jmap", get(session::well_known))
            .merge(jmap)
            .with_state(self.inner.clone())
    }
}
