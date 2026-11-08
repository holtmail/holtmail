//! An in-memory [`Source`] and [`MailboxWriter`] for testing code that syncs and writes, and the
//! conformance suites for watches and writers.

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
    write_caps: Option<MailboxWriteCaps>,
    write_error: Option<String>,
    writes: usize,
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

impl FakeSource {
    /// A [`MailboxWriter`] on this server. Its changes show in sessions and wake watches, as a
    /// real server's would.
    pub fn writer(&self) -> FakeWriter {
        FakeWriter {
            source: self.clone(),
        }
    }

    /// Makes writers report and keep to these capabilities; [`MailboxWriteCaps::full`] until
    /// called.
    pub fn set_write_caps(&self, caps: MailboxWriteCaps) {
        self.state.lock().unwrap().write_caps = Some(caps);
    }

    /// Makes every writer call fail with this connection error before changing anything, or
    /// succeed again with `None`.
    pub fn fail_writes(&self, reason: Option<&str>) {
        self.state.lock().unwrap().write_error = reason.map(str::to_owned);
    }

    /// How many writer calls changed or tried to change the server so far, failed ones included.
    pub fn writes(&self) -> usize {
        self.state.lock().unwrap().writes
    }
}

/// The [`MailboxWriter`] of a [`FakeSource`], from [`FakeSource::writer`].
#[derive(Debug, Clone)]
pub struct FakeWriter {
    source: FakeSource,
}

fn has(flags: &[String], name: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(name))
}

impl FakeWriter {
    fn write<T>(
        &self,
        touched: &[&str],
        edit: impl FnOnce(&mut State, &MailboxWriteCaps) -> Result<T, SourceError>,
    ) -> Result<T, SourceError> {
        let mut state = self.source.state.lock().unwrap();
        state.writes += 1;
        if let Some(reason) = &state.write_error {
            return Err(SourceError::Connection(reason.clone()));
        }
        let caps = state
            .write_caps
            .clone()
            .unwrap_or_else(MailboxWriteCaps::full);
        let out = edit(&mut state, &caps)?;
        for path in touched {
            touch(&mut state, path);
        }
        drop(state);
        self.source.notify.notify_waiters();
        Ok(out)
    }
}

fn check_flags(caps: &MailboxWriteCaps, flags: &[&[Flag]]) -> Result<(), SourceError> {
    match flags
        .iter()
        .flat_map(|f| f.iter())
        .find(|f| !caps.supports(f))
    {
        Some(flag) => Err(SourceError::Unsupported(format!(
            "flag {} is not supported",
            flag.as_imap()
        ))),
        None => Ok(()),
    }
}

fn missing(path: &str) -> SourceError {
    SourceError::Missing(format!("folder {path}"))
}

#[async_trait]
impl MailboxWriter for FakeWriter {
    fn capabilities(&self) -> MailboxWriteCaps {
        self.source
            .state
            .lock()
            .unwrap()
            .write_caps
            .clone()
            .unwrap_or_else(MailboxWriteCaps::full)
    }

    async fn set_flags(
        &self,
        folder: &str,
        uids: &[u64],
        add: &[Flag],
        remove: &[Flag],
        unchanged_since: Option<u64>,
    ) -> Result<FlagResult, SourceError> {
        self.write(&[folder], |state, caps| {
            check_flags(caps, &[add, remove])?;
            if unchanged_since.is_some() && !caps.conditional {
                return Err(SourceError::Unsupported("unchanged_since".into()));
            }
            if !state.folders.contains_key(folder) {
                return Err(missing(folder));
            }
            let mut result = FlagResult::default();
            let mut sorted = uids.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            for uid in sorted {
                let next = state.modseq + 1;
                let target = state.folders.get_mut(folder).unwrap();
                let Some(message) = target.messages.get_mut(&uid) else {
                    continue;
                };
                if unchanged_since.is_some_and(|since| message.modseq > since) {
                    result.modified.push(uid);
                    continue;
                }
                let mut flags = message.flags.clone();
                for flag in add {
                    if !has(&flags, flag.as_imap()) {
                        flags.push(flag.as_imap().to_owned());
                    }
                }
                flags.retain(|f| !remove.iter().any(|r| r.is(f)));
                if flags != message.flags {
                    message.flags = flags;
                    message.modseq = next;
                    target.version += 1;
                    state.modseq = next;
                }
                result.updated.push(uid);
            }
            Ok(result)
        })
    }

