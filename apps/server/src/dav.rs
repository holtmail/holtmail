use std::time::Duration;

use anyhow::{Context, bail};
use holtmail_api::{ContactsCalendars, ContactsCalendarsChange, ContactsProtocol, MailboxError};
use holtmail_collection::CollectionKind;
use holtmail_collection::{CollectionError, CollectionSource, CollectionWriter, Credentials};
use holtmail_collection_dav::{DavConfig, DavSource};
use holtmail_collection_graph::GraphCollections;
use holtmail_collection_jmap::JmapCollections;
use holtmail_collection_merge::MergedCollections;
use holtmail_oauth::{Access, Provider};
use holtmail_secret::SecretBox;
use holtmail_source_graph::GraphSettings;
use holtmail_source_imap::ImapSettings;
use holtmail_source_jmap::{JmapAuth, JmapSettings};
use holtmail_store::{Account, AccountKind, Store};
use serde::{Deserialize, Serialize};

use crate::oauth::{OAuth, OAuthSettings, granted_access};

pub const GOOGLE_CARDDAV: &str = "https://www.googleapis.com/.well-known/carddav";
pub const GOOGLE_CALDAV: &str = "https://apidata.googleusercontent.com/caldav/v2/";

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);

/// How an account's contacts and calendars are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Dav,
    Graph,
    Jmap,
}

impl Protocol {
    fn api(self) -> ContactsProtocol {
        match self {
            Self::Dav => ContactsProtocol::Dav,
            Self::Graph => ContactsProtocol::Graph,
            Self::Jmap => ContactsProtocol::Jmap,
        }
    }

    fn is_dav(&self) -> bool {
        *self == Self::Dav
    }
}

/// What `settings.dav` holds: whether contacts and calendars are archived, how and where from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DavSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Protocol::is_dav")]
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contacts_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendars_url: Option<String>,
    /// How calendars are read when not as `protocol`: CalDAV for a JMAP account whose session
    /// offers no calendars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendars_protocol: Option<Protocol>,
    /// Minutes between syncs; `None` follows the account's mail interval, 0 only on request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u64>,
    /// A CardDAV and CalDAV password of its own, sealed and base64-encoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dav_secret: Option<String>,
}

impl DavSettings {
    pub fn calendars(&self) -> Protocol {
        self.calendars_protocol.unwrap_or(self.protocol)
    }

    fn uses_dav(&self) -> bool {
        self.protocol == Protocol::Dav || self.calendars() == Protocol::Dav
    }

    pub fn of(account: &Account) -> Option<Self> {
        serde_json::from_value(account.settings.get("dav")?.clone()).ok()
    }

    pub fn write(&self, settings: &mut serde_json::Value) {
        if !settings.is_object() {
            *settings = serde_json::json!({});
        }
        settings["dav"] = serde_json::to_value(self).expect("plain settings serialize");
    }
}

/// The CardDAV and CalDAV addresses of well-known providers for an address on their domains.
pub fn preset(address: &str) -> Option<(&'static str, &'static str)> {
    let domain = address.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    Some(match domain.as_str() {
        "fastmail.com"
        | "fastmail.fm"
        | "fastmail.net"
        | "fastmail.org"
        | "fastmail.us"
        | "fastmail.to"
        | "fastmail.cn"
        | "fastmail.es"
        | "fastmail.in"
        | "fastmail.jp"
        | "fastmail.se"
        | "messagingengine.com"
        | "sent.com"
        | "imap.cc" => (
            "https://carddav.fastmail.com",
            "https://caldav.fastmail.com",
        ),
        "icloud.com" | "me.com" | "mac.com" => {
            ("https://contacts.icloud.com", "https://caldav.icloud.com")
        }
        "yandex.ru" | "yandex.com" | "yandex.by" | "yandex.kz" | "yandex.ua" | "ya.ru" => {
            ("https://carddav.yandex.ru", "https://caldav.yandex.ru")
        }
        _ => return None,
    })
}

fn user_name(account: &Account) -> Option<String> {
    match account.kind {
        AccountKind::Imap => ImapSettings::from_json(&account.settings)
            .ok()
            .map(|s| s.username),
        AccountKind::Jmap => JmapSettings::from_json(&account.settings)
            .ok()
            .map(|s| s.username),
        _ => None,
    }
}

