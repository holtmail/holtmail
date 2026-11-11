//! CalDAV and CardDAV for the user's own calendar and contacts apps: an axum router over the
//! archive's collections, their held edits and the write locks. A protocol front end, not a
//! contract implementation: it receives the store, the ingest pipeline and `DeviceAuth`, and
//! only `apps/server` mounts it, behind `[serve.dav]`. See `docs/README.md`.

mod auth;
mod filter;
mod model;
mod path;
mod people;
mod props;
mod read;
mod sched;
mod write;
mod xml;

#[cfg(test)]
mod tests;

use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use holtmail_access::{Access, Scopes, WriteSwitches};
use holtmail_blob::BlobStore;
use holtmail_core::{ArchiveId, DeviceProtocol};
use holtmail_device::{DeviceAuth, DeviceAuthError};
use holtmail_ingest::Ingest;
use holtmail_pim_sync::Edits;
use holtmail_schedule::Scheduler;
use holtmail_serve_net::TrustedProxies;
use holtmail_store::Store;

use crate::model::Caller;
use crate::path::Target;
pub use crate::sched::CalendarHosts;
use crate::sched::Scheduling;

/// How the DAV server is set up (`[serve.dav]`).
#[derive(Debug, Clone)]
pub struct DavOptions {
    /// The instance's public origin, such as `https://mail.example.com`. Sign-in is accepted
    /// only when it is `https`, or from a client on this machine; redirects are absolute on it.
    pub public_url: String,
    /// Whether any collection may be written (`edits`); `false` serves every collection
    /// read-only.
    pub edits: bool,
    /// Peers whose `X-Forwarded-For` is believed for the client address.
    pub trusted_proxies: TrustedProxies,
    /// The instance write switches (`[write]`).
    pub switches: WriteSwitches,
    /// Whether calendar-auto-schedule may be offered (`scheduling`); it is advertised only to
    /// callers for whom every condition holds, and only with [`Dav::with_scheduler`].
    pub scheduling: bool,
}

impl DavOptions {
    /// Options for `public_url` with edits on, no trusted proxies and every switch on.
    pub fn new(public_url: impl Into<String>) -> Self {
        Self {
            public_url: public_url.into(),
            edits: true,
            trusted_proxies: TrustedProxies::default(),
            switches: WriteSwitches::default(),
            scheduling: false,
        }
    }
}

/// Called with the archive after every change a client recorded, so live views can refresh.
pub type ChangeHook = Arc<dyn Fn(ArchiveId) + Send + Sync>;

pub(crate) struct Shared {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    devices: Arc<dyn DeviceAuth>,
    access: Access,
    ingest: Ingest,
    edits: Edits,
    options: DavOptions,
    secure: bool,
    hook: Option<ChangeHook>,
    scheduling: Option<Scheduling>,
}

/// The CalDAV and CardDAV server. Cheap to clone; every clone serves the same state.
#[derive(Clone)]
pub struct Dav(Arc<Shared>);

impl Dav {
    /// A server over these contracts. `ingest` rebuilds an item's derived data after an edit
    /// and holds the blob store the edits' bytes go to.
    pub fn new(
        store: Arc<dyn Store>,
        ingest: Ingest,
        devices: Arc<dyn DeviceAuth>,
        options: DavOptions,
    ) -> Self {
        Self::build(store, ingest, devices, options, None, None)
    }

    /// The same server, calling `hook` after each recorded change.
    pub fn with_hook(self, hook: ChangeHook) -> Self {
        let shared = &self.0;
        Self::build(
            shared.store.clone(),
            shared.ingest.clone(),
            shared.devices.clone(),
            shared.options.clone(),
            Some(hook),
            shared.scheduling.clone(),
        )
    }

    /// The same server with calendar-auto-schedule: `scheduler` prepares and delivers what
    /// calendar clients write, `hosts` tells which upstream calendars schedule themselves.
    /// Offered only while [`DavOptions::scheduling`] is on.
    pub fn with_scheduler(
        self,
        scheduler: Arc<dyn Scheduler>,
        hosts: Arc<dyn CalendarHosts>,
    ) -> Self {
        let shared = &self.0;
        Self::build(
            shared.store.clone(),
            shared.ingest.clone(),
            shared.devices.clone(),
            shared.options.clone(),
            shared.hook.clone(),
            Some(Scheduling { scheduler, hosts }),
        )
    }