    async fn set_labels(
        &self,
        uids: &[u64],
        add: &[String],
        remove: &[String],
    ) -> Result<(), SourceError> {
        let all = self
            .source
            .state
            .lock()
            .unwrap()
            .folders
            .iter()
            .find(|(_, f)| f.role == Some(FolderRole::All))
            .map(|(path, _)| path.clone());
        let touched = all.clone().unwrap_or_default();
        self.write(&[&touched], |state, caps| {
            if !caps.labels {
                return Err(SourceError::Unsupported("labels".into()));
            }
            let path = all.ok_or_else(|| missing("with the All role"))?;
            for uid in uids {
                let next = state.modseq + 1;
                let folder = state.folders.get_mut(&path).unwrap();
                let Some(message) = folder.messages.get_mut(uid) else {
                    continue;
                };
                let mut labels = message.labels.clone().unwrap_or_default();
                for label in add {
                    if !labels.contains(label) {
                        labels.push(label.clone());
                    }
                }
                labels.retain(|l| !remove.contains(l));
                if message.labels.as_ref() != Some(&labels) {
                    message.labels = Some(labels);
                    message.modseq = next;
                    folder.version += 1;
                    state.modseq = next;
                }
            }
            Ok(())
        })
    }

    async fn move_to(
        &self,
        folder: &str,
        uids: &[u64],
        target: &str,
    ) -> Result<Moved, SourceError> {
        self.write(&[folder, target], |state, caps| {
            if !state.folders.contains_key(folder) {
                return Err(missing(folder));
            }
            if !state.folders.contains_key(target) {
                return Err(missing(target));
            }
            let mut sorted = uids.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            let mut moved = Moved::default();
            for uid in sorted {
                let Some(mut message) =
                    state.folders.get_mut(folder).unwrap().messages.remove(&uid)
                else {
                    continue;
                };
                state.modseq += 1;
                message.modseq = state.modseq;
                state.folders.get_mut(folder).unwrap().version += 1;
                let to = state.folders.get_mut(target).unwrap();
                let new_uid = to.next_uid;
                to.next_uid += 1;
                to.version += 1;
                to.messages.insert(new_uid, message);
                moved.uids.push((uid, caps.move_uids.then_some(new_uid)));
            }
            Ok(moved)
        })
    }

    async fn append(
        &self,
        folder: &str,
        raw: &[u8],
        flags: &[Flag],
        date: Timestamp,
    ) -> Result<Option<u64>, SourceError> {
        self.write(&[folder], |state, caps| {
            check_flags(caps, &[flags])?;
            state.modseq += 1;
            let modseq = state.modseq;
            let target = state
                .folders
                .get_mut(folder)
                .ok_or_else(|| missing(folder))?;
            let uid = target.next_uid;
            target.next_uid += 1;
            target.version += 1;
            target.messages.insert(
                uid,
                Stored {
                    flags: flags.iter().map(|f| f.as_imap().to_owned()).collect(),
                    labels: None,
                    modseq,
                    received_at: Some(date),
                    raw: Bytes::copy_from_slice(raw),
                },
            );
            Ok(caps.append_uid.then_some(uid))
        })
    }

    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError> {
        self.write(&[folder], |state, caps| {
            if !expunge && !caps.mark_deleted {
                return Err(SourceError::Unsupported("marking deleted".into()));
            }
            if !state.folders.contains_key(folder) {
                return Err(missing(folder));
            }
            for uid in uids {
                let next = state.modseq + 1;
                let target = state.folders.get_mut(folder).unwrap();
                if expunge {
                    if target.messages.remove(uid).is_some() {
                        target.version += 1;
                    }
                } else if let Some(message) = target.messages.get_mut(uid)
                    && !has(&message.flags, "\\Deleted")
                {
                    message.flags.push("\\Deleted".into());
                    message.modseq = next;
                    target.version += 1;
                    state.modseq = next;
                }
            }
            Ok(())
        })
    }
}

