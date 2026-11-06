//! An in-memory [`Source`] for testing code that syncs, and the conformance suite for watches.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;

#[derive(Debug, Clone)]
struct Stored {
    flags: Vec<String>,
    labels: Option<Vec<String>>,
    modseq: u64,
    received_at: Option<Timestamp>,
    raw: Bytes,
}

#[derive(Debug, Clone)]
struct Folder {
    role: Option<FolderRole>,
    selectable: bool,
    label: bool,
    unordered: bool,
    uid_validity: u64,
    next_uid: u64,
    version: u64,
    messages: BTreeMap<u64, Stored>,
}

#[derive(Debug, Default)]
struct State {
    folders: BTreeMap<String, Folder>,
    fail_connect: Option<String>,
    fetches: Vec<Vec<u64>>,
    flag_reads: Vec<(RangeInclusive<u64>, Option<u64>)>,
    condstore: bool,
    modseq: u64,
    track_changes: bool,
    listings: usize,
    watchable: bool,
    watch_error: Option<String>,
    watch_sign_in: Option<String>,
    watches: usize,
    change_seq: u64,
    change_log: Vec<(u64, String)>,
}

/// A mail server in memory. Clones share the same server.
#[derive(Debug, Clone, Default)]
pub struct FakeSource {
    state: Arc<Mutex<State>>,
    notify: Arc<tokio::sync::Notify>,
}

impl FakeSource {
    /// An empty server.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a folder with `UIDVALIDITY` 1.
    pub fn add_folder(&self, path: &str, role: Option<FolderRole>) {
        self.state.lock().unwrap().folders.insert(
            path.to_owned(),
            Folder {
                role,
                selectable: true,
                label: false,
                unordered: false,
                uid_validity: 1,
                next_uid: 1,
                version: 0,
                messages: BTreeMap::new(),
            },
        );
    }

    /// Adds a parent folder that cannot hold messages.
    pub fn add_parent(&self, path: &str) {
        self.add_folder(path, None);
        self.state
            .lock()
            .unwrap()
            .folders
            .get_mut(path)
            .unwrap()
            .selectable = false;
    }

    /// Marks the folder as a label shown as a folder, as Gmail's are.
    pub fn mark_label(&self, path: &str) {
        self.state
            .lock()
            .unwrap()
            .folders
            .get_mut(path)
            .expect("folder exists")
            .label = true;
    }

    /// Makes the folder report UIDs that do not ascend with arrival, as JMAP sources do, so
    /// [`FakeSource::deliver_at`] may put a message below archived UIDs.
    pub fn mark_unordered(&self, path: &str) {
        self.state
            .lock()
            .unwrap()
            .folders
            .get_mut(path)
            .expect("folder exists")
            .unordered = true;
    }

    /// Delivers a message at the given UID, replacing what was there.
    pub fn deliver_at(&self, path: &str, uid: u64, raw: &[u8], flags: &[&str]) {
        let mut state = self.state.lock().unwrap();
        state.modseq += 1;
        let modseq = state.modseq;
        touch(&mut state, path);
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.version += 1;
        folder.next_uid = folder.next_uid.max(uid + 1);
        folder.messages.insert(
            uid,
            Stored {
                flags: strings(flags),
                labels: None,
                modseq,
                received_at: None,
                raw: Bytes::copy_from_slice(raw),
            },
        );
        drop(state);
        self.notify.notify_waiters();
    }

    /// Makes the server report `HIGHESTMODSEQ` and answer flag reads with `changed_since`.
    pub fn enable_condstore(&self) {
        self.state.lock().unwrap().condstore = true;
    }

    /// Delivers a message to the folder and returns its UID.
    pub fn deliver(&self, path: &str, raw: &[u8], flags: &[&str]) -> u64 {
        self.put(path, raw, flags, None)
    }

    /// Delivers a message the server says it received at `received_at` (IMAP `INTERNALDATE`) and
    /// returns its UID. Messages from the other methods have no received time.
    pub fn deliver_received(&self, path: &str, raw: &[u8], received_at: Timestamp) -> u64 {
        let uid = self.put(path, raw, &[], None);
        self.change(path, uid, |m| m.received_at = Some(received_at));
        uid
    }

