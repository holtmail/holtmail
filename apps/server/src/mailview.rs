//! The served mail view: the change hub every push channel waits on, the archive-backed
//! [`ArchiveMailView`] and the job that builds served mailboxes ahead of the first client.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holtmail_access::Access;
use holtmail_core::{ArchiveId, DeviceProtocol, DeviceScope, Timestamp, UserId};
use holtmail_mailview::Viewer;
use holtmail_mailview_archive::{ArchiveMailView, ViewConfig};
use holtmail_store::{ChangeLogEntry, ServedPrune, Store, StoreError};
use tokio::sync::watch;

use crate::config::{Config, Services};

/// How often one listener is woken at most; changes in between are coalesced into one wake.
pub const WAKE_EVERY: Duration = Duration::from_millis(250);

const BATCH: u32 = 1_000;

struct Subscriber {
    archive: ArchiveId,
    user: UserId,
    tx: watch::Sender<u64>,
}

/// Reads each new batch of the change log once and wakes only the listeners of the archives it
/// touches; rows of one user's own state wake only that user's listeners. Listeners learn that
/// something after a `seq` changed, never what.
#[derive(Default)]
pub struct ChangeHub {
    subscribers: Mutex<Vec<Subscriber>>,
}

/// One listener of the hub, for one archive through one user's eyes.
pub struct HubListener {
    rx: watch::Receiver<u64>,
    last: Option<Instant>,
    pending: bool,
}

impl HubListener {
    /// Waits until a change after the last one returned touches the listener's archive and
    /// returns the highest `seq` seen; at most once per [`WAKE_EVERY`]. `None` once the hub
    /// stopped. Safe to cancel: a change seen by a call that was dropped is returned by the next.
    pub async fn changed(&mut self) -> Option<u64> {
        if !self.pending {
            self.rx.changed().await.ok()?;
            self.pending = true;
        }
        if let Some(last) = self.last {
            let next = last + WAKE_EVERY;
            if next > Instant::now() {
                tokio::time::sleep_until(next.into()).await;
            }
        }
        self.pending = false;
        self.last = Some(Instant::now());
        Some(*self.rx.borrow_and_update())
    }
}

impl ChangeHub {
    /// A listener for changes of `archive` that `user` may see.
    pub fn subscribe(&self, archive: ArchiveId, user: UserId) -> HubListener {
        let (tx, rx) = watch::channel(0);
        self.subscribers
            .lock()
            .expect("hub subscribers")
            .push(Subscriber { archive, user, tx });
        HubListener {
            rx,
            last: None,
            pending: false,
        }
    }

    fn fan_out(&self, rows: &[ChangeLogEntry]) {
        let mut subscribers = self.subscribers.lock().expect("hub subscribers");
        subscribers.retain(|s| !s.tx.is_closed());
        for subscriber in subscribers.iter() {
            let newest = rows
                .iter()
                .filter(|r| {
                    r.archive_id == subscriber.archive
                        && r.user_id.is_none_or(|u| u == subscriber.user)
                })
                .map(|r| r.seq)
                .max();
            if let Some(seq) = newest {
                subscriber.tx.send_replace(seq);
            }
        }
    }

