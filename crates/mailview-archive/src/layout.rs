use std::collections::HashSet;

use holtmail_core::{AccountId, FolderId};
use holtmail_mailview::{Mailbox, MailboxId, MailboxRights};
use holtmail_store::{Capability, Folder, FolderRole, ServedKey};

use crate::{Ctx, same};

pub(crate) const UNIFIED: [(FolderRole, &str); 5] = [
    (FolderRole::Inbox, "INBOX"),
    (FolderRole::Sent, "Sent"),
    (FolderRole::Junk, "Junk"),
    (FolderRole::Trash, "Trash"),
    (FolderRole::Archive, "Archive"),
];

const RESERVED: [&str; 4] = ["INBOX", "Drafts", "All Mail", "Only in Holtmail"];

/// One mailbox of the archive's layout and what is behind it.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The mailbox.
    pub id: MailboxId,
    /// Its name as levels.
    pub path: Vec<String>,
    /// Its role.
    pub role: Option<FolderRole>,
    /// The served view behind it; `None` for a placeholder.
    pub key: Option<ServedKey>,
    /// The folders whose places count for flags; `None` for every place.
    pub folders: Option<Vec<FolderId>>,
    /// The role of the local places it shows.
    pub local_role: Option<FolderRole>,
    /// The accounts behind it.
    pub accounts: Vec<AccountId>,
}

fn level(name: &str) -> String {
    name.replace('/', "\u{2215}")
}

fn split(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|l| !l.is_empty())
        .map(level)
        .collect()
}

impl Ctx {
    /// Whether the folder is served as a mailbox of its own.
    pub(crate) fn served_folder(&self, folder: &Folder) -> bool {
        folder.role != Some(FolderRole::All)
            && (self.multi() || folder.role != Some(FolderRole::Drafts))
    }

    fn folder_path(&self, folder: &Folder) -> Vec<String> {
        if self.multi() {
            let mut path = vec![
                "Accounts".to_string(),
                level(&self.account_name(folder.account_id)),
            ];
            path.extend(split(&folder.path));
            return path;
        }
        if folder.role == Some(FolderRole::Inbox) {
            return vec!["INBOX".into()];
        }
        let mut path = split(&folder.path);
        if path.is_empty() {
            path.push(folder.id.to_string());
        }
        if path.len() == 1 && RESERVED.iter().any(|r| same(r, &path[0])) {
            path[0] = format!(
                "{} ({})",
                path[0],
                level(&self.account_name(folder.account_id))
            );
        }
        path
    }

    fn folder_key(&self, folder: &Folder) -> (ServedKey, Option<FolderRole>) {
        if !self.multi()
            && let Some(role) = folder.role
            && self.role_folders(role).len() == 1
        {
            return (ServedKey::Role(role), Some(role));
        }
        (ServedKey::Folder(folder.id), None)
    }

    fn accounts_of(&self, folders: &[FolderId]) -> Vec<AccountId> {
        let mut out: Vec<AccountId> = Vec::new();
        for folder in folders.iter().filter_map(|f| self.folder(*f)) {
            if !out.contains(&folder.account_id) {
                out.push(folder.account_id);
            }
        }
        out
    }

    /// The archive's mailboxes in tree order: parents before children.
    pub(crate) fn layout(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        if self.multi() {
            for (role, name) in UNIFIED {
                let folders = self.role_folders(role);
                if role != FolderRole::Inbox && folders.is_empty() {
                    continue;
                }
                entries.push(Entry {
                    id: MailboxId::Role(role),
                    path: vec![name.into()],
                    role: Some(role),
                    key: Some(ServedKey::Role(role)),
                    accounts: self.accounts_of(&folders),
                    folders: Some(folders),
                    local_role: Some(role),
                });
            }
        }
        let drafts = self.role_folders(FolderRole::Drafts);
        entries.push(Entry {
            id: MailboxId::Drafts,
            path: vec!["Drafts".into()],
            role: Some(FolderRole::Drafts),
            key: Some(ServedKey::Drafts(self.user())),
            accounts: self.accounts_of(&drafts),
            folders: Some(drafts),
            local_role: None,
        });
        entries.push(Entry {
            id: MailboxId::All,
            path: vec!["All Mail".into()],
            role: Some(FolderRole::All),
            key: Some(ServedKey::All),
            folders: None,
            local_role: None,
            accounts: self.accounts.iter().map(|a| a.account.id).collect(),
        });
        entries.push(Entry {
            id: MailboxId::Kept,
            path: vec!["Only in Holtmail".into()],
            role: None,
            key: Some(ServedKey::Kept),
            folders: None,
            local_role: None,
            accounts: Vec::new(),
        });
        for folder in self.folders.iter().filter(|f| self.served_folder(f)) {
            let (key, local_role) = self.folder_key(folder);
            entries.push(Entry {
                id: MailboxId::Folder(folder.id),
                path: self.folder_path(folder),
                role: if self.multi() { None } else { folder.role },
                key: Some(key),
                folders: Some(vec![folder.id]),
                local_role,
                accounts: vec![folder.account_id],
            });
        }
        let mut seen: HashSet<String> = HashSet::new();
        entries.retain(|e| seen.insert(e.path.join("/").to_lowercase()));
        let mut nodes = Vec::new();
        for entry in &entries {
            for len in 1..entry.path.len() {
                let joined = entry.path[..len].join("/");
                if seen.insert(joined.to_lowercase()) {
                    nodes.push(Entry {
                        id: MailboxId::Node(joined),
                        path: entry.path[..len].to_vec(),
                        role: None,
                        key: None,
                        folders: Some(Vec::new()),
                        local_role: None,
                        accounts: Vec::new(),
                    });
                }
            }
        }
        entries.extend(nodes);
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries
    }

