//! Mailbox changes: the writers `mail-writeback` sends held changes through, handed out only for
//! accounts whose capability for the change is on while the instance allows writing, and the
//! retention job that deletes old mail from servers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use holtmail_core::{AccountId, Timestamp};
use holtmail_mail_writeback::{Writeback, WritebackConfig, Writers};
use holtmail_secret::SecretBox;
use holtmail_source::{MailboxWriter, SourceError};
use holtmail_source_graph::{GraphSettings, GraphSource};
use holtmail_source_imap::{ImapSettings, ImapSource};
use holtmail_source_jmap::{JmapSettings, JmapSource};
use holtmail_store::{
    Account, AccountKind, AuditKind, FolderRole, MailOp, NewAuditEntry, Store, StoreError,
};

use crate::accounts;
use crate::config::{self, Services, WriteConfig};
use crate::oauth::OAuth;

struct Cached {
    at: Instant,
    built_from: Fingerprint,
    writer: Arc<dyn MailboxWriter>,
}

#[derive(PartialEq, Eq)]
struct Fingerprint {
    settings: serde_json::Value,
    secret: Option<Vec<u8>>,
}

impl Fingerprint {
    fn of(account: &Account) -> Self {
        Self {
            settings: account.settings.clone(),
            secret: account.sealed_secret.clone(),
        }
    }
}

const WRITER_TTL: Duration = Duration::from_secs(5 * 60);
const RETENTION_EVERY: Duration = Duration::from_secs(60 * 60);
const RETENTION_BATCH: u32 = 1000;

/// Builds the [`MailboxWriter`] of an account for a change, refusing (with `None`) whatever the
/// instance switch or the account's capabilities do not allow. Writers are kept for a few
/// minutes so a run of changes signs in once, and only while the account's settings and stored
/// sign-in stay the same: a new host, user, password or OAuth sign-in builds a new writer at
/// the next change.
pub struct ServerWriters {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretBox>,
    oauth: Arc<OAuth>,
    switches: WriteConfig,
    cache: Mutex<HashMap<AccountId, Cached>>,
}

impl ServerWriters {
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

    async fn to_trash(&self, op: &MailOp) -> Result<bool, StoreError> {
        let MailOp::Move { target } = op else {
            return Ok(false);
        };
        Ok(self
            .store
            .get_folder(*target)
            .await?
            .is_some_and(|f| f.role == Some(FolderRole::Trash)))
    }

    fn cached(&self, account: &Account) -> Option<Arc<dyn MailboxWriter>> {
        let mut cache = self.cache.lock().expect("writer cache");
        match cache.get(&account.id) {
            Some(cached)
                if cached.at.elapsed() < WRITER_TTL
                    && cached.built_from == Fingerprint::of(account) =>
            {
                Some(cached.writer.clone())
            }
            Some(_) => {
                cache.remove(&account.id);
                None
            }
            None => None,
        }
    }

    async fn build(&self, account: &Account) -> Result<Arc<dyn MailboxWriter>, SourceError> {
        let credentials = accounts::credentials(account, &*self.store, &*self.secrets, &self.oauth)
            .await
            .map_err(|e| SourceError::Auth(e.to_string()))?;
        let roots = config::tls_roots();
        Ok(match account.kind {
            AccountKind::Imap => {
                let settings = ImapSettings::from_json(&account.settings)?;
                let source = ImapSource::new(settings, credentials).with_roots(&roots)?;
                Arc::new(source.writer().await?)
            }
            AccountKind::Jmap => {
                let settings = JmapSettings::from_json(&account.settings)?;
                Arc::new(
                    JmapSource::new(settings, credentials)?
                        .with_roots(&roots)?
                        .writer(),
                )
            }
            AccountKind::Graph => {
                let settings = GraphSettings::from_json(&account.settings)?;
                Arc::new(
                    GraphSource::new(&settings, credentials)?
                        .with_roots(&roots)?
                        .writer(),
                )
            }
            AccountKind::Import => {
                return Err(SourceError::Config("import accounts have no server".into()));
            }
        })
    }
}

