//! A [`Source`] and a [`MailboxWriter`](holtmail_source::MailboxWriter) for IMAP servers.

mod labels;
mod stream;
mod utf7;
mod watch;
mod writer;

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use async_imap::types::{Flag, NameAttribute};
use async_trait::async_trait;
use bytes::Bytes;
use futures::TryStreamExt;
use holtmail_core::{Timestamp, TlsRoots};
use holtmail_source::{
    Credentials, FolderStatus, RemoteFlags, RemoteFolder, RemoteMessage, Source, SourceError,
    SourceSession, SourceWatch,
};
use holtmail_store::FolderRole;
use serde::{Deserialize, Serialize};
use stream::ImapStream;
pub use writer::ImapWriter;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one `IDLE` lasts before it is renewed. RFC 2177 allows 29 minutes; NAT gateways and
/// firewalls often forget a silent connection sooner, and a renewal on a dead connection fails
/// and starts a new watch.
const IDLE_RENEW: Duration = Duration::from_secs(10 * 60);
/// Connections one watch opens at most when the server has no `NOTIFY`, one per folder: servers
/// limit connections per user (Dovecot 10, Gmail 15) and syncs need theirs.
pub const MAX_WATCH_CONNECTIONS: usize = 3;

/// How to reach the server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    /// TLS from the first byte, usually port 993.
    #[default]
    Tls,
    /// Plain connection upgraded with `STARTTLS`, usually port 143.
    Starttls,
    /// No encryption. Only for servers on the same machine or in tests.
    Plain,
}

/// Account settings stored in `Account.settings`; credentials are sealed separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImapSettings {
    /// Server host name.
    pub host: String,
    /// Port; 993 for `tls`, 143 otherwise when omitted.
    #[serde(default)]
    pub port: Option<u16>,
    /// Connection security.
    #[serde(default)]
    pub security: Security,
    /// User name, often the address.
    pub username: String,
}

impl ImapSettings {
    /// Reads settings from an account's JSON.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, SourceError> {
        serde_json::from_value(value.clone()).map_err(|e| SourceError::Config(e.to_string()))
    }

    /// The port to connect to: the configured one, else the default of `security`.
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(match self.security {
            Security::Tls => 993,
            Security::Starttls | Security::Plain => 143,
        })
    }
}

/// An IMAP account.
#[derive(Clone)]
pub struct ImapSource {
    settings: ImapSettings,
    credentials: Credentials,
    tls: Arc<tokio_rustls::rustls::ClientConfig>,
    timeout: Duration,
    idle_renew: Duration,
}

impl ImapSource {
    /// A source for the server in `settings`. A password signs in with `LOGIN`, an access token
    /// with `AUTHENTICATE XOAUTH2` for the user name in `settings`.
    pub fn new(settings: ImapSettings, credentials: Credentials) -> Self {
        Self {
            settings,
            credentials,
            tls: Arc::new(tls_config(&TlsRoots::default()).expect("the built-in roots are usable")),
            timeout: COMMAND_TIMEOUT,
            idle_renew: IDLE_RENEW,
        }
    }

    /// Renews a watch's `IDLE` after `renew` instead of ten minutes.
    pub fn with_idle_renew(mut self, renew: Duration) -> Self {
        self.idle_renew = renew;
        self
    }

    /// Gives every command, the connection and the sign-in `timeout` instead of two minutes; a
    /// server that does not answer within it is a `Connection` error.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Also trusts `roots` for TLS and STARTTLS, besides the built-in roots. `Config` when one of
    /// them is not a usable CA certificate.
    pub fn with_roots(mut self, roots: &TlsRoots) -> Result<Self, SourceError> {
        self.tls = Arc::new(tls_config(roots)?);
        Ok(self)
    }
}

