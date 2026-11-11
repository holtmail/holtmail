use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, ArchiveId, DeviceId, DeviceScope, DraftId, FolderId, MessageId, ThreadId, Timestamp,
    UserId,
};
use holtmail_store::FolderRole;

use super::mime;
use super::{Driver, Harness, Scenario, Setup, World, Write};
use crate::{
    Appended, Changed, EmailRef, Envelope, Filter, FlagChange, FlagResult, Lock, MailActions,
    MailView, MailViewError, Mailbox, MailboxChanges, MailboxId, MailboxRights, MailboxStatus,
    Member, MemberFlags, NewEmail, Outcome, Part, PlaceResult, Refusal, Snapshot, TextField,
    ViewChanges, ViewState, Viewer,
};

/// The largest message the fake takes: 10 MiB.
pub const FAKE_APPEND_LIMIT: u64 = 10 * 1024 * 1024;

const UNIFIED: [FolderRole; 5] = [
    FolderRole::Inbox,
    FolderRole::Sent,
    FolderRole::Junk,
    FolderRole::Trash,
    FolderRole::Archive,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grant {
    Editor,
    Reader,
}

struct Account {
    id: AccountId,
    name: String,
    writes: BTreeSet<Write>,
}

struct Folder {
    id: FolderId,
    account: AccountId,
    path: String,
    role: Option<FolderRole>,
}

#[derive(Clone)]
enum Op {
    Flags(Vec<String>, Vec<String>),
    Move(FolderId),
    Delete(Option<FolderId>),
    Append,
}

#[derive(Clone)]
struct Place {
    folder: FolderId,
    flags: Vec<String>,
    live: bool,
    appended: bool,
    pending: Vec<Op>,
}

impl Place {
    fn effective(&self) -> (FolderId, Vec<String>, bool) {
        let (mut folder, mut flags, mut live) = (self.folder, self.flags.clone(), self.live);
        for op in &self.pending {
            match op {
                Op::Flags(add, remove) => apply_flags(&mut flags, add, remove),
                Op::Move(target) | Op::Delete(Some(target)) => folder = *target,
                Op::Delete(None) => live = false,
                Op::Append => live = true,
            }
        }
        (folder, flags, live)
    }
}

struct Msg {
    id: MessageId,
    raw: Bytes,
    blob: BlobId,
    received_at: Timestamp,
    thread: ThreadId,
    order: u64,
    places: Vec<Place>,
}

struct DraftVersion {
    raw: Bytes,
    blob: BlobId,
    saved_at: Timestamp,
    order: u64,
}

struct Draft {
    id: DraftId,
    user: UserId,
    versions: Vec<DraftVersion>,
    deleted: bool,
}

struct Entry {
    uid: u32,
    email: EmailRef,
    owner: Option<UserId>,
    added: u64,
    gone: Option<u64>,
}

struct Table {
    uidvalidity: u32,
    uidnext: u32,
    entries: Vec<Entry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ev {
    Created,
    Updated,
    Destroyed,
}

#[derive(Clone)]
enum What {
    Email(EmailRef),
    Mailbox(MailboxId),
    Counts,
    Thread(ThreadId),
}

struct Row {
    seq: u64,
    what: What,
    ev: Ev,
    user: Option<UserId>,
}

struct State {
    archive: ArchiveId,
    setup: Setup,
    grants: HashMap<UserId, Grant>,
    accounts: Vec<Account>,
    folders: Vec<Folder>,
    messages: Vec<Msg>,
    drafts: Vec<Draft>,
    local_flags: HashMap<(UserId, MessageId), Vec<(String, bool)>>,
    local_places: Vec<(UserId, FolderRole, MessageId)>,
    subscriptions: HashMap<(UserId, String), bool>,
    tables: HashMap<String, Table>,
    email_modseq: HashMap<EmailRef, u64>,
    rfc_threads: HashMap<String, ThreadId>,
    log: Vec<Row>,
    seq: u64,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn apply_flags(flags: &mut Vec<String>, add: &[String], remove: &[String]) {
    flags.retain(|f| !remove.iter().any(|r| same(r, f)));
    for a in add {
        if !flags.iter().any(|f| same(f, a)) {
            flags.push(a.clone());
        }
    }
}

fn normalize(mut flags: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    flags.retain(|f| seen.insert(f.to_ascii_lowercase()));
    flags.sort();
    flags
}

fn day(t: Timestamp) -> i64 {
    t.as_millis().div_euclid(86_400_000)
}

fn random_u32() -> u32 {
    (MessageId::new().as_uuid().as_u128() as u32) | 1
}

fn split(path: &str) -> Vec<String> {
    path.split('/').map(str::to_string).collect()
}

impl State {
    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn log(&mut self, what: What, ev: Ev, user: Option<UserId>) {
        let seq = self.seq;
        self.log.push(Row {
            seq,
            what,
            ev,
            user,
        });
    }

    fn touch(&mut self, email: EmailRef, user: Option<UserId>) {
        let seq = self.bump();
        self.email_modseq.insert(email, seq);
        self.log(What::Email(email), Ev::Updated, user);
        self.log(What::Counts, Ev::Updated, user);
    }

    fn check(&self, viewer: &Viewer) -> Result<Grant, MailViewError> {
        if viewer.archive != self.archive || !viewer.scopes.contains(&DeviceScope::Read) {
            return Err(MailViewError::NotFound("archive"));
        }
        self.grants
            .get(&viewer.user)
            .copied()
            .ok_or(MailViewError::NotFound("archive"))
    }

    fn multi(&self) -> bool {
        self.accounts.len() > 1
    }

    fn folder(&self, id: FolderId) -> &Folder {
        self.folders.iter().find(|f| f.id == id).expect("folder")
    }

    fn account(&self, id: AccountId) -> &Account {
        self.accounts.iter().find(|a| a.id == id).expect("account")
    }

    fn msg(&self, id: MessageId) -> Option<&Msg> {
        self.messages.iter().find(|m| m.id == id)
    }

    fn msg_mut(&mut self, id: MessageId) -> &mut Msg {
        self.messages
            .iter_mut()
            .find(|m| m.id == id)
            .expect("message")
    }

    fn served(&self, folder: &Folder) -> bool {
        folder.role != Some(FolderRole::All)
            && (self.multi() || folder.role != Some(FolderRole::Drafts))
    }

    fn role_folders(&self, role: FolderRole) -> Vec<FolderId> {
        self.folders
            .iter()
            .filter(|f| f.role == Some(role))
            .map(|f| f.id)
            .collect()
    }

    fn role_folder_in(&self, account: AccountId, role: FolderRole) -> Option<FolderId> {
        self.folders
            .iter()
            .find(|f| f.account == account && f.role == Some(role))
            .map(|f| f.id)
    }

    fn lock(&self, viewer: &Viewer, account: AccountId, writes: &[Write]) -> Option<Lock> {
        let account = self.account(account);
        let needs_delete = writes.contains(&Write::Expunge);
        if writes.iter().any(|w| !account.writes.contains(w))
            || (needs_delete && !account.writes.contains(&Write::Delete))
        {
            return Some(Lock::Capability);
        }
        self.viewer_lock(viewer)
    }

    /// The locks that belong to the viewer rather than an account: grant and scope.
    fn viewer_lock(&self, viewer: &Viewer) -> Option<Lock> {
        if self.grants.get(&viewer.user) != Some(&Grant::Editor) {
            return Some(Lock::Grant);
        }
        if !viewer.scopes.contains(&DeviceScope::WriteMail) {
            return Some(Lock::Scope);
        }
        None
    }

    fn exists(&self, id: &MailboxId) -> bool {
        match id {
            MailboxId::Role(role) => {
                self.multi()
                    && (*role == FolderRole::Inbox
                        || (UNIFIED.contains(role) && !self.role_folders(*role).is_empty()))
            }
            MailboxId::Folder(f) => self
                .folders
                .iter()
                .any(|folder| folder.id == *f && self.served(folder)),
            MailboxId::All | MailboxId::Kept | MailboxId::Drafts => true,
            MailboxId::Node(path) => self.nodes().contains(path),
        }
    }

    fn folder_path(&self, folder: &Folder) -> Vec<String> {
        if self.multi() {
            let mut path = vec!["Accounts".into(), self.account(folder.account).name.clone()];
            path.extend(split(&folder.path));
            return path;
        }
        if folder.role == Some(FolderRole::Inbox) {
            return vec!["INBOX".into()];
        }
        let mut path = split(&folder.path);
        let reserved = ["INBOX", "Drafts", "All Mail", "Only in Holtmail"];
        if path.len() == 1 && reserved.iter().any(|r| same(r, &path[0])) {
            path[0] = format!("{} ({})", path[0], self.account(folder.account).name);
        }
        path
    }

    fn top(&self) -> Vec<(Vec<String>, MailboxId, Option<FolderRole>)> {
        let mut out = Vec::new();
        if self.multi() {
            for role in UNIFIED {
                let id = MailboxId::Role(role);
                if self.exists(&id) {
                    let name = match role {
                        FolderRole::Inbox => "INBOX",
                        FolderRole::Sent => "Sent",
                        FolderRole::Junk => "Junk",
                        FolderRole::Trash => "Trash",
                        _ => "Archive",
                    };
                    out.push((vec![name.to_string()], id, Some(role)));
                }
            }
        }
        out.push((
            vec!["Drafts".into()],
            MailboxId::Drafts,
            Some(FolderRole::Drafts),
        ));
        out.push((
            vec!["All Mail".into()],
            MailboxId::All,
            Some(FolderRole::All),
        ));
        out.push((vec!["Only in Holtmail".into()], MailboxId::Kept, None));
        for folder in self.folders.iter().filter(|f| self.served(f)) {
            let role = if self.multi() { None } else { folder.role };
            out.push((self.folder_path(folder), MailboxId::Folder(folder.id), role));
        }
        out
    }

    fn nodes(&self) -> Vec<String> {
        let top = self.top();
        let paths: HashSet<String> = top.iter().map(|(p, _, _)| p.join("/")).collect();
        let mut nodes = Vec::new();
        for (path, _, _) in &top {
            for len in 1..path.len() {
                let joined = path[..len].join("/");
                if !paths.contains(&joined) && !nodes.contains(&joined) {
                    nodes.push(joined);
                }
            }
        }
        nodes
    }

    fn default_subscribed(&self, id: &MailboxId) -> bool {
        match id {
            MailboxId::Role(_) | MailboxId::Drafts => true,
            MailboxId::Folder(f) => !self.multi() || self.folder(*f).role.is_none(),
            MailboxId::All | MailboxId::Kept | MailboxId::Node(_) => false,
        }
    }

    fn subscribed(&self, user: UserId, id: &MailboxId) -> bool {
        self.subscriptions
            .get(&(user, id.key()))
            .copied()
            .unwrap_or_else(|| self.default_subscribed(id))
    }

    fn accounts_behind(&self, id: &MailboxId) -> Vec<AccountId> {
        let folders = match id {
            MailboxId::Folder(f) => vec![*f],
            MailboxId::Role(role) => self.role_folders(*role),
            MailboxId::All => self.folders.iter().map(|f| f.id).collect(),
            _ => Vec::new(),
        };
        let mut out: Vec<AccountId> = folders.iter().map(|f| self.folder(*f).account).collect();
        out.dedup();
        out
    }

    fn rights(&self, viewer: &Viewer, id: &MailboxId) -> MailboxRights {
        let local = self.setup.local_flags;
        match id {
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
            _ => {
                let accounts = self.accounts_behind(id);
                let open =
                    |w: &[Write]| accounts.iter().any(|a| self.lock(viewer, *a, w).is_none());
                let flags_open = open(&[Write::Flags]);
                let all_flags_open = accounts
                    .iter()
                    .all(|a| self.lock(viewer, *a, &[Write::Flags]).is_none());
                let writes = *id != MailboxId::All;
                MailboxRights {
                    read: true,
                    add: writes && open(&[Write::Append]),
                    remove: writes && open(&[Write::Delete]),
                    set_seen: flags_open || local,
                    set_keywords: flags_open || local,
                    local_flags: local && !all_flags_open,
                }
            }
        }
    }

    fn mailbox(
        &self,
        viewer: &Viewer,
        path: Vec<String>,
        id: MailboxId,
        role: Option<FolderRole>,
    ) -> Mailbox {
        Mailbox {
            selectable: id.selectable(),
            subscribed: self.subscribed(viewer.user, &id),
            rights: self.rights(viewer, &id),
            id,
            path,
            role,
        }
    }

    fn tree(&self, viewer: &Viewer) -> Vec<Mailbox> {
        let mut all: Vec<Mailbox> = self
            .top()
            .into_iter()
            .map(|(path, id, role)| self.mailbox(viewer, path, id, role))
            .collect();
        for node in self.nodes() {
            all.push(self.mailbox(viewer, split(&node), MailboxId::Node(node), None));
        }
        all.sort_by(|a, b| a.path.cmp(&b.path));
        all
    }

    fn find_mailbox(&self, viewer: &Viewer, id: &MailboxId) -> Result<Mailbox, MailViewError> {
        self.tree(viewer)
            .into_iter()
            .find(|m| m.id == *id)
            .ok_or(MailViewError::NotFound("mailbox"))
    }

    fn folder_set(&self, id: &MailboxId) -> Option<Vec<FolderId>> {
        match id {
            MailboxId::Folder(f) => Some(vec![*f]),
            MailboxId::Role(role) => Some(self.role_folders(*role)),
            MailboxId::Drafts => Some(self.role_folders(FolderRole::Drafts)),
            MailboxId::All | MailboxId::Kept => None,
            MailboxId::Node(_) => Some(Vec::new()),
        }
    }

    fn live_places(&self, msg: &Msg, folders: Option<&[FolderId]>) -> Vec<usize> {
        msg.places
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                let (folder, _, live) = p.effective();
                live && folders.is_none_or(|set| set.contains(&folder))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn local_role(&self, id: &MailboxId) -> Option<FolderRole> {
        match id {
            MailboxId::Role(role) => Some(*role),
            MailboxId::Folder(f) if !self.multi() => self.folder(*f).role,
            _ => None,
        }
    }

    fn members_now(&self, user: UserId, id: &MailboxId) -> (Vec<EmailRef>, Vec<EmailRef>) {
        let mut shared: Vec<(u64, EmailRef)> = Vec::new();
        let folders = self.folder_set(id);
        for msg in &self.messages {
            let email = EmailRef::Message(msg.id);
            let live = !self.live_places(msg, None).is_empty();
            let member = match id {
                MailboxId::All => true,
                MailboxId::Kept => !live,
                _ => !self.live_places(msg, folders.as_deref()).is_empty(),
            };
            if member {
                shared.push((msg.order, email));
            }
        }
        let mut personal: Vec<(u64, EmailRef)> = Vec::new();
        if *id == MailboxId::Drafts {
            for draft in self.drafts.iter().filter(|d| d.user == user && !d.deleted) {
                let version = draft.versions.last().expect("version");
                personal.push((
                    version.order,
                    EmailRef::Draft {
                        draft: draft.id,
                        version: draft.versions.len() as u32,
                    },
                ));
            }
        }
        if let Some(role) = self.local_role(id) {
            for (u, r, m) in &self.local_places {
                let email = EmailRef::Message(*m);
                if *u == user && *r == role && !shared.iter().any(|(_, e)| *e == email) {
                    personal.push((self.msg(*m).expect("message").order, email));
                }
            }
        }
        shared.sort();
        personal.sort();
        (
            shared.into_iter().map(|(_, e)| e).collect(),
            personal.into_iter().map(|(_, e)| e).collect(),
        )
    }

    fn table_key(user: UserId, id: &MailboxId) -> String {
        match id {
            MailboxId::Drafts => format!("drafts:{user}"),
            _ => id.key(),
        }
    }

    fn catch_up(&mut self, user: UserId, id: &MailboxId) -> Vec<Member> {
        let (shared, personal) = self.members_now(user, id);
        let key = Self::table_key(user, id);
        let seq = self.seq;
        let table = self.tables.entry(key).or_insert_with(|| Table {
            uidvalidity: random_u32(),
            uidnext: 1,
            entries: Vec::new(),
        });
        for entry in table.entries.iter_mut().filter(|e| e.gone.is_none()) {
            let keep = match entry.owner {
                None => shared.contains(&entry.email),
                Some(owner) if owner == user => personal.contains(&entry.email),
                Some(_) => true,
            };
            if !keep {
                entry.gone = Some(seq);
            }
        }
        let wanted = shared
            .iter()
            .map(|e| (*e, None))
            .chain(personal.iter().map(|e| (*e, Some(user))));
        for (email, owner) in wanted {
            let present = table
                .entries
                .iter()
                .any(|e| e.email == email && e.owner == owner && e.gone.is_none());
            if !present {
                table.entries.push(Entry {
                    uid: table.uidnext,
                    email,
                    owner,
                    added: seq,
                    gone: None,
                });
                table.uidnext += 1;
            }
        }
        let table = &self.tables[&Self::table_key(user, id)];
        table
            .entries
            .iter()
            .filter(|e| e.gone.is_none() && e.owner.is_none_or(|o| o == user))
            .map(|e| Member {
                uid: e.uid,
                email: e.email,
                modseq: e
                    .added
                    .max(self.email_modseq.get(&e.email).copied().unwrap_or(0)),
            })
            .collect()
    }

    fn effective_flags(
        &self,
        user: UserId,
        folders: Option<&[FolderId]>,
        email: &EmailRef,
    ) -> Option<Vec<String>> {
        let EmailRef::Message(id) = email else {
            let EmailRef::Draft { draft, version } = email else {
                unreachable!()
            };
            let known = self
                .drafts
                .iter()
                .any(|d| d.id == *draft && !d.deleted && d.versions.len() as u32 == *version);
            return known.then(|| vec!["\\Draft".to_string(), "\\Seen".to_string()]);
        };
        let msg = self.msg(*id)?;
        let mut flags = Vec::new();
        let any_live = !self.live_places(msg, None).is_empty();
        for place in &msg.places {
            let (folder, place_flags, live) = place.effective();
            let counts = match folders {
                Some(set) => live && set.contains(&folder),
                None => live || (!any_live && !place.appended),
            };
            if counts {
                apply_flags(&mut flags, &place_flags, &[]);
            }
        }
        if self.setup.local_flags
            && let Some(local) = self.local_flags.get(&(user, *id))
        {
            for (flag, present) in local {
                if *present {
                    apply_flags(&mut flags, std::slice::from_ref(flag), &[]);
                } else {
                    apply_flags(&mut flags, &[], std::slice::from_ref(flag));
                }
            }
        }
        Some(normalize(flags))
    }

    fn status(&mut self, user: UserId, id: &MailboxId) -> MailboxStatus {
        let members = self.catch_up(user, id);
        let folders = self.folder_set(id);
        let unseen = members
            .iter()
            .filter(|m| {
                !self
                    .effective_flags(user, folders.as_deref(), &m.email)
                    .unwrap_or_default()
                    .iter()
                    .any(|f| same(f, "\\Seen"))
            })
            .count() as u32;
        let table = &self.tables[&Self::table_key(user, id)];
        let highest = table
            .entries
            .iter()
            .filter(|e| e.owner.is_none_or(|o| o == user))
            .map(|e| {
                e.gone
                    .unwrap_or(0)
                    .max(e.added)
                    .max(self.email_modseq.get(&e.email).copied().unwrap_or(0))
            })
            .max()
            .unwrap_or(0)
            .max(1);
        MailboxStatus {
            messages: members.len() as u32,
            unseen,
            uidvalidity: table.uidvalidity,
            uidnext: table.uidnext,
            highest_modseq: highest,
        }
    }

    fn selectable(&self, viewer: &Viewer, id: &MailboxId) -> Result<(), MailViewError> {
        if !id.selectable() {
            return Err(MailViewError::NotFound("mailbox"));
        }
        self.find_mailbox(viewer, id).map(|_| ())
    }

    fn raw(&self, user: UserId, email: &EmailRef) -> Option<(Bytes, BlobId, Timestamp)> {
        match email {
            EmailRef::Message(id) => self
                .msg(*id)
                .map(|m| (m.raw.clone(), m.blob, m.received_at)),
            EmailRef::Draft { draft, version } => self
                .drafts
                .iter()
                .find(|d| d.id == *draft && d.user == user && !d.deleted)
                .filter(|d| d.versions.len() as u32 == *version)
                .map(|d| {
                    let v = d.versions.last().expect("version");
                    (v.raw.clone(), v.blob, v.saved_at)
                }),
        }
    }

    fn mailboxes_of(&mut self, user: UserId, email: &EmailRef) -> Vec<MailboxId> {
        let ids: Vec<MailboxId> = self.top().into_iter().map(|(_, id, _)| id).collect();
        let mut out: Vec<MailboxId> = ids
            .into_iter()
            .filter(|id| {
                let (shared, personal) = self.members_now(user, id);
                shared.contains(email) || personal.contains(email)
            })
            .collect();
        out.sort_by_key(MailboxId::key);
        out
    }

    fn envelope(&mut self, user: UserId, email: &EmailRef) -> Option<Envelope> {
        let (raw, blob, received_at) = self.raw(user, email)?;
        let fields = mime::top_fields(&raw);
        let part = mime::structure(&raw);
        let thread = email.message().and_then(|m| self.msg(m)).map(|m| m.thread);
        let bare_lf = raw
            .iter()
            .enumerate()
            .filter(|(i, b)| **b == b'\n' && (*i == 0 || raw[i - 1] != b'\r'))
            .count() as u64;
        let preview = mime::first_text(&raw, &part)
            .map(|t| {
                t.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or_default()
                    .chars()
                    .take(80)
                    .collect()
            })
            .unwrap_or_default();
        Some(Envelope {
            email: *email,
            thread,
            blob,
            size: raw.len() as u64,
            crlf_size: raw.len() as u64 + bare_lf,
            rfc_message_id: mime::angle_id(mime::get(&fields, "message-id")),
            in_reply_to: mime::angle_id(mime::get(&fields, "in-reply-to")),
            subject: mime::get(&fields, "subject").map(str::to_string),
            from: mime::addresses(mime::get(&fields, "from")),
            sender: mime::addresses(mime::get(&fields, "sender")),
            reply_to: mime::addresses(mime::get(&fields, "reply-to")),
            to: mime::addresses(mime::get(&fields, "to")),
            cc: mime::addresses(mime::get(&fields, "cc")),
            bcc: mime::addresses(mime::get(&fields, "bcc")),
            sent_at: mime::date(mime::get(&fields, "date")),
            received_at,
            preview,
            has_attachment: mime::has_attachment(&part),
            mailboxes: self.mailboxes_of(user, email),
        })
    }

    fn text_matches(&self, raw: &[u8], field: &TextField, value: &str) -> bool {
        let fields = mime::top_fields(raw);
        let joined = |name: &str| {
            fields
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let text = match field {
            TextField::Any => String::from_utf8_lossy(raw).into_owned(),
            TextField::Body => {
                let end = mime::header_end(raw, 0, raw.len());
                String::from_utf8_lossy(&raw[end..]).into_owned()
            }
            TextField::Subject => joined("subject"),
            TextField::From => joined("from"),
            TextField::To => joined("to"),
            TextField::Cc => joined("cc"),
            TextField::Bcc => joined("bcc"),
            TextField::Header(name) => {
                let name = name.to_ascii_lowercase();
                if value.trim().is_empty() {
                    return fields.iter().any(|(n, _)| *n == name);
                }
                joined(&name)
            }
        };
        let have = mime::words(&text);
        mime::words(value)
            .iter()
            .all(|w| have.iter().any(|h| h.starts_with(w.as_str())))
    }

    fn matches(&self, user: UserId, id: &MailboxId, member: &Member, filter: &Filter) -> bool {
        let Some((raw, _, received_at)) = self.raw(user, &member.email) else {
            return false;
        };
        let sent_at = mime::date(mime::get(&mime::top_fields(&raw), "date"));
        match filter {
            Filter::All => true,
            Filter::And(all) => all.iter().all(|f| self.matches(user, id, member, f)),
            Filter::Or(any) => any.iter().any(|f| self.matches(user, id, member, f)),
            Filter::Not(inner) => !self.matches(user, id, member, inner),
            Filter::Text(field, value) => self.text_matches(&raw, field, value),
            Filter::Flag(flag) => self
                .effective_flags(user, self.folder_set(id).as_deref(), &member.email)
                .unwrap_or_default()
                .iter()
                .any(|f| same(f, flag)),
            Filter::ReceivedBefore(t) => day(received_at) < day(*t),
            Filter::ReceivedSince(t) => day(received_at) >= day(*t),
            Filter::SentBefore(t) => sent_at.is_some_and(|s| day(s) < day(*t)),
            Filter::SentSince(t) => sent_at.is_some_and(|s| day(s) >= day(*t)),
            Filter::Larger(n) => raw.len() as u64 > *n,
            Filter::Smaller(n) => (raw.len() as u64) < *n,
            Filter::Uids(ranges) => ranges
                .iter()
                .any(|(a, b)| (*a.min(b)..=*a.max(b)).contains(&member.uid)),
            Filter::ModseqSince(m) => member.modseq >= *m,
            Filter::HasAttachment => mime::has_attachment(&mime::structure(&raw)),
        }
    }

    fn archive_bytes(&mut self, raw: Bytes, received_at: Timestamp) -> MessageId {
        let blob = BlobId::of(&raw);
        if let Some(msg) = self.messages.iter().find(|m| m.blob == blob) {
            return msg.id;
        }
        let fields = mime::top_fields(&raw);
        let seq = self.bump();
        let parent = mime::angle_id(mime::get(&fields, "in-reply-to"))
            .and_then(|p| self.rfc_threads.get(&p).copied());
        let thread = parent.unwrap_or_else(ThreadId::new);
        if let Some(own) = mime::angle_id(mime::get(&fields, "message-id")) {
            self.rfc_threads.entry(own).or_insert(thread);
        }
        let id = MessageId::new();
        self.messages.push(Msg {
            id,
            raw,
            blob,
            received_at,
            thread,
            order: seq,
            places: Vec::new(),
        });
        let email = EmailRef::Message(id);
        self.email_modseq.insert(email, seq);
        self.log(What::Email(email), Ev::Created, None);
        self.log(What::Counts, Ev::Updated, None);
        let ev = if parent.is_some() {
            Ev::Updated
        } else {
            Ev::Created
        };
        self.log(What::Thread(thread), ev, None);
        id
    }

    fn member_of(&mut self, user: UserId, id: &MailboxId, email: &EmailRef) -> Option<Member> {
        self.catch_up(user, id)
            .into_iter()
            .find(|m| m.email == *email)
    }

    fn uid_in(&mut self, user: UserId, id: &MailboxId, email: &EmailRef) -> Option<u32> {
        self.member_of(user, id, email).map(|m| m.uid)
    }

    fn delete_target(&self, place: &Place, account: AccountId) -> (Option<FolderId>, Vec<Write>) {
        let (folder, _, _) = place.effective();
        match self.role_folder_in(account, FolderRole::Trash) {
            Some(trash) if trash != folder => (Some(trash), vec![Write::Delete]),
            _ => (None, vec![Write::Delete, Write::Expunge]),
        }
    }

    fn settle(&mut self, applied: bool) {
        let mut touched = Vec::new();
        for msg in &mut self.messages {
            let before = msg.places.len();
            let mut changed = false;
            for place in &mut msg.places {
                if place.pending.is_empty() {
                    continue;
                }
                changed = true;
                if applied {
                    let (folder, flags, live) = place.effective();
                    place.folder = folder;
                    place.flags = flags;
                    place.live = live;
                    place.appended = false;
                }
                place.pending.clear();
            }
            msg.places.retain(|p| !p.appended);
            if changed || msg.places.len() != before {
                touched.push(EmailRef::Message(msg.id));
            }
        }
        for email in touched {
            self.touch(email, None);
        }
    }
}

/// The whole [`MailView`] and [`MailActions`] contract in memory, over a [`Scenario`], with
/// the [`Driver`] of the conformance suite. For tests of protocol front ends.
pub struct FakeMailView {
    state: Mutex<State>,
}

/// A [`Harness`] over [`FakeMailView`].
pub struct FakeHarness;

#[async_trait]
impl Harness for FakeHarness {
    async fn start(&self, scenario: &Scenario, setup: Setup) -> World {
        FakeMailView::world(scenario, setup)
    }
}

impl FakeMailView {
    /// A fresh fake holding `scenario`, and the world the suite reads it through.
    pub fn world(scenario: &Scenario, setup: Setup) -> World {
        let archive = ArchiveId::new();
        let (owner, member) = (UserId::new(), UserId::new());
        let mut state = State {
            archive,
            setup,
            grants: [(owner, Grant::Editor), (member, Grant::Reader)].into(),
            accounts: Vec::new(),
            folders: Vec::new(),
            messages: Vec::new(),
            drafts: Vec::new(),
            local_flags: HashMap::new(),
            local_places: Vec::new(),
            subscriptions: HashMap::new(),
            tables: HashMap::new(),
            email_modseq: HashMap::new(),
            rfc_threads: HashMap::new(),
            log: Vec::new(),
            seq: 0,
        };
        let mut folders = HashMap::new();
        for account in &scenario.accounts {
            let id = AccountId::new();
            state.accounts.push(Account {
                id,
                name: account.name.into(),
                writes: account.writes.iter().copied().collect(),
            });
            for (path, role) in account.folders {
                let folder = FolderId::new();
                state.folders.push(Folder {
                    id: folder,
                    account: id,
                    path: (*path).into(),
                    role: *role,
                });
                folders.insert((account.name.to_string(), path.to_string()), folder);
                state.bump();
                state.log(What::Mailbox(MailboxId::Folder(folder)), Ev::Created, None);
            }
        }
        let mut messages = HashMap::new();
        for message in &scenario.messages {
            let id = state.archive_bytes(Bytes::from(message.raw.clone()), message.received_at);
            for place in &message.places {
                let folder = folders[&(place.account.to_string(), place.folder.to_string())];
                state.msg_mut(id).places.push(Place {
                    folder,
                    flags: place.flags.iter().map(|f| f.to_string()).collect(),
                    live: place.live,
                    appended: false,
                    pending: Vec::new(),
                });
            }
            messages.insert(message.name.to_string(), id);
        }
        let fake = Arc::new(FakeMailView {
            state: Mutex::new(state),
        });
        let viewer = |user, scopes: &[DeviceScope]| Viewer {
            user,
            device: Some(DeviceId::new()),
            archive,
            scopes: scopes.iter().copied().collect(),
        };
        World {
            view: fake.clone(),
            actions: fake.clone(),
            driver: fake,
            editor: viewer(owner, &[DeviceScope::Read, DeviceScope::WriteMail]),
            reader: viewer(owner, &[DeviceScope::Read]),
            member: viewer(member, &[DeviceScope::Read, DeviceScope::WriteMail]),
            stranger: viewer(UserId::new(), &DeviceScope::ALL),
            folders,
            messages,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("fake state")
    }
}

fn dedup(emails: &[EmailRef]) -> Vec<EmailRef> {
    let mut seen = HashSet::new();
    emails.iter().copied().filter(|e| seen.insert(*e)).collect()
}

#[async_trait]
impl MailView for FakeMailView {
    async fn tree(&self, viewer: &Viewer) -> Result<Vec<Mailbox>, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        Ok(s.tree(viewer))
    }

    async fn status(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
    ) -> Result<MailboxStatus, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.selectable(viewer, mailbox)?;
        Ok(s.status(viewer.user, mailbox))
    }

    async fn select(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
    ) -> Result<Snapshot, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.selectable(viewer, mailbox)?;
        let members = s.catch_up(viewer.user, mailbox);
        let status = s.status(viewer.user, mailbox);
        Ok(Snapshot {
            mailbox: s.find_mailbox(viewer, mailbox)?,
            status,
            members,
        })
    }

    async fn mailbox_changes(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        since: u64,
    ) -> Result<MailboxChanges, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.selectable(viewer, mailbox)?;
        let members = s.catch_up(viewer.user, mailbox);
        let status = s.status(viewer.user, mailbox);
        let folders = s.folder_set(mailbox);
        let changed = members
            .into_iter()
            .filter(|m| m.modseq > since)
            .map(|m| MemberFlags {
                flags: s
                    .effective_flags(viewer.user, folders.as_deref(), &m.email)
                    .unwrap_or_default(),
                member: m,
            })
            .collect();
        let table = &s.tables[&State::table_key(viewer.user, mailbox)];
        let mut vanished: Vec<u32> = table
            .entries
            .iter()
            .filter(|e| e.gone.is_some_and(|g| g > since))
            .map(|e| e.uid)
            .collect();
        vanished.sort_unstable();
        Ok(MailboxChanges {
            status,
            changed,
            vanished,
        })
    }

    async fn flags(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
    ) -> Result<HashMap<EmailRef, Vec<String>>, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        if let Some(mailbox) = mailbox {
            s.selectable(viewer, mailbox)?;
        }
        let folders = mailbox.and_then(|m| s.folder_set(m));
        Ok(emails
            .iter()
            .filter_map(|e| {
                s.effective_flags(viewer.user, folders.as_deref(), e)
                    .map(|f| (*e, f))
            })
            .collect())
    }

    async fn envelopes(
        &self,
        viewer: &Viewer,
        emails: &[EmailRef],
    ) -> Result<Vec<Envelope>, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        Ok(emails
            .iter()
            .filter_map(|e| s.envelope(viewer.user, e))
            .collect())
    }

    async fn raw(&self, viewer: &Viewer, email: &EmailRef) -> Result<Bytes, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        s.raw(viewer.user, email)
            .map(|(raw, _, _)| raw)
            .ok_or(MailViewError::NotFound("email"))
    }

    async fn structure(&self, viewer: &Viewer, email: &EmailRef) -> Result<Part, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        s.raw(viewer.user, email)
            .map(|(raw, _, _)| mime::structure(&raw))
            .ok_or(MailViewError::NotFound("email"))
    }

    async fn blob(&self, viewer: &Viewer, blob: &BlobId) -> Result<Bytes, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        if let Some(msg) = s.messages.iter().find(|m| m.blob == *blob) {
            return Ok(msg.raw.clone());
        }
        s.drafts
            .iter()
            .filter(|d| d.user == viewer.user && !d.deleted)
            .flat_map(|d| d.versions.iter())
            .find(|v| v.blob == *blob)
            .map(|v| v.raw.clone())
            .ok_or(MailViewError::NotFound("blob"))
    }

    async fn search(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        filter: &Filter,
    ) -> Result<Vec<Member>, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.selectable(viewer, mailbox)?;
        let members = s.catch_up(viewer.user, mailbox);
        Ok(members
            .into_iter()
            .filter(|m| s.matches(viewer.user, mailbox, m, filter))
            .collect())
    }

    async fn thread(
        &self,
        viewer: &Viewer,
        thread: ThreadId,
    ) -> Result<Vec<EmailRef>, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        let mut msgs: Vec<&Msg> = s.messages.iter().filter(|m| m.thread == thread).collect();
        if msgs.is_empty() {
            return Err(MailViewError::NotFound("thread"));
        }
        msgs.sort_by_key(|m| (m.received_at, m.order));
        Ok(msgs.iter().map(|m| EmailRef::Message(m.id)).collect())
    }

    async fn state(&self, viewer: &Viewer) -> Result<ViewState, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        let mut state = ViewState::default();
        for row in s
            .log
            .iter()
            .filter(|r| r.user.is_none_or(|u| u == viewer.user))
        {
            let slot = match row.what {
                What::Email(_) => &mut state.email,
                What::Mailbox(_) | What::Counts => &mut state.mailbox,
                What::Thread(_) => &mut state.thread,
            };
            *slot = (*slot).max(row.seq);
        }
        Ok(state)
    }

    async fn changes(
        &self,
        viewer: &Viewer,
        since: u64,
        limit: u32,
    ) -> Result<ViewChanges, MailViewError> {
        let s = self.lock();
        s.check(viewer)?;
        if since > s.seq {
            return Err(MailViewError::CannotCalculateChanges);
        }
        let limit = limit.max(1) as usize;
        let rows: Vec<&Row> = s
            .log
            .iter()
            .filter(|r| r.seq > since && r.user.is_none_or(|u| u == viewer.user))
            .collect();
        let mut seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();
        seqs.dedup();
        let more = seqs.len() > limit;
        let upto = if more {
            seqs[limit - 1]
        } else {
            s.seq.max(since)
        };
        let window: Vec<&&Row> = rows.iter().filter(|r| r.seq <= upto).collect();
        let mut emails: Vec<(EmailRef, Ev, Ev)> = Vec::new();
        let mut mailboxes: Vec<(MailboxId, Ev, Ev)> = Vec::new();
        let mut threads: Vec<(ThreadId, Ev, Ev)> = Vec::new();
        fn note<T: PartialEq + Clone>(list: &mut Vec<(T, Ev, Ev)>, id: &T, ev: Ev) {
            match list.iter_mut().find(|(i, _, _)| i == id) {
                Some(entry) => entry.2 = ev,
                None => list.push((id.clone(), ev, ev)),
            }
        }
        let mut counts_changed = false;
        for row in window {
            match &row.what {
                What::Email(e) => note(&mut emails, e, row.ev),
                What::Mailbox(m) => note(&mut mailboxes, m, row.ev),
                What::Thread(t) => note(&mut threads, t, row.ev),
                What::Counts => counts_changed = true,
            }
        }
        fn fold<T>(list: Vec<(T, Ev, Ev)>) -> Changed<T> {
            let mut out = Changed::default();
            for (id, first, last) in list {
                let start = first != Ev::Created;
                let end = last != Ev::Destroyed;
                match (start, end) {
                    (false, true) => out.created.push(id),
                    (true, true) => out.updated.push(id),
                    (true, false) => out.destroyed.push(id),
                    (false, false) => {}
                }
            }
            out
        }
        Ok(ViewChanges {
            upto,
            more,
            emails: fold(emails),
            mailboxes: fold(mailboxes),
            counts_changed,
            threads: fold(threads),
        })
    }
}

