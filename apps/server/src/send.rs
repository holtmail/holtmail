//! Sending: the transports the outbox hands messages to, the archiver it puts sent bytes in, and
//! the `holtmail send` and `holtmail outbox` commands.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, FolderId, MessageId, OutboxId, Timestamp, TlsRoots, UserId};
use holtmail_ingest::Ingest;
use holtmail_oauth::Provider;
use holtmail_outbox::{Archiver, Arrival, Outbox, OutboxConfig, OutboxError, Queue, Transports};
use holtmail_secret::SecretBox;
use holtmail_source_graph::GraphSettings;
use holtmail_source_imap::ImapSettings;
use holtmail_source_jmap::{JmapAuth, JmapSettings};
use holtmail_store::{
    AccountKind, AuditKind, Capability, GrantRole, InstanceRole, NewAuditEntry, OutboxQuery,
    OutboxState, RecipientState, Store, Transport, TransportKind,
};
use holtmail_submit::{Envelope, SubmitError, Submitter};
use holtmail_submit_graph::{GraphConfig, GraphSubmitter, StaticToken, TokenProvider};
use holtmail_submit_jmap::{JmapSecret, JmapSubmitConfig, JmapSubmitter};
use holtmail_submit_smtp::{Secret, Security, SmtpConfig, SmtpSubmitter};

use crate::config::{self, Config, Services, WriteConfig};
use crate::oauth::{OAuth, OAuthSettings};

const GMAIL_SMTP: (&str, u16) = ("smtp.gmail.com", 465);
const MICROSOFT_SMTP: (&str, u16) = ("smtp.office365.com", 587);

/// Builds the submitter of a stored transport, refusing whatever the instance or the account
/// does not allow.
pub struct ServerTransports {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
    oauth: Arc<OAuth>,
    switches: WriteConfig,
    roots: TlsRoots,
    instance_token: OnceLock<String>,
}