fn tls_config(extra: &TlsRoots) -> Result<tokio_rustls::rustls::ClientConfig, SourceError> {
    let mut roots = tokio_rustls::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    for der in extra.iter() {
        roots
            .add(der.to_vec().into())
            .map_err(|e| SourceError::Config(format!("an extra trusted certificate: {e}")))?;
    }
    Ok(
        tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

struct SignedIn {
    session: async_imap::Session<ImapStream>,
    gmail: bool,
    idle: bool,
    notify: bool,
    condstore: bool,
    uidplus: bool,
    moves: bool,
}

impl ImapSource {
    async fn sign_in(&self) -> Result<SignedIn, SourceError> {
        let greeting_wait = self.timeout.min(stream::GREETING_WAIT);
        let (stream, greeting) = timed(
            self.timeout + greeting_wait,
            ImapStream::connect(
                &self.settings.host,
                self.settings.port(),
                self.settings.security,
                self.tls.clone(),
                greeting_wait,
            ),
        )
        .await??;
        let mut client = async_imap::Client::new(stream);
        if greeting {
            timed(self.timeout, client.read_response())
                .await?
                .map_err(|e| SourceError::Connection(e.to_string()))?
                .ok_or_else(|| SourceError::Connection("closed before the greeting".into()))?;
        }
        let signed_in = match &self.credentials {
            Credentials::Password(password) => {
                timed(
                    self.timeout,
                    client.login(&self.settings.username, password),
                )
                .await?
            }
            Credentials::AccessToken(token) => {
                timed(
                    self.timeout,
                    client.authenticate(
                        "XOAUTH2",
                        Xoauth2 {
                            response: xoauth2(&self.settings.username, token),
                            sent: false,
                        },
                    ),
                )
                .await?
            }
        };
        let mut session = signed_in.map_err(|(error, _)| match error {
            async_imap::error::Error::No(message) | async_imap::error::Error::Bad(message) => {
                SourceError::Auth(message)
            }
            other => convert(other),
        })?;
        let capabilities = timed(self.timeout, session.capabilities())
            .await?
            .map_err(convert)?;
        let gmail = capabilities.has_str("X-GM-EXT-1");
        let condstore = capabilities.has_str("CONDSTORE") || capabilities.has_str("QRESYNC");
        if condstore && capabilities.has_str("ENABLE") {
            match timed(
                self.timeout,
                session.run_command_and_check_ok("ENABLE CONDSTORE"),
            )
            .await?
            {
                Ok(())
                | Err(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)) => {}
                Err(other) => return Err(convert(other)),
            }
        }
        Ok(SignedIn {
            session,
            gmail,
            idle: capabilities.has_str("IDLE"),
            notify: capabilities.has_str("NOTIFY"),
            condstore,
            uidplus: capabilities.has_str("UIDPLUS"),
            moves: capabilities.has_str("MOVE"),
        })
    }
}

#[async_trait]
impl Source for ImapSource {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError> {
        let signed_in = self.sign_in().await?;
        Ok(Box::new(ImapSession {
            session: signed_in.session,
            names: HashMap::new(),
            gmail: signed_in.gmail,
            modseq: false,
            timeout: self.timeout,
        }))
    }

    /// Watches with `IDLE` when the server has it: with `NOTIFY` (RFC 5465) on one connection
    /// for every folder, otherwise one connection per folder for the first
    /// [`MAX_WATCH_CONNECTIONS`] of them. Folders that do not exist are left out; with none left,
    /// or none named, `INBOX` is watched.
    async fn watch(&self, folders: &[String]) -> Result<Option<Box<dyn SourceWatch>>, SourceError> {
        let SignedIn {
            mut session,
            gmail,
            idle,
            notify,
            ..
        } = self.sign_in().await?;
        if !idle {
            let _ = timed(self.timeout, session.logout()).await;
            return Ok(None);
        }
        let listed = list_folders(&mut session, gmail, self.timeout).await?;
        let mut wanted: Vec<(String, String)> = Vec::new();
        for path in folders {
            if let Some((folder, raw)) = listed.iter().find(|(f, _)| &f.path == path)
                && folder.selectable
                && !wanted.iter().any(|(p, _)| p == path)
            {
                wanted.push((folder.path.clone(), raw.clone()));
            }
        }
        if wanted.is_empty() {
            let inbox = listed
                .iter()
                .find(|(_, raw)| raw.eq_ignore_ascii_case("INBOX"))
                .map_or_else(
                    || ("INBOX".to_owned(), "INBOX".to_owned()),
                    |(f, raw)| (f.path.clone(), raw.clone()),
                );
            wanted.push(inbox);
        }
        if wanted.len() > 1 && notify {
            let command = watch::notify_command(wanted.iter().map(|(_, raw)| raw.as_str()));
            match timed(self.timeout, session.run_command_and_check_ok(&command)).await? {
                Ok(()) => {
                    let paths = wanted.into_iter().map(|(path, raw)| (raw, path)).collect();
                    return Ok(Some(Box::new(watch::ImapWatch::start(
                        vec![(session, watch::Target::Notify { paths })],
                        self.timeout,
                        self.idle_renew,
                    ))));
                }
                Err(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)) => {}
                Err(other) => return Err(convert(other)),
            }
        }
        let mut connections = Vec::new();
        let mut first = Some(session);
        for (path, raw) in wanted.into_iter().take(MAX_WATCH_CONNECTIONS) {
            let session = match first.take() {
                Some(session) => session,
                None => match self.sign_in().await {
                    Ok(more) => more.session,
                    Err(error) => {
                        tracing::warn!(%error, watched = connections.len(), "no further watch connection");
                        break;
                    }
                },
            };
            connections.push((session, watch::Target::Folder { path, raw }));
        }
        Ok(Some(Box::new(watch::ImapWatch::start(
            connections,
            self.timeout,
            self.idle_renew,
        ))))
    }
}

