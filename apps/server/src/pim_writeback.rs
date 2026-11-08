//! Contacts and calendars write-through: the writers `pim-writeback` sends held edits through,
//! handed out only for `write_through` collections of accounts with `pim.edit` while the
//! instance allows writing, and the rebaser built from `pim-edit`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_api::{Scheduling, SchedulingProbe};
use holtmail_collection::{CollectionError, CollectionKind, Field, RemoteItem, WriteCaps};
use holtmail_core::AccountId;
use holtmail_pim::{Format, ItemKind};
use holtmail_pim_edit::{Class, Host, Patch, Rebased};
use holtmail_pim_writeback::{
    Rebaser, Respond, Upstream, Writeback, WritebackConfig, Writers, pim_write_consent_missing,
};
use holtmail_secret::SecretBox;
use holtmail_store::{
    Account, AccountKind, Capability, Collection, ItemChange, Store, StoreError, WriteMode,
};

use crate::config::{Services, WriteConfig};
use crate::oauth::OAuth;

const WRITER_TTL: Duration = Duration::from_secs(5 * 60);

struct Cached {
    at: Instant,
    built_from: (serde_json::Value, Option<Vec<u8>>),
    upstream: Upstream,
}

fn fingerprint(account: &Account) -> (serde_json::Value, Option<Vec<u8>>) {
    (account.settings.clone(), account.sealed_secret.clone())
}

fn remote_kind(collection: &Collection) -> CollectionKind {
    match collection.kind {
        holtmail_store::CollectionKind::Contacts => CollectionKind::Contacts,
        holtmail_store::CollectionKind::Calendar => CollectionKind::Calendar,
    }
}

/// Builds the upstream of a collection for `pim-writeback`, refusing (with `None`) whatever
/// may not be written now: the instance switch is off, the collection is not `write_through`,
/// it belongs to no account, the account lacks `pim.edit`, or it is an import account. A
/// Microsoft account whose sign-in lacks `Contacts.ReadWrite` and `Calendars.ReadWrite` records
/// the missing consent (the account then asks to sign in again) and answers `Auth`, so changes
/// wait. Writers are kept for a few minutes per account and protocol, and only while the
/// account's settings and stored sign-in stay the same.
pub struct PimWriters {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
    oauth: Arc<OAuth>,
    switches: WriteConfig,
    cache: Mutex<HashMap<(AccountId, CollectionKind), Cached>>,
}

impl PimWriters {
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
            cache: Mutex::default(),
        }
    }

    fn cached(&self, account: &Account, kind: CollectionKind) -> Option<Upstream> {
        let mut cache = self.cache.lock().expect("writer cache");
        let key = (account.id, kind);
        match cache.get(&key) {
            Some(cached)
                if cached.at.elapsed() < WRITER_TTL
                    && cached.built_from == fingerprint(account) =>
            {
                Some(cached.upstream.clone())
            }
            Some(_) => {
                cache.remove(&key);
                None
            }
            None => None,
        }
    }

    async fn own_addresses(&self, account: &Account) -> Result<Vec<String>, StoreError> {
        let mut own: Vec<String> = account.address.iter().cloned().collect();
        own.extend(
            self.store
                .list_identities(account.archive_id)
                .await?
                .into_iter()
                .filter(|i| i.fields.account_id == Some(account.id))
                .map(|i| i.fields.address),
        );
        Ok(own)
    }
}

#[async_trait]
impl Writers for PimWriters {
    async fn upstream(&self, collection: &Collection) -> Result<Option<Upstream>, CollectionError> {
        let store_error = |e: StoreError| CollectionError::Connection(e.to_string());
        if !self.switches.enabled || collection.write_mode != WriteMode::WriteThrough {
            return Ok(None);
        }
        let Some(account) = collection.account_id else {
            return Ok(None);
        };
        let Some(account) = self.store.get_account(account).await.map_err(store_error)? else {
            return Ok(None);
        };
        let capabilities = self
            .store
            .account_capabilities(account.id)
            .await
            .map_err(store_error)?;
        let writes =
            capabilities.has(Capability::PimEdit) || capabilities.has(Capability::PimRespond);
        if !writes || account.kind == AccountKind::Import {
            return Ok(None);
        }
        if pim_write_consent_missing(&account) {
            if let Err(error) = self
                .oauth
                .ask_consent(&account, &*self.store, "pim.edit")
                .await
            {
                tracing::warn!(account = %account.id, %error, "pim write consent not recorded");
            }
            return Err(CollectionError::Auth(
                "the sign-in does not allow changing contacts and calendars; sign in again".into(),
            ));
        }
        self.built(&account, collection).await.map(Some)
    }

