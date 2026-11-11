//! The archive-backed mail view keeps the mail view contract.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{
    AccountId, ArchiveId, DeviceId, DeviceScope, FolderId, MessageId, Timestamp, TransportId,
    UserId,
};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{Ingest, Placement};
use holtmail_mailview::testing::{Driver, Harness, Scenario, Setup, World, Write, conformance};
use holtmail_mailview::{
    EmailRef, Filter, MailViewError, MailboxId, NewEmail, Outcome, TextField, Viewer,
};
use holtmail_mailview_archive::{ArchiveMailView, ViewConfig};
use holtmail_outbox::{Archiver, OutboxError};
use holtmail_store::{
    AccountKind, Capabilities, Capability, DraftSave, FolderRole, GrantRole, InstanceRole,
    MailChangeQuery, MailChangeState, MailOp, NewAccount, NewIdentity, NewOutboxEntry, NewUser,
    OutboxState, SentCopyPolicy, Store, Transport, TransportKind, TransportLimits,
};
use holtmail_store_sqlite::SqliteStore;
use tokio::sync::Mutex;

struct IngestArchiver(Ingest);

#[async_trait]
impl Archiver for IngestArchiver {
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
                &Placement {
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

struct Upstream {
    store: Arc<dyn Store>,
    ingest: Ingest,
    archive: ArchiveId,
    accounts: HashMap<String, AccountId>,
    folders: HashMap<(String, String), FolderId>,
    next_uid: Mutex<HashMap<FolderId, u64>>,
    _dir: tempfile::TempDir,
}

impl Upstream {
    async fn uid(&self, folder: FolderId) -> u64 {
        let mut next = self.next_uid.lock().await;
        let uid = next.entry(folder).or_insert(0);
        *uid += 1;
        *uid
    }

    async fn place(&self, message: MessageId, folder: FolderId, flags: &[String]) -> u64 {
        let uid = self.uid(folder).await;
        self.store
            .record_location(message, folder, Some(uid), flags, Timestamp::now())
            .await
            .unwrap();
        uid
    }

    async fn settle_one(&self, account: AccountId, applied: bool) -> bool {
        let store = &self.store;
        let Some(change) = store
            .claim_mail_change(account, Timestamp::now())
            .await
            .unwrap()
        else {
            return false;
        };
        let new = &change.new;
        if !applied && !matches!(new.op, MailOp::Flags { .. } | MailOp::Labels { .. }) {
            store
                .fail_mail_change(change.id, "refused", Timestamp::now())
                .await
                .unwrap();
            return true;
        }
        let sent = Timestamp::now();
        let message = new.message_id.unwrap();
        match &new.op {
            MailOp::Flags { add, remove } => {
                store
                    .mark_mail_change_sent(change.id, None, sent)
                    .await
                    .unwrap();
                let uid = new.uid.unwrap();
                let mut flags = store
                    .locations(message)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|l| l.folder_id == new.folder_id)
                    .map(|l| l.flags)
                    .unwrap_or_default();
                if applied {
                    flags.retain(|f| !remove.iter().any(|r| r.eq_ignore_ascii_case(f)));
                    for flag in add {
                        if !flags.iter().any(|f| f.eq_ignore_ascii_case(flag)) {
                            flags.push(flag.clone());
                        }
                    }
                    store.set_flags(new.folder_id, uid, &flags).await.unwrap();
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
                store
                    .confirm_mail_flags(new.folder_id, uid, &flags, None, Timestamp::now())
                    .await
                    .unwrap();
            }
            MailOp::Labels { .. } => {
                store
                    .mark_mail_change_sent(change.id, None, sent)
                    .await
                    .unwrap();
                store
                    .settle_mail_changes(account, Timestamp::now(), Timestamp::now())
                    .await
                    .unwrap();
            }
            MailOp::Move { target } => {
                let uid = self.uid(*target).await;
                store
                    .mark_mail_change_sent(change.id, Some(uid), sent)
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
                let flags = store
                    .locations(message)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|l| l.folder_id == new.folder_id)
                    .map(|l| l.flags)
                    .unwrap_or_default();
                store
                    .record_location(message, *target, Some(uid), &flags, Timestamp::now())
                    .await
                    .unwrap();
                store
                    .mark_removed(new.folder_id, &[new.uid.unwrap()], Timestamp::now())
                    .await
                    .unwrap();
            }
            MailOp::Delete { .. } => {
                store
                    .mark_mail_change_sent(change.id, None, sent)
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
                store
                    .mark_removed(new.folder_id, &[new.uid.unwrap()], Timestamp::now())
                    .await
                    .unwrap();
            }
            MailOp::Append { flags, .. } => {
                let uid = self.uid(new.folder_id).await;
                store
                    .mark_mail_change_sent(change.id, Some(uid), sent)
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
                store
                    .record_location(message, new.folder_id, Some(uid), flags, Timestamp::now())
                    .await
                    .unwrap();
            }
        }
        true
    }
}

#[async_trait]
impl Driver for Upstream {
    async fn settle(&self, applied: bool) {
        for account in self.accounts.values() {
            while self.settle_one(*account, applied).await {}
            let open = self
                .store
                .list_mail_changes(&MailChangeQuery {
                    account: *account,
                    states: vec![MailChangeState::Held, MailChangeState::Sending],
                    limit: 500,
                })
                .await
                .unwrap();
            assert!(open.is_empty(), "every change settled");
        }
    }