/// Every folder with the server's own name for it.
async fn list_folders(
    session: &mut async_imap::Session<ImapStream>,
    gmail: bool,
    timeout: Duration,
) -> Result<Vec<(RemoteFolder, String)>, SourceError> {
    let names: Vec<_> = timed(timeout, async {
        session
            .list(Some(""), Some("*"))
            .await?
            .try_collect::<Vec<_>>()
            .await
    })
    .await?
    .map_err(convert)?;
    let mut out = Vec::with_capacity(names.len());
    for name in &names {
        let raw = name.name().to_owned();
        let decoded = utf7::decode(&raw);
        let path = match name.delimiter() {
            Some(delimiter) if delimiter != "/" => decoded.replace(delimiter, "/"),
            _ => decoded,
        };
        let mut role = None;
        let mut selectable = true;
        for attribute in name.attributes() {
            match attribute {
                NameAttribute::NoSelect => selectable = false,
                NameAttribute::All => role = Some(FolderRole::All),
                NameAttribute::Archive => role = Some(FolderRole::Archive),
                NameAttribute::Drafts => role = Some(FolderRole::Drafts),
                NameAttribute::Flagged => role = Some(FolderRole::Flagged),
                NameAttribute::Junk => role = Some(FolderRole::Junk),
                NameAttribute::Sent => role = Some(FolderRole::Sent),
                NameAttribute::Trash => role = Some(FolderRole::Trash),
                NameAttribute::Extension(ext) if ext.eq_ignore_ascii_case("\\Important") => {
                    role = Some(FolderRole::Important)
                }
                NameAttribute::Extension(ext) if ext.eq_ignore_ascii_case("\\NonExistent") => {
                    selectable = false
                }
                _ => {}
            }
        }
        if raw.eq_ignore_ascii_case("INBOX") {
            role = Some(FolderRole::Inbox);
        }
        let role = role.or_else(|| FolderRole::guess(&path));
        let label = gmail
            && selectable
            && !matches!(
                role,
                Some(FolderRole::All | FolderRole::Trash | FolderRole::Junk)
            );
        out.push((
            RemoteFolder {
                path,
                role,
                selectable,
                label,
            },
            raw,
        ));
    }
    Ok(out)
}

/// The SASL XOAUTH2 initial response for `user` and an access token.
fn xoauth2(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

/// Sends the XOAUTH2 response once. A server that refuses the token sends a continuation with
/// an error in JSON instead of a tagged `NO`; the empty answer to it makes the server finish with
/// `NO`, which becomes `SourceError::Auth`.
struct Xoauth2 {
    response: String,
    sent: bool,
}

impl async_imap::Authenticator for Xoauth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        if std::mem::replace(&mut self.sent, true) {
            String::new()
        } else {
            self.response.clone()
        }
    }
}

struct ImapSession {
    session: async_imap::Session<ImapStream>,
    names: HashMap<String, String>,
    gmail: bool,
    modseq: bool,
    timeout: Duration,
}

#[async_trait]
impl SourceSession for ImapSession {
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError> {
        let listed = list_folders(&mut self.session, self.gmail, self.timeout).await?;
        self.names.clear();
        let mut out = Vec::with_capacity(listed.len());
        for (folder, raw) in listed {
            self.names.insert(folder.path.clone(), raw);
            out.push(folder);
        }
        Ok(out)
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError> {
        let name = self
            .names
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_owned());
        let mailbox = timed(self.timeout, self.session.examine(&name))
            .await?
            .map_err(convert)?;
        let uid_validity = mailbox
            .uid_validity
            .ok_or_else(|| SourceError::Protocol(format!("{path} has no UIDVALIDITY")))?;
        self.modseq = mailbox.highest_modseq.is_some();
        Ok(FolderStatus {
            uid_validity: u64::from(uid_validity),
            uid_next: mailbox.uid_next.map(u64::from),
            highest_modseq: mailbox.highest_modseq,
            exists: u64::from(mailbox.exists),
            uids_ascend: true,
        })
    }

