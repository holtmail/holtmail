use std::collections::HashMap;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_core::{FolderId, MessageId, ThreadId, Timestamp};
use holtmail_mailview::{
    EmailRef, Envelope, Filter, MailView, MailViewError, Mailbox, MailboxChanges, MailboxId,
    MailboxStatus, Member, MemberFlags, Part, Snapshot, ViewChanges, ViewState, Viewer,
};
use holtmail_store::{EffectiveFlagsQuery, FolderRole, Message};

use crate::layout::Entry;
use crate::{ArchiveMailView, Ctx, has, member_of, mime, not_selectable, store_error, unavailable};

const DRAFT_FLAGS: [&str; 2] = ["\\Draft", "\\Seen"];

pub(crate) struct Selected {
    pub(crate) entry: Entry,
    pub(crate) status: MailboxStatus,
    pub(crate) members: Vec<Member>,
}

impl ArchiveMailView {
    pub(crate) fn selectable(&self, ctx: &Ctx, id: &MailboxId) -> Result<Entry, MailViewError> {
        if !id.selectable() {
            return Err(not_selectable());
        }
        ctx.entry(id).ok_or_else(not_selectable)
    }

    /// The mailbox's members and counts after a catch-up.
    pub(crate) async fn select_entry(
        &self,
        ctx: &Ctx,
        entry: Entry,
    ) -> Result<Selected, MailViewError> {
        let key = entry.key.ok_or_else(not_selectable)?;
        let view = self.served(ctx, key).await?;
        let uids = self.uids(ctx, key, None).await?;
        let members = self.visible(ctx, &entry, uids).await?;
        let emails: Vec<EmailRef> = members.iter().map(|m| m.email).collect();
        let flags = self.effective(ctx, &entry, &emails).await?;
        let unseen = emails
            .iter()
            .filter(|e| !flags.get(e).is_some_and(|f| has(f, "\\Seen")))
            .count() as u32;
        let status = MailboxStatus {
            messages: members.len() as u32,
            unseen,
            uidvalidity: view.uidvalidity,
            uidnext: view.uidnext,
            highest_modseq: view.highest_modseq.max(1),
        };
        Ok(Selected {
            entry,
            status,
            members,
        })
    }

    pub(crate) async fn member(
        &self,
        ctx: &Ctx,
        id: &MailboxId,
        email: EmailRef,
    ) -> Result<Option<Member>, MailViewError> {
        let entry = self.selectable(ctx, id)?;
        let key = entry.key.ok_or_else(not_selectable)?;
        self.served(ctx, key).await?;
        let found = self
            .store
            .served_uid_of(ctx.viewer.archive, key, &[member_of(email)])
            .await
            .map_err(store_error)?;
        let Some(uid) = found.get(&member_of(email)).copied() else {
            return Ok(None);
        };
        let members = self
            .store
            .served_uids(
                ctx.viewer.archive,
                key,
                &holtmail_store::ServedUidQuery {
                    after_uid: uid - 1,
                    changed_since: None,
                    limit: 1,
                },
            )
            .await
            .map_err(store_error)?
            .into_iter()
            .filter(|u| u.uid == uid)
            .collect();
        Ok(self.visible(ctx, &entry, members).await?.into_iter().next())
    }

    /// The effective flags of the emails in the mailbox; unknown emails left out.
    pub(crate) async fn effective(
        &self,
        ctx: &Ctx,
        entry: &Entry,
        emails: &[EmailRef],
    ) -> Result<HashMap<EmailRef, Vec<String>>, MailViewError> {
        self.flags_in(ctx, entry.folders.clone(), emails).await
    }

    pub(crate) async fn flags_in(
        &self,
        ctx: &Ctx,
        folders: Option<Vec<FolderId>>,
        emails: &[EmailRef],
    ) -> Result<HashMap<EmailRef, Vec<String>>, MailViewError> {
        let mut out = HashMap::new();
        let mut messages = Vec::new();
        for email in emails {
            match email {
                EmailRef::Message(m) => messages.push(*m),
                EmailRef::Draft { draft, version } => {
                    if self.raw_draft(ctx, *draft, *version).await?.is_some() {
                        out.insert(*email, DRAFT_FLAGS.map(String::from).to_vec());
                    }
                }
            }
        }
        let known: Vec<MessageId> = self
            .messages(ctx, &messages)
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect();
        for chunk in known.chunks(500) {
            let found = self
                .store
                .effective_flags(&EffectiveFlagsQuery {
                    user: ctx.user(),
                    messages: chunk.to_vec(),
                    folders: folders.clone(),
                    local_flags: ctx.local,
                })
                .await
                .map_err(store_error)?;
            for message in chunk {
                out.insert(
                    EmailRef::Message(*message),
                    found.get(message).cloned().unwrap_or_default(),
                );
            }
        }
        Ok(out)
    }