fn flag_valid(change: &FlagChange) -> Result<(), MailViewError> {
    if change
        .add
        .iter()
        .chain(&change.remove)
        .any(|f| f.trim().is_empty())
    {
        return Err(MailViewError::Invalid("empty flag".into()));
    }
    if change
        .add
        .iter()
        .any(|a| change.remove.iter().any(|r| same(a, r)))
    {
        return Err(MailViewError::Invalid(
            "a flag both added and removed".into(),
        ));
    }
    Ok(())
}

fn refused(email: EmailRef, refusal: Refusal) -> PlaceResult {
    PlaceResult {
        email,
        outcome: Outcome::Refused(refusal),
        uid: None,
    }
}

fn outcome(held: bool, applied: bool, first: Option<Refusal>) -> Outcome {
    if held {
        Outcome::Held
    } else if applied {
        Outcome::Applied
    } else {
        Outcome::Refused(first.unwrap_or(Refusal::Cannot))
    }
}

#[async_trait]
impl MailActions for FakeMailView {
    async fn set_flags(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
        change: &FlagChange,
        unchanged_since: Option<u64>,
    ) -> Result<Vec<FlagResult>, MailViewError> {
        flag_valid(change)?;
        let mut s = self.lock();
        s.check(viewer)?;
        if let Some(m) = mailbox {
            s.selectable(viewer, m)?;
        }
        let user = viewer.user;
        let folders = mailbox.and_then(|m| s.folder_set(m));
        let touches_deleted = change
            .add
            .iter()
            .chain(&change.remove)
            .any(|f| same(f, holtmail_store::DELETED_FLAG));
        let mut results = Vec::new();
        for email in dedup(emails) {
            let modseq_of = |s: &mut State| match mailbox {
                Some(m) => s.member_of(user, m, &email).map_or(0, |m| m.modseq),
                None => s.seq,
            };
            let known = match mailbox {
                Some(m) => s.member_of(user, m, &email).is_some(),
                None => s.effective_flags(user, None, &email).is_some(),
            };
            if !known {
                results.push(FlagResult {
                    email,
                    outcome: Outcome::Refused(Refusal::NotFound),
                    flags: Vec::new(),
                    modseq: 0,
                });
                continue;
            }
            let before = s
                .effective_flags(user, folders.as_deref(), &email)
                .unwrap_or_default();
            let current = modseq_of(&mut s);
            let outcome = if unchanged_since.is_some_and(|u| current > u) {
                Outcome::Refused(Refusal::Modified)
            } else if email.message().is_none()
                || (change.add.iter().all(|a| before.iter().any(|f| same(f, a)))
                    && change
                        .remove
                        .iter()
                        .all(|r| !before.iter().any(|f| same(f, r))))
            {
                Outcome::Applied
            } else {
                let id = email.message().expect("message");
                let msg = s.msg(id).expect("message");
                let places = match mailbox {
                    Some(MailboxId::Kept) | Some(MailboxId::All) | None => s.live_places(msg, None),
                    Some(_) => s.live_places(msg, folders.as_deref()),
                };
                let mut writes = vec![Write::Flags];
                if touches_deleted {
                    writes.push(Write::Delete);
                }
                let mut open = Vec::new();
                let mut first: Option<Refusal> = None;
                let mut need_local = places.is_empty();
                for i in places {
                    let (folder, _, _) = msg.places[i].effective();
                    match s.lock(viewer, s.folder(folder).account, &writes) {
                        None => open.push(i),
                        Some(lock) => {
                            need_local = true;
                            first.get_or_insert(Refusal::Locked(lock));
                        }
                    }
                }
                let local_ok = s.setup.local_flags && !touches_deleted;
                if touches_deleted && first.is_none() && open.is_empty() {
                    first = Some(Refusal::Cannot);
                }
                let held = !open.is_empty();
                let local = need_local && local_ok;
                if held || local {
                    let msg = s.msg_mut(id);
                    for i in &open {
                        msg.places[*i]
                            .pending
                            .push(Op::Flags(change.add.clone(), change.remove.clone()));
                    }
                    if local {
                        let entry = s.local_flags.entry((user, id)).or_default();
                        for (flags, present) in [(&change.add, true), (&change.remove, false)] {
                            for flag in flags {
                                entry.retain(|(f, _)| !same(f, flag));
                                entry.push((flag.clone(), present));
                            }
                        }
                    }
                    s.touch(email, (!held).then_some(user));
                }
                outcome(held, local, first)
            };
            let flags = s
                .effective_flags(user, folders.as_deref(), &email)
                .unwrap_or_default();
            let modseq = modseq_of(&mut s);
            results.push(FlagResult {
                email,
                outcome,
                flags,
                modseq,
            });
        }
        Ok(results)
    }

