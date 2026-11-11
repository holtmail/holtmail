//! Message commands in the selected mailbox, and appends.

use std::collections::HashMap;
use std::io;

use bytes::Bytes;
use holtmail_core::Timestamp;
use holtmail_mailview::{EmailRef, Envelope, FlagChange, Member, NewEmail, Outcome, Part, Refusal};
use imap_types::command::{FetchModifier, StoreModifier};
use imap_types::core::{Charset, Tag, Vec1};
use imap_types::extensions::binary::LiteralOrLiteral8;
use imap_types::fetch::{MacroOrMessageDataItemNames, MessageDataItem, MessageDataItemName};
use imap_types::flag::{Flag, StoreResponse, StoreType};
use imap_types::mailbox::Mailbox;
use imap_types::response::{Code, Data};
use imap_types::search::SearchKey;
use imap_types::sequence::SequenceSet;

use crate::conn::{Conn, Wire};
use crate::fetch::{self, Fields, Wanted};
use crate::respond::{self, Kind, code, nz, nz64, tagged};
use crate::search;

use super::{LITERAL_MINUS, Session, has_flag, literal_data, view_error};

impl Session {
    pub(super) async fn close(&mut self) {
        let Some(auth) = &mut self.auth else {
            return;
        };
        let Some(selected) = auth.selected.take() else {
            return;
        };
        if selected.read_only {
            return;
        }
        let emails: Vec<EmailRef> = selected.members.iter().map(|m| m.email).collect();
        let view = &self.shared.view;
        let Ok(flags) = view.flags(&auth.viewer, Some(&selected.id), &emails).await else {
            return;
        };
        let deleted: Vec<EmailRef> = emails
            .into_iter()
            .filter(|e| flags.get(e).is_some_and(|f| has_flag(f, "\\Deleted")))
            .collect();
        if !deleted.is_empty() {
            let _ = self
                .shared
                .actions
                .remove(&auth.viewer, Some(&selected.id), &deleted)
                .await;
        }
    }

