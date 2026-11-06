use anyhow::{Context, bail};
use holtmail_core::ArchiveId;
use holtmail_oauth::{Access, OAuthError, Provider, read_redirect};
use holtmail_secret::SecretBox;
use holtmail_source::{Credentials, Source, SourceError};
use holtmail_source_graph::{GraphSettings, GraphSource};
use holtmail_source_imap::{ImapSettings, ImapSource, Security};
use holtmail_source_jmap::{JmapAuth, JmapSettings, JmapSource};
use holtmail_store::{Account, AccountKind, NewAccount, Store};
use holtmail_sync::{SyncError, SyncOptions, SyncReport, Syncer};
use serde::Deserialize;

use holtmail_api::{
    AccountChange, ConnectionChange, ImapConnection, JmapConnection, JmapConnectionChange,
    MailboxError,
};

use crate::config::Services;
use crate::oauth::{OAuth, OAuthSettings, Pending, Purpose};

pub struct NewImapAccount {
    pub archive: ArchiveId,
    pub name: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub security: Security,
    pub username: String,
    pub password: String,
    pub exclude: Vec<String>,
}

pub async fn add_imap(
    store: &dyn Store,
    secrets: &dyn SecretBox,
    new: NewImapAccount,
) -> anyhow::Result<Account> {
    let archive = new.archive;
    let settings = ImapSettings {
        host: new.host,
        port: new.port,
        security: new.security,
        username: new.username.clone(),
    };
    let mut json = serde_json::to_value(&settings)?;
    if !new.exclude.is_empty() {
        json["folders"] = serde_json::json!({ "exclude": new.exclude });
    }
    let mut account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: new.name.unwrap_or_else(|| new.username.clone()),
            address: new.username.contains('@').then(|| new.username.clone()),
            settings: json,
            sealed_secret: None,
            enabled: true,
        })
        .await?;
    account.sealed_secret = Some(secrets.seal(&account.id.to_string(), new.password.as_bytes())?);
    store.update_account(&account).await?;
    Ok(account)
}

pub struct NewJmapAccount {
    pub archive: ArchiveId,
    pub name: Option<String>,
    pub settings: JmapSettings,
    pub secret: String,
    pub exclude: Vec<String>,
}

pub async fn add_jmap(
    store: &dyn Store,
    secrets: &dyn SecretBox,
    new: NewJmapAccount,
) -> anyhow::Result<Account> {
    let mut json = serde_json::to_value(&new.settings)?;
    if !new.exclude.is_empty() {
        json["folders"] = serde_json::json!({ "exclude": new.exclude });
    }
    let username = new.settings.username.trim().to_owned();
    let mut account = store
        .create_account(NewAccount {
            archive_id: new.archive,
            kind: AccountKind::Jmap,
            name: new.name.unwrap_or_else(|| jmap_name(&new.settings)),
            address: username.contains('@').then_some(username),
            settings: json,
            sealed_secret: None,
            enabled: true,
        })
        .await?;
    account.sealed_secret = Some(secrets.seal(&account.id.to_string(), new.secret.as_bytes())?);
    store.update_account(&account).await?;
    Ok(account)
}

fn jmap_name(settings: &JmapSettings) -> String {
    if !settings.username.trim().is_empty() {
        return settings.username.trim().to_owned();
    }
    url::Url::parse(&settings.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "JMAP".to_owned())
}

pub fn parse_jmap_auth(raw: &str) -> anyhow::Result<JmapAuth> {
    match raw {
        "password" => Ok(JmapAuth::Password),
        "token" => Ok(JmapAuth::Token),
        other => bail!("unknown sign-in {other:?}; use password or token"),
    }
}

pub fn jmap_auth_name(auth: JmapAuth) -> &'static str {
    match auth {
        JmapAuth::Password => "password",
        JmapAuth::Token => "token",
    }
}

/// JMAP settings from what someone typed: the address made a session address, checked like
/// the source checks it.
pub fn jmap_settings(url: &str, username: &str, auth: JmapAuth) -> anyhow::Result<JmapSettings> {
    let settings = JmapSettings {
        url: holtmail_source_jmap::session_url(url).map_err(|e| anyhow::anyhow!(jmap_reason(e)))?,
        username: username.trim().to_owned(),
        auth,
    };
    anyhow::ensure!(
        auth == JmapAuth::Token || !settings.username.is_empty(),
        "the user name is empty"
    );
    Ok(settings)
}

/// How a JMAP account connects, as stored.
pub fn jmap_connection(account: &Account) -> Option<JmapConnection> {
    if account.kind != AccountKind::Jmap {
        return None;
    }
    let settings = JmapSettings::from_json(&account.settings).ok()?;
    Some(JmapConnection {
        url: settings.url,
        username: settings.username,
        auth: jmap_auth_name(settings.auth).to_owned(),
        exclude: options(account).exclude,
    })
}