    async fn move_to(
        &self,
        viewer: &Viewer,
        from: &MailboxId,
        emails: &[EmailRef],
        to: &MailboxId,
    ) -> Result<Vec<PlaceResult>, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.selectable(viewer, from)?;
        s.find_mailbox(viewer, to)?;
        let user = viewer.user;
        let blocked = matches!(
            to,
            MailboxId::All | MailboxId::Kept | MailboxId::Drafts | MailboxId::Node(_)
        ) || matches!(from, MailboxId::All | MailboxId::Kept | MailboxId::Drafts);
        let folders = s.folder_set(from);
        let mut results = Vec::new();
        for email in dedup(emails) {
            if s.member_of(user, from, &email).is_none() {
                results.push(refused(email, Refusal::NotFound));
                continue;
            }
            let Some(id) = email.message().filter(|_| !blocked) else {
                results.push(refused(email, Refusal::Cannot));
                continue;
            };
            let msg = s.msg(id).expect("message");
            let places = s.live_places(msg, folders.as_deref());
            let mut first: Option<Refusal> = None;
            let mut plan: Vec<(usize, Op)> = Vec::new();
            let mut appends: Vec<FolderId> = Vec::new();
            let mut applied = false;
            for i in places {
                let place = &msg.places[i];
                let (folder, _, _) = place.effective();
                let account = s.folder(folder).account;
                let target = match to {
                    MailboxId::Folder(t) => Some(*t),
                    MailboxId::Role(role) => s.role_folder_in(account, *role),
                    _ => None,
                };
                let Some(target) = target else {
                    first.get_or_insert(Refusal::Cannot);
                    continue;
                };
                if target == folder {
                    applied = true;
                    continue;
                }
                let target_account = s.folder(target).account;
                if target_account == account {
                    let write = if s.folder(target).role == Some(FolderRole::Trash) {
                        Write::Delete
                    } else {
                        Write::Move
                    };
                    match s.lock(viewer, account, &[write]) {
                        None => plan.push((i, Op::Move(target))),
                        Some(lock) => {
                            first.get_or_insert(Refusal::Locked(lock));
                        }
                    }
                } else {
                    let (trash, writes) = s.delete_target(place, account);
                    let lock = s
                        .lock(viewer, target_account, &[Write::Append])
                        .or_else(|| s.lock(viewer, account, &writes));
                    match lock {
                        None => {
                            plan.push((i, Op::Delete(trash)));
                            appends.push(target);
                        }
                        Some(lock) => {
                            first.get_or_insert(Refusal::Locked(lock));
                        }
                    }
                }
            }
            let held = !plan.is_empty();
            if held {
                let msg = s.msg_mut(id);
                for (i, op) in plan {
                    msg.places[i].pending.push(op);
                }
                for target in appends {
                    let flags = Vec::new();
                    msg.places.push(Place {
                        folder: target,
                        flags,
                        live: false,
                        appended: true,
                        pending: vec![Op::Append],
                    });
                }
                s.touch(email, None);
            }
            let outcome = outcome(held, applied, first);
            let uid = if outcome.accepted() {
                s.uid_in(user, to, &email)
            } else {
                None
            };
            results.push(PlaceResult {
                email,
                outcome,
                uid,
            });
        }
        Ok(results)
    }

    async fn copy_to(
        &self,
        viewer: &Viewer,
        from: &MailboxId,
        emails: &[EmailRef],
        to: &MailboxId,
    ) -> Result<Vec<PlaceResult>, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.selectable(viewer, from)?;
        s.find_mailbox(viewer, to)?;
        let user = viewer.user;
        let blocked = matches!(
            to,
            MailboxId::All | MailboxId::Kept | MailboxId::Drafts | MailboxId::Node(_)
        );
        let mut results = Vec::new();
        for email in dedup(emails) {
            if s.member_of(user, from, &email).is_none() {
                results.push(refused(email, Refusal::NotFound));
                continue;
            }
            let Some(id) = email.message().filter(|_| !blocked) else {
                results.push(refused(email, Refusal::Cannot));
                continue;
            };
            let target = match to {
                MailboxId::Folder(t) => Some(*t),
                MailboxId::Role(role) => {
                    let mut candidates: Vec<(String, FolderId)> = s
                        .role_folders(*role)
                        .into_iter()
                        .map(|f| (s.account(s.folder(f).account).name.clone(), f))
                        .collect();
                    candidates.sort();
                    candidates
                        .iter()
                        .find(|(_, f)| {
                            s.lock(viewer, s.folder(*f).account, &[Write::Append])
                                .is_none()
                        })
                        .or(candidates.first())
                        .map(|(_, f)| *f)
                }
                _ => None,
            };
            let Some(target) = target else {
                results.push(refused(email, Refusal::Cannot));
                continue;
            };
            let msg = s.msg(id).expect("message");
            if !s.live_places(msg, Some(&[target])).is_empty() {
                let uid = s.uid_in(user, to, &email);
                results.push(PlaceResult {
                    email,
                    outcome: Outcome::Applied,
                    uid,
                });
                continue;
            }
            match s.lock(viewer, s.folder(target).account, &[Write::Append]) {
                Some(lock) => results.push(refused(email, Refusal::Locked(lock))),
                None => {
                    s.msg_mut(id).places.push(Place {
                        folder: target,
                        flags: Vec::new(),
                        live: false,
                        appended: true,
                        pending: vec![Op::Append],
                    });
                    s.touch(email, None);
                    let uid = s.uid_in(user, to, &email);
                    results.push(PlaceResult {
                        email,
                        outcome: Outcome::Held,
                        uid,
                    });
                }
            }
        }
        Ok(results)
    }

    async fn remove(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
    ) -> Result<Vec<PlaceResult>, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        if let Some(m) = mailbox {
            s.selectable(viewer, m)?;
        }
        let user = viewer.user;
        let folders = mailbox.and_then(|m| s.folder_set(m));
        let mut results = Vec::new();
        for email in dedup(emails) {
            let known = match mailbox {
                Some(m) => s.member_of(user, m, &email).is_some(),
                None => s.effective_flags(user, None, &email).is_some(),
            };
            if !known {
                results.push(refused(email, Refusal::NotFound));
                continue;
            }
            if matches!(mailbox, Some(MailboxId::All | MailboxId::Kept)) {
                results.push(refused(email, Refusal::Cannot));
                continue;
            }
            let id = match email {
                EmailRef::Draft { draft, .. } => {
                    if let Some(d) = s.drafts.iter_mut().find(|d| d.id == draft) {
                        d.deleted = true;
                    }
                    s.bump();
                    s.log(What::Email(email), Ev::Destroyed, Some(user));
                    s.log(What::Counts, Ev::Updated, Some(user));
                    results.push(PlaceResult {
                        email,
                        outcome: Outcome::Applied,
                        uid: None,
                    });
                    continue;
                }
                EmailRef::Message(id) => id,
            };
            let mut applied = false;
            if let Some(role) = mailbox.and_then(|m| s.local_role(m)) {
                let before = s.local_places.len();
                s.local_places
                    .retain(|(u, r, m)| !(*u == user && *r == role && *m == id));
                applied = s.local_places.len() != before;
            }
            let msg = s.msg(id).expect("message");
            let places = s.live_places(msg, folders.as_deref());
            let mut first: Option<Refusal> = None;
            if places.is_empty() && !applied {
                first = Some(Refusal::Cannot);
            }
            let mut plan = Vec::new();
            for i in places {
                let place = &msg.places[i];
                let (folder, _, _) = place.effective();
                let account = s.folder(folder).account;
                let (target, writes) = if mailbox.is_none() {
                    (None, vec![Write::Delete, Write::Expunge])
                } else {
                    s.delete_target(place, account)
                };
                match s.lock(viewer, account, &writes) {
                    None => plan.push((i, Op::Delete(target))),
                    Some(lock) => {
                        first.get_or_insert(Refusal::Locked(lock));
                    }
                }
            }
            let held = !plan.is_empty();
            if held {
                let msg = s.msg_mut(id);
                for (i, op) in plan {
                    msg.places[i].pending.push(op);
                }
            }
            if held || applied {
                s.touch(email, (!held).then_some(user));
            }
            results.push(PlaceResult {
                email,
                outcome: outcome(held, applied, first),
                uid: None,
            });
        }
        Ok(results)
    }

    async fn append(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        email: NewEmail,
    ) -> Result<Appended, MailViewError> {
        if email.raw.len() as u64 > FAKE_APPEND_LIMIT {
            return Err(MailViewError::TooLarge {
                limit: FAKE_APPEND_LIMIT,
            });
        }
        let mut s = self.lock();
        s.check(viewer)?;
        s.find_mailbox(viewer, mailbox)?;
        let user = viewer.user;
        let refused = |refusal| Appended {
            outcome: Outcome::Refused(refusal),
            email: None,
            uid: None,
        };
        let received_at = email.received_at.unwrap_or_else(Timestamp::now);
        let (target, role) = match mailbox {
            MailboxId::All | MailboxId::Kept | MailboxId::Node(_) => {
                return Ok(refused(Refusal::Cannot));
            }
            MailboxId::Drafts => {
                let seq = s.bump();
                let id = DraftId::new();
                s.drafts.push(Draft {
                    id,
                    user,
                    versions: vec![DraftVersion {
                        blob: BlobId::of(&email.raw),
                        raw: email.raw,
                        saved_at: received_at,
                        order: seq,
                    }],
                    deleted: false,
                });
                let draft = EmailRef::Draft {
                    draft: id,
                    version: 1,
                };
                s.log(What::Email(draft), Ev::Created, Some(user));
                s.log(What::Counts, Ev::Updated, Some(user));
                let uid = s.uid_in(user, mailbox, &draft);
                return Ok(Appended {
                    outcome: Outcome::Applied,
                    email: Some(draft),
                    uid,
                });
            }
            MailboxId::Folder(f) => (vec![*f], s.folder(*f).role),
            MailboxId::Role(role) => {
                let mut candidates: Vec<(String, FolderId)> = s
                    .role_folders(*role)
                    .into_iter()
                    .map(|f| (s.account(s.folder(f).account).name.clone(), f))
                    .collect();
                candidates.sort();
                (
                    candidates.into_iter().map(|(_, f)| f).collect(),
                    Some(*role),
                )
            }
        };
        let open = target
            .iter()
            .find(|f| {
                s.lock(viewer, s.folder(**f).account, &[Write::Append])
                    .is_none()
            })
            .copied();
        let first = target
            .first()
            .and_then(|f| s.lock(viewer, s.folder(*f).account, &[Write::Append]));
        let flags: Vec<String> = email
            .flags
            .iter()
            .filter(|f| !same(f, holtmail_store::DELETED_FLAG))
            .cloned()
            .collect();
        let viewer_lock = s.viewer_lock(viewer);
        let local_sent = viewer_lock.is_none()
            && target.iter().all(|f| {
                s.lock(viewer, s.folder(*f).account, &[Write::Append]) == Some(Lock::Capability)
            });
        let (id, outcome) = match (open, role) {
            (Some(folder), _) => {
                let id = s.archive_bytes(email.raw, received_at);
                let msg = s.msg(id).expect("message");
                if s.live_places(msg, Some(&[folder])).is_empty() {
                    s.msg_mut(id).places.push(Place {
                        folder,
                        flags: flags.clone(),
                        live: false,
                        appended: true,
                        pending: vec![Op::Append],
                    });
                    s.touch(EmailRef::Message(id), None);
                }
                (id, Outcome::Held)
            }
            (None, Some(FolderRole::Sent)) if local_sent => {
                let id = s.archive_bytes(email.raw, received_at);
                if !s
                    .local_places
                    .iter()
                    .any(|(u, r, m)| *u == user && *r == FolderRole::Sent && *m == id)
                {
                    s.local_places.push((user, FolderRole::Sent, id));
                    if s.setup.local_flags && !flags.is_empty() {
                        let entry = s.local_flags.entry((user, id)).or_default();
                        for flag in &flags {
                            entry.retain(|(f, _)| !same(f, flag));
                            entry.push((flag.clone(), true));
                        }
                    }
                    s.touch(EmailRef::Message(id), Some(user));
                }
                (id, Outcome::Applied)
            }
            (None, Some(FolderRole::Sent)) => {
                return Ok(refused(Refusal::Locked(
                    viewer_lock.or(first).unwrap_or(Lock::Capability),
                )));
            }
            (None, _) => {
                return Ok(refused(first.map_or(Refusal::Cannot, Refusal::Locked)));
            }
        };
        let email = EmailRef::Message(id);
        let uid = s.uid_in(user, mailbox, &email);
        Ok(Appended {
            outcome,
            email: Some(email),
            uid,
        })
    }

    async fn append_limit(&self, viewer: &Viewer) -> Result<u64, MailViewError> {
        self.lock().check(viewer)?;
        Ok(FAKE_APPEND_LIMIT)
    }

    async fn subscribe(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        subscribed: bool,
    ) -> Result<bool, MailViewError> {
        let mut s = self.lock();
        s.check(viewer)?;
        s.find_mailbox(viewer, mailbox)?;
        let changed = s.subscribed(viewer.user, mailbox) != subscribed;
        s.subscriptions
            .insert((viewer.user, mailbox.key()), subscribed);
        if changed {
            s.bump();
            s.log(
                What::Mailbox(mailbox.clone()),
                Ev::Updated,
                Some(viewer.user),
            );
        }
        Ok(changed)
    }
}

#[async_trait]
impl Driver for FakeMailView {
    async fn settle(&self, applied: bool) {
        self.lock().settle(applied);
    }

    async fn deliver(&self, account: &str, folder: &str, raw: &str) -> MessageId {
        let mut s = self.lock();
        let account = s
            .accounts
            .iter()
            .find(|a| a.name == account)
            .expect("account")
            .id;
        let folder = s
            .folders
            .iter()
            .find(|f| f.account == account && f.path == folder)
            .expect("folder")
            .id;
        let id = s.archive_bytes(Bytes::from(raw.to_string()), Timestamp::now());
        s.msg_mut(id).places.push(Place {
            folder,
            flags: Vec::new(),
            live: true,
            appended: false,
            pending: Vec::new(),
        });
        s.touch(EmailRef::Message(id), None);
        id
    }
}