    /// Delivers a message with labels, as Gmail's All Mail holds them, and returns its UID.
    pub fn deliver_labeled(&self, path: &str, raw: &[u8], flags: &[&str], labels: &[&str]) -> u64 {
        self.put(path, raw, flags, Some(labels))
    }

    fn put(&self, path: &str, raw: &[u8], flags: &[&str], labels: Option<&[&str]>) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.modseq += 1;
        let modseq = state.modseq;
        touch(&mut state, path);
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.version += 1;
        let uid = folder.next_uid;
        folder.next_uid += 1;
        folder.messages.insert(
            uid,
            Stored {
                flags: strings(flags),
                labels: labels.map(strings),
                modseq,
                received_at: None,
                raw: Bytes::copy_from_slice(raw),
            },
        );
        drop(state);
        self.notify.notify_waiters();
        uid
    }

    /// Replaces a message's flags, as another mail client would.
    pub fn set_flags(&self, path: &str, uid: u64, flags: &[&str]) {
        self.change(path, uid, |m| m.flags = strings(flags));
    }

    /// Replaces a message's labels.
    pub fn set_labels(&self, path: &str, uid: u64, labels: &[&str]) {
        self.change(path, uid, |m| m.labels = Some(strings(labels)));
    }

    fn change(&self, path: &str, uid: u64, edit: impl FnOnce(&mut Stored)) {
        let mut state = self.state.lock().unwrap();
        state.modseq += 1;
        let modseq = state.modseq;
        touch(&mut state, path);
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.version += 1;
        let message = folder.messages.get_mut(&uid).expect("message exists");
        edit(message);
        message.modseq = modseq;
        drop(state);
        self.notify.notify_waiters();
    }

    /// Deletes a message from the folder.
    pub fn expunge(&self, path: &str, uid: u64) {
        let mut state = self.state.lock().unwrap();
        touch(&mut state, path);
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.version += 1;
        folder.messages.remove(&uid);
        drop(state);
        self.notify.notify_waiters();
    }

    /// Changes `UIDVALIDITY` and renumbers every message, as servers do after a rebuild.
    pub fn renumber(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        touch(&mut state, path);
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.uid_validity += 1;
        folder.version += 1;
        let messages: Vec<_> = std::mem::take(&mut folder.messages).into_values().collect();
        folder.next_uid = 100 * folder.uid_validity;
        for message in messages {
            folder.messages.insert(folder.next_uid, message);
            folder.next_uid += 1;
        }
        drop(state);
        self.notify.notify_waiters();
    }

    /// Makes every `connect` fail with this sign-in error, or succeed again with `None`.
    pub fn reject_sign_in(&self, reason: Option<&str>) {
        self.state.lock().unwrap().fail_connect = reason.map(str::to_owned);
    }

    /// Makes sessions keep a state per folder that changes with every delivery, flag change and
    /// expunge, so [`SourceSession::unchanged_since`] answers as a Graph source does.
    pub fn track_changes(&self) {
        self.state.lock().unwrap().track_changes = true;
    }

    /// Makes [`Source::watch`] answer with a watch, as a server that can push does.
    pub fn enable_watch(&self) {
        self.state.lock().unwrap().watchable = true;
    }

    /// Ends every running watch with this connection error and makes new watches fail with it,
    /// or lets watches run again with `None`.
    pub fn break_watches(&self, reason: Option<&str>) {
        self.state.lock().unwrap().watch_error = reason.map(str::to_owned);
        self.notify.notify_waiters();
    }

    /// Makes new watches fail with this sign-in error, or succeed again with `None`.
    pub fn reject_watch_sign_in(&self, reason: Option<&str>) {
        self.state.lock().unwrap().watch_sign_in = reason.map(str::to_owned);
    }

    /// How many watches were started so far.
    pub fn watches(&self) -> usize {
        self.state.lock().unwrap().watches
    }

    /// How many times a folder's UIDs were listed so far.
    pub fn listings(&self) -> usize {
        self.state.lock().unwrap().listings
    }

    /// UIDs requested by each `fetch` call so far.
    pub fn fetches(&self) -> Vec<Vec<u64>> {
        self.state.lock().unwrap().fetches.clone()
    }

    /// The UID range and `changed_since` of each `flags` call so far.
    pub fn flag_reads(&self) -> Vec<(RangeInclusive<u64>, Option<u64>)> {
        self.state.lock().unwrap().flag_reads.clone()
    }
}