/// A message as a [`MailboxProbe`] sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// Its UID in the folder.
    pub uid: u64,
    /// Flags and keywords as IMAP spells them, `\Deleted` included.
    pub flags: Vec<String>,
    /// Labels, as in [`RemoteMessage::labels`].
    pub labels: Option<Vec<String>>,
    /// Its mod-sequence, where the server keeps them; required when the writer has
    /// [`MailboxWriteCaps::conditional`].
    pub modseq: Option<u64>,
    /// The complete message as stored on the server.
    pub raw: Bytes,
}

/// Reads and fills a mail server behind a [`MailboxWriter`], for [`writer_conformance`], without
/// going through the writer.
#[async_trait]
pub trait MailboxProbe: Send + Sync {
    /// Delivers `raw` to the existing folder `path` without flags, as an MTA would, and returns
    /// its UID.
    async fn deliver(&self, path: &str, raw: &[u8]) -> u64;

    /// Every message in the existing folder `path`, ascending by UID.
    async fn messages(&self, path: &str) -> Vec<Probed>;
}

#[async_trait]
impl MailboxProbe for FakeSource {
    async fn deliver(&self, path: &str, raw: &[u8]) -> u64 {
        FakeSource::deliver(self, path, raw, &[])
    }

    async fn messages(&self, path: &str) -> Vec<Probed> {
        let state = self.state.lock().unwrap();
        state.folders[path]
            .messages
            .iter()
            .map(|(uid, m)| Probed {
                uid: *uid,
                flags: m.flags.clone(),
                labels: m.labels.clone(),
                modseq: Some(m.modseq),
                raw: m.raw.clone(),
            })
            .collect()
    }
}

/// The folders [`writer_conformance`] works in.
#[derive(Debug, Clone, Copy)]
pub struct WriteFolders<'a> {
    /// An existing folder messages are delivered to and changed in.
    pub folder: &'a str,
    /// Another existing folder, the target of moves and appends.
    pub target: &'a str,
    /// The account's folder with the [`FolderRole::All`] role; needed when the writer has
    /// [`MailboxWriteCaps::labels`].
    pub all: Option<&'a str>,
}