fn mail_address(account: &Account) -> Option<String> {
    account
        .address
        .clone()
        .filter(|a| a.contains('@'))
        .or_else(|| user_name(account).filter(|u| u.contains('@')))
}

/// Whether the account signs in with Google over IMAP; those read contacts and calendars from
/// Google's own CardDAV and CalDAV addresses with the same sign-in.
fn google(account: &Account) -> bool {
    account.kind == AccountKind::Imap
        && OAuthSettings::of(account).and_then(|o| o.provider()) == Some(Provider::Google)
}

/// Whether the account signs in with Microsoft over IMAP. Its sign-in grants IMAP only, so its
/// contacts and calendars would need a second Microsoft sign-in for Graph; such accounts are
/// pointed at a Microsoft Graph account instead.
pub fn microsoft_imap(account: &Account) -> bool {
    account.kind == AccountKind::Imap
        && OAuthSettings::of(account).and_then(|o| o.provider()) == Some(Provider::Microsoft)
}

/// Whether the account's kind can have contacts and calendars.
pub fn available(account: &Account) -> bool {
    match account.kind {
        AccountKind::Imap => !microsoft_imap(account),
        AccountKind::Jmap | AccountKind::Graph => true,
        AccountKind::Import => false,
    }
}

fn enabled(account: &Account) -> bool {
    DavSettings::of(account).is_some_and(|d| d.enabled)
}

/// What a sign-in for `account` should ask for: with contacts and calendars on, IMAP plus
/// CardDAV and CalDAV for Google, and mail plus contacts and calendars for Microsoft Graph; else
/// what its kind needs.
pub fn wanted_access(account: &Account) -> Access {
    match account.kind {
        AccountKind::Imap if google(account) && enabled(account) => Access::ImapDav,
        AccountKind::Graph if enabled(account) => Access::GraphPim,
        kind => crate::oauth::access_for(kind),
    }
}

/// Whether a Google or Graph account has contacts and calendars on but its sign-in does not
/// grant them.
pub fn needs_consent(account: &Account) -> bool {
    (google(account) || account.kind == AccountKind::Graph)
        && enabled(account)
        && !granted_access(account).covers(wanted_access(account))
}

pub fn details(account: &Account) -> Option<ContactsCalendars> {
    if !available(account) {
        return None;
    }
    let settings = DavSettings::of(account).unwrap_or_default();
    Some(ContactsCalendars {
        enabled: settings.enabled,
        needs_consent: needs_consent(account),
        protocol: settings.protocol.api(),
        calendars_protocol: settings.calendars().api(),
        interval_minutes: settings.interval_minutes,
        has_dav_password: settings.dav_secret.is_some(),
        caldav_offer: None,
        contacts_url: settings.contacts_url,
        calendars_url: settings.calendars_url,
    })
}

/// Minutes between contacts and calendars syncs, as set for them; `None` follows the mail
/// interval.
pub fn interval(account: &Account) -> Option<u64> {
    DavSettings::of(account).and_then(|d| d.interval_minutes)
}

/// Whether `account` reads anything over CardDAV or CalDAV, where WebDAV-Push may apply.
pub fn uses_dav(account: &Account) -> bool {
    DavSettings::of(account).is_some_and(|d| d.enabled && d.uses_dav())
}

fn dav_context(account: &Account) -> String {
    format!("{}:dav", account.id)
}

fn open_secret(account: &Account, secrets: &dyn SecretBox) -> anyhow::Result<String> {
    let sealed = account
        .sealed_secret
        .as_deref()
        .context("the account has no stored password")?;
    String::from_utf8(
        secrets
            .open(&account.id.to_string(), sealed)
            .context("the stored password cannot be opened with this secret key")?,
    )
    .context("the stored password is not UTF-8")
}

