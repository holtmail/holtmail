//! An in-memory [`Source`] for testing code that syncs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;

#[derive(Debug, Clone)]
struct Folder {
    role: Option<FolderRole>,
    selectable: bool,
    uid_validity: u64,
    next_uid: u64,
    messages: BTreeMap<u64, (Vec<String>, Bytes)>,
}

#[derive(Debug, Default)]
struct State {
    folders: BTreeMap<String, Folder>,
    fail_connect: Option<String>,
    fetches: Vec<Vec<u64>>,
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
                uid_validity: 1,
                next_uid: 1,
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

    /// Delivers a message to the folder and returns its UID.
    pub fn deliver(&self, path: &str, raw: &[u8], flags: &[&str]) -> u64 {
        let mut state = self.state.lock().unwrap();
        let folder = state.folders.get_mut(path).expect("folder exists");
        let uid = folder.next_uid;
        folder.next_uid += 1;
        folder.messages.insert(
            uid,
            (
                flags.iter().map(|f| f.to_string()).collect(),
                Bytes::copy_from_slice(raw),
            ),
        );
        uid
    }

    /// Deletes a message from the folder.
    pub fn expunge(&self, path: &str, uid: u64) {
        self.state
            .lock()
            .unwrap()
            .folders
            .get_mut(path)
            .expect("folder exists")
            .messages
            .remove(&uid);
    }

    /// Changes `UIDVALIDITY` and renumbers every message, as servers do after a rebuild.
    pub fn renumber(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        let folder = state.folders.get_mut(path).expect("folder exists");
        folder.uid_validity += 1;
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

    /// UIDs requested by each `fetch` call so far.
    pub fn fetches(&self) -> Vec<Vec<u64>> {
        self.state.lock().unwrap().fetches.clone()
    }
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
        }))
    }
}

struct FakeSession {
    source: FakeSource,
    selected: Option<String>,
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
            })
            .collect())
    }

    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError> {
        self.selected = Some(path.to_owned());
        let folder = self.folder()?;
        if !folder.selectable {
            return Err(SourceError::Protocol(format!("{path} cannot be selected")));
        }
        Ok(FolderStatus {
            uid_validity: folder.uid_validity,
            uid_next: Some(folder.next_uid),
            highest_modseq: None,
            exists: folder.messages.len() as u64,
        })
    }

    async fn uids(&mut self) -> Result<Vec<u64>, SourceError> {
        Ok(self.folder()?.messages.keys().copied().collect())
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
                folder.messages.get(uid).map(|(flags, raw)| RemoteMessage {
                    uid: *uid,
                    flags: flags.clone(),
                    received_at: None,
                    raw: raw.clone(),
                })
            })
            .collect();
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    async fn logout(&mut self) -> Result<(), SourceError> {
        Ok(())
    }
}