    /// The archive's messages among `ids`, in order.
    pub(crate) async fn messages(
        &self,
        ctx: &Ctx,
        ids: &[MessageId],
    ) -> Result<Vec<Message>, MailViewError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(500) {
            out.extend(
                self.store
                    .get_messages(chunk)
                    .await
                    .map_err(store_error)?
                    .into_iter()
                    .filter(|m| m.archive_id == ctx.viewer.archive),
            );
        }
        Ok(out)
    }

    pub(crate) async fn read_blob(&self, blob: &BlobId) -> Result<Bytes, MailViewError> {
        self.blobs
            .get(blob)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| MailViewError::Unavailable("the archived bytes are missing".into()))
    }

    /// The email's bytes, its received time and, for a message, its record.
    pub(crate) async fn bytes(
        &self,
        ctx: &Ctx,
        email: &EmailRef,
    ) -> Result<Option<(Bytes, Timestamp, Option<Message>)>, MailViewError> {
        match email {
            EmailRef::Message(id) => {
                let Some(message) = self.messages(ctx, &[*id]).await?.into_iter().next() else {
                    return Ok(None);
                };
                let raw = self.read_blob(&message.blob_id).await?;
                let at = message.received_at.unwrap_or(message.archived_at);
                Ok(Some((raw, at, Some(message))))
            }
            EmailRef::Draft { draft, version } => {
                let Some((blob, saved)) = self.raw_draft(ctx, *draft, *version).await? else {
                    return Ok(None);
                };
                Ok(Some((self.read_blob(&blob).await?, saved, None)))
            }
        }
    }

    async fn envelope(
        &self,
        ctx: &Ctx,
        email: EmailRef,
    ) -> Result<Option<Envelope>, MailViewError> {
        let Some((raw, received_at, message)) = self.bytes(ctx, &email).await? else {
            return Ok(None);
        };
        let parsed = holtmail_mime::parse(&raw).ok();
        let fields = mime::top_fields(&raw);
        let structure = mime::structure(&raw);
        let in_reply_to = parsed.as_ref().and_then(|p| p.in_reply_to.first().cloned());
        let reply_to = parsed
            .as_ref()
            .map(|p| p.reply_to.clone())
            .unwrap_or_default();
        let sender = mime::addresses(mime::get(&fields, "sender"));
        let (folders, local_roles) = match &message {
            Some(message) => {
                let folders: Vec<FolderId> = self
                    .places(message.id)
                    .await?
                    .into_iter()
                    .map(|p| p.shown)
                    .collect();
                let roles: Vec<FolderRole> = self
                    .store
                    .local_places(ctx.user(), ctx.viewer.archive, None)
                    .await
                    .map_err(store_error)?
                    .into_iter()
                    .filter(|p| p.message_id == message.id)
                    .map(|p| p.role)
                    .collect();
                (folders, roles)
            }
            None => (Vec::new(), Vec::new()),
        };
        let mut mailboxes = ctx.mailboxes_of(&folders, &local_roles, message.is_some());
        if !email.message().is_some() {
            mailboxes = vec![MailboxId::Drafts];
        }
        let envelope = match message {
            Some(message) => Envelope {
                email,
                thread: Some(message.thread_id),
                blob: message.blob_id,
                size: message.size,
                crlf_size: mime::crlf_size(&raw),
                rfc_message_id: message.rfc_message_id,
                in_reply_to,
                subject: message.subject,
                from: message.from,
                sender,
                reply_to,
                to: message.to,
                cc: message.cc,
                bcc: message.bcc,
                sent_at: message.sent_at,
                received_at,
                preview: message.preview,
                has_attachment: message.attachment_count > 0,
                mailboxes,
            },
            None => {
                let parsed = parsed.unwrap_or_else(|| holtmail_mime::ParsedMessage {
                    message_id: None,
                    in_reply_to: Vec::new(),
                    references: Vec::new(),
                    subject: None,
                    sent_at: None,
                    from: Vec::new(),
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    reply_to: Vec::new(),
                    headers: Vec::new(),
                    text: None,
                    html: None,
                    preview: String::new(),
                    attachments: Vec::new(),
                    calendars: Vec::new(),
                });
                Envelope {
                    email,
                    thread: None,
                    blob: BlobId::of(&raw),
                    size: raw.len() as u64,
                    crlf_size: mime::crlf_size(&raw),
                    rfc_message_id: parsed.message_id,
                    in_reply_to,
                    subject: parsed.subject,
                    from: parsed.from,
                    sender,
                    reply_to,
                    to: parsed.to,
                    cc: parsed.cc,
                    bcc: parsed.bcc,
                    sent_at: parsed.sent_at.map(Timestamp::from_secs),
                    received_at,
                    preview: parsed.preview,
                    has_attachment: mime::has_attachment(&structure),
                    mailboxes,
                }
            }
        };
        Ok(Some(envelope))
    }
}

