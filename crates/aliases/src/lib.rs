//! Alias services connected to archives: their keys sealed in the store, their aliases listed
//! into the store, and aliases created, changed and deleted at the service.
//!
//! A library over the `holtmail-alias`, `holtmail-store` and `holtmail-secret` contracts. Which
//! services exist and how a directory is built for one is given by the caller ([`Connect`]), so
//! only the composition root knows the implementations.

use std::sync::Arc;

use holtmail_alias::{
    AliasChange, AliasDirectory, AliasError, AliasOptions, Capabilities, NewAlias, RemoteAlias,
};
use holtmail_core::{AliasServiceId, ArchiveId, Timestamp};
use holtmail_secret::SecretBox;
use holtmail_store::{AliasService, KnownAlias, Store, StoreError};

/// Builds a directory for `(provider, base_url, key)`. `Invalid` for an unknown provider or an
/// address that cannot be used.
pub type Connect =
    Arc<dyn Fn(&str, &str, &str) -> Result<Arc<dyn AliasDirectory>, AliasError> + Send + Sync>;

/// An alias service the caller knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    /// The provider id.
    pub id: String,
    /// What people call it.
    pub name: String,
    /// Its address when the owner gives none; `None` when it has no API.
    pub default_url: Option<String>,
    /// What its API can do; `None` without an API.
    pub capabilities: Option<Capabilities>,
    /// What the key is called at the service.
    pub key_label: String,
    /// Where the owner finds the key.
    pub key_help: String,
    /// Mail it forwarded is recognised.
    pub detects: bool,
}

/// Why a request was refused.
#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    /// The request cannot work, in words for the owner: an unknown service or alias, a refused
    /// key, a service that said no or could not be reached.
    #[error("{0}")]
    Invalid(String),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What an [`AliasError`] means to the owner.
pub fn explain(error: &AliasError) -> String {
    match error {
        AliasError::Auth => "the service refused the key".into(),
        AliasError::Unsupported => "the service cannot do this".into(),
        AliasError::NotFound => "the service has no such alias".into(),
        AliasError::Invalid(message) => message.clone(),
        AliasError::Unavailable(message) => format!("the service is unavailable: {message}"),
        AliasError::Protocol(message) => {
            format!("the service answered something unexpected: {message}")
        }
    }
}

fn refused(error: AliasError) -> ManageError {
    ManageError::Invalid(explain(&error))
}

fn context(id: AliasServiceId) -> String {
    format!("alias-service:{id}")
}

/// The alias as the store keeps it.
pub fn known(remote: RemoteAlias, provider: &str, service: AliasServiceId) -> KnownAlias {
    KnownAlias {
        address: remote.address,
        provider: provider.to_owned(),
        service_id: Some(service),
        remote_id: Some(remote.id),
        enabled: Some(remote.enabled),
        note: remote.note,
        name: remote.name,
        for_domain: remote.for_domain,
        mailboxes: remote.mailboxes.into_iter().map(|m| m.email).collect(),
        created_at: remote.created_at,
        forwarded: remote.forwarded,
        blocked: remote.blocked,
        replied: remote.replied,
    }
}

/// Alias services of every archive. Cheap to clone.
#[derive(Clone)]
pub struct AliasManager {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
    providers: Arc<Vec<ProviderInfo>>,
    connect: Connect,
}