    fn build(
        store: Arc<dyn Store>,
        ingest: Ingest,
        devices: Arc<dyn DeviceAuth>,
        options: DavOptions,
        hook: Option<ChangeHook>,
        scheduling: Option<Scheduling>,
    ) -> Self {
        let secure = options
            .public_url
            .trim()
            .to_ascii_lowercase()
            .starts_with("https://");
        Self(Arc::new(Shared {
            access: Access::new(store.clone(), options.switches),
            edits: Edits::new(store.clone(), ingest.clone()),
            blobs: ingest.blobs(),
            store,
            devices,
            ingest,
            options,
            secure,
            hook,
            scheduling,
        }))
    }

    /// The routes: `/dav` and everything below it, `/.well-known/caldav` and
    /// `/.well-known/carddav`. `PROPFIND` on `/` is [`root_propfind`]'s, so the web app keeps
    /// every other method there.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/dav", any(serve))
            .route("/dav/", any(serve))
            .route("/dav/{*rest}", any(serve))
            .route("/.well-known/caldav", any(well_known))
            .route("/.well-known/carddav", any(well_known))
            .with_state(self.clone())
    }
}

/// Middleware for the web app's router: answers `PROPFIND /` (clients that skip the well-known
/// redirect probe the root for `current-user-principal`) and passes everything else on.
/// Mounted with `axum::middleware::from_fn_with_state(dav, root_propfind)`.
pub async fn root_propfind(State(dav): State<Dav>, request: Request, next: Next) -> Response {
    if request.uri().path() == "/" && request.method().as_str() == "PROPFIND" {
        return dav.handle(request).await;
    }
    next.run(request).await
}

async fn serve(State(dav): State<Dav>, request: Request) -> Response {
    dav.handle(request).await
}

async fn well_known(State(dav): State<Dav>) -> Response {
    let base = dav.0.options.public_url.trim_end_matches('/');
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, format!("{base}{}/", path::BASE))
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::MOVED_PERMANENTLY.into_response())
}

pub(crate) fn text(status: StatusCode, body: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

pub(crate) fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "5")],
        "try again shortly",
    )
        .into_response()
}

pub(crate) fn xml_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

pub(crate) fn dav_error(status: StatusCode, ns: &str, condition: &str) -> Response {
    xml_response(status, xml::error(ns, condition))
}

fn challenge() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Holtmail\", charset=\"UTF-8\""),
        )],
        "sign in with an app password",
    )
        .into_response()
}