fn touch(state: &mut State, path: &str) {
    state.change_seq += 1;
    let seq = state.change_seq;
    state.change_log.push((seq, path.to_owned()));
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

#[async_trait]
impl Source for FakeSource {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError> {
        if let Some(reason) = &self.state.lock().unwrap().fail_connect {
            return Err(SourceError::Auth(reason.clone()));
        }
        Ok(Box::new(FakeSession {
            source: self.clone(),
            selected: None,
            listed: None,
        }))
    }

    async fn watch(&self, folders: &[String]) -> Result<Option<Box<dyn SourceWatch>>, SourceError> {
        let mut state = self.state.lock().unwrap();
        if !state.watchable {
            return Ok(None);
        }
        if let Some(reason) = state.fail_connect.clone().or(state.watch_sign_in.clone()) {
            return Err(SourceError::Auth(reason));
        }
        if let Some(reason) = &state.watch_error {
            return Err(SourceError::Connection(reason.clone()));
        }
        state.watches += 1;
        Ok(Some(Box::new(FakeWatch {
            source: self.clone(),
            folders: folders.to_vec(),
            seen: state.change_seq,
        })))
    }
}

struct FakeWatch {
    source: FakeSource,
    folders: Vec<String>,
    seen: u64,
}

#[async_trait]
impl SourceWatch for FakeWatch {
    async fn next(&mut self) -> Result<Changed, SourceError> {
        loop {
            let notify = self.source.notify.clone();
            let notified = notify.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.source.state.lock().unwrap();
                if let Some(reason) = &state.watch_error {
                    return Err(SourceError::Connection(reason.clone()));
                }
                let mut changed = Vec::new();
                for (seq, path) in &state.change_log {
                    if *seq > self.seen
                        && (self.folders.is_empty() || self.folders.contains(path))
                        && !changed.contains(path)
                    {
                        changed.push(path.clone());
                    }
                }
                self.seen = state.change_seq;
                if !changed.is_empty() {
                    return Ok(Changed {
                        folders: Some(changed),
                    });
                }
            }
            notified.await;
        }
    }

    async fn close(self: Box<Self>) {}
}

struct FakeSession {
    source: FakeSource,
    selected: Option<String>,
    listed: Option<u64>,
}

impl FakeSession {
    fn folder(&self) -> Result<Folder, SourceError> {
        let path = self
            .selected
            .as_ref()
            .ok_or_else(|| SourceError::Protocol("no folder selected".into()))?;
        self.source
            .state
            .lock()
            .unwrap()
            .folders
            .get(path)
            .cloned()
            .ok_or_else(|| SourceError::Protocol(format!("{path} does not exist")))
    }
}