    async fn uids(&mut self) -> Result<Vec<u64>, SourceError> {
        let found = timed(self.timeout, self.session.uid_search("ALL"))
            .await?
            .map_err(convert)?;
        let mut uids: Vec<u64> = found.into_iter().map(u64::from).collect();
        uids.sort_unstable();
        Ok(uids)
    }

    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let set = uid_set(uids);
        let items = if self.gmail {
            "(UID FLAGS INTERNALDATE X-GM-MSGID X-GM-LABELS BODY.PEEK[])"
        } else {
            "(UID FLAGS INTERNALDATE BODY.PEEK[])"
        };
        let fetches: Vec<_> = timed(self.timeout, async {
            self.session
                .uid_fetch(&set, items)
                .await?
                .try_collect::<Vec<_>>()
                .await
        })
        .await?
        .map_err(convert)?;
        let mut out = Vec::with_capacity(fetches.len());
        for fetch in &fetches {
            let (Some(uid), Some(body)) = (fetch.uid, fetch.body()) else {
                continue;
            };
            out.push(RemoteMessage {
                uid: u64::from(uid),
                flags: fetch.flags().filter_map(flag_name).collect(),
                labels: self.labels(fetch),
                server_id: fetch.gmail_msg_id().copied(),
                received_at: fetch
                    .internal_date()
                    .map(|date| Timestamp::from_secs(date.timestamp())),
                raw: Bytes::copy_from_slice(body),
            });
        }
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    async fn flags(
        &mut self,
        uids: RangeInclusive<u64>,
        changed_since: Option<u64>,
    ) -> Result<Vec<RemoteFlags>, SourceError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        if changed_since.is_some() && !self.modseq {
            return Err(SourceError::Protocol(
                "the folder has no mod-sequences (CONDSTORE)".into(),
            ));
        }
        let set = format!("{}:{}", uids.start(), uids.end());
        let mut items = String::from(if self.gmail {
            "(UID FLAGS X-GM-LABELS)"
        } else {
            "(UID FLAGS)"
        });
        if let Some(since) = changed_since {
            items.push_str(&format!(" (CHANGEDSINCE {since})"));
        }
        let fetches: Vec<_> = timed(self.timeout, async {
            self.session
                .uid_fetch(&set, &items)
                .await?
                .try_collect::<Vec<_>>()
                .await
        })
        .await?
        .map_err(convert)?;
        let mut out: Vec<RemoteFlags> = fetches
            .iter()
            .filter_map(|fetch| {
                let uid = u64::from(fetch.uid?);
                uids.contains(&uid).then(|| RemoteFlags {
                    uid,
                    flags: fetch.flags().filter_map(flag_name).collect(),
                    labels: self.labels(fetch),
                })
            })
            .collect();
        out.sort_by_key(|f| f.uid);
        out.dedup_by_key(|f| f.uid);
        Ok(out)
    }

    async fn logout(&mut self) -> Result<(), SourceError> {
        timed(self.timeout, self.session.logout())
            .await?
            .map_err(convert)
    }
}

impl ImapSession {
    fn labels(&self, fetch: &async_imap::types::Fetch) -> Option<Vec<String>> {
        self.gmail.then(|| {
            fetch
                .gmail_labels()
                .map(|raw| labels::label_names(raw.iter().map(|l| l.as_ref())))
                .unwrap_or_default()
        })
    }
}

fn flag_name(flag: Flag<'_>) -> Option<String> {
    Some(match flag {
        Flag::Seen => "\\Seen".into(),
        Flag::Answered => "\\Answered".into(),
        Flag::Flagged => "\\Flagged".into(),
        Flag::Deleted => "\\Deleted".into(),
        Flag::Draft => "\\Draft".into(),
        Flag::Recent | Flag::MayCreate => return None,
        Flag::Custom(name) => name.into_owned(),
    })
}

/// `1,2,3,7,9` becomes `1:3,7,9`.
fn uid_set(uids: &[u64]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        });
        i += 1;
    }
    parts.join(",")
}

async fn timed<T>(
    timeout: Duration,
    work: impl std::future::Future<Output = T>,
) -> Result<T, SourceError> {
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| SourceError::Connection("the server did not answer in time".into()))
}

fn convert(error: async_imap::error::Error) -> SourceError {
    use async_imap::error::Error;
    match error {
        Error::Io(e) => SourceError::Connection(e.to_string()),
        Error::ConnectionLost => SourceError::Connection("connection lost".into()),
        other => SourceError::Protocol(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_watch;
#[cfg(test)]
mod tests_writer;