fn options(scheduling: bool) -> Response {
    let (dav, allow) = match scheduling {
        true => (
            "1, 3, access-control, calendar-access, calendar-auto-schedule, addressbook",
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, POST",
        ),
        false => (
            "1, 3, access-control, calendar-access, addressbook",
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT",
        ),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("DAV", dav)
        .header(header::ALLOW, allow)
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}

impl Dav {
    async fn sign_in(&self, headers: &HeaderMap, ip: IpAddr) -> Result<Caller, Box<Response>> {
        let Some((login, secret)) = auth::basic(headers) else {
            return Err(Box::new(challenge()));
        };
        let principal = match self
            .0
            .devices
            .verify(&login, &secret, ip, DeviceProtocol::Dav)
            .await
        {
            Ok(principal) => principal,
            Err(DeviceAuthError::Invalid | DeviceAuthError::Revoked) => {
                return Err(Box::new(challenge()));
            }
            Err(DeviceAuthError::TooMany { retry_after }) => {
                return Err(Box::new(
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(
                            header::RETRY_AFTER,
                            retry_after.as_secs().max(1).to_string(),
                        )],
                        "too many failed sign-ins",
                    )
                        .into_response(),
                ));
            }
            Err(DeviceAuthError::Unavailable(_)) => return Err(Box::new(unavailable())),
        };
        let user = match self.0.store.get_user(principal.user).await {
            Ok(Some(user)) => user,
            Ok(None) => return Err(Box::new(challenge())),
            Err(_) => return Err(Box::new(unavailable())),
        };
        let mut caller = Caller {
            user,
            device: principal.device,
            archives: principal.archives,
            scopes: Scopes::of_device(&principal.scopes),
            device_scopes: principal.scopes,
            sched: None,
        };
        caller.sched = match self.0.sched(&caller).await {
            Ok(sched) => sched,
            Err(model::Unavailable) => return Err(Box::new(unavailable())),
        };
        Ok(caller)
    }

    /// Answers one request.
    async fn handle(&self, request: Request) -> Response {
        let (parts, body) = request.into_parts();
        if parts.method == Method::OPTIONS {
            return self.options_for(&parts).await;
        }
        let method = parts.method.as_str().to_owned();
        let creates = matches!(method.as_str(), "MKCOL" | "MKCALENDAR" | "MOVE" | "COPY");
        let target = path::parse(parts.uri.path());
        if target.is_none() && !creates {
            return text(StatusCode::NOT_FOUND, "not found");
        }
        let ip = auth::client_ip(
            &self.0.options.trusted_proxies,
            &parts.headers,
            &parts.extensions,
        );
        if !self.0.secure && !ip.is_loopback() {
            return text(
                StatusCode::FORBIDDEN,
                "CalDAV and CardDAV sign-in needs https",
            );
        }
        let caller = match self.sign_in(&parts.headers, ip).await {
            Ok(caller) => caller,
            Err(response) => return *response,
        };
        let target = match target {
            Some(t) if !creates && t.user().is_some_and(|u| u != caller.user.id) => {
                return text(StatusCode::NOT_FOUND, "not found");
            }
            Some(t) if !creates => t,
            _ => return text(StatusCode::FORBIDDEN, "collections come from accounts"),
        };
        let response = match (&target, &caller.sched) {
            (Target::Inbox(_) | Target::Outbox(_) | Target::InboxItem(..), None) => {
                text(StatusCode::NOT_FOUND, "not found")
            }
            (Target::Inbox(_) | Target::Outbox(_) | Target::InboxItem(..), Some(_)) => {
                self.scheduling_box(&caller, &target, &method, &parts.headers, body)
                    .await
            }
            _ => {
                self.method(&caller, &target, &method, &parts.headers, body)
                    .await
            }
        };
        tracing::debug!(
            device = %caller.device,
            method = %method,
            status = response.status().as_u16(),
            "dav request"
        );
        response
    }

    async fn method(
        &self,
        caller: &Caller,
        target: &Target,
        method: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Response {
        match method {
            "PROPFIND" => read::propfind(self, caller, target, headers, body).await,
            "REPORT" => read::report(self, caller, target, headers, body).await,
            "GET" | "HEAD" => read::get(self, caller, target, headers, method == "HEAD").await,
            "PUT" => write::put(self, caller, target, headers, body).await,
            "DELETE" => write::delete(self, caller, target, headers).await,
            "PROPPATCH" => write::proppatch(self, caller, target, body).await,
            _ => text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        }
    }

    async fn scheduling_box(
        &self,
        caller: &Caller,
        target: &Target,
        method: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Response {
        use crate::sched::Box_;
        let which = match target {
            Target::Inbox(_) => Box_::Inbox,
            Target::Outbox(_) => Box_::Outbox,
            Target::InboxItem(_, name) => Box_::Item(name.clone()),
            _ => return text(StatusCode::NOT_FOUND, "not found"),
        };
        match (method, which) {
            ("PROPFIND", which) => read::propfind_box(self, caller, which, headers, body).await,
            ("GET" | "HEAD", Box_::Item(name)) => {
                sched::get(self, caller, &name, method == "HEAD").await
            }
            ("DELETE", Box_::Item(name)) => sched::delete(self, caller, &name).await,
            ("REPORT", Box_::Inbox) => read::report_inbox(self, caller, body).await,
            ("POST", Box_::Outbox) => sched::post(self, caller, headers, body).await,
            _ => text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        }
    }

    /// `OPTIONS`: answered without sign-in; with valid credentials it also says whether
    /// calendar-auto-schedule is offered to that caller.
    async fn options_for(&self, parts: &axum::http::request::Parts) -> Response {
        if !self.0.options.scheduling
            || self.0.scheduling.is_none()
            || auth::basic(&parts.headers).is_none()
        {
            return options(false);
        }
        let ip = auth::client_ip(
            &self.0.options.trusted_proxies,
            &parts.headers,
            &parts.extensions,
        );
        if !self.0.secure && !ip.is_loopback() {
            return options(false);
        }
        match self.sign_in(&parts.headers, ip).await {
            Ok(caller) => options(caller.sched.is_some()),
            Err(_) => options(false),
        }
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.0
    }
}

/// Reads a request body up to `limit` bytes; `None` when it is longer or cannot be read.
pub(crate) async fn body_bytes(body: Body, limit: usize) -> Option<bytes::Bytes> {
    axum::body::to_bytes(body, limit).await.ok()
}