    async fn may_reply(&self, collection: &Collection) -> Result<bool, CollectionError> {
        self.has(collection, Capability::PimRespond).await
    }

    async fn may_edit(&self, collection: &Collection) -> Result<bool, CollectionError> {
        self.has(collection, Capability::PimEdit).await
    }
}

impl PimWriters {
    async fn has(
        &self,
        collection: &Collection,
        capability: Capability,
    ) -> Result<bool, CollectionError> {
        let Some(account) = collection.account_id else {
            return Ok(false);
        };
        self.store
            .account_capabilities(account)
            .await
            .map(|c| c.has(capability))
            .map_err(|e| CollectionError::Connection(e.to_string()))
    }

    /// How the server holding `collection` schedules and what the account's writer can do
    /// there, whatever the write mode, the switches and the capabilities say: the host
    /// `pim-edit`'s routes are chosen for. A Graph calendar always schedules and a CalDAV one
    /// does when its writer offers `respond` (it only advertises it with
    /// `calendar-auto-schedule`). A JMAP calendar counts as not scheduling: writes never pass
    /// `sendSchedulingMessages` and the protocol has no way to learn whether a server would
    /// honour it, so Holtmail mails attendees itself.
    pub async fn scheduling(&self, collection: &Collection) -> SchedulingProbe {
        let Some(account) = collection.account_id else {
            return SchedulingProbe::NoServer;
        };
        let account = match self.store.get_account(account).await {
            Ok(Some(account)) => account,
            Ok(None) => return SchedulingProbe::NoServer,
            Err(_) => return SchedulingProbe::Unknown,
        };
        if account.kind == AccountKind::Import {
            return SchedulingProbe::NoServer;
        }
        let Some(settings) = crate::dav::DavSettings::of(&account).filter(|d| d.enabled) else {
            return SchedulingProbe::Unknown;
        };
        let protocol = match collection.kind {
            holtmail_store::CollectionKind::Contacts => settings.protocol,
            holtmail_store::CollectionKind::Calendar => settings.calendars(),
        };
        if protocol == crate::dav::Protocol::Graph && pim_write_consent_missing(&account) {
            return SchedulingProbe::Known(Scheduling {
                host: Host::Graph,
                caps: WriteCaps::default(),
            });
        }
        let caps = match self.built(&account, collection).await {
            Ok(upstream) => upstream.writer.capabilities(&collection.remote_id),
            Err(error) => {
                tracing::debug!(collection = %collection.id, %error, "scheduling not probed");
                return SchedulingProbe::Unknown;
            }
        };
        let host = match protocol {
            crate::dav::Protocol::Graph => Host::Graph,
            crate::dav::Protocol::Jmap => Host::Jmap { scheduling: false },
            crate::dav::Protocol::Dav => Host::CalDav {
                auto_schedule: caps.respond,
            },
        };
        SchedulingProbe::Known(Scheduling { host, caps })
    }

    async fn built(
        &self,
        account: &Account,
        collection: &Collection,
    ) -> Result<Upstream, CollectionError> {
        let store_error = |e: StoreError| CollectionError::Connection(e.to_string());
        let kind = remote_kind(collection);
        if let Some(upstream) = self.cached(account, kind) {
            if upstream.writer.capabilities(&collection.remote_id) != WriteCaps::default() {
                return Ok(upstream);
            }
            self.cache
                .lock()
                .expect("writer cache")
                .remove(&(account.id, kind));
        }
        let own = self.own_addresses(account).await.map_err(store_error)?;
        let upstream = crate::dav::upstream_for(
            account,
            kind,
            own,
            &*self.store,
            &*self.secrets,
            &self.oauth,
        )
        .await
        .map_err(|e| match e.downcast::<CollectionError>() {
            Ok(error) => error,
            Err(other) => CollectionError::Auth(format!("{other:#}")),
        })?;
        self.cache.lock().expect("writer cache").insert(
            (account.id, kind),
            Cached {
                at: Instant::now(),
                built_from: fingerprint(account),
                upstream: upstream.clone(),
            },
        );
        Ok(upstream)
    }
}

