//! The served mail view over the archive: [`MailView`] and [`MailActions`] over the store, the
//! blob store, the search index and `holtmail-access`.
//!
//! Every read comes from the store's served tables, effective flags and messages, and from the
//! archived bytes; every write is a held mail change, a local flag, a local place, a raw draft
//! or a subscription, decided by the same locks the web API uses. Nothing here talks to a mail
//! server, and nothing deletes a message from the archive.

mod actions;
mod changes;
mod layout;
mod mime;
mod search;
mod view;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use holtmail_access::{Access, Denial, MAIL_WRITE_CAPABILITIES, Scopes, WriteSwitches, decide};
use holtmail_blob::BlobStore;
use holtmail_core::{AccountId, DeviceScope, FolderId, MessageId, Timestamp, UserId};
use holtmail_index::SearchIndex;
use holtmail_mailview::{EmailRef, Lock, MailViewError, MailboxId, Member, Part, Viewer};
use holtmail_outbox::Archiver;
use holtmail_store::{
    Account, AccountKind, Capabilities, Capability, Folder, FolderRole, GrantRole, MailOp,
    ServedKey, ServedMailbox, ServedMember, ServedUid, ServedUidQuery, Store, StoreError,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub use layout::Entry;

/// How the view behaves; the defaults are SERVE.md's.
#[derive(Debug, Clone, Copy)]
pub struct ViewConfig {
    /// The `[write]` instance switches.
    pub switches: WriteSwitches,
    /// `[serve] local_flags`: keep flag changes that cannot go upstream as the user's local
    /// flags.
    pub local_flags: bool,
    /// The largest message `append` takes (`APPENDLIMIT`).
    pub append_limit: u64,
    /// How many members a read may give UIDs to by itself when a view was never built; a view
    /// with more answers [`MailViewError::Building`] until the background build is done.
    pub inline_build: u32,
    /// How many index matches one search text may have, after narrowing to the mailbox's
    /// dates, before the search fails with [`MailViewError::TooMany`] instead of answering
    /// incompletely. At most [`holtmail_index::MAX_MATCHES`].
    pub search_cap: u32,
    /// How many parsed MIME structures are kept, by blob, so a `BODYSTRUCTURE` or part fetch of
    /// a message read before needs no read or parse of its bytes.
    pub structure_cache: usize,
    /// How many expensive steps (index searches, structure parses) one user runs at once; more
    /// wait their turn.
    pub expensive_per_user: usize,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            switches: WriteSwitches::default(),
            local_flags: true,
            append_limit: 50 * 1024 * 1024,
            inline_build: 5_000,
            search_cap: holtmail_index::MAX_MATCHES,
            structure_cache: 10_000,
            expensive_per_user: 2,
        }
    }
}

/// The mail view of every archive of the instance, through each viewer's eyes. Cheap to share;
/// besides its handles it keeps only derived data that its blob fully determines (parsed MIME
/// structures) and per-user turns for expensive work, so every answer reflects the store.
pub struct ArchiveMailView {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    index: Arc<dyn SearchIndex>,
    archiver: Arc<dyn Archiver>,
    access: Access,
    config: ViewConfig,
    structures: Mutex<Structures>,
    turns: Mutex<HashMap<UserId, Arc<Semaphore>>>,
}

/// Parsed structures by blob, the oldest dropped first.
#[derive(Default)]
struct Structures {
    parts: HashMap<holtmail_blob::BlobId, Part>,
    order: VecDeque<holtmail_blob::BlobId>,
}

