use std::fmt;
use std::str::FromStr;

use holtmail_core::FolderId;
use holtmail_store::FolderRole;

use crate::message::EmailRef;

/// A served mailbox, keyed by what it shows rather than by its name, so a rename upstream or of
/// an account is a rename to the client and keeps every UID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MailboxId {
    /// Every place with this role in every account of the archive, for an archive with several
    /// accounts: `role:<role>`.
    Role(FolderRole),
    /// One source folder: `folder:<id>`.
    Folder(FolderId),
    /// Every message of the archive, once (`All Mail`): `all`.
    All,
    /// Messages with no place left upstream (`Only in Holtmail`): `kept`.
    Kept,
    /// The viewer's drafts and the upstream drafts folders: `drafts`.
    Drafts,
    /// A level of the tree that holds no mail, only children: `node:<path>`.
    Node(String),
}

impl MailboxId {
    /// The key used in storage (served mailboxes, subscriptions) and as JMAP mailbox id.
    pub fn key(&self) -> String {
        match self {
            Self::Role(role) => format!("role:{}", role.as_str()),
            Self::Folder(id) => format!("folder:{id}"),
            Self::All => "all".into(),
            Self::Kept => "kept".into(),
            Self::Drafts => "drafts".into(),
            Self::Node(path) => format!("node:{path}"),
        }
    }

    /// The mailbox named by [`MailboxId::key`], or `None`.
    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "all" => return Some(Self::All),
            "kept" => return Some(Self::Kept),
            "drafts" => return Some(Self::Drafts),
            _ => {}
        }
        let (kind, rest) = key.split_once(':')?;
        match kind {
            "role" => role(rest).map(Self::Role),
            "folder" => FolderId::from_str(rest).ok().map(Self::Folder),
            "node" if !rest.is_empty() => Some(Self::Node(rest.into())),
            _ => None,
        }
    }

    /// Whether it can hold mail: everything but a placeholder.
    pub fn selectable(&self) -> bool {
        !matches!(self, Self::Node(_))
    }
}

fn role(name: &str) -> Option<FolderRole> {
    [
        FolderRole::Inbox,
        FolderRole::Sent,
        FolderRole::Drafts,
        FolderRole::Trash,
        FolderRole::Junk,
        FolderRole::Archive,
        FolderRole::All,
        FolderRole::Flagged,
        FolderRole::Important,
    ]
    .into_iter()
    .find(|r| r.as_str() == name)
}

impl fmt::Display for MailboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}

/// What the viewer may do in a mailbox, by the five locks and local flags. A right is given
/// when at least one place behind the mailbox allows it; a single message may still be refused,
/// and the result of the action says so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MailboxRights {
    /// Members can be listed and fetched.
    pub read: bool,
    /// Emails can be appended, copied or moved in.
    pub add: bool,
    /// Emails can be removed or moved out, and `\Deleted` set.
    pub remove: bool,
    /// `\Seen` can be changed, upstream or as a local flag.
    pub set_seen: bool,
    /// Other flags and keywords can be changed, upstream or as local flags.
    pub set_keywords: bool,
    /// Some flag changes here are kept as the viewer's local flags.
    pub local_flags: bool,
}

/// One mailbox of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    /// Id.
    pub id: MailboxId,
    /// Its name as levels from the top, e.g. `["Accounts", "Work", "Projects"]`. No level
    /// contains `/`: one from upstream shows as `∕` (U+2215). The inbox is `["INBOX"]`.
    pub path: Vec<String>,
    /// The role it carries (`SPECIAL-USE`, JMAP `role`); `All Mail` carries
    /// [`FolderRole::All`].
    pub role: Option<FolderRole>,
    /// Whether it holds mail; `false` for a placeholder.
    pub selectable: bool,
    /// Whether the viewer is subscribed, by their choice or by default.
    pub subscribed: bool,
    /// What the viewer may do in it.
    pub rights: MailboxRights,
}

/// Counts and served-UID state of a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxStatus {
    /// Members.
    pub messages: u32,
    /// Members whose effective flags lack `\Seen`.
    pub unseen: u32,
    /// Changes only when the served tables are lost; never 0.
    pub uidvalidity: u32,
    /// Greater than every UID ever given in the mailbox.
    pub uidnext: u32,
    /// The highest `MODSEQ` of a member or of a member's leaving; never goes down.
    pub highest_modseq: u64,
}

/// A member of a mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its served UID.
    pub uid: u32,
    /// The email.
    pub email: EmailRef,
    /// The change log `seq` at which the mailbox last saw it change.
    pub modseq: u64,
}

/// A member with its flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberFlags {
    /// The member.
    pub member: Member,
    /// Its effective flags in the mailbox.
    pub flags: Vec<String>,
}

/// A mailbox with all its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The mailbox.
    pub mailbox: Mailbox,
    /// Its counts.
    pub status: MailboxStatus,
    /// Its members, ascending by UID.
    pub members: Vec<Member>,
}

/// What changed in a mailbox after a `MODSEQ`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxChanges {
    /// The mailbox's counts now.
    pub status: MailboxStatus,
    /// Members changed after it, ascending by UID.
    pub changed: Vec<MemberFlags>,
    /// UIDs that left after it, ascending.
    pub vanished: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for id in [
            MailboxId::Role(FolderRole::Inbox),
            MailboxId::Folder(FolderId::new()),
            MailboxId::All,
            MailboxId::Kept,
            MailboxId::Drafts,
            MailboxId::Node("Accounts/Work".into()),
        ] {
            assert_eq!(MailboxId::parse(&id.key()), Some(id.clone()), "{id}");
        }
        for bad in ["", "role:", "role:nope", "folder:x", "node:", "inbox"] {
            assert_eq!(MailboxId::parse(bad), None, "{bad}");
        }
    }
}