fn patch_of(change: &ItemChange) -> Option<Patch> {
    serde_json::from_value(change.patch.clone()?).ok()
}

fn text(patch: &Patch, raw: &[u8], content_type: &str) -> Option<Vec<u8>> {
    let kind: ItemKind = patch.kind();
    holtmail_pim::as_text(Format::stored(content_type, kind), kind, raw).map(|t| t.into_owned())
}

/// The [`Rebaser`] over `pim-edit`: the fields a change touched come from its stored patch, and
/// a rebase re-applies that patch on upstream's version, matching the user's attendee entry by
/// the entries the patch itself touches.
pub struct EditRebaser;

impl Rebaser for EditRebaser {
    fn fields(&self, change: &ItemChange) -> Option<Vec<Field>> {
        let patch = patch_of(change)?;
        (patch.class() != Class::Create).then(|| patch.fields())
    }

    fn rebased(
        &self,
        change: &ItemChange,
        mine: &[u8],
        onto: &[u8],
        onto_type: &str,
    ) -> Option<Bytes> {
        let patch = patch_of(change)?;
        let theirs = text(&patch, onto, onto_type)?;
        let served = holtmail_pim_edit::serve(
            Some(&patch),
            mine,
            None,
            Some((0, &theirs)),
            &patch.attendees(),
        );
        Some(Bytes::from(served))
    }

    fn rebase_reply(
        &self,
        change: &ItemChange,
        _mine: &[u8],
        upstream: &RemoteItem,
    ) -> Option<Bytes> {
        let patch = patch_of(change)?;
        let theirs = text(&patch, &upstream.raw, &upstream.content_type)?;
        match holtmail_pim_edit::rebase(&patch, &theirs, &patch.attendees()) {
            Rebased::Applied => Some(Bytes::from(theirs)),
            Rebased::Clean(raw) => Some(Bytes::from(raw)),
            _ => None,
        }
    }

    fn respond(&self, change: &ItemChange) -> Option<Respond> {
        let patch = patch_of(change)?;
        let respond = patch.respond()?;
        Some(Respond {
            recurrence_id: respond.recurrence_id.clone(),
            answer: respond.partstat()?,
            notify: respond.notify,
        })
    }
}

/// The contacts and calendars writeback over the server's store, blobs and writers.
pub fn open(services: &Services, config: &crate::config::Config) -> Arc<Writeback> {
    let writers = Arc::new(PimWriters::new(
        services.store.clone(),
        services.secrets.clone(),
        services.oauth.clone(),
        config.write,
    ));
    let _ = services.pim_writers.set(writers.clone());
    Arc::new(Writeback::new(
        services.store.clone(),
        services.blobs.clone(),
        services.ingest(),
        writers,
        Arc::new(EditRebaser),
        WritebackConfig::default(),
    ))
}

/// Puts edits a stopped process left mid-send back to held, then sends held edits until the
/// process ends.
pub async fn run(writeback: Arc<Writeback>) {
    match writeback.recover().await {
        Ok(ids) if !ids.is_empty() => {
            tracing::warn!(
                count = ids.len(),
                "contact and calendar edits interrupted by a restart go again"
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "interrupted contact and calendar edits not recovered")
        }
    }
    writeback.run().await;
}

#[cfg(test)]
mod tests {
    use holtmail_core::ArchiveId;
    use holtmail_oauth::Client;
    use holtmail_oauth::testing::FakeTokenEndpoint;
    use holtmail_secret_local::LocalSecretBox;
    use holtmail_store::{
        ArchiveStore, Capabilities, NewAccount, NewCollection, PimStore, WriteStore,
    };
    use holtmail_store_sqlite::SqliteStore;
    use serde_json::json;

    use super::*;
    use crate::oauth::OAuthSettings;

    struct World {
        _dir: tempfile::TempDir,
        store: Arc<SqliteStore>,
        secrets: Arc<LocalSecretBox>,
        oauth: Arc<OAuth>,
        archive: ArchiveId,
    }

