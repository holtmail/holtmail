use std::collections::{HashMap, HashSet};

use holtmail_core::{DraftId, FolderId, MessageId, ThreadId, Timestamp};
use holtmail_mailview::{Changed, EmailRef, MailViewError, MailboxId, ViewChanges, ViewState};
use holtmail_store::{ChangeKind, ChangeLogEntry};

use crate::{ArchiveMailView, Ctx, store_error};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ev {
    Created,
    Updated,
    Destroyed,
}

struct Notes<T> {
    order: Vec<T>,
    events: HashMap<T, (Ev, Ev)>,
}

impl<T: Clone + Eq + std::hash::Hash> Notes<T> {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            events: HashMap::new(),
        }
    }

    fn note(&mut self, id: T, ev: Ev) {
        match self.events.get_mut(&id) {
            Some(entry) => entry.1 = ev,
            None => {
                self.order.push(id.clone());
                self.events.insert(id, (ev, ev));
            }
        }
    }

    fn fold(self) -> Changed<T> {
        let mut out = Changed::default();
        for id in self.order {
            let (first, last) = self.events[&id];
            match (first != Ev::Created, last != Ev::Destroyed) {
                (false, true) => out.created.push(id),
                (true, true) => out.updated.push(id),
                (true, false) => out.destroyed.push(id),
                (false, false) => {}
            }
        }
        out
    }
}

const PAGE: u32 = 1_000;

impl ArchiveMailView {
    pub(crate) async fn view_state(&self, ctx: &Ctx) -> Result<ViewState, MailViewError> {
        let heads = self
            .store
            .archive_heads(ctx.viewer.archive, Some(ctx.user()))
            .await
            .map_err(store_error)?;
        let head = |kinds: &[ChangeKind]| {
            kinds
                .iter()
                .filter_map(|k| heads.get(k))
                .copied()
                .max()
                .unwrap_or(0)
        };
        Ok(ViewState {
            email: head(&[
                ChangeKind::Message,
                ChangeKind::Place,
                ChangeKind::Flags,
                ChangeKind::Draft,
                ChangeKind::Thread,
            ]),
            mailbox: head(&[
                ChangeKind::Mailbox,
                ChangeKind::Message,
                ChangeKind::Place,
                ChangeKind::Flags,
                ChangeKind::Draft,
            ]),
            thread: head(&[ChangeKind::Message, ChangeKind::Thread]),
        })
    }

    /// The viewer's rows after `since`: at most `limit` of them, and whether more follow.
    async fn window(
        &self,
        ctx: &Ctx,
        since: u64,
        limit: usize,
    ) -> Result<(Vec<ChangeLogEntry>, bool), MailViewError> {
        let mut rows = Vec::new();
        let mut cursor = since;
        loop {
            let page = self
                .store
                .changes_since(ctx.viewer.archive, Some(cursor), PAGE)
                .await
                .map_err(store_error)?;
            let full = page.len() == PAGE as usize;
            for row in page {
                cursor = row.seq;
                if row.user_id.is_some_and(|u| u != ctx.user())
                    || matches!(row.kind, ChangeKind::Item | ChangeKind::Submission)
                {
                    continue;
                }
                if rows.len() == limit {
                    return Ok((rows, true));
                }
                rows.push(row);
            }
            if !full {
                return Ok((rows, false));
            }
        }
    }