impl ArchiveMailView {
    /// A view over these handles. `archiver` archives the bytes of client appends.
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        index: Arc<dyn SearchIndex>,
        archiver: Arc<dyn Archiver>,
        config: ViewConfig,
    ) -> Self {
        Self {
            access: Access::new(store.clone(), config.switches),
            store,
            blobs,
            index,
            archiver,
            config,
            structures: Mutex::new(Structures::default()),
            turns: Mutex::new(HashMap::new()),
        }
    }

    /// The configuration in force.
    pub fn config(&self) -> ViewConfig {
        self.config
    }

    /// Opens every view of the viewer's layout and gives up to `budget` more members of views
    /// that are not built yet their UIDs, so reads later find them built. Safe to run beside
    /// reads and writes: each step is one short store call. Errors like [`MailView`] does for a
    /// viewer who may not read the archive.
    ///
    /// [`MailView`]: holtmail_mailview::MailView
    pub async fn prepare(&self, viewer: &Viewer, budget: u32) -> Result<Progress, MailViewError> {
        let ctx = self.ctx(viewer).await?;
        let archive = viewer.archive;
        let mut progress = Progress::default();
        let mut budget = budget;
        for key in ctx.layout().into_iter().filter_map(|e| e.key) {
            let mut view = match self
                .store
                .served_mailbox(archive, key)
                .await
                .map_err(store_error)?
            {
                Some(view) => view,
                None => self
                    .store
                    .open_served_mailbox(archive, key, Timestamp::now())
                    .await
                    .map_err(store_error)?,
            };
            while !view.built && budget > 0 {
                let step = budget.min(1_000);
                view = self
                    .store
                    .build_served_mailbox(archive, key, step, Timestamp::now())
                    .await
                    .map_err(store_error)?;
                budget -= step;
            }
            progress.views += 1;
            progress.built += u32::from(view.built);
            progress.assigned += view.progress.assigned;
            progress.total += view.progress.total.max(view.progress.assigned);
        }
        Ok(progress)
    }
}

/// How far the views of one viewer's layout are built.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Views in the layout.
    pub views: u32,
    /// Those of them that are built.
    pub built: u32,
    /// Members with a UID, over every view.
    pub assigned: u64,
    /// Members in all, over every view.
    pub total: u64,
}

impl Progress {
    /// Whether every view is built.
    pub fn done(&self) -> bool {
        self.built == self.views
    }
}

fn lock_of(denial: Denial) -> Lock {
    match denial {
        Denial::Switch => Lock::Switch,
        Denial::Capability => Lock::Capability,
        Denial::Grant => Lock::Grant,
        Denial::Scope => Lock::Scope,
    }
}

pub(crate) fn unavailable(error: impl std::fmt::Display) -> MailViewError {
    MailViewError::Unavailable(error.to_string())
}

pub(crate) fn store_error(error: StoreError) -> MailViewError {
    match error {
        StoreError::NotFound(what) => MailViewError::NotFound(what),
        StoreError::Invalid(reason) => MailViewError::Invalid(reason),
        other => unavailable(other),
    }
}

pub(crate) fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub(crate) fn has(flags: &[String], flag: &str) -> bool {
    flags.iter().any(|f| same(f, flag))
}

pub(crate) struct AccountInfo {
    pub(crate) account: Account,
    pub(crate) capabilities: Capabilities,
}

/// What one call knows about the viewer and the archive.
pub(crate) struct Ctx {
    pub(crate) viewer: Viewer,
    pub(crate) role: Option<GrantRole>,
    pub(crate) scopes: Scopes,
    pub(crate) accounts: Vec<AccountInfo>,
    pub(crate) folders: Vec<Folder>,
    pub(crate) subscriptions: HashMap<String, bool>,
    pub(crate) local: bool,
    pub(crate) switches: WriteSwitches,
}

impl Ctx {
    pub(crate) fn multi(&self) -> bool {
        self.accounts.len() > 1
    }

    pub(crate) fn user(&self) -> UserId {
        self.viewer.user
    }

    pub(crate) fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub(crate) fn account(&self, id: AccountId) -> Option<&AccountInfo> {
        self.accounts.iter().find(|a| a.account.id == id)
    }

    pub(crate) fn role_folders(&self, role: FolderRole) -> Vec<FolderId> {
        self.folders
            .iter()
            .filter(|f| f.role == Some(role))
            .map(|f| f.id)
            .collect()
    }

    pub(crate) fn role_folder_in(&self, account: AccountId, role: FolderRole) -> Option<FolderId> {
        self.folders
            .iter()
            .find(|f| f.account_id == account && f.role == Some(role))
            .map(|f| f.id)
    }