#[async_trait]
impl MailView for ArchiveMailView {
    async fn tree(&self, viewer: &Viewer) -> Result<Vec<Mailbox>, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        Ok(ctx.layout().iter().map(|e| ctx.mailbox(e)).collect())
    }

    async fn status(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
    ) -> Result<MailboxStatus, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let entry = self.selectable(&ctx, mailbox)?;
        Ok(self.select_entry(&ctx, entry).await?.status)
    }

    async fn select(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
    ) -> Result<Snapshot, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let entry = self.selectable(&ctx, mailbox)?;
        let selected = self.select_entry(&ctx, entry).await?;
        Ok(Snapshot {
            mailbox: ctx.mailbox(&selected.entry),
            status: selected.status,
            members: selected.members,
        })
    }

    async fn mailbox_changes(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        since: u64,
    ) -> Result<MailboxChanges, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let entry = self.selectable(&ctx, mailbox)?;
        let key = entry.key.ok_or_else(not_selectable)?;
        let selected = self.select_entry(&ctx, entry).await?;
        let changed: Vec<Member> = selected
            .members
            .into_iter()
            .filter(|m| m.modseq > since)
            .collect();
        let emails: Vec<EmailRef> = changed.iter().map(|m| m.email).collect();
        let flags = self.effective(&ctx, &selected.entry, &emails).await?;
        let changed = changed
            .into_iter()
            .map(|member| MemberFlags {
                flags: flags.get(&member.email).cloned().unwrap_or_default(),
                member,
            })
            .collect();
        let mut vanished = Vec::new();
        let mut after = 0;
        loop {
            let page = self
                .store
                .served_vanished(viewer.archive, key, since, after, 10_000)
                .await
                .map_err(store_error)?;
            let full = page.len() == 10_000;
            after = page.last().copied().unwrap_or(after);
            vanished.extend(page);
            if !full {
                break;
            }
        }
        Ok(MailboxChanges {
            status: selected.status,
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
        let ctx = self.ctx(viewer).await?;
        let folders = match mailbox {
            Some(id) => self.selectable(&ctx, id)?.folders,
            None => None,
        };
        self.flags_in(&ctx, folders, emails).await
    }

    async fn envelopes(
        &self,
        viewer: &Viewer,
        emails: &[EmailRef],
    ) -> Result<Vec<Envelope>, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let mut out = Vec::new();
        for email in emails {
            if let Some(envelope) = self.envelope(&ctx, *email).await? {
                out.push(envelope);
            }
        }
        Ok(out)
    }

    async fn raw(&self, viewer: &Viewer, email: &EmailRef) -> Result<Bytes, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        self.bytes(&ctx, email)
            .await?
            .map(|(raw, _, _)| raw)
            .ok_or(MailViewError::NotFound("email"))
    }

    async fn structure(&self, viewer: &Viewer, email: &EmailRef) -> Result<Part, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let blob = match email {
            EmailRef::Message(id) => self
                .messages(&ctx, &[*id])
                .await?
                .into_iter()
                .next()
                .map(|m| m.blob_id),
            EmailRef::Draft { draft, version } => self
                .raw_draft(&ctx, *draft, *version)
                .await?
                .map(|(blob, _)| blob),
        };
        let blob = blob.ok_or(MailViewError::NotFound("email"))?;
        if let Some(part) = self.cached_structure(&blob) {
            return Ok(part);
        }
        let _turn = self.turn(ctx.user()).await;
        if let Some(part) = self.cached_structure(&blob) {
            return Ok(part);
        }
        let raw = self.read_blob(&blob).await?;
        let part = mime::structure(&raw);
        self.keep_structure(blob, &part);
        Ok(part)
    }

    async fn blob(&self, viewer: &Viewer, blob: &BlobId) -> Result<Bytes, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        if self
            .store
            .find_by_blob(viewer.archive, blob)
            .await
            .map_err(store_error)?
            .is_some()
        {
            return self.read_blob(blob).await;
        }
        let drafts = self
            .store
            .list_drafts(viewer.archive, ctx.user())
            .await
            .map_err(store_error)?;
        if drafts.iter().any(|d| d.current.raw.as_ref() == Some(blob)) {
            return self.read_blob(blob).await;
        }
        Err(MailViewError::NotFound("blob"))
    }

    async fn search(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        filter: &Filter,
    ) -> Result<Vec<Member>, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let entry = self.selectable(&ctx, mailbox)?;
        let selected = self.select_entry(&ctx, entry).await?;
        let _turn = self.turn(ctx.user()).await;
        self.matching(&ctx, &selected.entry, selected.members, filter)
            .await
    }

    async fn thread(
        &self,
        viewer: &Viewer,
        thread: ThreadId,
    ) -> Result<Vec<EmailRef>, MailViewError> {
        self.ctx(viewer).await?;
        let mut messages: Vec<Message> = self
            .store
            .thread_messages(thread)
            .await
            .map_err(store_error)?
            .into_iter()
            .filter(|m| m.archive_id == viewer.archive)
            .collect();
        if messages.is_empty() {
            return Err(MailViewError::NotFound("thread"));
        }
        messages.sort_by_key(|m| (m.received_at.unwrap_or(m.archived_at), m.archived_at, m.id));
        Ok(messages
            .into_iter()
            .map(|m| EmailRef::Message(m.id))
            .collect())
    }

    async fn state(&self, viewer: &Viewer) -> Result<ViewState, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        self.view_state(&ctx).await
    }

    async fn changes(
        &self,
        viewer: &Viewer,
        since: u64,
        limit: u32,
    ) -> Result<ViewChanges, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        self.view_changes(&ctx, since, limit).await
    }
}
