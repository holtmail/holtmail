use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, DraftId, FolderId, MessageId, Timestamp, UserId};
use serde::{Deserialize, Serialize};

use crate::{FolderRole, StoreError};

/// Which served mailbox of an archive: a view the mail view names, never an upstream name.
///
/// Every key but [`ServedKey::Drafts`] names a view shared by everyone who reads the archive.
/// Membership, as the store computes it on catch-up:
///
/// - a message is in a folder when it has a live location there that no open (held or
///   sending) `move` or expunging `delete` takes away, or when an open `move` targets the folder
///   or an open `append` of it goes there. An applied `move`, expunging `delete` or `append`
///   keeps counting this way until sync sees the message at its new place (a location there
///   seen after the change was applied) or sees it again at the old one, so a confirmed move
///   never leaves the view in between;
/// - [`ServedKey::Folder`] holds the messages in that folder;
/// - [`ServedKey::Role`] holds the messages in any folder of the archive with that role, and
///   the messages any user keeps as a local place of that role (the mail view shows those to
///   their user only);
/// - [`ServedKey::All`] holds every message of the archive;
/// - [`ServedKey::Kept`] holds the messages of the archive in no folder at all ("Only in
///   Holtmail");
/// - [`ServedKey::Drafts`] holds the newest version of each of the user's drafts in the archive
///   and the messages in the archive's folders with the `drafts` role.
///
/// Local flags are per user and applied on output, never stored here; local places get their
/// UIDs in the role view and are filtered per user on output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServedKey {
    /// Every folder with this role, unified across the archive's accounts.
    Role(FolderRole),
    /// One folder, keyed by id: a rename upstream keeps the view and its UIDs.
    Folder(FolderId),
    /// `All Mail`.
    All,
    /// `Only in Holtmail`.
    Kept,
    /// One user's `Drafts`.
    Drafts(UserId),
}

impl ServedKey {
    /// The view's name in storage and in subscriptions: `role:<role>`, `folder:<id>`, `all`,
    /// `kept` or `drafts` (the user is kept beside it).
    pub fn name(&self) -> String {
        match self {
            Self::Role(role) => format!("role:{}", role.as_str()),
            Self::Folder(folder) => format!("folder:{folder}"),
            Self::All => "all".into(),
            Self::Kept => "kept".into(),
            Self::Drafts(_) => "drafts".into(),
        }
    }

    /// The user of a per-user view.
    pub fn user(&self) -> Option<UserId> {
        match self {
            Self::Drafts(user) => Some(*user),
            _ => None,
        }
    }

    /// The key named by [`ServedKey::name`], with the user a `drafts` key needs; `None` for an
    /// unknown name, a `drafts` name without a user or another name with one.
    pub fn parse(name: &str, user: Option<UserId>) -> Option<Self> {
        let key = match (name, user) {
            ("drafts", Some(user)) => return Some(Self::Drafts(user)),
            (_, Some(_)) => return None,
            ("all", None) => Self::All,
            ("kept", None) => Self::Kept,
            _ => {
                if let Some(role) = name.strip_prefix("role:") {
                    Self::Role(FolderRole::parse(role)?)
                } else {
                    Self::Folder(name.strip_prefix("folder:")?.parse().ok()?)
                }
            }
        };
        Some(key)
    }
}

impl fmt::Display for ServedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// What a served UID stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ServedMember {
    /// An archived message.
    Message(MessageId),
    /// One saved version of a draft, in a drafts view only. A new version is a new member.
    Draft {
        /// The draft.
        draft: DraftId,
        /// Its version.
        version: u32,
    },
}

/// One served mailbox's protocol state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedMailbox {
    /// The archive.
    pub archive_id: ArchiveId,
    /// Which view.
    pub key: ServedKey,
    /// IMAP `UIDVALIDITY`: random and non-zero when the view is created, never changed after.
    pub uidvalidity: u32,
    /// The UID the next member gets; UIDs below it are never handed out again.
    pub uidnext: u32,
    /// IMAP `HIGHESTMODSEQ`: the highest `modseq` of a member or a tombstone, at least 1.
    pub highest_modseq: u64,
    /// The change log `seq` the view has caught up to.
    pub synced_seq: u64,
    /// Tombstones with an `expunged_modseq` at or below this may be gone, so a client that
    /// knows only an older `modseq` needs the full UID set.
    pub lowest_modseq: u64,
    /// Whether every member has a UID. Until then, catch-up leaves the view alone and a client
    /// is told to come back (`NO [UNAVAILABLE]`).
    pub built: bool,
    /// How far the first build is.
    pub progress: BuildProgress,
    /// When the view was created.
    pub created_at: Timestamp,
}

/// How far a view's first build is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildProgress {
    /// Members with a UID.
    pub assigned: u64,
    /// Members the view had when its build started, or `assigned` once built.
    pub total: u64,
}