impl ServerTransports {
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretBox>,
        oauth: Arc<OAuth>,
        switches: WriteConfig,
    ) -> Self {
        Self {
            store,
            secrets,
            oauth,
            switches,
            roots: config::tls_roots(),
            instance_token: OnceLock::new(),
        }
    }

    /// Makes every SMTP transport refuse a server that greets with this instance's token.
    pub fn refuse_own_instance(&self, token: String) {
        let _ = self.instance_token.set(token);
    }

    fn smtp_submitter(&self, config: SmtpConfig) -> Result<Arc<dyn Submitter>, SubmitError> {
        let mut submitter = SmtpSubmitter::new(config)?.with_roots(&self.roots)?;
        if let Some(token) = self.instance_token.get() {
            submitter = submitter.refusing_instance(token.clone());
        }
        Ok(Arc::new(submitter))
    }

    fn open(&self, transport: &Transport) -> Result<Option<String>, SubmitError> {
        let Some(sealed) = transport.sealed_secret.as_deref() else {
            return Ok(None);
        };
        let secret = self
            .secrets
            .open(&format!("transport:{}", transport.id), sealed)
            .map_err(|_| SubmitError::Invalid("the stored password cannot be opened".into()))?;
        String::from_utf8(secret)
            .map(Some)
            .map_err(|_| SubmitError::Invalid("the stored password is damaged".into()))
    }

    fn smtp(&self, transport: &Transport) -> Result<SmtpConfig, SubmitError> {
        let host = setting(transport, "host")
            .ok_or_else(|| SubmitError::Invalid("the transport has no host".into()))?;
        let mut config = SmtpConfig::new(host);
        apply_overrides(&mut config, transport)?;
        if let Some(username) = setting(transport, "username") {
            let password = self
                .open(transport)?
                .ok_or_else(|| SubmitError::Invalid("the transport has no password".into()))?;
            config.username = Some(username.to_owned());
            config.secret = Some(Secret::Password(password));
        }
        Ok(config)
    }

    fn jmap(&self, transport: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        let url = setting(transport, "url")
            .ok_or_else(|| SubmitError::Invalid("the transport has no address".into()))?;
        let secret = self
            .open(transport)?
            .ok_or_else(|| SubmitError::Invalid("the transport has no password".into()))?;
        let secret = match setting(transport, "auth") {
            Some("token") => JmapSecret::Token(secret),
            _ => JmapSecret::Password(secret),
        };
        let config =
            JmapSubmitConfig::new(url, setting(transport, "username").unwrap_or(""), secret);
        Ok(Arc::new(
            JmapSubmitter::new(config)?.with_roots(&self.roots)?,
        ))
    }

    fn graph(&self, transport: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        let token = self
            .open(transport)?
            .ok_or_else(|| SubmitError::Invalid("the transport has no token".into()))?;
        let config = GraphConfig {
            api_url: setting(transport, "url")
                .unwrap_or(holtmail_submit_graph::GRAPH_URL)
                .to_owned(),
            mailbox: setting(transport, "mailbox").map(str::to_owned),
            roots: self.roots.clone(),
            ..GraphConfig::default()
        };
        Ok(Arc::new(GraphSubmitter::new(
            config,
            Arc::new(StaticToken::new(token)),
        )?))
    }

    async fn account(&self, transport: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        let account_id = transport
            .account_id
            .ok_or_else(|| SubmitError::Invalid("the transport's account was removed".into()))?;
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| SubmitError::Temporary(e.to_string()))?
            .ok_or_else(|| SubmitError::Invalid("the transport's account was removed".into()))?;
        let capabilities = self
            .store
            .account_capabilities(account.id)
            .await
            .map_err(|e| SubmitError::Temporary(e.to_string()))?;
        if !capabilities.has(Capability::MailSend) {
            return Err(SubmitError::Unavailable(
                "the account does not allow sending (mail.send is off)".into(),
            ));
        }
        if OAuthSettings::of(&account).is_some() && !crate::oauth::send_granted(&account) {
            return Err(self.consent(&account).await);
        }
        match account.kind {
            AccountKind::Imap => {
                let config = self.account_smtp(transport, &account).await?;
                self.smtp_submitter(config)
            }
            AccountKind::Jmap => {
                let settings = JmapSettings::from_json(&account.settings)
                    .map_err(|e| SubmitError::Invalid(e.to_string()))?;
                let secret = crate::accounts::open_secret(&account, &*self.secrets)
                    .map_err(|e| SubmitError::Invalid(e.to_string()))?;
                let secret = match settings.auth {
                    JmapAuth::Password => JmapSecret::Password(secret),
                    JmapAuth::Token => JmapSecret::Token(secret),
                };
                let config = JmapSubmitConfig::new(settings.url, settings.username, secret);
                Ok(Arc::new(
                    JmapSubmitter::new(config)?.with_roots(&self.roots)?,
                ))
            }
            AccountKind::Graph => {
                let settings = GraphSettings::from_json(&account.settings)
                    .map_err(|e| SubmitError::Invalid(e.to_string()))?;
                let config = GraphConfig {
                    api_url: settings
                        .api_url
                        .unwrap_or_else(|| holtmail_submit_graph::GRAPH_URL.to_owned()),
                    roots: self.roots.clone(),
                    ..GraphConfig::default()
                };
                let tokens = AccountToken {
                    parts: self.parts(),
                    account,
                };
                Ok(Arc::new(GraphSubmitter::new(config, Arc::new(tokens))?))
            }
            AccountKind::Import => Err(SubmitError::Invalid(
                "an import account has no server to send through".into(),
            )),
        }
    }

    fn parts(&self) -> Parts {
        Parts {
            store: self.store.clone(),
            secrets: self.secrets.clone(),
            oauth: self.oauth.clone(),
        }
    }

    /// The account's sign-in lacks the provider's send scope: the account asks for another
    /// sign-in and the message waits.
    async fn consent(&self, account: &holtmail_store::Account) -> SubmitError {
        if let Err(error) = self
            .oauth
            .ask_consent(account, &*self.store, Capability::MailSend.as_str())
            .await
        {
            tracing::warn!(account = %account.id, %error, "missing consent not recorded");
        }
        let provider = OAuthSettings::of(account)
            .and_then(|o| o.provider())
            .map_or("The provider", Provider::name);
        SubmitError::Unavailable(format!(
            "{provider} has not allowed sending for this account yet; sign in to the account again to give that permission"
        ))
    }

    async fn account_smtp(
        &self,
        transport: &Transport,
        account: &holtmail_store::Account,
    ) -> Result<SmtpConfig, SubmitError> {
        let imap = ImapSettings::from_json(&account.settings)
            .map_err(|e| SubmitError::Invalid(e.to_string()))?;
        let mut config;
        if let Some(oauth) = OAuthSettings::of(account) {
            let provider = oauth
                .provider()
                .ok_or_else(|| SubmitError::Invalid("unknown OAuth provider".into()))?;
            let (host, port, security) = match provider {
                Provider::Google => (GMAIL_SMTP.0, GMAIL_SMTP.1, Security::Tls),
                Provider::Microsoft => (MICROSOFT_SMTP.0, MICROSOFT_SMTP.1, Security::Starttls),
            };
            config = SmtpConfig::new(host);
            config.port = port;
            config.security = security;
            let token = self.parts().token(account).await?;
            config.username = Some(imap.username);
            config.secret = Some(Secret::OAuthToken(token));
        } else {
            config = SmtpConfig::new(imap.host.clone());
            match (setting(transport, "username"), self.open(transport)?) {
                (Some(username), Some(password)) => {
                    apply_overrides(&mut config, transport)?;
                    config.username = Some(username.to_owned());
                    config.secret = Some(Secret::Password(password));
                }
                _ => {
                    apply_overrides(&mut config, transport)?;
                    config.host = imap.host.clone();
                    let password = crate::accounts::open_secret(account, &*self.secrets)
                        .map_err(|e| SubmitError::Invalid(e.to_string()))?;
                    config.username = Some(imap.username);
                    config.secret = Some(Secret::Password(password));
                }
            }
        }
        Ok(config)
    }
}