#[async_trait]
impl Writers for ServerWriters {
    async fn writer(
        &self,
        account: AccountId,
        op: &MailOp,
    ) -> Result<Option<Arc<dyn MailboxWriter>>, SourceError> {
        let store_error = |e: StoreError| SourceError::Connection(e.to_string());
        if !self.switches.enabled {
            return Ok(None);
        }
        let Some(account) = self.store.get_account(account).await.map_err(store_error)? else {
            return Ok(None);
        };
        let capabilities = self
            .store
            .account_capabilities(account.id)
            .await
            .map_err(store_error)?;
        let to_trash = self.to_trash(op).await.map_err(store_error)?;
        if !holtmail_mail_writeback::allows(op, to_trash, &capabilities)
            || account.kind == AccountKind::Import
        {
            return Ok(None);
        }
        if holtmail_mail_writeback::write_consent_missing(&account) {
            if let Err(error) = self
                .oauth
                .ask_consent(&account, &*self.store, "mail.write")
                .await
            {
                tracing::warn!(account = %account.id, %error, "write consent not recorded");
            }
            return Err(SourceError::Auth(
                "the sign-in does not allow changing mail; sign in again".into(),
            ));
        }
        if let Some(writer) = self.cached(&account) {
            return Ok(Some(writer));
        }
        let writer = self.build(&account).await?;
        self.cache.lock().expect("writer cache").insert(
            account.id,
            Cached {
                at: Instant::now(),
                built_from: Fingerprint::of(&account),
                writer: writer.clone(),
            },
        );
        Ok(Some(writer))
    }
}

/// The writeback over the server's store, blobs and writers.
pub fn open(services: &Services, config: &config::Config) -> Arc<Writeback> {
    let writers = Arc::new(ServerWriters::new(
        services.store.clone(),
        services.secrets.clone(),
        services.oauth.clone(),
        config.write,
    ));
    Arc::new(Writeback::new(
        services.store.clone(),
        services.blobs.clone(),
        writers,
        WritebackConfig::default(),
    ))
}