    /// Follows the change log until the process ends.
    pub async fn run(self: Arc<Self>, store: Arc<dyn Store>) {
        let mut watch = loop {
            match store.watch_changes().await {
                Ok(watch) => break watch,
                Err(error) => {
                    tracing::warn!(%error, "change log not watched; trying again");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        };
        let mut cursor = match store.latest_seq().await {
            Ok(seq) => seq,
            Err(error) => {
                tracing::warn!(%error, "change log head not read");
                0
            }
        };
        loop {
            let head = watch.next().await;
            let mut pause = Duration::from_secs(1);
            while cursor < head {
                match store.all_changes_since(cursor, BATCH).await {
                    Ok(rows) if rows.is_empty() => {
                        cursor = head;
                    }
                    Ok(rows) => {
                        cursor = rows.last().map_or(head, |r| r.seq);
                        self.fan_out(&rows);
                        pause = Duration::from_secs(1);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "change log not read; trying again");
                        tokio::time::sleep(pause).await;
                        pause = (pause * 2).min(Duration::from_secs(30));
                    }
                }
            }
        }
    }
}

/// How long the change log keeps its rows (CLIENTS.md, SERVE.md "Pruning").
pub const CHANGE_LOG_KEEP: Duration = Duration::from_secs(90 * 24 * 60 * 60);

const PRUNE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Prunes the change log to [`CHANGE_LOG_KEEP`] before `now`, then the served tables to the
/// same `seq`. Returns that `seq` and what the served tables lost. Safe to repeat: a pass that
/// failed halfway is finished by the next.
pub async fn prune_change_log(
    store: &dyn Store,
    now: Timestamp,
) -> Result<(u64, ServedPrune), StoreError> {
    let before = Timestamp::from_millis(now.as_millis() - CHANGE_LOG_KEEP.as_millis() as i64);
    let through = store.prune_changes(before).await?;
    if through == 0 {
        return Ok((0, ServedPrune::default()));
    }
    let served = store.prune_served(through, now).await?;
    Ok((through, served))
}

/// Runs [`prune_change_log`] every six hours until the process ends.
pub async fn run_pruning(store: Arc<dyn Store>) {
    let mut every = tokio::time::interval(PRUNE_EVERY);
    loop {
        every.tick().await;
        match prune_change_log(&*store, Timestamp::now()).await {
            Ok((through, served)) => tracing::debug!(
                through,
                tombstones = served.tombstones,
                stale = served.stale,
                "change log pruned"
            ),
            Err(error) => tracing::warn!(%error, "change log not pruned"),
        }
    }
}

/// The mail view of the running server.
pub fn open(services: &Services, config: &Config) -> Arc<ArchiveMailView> {
    Arc::new(ArchiveMailView::new(
        services.store.clone(),
        services.blobs.clone(),
        services.index.clone(),
        Arc::new(crate::send::ServerArchiver(services.ingest())),
        ViewConfig {
            switches: config.write.into(),
            local_flags: config.serve.local_flags,
            ..ViewConfig::default()
        },
    ))
}

/// The archives and users whose views clients will read: every live device with IMAP or JMAP,
/// IMAP for its mail archive, JMAP for its archives (or every archive its user can read).
async fn served(store: &Arc<dyn Store>, access: &Access) -> Vec<(ArchiveId, UserId)> {
    let mut out: Vec<(ArchiveId, UserId)> = Vec::new();
    let Ok(users) = store.list_users().await else {
        return out;
    };
    let archives = store.list_archives().await.unwrap_or_default();
    for user in users {
        let Ok(devices) = store.list_devices(user.id).await else {
            continue;
        };
        let mut wanted = HashSet::new();
        for device in devices.iter().filter(|d| d.revoked_at.is_none()) {
            let protocols = &device.settings.protocols;
            if protocols.contains(&DeviceProtocol::Imap)
                && let Some(archive) = device.settings.mail_archive
            {
                wanted.insert(archive);
            }
            if protocols.contains(&DeviceProtocol::Jmap) {
                match &device.settings.archives {
                    Some(list) => wanted.extend(list.iter().copied()),
                    None => {
                        for archive in &archives {
                            if access
                                .role_in(&user, archive.id)
                                .await
                                .ok()
                                .flatten()
                                .is_some()
                            {
                                wanted.insert(archive.id);
                            }
                        }
                    }
                }
            }
        }
        out.extend(wanted.into_iter().map(|a| (a, user.id)));
    }
    out.sort();
    out
}

/// Builds the served mailboxes of every archive a device reads, a thousand members at a time,
/// then waits for a change in one of them or a minute, until the process ends.
pub async fn build_ahead(
    view: Arc<ArchiveMailView>,
    store: Arc<dyn Store>,
    hub: Arc<ChangeHub>,
    switches: holtmail_access::WriteSwitches,
) {
    let access = Access::new(store.clone(), switches);
    loop {
        let targets = served(&store, &access).await;
        let mut listeners = Vec::new();
        for (archive, user) in &targets {
            let viewer = Viewer {
                user: *user,
                device: None,
                archive: *archive,
                scopes: [DeviceScope::Read].into_iter().collect(),
            };
            loop {
                match view.prepare(&viewer, 1_000).await {
                    Ok(progress) if progress.done() => break,
                    Ok(progress) => {
                        tracing::debug!(%archive, assigned = progress.assigned, total = progress.total, "building served mailboxes");
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        tracing::warn!(%archive, %error, "served mailboxes not built");
                        break;
                    }
                }
            }
            listeners.push(hub.subscribe(*archive, *user));
        }
        let any = async {
            if listeners.is_empty() {
                std::future::pending::<()>().await;
            }
            let waits = listeners
                .iter_mut()
                .map(|l| Box::pin(l.changed()))
                .collect::<Vec<_>>();
            futures::future::select_all(waits).await;
        };
        let _ = tokio::time::timeout(Duration::from_secs(60), any).await;
    }
}

#[cfg(test)]
mod tests {
    use holtmail_store::{InstanceRole, NewUser, ServedKey, Store};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    async fn user(store: &dyn Store, email: &str) -> UserId {
        store
            .create_user(NewUser {
                email: email.into(),
                name: email.into(),
                role: InstanceRole::Member,
                password_hash: None,
            })
            .await
            .unwrap()
            .id
    }

