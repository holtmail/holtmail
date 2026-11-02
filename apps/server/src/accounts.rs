use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use holtmail_ingest::Ingest;
use holtmail_secret::SecretBox;
use holtmail_source::Source;
use holtmail_source_imap::{ImapSettings, ImapSource, Security};
use holtmail_store::{Account, AccountKind, NewAccount, Store};
use holtmail_sync::{SyncOptions, SyncReport, Syncer};
use serde::Deserialize;

use crate::config::Services;

pub struct NewImapAccount {
    pub archive: String,
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
    let archive = crate::import::find_or_create_archive(store, &new.archive).await?;
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
        ..SyncOptions::default()
    }
}

pub fn source_for(account: &Account, secrets: &dyn SecretBox) -> anyhow::Result<Box<dyn Source>> {
    match account.kind {
        AccountKind::Imap => {
            let settings = ImapSettings::from_json(&account.settings)?;
            let sealed = account
                .sealed_secret
                .as_deref()
                .context("the account has no stored password")?;
            let password = secrets
                .open(&account.id.to_string(), sealed)
                .context("the stored password cannot be opened with this secret key")?;
            Ok(Box::new(ImapSource::new(
                settings,
                String::from_utf8(password).context("the stored password is not UTF-8")?,
            )))
        }
        AccountKind::Import => bail!("import accounts have no server"),
        other => bail!("{} accounts are not supported yet", other.as_str()),
    }
}

pub async fn sync_one(
    services: &Services,
    syncer: &Syncer,
    account: &Account,
) -> anyhow::Result<SyncReport> {
    let source = source_for(account, &*services.secrets)?;
    Ok(syncer
        .sync_account(account, &*source, &options(account))
        .await?)
}

pub async fn syncable_accounts(store: &dyn Store) -> anyhow::Result<Vec<Account>> {
    let mut out = Vec::new();
    for archive in store.list_archives().await? {
        out.extend(
            store
                .list_accounts(archive.id)
                .await?
                .into_iter()
                .filter(|a| a.enabled && a.kind != AccountKind::Import),
        );
    }
    Ok(out)
}

pub fn syncer(services: &Services) -> Syncer {
    Syncer::new(
        services.store.clone(),
        Ingest::new(
            services.blobs.clone(),
            services.store.clone(),
            services.index.clone(),
        ),
    )
}

/// Syncs every enabled account, one after another, then waits `every` and starts again.
pub async fn run_schedule(services: Arc<Services>, every: Duration) {
    let syncer = syncer(&services);
    loop {
        match syncable_accounts(&*services.store).await {
            Ok(accounts) => {
                for account in accounts {
                    match sync_one(&services, &syncer, &account).await {
                        Ok(report) => tracing::info!(
                            account = %account.id,
                            new = report.new_messages(),
                            folders = report.folders.len(),
                            "synced"
                        ),
                        Err(error) => tracing::warn!(account = %account.id, %error, "sync failed"),
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "listing accounts failed"),
        }
        tokio::time::sleep(every).await;
    }
}

#[cfg(test)]
mod tests {
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
                archive: "Personal".into(),
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
        assert!(source_for(&stored, &secrets).is_ok());
        assert!(source_for(&stored, &LocalSecretBox::from_key([4; 32])).is_err());
        assert_eq!(syncable_accounts(&store).await.unwrap().len(), 1);
    }
}
