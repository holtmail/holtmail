//! An in-memory [`Source`] for testing code that syncs.

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
}

/// A mail server in memory. Clones share the same server.
#[derive(Debug, Clone, Default)]
pub struct FakeSource {
    state: Arc<Mutex<State>>,
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
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.version += 1;
        let message = folder.messages.get_mut(&uid).expect("message exists");
        edit(message);
        message.modseq = modseq;
    }

    /// Deletes a message from the folder.
    pub fn expunge(&self, path: &str, uid: u64) {
        let mut state = self.state.lock().unwrap();
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.version += 1;
        folder.messages.remove(&uid);
    }

    /// Changes `UIDVALIDITY` and renumbers every message, as servers do after a rebuild.
    pub fn renumber(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.uid_validity += 1;
        folder.version += 1;
        let messages: Vec<_> = std::mem::take(&mut folder.messages).into_values().collect();
        folder.next_uid = 100 * folder.uid_validity;
        for message in messages {
            folder.messages.insert(folder.next_uid, message);
            folder.next_uid += 1;
        }
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