    async fn woken(listener: &mut HubListener) -> bool {
        tokio::time::timeout(Duration::from_millis(600), listener.changed())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn the_hub_wakes_only_the_archive_and_user_a_change_touches() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            SqliteStore::open(dir.path().join("db.sqlite"))
                .await
                .unwrap(),
        );
        let (one, two) = (
            store.create_archive("One").await.unwrap().id,
            store.create_archive("Two").await.unwrap().id,
        );
        let (ada, bob) = (
            user(&*store, "ada@example.com").await,
            user(&*store, "bob@example.com").await,
        );
        let hub = Arc::new(ChangeHub::default());
        let mut ada_one = hub.subscribe(one, ada);
        let mut bob_one = hub.subscribe(one, bob);
        let mut ada_two = hub.subscribe(two, ada);
        tokio::spawn(hub.clone().run(store.clone()));
        tokio::time::sleep(Duration::from_millis(100)).await;

        store
            .set_subscription(ada, one, "all", true, Timestamp::now())
            .await
            .unwrap();
        assert!(woken(&mut ada_one).await, "the user's own row");
        assert!(!woken(&mut bob_one).await, "not another user's");
        assert!(!woken(&mut ada_two).await, "not another archive");

        store
            .set_subscription(bob, one, "all", true, Timestamp::now())
            .await
            .unwrap();
        store
            .set_subscription(bob, one, "kept", true, Timestamp::now())
            .await
            .unwrap();
        let latest = store.latest_seq().await.unwrap();
        let mut wakes = 0;
        let caught_up = tokio::time::timeout(Duration::from_secs(4), async {
            while bob_one.changed().await.unwrap() != latest {
                wakes += 1;
            }
        })
        .await;
        assert!(caught_up.is_ok(), "the latest change arrives");
        assert!(wakes <= 1, "coalesced: at most one earlier wake under load");
        assert!(!woken(&mut ada_one).await);
        drop(ada_two);
        hub.fan_out(&[]);
        assert_eq!(
            hub.subscribers.lock().unwrap().len(),
            2,
            "dropped listeners go"
        );
    }

    #[tokio::test]
    async fn a_dropped_wait_keeps_its_change_for_the_next() {
        let hub = ChangeHub::default();
        let archive = ArchiveId::new();
        let user = UserId::new();
        let mut listener = hub.subscribe(archive, user);
        let row = |seq| ChangeLogEntry {
            seq,
            archive_id: archive,
            kind: holtmail_store::ChangeKind::Message,
            collection_id: None,
            item_id: None,
            href: None,
            item_change_id: None,
            message_id: None,
            folder_id: None,
            thread_id: None,
            outbox_id: None,
            draft_id: None,
            user_id: None,
            at: Timestamp::now(),
        };
        hub.fan_out(&[row(1)]);
        assert_eq!(listener.changed().await, Some(1));
        hub.fan_out(&[row(2)]);
        let cut = tokio::time::timeout(Duration::from_millis(20), listener.changed()).await;
        assert!(cut.is_err(), "the second wake waits for the throttle");
        let next = tokio::time::timeout(Duration::from_millis(600), listener.changed())
            .await
            .expect("the cut wait's change is not lost");
        assert_eq!(next, Some(2));
    }

    #[tokio::test]
    async fn pruning_drops_old_rows_and_marks_lagging_views_stale() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            SqliteStore::open(dir.path().join("db.sqlite"))
                .await
                .unwrap(),
        );
        let archive = store.create_archive("Old").await.unwrap().id;
        let ada = user(&*store, "ada@example.com").await;
        let now = Timestamp::now();
        let (none, _) = prune_change_log(&*store, now).await.unwrap();
        assert_eq!(none, 0, "nothing to prune");
        store
            .open_served_mailbox(archive, ServedKey::All, now)
            .await
            .unwrap();
        let built = store
            .build_served_mailbox(archive, ServedKey::All, 1_000, now)
            .await
            .unwrap();
        assert!(built.built);
        let old = Timestamp::from_millis(
            now.as_millis() - CHANGE_LOG_KEEP.as_millis() as i64 - 86_400_000,
        );
        for mailbox in ["all", "kept"] {
            store
                .set_subscription(ada, archive, mailbox, true, old)
                .await
                .unwrap();
        }
        let (through, served) = prune_change_log(&*store, now).await.unwrap();
        assert_eq!(through, store.latest_seq().await.unwrap());
        assert_eq!(store.pruned_through().await.unwrap(), through);
        assert!(
            store
                .changes_since(archive, None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(served.stale, 1, "the view behind the cut rescans");

        store
            .set_subscription(ada, archive, "drafts", true, now)
            .await
            .unwrap();
        let (again, _) = prune_change_log(&*store, now).await.unwrap();
        assert_eq!(again, through, "recent rows stay");
        assert_eq!(
            store.changes_since(archive, None, 100).await.unwrap().len(),
            1
        );
    }
}