/// The account's settings with the JMAP `change` applied; other keys stay.
pub fn with_jmap_connection(
    settings: &serde_json::Value,
    change: &JmapConnectionChange,
) -> anyhow::Result<(serde_json::Value, JmapSettings)> {
    let jmap = jmap_settings(
        &change.url,
        &change.username,
        parse_jmap_auth(&change.auth)?,
    )?;
    let mut json = match settings {
        serde_json::Value::Object(_) => settings.clone(),
        _ => serde_json::json!({}),
    };
    let fields = serde_json::to_value(&jmap)?;
    if let (Some(target), Some(fields)) = (json.as_object_mut(), fields.as_object()) {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
    let exclude: Vec<String> = change
        .exclude
        .iter()
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .collect();
    let mut folders = json
        .get("folders")
        .cloned()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    folders["exclude"] = serde_json::json!(exclude);
    json["folders"] = folders;
    Ok((json, jmap))
}

fn jmap_credentials(auth: JmapAuth, secret: String) -> Credentials {
    match auth {
        JmapAuth::Password => Credentials::Password(secret),
        JmapAuth::Token => Credentials::AccessToken(secret),
    }
}

fn jmap_reason(error: SourceError) -> String {
    match error {
        SourceError::Config(reason) | SourceError::Protocol(reason) => {
            let mut chars = reason.chars();
            chars.next().map_or_else(String::new, |first| {
                format!("{}{}.", first.to_uppercase(), chars.as_str())
            })
        }
        other => other.to_string(),
    }
}

/// A JMAP sign-in failure in words for people; never holds the password or token.
pub fn jmap_sign_in_error(error: SourceError, settings: &JmapSettings) -> MailboxError {
    MailboxError::Invalid(match error {
        SourceError::Auth(_) if settings.auth == JmapAuth::Token => {
            "The server refused this token. Check that it is current and allows mail access."
                .to_owned()
        }
        SourceError::Auth(_) => {
            "The server refused this user name and password. Some providers need an app password or an API token."
                .to_owned()
        }
        SourceError::Connection(reason) => format!("Could not connect: {reason}."),
        SourceError::Protocol(reason) => {
            format!("The JMAP server answered unexpectedly: {reason}.")
        }
        config @ SourceError::Config(_) => jmap_reason(config),
    })
}

/// Signs in to a JMAP server and lists its mailboxes, to check settings before they are stored.
pub async fn check_jmap_sign_in(settings: &JmapSettings, secret: &str) -> Result<(), MailboxError> {
    let source = JmapSource::new(
        settings.clone(),
        jmap_credentials(settings.auth, secret.to_owned()),
    )
    .map_err(|e| jmap_sign_in_error(e, settings))?;
    let mut session = source
        .connect()
        .await
        .map_err(|e| jmap_sign_in_error(e, settings))?;
    session
        .folders()
        .await
        .map_err(|e| jmap_sign_in_error(e, settings))?;
    let _ = session.logout().await;
    Ok(())
}

fn open_secret(account: &Account, secrets: &dyn SecretBox) -> anyhow::Result<String> {
    let sealed = account
        .sealed_secret
        .as_deref()
        .context("the account has no stored password")?;
    let secret = secrets
        .open(&account.id.to_string(), sealed)
        .context("the stored password cannot be opened with this secret key")?;
    String::from_utf8(secret).context("the stored password is not UTF-8")
}

#[derive(Debug, Default, Deserialize)]
struct FolderRules {
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn options(account: &Account) -> SyncOptions {
    let rules: FolderRules = account
        .settings
        .get("folders")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    SyncOptions {
        include: rules.include,
        exclude: rules.exclude,
        min_age: min_age_days(account)
            .map(|days| std::time::Duration::from_secs(u64::from(days) * 24 * 60 * 60)),
        ..SyncOptions::default()
    }
}

/// The longest minimum age an account can wait for, in days.
pub const MAX_MIN_AGE_DAYS: u32 = 3650;

/// How many days old a message must be before the account archives it, when it waits at all.
pub fn min_age_days(account: &Account) -> Option<u32> {
    account
        .settings
        .get("min_age_days")
        .and_then(serde_json::Value::as_u64)
        .and_then(|days| u32::try_from(days).ok())
        .filter(|days| *days > 0)
}

/// The account's settings with its minimum age set, or removed for `None`.
pub fn with_min_age_days(settings: &serde_json::Value, days: Option<u32>) -> serde_json::Value {
    let mut json = match settings {
        serde_json::Value::Object(_) => settings.clone(),
        _ => serde_json::json!({}),
    };
    if let Some(object) = json.as_object_mut() {
        match days.filter(|days| *days > 0) {
            Some(days) => {
                object.insert("min_age_days".into(), days.into());
            }
            None => {
                object.remove("min_age_days");
            }
        }
    }
    json
}

/// The account's own sync interval in minutes, when it has one.
pub fn sync_interval(account: &Account) -> Option<u64> {
    account
        .settings
        .get("sync_interval_minutes")
        .and_then(serde_json::Value::as_u64)
}

pub fn security_name(security: Security) -> &'static str {
    match security {
        Security::Tls => "tls",
        Security::Starttls => "starttls",
        Security::Plain => "plain",
    }
}

/// How an IMAP account connects, as stored.
pub fn connection(account: &Account) -> Option<ImapConnection> {
    if account.kind != AccountKind::Imap {
        return None;
    }
    let settings = ImapSettings::from_json(&account.settings).ok()?;
    Some(ImapConnection {
        port: settings.port(),
        security: security_name(settings.security).to_owned(),
        host: settings.host,
        username: settings.username,
        exclude: options(account).exclude,
    })
}

/// The account's settings with `change` applied; other keys, such as folder includes, stay.
pub fn with_connection(
    settings: &serde_json::Value,
    change: &ConnectionChange,
) -> anyhow::Result<(serde_json::Value, ImapSettings)> {
    let imap = ImapSettings {
        host: change.host.trim().to_owned(),
        port: change.port,
        security: parse_security(&change.security)?,
        username: change.username.trim().to_owned(),
    };
    anyhow::ensure!(!imap.host.is_empty(), "the server is empty");
    anyhow::ensure!(!imap.username.is_empty(), "the user name is empty");
    let mut json = match settings {
        serde_json::Value::Object(_) => settings.clone(),
        _ => serde_json::json!({}),
    };
    let fields = serde_json::to_value(&imap)?;
    if let (Some(target), Some(fields)) = (json.as_object_mut(), fields.as_object()) {
        target.remove("port");
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
    let exclude: Vec<String> = change
        .exclude
        .iter()
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .collect();
    let mut folders = json
        .get("folders")
        .cloned()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    folders["exclude"] = serde_json::json!(exclude);
    json["folders"] = folders;
    Ok((json, imap))
}

/// The account's settings with its own sync interval set, or removed for `None`.
pub fn with_sync_interval(settings: &serde_json::Value, minutes: Option<u64>) -> serde_json::Value {
    let mut json = match settings {
        serde_json::Value::Object(_) => settings.clone(),
        _ => serde_json::json!({}),
    };
    if let Some(object) = json.as_object_mut() {
        match minutes {
            Some(minutes) => {
                object.insert("sync_interval_minutes".into(), minutes.into());
            }
            None => {
                object.remove("sync_interval_minutes");
            }
        }
    }
    json
}

pub fn parse_security(raw: &str) -> anyhow::Result<Security> {
    match raw {
        "tls" => Ok(Security::Tls),
        "starttls" => Ok(Security::Starttls),
        "plain" => Ok(Security::Plain),
        other => bail!("unknown security {other:?}; use tls, starttls or plain"),
    }
}

/// Whether `next` is another IMAP server than `stored`: another host or port, or a move from an
/// encrypted connection to plain text. A stored password is never sent to such a server.
pub fn imap_moves(stored: &ImapSettings, next: &ImapSettings) -> bool {
    !stored.host.trim().eq_ignore_ascii_case(next.host.trim())
        || stored.port() != next.port()
        || (next.security == Security::Plain && stored.security != Security::Plain)
}

/// Whether `next` is served from another origin (scheme, host and port) than `stored`. A
/// stored password or token is never sent to such a server; an unreadable address counts as
/// another one.
pub fn jmap_moves(stored: &JmapSettings, next: &JmapSettings) -> bool {
    match (url::Url::parse(&stored.url), url::Url::parse(&next.url)) {
        (Ok(stored), Ok(next)) => stored.origin() != next.origin(),
        _ => true,
    }
}

/// Whether `change` points `account` at another server than the one it signs in to now, by
/// [`imap_moves`] or [`jmap_moves`]. False for a change that cannot be read; applying it fails.
pub fn moves_server(account: &Account, change: &AccountChange) -> bool {
    if let Some(connection) = &change.connection {
        let Ok((_, next)) = with_connection(&account.settings, connection) else {
            return false;
        };
        return ImapSettings::from_json(&account.settings)
            .ok()
            .is_none_or(|old| imap_moves(&old, &next));
    }
    if let Some(connection) = &change.jmap {
        let Ok((_, next)) = with_jmap_connection(&account.settings, connection) else {
            return false;
        };
        return JmapSettings::from_json(&account.settings)
            .ok()
            .is_none_or(|old| jmap_moves(&old, &next));
    }
    false
}

fn moved_without_secret(token: bool) -> MailboxError {
    MailboxError::Invalid(if token {
        "Enter the token again when changing the server.".into()
    } else {
        "Enter the password again when changing the server.".into()
    })
}

/// How `account` signs in now: its sealed password, or a fresh access token for OAuth.
pub async fn credentials(
    account: &Account,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<Credentials> {
    if OAuthSettings::of(account).is_some() {
        return Ok(Credentials::AccessToken(
            oauth.access_token(account, store, secrets).await?,
        ));
    }
    let secret = open_secret(account, secrets)?;
    if account.kind == AccountKind::Jmap {
        let settings = JmapSettings::from_json(&account.settings)?;
        return Ok(jmap_credentials(settings.auth, secret));
    }
    Ok(Credentials::Password(secret))
}

pub async fn source_for(
    account: &Account,
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
) -> anyhow::Result<Box<dyn Source>> {
    match account.kind {
        AccountKind::Imap => {
            let settings = ImapSettings::from_json(&account.settings)?;
            let credentials = credentials(account, store, secrets, oauth).await?;
            Ok(Box::new(ImapSource::new(settings, credentials)))
        }
        AccountKind::Jmap => {
            let settings = JmapSettings::from_json(&account.settings)?;
            let credentials = credentials(account, store, secrets, oauth).await?;
            Ok(Box::new(JmapSource::new(settings, credentials)?))
        }
        AccountKind::Graph => {
            let settings = GraphSettings::from_json(&account.settings)?;
            let credentials = credentials(account, store, secrets, oauth).await?;
            Ok(Box::new(GraphSource::new(&settings, credentials)?))
        }
        AccountKind::Import => bail!("import accounts have no server"),
    }
}

pub async fn sync_one(
    services: &Services,
    syncer: &Syncer,
    account: &Account,
) -> anyhow::Result<SyncReport> {
    let source = source_for(
        account,
        &*services.store,
        &*services.secrets,
        &services.oauth,
    )
    .await?;
    match syncer
        .sync_account(account, &*source, &options(account))
        .await
    {
        Ok(report) => Ok(report),
        Err(SyncError::Source(SourceError::Auth(reason)))
            if OAuthSettings::of(account).is_some() =>
        {
            services.oauth.forget(account.id);
            bail!("The mail server refused the access token: {reason}")
        }
        Err(error) => Err(error.into()),
    }
}

/// Applies `change` to `account` and stores it: a new connection or password is checked by
/// signing in first, and nothing is stored when that fails. Returns the stored account.
/// `Invalid` for a server setting on an import account, or an interval or minimum age out of
/// range; for a move to another server ([`moves_server`]) without a new password or token, so a
/// stored one never reaches a server it was not given for; and for any change of server, port
/// or security on an OAuth account. Whether the account is syncing is the caller's to check.
pub async fn configure(
    store: &dyn Store,
    secrets: &dyn SecretBox,
    oauth: &OAuth,
    account: &Account,
    change: AccountChange,
) -> Result<Account, MailboxError> {
    let jmap = account.kind == AccountKind::Jmap;
    if jmap && change.connection.is_some() {
        return Err(MailboxError::Invalid(
            "This is a JMAP account; change its JMAP connection instead.".into(),
        ));
    }
    if !jmap && change.jmap.is_some() {
        return Err(MailboxError::Invalid(
            "Only JMAP accounts have a JMAP connection.".into(),
        ));
    }
    let connects =
        change.connection.is_some() || change.jmap.is_some() || change.password.is_some();
    if account.kind == AccountKind::Graph && connects {
        return Err(MailboxError::Invalid(
            "A Microsoft Graph account has no server or password; sign in again with Microsoft to change its sign-in.".into(),
        ));
    }
    if account.kind == AccountKind::Import
        && (connects || change.sync_interval_minutes.is_some() || change.min_age_days.is_some())
    {
        return Err(MailboxError::Invalid(
            "An import account has no server.".into(),
        ));
    }
    if let Some(Some(minutes)) = change.sync_interval_minutes {
        crate::schedule::check_minutes(minutes)?;
    }
    if let Some(Some(days)) = change.min_age_days
        && days > MAX_MIN_AGE_DAYS
    {
        return Err(MailboxError::Invalid(format!(
            "Wait at most {MAX_MIN_AGE_DAYS} days before archiving mail."
        )));
    }
    let mut updated = account.clone();
    if connects && jmap {
        configure_jmap(secrets, account, &change, &mut updated).await?;
    } else if connects {
        let (settings, imap) = match &change.connection {
            Some(connection) => with_connection(&account.settings, connection)
                .map_err(|e| MailboxError::Invalid(format!("{e:#}")))?,
            None => (
                account.settings.clone(),
                ImapSettings::from_json(&account.settings)
                    .map_err(|e| MailboxError::Invalid(e.to_string()))?,
            ),
        };
        let signs_in_with = OAuthSettings::of(account);
        if let Some(oauth) = &signs_in_with
            && change.password.is_some()
        {
            return Err(MailboxError::Invalid(format!(
                "This account signs in with {}; it has no password.",
                oauth.provider().map_or("OAuth", Provider::name)
            )));
        }
        let stored = ImapSettings::from_json(&account.settings).ok();
        if let Some(oauth) = &signs_in_with
            && stored.as_ref().is_none_or(|old| {
                !old.host.trim().eq_ignore_ascii_case(imap.host.trim())
                    || old.port() != imap.port()
                    || old.security != imap.security
            })
        {
            return Err(MailboxError::Invalid(format!(
                "This account signs in with {}; its server, port and security are fixed.",
                oauth.provider().map_or("OAuth", Provider::name)
            )));
        }
        if signs_in_with.is_none()
            && change.password.is_none()
            && stored.as_ref().is_none_or(|old| imap_moves(old, &imap))
        {
            return Err(moved_without_secret(false));
        }
        let credentials = match &change.password {
            Some(password) => Credentials::Password(password.clone()),
            None if signs_in_with.is_some() => credentials(account, store, secrets, oauth)
                .await
                .map_err(|e| MailboxError::Invalid(format!("{e:#}")))?,
            None => {
                let sealed = account.sealed_secret.as_deref().ok_or_else(|| {
                    MailboxError::Invalid("Enter the password for this account.".into())
                })?;
                Credentials::Password(
                    String::from_utf8(
                        secrets
                            .open(&account.id.to_string(), sealed)
                            .map_err(backend)?,
                    )
                    .map_err(backend)?,
                )
            }
        };
        let password = match &credentials {
            Credentials::Password(password) => Some(password.clone()),
            Credentials::AccessToken(_) => None,
        };
        check_sign_in(&imap, credentials).await?;
        if let Some(connection) = &change.connection
            && let Some(old) = self::connection(account)
            && connection.username.trim() != old.username
            && (account.address.is_none() || account.address.as_deref() == Some(&old.username))
        {
            updated.address = imap.username.contains('@').then(|| imap.username.clone());
        }
        updated.settings = settings;
        if change.password.is_some()
            && let Some(password) = password
        {
            updated.sealed_secret = Some(
                secrets
                    .seal(&account.id.to_string(), password.as_bytes())
                    .map_err(backend)?,
            );
        }
    }
    if let Some(minutes) = change.sync_interval_minutes {
        updated.settings = with_sync_interval(&updated.settings, minutes);
    }
    if let Some(days) = change.min_age_days {
        updated.settings = with_min_age_days(&updated.settings, days);
    }
    if let Some(dav) = &change.contacts_calendars {
        crate::dav::configure(store, secrets, oauth, &mut updated, dav).await?;
    }
    store.update_account(&updated).await.map_err(backend)?;
    Ok(updated)
}

async fn configure_jmap(
    secrets: &dyn SecretBox,
    account: &Account,
    change: &AccountChange,
    updated: &mut Account,
) -> Result<(), MailboxError> {
    let old = JmapSettings::from_json(&account.settings).ok();
    let (settings, jmap) = match &change.jmap {
        Some(connection) => with_jmap_connection(&account.settings, connection)
            .map_err(|e| MailboxError::Invalid(format!("{e:#}")))?,
        None => (
            account.settings.clone(),
            old.clone().ok_or_else(|| {
                MailboxError::Invalid("The stored JMAP settings are unreadable.".into())
            })?,
        ),
    };
    if change.password.is_none() && old.as_ref().is_none_or(|old| jmap_moves(old, &jmap)) {
        return Err(moved_without_secret(jmap.auth == JmapAuth::Token));
    }
    let secret = match &change.password {
        Some(secret) => secret.clone(),
        None if old.as_ref().is_some_and(|o| o.auth == jmap.auth) => {
            open_secret(account, secrets).map_err(|e| MailboxError::Backend(format!("{e:#}")))?
        }
        None => {
            return Err(MailboxError::Invalid(match jmap.auth {
                JmapAuth::Token => "Enter the token to sign in with.".into(),
                JmapAuth::Password => "Enter the password to sign in with.".into(),
            }));
        }
    };
    check_jmap_sign_in(&jmap, &secret).await?;
    if let Some(old) = &old
        && jmap.username != old.username
        && (account.address.is_none() || account.address.as_deref() == Some(&old.username))
    {
        updated.address = jmap.username.contains('@').then(|| jmap.username.clone());
    }
    updated.settings = settings;
    if change.password.is_some() {
        updated.sealed_secret = Some(
            secrets
                .seal(&account.id.to_string(), secret.as_bytes())
                .map_err(backend)?,
        );
    }
    Ok(())
}

fn place(settings: &ImapSettings) -> String {
    format!("{}:{}", settings.host, settings.port())
}

/// A sign-in failure in words for people; never holds the password or token.
pub fn sign_in_error(error: SourceError, settings: &ImapSettings, oauth: bool) -> MailboxError {
    let place = place(settings);
    MailboxError::Invalid(match error {
        SourceError::Auth(_) if oauth => format!(
            "Signed in, but {} refused access for {}. Sign in as that address, and check that IMAP is turned on for it.",
            settings.host, settings.username
        ),
        SourceError::Auth(_) => {
            "The server refused this user name and password. Some providers need an app password."
                .to_owned()
        }
        SourceError::Connection(reason) => {
            let reason = reason
                .strip_prefix(&format!("{place}: "))
                .unwrap_or(&reason);
            format!("Could not connect to {place}: {reason}")
        }
        SourceError::Protocol(reason) => format!("{place} answered unexpectedly: {reason}"),
        SourceError::Config(reason) => reason,
    })
}

/// Signs in and lists the folders, to check settings before they are stored.
pub async fn check_sign_in(
    settings: &ImapSettings,
    credentials: Credentials,
) -> Result<(), MailboxError> {
    let oauth = matches!(credentials, Credentials::AccessToken(_));
    let source = ImapSource::new(settings.clone(), credentials);
    let mut session = source
        .connect()
        .await
        .map_err(|e| sign_in_error(e, settings, oauth))?;
    session
        .folders()
        .await
        .map_err(|e| sign_in_error(e, settings, oauth))?;
    let _ = session.logout().await;
    Ok(())
}

/// A Microsoft Graph sign-in failure in words for people; never holds the token.
pub fn graph_sign_in_error(error: SourceError) -> MailboxError {
    MailboxError::Invalid(match error {
        SourceError::Auth(_) => "Signed in, but Microsoft Graph refused access to the mailbox. Check that the address has an Exchange Online or Outlook.com mailbox and that reading mail was allowed.".to_owned(),
        SourceError::Connection(reason) => format!("Could not reach Microsoft Graph: {reason}."),
        SourceError::Protocol(reason) => format!("Microsoft Graph answered unexpectedly: {reason}."),
        SourceError::Config(reason) => reason,
    })
}

/// Lists the mailbox's folders through Graph with `credentials`, to check a sign-in before it
/// is stored, and returns the signed-in address when Graph names it.
pub async fn check_graph_sign_in(
    settings: &serde_json::Value,
    credentials: Credentials,
) -> Result<Option<String>, MailboxError> {
    let graph =
        GraphSettings::from_json(settings).map_err(|e| MailboxError::Invalid(e.to_string()))?;
    let source = GraphSource::new(&graph, credentials).map_err(graph_sign_in_error)?;
    let mut session = source.connect().await.map_err(graph_sign_in_error)?;
    session.folders().await.map_err(graph_sign_in_error)?;
    let _ = session.logout().await;
    match source.address().await {
        Ok(address) => Ok(address),
        Err(SourceError::Auth(_)) => Ok(None),
        Err(error) => Err(graph_sign_in_error(error)),
    }
}

fn oauth_refusal(provider: Provider, error: OAuthError) -> MailboxError {
    let name = provider.name();
    MailboxError::Invalid(match error {
        OAuthError::Redirect(reason) => format!(
            "That is not the address {name} sent you to ({reason}). Copy the whole address of the page that opened after you allowed access."
        ),
        OAuthError::Denied(reason) => format!("{name} did not grant access: {reason}."),
        OAuthError::Revoked(_) => {
            "This sign-in code was already used or has expired. Start the sign-in again.".into()
        }
        OAuthError::Rejected(reason) => format!("{name} refused the sign-in: {reason}"),
        OAuthError::Connection(reason) | OAuthError::Protocol(reason) => {
            format!("Could not finish the sign-in with {name}: {reason}")
        }
        OAuthError::Random => "The server could not create a sign-in.".into(),
    })
}

fn backend(error: impl std::fmt::Display) -> MailboxError {
    MailboxError::Backend(error.to_string())
}

/// Reads the provider's answer, takes the waiting sign-in it belongs to and finishes it: the
/// code is exchanged, the mail server must accept the token, then the account is created or
/// updated with the refresh token sealed. Nothing is stored when a step fails.
pub async fn finish_oauth(services: &Services, answer: &str) -> Result<Account, MailboxError> {
    let redirect = match read_redirect(answer) {
        Ok(redirect) => redirect,
        Err(OAuthError::Denied(reason)) => {
            if let Some(state) = url_state(answer) {
                services.oauth.take(&state);
            }
            return Err(MailboxError::Invalid(format!(
                "Access was not granted: {reason}."
            )));
        }
        Err(error) => {
            return Err(MailboxError::Invalid(format!(
                "That is not the address the sign-in page sent you to: {}. Copy the whole address of the page that opened after you allowed access.",
                match error {
                    OAuthError::Redirect(reason) => reason,
                    other => other.to_string(),
                }
            )));
        }
    };
    let pending = services.oauth.take(&redirect.state).ok_or_else(|| {
        MailboxError::Invalid(
            "This sign-in is unknown or has expired. Start the sign-in again.".into(),
        )
    })?;
    let provider = pending.provider;
    let tokens = services
        .oauth
        .exchange(&pending, &redirect.code)
        .await
        .map_err(|e| oauth_refusal(provider, e))?;
    let refresh = tokens.refresh_token.clone().ok_or_else(|| {
        MailboxError::Invalid(format!(
            "{} did not grant lasting access. Start the sign-in again and allow access.",
            provider.name()
        ))
    })?;
    let (account, settings) = target(services, &pending).await?;
    let token = Credentials::AccessToken(tokens.access_token.clone());
    let signed_in_as = match pending.access {
        Access::Imap | Access::ImapDav => {
            let imap = ImapSettings::from_json(&settings)
                .map_err(|e| MailboxError::Invalid(e.to_string()))?;
            check_sign_in(&imap, token).await?;
            None
        }
        Access::Graph | Access::GraphPim => check_graph_sign_in(&settings, token).await?,
    };
    if let (Some(account), Some(now)) = (&account, &signed_in_as)
        && let Some(before) = &account.address
        && !before.eq_ignore_ascii_case(now)
    {
        return Err(MailboxError::Invalid(format!(
            "You signed in as {now}, but this account archives {before}. Sign in as {before}."
        )));
    }
    let address = signed_in_as.unwrap_or_else(|| pending.email.clone());
    let store = &services.store;
    let mut account = match account {
        Some(mut account) => {
            account.settings = settings;
            if account.address.is_none() {
                account.address = Some(address);
            }
            account
        }
        None => {
            let Purpose::NewAccount { archive, name } = &pending.purpose else {
                unreachable!("only new accounts have no account yet");
            };
            store
                .create_account(NewAccount {
                    archive_id: *archive,
                    kind: match pending.access {
                        Access::Imap | Access::ImapDav => AccountKind::Imap,
                        Access::Graph | Access::GraphPim => AccountKind::Graph,
                    },
                    name: name.clone().unwrap_or_else(|| address.clone()),
                    address: Some(address),
                    settings,
                    sealed_secret: None,
                    enabled: true,
                })
                .await
                .map_err(backend)?
        }
    };
    account.sealed_secret = Some(
        services
            .secrets
            .seal(&account.id.to_string(), refresh.as_bytes())
            .map_err(backend)?,
    );
    store.update_account(&account).await.map_err(backend)?;
    services.oauth.remember(account.id, &tokens);
    tracing::info!(
        account = %account.id,
        provider = provider.as_str(),
        access = pending.access.as_str(),
        "signed in with OAuth"
    );
    Ok(account)
}

pub fn url_state(answer: &str) -> Option<String> {
    let query = answer.rsplit_once('?').map_or(answer, |(_, q)| q);
    url::form_urlencoded::parse(query.trim().as_bytes())
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
}

/// The account a sign-in is for, when it exists, and its settings after the sign-in.
async fn target(
    services: &Services,
    pending: &Pending,
) -> Result<(Option<Account>, serde_json::Value), MailboxError> {
    let (host, port) = pending.provider.imap();
    let oauth = OAuthSettings {
        provider: pending.provider.as_str().to_owned(),
        needs_sign_in: false,
        access: Some(pending.access.as_str().to_owned()),
    };
    match &pending.purpose {
        Purpose::NewAccount { archive, .. } => {
            services
                .store
                .get_archive(*archive)
                .await
                .map_err(backend)?
                .ok_or_else(|| MailboxError::Invalid("The archive was removed.".into()))?;
            if matches!(pending.access, Access::Graph | Access::GraphPim) {
                let mut settings = match services.oauth.graph_url() {
                    Some(url) => serde_json::json!({ "api_url": url }),
                    None => serde_json::json!({}),
                };
                oauth.write(&mut settings);
                return Ok((None, settings));
            }
            let mut settings = serde_json::to_value(ImapSettings {
                host: host.to_owned(),
                port: Some(port),
                security: Security::Tls,
                username: pending.email.clone(),
            })
            .map_err(backend)?;
            oauth.write(&mut settings);
            Ok((None, settings))
        }
        Purpose::SignInAgain(id) => {
            let account = services
                .store
                .get_account(*id)
                .await
                .map_err(backend)?
                .ok_or_else(|| MailboxError::Invalid("The account was removed.".into()))?;
            let mut settings = account.settings.clone();
            if matches!(pending.access, Access::Graph | Access::GraphPim) {
                oauth.write(&mut settings);
                return Ok((Some(account), settings));
            }
            let same =
                OAuthSettings::of(&account).is_some_and(|o| o.provider() == Some(pending.provider));
            if !same {
                let (settings_with, _) = with_connection(
                    &settings,
                    &ConnectionChange {
                        host: host.to_owned(),
                        port: Some(port),
                        security: "tls".into(),
                        username: pending.email.clone(),
                        exclude: options(&account).exclude,
                    },
                )
                .map_err(|e| MailboxError::Invalid(format!("{e:#}")))?;
                settings = settings_with;
            }
            oauth.write(&mut settings);
            Ok((Some(account), settings))
        }
    }
}

/// The address to sign in as for `account`: its user name when it already uses OAuth, the
/// mailbox address for Graph.
pub fn oauth_email(account: &Account) -> Option<String> {
    OAuthSettings::of(account)?;
    if account.kind == AccountKind::Graph {
        return account.address.clone();
    }
    ImapSettings::from_json(&account.settings)
        .ok()
        .map(|s| s.username)
}

pub async fn syncable_accounts(store: &dyn Store) -> anyhow::Result<Vec<Account>> {
    let mut out = Vec::new();
    for archive in store.list_archives().await? {
        out.extend(
            store
                .list_accounts(archive.id)
                .await?
                .into_iter()
                .filter(|a| {
                    a.enabled
                        && a.kind != AccountKind::Import
                        && !OAuthSettings::of(a).is_some_and(|o| o.needs_sign_in)
                }),
        );
    }
    Ok(out)
}

pub fn syncer(services: &Services) -> Syncer {
    Syncer::new(services.store.clone(), services.ingest())
}

#[cfg(test)]
mod tests {
    #[test]
    fn changes_the_connection_and_keeps_other_settings() {
        let stored = serde_json::json!({
            "host": "imap.example.com",
            "port": 1993,
            "security": "tls",
            "username": "ada",
            "folders": { "include": ["INBOX"], "exclude": ["Junk"] },
            "sync_interval_minutes": 5
        });
        let (json, imap) = super::with_connection(
            &stored,
            &holtmail_api::ConnectionChange {
                host: " mail.example.org ".into(),
                port: None,
                security: "starttls".into(),
                username: "ada@example.org".into(),
                exclude: vec!["Trash".into(), " ".into()],
            },
        )
        .unwrap();
        assert_eq!(
            imap.port(),
            143,
            "the old port survived a change to the default"
        );
        assert_eq!(json["host"], "mail.example.org");
        assert_eq!(json["folders"]["include"], serde_json::json!(["INBOX"]));
        assert_eq!(json["folders"]["exclude"], serde_json::json!(["Trash"]));
        assert_eq!(json["sync_interval_minutes"], 5);
        assert!(
            super::with_connection(
                &stored,
                &holtmail_api::ConnectionChange {
                    host: "x".into(),
                    port: None,
                    security: "ssl".into(),
                    username: "ada".into(),
                    exclude: Vec::new(),
                },
            )
            .is_err()
        );

        let aged = super::with_min_age_days(&json, Some(30));
        assert_eq!(aged["min_age_days"], 30);
        assert!(
            super::with_min_age_days(&aged, Some(0))
                .get("min_age_days")
                .is_none()
        );
        assert!(
            super::with_min_age_days(&aged, None)
                .get("min_age_days")
                .is_none()
        );

        let json = super::with_sync_interval(&json, Some(60));
        assert_eq!(json["sync_interval_minutes"], 60);
        assert!(
            super::with_sync_interval(&json, None)
                .get("sync_interval_minutes")
                .is_none()
        );
    }

    use holtmail_secret_local::LocalSecretBox;
    use holtmail_store::ArchiveStore;
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    #[tokio::test]
    async fn stores_imap_accounts_with_a_sealed_password() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("db")).await.unwrap();
        let secrets = LocalSecretBox::from_key([3; 32]);
        let account = add_imap(
            &store,
            &secrets,
            NewImapAccount {
                archive: crate::import::find_or_create_archive(&store, "Personal")
                    .await
                    .unwrap(),
                name: None,
                host: "imap.example.com".into(),
                port: None,
                security: Security::Tls,
                username: "ada@example.com".into(),
                password: "correct horse".into(),
                exclude: vec!["Junk".into()],
            },
        )
        .await
        .unwrap();
        let stored = store.get_account(account.id).await.unwrap().unwrap();
        let sealed = stored.sealed_secret.clone().unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("correct horse"));
        assert_eq!(
            secrets.open(&account.id.to_string(), &sealed).unwrap(),
            b"correct horse"
        );
        assert_eq!(options(&stored).exclude, ["Junk"]);
        assert_eq!(options(&stored).min_age, None);
        let mut aged = stored.clone();
        aged.settings = with_min_age_days(&aged.settings, Some(2));
        assert_eq!(min_age_days(&aged), Some(2));
        assert_eq!(
            options(&aged).min_age,
            Some(std::time::Duration::from_secs(2 * 24 * 60 * 60))
        );
        let oauth_for_checks = OAuth::new(
            Vec::new(),
            std::sync::Arc::new(holtmail_oauth::testing::FakeTokenEndpoint::default()),
        );
        let refused = configure(
            &store,
            &secrets,
            &oauth_for_checks,
            &stored,
            AccountChange {
                min_age_days: Some(Some(MAX_MIN_AGE_DAYS + 1)),
                ..AccountChange::default()
            },
        )
        .await;
        assert!(matches!(refused, Err(MailboxError::Invalid(_))));
        let changed = configure(
            &store,
            &secrets,
            &oauth_for_checks,
            &stored,
            AccountChange {
                min_age_days: Some(Some(14)),
                sync_interval_minutes: Some(Some(0)),
                ..AccountChange::default()
            },
        )
        .await
        .unwrap();
        let reread = store.get_account(stored.id).await.unwrap().unwrap();
        assert_eq!(reread, changed);
        assert_eq!(min_age_days(&reread), Some(14));
        assert_eq!(sync_interval(&reread), Some(0));
        assert_eq!(reread.sealed_secret, stored.sealed_secret);
        let stored = configure(
            &store,
            &secrets,
            &oauth_for_checks,
            &reread,
            AccountChange {
                min_age_days: Some(None),
                sync_interval_minutes: Some(None),
                ..AccountChange::default()
            },
        )
        .await
        .unwrap();
        let oauth = OAuth::new(
            Provider::ALL
                .into_iter()
                .map(holtmail_oauth::Client::thunderbird)
                .collect(),
            std::sync::Arc::new(holtmail_oauth::testing::FakeTokenEndpoint::default()),
        );
        assert!(source_for(&stored, &store, &secrets, &oauth).await.is_ok());
        assert!(
            source_for(&stored, &store, &LocalSecretBox::from_key([4; 32]), &oauth)
                .await
                .is_err()
        );
        assert_eq!(syncable_accounts(&store).await.unwrap().len(), 1);

        let mut revoked = stored.clone();
        OAuthSettings {
            provider: "google".into(),
            needs_sign_in: true,
            access: None,
        }
        .write(&mut revoked.settings);
        store.update_account(&revoked).await.unwrap();
        assert!(
            syncable_accounts(&store).await.unwrap().is_empty(),
            "an account that needs sign-in is still scheduled"
        );
        assert!(
            source_for(&revoked, &store, &secrets, &oauth)
                .await
                .is_err()
        );
        assert_eq!(oauth_email(&revoked).as_deref(), Some("ada@example.com"));
        assert_eq!(oauth_email(&stored), None);
    }

    #[tokio::test]
    async fn stores_jmap_accounts_and_checks_changes_before_signing_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("db")).await.unwrap();
        let secrets = LocalSecretBox::from_key([5; 32]);
        let oauth = OAuth::new(
            Vec::new(),
            std::sync::Arc::new(holtmail_oauth::testing::FakeTokenEndpoint::default()),
        );
        let settings = jmap_settings("api.example.com/jmap/session", "", JmapAuth::Token).unwrap();
        assert_eq!(settings.url, "https://api.example.com/jmap/session");
        assert!(jmap_settings("jmap.example.com", " ", JmapAuth::Password).is_err());
        assert!(jmap_settings("http://jmap.example.com", "ada", JmapAuth::Password).is_err());
        let account = add_jmap(
            &store,
            &secrets,
            NewJmapAccount {
                archive: crate::import::find_or_create_archive(&store, "Personal")
                    .await
                    .unwrap(),
                name: None,
                settings,
                secret: "fmu1-example".into(),
                exclude: vec!["Spam".into()],
            },
        )
        .await
        .unwrap();
        assert_eq!(account.kind, AccountKind::Jmap);
        assert_eq!(account.name, "api.example.com");
        assert_eq!(account.address, None);
        let connection = jmap_connection(&account).unwrap();
        assert_eq!(connection.auth, "token");
        assert_eq!(connection.exclude, ["Spam"]);
        assert_eq!(super::connection(&account), None);
        assert!(
            !String::from_utf8_lossy(account.sealed_secret.as_deref().unwrap()).contains("fmu1")
        );
        assert_eq!(
            credentials(&account, &store, &secrets, &oauth)
                .await
                .unwrap(),
            Credentials::AccessToken("fmu1-example".into())
        );
        assert!(source_for(&account, &store, &secrets, &oauth).await.is_ok());
        assert_eq!(syncable_accounts(&store).await.unwrap().len(), 1);

        let refused = |change: AccountChange| {
            let (store, secrets, oauth, account) = (&store, &secrets, &oauth, &account);
            async move {
                matches!(
                    configure(store, secrets, oauth, account, change).await,
                    Err(MailboxError::Invalid(_))
                )
            }
        };
        let change = |auth: &str, url: &str| JmapConnectionChange {
            url: url.into(),
            username: "ada@example.com".into(),
            auth: auth.into(),
            exclude: Vec::new(),
        };
        assert!(
            refused(AccountChange {
                jmap: Some(change("password", "https://api.example.com/jmap/session")),
                ..AccountChange::default()
            })
            .await,
            "a move to a password signed in with the token"
        );
        assert!(
            refused(AccountChange {
                connection: Some(ConnectionChange {
                    host: "imap.example.com".into(),
                    port: None,
                    security: "tls".into(),
                    username: "ada".into(),
                    exclude: Vec::new(),
                }),
                ..AccountChange::default()
            })
            .await
        );
        assert!(
            refused(AccountChange {
                jmap: Some(change("token", "http://127.0.0.1:9/jmap")),
                password: Some("fmu1-new".into()),
                ..AccountChange::default()
            })
            .await,
            "an unreachable server was stored"
        );
        for url in [
            "https://jmap.example.net/jmap/session",
            "https://api.example.com:8443/jmap/session",
        ] {
            let moved = configure(
                &store,
                &secrets,
                &oauth,
                &account,
                AccountChange {
                    jmap: Some(change("token", url)),
                    ..AccountChange::default()
                },
            )
            .await;
            assert!(
                matches!(&moved, Err(MailboxError::Invalid(reason)) if reason == "Enter the token again when changing the server."),
                "the stored token went to {url}"
            );
        }
        assert_eq!(
            store.get_account(account.id).await.unwrap().unwrap(),
            account
        );
    }

    fn imap(host: &str, port: Option<u16>, security: Security) -> ImapSettings {
        ImapSettings {
            host: host.into(),
            port,
            security,
            username: "ada".into(),
        }
    }

    #[test]
    fn a_server_moves_with_its_host_port_or_a_drop_to_plain_text() {
        let stored = imap("imap.example.com", None, Security::Tls);
        let moves = |next: ImapSettings| imap_moves(&stored, &next);
        assert!(!moves(imap(" IMAP.example.com ", Some(993), Security::Tls)));
        assert!(!moves(imap(
            "imap.example.com",
            Some(993),
            Security::Starttls
        )));
        assert!(moves(imap("imap.example.net", None, Security::Tls)));
        assert!(moves(imap("imap.example.com", Some(1993), Security::Tls)));
        assert!(moves(imap("imap.example.com", None, Security::Starttls)));
        assert!(moves(imap("imap.example.com", Some(993), Security::Plain)));
        let plain = imap("imap.example.com", None, Security::Plain);
        assert!(!imap_moves(
            &plain,
            &imap("imap.example.com", None, Security::Starttls)
        ));

        let jmap = |url: &str| jmap_settings(url, "", JmapAuth::Token).unwrap();
        let stored = jmap("api.example.com/jmap/session");
        assert!(!jmap_moves(&stored, &jmap("https://API.example.com/other")));
        assert!(!jmap_moves(
            &stored,
            &jmap("https://api.example.com:443/jmap")
        ));
        assert!(jmap_moves(
            &stored,
            &jmap("https://api.example.org/jmap/session")
        ));
        assert!(jmap_moves(
            &stored,
            &jmap("https://api.example.com:8443/jmap")
        ));
        assert!(jmap_moves(&stored, &jmap("http://127.0.0.1/jmap")));
    }

    #[tokio::test]
    async fn keeps_stored_secrets_off_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("db")).await.unwrap();
        let secrets = LocalSecretBox::from_key([6; 32]);
        let oauth = OAuth::new(
            Vec::new(),
            std::sync::Arc::new(holtmail_oauth::testing::FakeTokenEndpoint::default()),
        );
        let account = add_imap(
            &store,
            &secrets,
            NewImapAccount {
                archive: crate::import::find_or_create_archive(&store, "Personal")
                    .await
                    .unwrap(),
                name: None,
                host: "imap.example.com".into(),
                port: None,
                security: Security::Tls,
                username: "ada@example.com".into(),
                password: "correct horse".into(),
                exclude: Vec::new(),
            },
        )
        .await
        .unwrap();
        let connection = |host: &str, port: Option<u16>, security: &str| AccountChange {
            connection: Some(ConnectionChange {
                host: host.into(),
                port,
                security: security.into(),
                username: "ada@example.com".into(),
                exclude: Vec::new(),
            }),
            ..AccountChange::default()
        };
        for change in [
            connection("imap.example.net", None, "tls"),
            connection("imap.example.com", Some(1993), "tls"),
            connection("imap.example.com", Some(993), "plain"),
        ] {
            assert!(moves_server(&account, &change));
            assert!(matches!(
                configure(&store, &secrets, &oauth, &account, change).await,
                Err(MailboxError::Invalid(reason)) if reason == "Enter the password again when changing the server."
            ));
        }
        assert!(!moves_server(
            &account,
            &connection("IMAP.example.com", Some(993), "starttls")
        ));
        assert_eq!(
            store.get_account(account.id).await.unwrap().unwrap(),
            account
        );

        let mut google = account.clone();
        OAuthSettings {
            provider: "google".into(),
            needs_sign_in: false,
            access: None,
        }
        .write(&mut google.settings);
        store.update_account(&google).await.unwrap();
        for change in [
            connection("imap.example.net", None, "tls"),
            connection("imap.example.com", Some(993), "starttls"),
        ] {
            assert!(matches!(
                configure(&store, &secrets, &oauth, &google, change).await,
                Err(MailboxError::Invalid(reason)) if reason.contains("are fixed")
            ));
        }
        assert_eq!(
            store.get_account(account.id).await.unwrap().unwrap(),
            google
        );
    }
}