/// What getting an account's access token needs.
#[derive(Clone)]
struct Parts {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
    oauth: Arc<OAuth>,
}

impl Parts {
    /// A fresh access token. A refused or revoked sign-in fails the message; anything else (the
    /// token endpoint is down, the store is busy) is tried again later.
    async fn token(&self, account: &holtmail_store::Account) -> Result<String, SubmitError> {
        match self
            .oauth
            .access_token(account, &*self.store, &*self.secrets)
            .await
        {
            Ok(token) => Ok(token),
            Err(error) => {
                let needs_sign_in = matches!(
                    self.store.get_account(account.id).await,
                    Ok(Some(current)) if OAuthSettings::of(&current).is_some_and(|s| s.needs_sign_in)
                );
                Err(if needs_sign_in {
                    SubmitError::Auth(error.to_string())
                } else {
                    SubmitError::Unavailable(error.to_string())
                })
            }
        }
    }
}

/// Hands Graph the account's OAuth access token, refreshed as needed.
struct AccountToken {
    parts: Parts,
    account: holtmail_store::Account,
}

#[async_trait]
impl TokenProvider for AccountToken {
    async fn token(&self) -> Result<String, SubmitError> {
        self.parts.token(&self.account).await
    }
}

fn setting<'a>(transport: &'a Transport, key: &str) -> Option<&'a str> {
    transport
        .settings
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
}