    /// The first closed lock for `capabilities` on the account, in `holtmail-access` order.
    pub(crate) fn lock(&self, account: AccountId, capabilities: &[Capability]) -> Option<Lock> {
        let Some(info) = self.account(account) else {
            return Some(Lock::Capability);
        };
        if info.account.kind == AccountKind::Import {
            return Some(Lock::Capability);
        }
        for capability in capabilities {
            if let Err(denial) = decide(
                self.switches,
                self.role,
                &info.capabilities,
                &self.scopes,
                *capability,
            ) {
                return Some(lock_of(denial));
            }
        }
        if capabilities
            .iter()
            .any(|c| MAIL_WRITE_CAPABILITIES.contains(c))
            && holtmail_access::write_consent_missing(&info.account)
        {
            return Some(Lock::Consent);
        }
        None
    }

    /// The first closed lock for `capability` that belongs to the viewer and the instance
    /// rather than an account: the switch, the grant and the scope.
    pub(crate) fn viewer_lock(&self, capability: Capability) -> Option<Lock> {
        let mut every = Capabilities::none();
        every.set(capability, true);
        every.set(Capability::MailDelete, true);
        decide(self.switches, self.role, &every, &self.scopes, capability)
            .err()
            .map(lock_of)
    }

    pub(crate) fn account_name(&self, account: AccountId) -> String {
        self.account(account)
            .map(|a| a.account.name.clone())
            .unwrap_or_default()
    }
}

/// One place of a message as the view sees it: a live location, with held moves applied.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    /// Where the location is recorded.
    pub(crate) folder: FolderId,
    pub(crate) uid: Option<u64>,
    /// Where the message shows, after an open move.
    pub(crate) shown: FolderId,
    /// The open move that took it there, when it was still held.
    pub(crate) held_move: Option<holtmail_core::MailChangeId>,
    /// A move that is being sent or waits for sync: the place cannot be planned on.
    pub(crate) busy: bool,
}