/// How the account signs in to its contacts and calendars server: a fresh Google access token,
/// a JMAP account's token, or the mail user name with the mail password.
async fn credentials(
    account: &Account,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<Credentials> {
    if OAuthSettings::of(account).is_some() {
        return Ok(Credentials::Bearer(
            oauth.access_token(account, store, secrets).await?,
        ));
    }
    let secret = open_secret(account, secrets)?;
    if account.kind == AccountKind::Jmap
        && JmapSettings::from_json(&account.settings).is_ok_and(|s| s.auth == JmapAuth::Token)
    {
        return Ok(Credentials::Bearer(secret));
    }
    Ok(Credentials::Password {
        username: user_name(account).context("the account has no user name")?,
        password: secret,
    })
}

/// How the account signs in to CardDAV and CalDAV: with its own DAV password when one is
/// stored, else as [`credentials`] says.
async fn dav_credentials(
    account: &Account,
    settings: &DavSettings,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<Credentials> {
    let Some(sealed) = &settings.dav_secret else {
        return credentials(account, store, secrets, oauth).await;
    };
    use base64::Engine;
    let sealed = base64::engine::general_purpose::STANDARD
        .decode(sealed)
        .context("the stored DAV password is unreadable")?;
    let password = String::from_utf8(
        secrets
            .open(&dav_context(account), &sealed)
            .context("the stored DAV password cannot be opened with this secret key")?,
    )
    .context("the stored DAV password is not UTF-8")?;
    Ok(Credentials::Password {
        username: user_name(account)
            .or_else(|| mail_address(account))
            .context("the account has no user name")?,
        password,
    })
}

fn config(
    settings: &DavSettings,
    only: Option<CollectionKind>,
    credentials: Credentials,
    timeout: Duration,
) -> DavConfig {
    DavConfig {
        contacts: settings
            .contacts_url
            .clone()
            .filter(|_| only != Some(CollectionKind::Calendar)),
        calendars: settings
            .calendars_url
            .clone()
            .filter(|_| only != Some(CollectionKind::Contacts)),
        credentials,
        timeout,
    }
}

fn graph_source(
    account: &Account,
    credentials: Credentials,
    timeout: Duration,
) -> Result<GraphCollections, CollectionError> {
    let settings = GraphSettings::from_json(&account.settings)
        .map_err(|e| CollectionError::Config(e.to_string()))?;
    GraphCollections::new(settings.api_url.as_deref(), credentials, timeout)?
        .with_roots(&crate::config::tls_roots())
}

fn jmap_source(
    account: &Account,
    credentials: Credentials,
    timeout: Duration,
) -> Result<JmapCollections, CollectionError> {
    let settings = JmapSettings::from_json(&account.settings)
        .map_err(|e| CollectionError::Config(e.to_string()))?;
    let url = holtmail_source_jmap::session_url(&settings.url)
        .map_err(|e| CollectionError::Config(e.to_string()))?;
    JmapCollections::new(&url, credentials, timeout)?.with_roots(&crate::config::tls_roots())
}

/// The source `settings` describe for `account`: one protocol, or address books from one and
/// calendars from another. `dav` signs in to CardDAV and CalDAV, `own` to the others.
fn build(
    account: &Account,
    settings: &DavSettings,
    own: Credentials,
    dav: Credentials,
    timeout: Duration,
) -> Result<Box<dyn CollectionSource>, CollectionError> {
    let one = |protocol: Protocol,
               only: Option<CollectionKind>|
     -> Result<Box<dyn CollectionSource>, CollectionError> {
        Ok(match protocol {
            Protocol::Dav => Box::new(
                DavSource::new(config(settings, only, dav.clone(), timeout))?
                    .with_roots(&crate::config::tls_roots())?,
            ),
            Protocol::Graph => Box::new(graph_source(account, own.clone(), timeout)?),
            Protocol::Jmap => Box::new(jmap_source(account, own.clone(), timeout)?),
        })
    };
    if settings.protocol == settings.calendars() {
        return one(settings.protocol, None);
    }
    Ok(Box::new(MergedCollections::new(
        one(settings.protocol, Some(CollectionKind::Contacts))?,
        one(settings.calendars(), Some(CollectionKind::Calendar))?,
    )))
}

async fn both_credentials(
    account: &Account,
    settings: &DavSettings,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<(Credentials, Credentials)> {
    let own = credentials(account, store, secrets, oauth).await?;
    let dav = if settings.dav_secret.is_some() && settings.uses_dav() {
        dav_credentials(account, settings, store, secrets, oauth).await?
    } else {
        own.clone()
    };
    Ok((own, dav))
}

/// The contacts and calendars source of an account that has them on.
pub async fn source_for(
    account: &Account,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<Box<dyn CollectionSource>> {
    let settings = DavSettings::of(account)
        .filter(|d| d.enabled)
        .context("contacts and calendars are off for this account")?;
    if needs_consent(account) {
        bail!(
            "Sign in again with {} to allow access to contacts and calendars.",
            if account.kind == AccountKind::Graph {
                "Microsoft"
            } else {
                "Google"
            }
        );
    }
    let (own, dav) = both_credentials(account, &settings, store, secrets, oauth).await?;
    Ok(build(account, &settings, own, dav, SYNC_TIMEOUT)?)
}

fn upstream<T: CollectionSource + CollectionWriter + 'static>(
    source: T,
) -> holtmail_pim_writeback::Upstream {
    let source = std::sync::Arc::new(source);
    holtmail_pim_writeback::Upstream {
        writer: source.clone(),
        source,
    }
}

/// The writer of the account's collections of `kind`, with the source that reads them back:
/// the one protocol those collections are read with (so a JMAP account with CalDAV calendars
/// writes calendars over CalDAV), signed in as for syncing. `own` are the account's addresses,
/// which a CalDAV writer matches attendees against. A CardDAV, CalDAV or Graph writer has
/// listed its collections before it is returned, so it knows what each can do (Graph: which
/// calendars the account may only read).
pub async fn upstream_for(
    account: &Account,
    kind: CollectionKind,
    own: Vec<String>,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<holtmail_pim_writeback::Upstream> {
    let settings = DavSettings::of(account)
        .filter(|d| d.enabled)
        .context("contacts and calendars are off for this account")?;
    if needs_consent(account) {
        bail!("the sign-in does not allow access to contacts and calendars");
    }
    let (own_credentials, dav) =
        both_credentials(account, &settings, store, secrets, oauth).await?;
    let protocol = match kind {
        CollectionKind::Contacts => settings.protocol,
        CollectionKind::Calendar => settings.calendars(),
    };
    Ok(match protocol {
        Protocol::Dav => {
            let source = DavSource::new(config(&settings, Some(kind), dav, SYNC_TIMEOUT))?
                .with_roots(&crate::config::tls_roots())?
                .with_addresses(own);
            source.collections().await?;
            upstream(source)
        }
        Protocol::Graph => {
            let source = graph_source(account, own_credentials, SYNC_TIMEOUT)?;
            source.collections().await?;
            upstream(source)
        }
        Protocol::Jmap => upstream(jmap_source(account, own_credentials, SYNC_TIMEOUT)?),
    })
}

/// Where a JMAP account without calendars may have them over CalDAV, best first: its
/// provider's address, then the well-known addresses of its JMAP host and its mail domain.
fn caldav_candidates(account: &Account) -> Vec<String> {
    let mut out = Vec::new();
    if let Some((_, calendars)) = mail_address(account).as_deref().and_then(preset) {
        out.push(calendars.to_owned());
    }
    if let Ok(settings) = JmapSettings::from_json(&account.settings)
        && let Ok(url) = holtmail_source_jmap::session_url(&settings.url)
        && let Ok(url) = url::Url::parse(&url)
        && let Some(host) = url.host_str()
    {
        out.push(match url.port() {
            Some(port) => format!("{}://{host}:{port}/.well-known/caldav", url.scheme()),
            None => format!("{}://{host}/.well-known/caldav", url.scheme()),
        });
    }
    if let Some(domain) = mail_address(account)
        .as_deref()
        .and_then(|a| a.rsplit_once('@'))
        .map(|(_, d)| d.trim().to_ascii_lowercase())
    {
        out.push(format!("https://{domain}/.well-known/caldav"));
    }
    out.dedup();
    out
}

/// The first of `candidates` where CalDAV answers with `credentials` and lists a calendar.
async fn find_caldav(candidates: &[String], credentials: &Credentials) -> Option<String> {
    for url in candidates {
        let source = DavSource::new(DavConfig {
            contacts: None,
            calendars: Some(url.clone()),
            credentials: credentials.clone(),
            timeout: CHECK_TIMEOUT,
        })
        .and_then(|s| s.with_roots(&crate::config::tls_roots()));
        let Ok(source) = source else { continue };
        if let Ok(found) = source.collections().await
            && found.iter().any(|c| c.kind == CollectionKind::Calendar)
        {
            return Some(url.clone());
        }
    }
    None
}

/// For a JMAP account that reads contacts over JMAP and has no calendars: a CalDAV address
/// that lists calendars with the account's sign-in, to offer. `None` for every other account,
/// and when none answers.
pub async fn caldav_offer(
    account: &Account,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> Option<String> {
    let settings = DavSettings::of(account).filter(|d| d.enabled)?;
    if account.kind != AccountKind::Jmap
        || settings.protocol != Protocol::Jmap
        || settings.calendars() != Protocol::Jmap
    {
        return None;
    }
    let own = credentials(account, store, secrets, oauth).await.ok()?;
    let jmap = jmap_source(account, own, CHECK_TIMEOUT).ok()?;
    if jmap.offers_calendars().await.unwrap_or(true) {
        return None;
    }
    let dav = dav_credentials(account, &settings, store, secrets, oauth)
        .await
        .ok()?;
    find_caldav(&caldav_candidates(account), &dav).await
}

/// A failure to reach contacts and calendars, in words for people; never holds a secret.
pub fn reason(error: &CollectionError) -> String {
    match error {
        CollectionError::Auth(_) => {
            "The contacts and calendars server refused the sign-in. Some providers need an app password."
                .into()
        }
        CollectionError::Connection(reason) => format!("Could not connect: {reason}."),
        CollectionError::Protocol(reason) => {
            format!("The contacts and calendars server answered unexpectedly: {reason}.")
        }
        CollectionError::Config(reason) => format!("Check the address: {reason}."),
        CollectionError::Gone => "There is no address book or calendar at this address.".into(),
        other => other.to_string(),
    }
}

fn addresses(
    account: &Account,
    change: &ContactsCalendarsChange,
) -> Result<(String, String, bool), MailboxError> {
    if google(account) {
        return Ok((GOOGLE_CARDDAV.into(), GOOGLE_CALDAV.into(), false));
    }
    fn given(u: &Option<String>) -> Option<&str> {
        u.as_deref().map(str::trim).filter(|u| !u.is_empty())
    }
    match (given(&change.contacts_url), given(&change.calendars_url)) {
        (Some(c), Some(k)) => Ok((c.into(), k.into(), false)),
        (Some(one), None) | (None, Some(one)) => Ok((one.into(), one.into(), false)),
        (None, None) => {
            let address = mail_address(account).ok_or_else(|| {
                MailboxError::Invalid(
                    "Enter the contacts and calendars address; it cannot be found without a mail address."
                        .into(),
                )
            })?;
            if let Some((contacts, calendars)) = preset(&address) {
                return Ok((contacts.into(), calendars.into(), true));
            }
            let domain = address
                .rsplit_once('@')
                .map(|(_, d)| d.trim())
                .unwrap_or_default();
            Ok((
                format!("https://{domain}/.well-known/carddav"),
                format!("https://{domain}/.well-known/caldav"),
                true,
            ))
        }
    }
}

/// Applies `change` to `account`'s settings: turning on finds the addresses (Google's for
/// Google sign-in, the given ones, or detected ones) and signs in to them first, except for a
/// Google account that has not granted access yet, which is turned on and waits for its next
/// sign-in. Turning off keeps the addresses and everything archived.
pub async fn configure(
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
    account: &mut Account,
    change: &ContactsCalendarsChange,
) -> Result<(), MailboxError> {
    if !available(account) {
        return Err(MailboxError::Invalid(if microsoft_imap(account) {
            "Contacts and calendars of Microsoft accounts are read through Microsoft Graph. Add this mailbox as a Microsoft Graph account to archive them.".into()
        } else {
            "An import account has no server.".into()
        }));
    }
    if !change.enabled {
        let mut settings = DavSettings::of(account).unwrap_or_default();
        settings.enabled = false;
        settings.write(&mut account.settings);
        return Ok(());
    }
    let invalid = |e: anyhow::Error| MailboxError::Invalid(format!("{e:#}"));
    fn given(u: &Option<String>) -> Option<String> {
        u.as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
    }
    let (given_contacts, given_calendars) =
        (given(&change.contacts_url), given(&change.calendars_url));
    let stored = DavSettings::of(account).unwrap_or_default();
    let mut candidate = account.clone();
    let mut next = DavSettings {
        enabled: true,
        interval_minutes: stored.interval_minutes,
        dav_secret: stored.dav_secret.clone(),
        ..DavSettings::default()
    };
    match change.dav_password.as_deref() {
        Some("") => next.dav_secret = None,
        Some(password) => {
            use base64::Engine;
            let sealed = secrets
                .seal(&dav_context(account), password.as_bytes())
                .map_err(|e| MailboxError::Backend(e.to_string()))?;
            next.dav_secret = Some(base64::engine::general_purpose::STANDARD.encode(sealed));
        }
        None => {}
    }
    let mut detected = false;
    let mut probe_calendars = false;
    if account.kind == AccountKind::Graph {
        next.protocol = Protocol::Graph;
    } else if account.kind == AccountKind::Jmap && given_contacts.is_none() {
        next.protocol = Protocol::Jmap;
        next.write(&mut candidate.settings);
        let credentials = credentials(&candidate, store, secrets, oauth)
            .await
            .map_err(invalid)?;
        let jmap = jmap_source(&candidate, credentials, CHECK_TIMEOUT)
            .map_err(|e| MailboxError::Invalid(reason(&e)))?;
        let contacts = jmap
            .offers_contacts()
            .await
            .map_err(|e| MailboxError::Invalid(reason(&e)))?;
        let calendars = jmap
            .offers_calendars()
            .await
            .map_err(|e| MailboxError::Invalid(reason(&e)))?;
        if !contacts {
            next.protocol = Protocol::Dav;
        } else if given_calendars.is_some() || !calendars {
            next.calendars_protocol = Some(Protocol::Dav);
            match &given_calendars {
                Some(url) => next.calendars_url = Some(url.clone()),
                None => probe_calendars = true,
            }
        }
    } else if account.kind != AccountKind::Graph {
        next.protocol = Protocol::Dav;
    }
    if probe_calendars {
        let dav = dav_credentials(&candidate, &next, store, secrets, oauth)
            .await
            .map_err(invalid)?;
        match find_caldav(&caldav_candidates(account), &dav).await {
            Some(url) => next.calendars_url = Some(url),
            None => next.calendars_protocol = None,
        }
    }
    if next.protocol == Protocol::Dav {
        let (contacts, calendars, found) = addresses(account, change)?;
        next.contacts_url = Some(contacts);
        next.calendars_url = Some(calendars);
        detected = found;
    }
    next.write(&mut candidate.settings);
    if !needs_consent(&candidate) {
        let (own, dav) = both_credentials(&candidate, &next, store, secrets, oauth)
            .await
            .map_err(invalid)?;
        let source = build(&candidate, &next, own, dav, CHECK_TIMEOUT)
            .map_err(|e| MailboxError::Invalid(reason(&e)))?;
        match source.collections().await {
            Ok(found) => {
                tracing::info!(account = %account.id, collections = found.len(), "contacts and calendars found");
            }
            Err(error) if detected => {
                return Err(MailboxError::Invalid(format!(
                    "Contacts and calendars were not found automatically ({}). Enter the address your provider gives for CardDAV and CalDAV.",
                    reason(&error).trim_end_matches('.')
                )));
            }
            Err(error) => return Err(MailboxError::Invalid(reason(&error))),
        }
    }
    account.settings = candidate.settings;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_core::{AccountId, ArchiveId, Timestamp};

    fn account(kind: AccountKind, settings: serde_json::Value) -> Account {
        Account {
            id: AccountId::new(),
            archive_id: ArchiveId::new(),
            kind,
            name: "Ada".into(),
            address: Some("ada@example.com".into()),
            settings,
            sealed_secret: None,
            enabled: true,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn presets_for_known_providers() {
        assert_eq!(
            preset("ada@fastmail.com"),
            Some((
                "https://carddav.fastmail.com",
                "https://caldav.fastmail.com"
            ))
        );
        assert_eq!(
            preset("Ada@ICLOUD.com"),
            Some(("https://contacts.icloud.com", "https://caldav.icloud.com"))
        );
        assert_eq!(preset("ada@ya.ru").unwrap().0, "https://carddav.yandex.ru");
        assert_eq!(preset("ada@example.com"), None);
        assert_eq!(preset("not an address"), None);
    }

    #[test]
    fn google_accounts_need_consent_until_signed_in_for_dav() {
        let mut google = account(
            AccountKind::Imap,
            serde_json::json!({ "host": "imap.gmail.com", "port": 993, "security": "tls", "username": "ada@gmail.com",
                "oauth": { "provider": "google" } }),
        );
        assert!(available(&google));
        assert!(!needs_consent(&google), "off needs nothing");
        assert_eq!(wanted_access(&google), Access::Imap);
        DavSettings {
            enabled: true,
            ..DavSettings::default()
        }
        .write(&mut google.settings);
        assert!(needs_consent(&google));
        assert_eq!(wanted_access(&google), Access::ImapDav);
        google.settings["oauth"]["access"] = "imap_dav".into();
        assert!(!needs_consent(&google));
        let change = ContactsCalendarsChange {
            enabled: true,
            contacts_url: Some("https://elsewhere.example.com".into()),
            calendars_url: None,
            dav_password: None,
        };
        let (contacts, calendars, _) = addresses(&google, &change).unwrap();
        assert_eq!(
            (contacts.as_str(), calendars.as_str()),
            (GOOGLE_CARDDAV, GOOGLE_CALDAV),
            "Google's own addresses, whatever is entered"
        );
    }

    #[test]
    fn microsoft_imap_and_import_accounts_have_none() {
        let microsoft = account(
            AccountKind::Imap,
            serde_json::json!({ "oauth": { "provider": "microsoft" } }),
        );
        assert!(!available(&microsoft));
        assert!(microsoft_imap(&microsoft));
        assert!(!available(&account(
            AccountKind::Import,
            serde_json::json!({})
        )));
        assert!(details(&microsoft).is_none());
    }

    #[test]
    fn graph_accounts_need_consent_until_signed_in_for_contacts() {
        let mut graph = account(
            AccountKind::Graph,
            serde_json::json!({ "oauth": { "provider": "microsoft", "access": "graph" } }),
        );
        assert!(available(&graph));
        assert_eq!(wanted_access(&graph), Access::Graph);
        assert!(!needs_consent(&graph));
        DavSettings {
            enabled: true,
            protocol: Protocol::Graph,
            ..DavSettings::default()
        }
        .write(&mut graph.settings);
        assert_eq!(graph.settings["dav"]["protocol"], "graph");
        assert_eq!(wanted_access(&graph), Access::GraphPim);
        assert!(needs_consent(&graph));
        assert_eq!(details(&graph).unwrap().protocol, ContactsProtocol::Graph);
        graph.settings["oauth"]["access"] = "graph_pim".into();
        assert!(!needs_consent(&graph));
        let old = DavSettings::of(&account(
            AccountKind::Imap,
            serde_json::json!({ "dav": { "enabled": true, "contacts_url": "https://dav.example.com" } }),
        ))
        .unwrap();
        assert_eq!(
            old.protocol,
            Protocol::Dav,
            "settings stored before protocols"
        );
    }

    #[test]
    fn addresses_are_given_detected_or_shared() {
        let imap = account(
            AccountKind::Imap,
            serde_json::json!({ "host": "mail.example.com", "username": "ada@example.com" }),
        );
        let change = |c: Option<&str>, k: Option<&str>| ContactsCalendarsChange {
            enabled: true,
            contacts_url: c.map(Into::into),
            calendars_url: k.map(Into::into),
            dav_password: None,
        };
        assert_eq!(
            addresses(&imap, &change(None, None)).unwrap(),
            (
                "https://example.com/.well-known/carddav".into(),
                "https://example.com/.well-known/caldav".into(),
                true
            )
        );
        assert_eq!(
            addresses(
                &imap,
                &change(Some("https://dav.example.com/remote.php/dav"), Some(" "))
            )
            .unwrap(),
            (
                "https://dav.example.com/remote.php/dav".into(),
                "https://dav.example.com/remote.php/dav".into(),
                false
            )
        );
        let mut fastmail = imap.clone();
        fastmail.address = Some("ada@fastmail.com".into());
        assert_eq!(
            addresses(&fastmail, &change(None, None)).unwrap().1,
            "https://caldav.fastmail.com"
        );
    }
}