/// Checks that `writer` keeps the [`MailboxWriter`] contract, filling and reading the server
/// through `probe`. The folders may hold other messages; every message the suite delivers or
/// appends has a `Message-ID` of its own on `example.com`. Panics on the first violation.
pub async fn writer_conformance(
    writer: &dyn MailboxWriter,
    probe: &dyn MailboxProbe,
    folders: WriteFolders<'_>,
) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static RUN: AtomicU64 = AtomicU64::new(0);
    let run = format!(
        "{}-{}",
        Timestamp::now().as_millis(),
        RUN.fetch_add(1, Ordering::Relaxed)
    );
    let mut count = 0;
    let tag = run.clone();
    let mut mail = move || {
        count += 1;
        format!(
            "From: Ada Example <ada@example.com>\r\nTo: Bo Example <bo@example.org>\r\n\
             Subject: Conformance {count}\r\nMessage-ID: <conformance-{tag}-{count}@example.com>\r\n\
             Date: Mon, 2 Nov 2026 10:00:00 +0000\r\n\r\nHello {count}\r\n"
        )
        .into_bytes()
    };
    let caps = writer.capabilities();
    assert!(!caps.flags.is_empty(), "a writer sets at least one flag");
    let first = caps.flags[0].clone();
    let one = std::slice::from_ref(&first);
    let second = caps.flags.get(1).cloned();
    let folder = folders.folder;
    let target = folders.target;
    let find = |messages: &[Probed], uid: u64| messages.iter().find(|m| m.uid == uid).cloned();
    let flags_of = async |path: &str, uid: u64| {
        find(&probe.messages(path).await, uid)
            .expect("the message is in the folder")
            .flags
    };
    let unknown = u64::from(u32::MAX) - 7;

    let a = probe.deliver(folder, &mail()).await;
    let b = probe.deliver(folder, &mail()).await;
    let mut uids = vec![a, b];
    uids.sort_unstable();
    let mut add = vec![first.clone()];
    add.extend(second.clone());
    let result = writer
        .set_flags(folder, &[b, unknown, a], &add, &[], None)
        .await
        .expect("flags are set");
    assert_eq!(
        result.updated, uids,
        "updated ascending, unknown UIDs left out"
    );
    assert!(result.modified.is_empty());
    for uid in [a, b] {
        let flags = flags_of(folder, uid).await;
        assert!(add.iter().all(|f| flags.iter().any(|g| f.is(g))));
    }
    let again = writer
        .set_flags(folder, &[a, b], &add, &[], None)
        .await
        .expect("setting again works");
    assert_eq!(again.updated, uids, "setting flags again is idempotent");
    let flags = flags_of(folder, a).await;
    assert_eq!(
        flags.iter().filter(|f| first.is(f)).count(),
        1,
        "a flag is never there twice"
    );
    writer
        .set_flags(folder, &[a], &[], one, None)
        .await
        .expect("flags are removed");
    let flags = flags_of(folder, a).await;
    assert!(!flags.iter().any(|f| first.is(f)), "the flag is gone");
    if let Some(second) = &second {
        assert!(flags.iter().any(|f| second.is(f)), "other flags stay");
    }
    writer
        .set_flags(folder, &[a], &[], one, None)
        .await
        .expect("removing a missing flag is harmless");
    assert_eq!(
        writer
            .set_flags(folder, &[], &add, &[], None)
            .await
            .unwrap(),
        FlagResult::default(),
        "no UIDs, nothing to do"
    );
    writer
        .set_flags(folder, &[b], one, one, None)
        .await
        .expect("a flag in both lists is accepted");
    assert!(
        !flags_of(folder, b).await.iter().any(|f| first.is(f)),
        "a flag in both lists ends up removed"
    );

    let keyword = Flag::Keyword("$Conformance".into());
    let before = flags_of(folder, a).await;
    match writer
        .set_flags(folder, &[a], std::slice::from_ref(&keyword), &[], None)
        .await
    {
        Ok(_) => {
            assert!(caps.keywords, "keywords work only with the capability");
            assert!(flags_of(folder, a).await.iter().any(|f| keyword.is(f)));
        }
        Err(SourceError::Unsupported(_)) => {
            assert!(
                !caps.keywords,
                "keywords are refused only without the capability"
            );
            assert_eq!(flags_of(folder, a).await, before, "nothing changed");
        }
        Err(other) => panic!("keyword: {other}"),
    }
    for flag in [Flag::Seen, Flag::Answered, Flag::Flagged, Flag::Draft] {
        if !caps.supports(&flag) {
            assert!(matches!(
                writer.set_flags(folder, &[a], &[flag], &[], None).await,
                Err(SourceError::Unsupported(_))
            ));
        }
    }

    let c = probe.deliver(folder, &mail()).await;
    if caps.conditional {
        let modseq = find(&probe.messages(folder).await, c)
            .unwrap()
            .modseq
            .expect("the probe reports mod-sequences for a conditional writer");
        let result = writer
            .set_flags(folder, &[c], one, &[], Some(modseq))
            .await
            .expect("an unchanged message is updated");
        assert_eq!(result.updated, vec![c]);
        let result = writer
            .set_flags(folder, &[c], &[], one, Some(modseq))
            .await
            .expect("a changed message is reported, not an error");
        assert_eq!(result.modified, vec![c], "changed since: left alone");
        assert!(result.updated.is_empty());
        assert!(
            flags_of(folder, c).await.iter().any(|f| first.is(f)),
            "the server's state wins"
        );
    } else {
        assert!(matches!(
            writer.set_flags(folder, &[c], one, &[], Some(1)).await,
            Err(SourceError::Unsupported(_))
        ));
        assert!(!flags_of(folder, c).await.iter().any(|f| first.is(f)));
    }

    if caps.labels {
        let all = folders
            .all
            .expect("a writer with labels needs the All folder");
        let d = probe.deliver(all, &mail()).await;
        let labels = ["Conformance/One".to_owned(), "Conformance Two".to_owned()];
        writer
            .set_labels(&[d, unknown], &labels, &[])
            .await
            .expect("labels are added");
        writer
            .set_labels(&[d], &labels, &[])
            .await
            .expect("adding labels again works");
        let probed = find(&probe.messages(all).await, d).unwrap();
        let got = probed.labels.expect("the message has labels");
        for label in &labels {
            assert_eq!(got.iter().filter(|l| *l == label).count(), 1);
        }
        writer
            .set_labels(&[d], &[], &labels[1..])
            .await
            .expect("labels are removed");
        let got = find(&probe.messages(all).await, d)
            .unwrap()
            .labels
            .unwrap_or_default();
        assert!(got.contains(&labels[0]));
        assert!(!got.contains(&labels[1]));
    } else {
        assert!(matches!(
            writer.set_labels(&[a], &["Conformance".into()], &[]).await,
            Err(SourceError::Unsupported(_))
        ));
    }

    let raw = mail();
    let e = probe.deliver(folder, &raw).await;
    writer
        .set_flags(folder, &[e], one, &[], None)
        .await
        .unwrap();
    let moved = writer
        .move_to(folder, &[e, unknown], target)
        .await
        .expect("the message moves");
    assert_eq!(moved.uids.len(), 1, "unknown UIDs are left out");
    assert_eq!(moved.uids[0].0, e);
    if caps.move_uids {
        assert!(moved.uids[0].1.is_some(), "the new UID is reported");
    }
    assert!(
        find(&probe.messages(folder).await, e).is_none(),
        "gone from the folder"
    );
    let landed = probe.messages(target).await;
    let copy = landed
        .iter()
        .find(|m| m.raw == raw)
        .expect("the bytes are in the target");
    if let Some(new_uid) = moved.uids[0].1 {
        assert_eq!(copy.uid, new_uid);
    }
    assert!(copy.flags.iter().any(|f| first.is(f)), "flags move along");
    let again = writer
        .move_to(folder, &[e], target)
        .await
        .expect("moving again is harmless");
    assert!(again.uids.is_empty());
    assert_eq!(
        probe
            .messages(target)
            .await
            .iter()
            .filter(|m| m.raw == raw)
            .count(),
        1
    );

    let raw = mail();
    let date = Timestamp::from_secs(1_793_000_000);
    let appended = writer
        .append(target, &raw, std::slice::from_ref(&first), date)
        .await
        .expect("the message is appended");
    if caps.append_uid {
        assert!(appended.is_some(), "the new UID is reported");
    }
    let landed = probe.messages(target).await;
    let copy = landed
        .iter()
        .find(|m| m.raw == raw)
        .expect("the exact bytes are in the folder");
    if let Some(uid) = appended {
        assert_eq!(copy.uid, uid);
    }
    assert!(copy.flags.iter().any(|f| first.is(f)), "with its flags");

    let f = probe.deliver(folder, &mail()).await;
    let g = probe.deliver(folder, &mail()).await;
    if caps.mark_deleted {
        writer
            .delete(folder, &[f], false)
            .await
            .expect("marked deleted");
        let probed = find(&probe.messages(folder).await, f).expect("still listed");
        assert!(
            probed
                .flags
                .iter()
                .any(|x| x.eq_ignore_ascii_case("\\Deleted"))
        );
    } else {
        assert!(matches!(
            writer.delete(folder, &[f], false).await,
            Err(SourceError::Unsupported(_))
        ));
        assert!(find(&probe.messages(folder).await, f).is_some());
    }
    writer
        .delete(folder, &[f, g, unknown], true)
        .await
        .expect("expunged");
    let left = probe.messages(folder).await;
    assert!(find(&left, f).is_none() && find(&left, g).is_none());
    writer
        .delete(folder, &[f, g], true)
        .await
        .expect("deleting again is harmless");

    let nowhere = format!("Conformance missing {run}");
    assert!(matches!(
        writer.set_flags(&nowhere, &[a], one, &[], None).await,
        Err(SourceError::Missing(_))
    ));
    assert!(matches!(
        writer.move_to(folder, &[a], &nowhere).await,
        Err(SourceError::Missing(_))
    ));
    assert!(
        find(&probe.messages(folder).await, a).is_some(),
        "nothing moved"
    );
    assert!(matches!(
        writer.append(&nowhere, &mail(), &[], date).await,
        Err(SourceError::Missing(_))
    ));
    assert!(matches!(
        writer.delete(&nowhere, &[a], true).await,
        Err(SourceError::Missing(_))
    ));
}
