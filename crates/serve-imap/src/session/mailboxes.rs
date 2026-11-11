//! Mailbox commands: the tree, status, subscriptions, selecting and catching up.

use std::collections::BTreeSet;

use holtmail_mailview::{MailViewError, Mailbox as ViewMailbox};
use imap_types::command::SelectParameter;
use imap_types::core::{QuotedChar, Tag, Vec1};
use imap_types::extensions::enable::CapabilityEnable;
use imap_types::flag::{Flag, FlagNameAttribute};
use imap_types::mailbox::{ListMailbox, Mailbox};
use imap_types::response::{Code, Data, Status};
use imap_types::status::{StatusDataItem, StatusDataItemName};

use crate::conn::{Conn, Wire};
use crate::names;
use crate::respond::{self, Kind, code, nz, nz64, tagged};
use crate::selected::Selected;

use super::{KEYWORDS, Session, attribute, flags_fetch, mailbox_name, view_error, wire_mailbox};

impl Session {
    pub(super) fn enable<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        capabilities: Vec1<CapabilityEnable<'static>>,
    ) {
        let Some(auth) = &mut self.auth else {
            return;
        };
        let mut enabled = Vec::new();
        for capability in capabilities.as_ref() {
            let name = capability.to_string().to_ascii_uppercase();
            match name.as_str() {
                "CONDSTORE" if !auth.condstore => {
                    auth.condstore = true;
                    enabled.push(CapabilityEnable::CondStore);
                }
                "QRESYNC" if !auth.qresync => {
                    auth.qresync = true;
                    auth.condstore = true;
                    enabled.push(capability.clone());
                }
                _ => {}
            }
        }
        conn.data(Data::Enabled {
            capabilities: enabled,
        });
        conn.status(tagged(tag, Kind::Ok, None, "ENABLE completed"));
    }

    pub(super) async fn tree(&self) -> Result<Vec<ViewMailbox>, MailViewError> {
        let auth = self.auth.as_ref().expect("signed in");
        self.shared.view.tree(&auth.viewer).await
    }

    pub(super) async fn resolve(
        &self,
        tag: &Tag<'static>,
        mailbox: &Mailbox<'_>,
    ) -> Result<ViewMailbox, Status<'static>> {
        let tree = self.tree().await.map_err(|e| view_error(tag, e))?;
        let wanted = mailbox_name(mailbox);
        names::find(&tree, &wanted)
            .cloned()
            .ok_or_else(|| tagged(tag, Kind::No, Some(code("NONEXISTENT")), "No such mailbox"))
    }

    pub(super) async fn list<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        reference: &Mailbox<'_>,
        pattern: &ListMailbox<'_>,
        lsub: bool,
    ) {
        let delimiter = QuotedChar::try_from(names::DELIMITER).ok();
        let reference = match reference {
            Mailbox::Inbox => "INBOX".to_string(),
            Mailbox::Other(other) => crate::utf7::decode(&String::from_utf8_lossy(other.as_ref())),
        };
        let pattern = match pattern {
            ListMailbox::Token(token) => {
                crate::utf7::decode(&String::from_utf8_lossy(token.as_ref()))
            }
            ListMailbox::String(string) => {
                crate::utf7::decode(&String::from_utf8_lossy(string.as_ref()))
            }
        };
        if pattern.is_empty() && !lsub {
            conn.data(Data::List {
                items: vec![FlagNameAttribute::Noselect],
                delimiter,
                mailbox: Mailbox::try_from(String::new()).unwrap_or(Mailbox::Inbox),
            });
            conn.status(tagged(tag, Kind::Ok, None, "LIST completed"));
            return;
        }
        let tree = match self.tree().await {
            Ok(tree) => tree,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        let full = format!("{reference}{pattern}");
        for mailbox in &tree {
            let name = names::name(mailbox);
            if (lsub && !mailbox.subscribed) || !names::matches(&full, &name) {
                continue;
            }
            let mut items = Vec::new();
            if !mailbox.selectable {
                items.push(FlagNameAttribute::Noselect);
            }
            if !lsub {
                items.push(attribute(if names::has_children(&tree, mailbox) {
                    "HasChildren"
                } else {
                    "HasNoChildren"
                }));
                if let Some(special) = mailbox.role.and_then(names::special_use) {
                    items.push(attribute(special));
                }
            }
            let mailbox = wire_mailbox(&name);
            conn.data(if lsub {
                Data::Lsub {
                    items,
                    delimiter,
                    mailbox,
                }
            } else {
                Data::List {
                    items,
                    delimiter,
                    mailbox,
                }
            });
        }
        let text = if lsub {
            "LSUB completed"
        } else {
            "LIST completed"
        };
        conn.status(tagged(tag, Kind::Ok, None, text));
    }

    pub(super) async fn status<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        mailbox: &Mailbox<'_>,
        items: &[StatusDataItemName],
    ) {
        let target = match self.resolve(tag, mailbox).await {
            Ok(target) => target,
            Err(status) => return conn.status(status),
        };
        let auth = self.auth.as_mut().expect("signed in");
        let status = match self.shared.view.status(&auth.viewer, &target.id).await {
            Ok(status) => status,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                StatusDataItemName::Messages => out.push(StatusDataItem::Messages(status.messages)),
                StatusDataItemName::Recent => out.push(StatusDataItem::Recent(0)),
                StatusDataItemName::UidNext => {
                    out.push(StatusDataItem::UidNext(nz(status.uidnext)))
                }
                StatusDataItemName::UidValidity => {
                    out.push(StatusDataItem::UidValidity(nz(status.uidvalidity)))
                }
                StatusDataItemName::Unseen => out.push(StatusDataItem::Unseen(status.unseen)),
                StatusDataItemName::HighestModSeq => {
                    auth.condstore = true;
                    out.push(StatusDataItem::HighestModSeq(status.highest_modseq));
                }
                StatusDataItemName::Deleted | StatusDataItemName::DeletedStorage => {}
            }
        }
        conn.data(Data::Status {
            mailbox: wire_mailbox(&names::name(&target)),
            items: out.into(),
        });
        conn.status(tagged(tag, Kind::Ok, None, "STATUS completed"));
    }

    pub(super) async fn subscribe<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        mailbox: &Mailbox<'_>,
        on: bool,
    ) {
        let target = match self.resolve(tag, mailbox).await {
            Ok(target) => target,
            Err(status) => return conn.status(status),
        };
        let auth = self.auth.as_ref().expect("signed in");
        match self
            .shared
            .actions
            .subscribe(&auth.viewer, &target.id, on)
            .await
        {
            Ok(_) => conn.status(tagged(tag, Kind::Ok, None, "Done")),
            Err(error) => conn.status(view_error(tag, error)),
        }
    }

    pub(super) async fn select<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        mailbox: &Mailbox<'_>,
        parameters: Vec<SelectParameter>,
        examine: bool,
    ) {
        let qresync_enabled = self.auth.as_ref().is_some_and(|a| a.qresync);
        if let Some(auth) = &mut self.auth
            && auth.selected.take().is_some()
        {
            conn.status(respond::untagged_ok(
                Some(Code::Closed),
                "Previous mailbox closed",
            ));
        }
        let mut resync = None;
        for parameter in parameters {
            match parameter {
                SelectParameter::CondStore => {}
                SelectParameter::QResync {
                    uid_validity,
                    mod_sequence_value,
                    known_uids,
                    ..
                } => {
                    if !qresync_enabled {
                        return conn.status(tagged(tag, Kind::Bad, None, "ENABLE QRESYNC first"));
                    }
                    resync = Some((uid_validity.get(), mod_sequence_value.get(), known_uids));
                }
            }
        }
        let target = match self.resolve(tag, mailbox).await {
            Ok(target) => target,
            Err(status) => return conn.status(status),
        };
        if !target.selectable {
            return conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                "This level holds no mail",
            ));
        }
        let auth = self.auth.as_mut().expect("signed in");
        auth.condstore = true;
        let view = self.shared.view.clone();
        let snapshot = match view.select(&auth.viewer, &target.id).await {
            Ok(snapshot) => snapshot,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        let rights = target.rights;
        let read_only =
            examine || !(rights.add || rights.remove || rights.set_seen || rights.set_keywords);
        let flags: Vec<Flag<'static>> = KEYWORDS.iter().filter_map(|f| respond::flag(f)).collect();
        conn.data(Data::Flags(flags));
        let mut permanent: Vec<&str> = Vec::new();
        if !read_only {
            if rights.set_seen {
                permanent.push("\\Seen");
            }
            if rights.set_keywords {
                permanent.extend(["\\Answered", "\\Flagged", "\\Draft"]);
            }
            if rights.remove {
                permanent.push("\\Deleted");
            }
        }
        conn.status(respond::untagged_ok(
            Some(Code::PermanentFlags(respond::permanent_flags(
                &permanent,
                !read_only && rights.set_keywords,
            ))),
            "Flags that stick",
        ));
        conn.data(Data::Exists(
            snapshot
                .status
                .messages
                .max(u32::try_from(snapshot.members.len()).unwrap_or(u32::MAX)),
        ));
        conn.data(Data::Recent(0));
        conn.status(respond::untagged_ok(
            Some(Code::UidValidity(nz(snapshot.status.uidvalidity))),
            "UIDs valid",
        ));
        conn.status(respond::untagged_ok(
            Some(Code::UidNext(nz(snapshot.status.uidnext))),
            "Predicted next UID",
        ));
        conn.status(respond::untagged_ok(
            Some(Code::HighestModSeq(nz64(snapshot.status.highest_modseq))),
            "Highest",
        ));
        let selected = Selected {
            id: target.id.clone(),
            read_only,
            uidvalidity: snapshot.status.uidvalidity,
            members: snapshot.members,
            known_modseq: snapshot.status.highest_modseq,
            deferred: BTreeSet::new(),
        };
        if let Some((uidvalidity, since, known)) = resync
            && uidvalidity == selected.uidvalidity
        {
            match view
                .mailbox_changes(&auth.viewer, &selected.id, since)
                .await
            {
                Ok(changes) => {
                    let known = known.map(|set| respond::resolve(&set, u32::MAX));
                    let vanished: Vec<u32> = changes
                        .vanished
                        .iter()
                        .copied()
                        .filter(|uid| known.as_ref().is_none_or(|k| respond::contains(k, *uid)))
                        .filter(|uid| selected.member(*uid).is_none())
                        .collect();
                    if let Some(set) = respond::sequence_set(&vanished) {
                        conn.data(Data::Vanished {
                            earlier: true,
                            known_uids: set,
                        });
                    }
                    for changed in &changes.changed {
                        if let Some(seq) = selected.seq(changed.member.uid) {
                            conn.data(flags_fetch(seq, &changed.member, &changed.flags, true));
                        }
                    }
                }
                Err(error) => return conn.status(view_error(tag, error)),
            }
        }
        auth.selected = Some(selected);
        let code = if read_only {
            Code::ReadOnly
        } else {
            Code::ReadWrite
        };
        let text = if examine {
            "EXAMINE completed"
        } else {
            "SELECT completed"
        };
        conn.status(tagged(tag, Kind::Ok, Some(code), text));
    }

    /// Tells the client what changed in the selected mailbox since it was last told: new
    /// members (`EXISTS`), flag changes (`FETCH`) and, when `expunge` allows, members that
    /// left (`EXPUNGE`, or `VANISHED` with QRESYNC). Departures are held back otherwise.
    pub(super) async fn refresh<W: Wire>(&mut self, conn: &mut Conn<W>, expunge: bool) {
        let Some(auth) = &mut self.auth else {
            return;
        };
        let Some(selected) = &mut auth.selected else {
            return;
        };
        let changes = match self
            .shared
            .view
            .mailbox_changes(&auth.viewer, &selected.id, selected.known_modseq)
            .await
        {
            Ok(changes) => changes,
            Err(error) => {
                tracing::debug!(%error, "mailbox changes not read");
                return;
            }
        };
        for uid in &changes.vanished {
            if selected.member(*uid).is_some() {
                selected.deferred.insert(*uid);
            }
        }
        if expunge && !selected.deferred.is_empty() {
            let uids = std::mem::take(&mut selected.deferred);
            let seqs = selected.expunge(&uids);
            if auth.qresync {
                let list: Vec<u32> = uids.iter().copied().collect();
                if let Some(set) = respond::sequence_set(&list) {
                    conn.data(Data::Vanished {
                        earlier: false,
                        known_uids: set,
                    });
                }
            } else {
                for seq in seqs {
                    conn.data(Data::Expunge(nz(seq)));
                }
            }
        }
        let mut added = false;
        for changed in changes.changed {
            let uid = changed.member.uid;
            if let Some(seq) = selected.seq(uid) {
                let Some(member) = selected.members.get_mut(seq as usize - 1) else {
                    continue;
                };
                let newer = changed.member.modseq > member.modseq;
                member.modseq = member.modseq.max(changed.member.modseq);
                if newer && !selected.deferred.contains(&uid) {
                    conn.data(flags_fetch(
                        seq,
                        &changed.member,
                        &changed.flags,
                        auth.condstore,
                    ));
                }
            } else if uid > selected.largest_uid() {
                selected.members.push(changed.member);
                added = true;
            }
        }
        if added {
            conn.data(Data::Exists(selected.exists()));
        }
        selected.known_modseq = selected.known_modseq.max(changes.status.highest_modseq);
    }
}