#[async_trait]
impl SourceSession for FakeSession {
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError> {
        Ok(self
            .source
            .state
            .lock()
            .unwrap()
            .folders
            .iter()
            .map(|(path, folder)| RemoteFolder {
                path: path.clone(),
                role: folder.role,
                selectable: folder.selectable,
                label: folder.label,
            })
            .collect())
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError> {
        self.selected = Some(path.to_owned());
        self.listed = None;
        let folder = self.folder()?;
        if !folder.selectable {
            return Err(SourceError::Protocol(format!("{path} cannot be selected")));
        }
        let state = self.source.state.lock().unwrap();
        Ok(FolderStatus {
            uid_validity: folder.uid_validity,
            uid_next: Some(folder.next_uid),
            highest_modseq: state.condstore.then_some(state.modseq),
            exists: folder.messages.len() as u64,
            uids_ascend: !folder.unordered,
        })
    }

    async fn uids(&mut self) -> Result<Vec<u64>, SourceError> {
        let folder = self.folder()?;
        let mut state = self.source.state.lock().unwrap();
        state.listings += 1;
        self.listed = state.track_changes.then_some(folder.version);
        Ok(folder.messages.keys().copied().collect())
    }

    fn state(&self) -> Option<String> {
        self.listed.map(|version| version.to_string())
    }

    async fn unchanged_since(&mut self, state: &str) -> Result<Option<String>, SourceError> {
        let folder = self.folder()?;
        let track = self.source.state.lock().unwrap().track_changes;
        Ok((track && state == folder.version.to_string()).then(|| state.to_owned()))
    }

    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError> {
        let folder = self.folder()?;
        self.source
            .state
            .lock()
            .unwrap()
            .fetches
            .push(uids.to_vec());
        let mut out: Vec<_> = uids
            .iter()
            .filter_map(|uid| {
                folder.messages.get(uid).map(|m| RemoteMessage {
                    uid: *uid,
                    flags: m.flags.clone(),
                    labels: m.labels.clone(),
                    server_id: None,
                    received_at: m.received_at,
                    raw: m.raw.clone(),
                })
            })
            .collect();
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    async fn flags(
        &mut self,
        uids: RangeInclusive<u64>,
        changed_since: Option<u64>,
    ) -> Result<Vec<RemoteFlags>, SourceError> {
        let folder = self.folder()?;
        let mut state = self.source.state.lock().unwrap();
        if changed_since.is_some() && !state.condstore {
            return Err(SourceError::Protocol(
                "CHANGEDSINCE without CONDSTORE".into(),
            ));
        }
        state.flag_reads.push((uids.clone(), changed_since));
        Ok(folder
            .messages
            .range(uids)
            .filter(|(_, m)| changed_since.is_none_or(|since| m.modseq > since))
            .map(|(uid, m)| RemoteFlags {
                uid: *uid,
                flags: m.flags.clone(),
                labels: m.labels.clone(),
            })
            .collect())
    }

    async fn logout(&mut self) -> Result<(), SourceError> {
        Ok(())
    }
}

/// Changes a mail server behind a [`Source`], for [`watch_conformance`].
#[async_trait]
pub trait MailRemote: Send + Sync {
    /// Delivers a new message to the folder `path`, as another mail client or an MTA would.
    async fn deliver(&self, path: &str);
}

#[async_trait]
impl MailRemote for FakeSource {
    async fn deliver(&self, path: &str) {
        FakeSource::deliver(self, path, b"Subject: watched\r\n\r\nHello\r\n", &[]);
    }
}

/// Checks that the watch of `source` keeps the [`SourceWatch`] contract, changing the server
/// through `remote`. `folder` and `other` are two existing folders; only `folder` is watched.
/// Panics on the first violation. A source without push must answer `None` and skip it.
pub async fn watch_conformance(
    source: &dyn Source,
    remote: &dyn MailRemote,
    folder: &str,
    other: &str,
) {
    use std::time::Duration;
    const WAIT: Duration = Duration::from_secs(10);
    let covers = |changed: &Changed, path: &str| {
        changed
            .folders
            .as_ref()
            .is_none_or(|folders| folders.iter().any(|p| p == path))
    };
    let mut watch = source
        .watch(&[folder.to_owned()])
        .await
        .expect("watch starts")
        .expect("the source watches");

    remote.deliver(folder).await;
    let changed = tokio::time::timeout(WAIT, watch.next())
        .await
        .expect("a delivery wakes the watch")
        .expect("the watch keeps running");
    assert!(covers(&changed, folder), "the change names the folder");
    if let Some(folders) = &changed.folders {
        let mut unique = folders.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), folders.len(), "each folder once");
    }

    remote.deliver(folder).await;
    remote.deliver(folder).await;
    let changed = tokio::time::timeout(WAIT, watch.next())
        .await
        .expect("a later delivery wakes the watch again")
        .expect("the watch keeps running");
    assert!(covers(&changed, folder));

    remote.deliver(other).await;
    remote.deliver(folder).await;
    let changed = tokio::time::timeout(WAIT, async {
        loop {
            let changed = watch.next().await.expect("the watch keeps running");
            if covers(&changed, folder) {
                return changed;
            }
        }
    })
    .await
    .expect("a delivery after one to another folder is not swallowed");
    assert!(covers(&changed, folder));
    watch.close().await;
}