    async fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let oauth = Arc::new(OAuth::new(
            holtmail_oauth::Provider::ALL
                .into_iter()
                .map(Client::thunderbird)
                .collect(),
            Arc::new(FakeTokenEndpoint::default()),
        ));
        let archive = store.create_archive("Writing").await.unwrap().id;
        World {
            _dir: dir,
            store,
            secrets: Arc::new(LocalSecretBox::from_key([9; 32])),
            oauth,
            archive,
        }
    }

    impl World {
        async fn collection(
            &self,
            kind: AccountKind,
            settings: serde_json::Value,
            capabilities: &[Capability],
        ) -> Collection {
            let mut account = self
                .store
                .create_account(NewAccount {
                    archive_id: self.archive,
                    kind,
                    name: "Ada".into(),
                    address: Some("ada@example.com".into()),
                    settings,
                    sealed_secret: None,
                    enabled: false,
                })
                .await
                .unwrap();
            account.sealed_secret = Some(
                self.secrets
                    .seal(&account.id.to_string(), b"secret-1")
                    .unwrap(),
            );
            self.store.update_account(&account).await.unwrap();
            self.store
                .set_account_capabilities(
                    account.id,
                    &capabilities.iter().copied().collect::<Capabilities>(),
                )
                .await
                .unwrap();
            self.store
                .upsert_collection(NewCollection {
                    archive_id: self.archive,
                    account_id: Some(account.id),
                    kind: holtmail_store::CollectionKind::Contacts,
                    remote_id: "addressbook/b1".into(),
                    name: "Contacts".into(),
                    color: None,
                })
                .await
                .unwrap()
        }

        fn writers(&self, enabled: bool) -> PimWriters {
            PimWriters::new(
                self.store.clone(),
                self.secrets.clone(),
                self.oauth.clone(),
                WriteConfig {
                    enabled,
                    ..WriteConfig::default()
                },
            )
        }

        async fn through(&self, collection: &Collection) -> Collection {
            holtmail_store::ItemChangeStore::set_write_mode(
                &*self.store,
                collection.id,
                WriteMode::WriteThrough,
            )
            .await
            .unwrap()
        }
    }

    fn jmap() -> serde_json::Value {
        json!({
            "url": "https://jmap.example.com/.well-known/jmap",
            "username": "ada",
            "auth": "password",
            "dav": { "enabled": true, "protocol": "jmap" },
        })
    }

    #[tokio::test]
    async fn writers_need_write_through_the_switch_and_pim_edit() {
        let w = world().await;
        let held = w
            .collection(AccountKind::Jmap, jmap(), &[Capability::PimEdit])
            .await;
        assert!(w.writers(true).upstream(&held).await.unwrap().is_none());
        let through = w.through(&held).await;
        assert!(w.writers(false).upstream(&through).await.unwrap().is_none());
        let writers = w.writers(true);
        let first = writers.upstream(&through).await.unwrap().unwrap();
        let again = writers.upstream(&through).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&first.writer, &again.writer), "kept");
        assert!(
            !writers.may_reply(&through).await.unwrap(),
            "pim.respond is off"
        );
        let answering = w
            .collection(
                AccountKind::Jmap,
                jmap(),
                &[Capability::PimEdit, Capability::PimRespond],
            )
            .await;
        assert!(writers.may_reply(&answering).await.unwrap());

        let other = w.collection(AccountKind::Jmap, jmap(), &[]).await;
        let other = w.through(&other).await;
        assert!(writers.upstream(&other).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn graph_writes_wait_for_the_contacts_and_calendars_consent() {
        let w = world().await;
        let mut settings = json!({ "dav": { "enabled": true, "protocol": "graph" } });
        OAuthSettings {
            provider: "microsoft".into(),
            access: Some("graph_pim".into()),
            ..OAuthSettings::default()
        }
        .write(&mut settings);
        let collection = w
            .collection(AccountKind::Graph, settings, &[Capability::PimEdit])
            .await;
        let collection = w.through(&collection).await;
        let error = match w.writers(true).upstream(&collection).await {
            Err(error) => error,
            Ok(_) => panic!("a Graph account without Contacts.ReadWrite got a writer"),
        };
        assert!(matches!(error, CollectionError::Auth(_)));
        let stored = w
            .store
            .get_account(collection.account_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            OAuthSettings::of(&stored)
                .unwrap()
                .missing_consent
                .as_deref(),
            Some("pim.edit")
        );
    }
}
