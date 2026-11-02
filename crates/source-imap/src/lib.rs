//! A [`Source`] for IMAP servers.

mod stream;
mod utf7;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_imap::types::{Flag, NameAttribute};
use async_trait::async_trait;
use bytes::Bytes;
use futures::TryStreamExt;
use holtmail_core::Timestamp;
use holtmail_source::{
    FolderStatus, RemoteFolder, RemoteMessage, Source, SourceError, SourceSession,
};
use holtmail_store::FolderRole;
use serde::{Deserialize, Serialize};
use stream::ImapStream;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Account settings stored in `Account.settings`; the password is sealed separately.
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

    fn port(&self) -> u16 {
        self.port.unwrap_or(match self.security {
            Security::Tls => 993,
            Security::Starttls | Security::Plain => 143,
        })
    }
}

/// An IMAP account.
pub struct ImapSource {
    settings: ImapSettings,
    password: String,
    tls: Arc<tokio_rustls::rustls::ClientConfig>,
}

impl ImapSource {
    /// A source for the server in `settings`, signing in with `password`.
    pub fn new(settings: ImapSettings, password: String) -> Self {
        let roots = tokio_rustls::rustls::RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        );
        let tls = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self {
            settings,
            password,
            tls: Arc::new(tls),
        }
    }
}

#[async_trait]
impl Source for ImapSource {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError> {
        let (stream, greeting) = timed(ImapStream::connect(
            &self.settings.host,
            self.settings.port(),
            self.settings.security,
            self.tls.clone(),
        ))
        .await??;
        let mut client = async_imap::Client::new(stream);
        if greeting {
            timed(client.read_response())
                .await?
                .map_err(|e| SourceError::Connection(e.to_string()))?
                .ok_or_else(|| SourceError::Connection("closed before the greeting".into()))?;
        }
        let session = timed(client.login(&self.settings.username, &self.password))
            .await?
            .map_err(|(error, _)| match error {
                async_imap::error::Error::No(message) | async_imap::error::Error::Bad(message) => {
                    SourceError::Auth(message)
                }
                other => convert(other),
            })?;
        Ok(Box::new(ImapSession {
            session,
            names: HashMap::new(),
        }))
    }
}

struct ImapSession {
    session: async_imap::Session<ImapStream>,
    names: HashMap<String, String>,
}

#[async_trait]
impl SourceSession for ImapSession {
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError> {
        let names: Vec<_> = timed(async {
            self.session
                .list(Some(""), Some("*"))
                .await?
                .try_collect::<Vec<_>>()
                .await
        })
        .await?
        .map_err(convert)?;
        let mut out = Vec::with_capacity(names.len());
        self.names.clear();
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
            self.names.insert(path.clone(), raw);
            out.push(RemoteFolder {
                path,
                role,
                selectable,
            });
        }
        Ok(out)
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError> {
        let name = self
            .names
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_owned());
        let mailbox = timed(self.session.examine(&name)).await?.map_err(convert)?;
        let uid_validity = mailbox
            .uid_validity
            .ok_or_else(|| SourceError::Protocol(format!("{path} has no UIDVALIDITY")))?;
        Ok(FolderStatus {
            uid_validity: u64::from(uid_validity),
            uid_next: mailbox.uid_next.map(u64::from),
            highest_modseq: mailbox.highest_modseq,
            exists: u64::from(mailbox.exists),
        })
    }

    async fn uids(&mut self) -> Result<Vec<u64>, SourceError> {
        let found = timed(self.session.uid_search("ALL"))
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
        let fetches: Vec<_> = timed(async {
            self.session
                .uid_fetch(&set, "(UID FLAGS INTERNALDATE BODY.PEEK[])")
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
                received_at: fetch
                    .internal_date()
                    .map(|date| Timestamp::from_secs(date.timestamp())),
                raw: Bytes::copy_from_slice(body),
            });
        }
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    async fn logout(&mut self) -> Result<(), SourceError> {
        timed(self.session.logout()).await?.map_err(convert)
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

async fn timed<T>(work: impl std::future::Future<Output = T>) -> Result<T, SourceError> {
    tokio::time::timeout(COMMAND_TIMEOUT, work)
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
