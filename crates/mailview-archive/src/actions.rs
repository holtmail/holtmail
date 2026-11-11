use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use holtmail_core::{AccountId, DeviceScope, FolderId, MailChangeId, MessageId, Timestamp};
use holtmail_mailview::{
    Appended, EmailRef, FlagChange, FlagResult, Lock, MailActions, MailViewError, MailboxId,
    NewEmail, Outcome, PlaceResult, Refusal, Viewer,
};
use holtmail_store::{
    AuditKind, Capability, DELETED_FLAG, EffectiveFlagsQuery, FolderRole, MailChangeOrigin, MailOp,
    NewAuditEntry, NewMailChange, OutboxState,
};

use crate::{ArchiveMailView, Ctx, Place, has, is_special, same, store_error, unavailable};

struct Plan {
    account: AccountId,
    audit: AuditKind,
    change: NewMailChange,
}

/// The writes one action decided on, recorded together once every email is planned.
#[derive(Default)]
struct Writes {
    cancel: Vec<MailChangeId>,
    plans: Vec<Plan>,
}

const OUTBOX_COPY_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

fn dedup(emails: &[EmailRef]) -> Vec<EmailRef> {
    let mut seen = HashSet::new();
    emails.iter().copied().filter(|e| seen.insert(*e)).collect()
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

fn valid(change: &FlagChange) -> Result<(), MailViewError> {
    if change
        .add
        .iter()
        .chain(&change.remove)
        .any(|f| f.trim().is_empty() || f.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(MailViewError::Invalid("an empty or malformed flag".into()));
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

fn audit_kind(op: &MailOp, to_trash: bool) -> AuditKind {
    match op {
        MailOp::Flags { .. } | MailOp::Labels { .. } => AuditKind::MailFlags,
        MailOp::Move { .. } if to_trash => AuditKind::MailDelete,
        MailOp::Move { .. } => AuditKind::MailMove,
        MailOp::Delete { .. } => AuditKind::MailDelete,
        MailOp::Append { .. } => AuditKind::MailAppend,
    }
}

impl ArchiveMailView {
    fn change(
        ctx: &Ctx,
        account: AccountId,
        folder: FolderId,
        message: MessageId,
        uid: Option<u64>,
        op: MailOp,
    ) -> NewMailChange {
        NewMailChange {
            account_id: account,
            folder_id: folder,
            message_id: Some(message),
            uid,
            op,
            origin: MailChangeOrigin::User,
            user_id: Some(ctx.user()),
            device_id: ctx.viewer.device,
            outbox_id: None,
            at: Timestamp::now(),
        }
    }

    fn plan(writes: &mut Writes, ctx: &Ctx, account: AccountId, change: NewMailChange) {
        let to_trash = match &change.op {
            MailOp::Move { target } => {
                ctx.folder(*target).and_then(|f| f.role) == Some(FolderRole::Trash)
            }
            _ => false,
        };
        writes.plans.push(Plan {
            account,
            audit: audit_kind(&change.op, to_trash),
            change,
        });
    }

    /// Plans moving a place to `target` within its account: a held move from where the
    /// location is recorded, replacing a held move that took it elsewhere.
    fn plan_move(
        writes: &mut Writes,
        ctx: &Ctx,
        account: AccountId,
        message: MessageId,
        place: &Place,
        target: FolderId,
    ) {
        if let Some(held) = place.held_move {
            writes.cancel.push(held);
        }
        if target != place.folder {
            let change = Self::change(
                ctx,
                account,
                place.folder,
                message,
                place.uid,
                MailOp::Move { target },
            );
            Self::plan(writes, ctx, account, change);
        }
    }

    fn plan_expunge(
        writes: &mut Writes,
        ctx: &Ctx,
        account: AccountId,
        message: MessageId,
        place: &Place,
    ) {
        if let Some(held) = place.held_move {
            writes.cancel.push(held);
        }
        let change = Self::change(
            ctx,
            account,
            place.folder,
            message,
            place.uid,
            MailOp::Delete { expunge: true },
        );
        Self::plan(writes, ctx, account, change);
    }

    /// How a delete of the place goes: to the account's trash, or expunged in the trash and
    /// without one. Returns the trash and the capabilities it needs.
    fn delete_target(
        ctx: &Ctx,
        account: AccountId,
        place: &Place,
    ) -> (Option<FolderId>, Vec<Capability>) {
        match ctx.role_folder_in(account, FolderRole::Trash) {
            Some(trash) if trash != place.shown => (Some(trash), vec![Capability::MailDelete]),
            _ => (None, vec![Capability::MailExpunge]),
        }
    }

    fn move_capability(ctx: &Ctx, account: AccountId, target: FolderId) -> Capability {
        let to_trash = ctx.folder(target).and_then(|f| f.role) == Some(FolderRole::Trash);
        let capabilities = ctx
            .account(account)
            .map(|a| a.capabilities.clone())
            .unwrap_or_else(holtmail_store::Capabilities::none);
        holtmail_mail_writeback::capability_for(&MailOp::Move { target }, to_trash, &capabilities)
    }

    /// Records the planned writes in one store step. `false` when a held move it replaces was
    /// claimed by the writeback worker meanwhile: nothing is written and the place is busy.
    async fn record(&self, ctx: &Ctx, writes: Writes) -> Result<bool, MailViewError> {
        let now = Timestamp::now();
        let changes: Vec<NewMailChange> = writes.plans.iter().map(|p| p.change.clone()).collect();
        if self
            .store
            .replace_held_mail_changes(&writes.cancel, "replaced", &changes, now)
            .await
            .map_err(store_error)?
            .is_none()
        {
            return Ok(false);
        }
        let mut audits: BTreeMap<(AccountId, &'static str), (AuditKind, Vec<String>)> =
            BTreeMap::new();
        for plan in &writes.plans {
            let entry = audits
                .entry((plan.account, plan.audit.as_str()))
                .or_insert_with(|| (plan.audit, Vec::new()));
            if let Some(message) = plan.change.message_id {
                entry.1.push(message.to_string());
            }
        }
        for ((account, _), (kind, mut targets)) in audits {
            let count = targets.len() as u64;
            if let Some(device) = ctx.viewer.device {
                targets.push(format!("device:{device}"));
            }
            self.store
                .append_audit(&NewAuditEntry {
                    at: now,
                    user: Some(ctx.user()),
                    kind,
                    archive: Some(ctx.viewer.archive),
                    account: Some(account),
                    count,
                    targets,
                })
                .await
                .map_err(store_error)?;
        }
        Ok(true)
    }

    /// The message's flags in `folders` without the viewer's local flags or `\Deleted`: what
    /// an append of it upstream carries along.
    async fn synced_flags(
        &self,
        ctx: &Ctx,
        folders: Option<Vec<FolderId>>,
        message: MessageId,
    ) -> Result<Vec<String>, MailViewError> {
        let mut found = self
            .store
            .effective_flags(&EffectiveFlagsQuery {
                user: ctx.user(),
                messages: vec![message],
                folders,
                local_flags: false,
            })
            .await
            .map_err(store_error)?;
        Ok(found
            .remove(&message)
            .unwrap_or_default()
            .into_iter()
            .filter(|f| !same(f, DELETED_FLAG))
            .collect())
    }

    async fn known(
        &self,
        ctx: &Ctx,
        mailbox: Option<&MailboxId>,
        email: EmailRef,
    ) -> Result<Option<u64>, MailViewError> {
        match mailbox {
            Some(id) => Ok(self.member(ctx, id, email).await?.map(|m| m.modseq)),
            None => {
                let found = self.flags_in(ctx, None, &[email]).await?;
                if found.contains_key(&email) {
                    Ok(Some(self.store.latest_seq().await.map_err(store_error)?))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// A client's own copy of mail it sent through Holtmail: the same user's outbox message of
    /// the last day with this `Message-ID`, not failed or cancelled, for a viewer with
    /// `write:mail` or `send:mail`. Answered with the outbox's
    /// archived message (archived now from the outbox's own bytes if it was not yet, which is
    /// the message the outbox archives later), shown in Sent as a local place when no folder
    /// there holds it. The client's bytes are dropped: nothing is archived twice or appended
    /// upstream, because the identity's sent-copy policy already handled that.
    async fn outbox_copy(
        &self,
        ctx: &Ctx,
        mailbox: &MailboxId,
        raw: &bytes::Bytes,
    ) -> Result<Option<Appended>, MailViewError> {
        let scopes = &ctx.viewer.scopes;
        if !scopes.contains(&DeviceScope::WriteMail) && !scopes.contains(&DeviceScope::SendMail) {
            return Ok(None);
        }
        let Some(rfc_id) = mail_parser::MessageParser::default()
            .parse_headers(raw.as_ref())
            .and_then(|m| m.message_id().map(str::to_owned))
        else {
            return Ok(None);
        };
        let now = Timestamp::now();
        let since = Timestamp::from_millis(now.as_millis() - OUTBOX_COPY_WINDOW_MS);
        let rows = self
            .store
            .outbox_by_rfc_message_id(ctx.viewer.archive, &rfc_id)
            .await
            .map_err(store_error)?;
        let Some(row) = rows.into_iter().rev().find(|r| {
            r.new.user_id == Some(ctx.user())
                && r.new.at >= since
                && !matches!(r.state, OutboxState::Failed | OutboxState::Cancelled)
        }) else {
            return Ok(None);
        };
        let id = match row.message_id {
            Some(id) => id,
            None => {
                let bytes = self
                    .blobs
                    .get(&row.new.blob_id)
                    .await
                    .map_err(unavailable)?
                    .ok_or(MailViewError::NotFound("email"))?;
                self.archiver
                    .archive(ctx.viewer.archive, bytes, None, &[])
                    .await
                    .map_err(unavailable)?
            }
        };
        let created = EmailRef::Message(id);
        let mut uid = self.uid_in(ctx, mailbox, created).await?;
        if uid.is_none() {
            self.store
                .add_local_place(ctx.user(), FolderRole::Sent, id, now)
                .await
                .map_err(store_error)?;
            uid = self.uid_in(ctx, mailbox, created).await?;
        }
        tracing::debug!(outbox = %row.id, message = %id, "client Sent copy matched its outbox message");
        Ok(Some(Appended {
            outcome: Outcome::Applied,
            email: Some(created),
            uid,
        }))
    }

    async fn uid_in(
        &self,
        ctx: &Ctx,
        mailbox: &MailboxId,
        email: EmailRef,
    ) -> Result<Option<u32>, MailViewError> {
        if !mailbox.selectable() {
            return Ok(None);
        }
        Ok(self.member(ctx, mailbox, email).await?.map(|m| m.uid))
    }

    /// The candidate folders of a target mailbox, and the role a refused Sent copy falls back
    /// to.
    fn targets(&self, ctx: &Ctx, mailbox: &MailboxId) -> Vec<FolderId> {
        match mailbox {
            MailboxId::Folder(f) => vec![*f],
            MailboxId::Role(role) => {
                let mut candidates: Vec<(String, FolderId)> = ctx
                    .role_folders(*role)
                    .into_iter()
                    .filter_map(|f| ctx.folder(f))
                    .map(|f| (ctx.account_name(f.account_id), f.id))
                    .collect();
                candidates.sort();
                candidates.into_iter().map(|(_, f)| f).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Why a Sent copy no target takes may not become a local place: the viewer's own locks
    /// for appending (switch, grant, scope) must be open, and every candidate folder may be
    /// closed only by its account (capability or consent). `None` when it may.
    fn local_sent_lock(&self, ctx: &Ctx, targets: &[FolderId]) -> Option<Lock> {
        if let Some(lock) = ctx.viewer_lock(Capability::MailAppend) {
            return Some(lock);
        }
        targets
            .iter()
            .filter_map(|f| ctx.folder(*f).map(|f| f.account_id))
            .filter_map(|a| ctx.lock(a, &[Capability::MailAppend]))
            .find(|lock| !matches!(lock, Lock::Capability | Lock::Consent))
    }

    fn open_target(&self, ctx: &Ctx, targets: &[FolderId]) -> (Option<FolderId>, Option<Refusal>) {
        let account = |f: &FolderId| ctx.folder(*f).map(|f| f.account_id);
        let open = targets
            .iter()
            .copied()
            .find(|f| account(f).is_some_and(|a| ctx.lock(a, &[Capability::MailAppend]).is_none()));
        let first = targets
            .first()
            .and_then(account)
            .and_then(|a| ctx.lock(a, &[Capability::MailAppend]))
            .map(Refusal::Locked);
        (open, first)
    }
}

#[async_trait]
impl MailActions for ArchiveMailView {
    async fn set_flags(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
        change: &FlagChange,
        unchanged_since: Option<u64>,
    ) -> Result<Vec<FlagResult>, MailViewError> {
        valid(change)?;
        let ctx = self.ctx(viewer).await?;
        let entry = mailbox.map(|m| self.selectable(&ctx, m)).transpose()?;
        let folders = entry.as_ref().and_then(|e| e.folders.clone());
        let whole = matches!(mailbox, None | Some(MailboxId::All | MailboxId::Kept));
        let touches_deleted = change
            .add
            .iter()
            .chain(&change.remove)
            .any(|f| same(f, DELETED_FLAG));
        let mut capabilities = vec![Capability::MailFlags];
        if touches_deleted {
            capabilities.push(Capability::MailDelete);
        }
        let mut results = Vec::new();
        for email in dedup(emails) {
            let Some(current) = self.known(&ctx, mailbox, email).await? else {
                results.push(FlagResult {
                    email,
                    outcome: Outcome::Refused(Refusal::NotFound),
                    flags: Vec::new(),
                    modseq: 0,
                });
                continue;
            };
            let before = self
                .flags_in(&ctx, folders.clone(), &[email])
                .await?
                .remove(&email)
                .unwrap_or_default();
            let shown = change.add.iter().all(|a| has(&before, a))
                && change.remove.iter().all(|r| !has(&before, r));
            let outcome = if unchanged_since.is_some_and(|u| current > u) {
                Outcome::Refused(Refusal::Modified)
            } else if shown {
                Outcome::Applied
            } else if let EmailRef::Message(id) = email {
                let places: Vec<Place> = self
                    .places(id)
                    .await?
                    .into_iter()
                    .filter(|p| whole || folders.as_ref().is_some_and(|set| set.contains(&p.shown)))
                    .collect();
                let mut writes = Writes::default();
                let mut first: Option<Refusal> = None;
                let mut need_local = places.is_empty();
                let mut held = false;
                for place in &places {
                    let Some(account) = ctx.folder(place.folder).map(|f| f.account_id) else {
                        continue;
                    };
                    if place.uid.is_none() {
                        need_local = true;
                        continue;
                    }
                    match ctx.lock(account, &capabilities) {
                        None => {
                            held = true;
                            let change = Self::change(
                                &ctx,
                                account,
                                place.folder,
                                id,
                                place.uid,
                                MailOp::Flags {
                                    add: change.add.clone(),
                                    remove: change.remove.clone(),
                                },
                            );
                            Self::plan(&mut writes, &ctx, account, change);
                        }
                        Some(lock) => {
                            need_local = true;
                            first.get_or_insert(Refusal::Locked(lock));
                        }
                    }
                }
                if touches_deleted && first.is_none() && !held {
                    first = Some(Refusal::Cannot);
                }
                let local = need_local && ctx.local && !touches_deleted;
                if held {
                    self.record(&ctx, writes).await?;
                }
                if local {
                    self.store
                        .set_local_flags(
                            ctx.user(),
                            id,
                            &change.add,
                            &change.remove,
                            Timestamp::now(),
                        )
                        .await
                        .map_err(store_error)?;
                }
                outcome(held, local, first)
            } else {
                Outcome::Applied
            };
            let flags = self
                .flags_in(&ctx, folders.clone(), &[email])
                .await?
                .remove(&email)
                .unwrap_or_default();
            let modseq = self.known(&ctx, mailbox, email).await?.unwrap_or(current);
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
        let ctx = self.ctx(viewer).await?;
        let source = self.selectable(&ctx, from)?;
        ctx.entry(to).ok_or(MailViewError::NotFound("mailbox"))?;
        let blocked =
            is_special(to) || matches!(from, MailboxId::All | MailboxId::Kept | MailboxId::Drafts);
        let mut results = Vec::new();
        for email in dedup(emails) {
            if self.member(&ctx, from, email).await?.is_none() {
                results.push(refused(email, Refusal::NotFound));
                continue;
            }
            let Some(id) = email.message().filter(|_| !blocked) else {
                results.push(refused(email, Refusal::Cannot));
                continue;
            };
            let folders = source.folders.clone().unwrap_or_default();
            let places: Vec<Place> = self
                .places(id)
                .await?
                .into_iter()
                .filter(|p| folders.contains(&p.shown))
                .collect();
            let message = self
                .messages(&ctx, &[id])
                .await?
                .into_iter()
                .next()
                .ok_or(MailViewError::NotFound("email"))?;
            let mut writes = Writes::default();
            let mut first: Option<Refusal> = None;
            let mut applied = false;
            for place in &places {
                let Some(account) = ctx.folder(place.shown).map(|f| f.account_id) else {
                    continue;
                };
                let target = match to {
                    MailboxId::Folder(t) => ctx.folder(*t).map(|f| f.id),
                    MailboxId::Role(role) => ctx.role_folder_in(account, *role),
                    _ => None,
                };
                let Some(target) = target else {
                    first.get_or_insert(Refusal::Cannot);
                    continue;
                };
                if target == place.shown {
                    applied = true;
                    continue;
                }
                if place.busy || place.uid.is_none() {
                    first.get_or_insert(Refusal::Cannot);
                    continue;
                }
                let target_account = ctx.folder(target).map(|f| f.account_id);
                if target_account == Some(account) {
                    let capability = Self::move_capability(&ctx, account, target);
                    match ctx.lock(account, &[capability]) {
                        None => Self::plan_move(&mut writes, &ctx, account, id, place, target),
                        Some(lock) => {
                            first.get_or_insert(Refusal::Locked(lock));
                        }
                    }
                    continue;
                }
                let Some(target_account) = target_account else {
                    first.get_or_insert(Refusal::Cannot);
                    continue;
                };
                let (trash, capabilities) = Self::delete_target(&ctx, account, place);
                let lock = ctx
                    .lock(target_account, &[Capability::MailAppend])
                    .or_else(|| ctx.lock(account, &capabilities));
                if let Some(lock) = lock {
                    first.get_or_insert(Refusal::Locked(lock));
                    continue;
                }
                let flags = self.synced_flags(&ctx, Some(vec![place.shown]), id).await?;
                let append = Self::change(
                    &ctx,
                    target_account,
                    target,
                    id,
                    None,
                    MailOp::Append {
                        blob_id: message.blob_id,
                        flags,
                        date: message.received_at.unwrap_or(message.date),
                    },
                );
                Self::plan(&mut writes, &ctx, target_account, append);
                match trash {
                    Some(trash) => Self::plan_move(&mut writes, &ctx, account, id, place, trash),
                    None => Self::plan_expunge(&mut writes, &ctx, account, id, place),
                }
            }
            let mut held = !writes.plans.is_empty() || !writes.cancel.is_empty();
            if held && !self.record(&ctx, writes).await? {
                held = false;
                first = Some(Refusal::Cannot);
            }
            let outcome = outcome(held, applied, first);
            let uid = if outcome.accepted() {
                self.uid_in(&ctx, to, email).await?
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
        let ctx = self.ctx(viewer).await?;
        let source = self.selectable(&ctx, from)?;
        ctx.entry(to).ok_or(MailViewError::NotFound("mailbox"))?;
        let targets = self.targets(&ctx, to);
        let (open, first) = self.open_target(&ctx, &targets);
        let target = open.or(targets.first().copied());
        let mut results = Vec::new();
        for email in dedup(emails) {
            if self.member(&ctx, from, email).await?.is_none() {
                results.push(refused(email, Refusal::NotFound));
                continue;
            }
            let (Some(id), Some(target)) = (email.message().filter(|_| !is_special(to)), target)
            else {
                results.push(refused(email, Refusal::Cannot));
                continue;
            };
            if self.places(id).await?.iter().any(|p| p.shown == target) {
                let uid = self.uid_in(&ctx, to, email).await?;
                results.push(PlaceResult {
                    email,
                    outcome: Outcome::Applied,
                    uid,
                });
                continue;
            }
            if open.is_none() {
                results.push(refused(email, first.unwrap_or(Refusal::Cannot)));
                continue;
            }
            let Some(account) = ctx.folder(target).map(|f| f.account_id) else {
                results.push(refused(email, Refusal::Cannot));
                continue;
            };
            let message = self
                .messages(&ctx, &[id])
                .await?
                .into_iter()
                .next()
                .ok_or(MailViewError::NotFound("email"))?;
            let flags = self.synced_flags(&ctx, source.folders.clone(), id).await?;
            let mut writes = Writes::default();
            let change = Self::change(
                &ctx,
                account,
                target,
                id,
                None,
                MailOp::Append {
                    blob_id: message.blob_id,
                    flags,
                    date: message.received_at.unwrap_or(message.date),
                },
            );
            Self::plan(&mut writes, &ctx, account, change);
            self.record(&ctx, writes).await?;
            let uid = self.uid_in(&ctx, to, email).await?;
            results.push(PlaceResult {
                email,
                outcome: Outcome::Held,
                uid,
            });
        }
        Ok(results)
    }

    async fn remove(
        &self,
        viewer: &Viewer,
        mailbox: Option<&MailboxId>,
        emails: &[EmailRef],
    ) -> Result<Vec<PlaceResult>, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let entry = mailbox.map(|m| self.selectable(&ctx, m)).transpose()?;
        let mut results = Vec::new();
        for email in dedup(emails) {
            if self.known(&ctx, mailbox, email).await?.is_none() {
                results.push(refused(email, Refusal::NotFound));
                continue;
            }
            if matches!(mailbox, Some(MailboxId::All | MailboxId::Kept)) {
                results.push(refused(email, Refusal::Cannot));
                continue;
            }
            let id = match email {
                EmailRef::Draft { draft, .. } => {
                    self.store.delete_draft(draft).await.map_err(store_error)?;
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
            if let Some(role) = entry.as_ref().and_then(|e| e.local_role) {
                applied = self
                    .store
                    .remove_local_place(ctx.user(), role, id, Timestamp::now())
                    .await
                    .map_err(store_error)?;
            }
            let folders = entry.as_ref().and_then(|e| e.folders.clone());
            let places: Vec<Place> = self
                .places(id)
                .await?
                .into_iter()
                .filter(|p| folders.as_ref().is_none_or(|set| set.contains(&p.shown)))
                .collect();
            let mut first = (places.is_empty() && !applied).then_some(Refusal::Cannot);
            let mut writes = Writes::default();
            for place in &places {
                let Some(account) = ctx.folder(place.shown).map(|f| f.account_id) else {
                    continue;
                };
                if place.busy || place.uid.is_none() {
                    first.get_or_insert(Refusal::Cannot);
                    continue;
                }
                let (trash, capabilities) = if mailbox.is_none() {
                    (None, vec![Capability::MailExpunge])
                } else {
                    Self::delete_target(&ctx, account, place)
                };
                if let Some(lock) = ctx.lock(account, &capabilities) {
                    first.get_or_insert(Refusal::Locked(lock));
                    continue;
                }
                match trash {
                    Some(trash) => Self::plan_move(&mut writes, &ctx, account, id, place, trash),
                    None => Self::plan_expunge(&mut writes, &ctx, account, id, place),
                }
            }
            let mut held = !writes.plans.is_empty() || !writes.cancel.is_empty();
            if held && !self.record(&ctx, writes).await? {
                held = false;
                first = Some(Refusal::Cannot);
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
        let limit = self.config.append_limit;
        if email.raw.len() as u64 > limit {
            return Err(MailViewError::TooLarge { limit });
        }
        let ctx = self.ctx(viewer).await?;
        let entry = ctx
            .entry(mailbox)
            .ok_or(MailViewError::NotFound("mailbox"))?;
        let refused = |refusal| Appended {
            outcome: Outcome::Refused(refusal),
            email: None,
            uid: None,
        };
        let received_at = email.received_at.unwrap_or_else(Timestamp::now);
        if matches!(
            mailbox,
            MailboxId::All | MailboxId::Kept | MailboxId::Node(_)
        ) {
            return Ok(refused(Refusal::Cannot));
        }
        if *mailbox == MailboxId::Drafts {
            let blob = self.blobs.put(email.raw).await.map_err(unavailable)?;
            let draft = self
                .store
                .create_raw_draft(viewer.archive, ctx.user(), &blob, received_at)
                .await
                .map_err(store_error)?;
            let created = EmailRef::Draft {
                draft: draft.id,
                version: draft.current.version,
            };
            let uid = self.uid_in(&ctx, mailbox, created).await?;
            return Ok(Appended {
                outcome: Outcome::Applied,
                email: Some(created),
                uid,
            });
        }
        let targets = self.targets(&ctx, mailbox);
        let role = match mailbox {
            MailboxId::Role(role) => Some(*role),
            _ => entry.role,
        };
        if role == Some(FolderRole::Sent)
            && let Some(linked) = self.outbox_copy(&ctx, mailbox, &email.raw).await?
        {
            return Ok(linked);
        }
        let (open, first) = self.open_target(&ctx, &targets);
        let flags: Vec<String> = email
            .flags
            .iter()
            .filter(|f| !same(f, DELETED_FLAG) && !f.trim().is_empty())
            .cloned()
            .collect();
        let local_sent = if open.is_none() && role == Some(FolderRole::Sent) {
            Some(self.local_sent_lock(&ctx, &targets))
        } else {
            None
        };
        if let Some(Some(lock)) = local_sent {
            return Ok(refused(Refusal::Locked(lock)));
        }
        let outcome = match (open, role) {
            (Some(_), _) | (None, Some(FolderRole::Sent)) => {
                let id = self
                    .archiver
                    .archive(viewer.archive, email.raw.clone(), None, &[])
                    .await
                    .map_err(unavailable)?;
                match open {
                    Some(folder) => {
                        if !self.places(id).await?.iter().any(|p| p.shown == folder) {
                            let account = ctx
                                .folder(folder)
                                .map(|f| f.account_id)
                                .ok_or(MailViewError::NotFound("mailbox"))?;
                            let message = self
                                .messages(&ctx, &[id])
                                .await?
                                .into_iter()
                                .next()
                                .ok_or(MailViewError::NotFound("email"))?;
                            let mut writes = Writes::default();
                            let change = Self::change(
                                &ctx,
                                account,
                                folder,
                                id,
                                None,
                                MailOp::Append {
                                    blob_id: message.blob_id,
                                    flags: flags.clone(),
                                    date: received_at,
                                },
                            );
                            Self::plan(&mut writes, &ctx, account, change);
                            self.record(&ctx, writes).await?;
                        }
                        (id, Outcome::Held)
                    }
                    None => {
                        let new = self
                            .store
                            .add_local_place(ctx.user(), FolderRole::Sent, id, Timestamp::now())
                            .await
                            .map_err(store_error)?;
                        if new {
                            let mut targets = vec![id.to_string()];
                            if let Some(device) = viewer.device {
                                targets.push(format!("device:{device}"));
                            }
                            self.store
                                .append_audit(&NewAuditEntry {
                                    at: Timestamp::now(),
                                    user: Some(ctx.user()),
                                    kind: AuditKind::MailAppend,
                                    archive: Some(viewer.archive),
                                    account: None,
                                    count: 1,
                                    targets,
                                })
                                .await
                                .map_err(store_error)?;
                        }
                        if new && ctx.local && !flags.is_empty() {
                            self.store
                                .set_local_flags(ctx.user(), id, &flags, &[], Timestamp::now())
                                .await
                                .map_err(store_error)?;
                        }
                        (id, Outcome::Applied)
                    }
                }
            }
            (None, _) => return Ok(refused(first.unwrap_or(Refusal::Cannot))),
        };
        let (id, outcome) = outcome;
        let created = EmailRef::Message(id);
        let uid = self.uid_in(&ctx, mailbox, created).await?;
        Ok(Appended {
            outcome,
            email: Some(created),
            uid,
        })
    }

    async fn append_limit(&self, viewer: &Viewer) -> Result<u64, MailViewError> {
        self.ctx(viewer).await?;
        Ok(self.config.append_limit)
    }

    async fn subscribe(
        &self,
        viewer: &Viewer,
        mailbox: &MailboxId,
        subscribed: bool,
    ) -> Result<bool, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let entry = ctx
            .entry(mailbox)
            .ok_or(MailViewError::NotFound("mailbox"))?;
        let changed = ctx.subscribed(&entry) != subscribed;
        self.store
            .set_subscription(
                ctx.user(),
                viewer.archive,
                &mailbox.key(),
                subscribed,
                Timestamp::now(),
            )
            .await
            .map_err(store_error)?;
        Ok(changed)
    }
}