fn apply_overrides(config: &mut SmtpConfig, transport: &Transport) -> Result<(), SubmitError> {
    if let Some(host) = setting(transport, "host") {
        config.host = host.to_owned();
    }
    if let Some(security) = setting(transport, "security") {
        config.security = match security {
            "tls" => Security::Tls,
            "starttls" => Security::Starttls,
            "none" => Security::None,
            _ => return Err(SubmitError::Invalid("unknown security".into())),
        };
        config.port = if config.security == Security::Tls {
            465
        } else {
            587
        };
    }
    if let Some(port) = transport
        .settings
        .get("port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
    {
        config.port = port;
    }
    Ok(())
}

#[async_trait]
impl Transports for ServerTransports {
    async fn submitter(&self, transport: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        if !self.switches.enabled || !self.switches.send {
            return Err(SubmitError::Unavailable(
                "sending is turned off on this instance".into(),
            ));
        }
        match transport.kind {
            TransportKind::Smtp => self.smtp_submitter(self.smtp(transport)?),
            TransportKind::Jmap => self.jmap(transport),
            TransportKind::Graph => self.graph(transport),
            TransportKind::Account => self.account(transport).await,
        }
    }

    async fn may_send(&self, archive: ArchiveId, user: UserId) -> Result<bool, OutboxError> {
        let Some(user) = self.store.get_user(user).await? else {
            return Ok(false);
        };
        if user.role == InstanceRole::Owner {
            return Ok(true);
        }
        Ok(self
            .store
            .grant_roles(user.id)
            .await?
            .iter()
            .any(|g| g.archive == archive && g.role >= GrantRole::Sender))
    }
}

/// Tells the outbox about every message ingest adds, once [`open`] made the outbox: provider
/// copies of sent mail and bounces. Without an outbox (the command line) it does nothing.
#[derive(Default)]
pub struct SentMail(std::sync::OnceLock<Arc<Outbox>>);

#[async_trait]
impl holtmail_ingest::Observer for SentMail {
    async fn added(
        &self,
        archive: ArchiveId,
        message: MessageId,
        raw: &[u8],
        parsed: &holtmail_mime::ParsedMessage,
        placement: &holtmail_ingest::Placement,
    ) {
        let Some(outbox) = self.0.get() else {
            return;
        };
        let arrival = Arrival {
            archive,
            message,
            raw,
            parsed,
            folder: placement.folder,
            labels: placement.labels.as_deref(),
            live: placement.live,
        };
        if let Err(error) = outbox.observe(arrival).await {
            tracing::warn!(%message, %error, "message not matched against the outbox");
        }
    }
}

/// Archives sent bytes through the ingest pipeline and commits the index.
pub struct ServerArchiver(pub Ingest);

#[async_trait]
impl Archiver for ServerArchiver {
    async fn archive(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        folder: Option<FolderId>,
        flags: &[String],
    ) -> Result<MessageId, OutboxError> {
        let done = self
            .0
            .ingest(
                archive,
                raw,
                &holtmail_ingest::Placement {
                    folder,
                    flags: flags.to_vec(),
                    received_at: Some(Timestamp::now()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| OutboxError::Archive(e.to_string()))?;
        self.0
            .commit()
            .await
            .map_err(|e| OutboxError::Archive(e.to_string()))?;
        Ok(done.message_id)
    }
}

/// Only queues: the commands that run beside `serve` never archive or send.
struct QueueOnly;

#[async_trait]
impl Archiver for QueueOnly {
    async fn archive(
        &self,
        _: ArchiveId,
        _: Bytes,
        _: Option<FolderId>,
        _: &[String],
    ) -> Result<MessageId, OutboxError> {
        Err(OutboxError::Archive(
            "the command line only queues; the server sends".into(),
        ))
    }
}

fn outbox_config(config: &Config) -> OutboxConfig {
    OutboxConfig {
        undo_window: Duration::from_secs(config.write.undo_seconds),
        ..OutboxConfig::default()
    }
}

/// The outbox of the running server with its transports.
pub fn open(services: &Services, config: &Config) -> (Arc<Outbox>, Arc<ServerTransports>) {
    let transports = Arc::new(ServerTransports::new(
        services.store.clone(),
        services.secrets.clone(),
        services.oauth.clone(),
        config.write,
    ));
    let outbox = Outbox::new(
        services.store.clone(),
        services.blobs.clone(),
        transports.clone(),
        Arc::new(ServerArchiver(services.ingest())),
        outbox_config(config),
    );
    let outbox = Arc::new(outbox);
    let _ = services.sent_mail.0.set(outbox.clone());
    (outbox, transports)
}

/// Recovers sends a stopped process left behind, then runs the worker until the process ends.
pub async fn run(outbox: Arc<Outbox>) {
    if let Err(error) = outbox.recover_held().await {
        tracing::warn!(%error, "held messages of recorded changes not released");
    }
    match outbox.recover().await {
        Ok(ids) if !ids.is_empty() => {
            tracing::warn!(
                count = ids.len(),
                "sends interrupted by a restart are uncertain"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "interrupted sends not recovered"),
    }
    outbox.run().await;
}

/// What `holtmail send` was given.
pub struct SendArgs {
    pub archive: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub eml: Option<std::path::PathBuf>,
    pub at: Option<String>,
    pub wait: bool,
}

struct Cli {
    store: Arc<dyn Store>,
    outbox: Outbox,
}

async fn open_cli(config: &Config) -> anyhow::Result<Cli> {
    let store = config::open_store(config).await?;
    let blobs = config::open_blobs(config).await?;
    let secrets = config::open_secrets(config)?;
    let oauth = config::open_oauth(config)?;
    let transports = Arc::new(ServerTransports::new(
        store.clone(),
        secrets,
        oauth,
        config.write,
    ));
    let outbox = Outbox::new(
        store.clone(),
        blobs,
        transports,
        Arc::new(QueueOnly),
        outbox_config(config),
    );
    Ok(Cli { store, outbox })
}

fn crlf(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 32);
    let mut previous = 0u8;
    for &b in raw {
        if b == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        previous = b;
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn parse_time(raw: &str) -> anyhow::Result<Timestamp> {
    let at = chrono::DateTime::parse_from_rfc3339(raw.trim())
        .with_context(|| format!("--at {raw:?} is not an RFC 3339 time"))?;
    Ok(Timestamp::from_millis(at.timestamp_millis()))
}

async fn audit(store: &dyn Store, archive: ArchiveId, id: OutboxId, action: &str) {
    let entry = NewAuditEntry {
        at: Timestamp::now(),
        user: None,
        kind: AuditKind::OutboxChanged,
        archive: Some(archive),
        account: None,
        targets: vec![id.to_string(), action.to_owned()],
        count: 1,
    };
    if let Err(error) = store.append_audit(&entry).await {
        tracing::warn!(%error, "audit entry not stored");
    }
}

/// `holtmail send`: queues a message built from flags or read from an `.eml` file, sent as one
/// of the archive's identities. The running server sends it once the undo window ends.
pub async fn send(config: &Config, args: SendArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.write.enabled && config.write.send,
        "sending is turned off in [write]"
    );
    let cli = open_cli(config).await?;
    let store = &*cli.store;
    let archive = crate::export::find_archive(store, &args.archive).await?;
    let identities = store.list_identities(archive).await?;
    let wanted = args.from.trim().to_lowercase();
    let identity = identities
        .iter()
        .find(|i| i.id.to_string() == wanted || i.fields.address.eq_ignore_ascii_case(&wanted))
        .with_context(|| format!("no identity {:?} in this archive", args.from))?;
    let route_identities: Vec<holtmail_route::Identity> = identities
        .iter()
        .map(|i| holtmail_route::Identity {
            id: i.id.to_string(),
            address: i.fields.address.clone(),
            name: i.fields.name.clone(),
            transport: i.fields.transport_id.to_string(),
            account: i.fields.account_id.map(|a| a.to_string()),
            alias: i.fields.alias.clone(),
            active: i.fields.active,
            plus_addresses: i.fields.plus_addresses,
        })
        .collect();
    let transports: Vec<holtmail_route::Transport> = store
        .archive_transports(archive)
        .await?
        .into_iter()
        .map(|t| holtmail_route::Transport {
            id: t.id.to_string(),
            allowed_from: t.allowed_from,
        })
        .collect();
    let chosen = identity.id.to_string();
    let route = holtmail_route::route(holtmail_route::Request {
        identities: &route_identities,
        transports: &transports,
        context: &holtmail_route::Context::New,
        default_identity: None,
        chosen: Some(&chosen),
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (bytes, recipients) = match &args.eml {
        Some(path) => {
            anyhow::ensure!(
                args.subject.is_none() && args.body.is_none(),
                "--subject and --body do not go with --eml"
            );
            let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let bytes = crlf(&raw);
            let parsed = holtmail_mime::parse(&bytes).context("the file is not a message")?;
            let mut recipients: Vec<String> = args
                .to
                .iter()
                .chain(&args.cc)
                .chain(&args.bcc)
                .cloned()
                .collect();
            if recipients.is_empty() {
                recipients = parsed
                    .to
                    .iter()
                    .chain(&parsed.cc)
                    .map(|a| a.email.clone())
                    .collect();
            }
            (bytes, recipients)
        }
        None => {
            let body = match args.body.as_deref() {
                Some("-") => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
                    text
                }
                Some(text) => text.to_owned(),
                None => String::new(),
            };
            let mailboxes = |list: &[String]| {
                list.iter()
                    .map(|a| holtmail_compose::Mailbox::new(a.trim()))
                    .collect::<Vec<_>>()
            };
            let built = holtmail_compose::build(&holtmail_compose::Draft {
                from: holtmail_compose::Mailbox {
                    name: route.name.clone(),
                    address: route.from.clone(),
                },
                to: mailboxes(&args.to),
                cc: mailboxes(&args.cc),
                bcc: mailboxes(&args.bcc),
                reply_to: identity
                    .fields
                    .reply_to
                    .iter()
                    .map(|r| holtmail_compose::Mailbox::new(r.clone()))
                    .collect(),
                subject: args.subject.clone().unwrap_or_default(),
                text: body,
                ..Default::default()
            })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            (built.bytes, built.recipients)
        }
    };
    anyhow::ensure!(!recipients.is_empty(), "no recipients; pass --to");
    let send_at = args.at.as_deref().map(parse_time).transpose()?;
    let entry = cli
        .outbox
        .queue(Queue {
            archive,
            user: None,
            device: None,
            route: route.clone(),
            envelope: Envelope {
                mail_from: route.from.clone(),
                recipients,
            },
            bytes: Bytes::from(bytes),
            send_at,
            hold: false,
            flow_run: None,
            idempotency_key: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    audit(store, archive, entry.id, "queued").await;
    println!("Queued {} from {}.", entry.id, route.from);
    if !args.wait {
        println!("The running server sends it; `holtmail outbox list` shows how it went.");
        return Ok(());
    }
    let ended = wait_for(store, entry.id).await?;
    print_entry(&ended);
    if ended.state != OutboxState::Sent {
        std::process::exit(1);
    }
    Ok(())
}

async fn wait_for(store: &dyn Store, id: OutboxId) -> anyhow::Result<holtmail_store::OutboxEntry> {
    let started = std::time::Instant::now();
    loop {
        let entry = store.get_outbox(id).await?.context("the message is gone")?;
        let waiting = matches!(
            entry.state,
            OutboxState::Queued | OutboxState::Sending | OutboxState::Retrying
        );
        if !waiting || started.elapsed() > Duration::from_secs(600) {
            return Ok(entry);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn print_entry(entry: &holtmail_store::OutboxEntry) {
    let time = |t: Timestamp| {
        chrono::DateTime::from_timestamp_millis(t.as_millis())
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    };
    println!(
        "{}  {:<9}  {}  {} attempts  {}",
        entry.id,
        entry.state.as_str(),
        time(entry.new.send_at),
        entry.attempts,
        entry.last_error.as_deref().unwrap_or("")
    );
    for recipient in &entry.recipients {
        match &recipient.state {
            RecipientState::Pending => println!("    {}  pending", recipient.address),
            RecipientState::Accepted => println!("    {}  accepted", recipient.address),
            RecipientState::Refused { code, text, .. } => {
                println!("    {}  refused {code} {text}", recipient.address);
            }
            RecipientState::Bounced { status, text } => println!(
                "    {}  bounced {} {}",
                recipient.address,
                status.as_deref().unwrap_or(""),
                text.as_deref().unwrap_or("")
            ),
        }
    }
}

/// `holtmail outbox` subcommands.
pub enum OutboxArgs {
    List {
        archive: Option<String>,
        state: Option<String>,
    },
    Retry(String),
    Cancel(String),
}

/// `holtmail outbox`: lists messages, or queues one again or cancels it.
pub async fn outbox(config: &Config, args: OutboxArgs) -> anyhow::Result<()> {
    let cli = open_cli(config).await?;
    let store = &*cli.store;
    match args {
        OutboxArgs::List { archive, state } => {
            let states = match state.as_deref() {
                Some(raw) => raw
                    .split(',')
                    .map(|s| {
                        OutboxState::parse(s.trim()).with_context(|| format!("unknown state {s:?}"))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?,
                None => Vec::new(),
            };
            let archives = match archive {
                Some(name) => vec![crate::export::find_archive(store, &name).await?],
                None => store
                    .list_archives()
                    .await?
                    .into_iter()
                    .map(|a| a.id)
                    .collect(),
            };
            for archive in archives {
                for entry in store
                    .list_outbox(&OutboxQuery {
                        archive,
                        states: states.clone(),
                        before: None,
                        user: None,
                        limit: 100,
                    })
                    .await?
                {
                    print_entry(&entry);
                }
            }
            Ok(())
        }
        OutboxArgs::Retry(raw) | OutboxArgs::Cancel(raw) if raw.parse::<OutboxId>().is_err() => {
            bail!("{raw:?} is not an outbox id")
        }
        OutboxArgs::Retry(raw) => {
            anyhow::ensure!(
                config.write.enabled && config.write.send,
                "sending is turned off in [write]"
            );
            let entry = cli
                .outbox
                .retry(raw.parse()?, None)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            audit(store, entry.new.archive_id, entry.id, "retried").await;
            print_entry(&entry);
            Ok(())
        }
        OutboxArgs::Cancel(raw) => {
            let entry = cli
                .outbox
                .cancel(raw.parse()?)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            audit(store, entry.new.archive_id, entry.id, "cancelled").await;
            print_entry(&entry);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use holtmail_oauth::Client;
    use holtmail_oauth::testing::FakeTokenEndpoint;
    use holtmail_secret_local::LocalSecretBox;
    use holtmail_store::{ArchiveStore, Capabilities, NewAccount, WriteStore};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    struct Accounts {
        _dir: tempfile::TempDir,
        store: Arc<SqliteStore>,
        secrets: Arc<LocalSecretBox>,
        transports: ServerTransports,
        archive: ArchiveId,
    }

    async fn accounts() -> Accounts {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let secrets = Arc::new(LocalSecretBox::from_key([7; 32]));
        let endpoint = Arc::new(FakeTokenEndpoint::default());
        for _ in 0..8 {
            endpoint.tokens("access", None, 3600);
        }
        let oauth = Arc::new(OAuth::new(
            holtmail_oauth::Provider::ALL
                .into_iter()
                .map(Client::thunderbird)
                .collect(),
            endpoint,
        ));
        let archive = store.create_archive("Sending").await.unwrap().id;
        let transports = ServerTransports::new(
            store.clone(),
            secrets.clone(),
            oauth,
            WriteConfig::default(),
        );
        Accounts {
            _dir: dir,
            store,
            secrets,
            transports,
            archive,
        }
    }

    impl Accounts {
        async fn account(
            &self,
            kind: AccountKind,
            mut settings: serde_json::Value,
            oauth: Option<OAuthSettings>,
            send: bool,
        ) -> Transport {
            if let Some(oauth) = oauth {
                oauth.write(&mut settings);
            }
            let mut account = self
                .store
                .create_account(NewAccount {
                    archive_id: self.archive,
                    kind,
                    name: "Ada".into(),
                    address: Some("ada@example.com".into()),
                    settings,
                    sealed_secret: None,
                    enabled: true,
                })
                .await
                .unwrap();
            account.sealed_secret = Some(
                self.secrets
                    .seal(&account.id.to_string(), b"secret-1")
                    .unwrap(),
            );
            self.store.update_account(&account).await.unwrap();
            if send {
                self.store
                    .set_account_capabilities(
                        account.id,
                        &[Capability::MailSend].into_iter().collect::<Capabilities>(),
                    )
                    .await
                    .unwrap();
            }
            Transport {
                kind: TransportKind::Account,
                account_id: Some(account.id),
                archive_id: Some(self.archive),
                ..transport(serde_json::json!({}))
            }
        }

        async fn oauth_of(&self, transport: &Transport) -> OAuthSettings {
            let account = self
                .store
                .get_account(transport.account_id.unwrap())
                .await
                .unwrap()
                .unwrap();
            OAuthSettings::of(&account).unwrap()
        }
    }

    fn microsoft(send: bool) -> OAuthSettings {
        OAuthSettings {
            provider: "microsoft".into(),
            send,
            ..OAuthSettings::default()
        }
    }

    #[tokio::test]
    async fn account_transports_follow_the_account_kind() {
        let a = accounts().await;
        let jmap = a
            .account(
                AccountKind::Jmap,
                serde_json::json!({ "url": "https://jmap.example.com/.well-known/jmap", "username": "ada", "auth": "password" }),
                None,
                true,
            )
            .await;
        let built = a.transports.submitter(&jmap).await.unwrap();
        assert_eq!(built.limits().max_size, None);

        let graph = a
            .account(
                AccountKind::Graph,
                serde_json::json!({}),
                Some(microsoft(true)),
                true,
            )
            .await;
        let built = a.transports.submitter(&graph).await.unwrap();
        assert_eq!(
            built.limits().max_size,
            Some(holtmail_submit_graph::MAX_SIZE),
            "a Graph account does not send through Graph"
        );

        let off = a
            .account(AccountKind::Jmap, jmap_settings(), None, false)
            .await;
        assert!(matches!(
            a.transports.submitter(&off).await.err(),
            Some(SubmitError::Unavailable(_))
        ));
    }

    fn jmap_settings() -> serde_json::Value {
        serde_json::json!({ "url": "https://jmap.example.com/.well-known/jmap", "username": "ada", "auth": "password" })
    }

    #[tokio::test]
    async fn microsoft_accounts_wait_for_consent_to_send() {
        let a = accounts().await;
        let imap = serde_json::json!({ "host": "outlook.office365.com", "port": 993, "security": "tls", "username": "ada@example.com" });
        for (kind, settings) in [
            (AccountKind::Imap, imap.clone()),
            (AccountKind::Graph, serde_json::json!({})),
        ] {
            let transport = a
                .account(kind, settings, Some(microsoft(false)), true)
                .await;
            let error = a.transports.submitter(&transport).await.err().unwrap();
            assert!(
                matches!(&error, SubmitError::Unavailable(text) if text.contains("sign in")),
                "{error:?}"
            );
            let stored = a.oauth_of(&transport).await;
            assert_eq!(stored.missing_consent.as_deref(), Some("mail.send"));
            assert!(!stored.needs_sign_in, "syncs stopped for a missing consent");
            assert!(
                crate::oauth::consent_reason(&stored)
                    .unwrap()
                    .contains("Microsoft")
            );
        }
        let granted = a
            .account(AccountKind::Imap, imap, Some(microsoft(true)), true)
            .await;
        a.transports.submitter(&granted).await.unwrap();
        assert_eq!(a.oauth_of(&granted).await.missing_consent, None);

        let google = a
            .account(
                AccountKind::Imap,
                serde_json::json!({ "host": "imap.gmail.com", "port": 993, "security": "tls", "username": "ada@example.com" }),
                Some(OAuthSettings {
                    provider: "google".into(),
                    ..OAuthSettings::default()
                }),
                true,
            )
            .await;
        a.transports.submitter(&google).await.unwrap();
    }

    #[tokio::test]
    async fn jmap_and_graph_transports_need_their_secret() {
        let a = accounts().await;
        let mut jmap = transport(jmap_settings());
        jmap.kind = TransportKind::Jmap;
        let mut graph = transport(serde_json::json!({}));
        graph.kind = TransportKind::Graph;
        for t in [&jmap, &graph] {
            assert!(matches!(
                a.transports.submitter(t).await.err(),
                Some(SubmitError::Invalid(_))
            ));
        }
        for mut sealed in [jmap, graph] {
            sealed.sealed_secret = Some(
                a.secrets
                    .seal(&format!("transport:{}", sealed.id), b"token-1")
                    .unwrap(),
            );
            a.transports.submitter(&sealed).await.unwrap();
        }
    }

    #[test]
    fn makes_line_endings_crlf() {
        assert_eq!(crlf(b"a\nb\r\nc"), b"a\r\nb\r\nc\r\n");
        assert_eq!(crlf(b"a\r\n"), b"a\r\n");
    }

    fn transport(settings: serde_json::Value) -> Transport {
        Transport {
            id: holtmail_core::TransportId::new(),
            archive_id: None,
            name: "t".into(),
            kind: TransportKind::Smtp,
            account_id: None,
            settings,
            sealed_secret: None,
            allowed_from: vec![],
            limits: Default::default(),
            enabled: true,
            smtputf8: false,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn overrides_host_port_and_security() {
        let mut config = SmtpConfig::new("imap.example.com");
        apply_overrides(
            &mut config,
            &transport(serde_json::json!({ "security": "tls" })),
        )
        .unwrap();
        assert_eq!(
            (config.host.as_str(), config.port, config.security),
            ("imap.example.com", 465, Security::Tls)
        );
        apply_overrides(
            &mut config,
            &transport(serde_json::json!({ "host": "smtp.example.com", "port": 2525, "security": "starttls" })),
        )
        .unwrap();
        assert_eq!(
            (config.host.as_str(), config.port, config.security),
            ("smtp.example.com", 2525, Security::Starttls)
        );
        assert!(
            apply_overrides(
                &mut config,
                &transport(serde_json::json!({ "security": "ssl" }))
            )
            .is_err()
        );
    }
}