/// One member with its served UID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedUid {
    /// The UID, unique in the view for good.
    pub uid: u32,
    /// What it stands for.
    pub member: ServedMember,
    /// The `seq` at which the view last saw the member change (IMAP `MODSEQ`).
    pub modseq: u64,
}

/// Which members [`ServeStore::served_uids`] lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServedUidQuery {
    /// Only UIDs above this.
    pub after_uid: u32,
    /// Only members whose `modseq` is above this (IMAP `CHANGEDSINCE`).
    pub changed_since: Option<u64>,
    /// At most this many, capped at 10 000.
    pub limit: u32,
}

/// What [`ServeStore::prune_served`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServedPrune {
    /// Tombstones dropped.
    pub tombstones: u64,
    /// Views whose next catch-up rescans their membership.
    pub stale: u64,
}

/// Served mailboxes and their UIDs, MODSEQs and tombstones (SERVE.md, "UIDs, MODSEQ and
/// states"): protocol state that cannot be derived from the archive.
///
/// UIDs are keyed by member, not by any upstream UID: a member keeps its UID while it stays in
/// the view, whatever happens upstream (a confirmed move, a `UIDVALIDITY` reset, a rename). A
/// UID is never handed out twice in a view, also not to a member that leaves and comes back,
/// which gets a new one. Catch-ups and build steps of one view never run at the same time:
/// each is one compare-and-set on the view's `synced_seq` and `uidnext`.
#[async_trait]
pub trait ServeStore: Send + Sync {
    /// The view, created when missing with a random `UIDVALIDITY`, no members, not built, and
    /// `synced_seq`, `highest_modseq` and `lowest_modseq` at the change log's head (at least
    /// 1). Idempotent. `NotFound` for an unknown archive, a folder of another archive or an
    /// unknown user.
    async fn open_served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        at: Timestamp,
    ) -> Result<ServedMailbox, StoreError>;

    /// The view, or `None` when it was never opened.
    async fn served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
    ) -> Result<Option<ServedMailbox>, StoreError>;

    /// Every opened view of the archive, by name and then user.
    async fn served_mailboxes(&self, archive: ArchiveId) -> Result<Vec<ServedMailbox>, StoreError>;

    /// Gives up to `limit` more members of an unbuilt view a UID, oldest archived first (a
    /// draft by when it was saved), with `modseq` at the view's creation point. When none is
    /// left the view is built and caught up in the same step. On a built view it is
    /// [`ServeStore::catch_up_served_mailbox`]. Each call is short, so a large view is built
    /// in many steps while other writes go on. `NotFound` for a view never opened.
    async fn build_served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        limit: u32,
        at: Timestamp,
    ) -> Result<ServedMailbox, StoreError>;

    /// Brings a built view up to the change log's head: members that joined get the next UIDs
    /// in archived order (a draft by when it was saved), members named by newer rows get the
    /// `seq` of the newest such row as their `modseq`, and members that left become
    /// tombstones with that `seq` as `expunged_modseq`. Rows of one user's own state count only
    /// in that user's views, except local flags, which raise `modseq` in shared views too. A
    /// view marked stale by [`ServeStore::prune_served`], or more than 100 000 rows behind,
    /// rescans its membership and gives every member the head as `modseq`. Returns the view;
    /// an unbuilt view is returned unchanged. Cheap when nothing changed. `NotFound` for a view
    /// never opened.
    async fn catch_up_served_mailbox(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        at: Timestamp,
    ) -> Result<ServedMailbox, StoreError>;

    /// The view's members by UID ascending, as of its last catch-up. `NotFound` for a view
    /// never opened.
    async fn served_uids(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        query: &ServedUidQuery,
    ) -> Result<Vec<ServedUid>, StoreError>;

    /// The UID of each member among `members` that is in the view as of its last catch-up
    /// (for `COPYUID` and `APPENDUID`). `NotFound` for a view never opened.
    async fn served_uid_of(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        members: &[ServedMember],
    ) -> Result<HashMap<ServedMember, u32>, StoreError>;

    /// UIDs that left the view with an `expunged_modseq` above `since`, ascending, above
    /// `after_uid`, at most `limit` (capped at 10 000) (IMAP `VANISHED`). Tombstones at or
    /// below the view's `lowest_modseq` may be missing. `NotFound` for a view never opened.
    async fn served_vanished(
        &self,
        archive: ArchiveId,
        key: ServedKey,
        since: u64,
        after_uid: u32,
        limit: u32,
    ) -> Result<Vec<u32>, StoreError>;

    /// Called with the `seq` through which the change log is pruned: drops every tombstone
    /// with `expunged_modseq` at or below `through`, raising the `lowest_modseq` of each view
    /// that lost one to `through`, and marks every view whose `synced_seq` is below `through`
    /// stale, so its next catch-up rescans instead of reading rows that are gone. UIDs never
    /// change. Idempotent.
    async fn prune_served(&self, through: u64, at: Timestamp) -> Result<ServedPrune, StoreError>;
}