impl ArchiveMailView {
    /// Checks the viewer may read the archive and loads what every call needs.
    pub(crate) async fn ctx(&self, viewer: &Viewer) -> Result<Ctx, MailViewError> {
        let hidden = MailViewError::NotFound("archive");
        if !viewer.scopes.contains(&DeviceScope::Read) {
            return Err(hidden);
        }
        let store = &self.store;
        if store
            .get_archive(viewer.archive)
            .await
            .map_err(store_error)?
            .is_none()
        {
            return Err(hidden);
        }
        let Some(user) = store.get_user(viewer.user).await.map_err(store_error)? else {
            return Err(hidden);
        };
        let Some(role) = self
            .access
            .role_in(&user, viewer.archive)
            .await
            .map_err(store_error)?
        else {
            return Err(hidden);
        };
        let mut accounts = Vec::new();
        for account in store
            .list_accounts(viewer.archive)
            .await
            .map_err(store_error)?
        {
            let capabilities = store
                .account_capabilities(account.id)
                .await
                .map_err(store_error)?;
            accounts.push(AccountInfo {
                account,
                capabilities,
            });
        }
        accounts.sort_by(|a, b| {
            a.account
                .name
                .cmp(&b.account.name)
                .then(a.account.id.cmp(&b.account.id))
        });
        let folders = store
            .list_folders(viewer.archive)
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|f| f.folder)
            .collect();
        let subscriptions = store
            .subscriptions(viewer.user, viewer.archive)
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|s| (s.mailbox, s.subscribed))
            .collect();
        Ok(Ctx {
            viewer: viewer.clone(),
            role: Some(role),
            scopes: Scopes::of_device(&viewer.scopes),
            accounts,
            folders,
            subscriptions,
            local: self.config.local_flags,
            switches: self.config.switches,
        })
    }

    /// The view behind a selectable mailbox, built when small enough and caught up.
    pub(crate) async fn served(
        &self,
        ctx: &Ctx,
        key: ServedKey,
    ) -> Result<ServedMailbox, MailViewError> {
        let archive = ctx.viewer.archive;
        let store = &self.store;
        let mut view = match store
            .served_mailbox(archive, key)
            .await
            .map_err(store_error)?
        {
            Some(view) => view,
            None => store
                .open_served_mailbox(archive, key, Timestamp::now())
                .await
                .map_err(store_error)?,
        };
        let mut budget = self.config.inline_build;
        while !view.built && budget > 0 {
            let step = budget.min(1_000);
            view = store
                .build_served_mailbox(archive, key, step, Timestamp::now())
                .await
                .map_err(store_error)?;
            budget -= step;
        }
        if !view.built {
            return Err(MailViewError::Building {
                done: view.progress.assigned,
                total: view.progress.total,
            });
        }
        store
            .catch_up_served_mailbox(archive, key, Timestamp::now())
            .await
            .map_err(store_error)
    }

    /// Every member of the view as of its last catch-up, by UID.
    pub(crate) async fn uids(
        &self,
        ctx: &Ctx,
        key: ServedKey,
        changed_since: Option<u64>,
    ) -> Result<Vec<ServedUid>, MailViewError> {
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            let page = self
                .store
                .served_uids(
                    ctx.viewer.archive,
                    key,
                    &ServedUidQuery {
                        after_uid: after,
                        changed_since,
                        limit: 10_000,
                    },
                )
                .await
                .map_err(store_error)?;
            let Some(last) = page.last() else {
                break;
            };
            after = last.uid;
            let full = page.len() == 10_000;
            out.extend(page);
            if !full {
                break;
            }
        }
        Ok(out)
    }

    /// The members of `entry` the viewer sees: drafts that are not raw drafts of the viewer, and
    /// other users' local places, are left out.
    pub(crate) async fn visible(
        &self,
        ctx: &Ctx,
        entry: &Entry,
        uids: Vec<ServedUid>,
    ) -> Result<Vec<Member>, MailViewError> {
        let hidden = self.hidden(ctx, entry, &uids).await?;
        Ok(uids
            .into_iter()
            .filter(|u| !hidden.contains(&u.member))
            .map(|u| Member {
                uid: u.uid,
                email: email_of(u.member),
                modseq: u.modseq,
            })
            .collect())
    }

    async fn hidden(
        &self,
        ctx: &Ctx,
        entry: &Entry,
        uids: &[ServedUid],
    ) -> Result<HashSet<ServedMember>, MailViewError> {
        let mut hidden = HashSet::new();
        for uid in uids {
            if let ServedMember::Draft { draft, version } = uid.member
                && !self.raw_draft(ctx, draft, version).await?.is_some()
            {
                hidden.insert(uid.member);
            }
        }
        let Some(role) = entry.local_role else {
            return Ok(hidden);
        };
        let others = self.others_local(ctx, role).await?;
        if others.is_empty() {
            return Ok(hidden);
        }
        let present: HashSet<MessageId> = uids
            .iter()
            .filter_map(|u| match u.member {
                ServedMember::Message(m) => Some(m),
                ServedMember::Draft { .. } => None,
            })
            .collect();
        let role_folders: HashSet<FolderId> = ctx.role_folders(role).into_iter().collect();
        for message in others.into_iter().filter(|m| present.contains(m)) {
            let places = self.places(message).await?;
            if !places.iter().any(|p| role_folders.contains(&p.shown)) {
                hidden.insert(ServedMember::Message(message));
            }
        }
        Ok(hidden)
    }

    /// Messages other users keep as local places of `role` in the archive and the viewer does
    /// not.
    async fn others_local(
        &self,
        ctx: &Ctx,
        role: FolderRole,
    ) -> Result<HashSet<MessageId>, MailViewError> {
        let archive = ctx.viewer.archive;
        let mut others = HashSet::new();
        let mut mine = HashSet::new();
        for user in self.store.list_users().await.map_err(store_error)? {
            let places = self
                .store
                .local_places(user.id, archive, Some(role))
                .await
                .map_err(store_error)?;
            let set = if user.id == ctx.user() {
                &mut mine
            } else {
                &mut others
            };
            set.extend(places.into_iter().map(|p| p.message_id));
        }
        Ok(others.difference(&mine).copied().collect())
    }

    /// The viewer's raw draft at this version, when it is the draft's newest: its bytes' blob.
    /// Waits for one of the user's turns at expensive work; the turn ends when dropped.
    pub(crate) async fn turn(&self, user: UserId) -> OwnedSemaphorePermit {
        let semaphore = self
            .turns
            .lock()
            .expect("turns")
            .entry(user)
            .or_insert_with(|| Arc::new(Semaphore::new(self.config.expensive_per_user.max(1))))
            .clone();
        semaphore
            .acquire_owned()
            .await
            .expect("turn semaphores are never closed")
    }

    pub(crate) fn cached_structure(&self, blob: &holtmail_blob::BlobId) -> Option<Part> {
        self.structures
            .lock()
            .expect("structures")
            .parts
            .get(blob)
            .cloned()
    }

    pub(crate) fn keep_structure(&self, blob: holtmail_blob::BlobId, part: &Part) {
        let cap = self.config.structure_cache;
        if cap == 0 {
            return;
        }
        let mut cache = self.structures.lock().expect("structures");
        if cache.parts.insert(blob, part.clone()).is_none() {
            cache.order.push_back(blob);
        }
        while cache.order.len() > cap {
            if let Some(old) = cache.order.pop_front() {
                cache.parts.remove(&old);
            }
        }
    }

    pub(crate) async fn raw_draft(
        &self,
        ctx: &Ctx,
        draft: holtmail_core::DraftId,
        version: u32,
    ) -> Result<Option<(holtmail_blob::BlobId, Timestamp)>, MailViewError> {
        let Some(found) = self.store.get_draft(draft).await.map_err(store_error)? else {
            return Ok(None);
        };
        if found.user_id != ctx.user()
            || found.archive_id != ctx.viewer.archive
            || found.current.version != version
        {
            return Ok(None);
        }
        Ok(found.current.raw.map(|blob| (blob, found.current.saved_at)))
    }

    /// The message's places: live locations with open moves and expunges applied, and open
    /// appends.
    pub(crate) async fn places(&self, message: MessageId) -> Result<Vec<Place>, MailViewError> {
        let store = &self.store;
        let locations = store.locations(message).await.map_err(store_error)?;
        let changes = store
            .pending_mail_changes(message)
            .await
            .map_err(store_error)?;
        let mut places = Vec::new();
        for location in locations.iter().filter(|l| l.removed_at.is_none()) {
            let mut place = Place {
                folder: location.folder_id,
                uid: location.uid,
                shown: location.folder_id,
                held_move: None,
                busy: false,
            };
            let mut gone = false;
            for change in &changes {
                let new = &change.new;
                if new.folder_id != location.folder_id || new.uid != location.uid {
                    continue;
                }
                match &new.op {
                    MailOp::Move { target } => {
                        place.shown = *target;
                        if change.state == holtmail_store::MailChangeState::Held {
                            place.held_move = Some(change.id);
                        } else {
                            place.busy = true;
                        }
                    }
                    MailOp::Delete { expunge: true } => gone = true,
                    _ => {}
                }
            }
            if !gone {
                places.push(place);
            }
        }
        for change in &changes {
            if let MailOp::Append { .. } = change.new.op
                && !places.iter().any(|p| p.shown == change.new.folder_id)
            {
                places.push(Place {
                    folder: change.new.folder_id,
                    uid: None,
                    shown: change.new.folder_id,
                    held_move: None,
                    busy: false,
                });
            }
        }
        Ok(places)
    }
}

pub(crate) fn email_of(member: ServedMember) -> EmailRef {
    match member {
        ServedMember::Message(m) => EmailRef::Message(m),
        ServedMember::Draft { draft, version } => EmailRef::Draft { draft, version },
    }
}

pub(crate) fn member_of(email: EmailRef) -> ServedMember {
    match email {
        EmailRef::Message(m) => ServedMember::Message(m),
        EmailRef::Draft { draft, version } => ServedMember::Draft { draft, version },
    }
}

pub(crate) fn not_selectable() -> MailViewError {
    MailViewError::NotFound("mailbox")
}

pub(crate) fn is_special(id: &MailboxId) -> bool {
    matches!(
        id,
        MailboxId::All | MailboxId::Kept | MailboxId::Drafts | MailboxId::Node(_)
    )
}