    async fn deliver(&self, account: &str, folder: &str, raw: &str) -> MessageId {
        let folder = self.folders[&(account.to_string(), folder.to_string())];
        let uid = self.uid(folder).await;
        let done = self
            .ingest
            .ingest(
                self.archive,
                Bytes::from(raw.to_string()),
                &Placement {
                    folder: Some(folder),
                    uid: Some(uid),
                    received_at: Some(Timestamp::now()),
                    live: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        self.ingest.commit().await.unwrap();
        done.message_id
    }
}

fn capability(write: Write) -> Capability {
    match write {
        Write::Flags => Capability::MailFlags,
        Write::Move => Capability::MailMove,
        Write::Delete => Capability::MailDelete,
        Write::Expunge => Capability::MailExpunge,
        Write::Append => Capability::MailAppend,
    }
}

async fn user(store: &dyn Store, email: &str, role: InstanceRole) -> UserId {
    store
        .create_user(NewUser {
            email: email.into(),
            name: email.into(),
            role,
            password_hash: None,
        })
        .await
        .unwrap()
        .id
}

struct ArchiveHarness {
    search_cap: u32,
    last_store: std::sync::Mutex<Option<Arc<dyn Store>>>,
    last_blobs: std::sync::Mutex<Option<Arc<FsBlobStore>>>,
}

impl ArchiveHarness {
    fn new(search_cap: u32) -> Self {
        Self {
            search_cap,
            last_store: std::sync::Mutex::new(None),
            last_blobs: std::sync::Mutex::new(None),
        }
    }

    fn store(&self) -> Arc<dyn Store> {
        self.last_store.lock().unwrap().clone().expect("started")
    }
}

#[async_trait]
impl Harness for ArchiveHarness {
    async fn start(&self, scenario: &Scenario, setup: Setup) -> World {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            SqliteStore::open(dir.path().join("db.sqlite"))
                .await
                .unwrap(),
        );
        *self.last_store.lock().unwrap() = Some(store.clone());
        let blobs = Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        *self.last_blobs.lock().unwrap() = Some(blobs.clone());
        let index = Arc::new(TantivyIndex::open(dir.path().join("index")).unwrap());
        let ingest = Ingest::new(blobs.clone(), store.clone(), index.clone());
        let archive = store.create_archive("Mail view").await.unwrap().id;
        let owner = user(&*store, "owner@example.com", InstanceRole::Owner).await;
        let member = user(&*store, "member@example.com", InstanceRole::Member).await;
        let stranger = user(&*store, "stranger@example.com", InstanceRole::Member).await;
        store
            .set_grant_role(member, archive, Some(GrantRole::Reader))
            .await
            .unwrap();
        let mut accounts = HashMap::new();
        let mut folders = HashMap::new();
        for account in &scenario.accounts {
            let id = store
                .create_account(NewAccount {
                    archive_id: archive,
                    kind: AccountKind::Imap,
                    name: account.name.into(),
                    address: Some(format!("{}@example.com", account.name.to_lowercase())),
                    settings: serde_json::json!({}),
                    sealed_secret: None,
                    enabled: false,
                })
                .await
                .unwrap()
                .id;
            let capabilities: Capabilities =
                account.writes.iter().copied().map(capability).collect();
            store
                .set_account_capabilities(id, &capabilities)
                .await
                .unwrap();
            for (path, role) in account.folders {
                let folder = store.upsert_folder(id, path, *role).await.unwrap().id;
                folders.insert((account.name.to_string(), path.to_string()), folder);
            }
            accounts.insert(account.name.to_string(), id);
        }
        let upstream = Arc::new(Upstream {
            store: store.clone(),
            ingest: Ingest::new(blobs.clone(), store.clone(), index.clone()),
            archive,
            accounts,
            folders: folders.clone(),
            next_uid: Mutex::new(HashMap::new()),
            _dir: dir,
        });
        let mut messages = HashMap::new();
        for message in &scenario.messages {
            let mut id = None;
            for place in &message.places {
                let folder = folders[&(place.account.to_string(), place.folder.to_string())];
                let flags: Vec<String> = place.flags.iter().map(|f| f.to_string()).collect();
                let uid = match id {
                    None => {
                        let uid = upstream.uid(folder).await;
                        let done = ingest
                            .ingest(
                                archive,
                                Bytes::from(message.raw.clone()),
                                &Placement {
                                    folder: Some(folder),
                                    uid: Some(uid),
                                    flags,
                                    received_at: Some(message.received_at),
                                    ..Default::default()
                                },
                            )
                            .await
                            .unwrap();
                        id = Some(done.message_id);
                        uid
                    }
                    Some(id) => upstream.place(id, folder, &flags).await,
                };
                if !place.live {
                    store
                        .mark_removed(folder, &[uid], Timestamp::now())
                        .await
                        .unwrap();
                }
            }
            messages.insert(message.name.to_string(), id.unwrap());
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        ingest.commit().await.unwrap();
        let view = Arc::new(ArchiveMailView::new(
            store.clone(),
            blobs.clone(),
            index.clone(),
            Arc::new(IngestArchiver(Ingest::new(blobs, store.clone(), index))),
            ViewConfig {
                local_flags: setup.local_flags,
                append_limit: 1024 * 1024,
                search_cap: self.search_cap,
                ..ViewConfig::default()
            },
        ));
        let viewer = |user, scopes: &[DeviceScope]| Viewer {
            user,
            device: Some(DeviceId::new()),
            archive,
            scopes: scopes.iter().copied().collect(),
        };
        World {
            view: view.clone(),
            actions: view,
            driver: upstream,
            editor: viewer(owner, &[DeviceScope::Read, DeviceScope::WriteMail]),
            reader: viewer(owner, &[DeviceScope::Read]),
            member: viewer(member, &[DeviceScope::Read, DeviceScope::WriteMail]),
            stranger: viewer(stranger, &DeviceScope::ALL),
            folders,
            messages,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_archive_keeps_the_contract() {
    conformance(&ArchiveHarness::new(ViewConfig::default().search_cap)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_too_broad_to_answer_fully_fails() {
    let w = ArchiveHarness::new(1)
        .start(&Scenario::two_accounts(), Setup::default())
        .await;
    let to_bob = Filter::Text(TextField::To, "bob".into());
    let inbox = MailboxId::Role(FolderRole::Inbox);
    assert!(
        matches!(
            w.view.search(&w.editor, &inbox, &to_bob).await,
            Err(MailViewError::TooMany { limit: 1 })
        ),
        "more matches than the cap in the mailbox's dates"
    );
    assert!(
        matches!(
            w.view
                .search(&w.editor, &inbox, &Filter::Not(Box::new(to_bob.clone())))
                .await,
            Err(MailViewError::TooMany { .. })
        ),
        "never inverted into wrong matches"
    );
    let projects = MailboxId::Folder(w.folder("Work", "Projects/2026"));
    let found: Vec<_> = w
        .view
        .search(&w.editor, &projects, &to_bob)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.email)
        .collect();
    assert_eq!(
        found,
        vec![w.email("report")],
        "narrowed to the mailbox's dates the text fits under the cap"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_structure_parsed_once_is_kept_by_blob() {
    let harness = ArchiveHarness::new(ViewConfig::default().search_cap);
    let w = harness
        .start(&Scenario::two_accounts(), Setup::default())
        .await;
    let email = w.email("report");
    let first = w.view.structure(&w.editor, &email).await.unwrap();
    let blob = w.view.envelopes(&w.editor, &[email]).await.unwrap()[0].blob;
    let blobs = harness.last_blobs.lock().unwrap().clone().unwrap();
    blobs.delete(&blob).await.unwrap();
    assert_eq!(
        w.view.structure(&w.editor, &email).await.unwrap(),
        first,
        "the second fetch neither reads nor parses the bytes"
    );
    assert!(
        w.view.structure(&w.reader, &email).await.is_ok(),
        "the cache is shared, the access check is not skipped"
    );
    assert!(w.view.structure(&w.stranger, &email).await.is_err());
}

fn draft_bytes() -> Bytes {
    Bytes::from_static(
        b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Plans\r\n\
          Message-ID: <plans@example.com>\r\n\r\nA first draft.\r\n",
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_destroy_a_client_draft_the_web_app_took_over() {
    let harness = ArchiveHarness::new(ViewConfig::default().search_cap);
    let w = harness
        .start(&Scenario::one_account(), Setup::default())
        .await;
    let store = harness.store();
    let appended = w
        .actions
        .append(
            &w.editor,
            &MailboxId::Drafts,
            NewEmail {
                raw: draft_bytes(),
                flags: Vec::new(),
                received_at: None,
            },
        )
        .await
        .unwrap();
    let Some(EmailRef::Draft { draft, version }) = appended.email else {
        panic!("a draft");
    };
    assert_eq!(version, 1);
    let since = w.view.state(&w.editor).await.unwrap().email;
    let saved = store
        .save_draft(
            draft,
            1,
            &serde_json::json!({"subject": "Plans"}),
            &[],
            Timestamp::now(),
        )
        .await
        .unwrap();
    assert!(matches!(saved, DraftSave::Saved(_)));
    let changes = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert_eq!(
        changes.emails.destroyed,
        vec![EmailRef::Draft { draft, version: 1 }],
        "the served version is gone once the web app saved over it"
    );
    assert!(
        changes.emails.created.is_empty(),
        "compose drafts are not served yet"
    );

    let appended = w
        .actions
        .append(
            &w.editor,
            &MailboxId::Drafts,
            NewEmail {
                raw: draft_bytes(),
                flags: Vec::new(),
                received_at: None,
            },
        )
        .await
        .unwrap();
    let Some(EmailRef::Draft { draft, .. }) = appended.email else {
        panic!("a draft");
    };
    let since = w.view.state(&w.editor).await.unwrap().email;
    store
        .save_draft(
            draft,
            1,
            &serde_json::json!({"subject": "Plans"}),
            &[],
            Timestamp::now(),
        )
        .await
        .unwrap();
    store.delete_draft(draft).await.unwrap();
    let changes = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert_eq!(
        changes.emails.destroyed,
        vec![EmailRef::Draft { draft, version: 1 }],
        "a save and a delete in one window still destroy what the client knew"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_changes_follow_the_layout() {
    let harness = ArchiveHarness::new(ViewConfig::default().search_cap);
    let w = harness
        .start(&Scenario::two_accounts(), Setup::default())
        .await;
    let store = harness.store();
    let home = store
        .list_accounts(w.editor.archive)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Home")
        .unwrap()
        .id;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let since = w.view.state(&w.editor).await.unwrap().mailbox;
    let trash = store
        .upsert_folder(home, "Bin", Some(FolderRole::Trash))
        .await
        .unwrap()
        .id;
    let nested = store
        .upsert_folder(home, "Clubs/Chess", None)
        .await
        .unwrap()
        .id;
    let changes = w.view.changes(&w.editor, since, 100).await.unwrap();
    let created = &changes.mailboxes.created;
    assert!(created.contains(&MailboxId::Folder(trash)));
    assert!(created.contains(&MailboxId::Folder(nested)));
    assert!(
        changes
            .mailboxes
            .updated
            .contains(&MailboxId::Role(FolderRole::Trash)),
        "the unified Trash gains a folder: {changes:?}"
    );
    assert!(
        created
            .iter()
            .any(|m| matches!(m, MailboxId::Node(path) if path.ends_with("Clubs"))),
        "the new tree level appears: {changes:?}"
    );

    tokio::time::sleep(Duration::from_millis(5)).await;
    let since = w.view.state(&w.editor).await.unwrap().mailbox;
    store.upsert_folder(home, "Bin", None).await.unwrap();
    let work_trash = w.folder("Work", "Trash");
    store
        .upsert_folder(
            store
                .list_accounts(w.editor.archive)
                .await
                .unwrap()
                .into_iter()
                .find(|a| a.name == "Work")
                .unwrap()
                .id,
            "Trash",
            None,
        )
        .await
        .unwrap();
    let changes = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert!(
        changes
            .mailboxes
            .updated
            .contains(&MailboxId::Folder(trash))
    );
    assert!(
        changes
            .mailboxes
            .updated
            .contains(&MailboxId::Folder(work_trash))
    );
    assert!(
        changes
            .mailboxes
            .destroyed
            .contains(&MailboxId::Role(FolderRole::Trash)),
        "the unified Trash is gone with its last folder: {changes:?}"
    );

    let since = w.view.state(&w.editor).await.unwrap().mailbox;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let third = store
        .create_account(NewAccount {
            archive_id: w.editor.archive,
            kind: AccountKind::Imap,
            name: "Club".into(),
            address: Some("club@example.com".into()),
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    store
        .upsert_folder(third, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    assert!(
        matches!(
            w.view.changes(&w.editor, since, 100).await,
            Err(MailViewError::CannotCalculateChanges)
        ),
        "a new account moves every path"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_sent_copy_answers_with_the_outbox_message() {
    let harness = ArchiveHarness::new(ViewConfig::default().search_cap);
    let w = harness
        .start(&Scenario::one_account(), Setup::default())
        .await;
    let store = harness.store();
    let blobs = harness.last_blobs.lock().unwrap().clone().unwrap();
    let archive = w.editor.archive;
    let transport = Transport {
        id: TransportId::new(),
        archive_id: Some(archive),
        name: "Relay".into(),
        kind: TransportKind::Smtp,
        account_id: None,
        settings: serde_json::json!({ "host": "smtp.example.com", "port": 587 }),
        sealed_secret: None,
        allowed_from: vec!["*@example.com".into()],
        limits: TransportLimits::default(),
        enabled: true,
        smtputf8: false,
        created_at: Timestamp::now(),
    };
    store.create_transport(&transport).await.unwrap();
    let identity = store
        .create_identity(NewIdentity {
            archive_id: archive,
            address: "ada@example.com".into(),
            name: Some("Ada".into()),
            signature_html: None,
            signature_text: None,
            reply_to: None,
            transport_id: transport.id,
            sent_copy: SentCopyPolicy::Append("Sent".into()),
            bcc_self: false,
            account_id: None,
            alias: None,
            plus_addresses: true,
            active: true,
            created_by: Some(w.editor.user),
        })
        .await
        .unwrap();
    let sent = Bytes::from_static(
        b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Lunch\r\n\
          Message-ID: <lunch@example.com>\r\n\r\nNoon?\r\n",
    );
    let blob = blobs.put(sent.clone()).await.unwrap();
    let now = Timestamp::now();
    store
        .enqueue_outbox(&NewOutboxEntry {
            archive_id: archive,
            identity_id: identity.id,
            transport_id: transport.id,
            user_id: Some(w.editor.user),
            device_id: w.editor.device,
            route: serde_json::json!({}),
            mail_from: "ada@example.com".into(),
            recipients: vec!["bob@example.org".into(), "carol@example.org".into()],
            blob_id: blob,
            size: sent.len() as u64,
            rfc_message_id: "lunch@example.com".into(),
            send_at: now,
            state: OutboxState::Queued,
            flow_run: None,
            idempotency_key: None,
            resend_of: None,
            at: now,
        })
        .await
        .unwrap();
    let copy = Bytes::from_static(
        b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\n\
          Bcc: carol@example.org\r\nSubject: Lunch\r\n\
          Message-ID: <lunch@example.com>\r\n\r\nNoon?\r\n",
    );
    let sent_box = MailboxId::Folder(w.folder("Work", "Sent"));
    let append = |raw: Bytes| NewEmail {
        raw,
        flags: vec!["\\Seen".into()],
        received_at: None,
    };
    let first = w
        .actions
        .append(&w.editor, &sent_box, append(copy.clone()))
        .await
        .unwrap();
    let email = first.email.expect("the outbox message");
    assert!(first.uid.is_some(), "a UID in Sent at once");
    assert_eq!(
        w.view.raw(&w.editor, &email).await.unwrap(),
        sent,
        "answered with the outbox's own bytes, not the client's"
    );
    let again = w
        .actions
        .append(&w.editor, &sent_box, append(copy))
        .await
        .unwrap();
    assert_eq!((again.email, again.uid), (Some(email), first.uid));
    let work = store
        .list_accounts(archive)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Work")
        .unwrap()
        .id;
    let appends = store
        .list_mail_changes(&MailChangeQuery {
            account: work,
            states: Vec::new(),
            limit: 500,
        })
        .await
        .unwrap();
    assert!(
        !appends
            .iter()
            .any(|c| matches!(c.new.op, MailOp::Append { .. })),
        "nothing is appended upstream for a linked copy"
    );
    let unmatched = Bytes::from_static(
        b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Other\r\n\
          Message-ID: <other@example.com>\r\n\r\nHi\r\n",
    );
    let other = w
        .actions
        .append(&w.editor, &sent_box, append(unmatched))
        .await
        .unwrap();
    assert!(
        other.email.is_some_and(|e| e != email),
        "unmatched bytes are archived on their own"
    );
    assert_eq!(other.outcome, Outcome::Held, "and appended upstream");
    let audit = store
        .list_audit(&holtmail_store::AuditQuery {
            archive: Some(archive),
            before: None,
            limit: 50,
        })
        .await
        .unwrap();
    let device = format!("device:{}", w.editor.device.unwrap());
    let entry = audit
        .iter()
        .find(|e| e.entry.kind == holtmail_store::AuditKind::MailAppend)
        .unwrap_or_else(|| panic!("the append is audited: {audit:?}"));
    assert!(entry.entry.targets.contains(&device), "naming the device");
    assert_eq!(entry.entry.count, 1, "counting messages only");
}