/// Puts changes a stopped process left mid-send back to held, then sends held changes until
/// the process ends.
pub async fn run(writeback: Arc<Writeback>) {
    match writeback.recover().await {
        Ok(ids) if !ids.is_empty() => {
            tracing::warn!(
                count = ids.len(),
                "mail changes interrupted by a restart go again"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "interrupted mail changes not recovered"),
    }
    writeback.run().await;
}

/// Records retention deletes for every account with a retention period, `mail.delete` and the
/// instance switch on, at most [`RETENTION_BATCH`] per account; returns how many. Each
/// account's batch is one `mail_delete` audit entry by the server.
pub async fn retention_pass(store: &dyn Store, switches: WriteConfig) -> Result<u64, StoreError> {
    if !switches.enabled {
        return Ok(0);
    }
    let now = Timestamp::now();
    let mut recorded = 0;
    for archive in store.list_archives().await? {
        for account in store.list_accounts(archive.id).await? {
            let Some(days) = holtmail_mail_writeback::retention_days(&account) else {
                continue;
            };
            if account.kind == AccountKind::Import {
                continue;
            }
            let plan = holtmail_mail_writeback::plan_retention(
                store,
                &account,
                days,
                now,
                None,
                RETENTION_BATCH,
            )
            .await?;
            if plan.changes.is_empty() {
                continue;
            }
            let mut ids = Vec::with_capacity(plan.changes.len());
            for change in &plan.changes {
                match store.enqueue_mail_change(change).await {
                    Ok(change) => ids.push(change.id.to_string()),
                    Err(StoreError::Invalid(_) | StoreError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            let count = ids.len() as u64;
            store
                .append_audit(&NewAuditEntry {
                    at: now,
                    user: None,
                    kind: AuditKind::MailDelete,
                    archive: Some(account.archive_id),
                    account: Some(account.id),
                    targets: ids,
                    count,
                })
                .await?;
            tracing::info!(account = %account.id, count, expunge = plan.expunge,
                "retention deletes held for the server");
            recorded += count;
        }
    }
    Ok(recorded)
}

/// Runs [`retention_pass`] every hour until the process ends.
pub async fn run_retention(
    store: Arc<dyn Store>,
    switches: WriteConfig,
    writeback: Arc<Writeback>,
) {
    let mut every = tokio::time::interval(RETENTION_EVERY);
    loop {
        every.tick().await;
        match retention_pass(&*store, switches).await {
            Ok(0) => {}
            Ok(_) => writeback.wake(),
            Err(error) => tracing::warn!(%error, "retention pass failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use holtmail_core::{ArchiveId, FolderId};
    use holtmail_oauth::Client;
    use holtmail_oauth::testing::FakeTokenEndpoint;
    use holtmail_secret_local::LocalSecretBox;
    use holtmail_store::{
        ArchiveStore, AuditQuery, Capabilities, Capability, MailChangeOrigin, MailChangeQuery,
        MailChangeStore, MessageStore, NewAccount, WriteStore,
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

    fn caps(list: &[Capability]) -> Capabilities {
        list.iter().copied().collect()
    }

    impl World {
        async fn account(
            &self,
            kind: AccountKind,
            settings: serde_json::Value,
            capabilities: &[Capability],
        ) -> Account {
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
                .set_account_capabilities(account.id, &caps(capabilities))
                .await
                .unwrap();
            account
        }

        fn writers(&self, enabled: bool) -> ServerWriters {
            ServerWriters::new(
                self.store.clone(),
                self.secrets.clone(),
                self.oauth.clone(),
                WriteConfig {
                    enabled,
                    ..WriteConfig::default()
                },
            )
        }

        async fn folder(&self, account: AccountId, path: &str, role: FolderRole) -> FolderId {
            self.store
                .upsert_folder(account, path, Some(role))
                .await
                .unwrap()
                .id
        }

        async fn old_message(&self, n: u32, folder: FolderId, uid: u64) {
            let id = self
                .store
                .insert_message(holtmail_store::testing::message(self.archive, n, 1000))
                .await
                .unwrap()
                .message_id;
            self.store
                .record_location(id, folder, Some(uid), &[], Timestamp::from_secs(2000))
                .await
                .unwrap();
        }

        async fn audits(&self) -> Vec<holtmail_store::AuditEntry> {
            self.store
                .list_audit(&AuditQuery {
                    archive: Some(self.archive),
                    before: None,
                    limit: 100,
                })
                .await
                .unwrap()
        }
    }

    fn jmap() -> serde_json::Value {
        json!({ "url": "https://jmap.example.com/.well-known/jmap", "username": "ada", "auth": "password" })
    }

    fn flags() -> MailOp {
        MailOp::Flags {
            add: vec!["\\Seen".into()],
            remove: Vec::new(),
        }
    }

    #[tokio::test]
    async fn writers_follow_the_switch_the_locks_and_the_account() {
        let w = world().await;
        let account = w
            .account(AccountKind::Jmap, jmap(), &[Capability::MailFlags])
            .await;
        let trash = w.folder(account.id, "Trash", FolderRole::Trash).await;
        let to_trash = MailOp::Move { target: trash };

        assert!(
            w.writers(false)
                .writer(account.id, &flags())
                .await
                .unwrap()
                .is_none()
        );
        let writers = w.writers(true);
        let first = writers.writer(account.id, &flags()).await.unwrap().unwrap();
        assert!(
            writers
                .writer(account.id, &to_trash)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            writers
                .writer(account.id, &MailOp::Delete { expunge: true })
                .await
                .unwrap()
                .is_none()
        );
        w.store
            .set_account_capabilities(account.id, &caps(&[Capability::MailDelete]))
            .await
            .unwrap();
        assert!(
            writers
                .writer(account.id, &to_trash)
                .await
                .unwrap()
                .is_some(),
            "a move to the trash needs only mail.delete"
        );
        assert!(
            writers
                .writer(account.id, &flags())
                .await
                .unwrap()
                .is_none()
        );

        w.store
            .set_account_capabilities(account.id, &caps(&[Capability::MailFlags]))
            .await
            .unwrap();
        let again = writers.writer(account.id, &flags()).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &again), "the writer is kept");
        let mut moved = w.store.get_account(account.id).await.unwrap().unwrap();
        moved.settings = json!({ "url": "https://other.example.org/.well-known/jmap", "username": "ada", "auth": "password" });
        w.store.update_account(&moved).await.unwrap();
        let rebuilt = writers.writer(account.id, &flags()).await.unwrap().unwrap();
        assert!(
            !Arc::ptr_eq(&first, &rebuilt),
            "a changed account gets a new writer"
        );

        let import = w
            .account(AccountKind::Import, json!({}), &[Capability::MailFlags])
            .await;
        assert!(writers.writer(import.id, &flags()).await.unwrap().is_none());
        assert!(
            writers
                .writer(AccountId::new(), &flags())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn graph_writes_wait_for_the_write_consent() {
        let w = world().await;
        let mut settings = json!({});
        OAuthSettings {
            provider: "microsoft".into(),
            ..OAuthSettings::default()
        }
        .write(&mut settings);
        let account = w
            .account(AccountKind::Graph, settings, &[Capability::MailFlags])
            .await;
        let error = match w.writers(true).writer(account.id, &flags()).await {
            Err(error) => error,
            Ok(_) => panic!("a Graph account without Mail.ReadWrite got a writer"),
        };
        assert!(matches!(error, SourceError::Auth(_)));
        let stored = w.store.get_account(account.id).await.unwrap().unwrap();
        assert_eq!(
            OAuthSettings::of(&stored)
                .unwrap()
                .missing_consent
                .as_deref(),
            Some("mail.write")
        );
    }

    #[tokio::test]
    async fn retention_records_old_mail_once_with_one_audit_entry() {
        let w = world().await;
        let settings = holtmail_mail_writeback::with_retention_days(&json!({}), Some(30));
        let account = w
            .account(
                AccountKind::Imap,
                settings.clone(),
                &[Capability::MailDelete],
            )
            .await;
        let inbox = w.folder(account.id, "INBOX", FolderRole::Inbox).await;
        let trash = w.folder(account.id, "Trash", FolderRole::Trash).await;
        w.old_message(1, inbox, 1).await;
        w.old_message(2, inbox, 2).await;
        w.old_message(3, trash, 3).await;
        let import = w
            .account(
                AccountKind::Import,
                settings.clone(),
                &[Capability::MailDelete],
            )
            .await;
        let elsewhere = w.folder(import.id, "INBOX", FolderRole::Inbox).await;
        w.old_message(4, elsewhere, 4).await;

        assert_eq!(
            retention_pass(
                &*w.store,
                WriteConfig {
                    enabled: false,
                    ..WriteConfig::default()
                }
            )
            .await
            .unwrap(),
            0,
            "nothing with the switch off"
        );
        assert_eq!(
            retention_pass(&*w.store, WriteConfig::default())
                .await
                .unwrap(),
            2
        );
        let changes = w
            .store
            .list_mail_changes(&MailChangeQuery {
                account: account.id,
                states: Vec::new(),
                limit: 100,
            })
            .await
            .unwrap();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.new.origin == MailChangeOrigin::Retention
                    && c.new.op == MailOp::Move { target: trash }
                    && c.new.folder_id == inbox)
        );
        let audits = w.audits().await;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].entry.kind, AuditKind::MailDelete);
        assert_eq!(audits[0].entry.user, None);
        assert_eq!(audits[0].entry.count, 2);
        assert_eq!(
            retention_pass(&*w.store, WriteConfig::default())
                .await
                .unwrap(),
            0
        );
        assert_eq!(w.audits().await.len(), 1, "a second pass records nothing");

        for change in &changes {
            w.store
                .fail_mail_change(change.id, "missing_folder", Timestamp::now())
                .await
                .unwrap();
        }
        assert_eq!(
            retention_pass(&*w.store, WriteConfig::default())
                .await
                .unwrap(),
            0,
            "failed retention changes are not recorded again"
        );

        let expunging = w
            .account(
                AccountKind::Imap,
                settings,
                &[Capability::MailDelete, Capability::MailExpunge],
            )
            .await;
        let folder = w.folder(expunging.id, "INBOX", FolderRole::Inbox).await;
        w.old_message(5, folder, 5).await;
        assert_eq!(
            retention_pass(&*w.store, WriteConfig::default())
                .await
                .unwrap(),
            1
        );
        let expunged = w
            .store
            .list_mail_changes(&MailChangeQuery {
                account: expunging.id,
                states: Vec::new(),
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(expunged[0].new.op, MailOp::Delete { expunge: true });
    }
}