    pub(super) async fn expunge<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        uids: Option<&SequenceSet>,
    ) {
        let auth = self.auth.as_mut().expect("signed in");
        let selected = auth.selected.as_ref().expect("selected");
        if selected.read_only {
            return conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                "The mailbox is read-only",
            ));
        }
        let ranges = uids.map(|set| respond::resolve(set, selected.largest_uid()));
        let candidates: Vec<Member> = selected
            .members
            .iter()
            .filter(|m| ranges.as_ref().is_none_or(|r| respond::contains(r, m.uid)))
            .cloned()
            .collect();
        let emails: Vec<EmailRef> = candidates.iter().map(|m| m.email).collect();
        let flags = match self
            .shared
            .view
            .flags(&auth.viewer, Some(&selected.id), &emails)
            .await
        {
            Ok(flags) => flags,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        let deleted: Vec<&Member> = candidates
            .iter()
            .filter(|m| {
                flags
                    .get(&m.email)
                    .is_some_and(|f| has_flag(f, "\\Deleted"))
            })
            .collect();
        let mut refused = false;
        if !deleted.is_empty() {
            let emails: Vec<EmailRef> = deleted.iter().map(|m| m.email).collect();
            let results = match self
                .shared
                .actions
                .remove(&auth.viewer, Some(&selected.id), &emails)
                .await
            {
                Ok(results) => results,
                Err(error) => return conn.status(view_error(tag, error)),
            };
            let selected = auth.selected.as_mut().expect("selected");
            for result in results {
                if result.outcome.accepted() {
                    if let Some(uid) = selected.uid_of_email(&result.email) {
                        selected.deferred.insert(uid);
                    }
                } else {
                    refused = true;
                }
            }
        }
        self.refresh(conn, true).await;
        if refused {
            conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                "Some messages cannot be removed from this mailbox",
            ));
        } else {
            conn.status(tagged(tag, Kind::Ok, None, "EXPUNGE completed"));
        }
    }

    pub(super) async fn search<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        charset: Option<Charset<'static>>,
        criteria: &[SearchKey<'static>],
        uid: bool,
    ) {
        if let Some(charset) = &charset {
            let name = match charset {
                Charset::Atom(atom) => atom.as_ref().to_string(),
                Charset::Quoted(quoted) => quoted.as_ref().to_string(),
            };
            if !name.eq_ignore_ascii_case("UTF-8") && !name.eq_ignore_ascii_case("US-ASCII") {
                return conn.status(tagged(
                    tag,
                    Kind::No,
                    Some(code("BADCHARSET (UTF-8 US-ASCII)")),
                    "Only UTF-8 and US-ASCII searches are offered",
                ));
            }
        }
        self.refresh(conn, uid).await;
        let auth = self.auth.as_mut().expect("signed in");
        let selected = auth.selected.as_ref().expect("selected");
        let exists = selected.exists();
        let (filter, modseq) = search::filter(
            criteria,
            &|set| selected.uid_ranges(&respond::resolve(set, exists)),
            selected.largest_uid(),
        );
        if modseq {
            auth.condstore = true;
        }
        let selected = auth.selected.as_ref().expect("selected");
        let found = match self
            .shared
            .view
            .search(&auth.viewer, &selected.id, &filter)
            .await
        {
            Ok(found) => found,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        let mut ids = Vec::new();
        let mut highest = 0;
        for member in &found {
            let Some(seq) = selected.seq(member.uid) else {
                continue;
            };
            ids.push(nz(if uid { member.uid } else { seq }));
            highest = highest.max(member.modseq);
        }
        let highest = (modseq && !ids.is_empty()).then(|| nz64(highest));
        conn.data(Data::Search(ids, highest));
        conn.status(tagged(tag, Kind::Ok, None, "SEARCH completed"));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn fetch<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        set: &SequenceSet,
        names: MacroOrMessageDataItemNames<'static>,
        uid: bool,
        modifiers: &[FetchModifier],
    ) -> io::Result<()> {
        self.refresh(conn, uid).await;
        let mut wanted = Wanted::new(names);
        if wanted.binary {
            conn.status(tagged(tag, Kind::Bad, None, "BINARY is not offered"));
            return Ok(());
        }
        let mut changed_since = None;
        let mut vanished = false;
        for modifier in modifiers {
            match modifier {
                FetchModifier::ChangedSince(value) => changed_since = Some(value.get()),
                FetchModifier::Vanished => vanished = true,
            }
        }
        let auth = self.auth.as_mut().expect("signed in");
        if vanished && (!uid || !auth.qresync || changed_since.is_none()) {
            conn.status(tagged(
                tag,
                Kind::Bad,
                None,
                "VANISHED needs UID FETCH, CHANGEDSINCE and ENABLE QRESYNC",
            ));
            return Ok(());
        }
        if uid {
            wanted.ensure(MessageDataItemName::Uid);
        }
        if changed_since.is_some() || wanted.modseq {
            auth.condstore = true;
            wanted.modseq = true;
            if !wanted.items.contains(&MessageDataItemName::ModSeq) {
                wanted.items.push(MessageDataItemName::ModSeq);
            }
        }
        let view = self.shared.view.clone();
        let selected = auth.selected.as_ref().expect("selected");
        let mailbox = selected.id.clone();
        let mut members = if uid {
            selected.by_uid(&respond::resolve(set, selected.largest_uid()))
        } else {
            selected.by_seq(&respond::resolve(set, selected.exists()))
        };
        if let Some(since) = changed_since {
            members.retain(|(_, m)| m.modseq > since);
            if vanished {
                let ranges = respond::resolve(set, selected.largest_uid());
                match view.mailbox_changes(&auth.viewer, &mailbox, since).await {
                    Ok(changes) => {
                        let gone: Vec<u32> = changes
                            .vanished
                            .into_iter()
                            .filter(|u| respond::contains(&ranges, *u))
                            .filter(|u| selected.member(*u).is_none())
                            .collect();
                        if let Some(set) = respond::sequence_set(&gone) {
                            conn.data(Data::Vanished {
                                earlier: true,
                                known_uids: set,
                            });
                        }
                    }
                    Err(error) => {
                        conn.status(view_error(tag, error));
                        return Ok(());
                    }
                }
            }
        }
        let read_only = selected.read_only;
        if wanted.sets_seen && auth.principal.implicit_seen && !read_only && !members.is_empty() {
            let emails: Vec<EmailRef> = members.iter().map(|(_, m)| m.email).collect();
            let change = FlagChange {
                add: vec!["\\Seen".into()],
                remove: Vec::new(),
            };
            if let Ok(results) = self
                .shared
                .actions
                .set_flags(&auth.viewer, Some(&mailbox), &emails, &change, None)
                .await
            {
                let modseqs: HashMap<EmailRef, u64> =
                    results.iter().map(|r| (r.email, r.modseq)).collect();
                for (_, member) in &mut members {
                    if let Some(modseq) = modseqs.get(&member.email) {
                        member.modseq = (*modseq).max(member.modseq);
                    }
                }
            }
            wanted.flags = true;
            wanted.ensure(MessageDataItemName::Flags);
        }
        let batch = self.shared.settings.fetch_batch.max(1);
        for chunk in members.chunks(batch) {
            let emails: Vec<EmailRef> = chunk.iter().map(|(_, m)| m.email).collect();
            let flags = if wanted.flags {
                match view.flags(&auth.viewer, Some(&mailbox), &emails).await {
                    Ok(flags) => flags,
                    Err(error) => {
                        conn.status(view_error(tag, error));
                        return Ok(());
                    }
                }
            } else {
                HashMap::new()
            };
            let envelopes: HashMap<EmailRef, Envelope> = if wanted.envelope {
                match view.envelopes(&auth.viewer, &emails).await {
                    Ok(list) => list.into_iter().map(|e| (e.email, e)).collect(),
                    Err(error) => {
                        conn.status(view_error(tag, error));
                        return Ok(());
                    }
                }
            } else {
                HashMap::new()
            };
            for (seq, member) in chunk {
                let envelope = envelopes.get(&member.email);
                if wanted.envelope && envelope.is_none() {
                    continue;
                }
                let structure = if wanted.structure {
                    match view.structure(&auth.viewer, &member.email).await {
                        Ok(part) => Some(part),
                        Err(error) => {
                            tracing::debug!(%error, "structure not read");
                            continue;
                        }
                    }
                } else {
                    None
                };
                let needs_raw = wanted.raw
                    || structure.as_ref().is_some_and(|part| {
                        fetch::has_message(part) || envelope.is_some_and(|e| e.size != e.crlf_size)
                    });
                let mut permit = None;
                let raw = if needs_raw {
                    let size = envelope.map_or(1, |e| e.crlf_size);
                    let budget = self
                        .shared
                        .settings
                        .response_bytes
                        .clamp(1, u32::MAX as usize);
                    let want = u32::try_from(size)
                        .unwrap_or(u32::MAX)
                        .clamp(1, budget as u32);
                    permit = self
                        .shared
                        .bytes
                        .clone()
                        .acquire_many_owned(want)
                        .await
                        .ok();
                    match view.raw(&auth.viewer, &member.email).await {
                        Ok(raw) => Some(raw),
                        Err(error) => {
                            tracing::debug!(%error, "raw bytes not read");
                            continue;
                        }
                    }
                } else {
                    None
                };
                let items = items(
                    &wanted,
                    member,
                    flags.get(&member.email).map(Vec::as_slice).unwrap_or(&[]),
                    envelope,
                    structure.as_ref(),
                    raw.as_deref(),
                );
                if let Ok(items) = Vec1::try_from(items) {
                    conn.data(Data::Fetch {
                        seq: nz(*seq),
                        items,
                    });
                }
                if permit.is_some() {
                    conn.flush().await?;
                    drop(permit);
                }
            }
            conn.flush().await?;
        }
        if let Some(selected) = &mut auth.selected {
            for (_, member) in &members {
                if let Some(seq) = selected.seq(member.uid)
                    && let Some(known) = selected.members.get_mut(seq as usize - 1)
                {
                    known.modseq = known.modseq.max(member.modseq);
                }
            }
        }
        conn.status(tagged(tag, Kind::Ok, None, "FETCH completed"));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn store<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        set: &SequenceSet,
        kind: StoreType,
        response: StoreResponse,
        flags: &[Flag<'static>],
        uid: bool,
        modifiers: &[StoreModifier],
    ) {
        self.refresh(conn, uid).await;
        let auth = self.auth.as_mut().expect("signed in");
        let unchanged_since = modifiers
            .iter()
            .map(|StoreModifier::UnchangedSince(value)| *value)
            .next();
        if unchanged_since.is_some() {
            auth.condstore = true;
        }
        let selected = auth.selected.as_ref().expect("selected");
        if selected.read_only {
            return conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                "The mailbox is read-only",
            ));
        }
        let mailbox = selected.id.clone();
        let members = if uid {
            selected.by_uid(&respond::resolve(set, selected.largest_uid()))
        } else {
            selected.by_seq(&respond::resolve(set, selected.exists()))
        };
        let names: Vec<String> = flags
            .iter()
            .map(respond::flag_name)
            .filter(|f| !f.eq_ignore_ascii_case("\\Recent"))
            .collect();
        let emails: Vec<EmailRef> = members.iter().map(|(_, m)| m.email).collect();
        let actions = self.shared.actions.clone();
        let mut results = Vec::new();
        match kind {
            StoreType::Add | StoreType::Remove => {
                let change = if kind == StoreType::Add {
                    FlagChange {
                        add: names,
                        remove: Vec::new(),
                    }
                } else {
                    FlagChange {
                        add: Vec::new(),
                        remove: names,
                    }
                };
                match actions
                    .set_flags(
                        &auth.viewer,
                        Some(&mailbox),
                        &emails,
                        &change,
                        unchanged_since,
                    )
                    .await
                {
                    Ok(list) => results = list,
                    Err(error) => return conn.status(view_error(tag, error)),
                }
            }
            StoreType::Replace => {
                let current = match self
                    .shared
                    .view
                    .flags(&auth.viewer, Some(&mailbox), &emails)
                    .await
                {
                    Ok(current) => current,
                    Err(error) => return conn.status(view_error(tag, error)),
                };
                for email in &emails {
                    let now = current.get(email).cloned().unwrap_or_default();
                    let change = FlagChange {
                        add: names
                            .iter()
                            .filter(|n| !has_flag(&now, n))
                            .cloned()
                            .collect(),
                        remove: now
                            .iter()
                            .filter(|f| !has_flag(&names, f) && !f.eq_ignore_ascii_case("\\Recent"))
                            .cloned()
                            .collect(),
                    };
                    match actions
                        .set_flags(
                            &auth.viewer,
                            Some(&mailbox),
                            &[*email],
                            &change,
                            unchanged_since,
                        )
                        .await
                    {
                        Ok(list) => results.extend(list),
                        Err(error) => return conn.status(view_error(tag, error)),
                    }
                }
            }
        }
        let by_email: HashMap<EmailRef, _> = results.into_iter().map(|r| (r.email, r)).collect();
        let mut modified = Vec::new();
        let mut cannot = false;
        let condstore = auth.condstore;
        let selected = auth.selected.as_mut().expect("selected");
        for (seq, member) in &members {
            let Some(result) = by_email.get(&member.email) else {
                continue;
            };
            match result.outcome {
                Outcome::Refused(Refusal::Modified) => {
                    modified.push(if uid { member.uid } else { *seq });
                    continue;
                }
                Outcome::Refused(Refusal::NotFound) => continue,
                Outcome::Refused(_) => cannot = true,
                Outcome::Held | Outcome::Applied => {}
            }
            let modseq = result.modseq.max(member.modseq);
            if let Some(known) = selected.members.get_mut(*seq as usize - 1) {
                known.modseq = modseq;
            }
            let mut items = Vec::new();
            if response == StoreResponse::Answer {
                items.push(MessageDataItem::Flags(respond::fetch_flags(&result.flags)));
            }
            if uid
                || (condstore && !items.is_empty())
                || (condstore && response == StoreResponse::Silent)
            {
                items.push(MessageDataItem::Uid(nz(member.uid)));
            }
            if condstore {
                items.push(MessageDataItem::ModSeq(nz64(modseq)));
            }
            if let Ok(items) = Vec1::try_from(items)
                && (response == StoreResponse::Answer || condstore)
            {
                conn.data(Data::Fetch {
                    seq: nz(*seq),
                    items,
                });
            }
        }
        if cannot {
            conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                "Some flags cannot be changed in this mailbox",
            ));
        } else if let Some(set) = respond::sequence_set(&modified) {
            conn.status(tagged(
                tag,
                Kind::Ok,
                Some(Code::Modified(set)),
                "Some messages changed meanwhile",
            ));
        } else {
            conn.status(tagged(tag, Kind::Ok, None, "STORE completed"));
        }
    }

    pub(super) async fn transfer<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        set: &SequenceSet,
        mailbox: &Mailbox<'_>,
        uid: bool,
        moving: bool,
    ) {
        self.refresh(conn, true).await;
        let target = match self.resolve(tag, mailbox).await {
            Ok(target) => target,
            Err(_) => {
                return conn.status(tagged(
                    tag,
                    Kind::No,
                    Some(Code::TryCreate),
                    "No such mailbox",
                ));
            }
        };
        let auth = self.auth.as_mut().expect("signed in");
        let selected = auth.selected.as_ref().expect("selected");
        if moving && selected.read_only {
            return conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                "The mailbox is read-only",
            ));
        }
        let source = selected.id.clone();
        let members = if uid {
            selected.by_uid(&respond::resolve(set, selected.largest_uid()))
        } else {
            selected.by_seq(&respond::resolve(set, selected.exists()))
        };
        if members.is_empty() {
            let text = if moving {
                "MOVE completed"
            } else {
                "COPY completed"
            };
            return conn.status(tagged(tag, Kind::Ok, None, text));
        }
        let emails: Vec<EmailRef> = members.iter().map(|(_, m)| m.email).collect();
        let actions = self.shared.actions.clone();
        let results = if moving {
            actions
                .move_to(&auth.viewer, &source, &emails, &target.id)
                .await
        } else {
            actions
                .copy_to(&auth.viewer, &source, &emails, &target.id)
                .await
        };
        let results = match results {
            Ok(results) => results,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        let uids: HashMap<EmailRef, u32> = members.iter().map(|(_, m)| (m.email, m.uid)).collect();
        let mut sources = Vec::new();
        let mut targets = Vec::new();
        let mut moved = Vec::new();
        let mut refused = 0;
        for result in &results {
            if !result.outcome.accepted() {
                refused += 1;
                continue;
            }
            let Some(source_uid) = uids.get(&result.email) else {
                continue;
            };
            moved.push(*source_uid);
            if let Some(target_uid) = result.uid {
                sources.push(*source_uid);
                targets.push(target_uid);
            }
        }
        if moved.is_empty() {
            return conn.status(tagged(
                tag,
                Kind::No,
                Some(code("CANNOT")),
                if moving {
                    "These messages cannot be moved there"
                } else {
                    "These messages cannot be copied there"
                },
            ));
        }
        let copy_uid = match (respond::uid_set(&sources), respond::uid_set(&targets)) {
            (Some(source), Some(destination)) => {
                match self.shared.view.status(&auth.viewer, &target.id).await {
                    Ok(status) => Some(Code::CopyUid {
                        uid_validity: nz(status.uidvalidity),
                        source,
                        destination,
                    }),
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let text = match (moving, refused) {
            (true, 0) => "MOVE completed",
            (false, 0) => "COPY completed",
            (true, _) => "Some messages could not be moved",
            (false, _) => "Some messages could not be copied",
        };
        if moving {
            if let Some(code) = copy_uid {
                conn.status(respond::untagged_ok(Some(code), "Moved"));
            }
            let selected = auth.selected.as_mut().expect("selected");
            selected.deferred.extend(moved);
            self.refresh(conn, true).await;
            conn.status(tagged(tag, Kind::Ok, None, text));
        } else {
            conn.status(tagged(tag, Kind::Ok, copy_uid, text));
        }
    }

    pub(super) async fn append<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        mailbox: &Mailbox<'_>,
        flags: Vec<Flag<'static>>,
        date: Option<imap_types::datetime::DateTime>,
        message: LiteralOrLiteral8<'static>,
    ) {
        let (data, non_sync) = literal_data(&message);
        if non_sync && data.len() > LITERAL_MINUS {
            return conn.status(tagged(
                tag,
                Kind::Bad,
                None,
                "Non-synchronizing literals above 4096 bytes are not taken",
            ));
        }
        let limit = self.auth.as_ref().map_or(0, |a| a.append_limit);
        if data.len() as u64 > limit {
            return conn.status(tagged(
                tag,
                Kind::No,
                Some(Code::TooBig),
                format!("The message is larger than the {limit} bytes allowed"),
            ));
        }
        let target = match self.resolve(tag, mailbox).await {
            Ok(target) => target,
            Err(_) => {
                return conn.status(tagged(
                    tag,
                    Kind::No,
                    Some(Code::TryCreate),
                    "No such mailbox",
                ));
            }
        };
        let auth = self.auth.as_ref().expect("signed in");
        let email = NewEmail {
            raw: Bytes::copy_from_slice(data),
            flags: flags.iter().map(respond::flag_name).collect(),
            received_at: date.map(|d| Timestamp::from_secs(d.as_ref().timestamp())),
        };
        let appended = match self
            .shared
            .actions
            .append(&auth.viewer, &target.id, email)
            .await
        {
            Ok(appended) => appended,
            Err(error) => return conn.status(view_error(tag, error)),
        };
        if let Outcome::Refused(refusal) = appended.outcome {
            let text = match refusal {
                Refusal::Locked(_) => "This device or account may not add messages there",
                _ => "This mailbox does not take messages",
            };
            return conn.status(tagged(tag, Kind::No, Some(code("CANNOT")), text));
        }
        let code = match appended.uid {
            Some(uid) => match self.shared.view.status(&auth.viewer, &target.id).await {
                Ok(status) => Some(Code::AppendUid {
                    uid_validity: nz(status.uidvalidity),
                    uid: nz(uid),
                }),
                Err(_) => None,
            },
            None => None,
        };
        let same = self.selected().is_some_and(|s| s.id == target.id);
        if same {
            self.refresh(conn, true).await;
        }
        conn.status(tagged(tag, Kind::Ok, code, "APPEND completed"));
    }
}

/// The items of one `FETCH` response, in the order asked.
fn items(
    wanted: &Wanted,
    member: &Member,
    flags: &[String],
    envelope: Option<&Envelope>,
    structure: Option<&Part>,
    raw: Option<&[u8]>,
) -> Vec<MessageDataItem<'static>> {
    use MessageDataItemName as N;
    let mut out = Vec::new();
    for item in &wanted.items {
        match item {
            N::Uid => out.push(MessageDataItem::Uid(nz(member.uid))),
            N::Flags => out.push(MessageDataItem::Flags(respond::fetch_flags(flags))),
            N::ModSeq => out.push(MessageDataItem::ModSeq(nz64(member.modseq))),
            N::Envelope => {
                if let Some(envelope) = envelope {
                    out.push(MessageDataItem::Envelope(Fields::of(envelope).envelope()));
                }
            }
            N::InternalDate => {
                if let Some(envelope) = envelope {
                    out.push(MessageDataItem::InternalDate(fetch::date_time(
                        envelope.received_at,
                    )));
                }
            }
            N::Rfc822Size => {
                if let Some(envelope) = envelope {
                    out.push(MessageDataItem::Rfc822Size(
                        u32::try_from(envelope.crlf_size).unwrap_or(u32::MAX),
                    ));
                }
            }
            N::Body => {
                if let Some(part) = structure {
                    out.push(MessageDataItem::Body(fetch::structure(part, raw, false)));
                }
            }
            N::BodyStructure => {
                if let Some(part) = structure {
                    out.push(MessageDataItem::BodyStructure(fetch::structure(
                        part, raw, true,
                    )));
                }
            }
            N::BodyExt {
                section, partial, ..
            } => {
                let bytes = match (raw, structure, section) {
                    (Some(raw), _, None) => Some(raw.to_vec()),
                    (Some(raw), Some(part), Some(section)) => {
                        fetch::section(raw, part, Some(section))
                    }
                    _ => None,
                };
                let data = bytes
                    .map(|bytes| fetch::nstring(fetch::served(&bytes, *partial)))
                    .unwrap_or_else(|| fetch::nstring(Vec::new()));
                out.push(MessageDataItem::BodyExt {
                    section: section.clone(),
                    origin: partial.map(|(origin, _)| origin),
                    data,
                });
            }
            N::Rfc822 => {
                if let Some(raw) = raw {
                    out.push(MessageDataItem::Rfc822(fetch::nstring(fetch::served(
                        raw, None,
                    ))));
                }
            }
            N::Rfc822Header | N::Rfc822Text => {
                let section = if matches!(item, N::Rfc822Header) {
                    imap_types::fetch::Section::Header(None)
                } else {
                    imap_types::fetch::Section::Text(None)
                };
                let bytes = match (raw, structure) {
                    (Some(raw), Some(part)) => fetch::section(raw, part, Some(&section)),
                    _ => None,
                }
                .unwrap_or_default();
                let data = fetch::nstring(fetch::served(&bytes, None));
                out.push(if matches!(item, N::Rfc822Header) {
                    MessageDataItem::Rfc822Header(data)
                } else {
                    MessageDataItem::Rfc822Text(data)
                });
            }
            N::Binary { .. } | N::BinarySize { .. } => {}
        }
    }
    out
}