    pub(crate) fn entry(&self, id: &MailboxId) -> Option<Entry> {
        self.layout().into_iter().find(|e| e.id == *id)
    }

    fn default_subscribed(&self, entry: &Entry) -> bool {
        match &entry.id {
            MailboxId::Role(_) | MailboxId::Drafts => true,
            MailboxId::Folder(f) => {
                !self.multi() || self.folder(*f).is_some_and(|f| f.role.is_none())
            }
            MailboxId::All | MailboxId::Kept | MailboxId::Node(_) => false,
        }
    }

    pub(crate) fn subscribed(&self, entry: &Entry) -> bool {
        self.subscriptions
            .get(&entry.id.key())
            .copied()
            .unwrap_or_else(|| self.default_subscribed(entry))
    }

    pub(crate) fn rights(&self, entry: &Entry) -> MailboxRights {
        let local = self.local;
        match &entry.id {
            MailboxId::Node(_) => MailboxRights::default(),
            MailboxId::Drafts => MailboxRights {
                read: true,
                add: true,
                remove: true,
                ..MailboxRights::default()
            },
            MailboxId::Kept => MailboxRights {
                read: true,
                set_seen: local,
                set_keywords: local,
                local_flags: local,
                ..MailboxRights::default()
            },
            id => {
                let open =
                    |c: Capability| entry.accounts.iter().any(|a| self.lock(*a, &[c]).is_none());
                let flags_open = open(Capability::MailFlags);
                let all_flags_open = entry
                    .accounts
                    .iter()
                    .all(|a| self.lock(*a, &[Capability::MailFlags]).is_none());
                let writes = *id != MailboxId::All;
                MailboxRights {
                    read: true,
                    add: writes && open(Capability::MailAppend),
                    remove: writes && open(Capability::MailDelete),
                    set_seen: flags_open || local,
                    set_keywords: flags_open || local,
                    local_flags: local && !all_flags_open,
                }
            }
        }
    }

    pub(crate) fn mailbox(&self, entry: &Entry) -> Mailbox {
        Mailbox {
            id: entry.id.clone(),
            path: entry.path.clone(),
            role: entry.role,
            selectable: entry.id.selectable(),
            subscribed: self.subscribed(entry),
            rights: self.rights(entry),
        }
    }

    /// The selectable mailboxes a message with places shown in `folders` is in, with the
    /// viewer's local places of `local_roles`, sorted by key.
    pub(crate) fn mailboxes_of(
        &self,
        folders: &[FolderId],
        local_roles: &[FolderRole],
        archived: bool,
    ) -> Vec<MailboxId> {
        let mut out = Vec::new();
        for entry in self.layout() {
            let member = match &entry.id {
                MailboxId::All => archived,
                MailboxId::Kept => archived && folders.is_empty(),
                MailboxId::Node(_) => false,
                _ => {
                    entry
                        .folders
                        .as_ref()
                        .is_some_and(|set| folders.iter().any(|f| set.contains(f)))
                        || entry.local_role.is_some_and(|r| local_roles.contains(&r))
                }
            };
            if member {
                out.push(entry.id);
            }
        }
        out.sort_by_key(MailboxId::key);
        out
    }
}