    pub(crate) async fn view_changes(
        &self,
        ctx: &Ctx,
        since: u64,
        limit: u32,
    ) -> Result<ViewChanges, MailViewError> {
        let latest = self.store.latest_seq().await.map_err(store_error)?;
        if since > latest || since < self.store.pruned_through().await.map_err(store_error)? {
            return Err(MailViewError::CannotCalculateChanges);
        }
        let (rows, more) = self.window(ctx, since, limit.max(1) as usize).await?;
        let upto = if more {
            rows.last().map_or(since, |r| r.seq)
        } else {
            latest.max(since)
        };
        let started = rows.first().map(|r| r.at).unwrap_or_else(Timestamp::now);
        let mut emails: Notes<EmailRef> = Notes::new();
        let mut mailboxes: Notes<MailboxId> = Notes::new();
        let mut threads: Notes<ThreadId> = Notes::new();
        let mut entered: Vec<(ThreadId, MessageId)> = Vec::new();
        let mut drafts: Vec<DraftId> = Vec::new();
        let mut folders: Vec<FolderId> = Vec::new();
        let mut subscriptions = false;
        let mut counts_changed = false;
        for row in &rows {
            match row.kind {
                ChangeKind::Message => {
                    let Some(message) = row.message_id else {
                        continue;
                    };
                    counts_changed = true;
                    match row.thread_id {
                        Some(thread) => {
                            emails.note(EmailRef::Message(message), Ev::Created);
                            entered.push((thread, message));
                        }
                        None => emails.note(EmailRef::Message(message), Ev::Destroyed),
                    }
                }
                ChangeKind::Place | ChangeKind::Flags => {
                    if let Some(message) = row.message_id {
                        emails.note(EmailRef::Message(message), Ev::Updated);
                        counts_changed = true;
                    }
                }
                ChangeKind::Thread => {
                    if let Some(message) = row.message_id {
                        emails.note(EmailRef::Message(message), Ev::Updated);
                    }
                    if let Some(thread) = row.thread_id {
                        threads.note(thread, Ev::Updated);
                    }
                }
                ChangeKind::Draft => {
                    if let Some(draft) = row.draft_id {
                        if !drafts.contains(&draft) {
                            drafts.push(draft);
                        }
                        counts_changed = true;
                    }
                }
                ChangeKind::Mailbox => match row.folder_id {
                    Some(folder) => {
                        if !folders.contains(&folder) {
                            folders.push(folder);
                        }
                    }
                    None => subscriptions = true,
                },
                ChangeKind::Item | ChangeKind::Submission => {}
            }
        }
        let created: HashSet<MessageId> = emails
            .events
            .iter()
            .filter(|(_, (first, _))| *first == Ev::Created)
            .filter_map(|(e, _)| e.message())
            .collect();
        for (thread, _) in &entered {
            let members = self
                .store
                .thread_messages(*thread)
                .await
                .map_err(store_error)?;
            let new = !members.is_empty() && members.iter().all(|m| created.contains(&m.id));
            threads.note(*thread, if new { Ev::Created } else { Ev::Updated });
        }
        for thread in threads.order.clone() {
            if self
                .store
                .thread_messages(thread)
                .await
                .map_err(store_error)?
                .is_empty()
            {
                threads.note(thread, Ev::Destroyed);
            }
        }
        for draft in drafts {
            match self.store.get_draft(draft).await.map_err(store_error)? {
                Some(found) if found.user_id == ctx.user() => {
                    let current = found.current.version;
                    for version in self
                        .store
                        .draft_versions(draft)
                        .await
                        .map_err(store_error)?
                    {
                        if version.version != current && version.raw.is_some() {
                            emails.note(
                                EmailRef::Draft {
                                    draft,
                                    version: version.version,
                                },
                                Ev::Destroyed,
                            );
                        }
                    }
                    if found.current.raw.is_some() {
                        emails.note(
                            EmailRef::Draft {
                                draft,
                                version: current,
                            },
                            Ev::Created,
                        );
                    }
                }
                Some(_) => {}
                None => emails.note(EmailRef::Draft { draft, version: 1 }, Ev::Destroyed),
            }
        }
        let layout = ctx.layout();
        let new_folder = |id: &FolderId| ctx.folder(*id).is_some_and(|f| f.created_at >= started);
        let new_account = |account: holtmail_core::AccountId| {
            let mut own = ctx.folders.iter().filter(|f| f.account_id == account);
            own.clone().next().is_some() && own.all(|f| f.created_at >= started)
        };
        if folders.iter().any(|f| match ctx.folder(*f) {
            None => true,
            Some(found) => new_account(found.account_id),
        }) {
            return Err(MailViewError::CannotCalculateChanges);
        }
        let behind = |entry: &crate::Entry| -> Vec<FolderId> {
            match &entry.id {
                MailboxId::Node(_) => layout
                    .iter()
                    .filter(|e| e.path.len() > entry.path.len() && e.path.starts_with(&entry.path))
                    .filter_map(|e| match e.id {
                        MailboxId::Folder(f) => Some(f),
                        _ => None,
                    })
                    .collect(),
                _ => entry.folders.clone().unwrap_or_default(),
            }
        };
        for folder in &folders {
            let path = layout
                .iter()
                .find(|e| e.id == MailboxId::Folder(*folder))
                .map(|e| e.path.clone());
            for entry in &layout {
                let touched = match &entry.id {
                    MailboxId::Folder(f) => f == folder,
                    MailboxId::Role(_) => behind(entry).contains(folder),
                    MailboxId::Node(_) => path
                        .as_ref()
                        .is_some_and(|p| p.len() > entry.path.len() && p.starts_with(&entry.path)),
                    _ => false,
                };
                if !touched {
                    continue;
                }
                let folders = behind(entry);
                let ev = if !folders.is_empty() && folders.iter().all(new_folder) {
                    Ev::Created
                } else {
                    Ev::Updated
                };
                mailboxes.note(entry.id.clone(), ev);
            }
            if ctx.multi() && !new_folder(folder) {
                for (role, _) in crate::layout::UNIFIED {
                    let id = MailboxId::Role(role);
                    if !layout.iter().any(|e| e.id == id) {
                        mailboxes.note(id, Ev::Destroyed);
                    }
                }
            }
        }
        if subscriptions {
            for entry in &layout {
                if ctx.subscriptions.contains_key(&entry.id.key()) {
                    mailboxes.note(entry.id.clone(), Ev::Updated);
                }
            }
        }
        Ok(ViewChanges {
            upto,
            more,
            emails: emails.fold(),
            mailboxes: mailboxes.fold(),
            counts_changed,
            threads: threads.fold(),
        })
    }
}
