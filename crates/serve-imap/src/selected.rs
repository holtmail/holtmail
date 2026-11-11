//! The selected mailbox as one session has told its client about it.

use std::collections::BTreeSet;

use holtmail_mailview::{EmailRef, MailboxId, Member};

/// What the client knows of the selected mailbox: its members in sequence order and the highest
/// `MODSEQ` it was told about.
#[derive(Debug, Clone)]
pub(crate) struct Selected {
    pub(crate) id: MailboxId,
    pub(crate) read_only: bool,
    pub(crate) uidvalidity: u32,
    /// Members the client knows, ascending by UID; message sequence number `n` is index `n-1`.
    pub(crate) members: Vec<Member>,
    /// The mailbox's `HIGHESTMODSEQ` when it was last caught up.
    pub(crate) known_modseq: u64,
    /// UIDs that left but could not be announced yet (no `EXPUNGE` during `FETCH`, `STORE` or
    /// `SEARCH`).
    pub(crate) deferred: BTreeSet<u32>,
}

impl Selected {
    pub(crate) fn largest_uid(&self) -> u32 {
        self.members.last().map_or(0, |m| m.uid)
    }

    pub(crate) fn exists(&self) -> u32 {
        u32::try_from(self.members.len()).unwrap_or(u32::MAX)
    }

    /// The message sequence number of a UID.
    pub(crate) fn seq(&self, uid: u32) -> Option<u32> {
        self.members
            .binary_search_by_key(&uid, |m| m.uid)
            .ok()
            .map(|i| u32::try_from(i + 1).unwrap_or(u32::MAX))
    }

    pub(crate) fn member(&self, uid: u32) -> Option<&Member> {
        self.members
            .binary_search_by_key(&uid, |m| m.uid)
            .ok()
            .map(|i| &self.members[i])
    }

    pub(crate) fn uid_of_email(&self, email: &EmailRef) -> Option<u32> {
        self.members
            .iter()
            .find(|m| m.email == *email)
            .map(|m| m.uid)
    }

    /// Members named by message sequence ranges, with their numbers.
    pub(crate) fn by_seq(&self, ranges: &[(u32, u32)]) -> Vec<(u32, Member)> {
        self.members
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let seq = u32::try_from(i + 1).ok()?;
                crate::respond::contains(ranges, seq).then(|| (seq, m.clone()))
            })
            .collect()
    }

    /// Members named by UID ranges, with their numbers.
    pub(crate) fn by_uid(&self, ranges: &[(u32, u32)]) -> Vec<(u32, Member)> {
        self.members
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let seq = u32::try_from(i + 1).ok()?;
                crate::respond::contains(ranges, m.uid).then(|| (seq, m.clone()))
            })
            .collect()
    }

    /// UID ranges for message sequence ranges.
    pub(crate) fn uid_ranges(&self, ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let uids: Vec<u32> = self.by_seq(ranges).iter().map(|(_, m)| m.uid).collect();
        crate::respond::ranges(&uids)
    }

    /// Drops the members with these UIDs and returns their sequence numbers as they were,
    /// highest first, so each can be announced as `EXPUNGE` in turn.
    pub(crate) fn expunge(&mut self, uids: &BTreeSet<u32>) -> Vec<u32> {
        let mut seqs = Vec::new();
        for (i, m) in self.members.iter().enumerate() {
            if uids.contains(&m.uid) {
                seqs.push(u32::try_from(i + 1).unwrap_or(u32::MAX));
            }
        }
        self.members.retain(|m| !uids.contains(&m.uid));
        seqs.reverse();
        seqs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_core::MessageId;

    fn member(uid: u32) -> Member {
        Member {
            uid,
            email: EmailRef::Message(MessageId::new()),
            modseq: 1,
        }
    }

    #[test]
    fn numbers_and_expunges() {
        let mut selected = Selected {
            id: MailboxId::All,
            read_only: false,
            uidvalidity: 1,
            members: vec![member(3), member(5), member(9)],
            known_modseq: 1,
            deferred: BTreeSet::new(),
        };
        assert_eq!(selected.seq(5), Some(2));
        assert_eq!(selected.uid_ranges(&[(2, 3)]), vec![(5, 5), (9, 9)]);
        assert_eq!(selected.by_uid(&[(4, 100)]).len(), 2);
        assert_eq!(selected.expunge(&[3, 9].into()), vec![3, 1]);
        assert_eq!(selected.seq(5), Some(1));
    }
}
