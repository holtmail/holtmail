use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, IdentityId, OutboxId, ThreadId};
use holtmail_mailview::{EmailRef, FolderRole, MailboxId};

pub(crate) fn account(id: ArchiveId) -> String {
    id.to_string()
}

pub(crate) fn parse_account(id: &str) -> Option<ArchiveId> {
    ArchiveId::from_str(id).ok()
}

pub(crate) fn mailbox(id: &MailboxId) -> String {
    match id {
        MailboxId::All => "all".into(),
        MailboxId::Kept => "kept".into(),
        MailboxId::Drafts => "drafts".into(),
        MailboxId::Role(role) => format!("role-{}", role.as_str()),
        MailboxId::Folder(folder) => format!("folder-{folder}"),
        MailboxId::Node(path) => format!("node-{}", URL_SAFE_NO_PAD.encode(path)),
    }
}

pub(crate) fn parse_mailbox(id: &str) -> Option<MailboxId> {
    match id {
        "all" => return Some(MailboxId::All),
        "kept" => return Some(MailboxId::Kept),
        "drafts" => return Some(MailboxId::Drafts),
        _ => {}
    }
    let (kind, rest) = id.split_once('-')?;
    match kind {
        "role" => MailboxId::parse(&format!("role:{rest}")),
        "folder" => MailboxId::parse(&format!("folder:{rest}")),
        "node" => {
            let path = String::from_utf8(URL_SAFE_NO_PAD.decode(rest).ok()?).ok()?;
            MailboxId::parse(&format!("node:{path}"))
        }
        _ => None,
    }
}

pub(crate) fn email(id: &EmailRef) -> String {
    match id {
        EmailRef::Message(message) => format!("M{message}"),
        EmailRef::Draft { draft, version } => format!("D{draft}_{version}"),
    }
}

pub(crate) fn parse_email(id: &str) -> Option<EmailRef> {
    if id.starts_with('M') {
        return EmailRef::parse(id);
    }
    let (draft, version) = id.strip_prefix('D')?.split_once('_')?;
    EmailRef::parse(&format!("D{draft}.{version}"))
}

pub(crate) fn thread(id: ThreadId) -> String {
    format!("T{id}")
}

pub(crate) fn parse_thread(id: &str) -> Option<ThreadId> {
    ThreadId::from_str(id.strip_prefix('T')?).ok()
}

pub(crate) fn identity(id: IdentityId) -> String {
    id.to_string()
}

pub(crate) fn parse_identity(id: &str) -> Option<IdentityId> {
    IdentityId::from_str(id).ok()
}

pub(crate) fn submission(id: OutboxId) -> String {
    format!("S{id}")
}

pub(crate) fn parse_submission(id: &str) -> Option<OutboxId> {
    OutboxId::from_str(id.strip_prefix('S')?).ok()
}

/// A blob a client can download or reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Blob {
    /// Whole bytes by hash: an email's raw form or an upload.
    Hash(BlobId),
    /// One MIME part of an email, decoded.
    Part { email: EmailRef, part: String },
}

pub(crate) fn blob(id: &BlobId) -> String {
    format!("B{id}")
}

pub(crate) fn part_blob(email_ref: &EmailRef, part: &str) -> String {
    format!("P{}_{}", part.replace('.', "-"), email(email_ref))
}

pub(crate) fn parse_blob(id: &str) -> Option<Blob> {
    if let Some(hex) = id.strip_prefix('B') {
        return BlobId::from_str(hex).ok().map(Blob::Hash);
    }
    let (part, email_id) = id.strip_prefix('P')?.split_once('_')?;
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return None;
    }
    Some(Blob::Part {
        email: parse_email(email_id)?,
        part: part.replace('-', "."),
    })
}

pub(crate) fn role_name(role: FolderRole) -> &'static str {
    role.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_core::{DraftId, FolderId, MessageId};

    #[test]
    fn ids_round_trip_inside_the_jmap_alphabet() {
        let valid = |s: &str| {
            !s.is_empty()
                && s.len() <= 255
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        for id in [
            MailboxId::All,
            MailboxId::Kept,
            MailboxId::Drafts,
            MailboxId::Role(FolderRole::Inbox),
            MailboxId::Folder(FolderId::new()),
            MailboxId::Node("Accounts/Work ∕ Ünïcode".into()),
        ] {
            let key = mailbox(&id);
            assert!(valid(&key), "{key}");
            assert_eq!(parse_mailbox(&key), Some(id));
        }
        let draft = EmailRef::Draft {
            draft: DraftId::new(),
            version: 3,
        };
        for e in [EmailRef::Message(MessageId::new()), draft] {
            let key = email(&e);
            assert!(valid(&key), "{key}");
            assert_eq!(parse_email(&key), Some(e));
            let part = part_blob(&e, "2.1");
            assert!(valid(&part), "{part}");
            assert_eq!(
                parse_blob(&part),
                Some(Blob::Part {
                    email: e,
                    part: "2.1".into()
                })
            );
        }
        let hash = BlobId::of(b"x");
        assert_eq!(parse_blob(&blob(&hash)), Some(Blob::Hash(hash)));
        for bad in [
            "",
            "role-nope",
            "folder-x",
            "node-***",
            "Mx",
            "Dx_1",
            "P_M1",
            "Bzz",
        ] {
            assert!(
                parse_mailbox(bad).is_none() && parse_email(bad).is_none(),
                "{bad}"
            );
        }
        assert!(parse_blob("Pa_M1").is_none());
    }
}