impl AliasManager {
    /// A manager storing in `store`, sealing keys with `secrets`, offering `providers` and
    /// reaching services through `connect`.
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretBox>,
        providers: Vec<ProviderInfo>,
        connect: Connect,
    ) -> Self {
        Self {
            store,
            secrets,
            providers: Arc::new(providers),
            connect,
        }
    }

    /// The providers, as given.
    pub fn providers(&self) -> &[ProviderInfo] {
        &self.providers
    }

    fn provider(&self, id: &str) -> Result<&ProviderInfo, ManageError> {
        self.providers
            .iter()
            .find(|p| p.id == id && p.capabilities.is_some())
            .ok_or_else(|| ManageError::Invalid(format!("{id} has no API Holtmail can use")))
    }

    async fn service(&self, id: AliasServiceId) -> Result<AliasService, ManageError> {
        self.store
            .get_alias_service(id)
            .await?
            .ok_or_else(|| ManageError::Invalid("no such alias service".into()))
    }

    fn directory(&self, service: &AliasService) -> Result<Arc<dyn AliasDirectory>, ManageError> {
        let key = self
            .secrets
            .open(&context(service.id), &service.sealed_key)
            .map_err(|_| ManageError::Invalid("the key cannot be opened; enter it again".into()))?;
        let key = String::from_utf8(key)
            .map_err(|_| ManageError::Invalid("the key cannot be opened; enter it again".into()))?;
        (self.connect)(&service.provider, &service.base_url, &key).map_err(refused)
    }

    /// Checks the key with the service, stores the service and lists its aliases. Nothing is
    /// stored when the check fails.
    pub async fn add_service(
        &self,
        archive: ArchiveId,
        provider: &str,
        name: &str,
        base_url: Option<&str>,
        key: &str,
    ) -> Result<AliasService, ManageError> {
        let info = self.provider(provider)?.clone();
        let base_url = base_url
            .map(str::to_owned)
            .or(info.default_url.clone())
            .ok_or_else(|| ManageError::Invalid("give the service's address".into()))?;
        let key = key.trim();
        let directory = (self.connect)(provider, &base_url, key).map_err(refused)?;
        directory.options().await.map_err(refused)?;
        let id = AliasServiceId::new();
        let service = AliasService {
            id,
            archive_id: archive,
            provider: provider.to_owned(),
            name: if name.trim().is_empty() {
                info.name.clone()
            } else {
                name.trim().to_owned()
            },
            base_url,
            sealed_key: self
                .secrets
                .seal(&context(id), key.as_bytes())
                .map_err(|e| ManageError::Invalid(e.to_string()))?,
            created_at: Timestamp::now(),
            synced_at: None,
            sync_error: None,
        };
        self.store.create_alias_service(&service).await?;
        let _ = self.sync_service(id).await;
        self.service(id).await
    }

    /// Changes a service's name, address or key. A new address or key is checked with the
    /// service first.
    pub async fn update_service(
        &self,
        id: AliasServiceId,
        name: Option<&str>,
        base_url: Option<&str>,
        key: Option<&str>,
    ) -> Result<AliasService, ManageError> {
        let mut service = self.service(id).await?;
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            service.name = name.to_owned();
        }
        let base_url = base_url.map(str::trim).filter(|u| !u.is_empty());
        if base_url.is_some() || key.is_some() {
            let current = match key {
                Some(key) => key.trim().to_owned(),
                None => String::from_utf8(
                    self.secrets
                        .open(&context(id), &service.sealed_key)
                        .map_err(|_| ManageError::Invalid("enter the key again".into()))?,
                )
                .map_err(|_| ManageError::Invalid("enter the key again".into()))?,
            };
            let url = base_url.unwrap_or(&service.base_url).to_owned();
            let directory = (self.connect)(&service.provider, &url, &current).map_err(refused)?;
            directory.options().await.map_err(refused)?;
            service.base_url = url;
            service.sealed_key = self
                .secrets
                .seal(&context(id), current.as_bytes())
                .map_err(|e| ManageError::Invalid(e.to_string()))?;
            service.sync_error = None;
        }
        self.store.update_alias_service(&service).await?;
        if base_url.is_some() || key.is_some() {
            let _ = self.sync_service(id).await;
        }
        self.service(id).await
    }

    /// Lists the service's aliases into the store. A failure is kept as the service's
    /// `sync_error` and returned.
    pub async fn sync_service(&self, id: AliasServiceId) -> Result<AliasService, ManageError> {
        let mut service = self.service(id).await?;
        let listed = match self.directory(&service) {
            Ok(directory) => directory.aliases().await.map_err(refused),
            Err(error) => Err(error),
        };
        match listed {
            Ok(remote) => {
                let aliases: Vec<KnownAlias> = remote
                    .into_iter()
                    .map(|r| known(r, &service.provider, id))
                    .collect();
                self.store
                    .replace_service_aliases(id, &aliases, Timestamp::now())
                    .await?;
                self.service(id).await
            }
            Err(ManageError::Invalid(reason)) => {
                service.sync_error = Some(reason.clone());
                self.store.update_alias_service(&service).await?;
                Err(ManageError::Invalid(reason))
            }
            Err(other) => Err(other),
        }
    }

    /// Lists the aliases of every service; returns the archives whose aliases were listed and
    /// how many services failed.
    pub async fn sync_all(&self) -> Result<(Vec<ArchiveId>, u32), ManageError> {
        let mut archives = Vec::new();
        let mut failed = 0;
        for service in self.store.all_alias_services().await? {
            match self.sync_service(service.id).await {
                Ok(_) => {
                    if !archives.contains(&service.archive_id) {
                        archives.push(service.archive_id);
                    }
                }
                Err(_) => failed += 1,
            }
        }
        Ok((archives, failed))
    }

    /// What the service offers for new aliases, with what it can do.
    pub async fn options(
        &self,
        id: AliasServiceId,
    ) -> Result<(Capabilities, AliasOptions), ManageError> {
        let service = self.service(id).await?;
        let directory = self.directory(&service)?;
        let options = directory.options().await.map_err(refused)?;
        Ok((directory.capabilities(), options))
    }

    /// Creates an alias at the service and stores it.
    pub async fn create_alias(
        &self,
        id: AliasServiceId,
        alias: &NewAlias,
    ) -> Result<KnownAlias, ManageError> {
        let service = self.service(id).await?;
        let directory = self.directory(&service)?;
        let made = directory.create(alias).await.map_err(refused)?;
        let made = known(made, &service.provider, id);
        self.store.put_alias(service.archive_id, &made).await?;
        Ok(made)
    }

    async fn known_alias(
        &self,
        archive: ArchiveId,
        address: &str,
    ) -> Result<KnownAlias, ManageError> {
        self.store
            .known_aliases_among(archive, &[address.to_owned()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| ManageError::Invalid(format!("{address} is not a known alias")))
    }

    /// Changes an alias at its service and in the store. An alias added by hand keeps only a
    /// note and a website.
    pub async fn update_alias(
        &self,
        archive: ArchiveId,
        address: &str,
        change: &AliasChange,
    ) -> Result<KnownAlias, ManageError> {
        let current = self.known_alias(archive, address).await?;
        let (Some(service_id), Some(remote_id)) = (current.service_id, current.remote_id.clone())
        else {
            if change.enabled.is_some() || change.name.is_some() || change.mailbox_ids.is_some() {
                return Err(ManageError::Invalid(
                    "an alias added by hand keeps only a note and a website".into(),
                ));
            }
            let clear = |v: &Option<String>, old: Option<String>| match v {
                Some(v) if v.is_empty() => None,
                Some(v) => Some(v.clone()),
                None => old,
            };
            let changed = KnownAlias {
                note: clear(&change.note, current.note.clone()),
                for_domain: clear(&change.for_domain, current.for_domain.clone()),
                ..current
            };
            self.store.put_alias(archive, &changed).await?;
            return Ok(changed);
        };
        let service = self.service(service_id).await?;
        let directory = self.directory(&service)?;
        let changed = directory
            .update(&remote_id, change)
            .await
            .map_err(refused)?;
        let changed = known(changed, &service.provider, service_id);
        self.store.put_alias(archive, &changed).await?;
        Ok(changed)
    }

    /// Forgets an alias; with `at_service`, deletes it at its service first.
    pub async fn delete_alias(
        &self,
        archive: ArchiveId,
        address: &str,
        at_service: bool,
    ) -> Result<(), ManageError> {
        let current = self.known_alias(archive, address).await?;
        if at_service
            && let (Some(service_id), Some(remote_id)) = (current.service_id, &current.remote_id)
        {
            let service = self.service(service_id).await?;
            match self.directory(&service)?.delete(remote_id).await {
                Ok(()) | Err(AliasError::NotFound) => {}
                Err(error) => return Err(refused(error)),
            }
        }
        self.store.remove_alias(archive, address).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
