//! A [`MailboxWriter`] for IMAP servers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_imap::imap_proto::rfc4315::UidSetMember;
use async_imap::imap_proto::{Response, ResponseCode, Status};
use async_trait::async_trait;
use holtmail_core::Timestamp;
use holtmail_source::RemoteFolder;
use holtmail_source::{Flag, FlagResult, MailboxWriteCaps, MailboxWriter, Moved, SourceError};
use holtmail_store::FolderRole;
use tokio::io::AsyncWriteExt;

use crate::stream::ImapStream;
use crate::{ImapSource, convert, labels, list_folders, timed, uid_set};

/// How long a connection left over from one call may wait to be used by the next. Servers close
/// idle connections after 30 minutes at the earliest (RFC 3501), NAT gateways often sooner.
const REUSE_WITHIN: Duration = Duration::from_secs(60);

type Session = async_imap::Session<ImapStream>;

#[derive(Debug, Clone, Copy)]
struct ServerCaps {
    gmail: bool,
    condstore: bool,
    uidplus: bool,
    moves: bool,
}

/// Changes messages on an IMAP account, from [`ImapSource::writer`].
///
/// Each call signs in on a connection of its own and opens its folder with `SELECT`; a
/// connection a call left in good order is reused by the next call within a minute. The
/// capabilities are those the server advertised when the writer was made.
pub struct ImapWriter {
    source: ImapSource,
    server: ServerCaps,
    idle: Mutex<Option<(Session, Instant)>>,
    folders: Mutex<Vec<(RemoteFolder, String)>>,
}

impl ImapSource {
    /// A [`MailboxWriter`] for this account. Signs in once to learn what the server can do:
    /// `CONDSTORE` (or `QRESYNC`) makes it conditional, `UIDPLUS` reports new UIDs after moves
    /// and appends, `MOVE` moves on the server, and `X-GM-EXT-1` (Gmail) enables labels. Fails
    /// like [`holtmail_source::Source::connect`].
    pub async fn writer(&self) -> Result<ImapWriter, SourceError> {
        let signed_in = self.sign_in().await?;
        let server = ServerCaps {
            gmail: signed_in.gmail,
            condstore: signed_in.condstore,
            uidplus: signed_in.uidplus,
            moves: signed_in.moves,
        };
        Ok(ImapWriter {
            source: self.clone(),
            server,
            idle: Mutex::new(Some((signed_in.session, Instant::now()))),
            folders: Mutex::new(Vec::new()),
        })
    }
}

enum Failure {
    Source(SourceError),
    Refused {
        verb: String,
        text: String,
        trycreate: bool,
    },
}

impl From<SourceError> for Failure {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<Failure> for SourceError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Source(error) => error,
            Failure::Refused { verb, text, .. } => {
                SourceError::Protocol(format!("{verb} refused: {text}"))
            }
        }
    }
}

impl ImapWriter {
    async fn open(&self) -> Result<Session, SourceError> {
        let cached = self.idle.lock().unwrap().take();
        if let Some((session, since)) = cached
            && since.elapsed() < REUSE_WITHIN
        {
            return Ok(session);
        }
        Ok(self.source.sign_in().await?.session)
    }

    fn finish<T>(&self, session: Session, result: &Result<T, SourceError>) {
        let healthy = match result {
            Ok(_) => true,
            Err(error) => matches!(error, SourceError::Missing(_) | SourceError::Unsupported(_)),
        };
        if healthy {
            *self.idle.lock().unwrap() = Some((session, Instant::now()));
        }
    }

    async fn folder(
        &self,
        session: &mut Session,
        want: impl Fn(&RemoteFolder) -> bool,
        missing: &str,
    ) -> Result<String, SourceError> {
        let find = |folders: &[(RemoteFolder, String)]| {
            folders
                .iter()
                .find(|(f, _)| want(f))
                .map(|(f, raw)| (f.selectable, raw.clone()))
        };
        let mut found = find(&self.folders.lock().unwrap());
        if found.is_none() {
            let listed = list_folders(session, self.server.gmail, self.source.timeout).await?;
            found = find(&listed);
            *self.folders.lock().unwrap() = listed;
        }
        match found {
            Some((true, raw)) => Ok(raw),
            _ => Err(SourceError::Missing(missing.to_owned())),
        }
    }

    async fn named(&self, session: &mut Session, path: &str) -> Result<String, SourceError> {
        self.folder(session, |f| f.path == path, path).await
    }

    async fn select(
        &self,
        session: &mut Session,
        raw: &str,
        path: &str,
    ) -> Result<async_imap::types::Mailbox, SourceError> {
        match timed(self.source.timeout, session.select(raw)).await? {
            Ok(mailbox) => Ok(mailbox),
            Err(async_imap::error::Error::No(_)) => {
                self.folders.lock().unwrap().clear();
                Err(SourceError::Missing(path.to_owned()))
            }
            Err(other) => Err(convert(other)),
        }
    }

    async fn search(
        &self,
        session: &mut Session,
        query: &str,
    ) -> Result<BTreeSet<u64>, SourceError> {
        let found = timed(self.source.timeout, session.uid_search(query))
            .await?
            .map_err(convert)?;
        Ok(found.into_iter().map(u64::from).collect())
    }

    async fn existing(
        &self,
        session: &mut Session,
        uids: &[u64],
    ) -> Result<BTreeSet<u64>, SourceError> {
        let wanted = imap_uids(uids);
        if wanted.is_empty() {
            return Ok(BTreeSet::new());
        }
        let found = self
            .search(session, &format!("UID {}", uid_set(&wanted)))
            .await?;
        Ok(found
            .into_iter()
            .filter(|uid| wanted.contains(uid))
            .collect())
    }

    async fn exchange(&self, session: &mut Session, command: &str) -> Result<Answer, Failure> {
        timed(self.source.timeout, exchange(session, command)).await?
    }

    async fn store(
        &self,
        session: &mut Session,
        uids: &BTreeSet<u64>,
        item: &str,
        values: &[String],
        unchanged_since: Option<u64>,
    ) -> Result<BTreeSet<u64>, SourceError> {
        if uids.is_empty() || values.is_empty() {
            return Ok(BTreeSet::new());
        }
        let set = uid_set(&uids.iter().copied().collect::<Vec<_>>());
        let condition = unchanged_since
            .map(|modseq| format!(" (UNCHANGEDSINCE {modseq})"))
            .unwrap_or_default();
        let answer = self
            .exchange(
                session,
                &format!("UID STORE {set}{condition} {item} ({})", values.join(" ")),
            )
            .await?;
        Ok(answer.modified)
    }

    async fn mark_deleted(
        &self,
        session: &mut Session,
        uids: &BTreeSet<u64>,
    ) -> Result<(), SourceError> {
        self.store(
            session,
            uids,
            "+FLAGS.SILENT",
            &["\\Deleted".to_owned()],
            None,
        )
        .await
        .map(drop)
    }

    /// Expunges `ours`, already marked `\Deleted`, and nothing else.
    async fn expunge(
        &self,
        session: &mut Session,
        ours: &BTreeSet<u64>,
    ) -> Result<(), SourceError> {
        if ours.is_empty() {
            return Ok(());
        }
        if self.server.uidplus {
            let set = uid_set(&ours.iter().copied().collect::<Vec<_>>());
            self.exchange(session, &format!("UID EXPUNGE {set}"))
                .await?;
            return Ok(());
        }
        self.only_ours_deleted(session, ours).await?;
        self.exchange(session, "EXPUNGE").await?;
        Ok(())
    }

    async fn only_ours_deleted(
        &self,
        session: &mut Session,
        ours: &BTreeSet<u64>,
    ) -> Result<(), SourceError> {
        let deleted = self.search(session, "DELETED").await?;
        if deleted.is_subset(ours) {
            return Ok(());
        }
        tracing::warn!(
            marked = ours.len(),
            others = deleted.difference(ours).count(),
            "not expunging: the folder holds other messages marked deleted and the server has no UIDPLUS"
        );
        Err(SourceError::Protocol(
            "not expunged: other messages in the folder are marked deleted \
             and the server has no UID EXPUNGE (UIDPLUS)"
                .into(),
        ))
    }

    fn check_flags(&self, flags: &[Flag]) -> Result<(), SourceError> {
        match flags.iter().find(|flag| !keyword_ok(flag)) {
            Some(flag) => Err(SourceError::Unsupported(format!(
                "{} is not a flag or keyword this writer sets",
                flag.as_imap()
            ))),
            None => Ok(()),
        }
    }

    async fn set_flags_on(
        &self,
        session: &mut Session,
        folder: &str,
        uids: &[u64],
        add: &[Flag],
        remove: &[Flag],
        unchanged_since: Option<u64>,
    ) -> Result<FlagResult, SourceError> {
        let raw = self.named(session, folder).await?;
        let mailbox = self.select(session, &raw, folder).await?;
        let keywords: Vec<&Flag> = add
            .iter()
            .filter(|f| matches!(f, Flag::Keyword(_)))
            .collect();
        if !keywords.is_empty() && !mailbox.permanent_flags.is_empty() {
            let any = mailbox
                .permanent_flags
                .iter()
                .any(|f| matches!(f, async_imap::types::Flag::MayCreate));
            let kept = |flag: &Flag| {
                mailbox
                    .permanent_flags
                    .iter()
                    .any(|f| matches!(f, async_imap::types::Flag::Custom(name) if flag.is(name)))
            };
            if let Some(flag) = keywords.iter().find(|flag| !any && !kept(flag)) {
                return Err(SourceError::Unsupported(format!(
                    "{folder} does not keep the keyword {}",
                    flag.as_imap()
                )));
            }
        }
        let existing = self.existing(session, uids).await?;
        let adding: Vec<String> = add
            .iter()
            .filter(|f| !remove.contains(f))
            .map(|f| f.as_imap().to_owned())
            .collect();
        let removing: Vec<String> = remove.iter().map(|f| f.as_imap().to_owned()).collect();
        let mut changed = BTreeSet::new();
        let mut condition = unchanged_since;
        for (item, values) in [("+FLAGS", &adding), ("-FLAGS", &removing)] {
            if values.is_empty() {
                continue;
            }
            let targets: BTreeSet<u64> = existing.difference(&changed).copied().collect();
            changed.extend(
                self.store(session, &targets, item, values, condition.take())
                    .await?
                    .intersection(&existing),
            );
        }
        Ok(FlagResult {
            updated: existing.difference(&changed).copied().collect(),
            modified: changed.into_iter().collect(),
        })
    }

    async fn set_labels_on(
        &self,
        session: &mut Session,
        uids: &[u64],
        add: &[String],
        remove: &[String],
    ) -> Result<(), SourceError> {
        let raw = self
            .folder(
                session,
                |f| f.role == Some(FolderRole::All),
                "the All Mail folder",
            )
            .await?;
        self.select(session, &raw, "the All Mail folder").await?;
        let wanted = imap_uids(uids);
        let set: BTreeSet<u64> = wanted.into_iter().collect();
        let adding: Vec<String> = add
            .iter()
            .filter(|l| !remove.contains(l))
            .map(|l| labels::label_value(l))
            .collect();
        let removing: Vec<String> = remove.iter().map(|l| labels::label_value(l)).collect();
        self.store(session, &set, "+X-GM-LABELS", &adding, None)
            .await?;
        self.store(session, &set, "-X-GM-LABELS", &removing, None)
            .await?;
        Ok(())
    }

    async fn move_on(
        &self,
        session: &mut Session,
        folder: &str,
        uids: &[u64],
        target: &str,
    ) -> Result<Moved, SourceError> {
        let raw = self.named(session, folder).await?;
        let target_raw = self.named(session, target).await?;
        self.select(session, &raw, folder).await?;
        let existing = self.existing(session, uids).await?;
        if existing.is_empty() {
            return Ok(Moved::default());
        }
        let set = |uids: &BTreeSet<u64>| uid_set(&uids.iter().copied().collect::<Vec<_>>());
        let new_uids = if self.server.moves {
            let answer = self
                .exchange(
                    session,
                    &format!("UID MOVE {} {}", set(&existing), quote(&target_raw)),
                )
                .await?;
            answer.copied
        } else {
            if !self.server.uidplus {
                self.only_ours_deleted(session, &existing).await?;
            }
            let answer = self
                .exchange(
                    session,
                    &format!("UID COPY {} {}", set(&existing), quote(&target_raw)),
                )
                .await?;
            self.mark_deleted(session, &existing).await?;
            self.expunge(session, &existing).await?;
            answer.copied
        };
        Ok(Moved {
            uids: existing
                .into_iter()
                .map(|uid| (uid, new_uids.get(&uid).copied()))
                .collect(),
        })
    }

    async fn append_on(
        &self,
        session: &mut Session,
        folder: &str,
        raw: &[u8],
        flags: &[Flag],
        date: Timestamp,
    ) -> Result<Option<u64>, SourceError> {
        let name = self.named(session, folder).await?;
        let flags: Vec<&str> = flags.iter().map(Flag::as_imap).collect();
        let date = chrono::DateTime::from_timestamp(date.as_secs(), 0)
            .unwrap_or_default()
            .format("%d-%b-%Y %H:%M:%S +0000");
        let command = format!(
            "APPEND {} ({}) \"{date}\" {{{}}}",
            quote(&name),
            flags.join(" "),
            raw.len()
        );
        let answer = match timed(self.source.timeout, append(session, &command, raw)).await? {
            Ok(answer) => answer,
            Err(Failure::Refused {
                trycreate: true, ..
            }) => {
                self.folders.lock().unwrap().clear();
                return Err(SourceError::Missing(folder.to_owned()));
            }
            Err(other) => return Err(other.into()),
        };
        Ok(answer.appended)
    }

    async fn delete_on(
        &self,
        session: &mut Session,
        folder: &str,
        uids: &[u64],
        expunge: bool,
    ) -> Result<(), SourceError> {
        let raw = self.named(session, folder).await?;
        self.select(session, &raw, folder).await?;
        let existing = self.existing(session, uids).await?;
        self.mark_deleted(session, &existing).await?;
        if expunge {
            self.expunge(session, &existing).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl MailboxWriter for ImapWriter {
    fn capabilities(&self) -> MailboxWriteCaps {
        MailboxWriteCaps {
            flags: vec![Flag::Seen, Flag::Answered, Flag::Flagged, Flag::Draft],
            keywords: true,
            conditional: self.server.condstore,
            labels: self.server.gmail,
            move_uids: self.server.uidplus,
            append_uid: self.server.uidplus,
            mark_deleted: true,
        }
    }

    async fn set_flags(
        &self,
        folder: &str,
        uids: &[u64],
        add: &[Flag],
        remove: &[Flag],
        unchanged_since: Option<u64>,
    ) -> Result<FlagResult, SourceError> {
        self.check_flags(add)?;
        self.check_flags(remove)?;
        if unchanged_since.is_some() && !self.server.condstore {
            return Err(SourceError::Unsupported(
                "the server has no conditional store (CONDSTORE)".into(),
            ));
        }
        if uids.is_empty() {
            return Ok(FlagResult::default());
        }
        let mut session = self.open().await?;
        let result = self
            .set_flags_on(&mut session, folder, uids, add, remove, unchanged_since)
            .await;
        self.finish(session, &result);
        result
    }

    async fn set_labels(
        &self,
        uids: &[u64],
        add: &[String],
        remove: &[String],
    ) -> Result<(), SourceError> {
        if !self.server.gmail {
            return Err(SourceError::Unsupported(
                "the server has no labels (X-GM-EXT-1)".into(),
            ));
        }
        if let Some(name) = add.iter().chain(remove).find(|l| labels::acts_as_folder(l)) {
            return Err(SourceError::Unsupported(format!(
                "{name:?} stands for a mailbox; move or delete the message instead"
            )));
        }
        if imap_uids(uids).is_empty() {
            return Ok(());
        }
        let mut session = self.open().await?;
        let result = self.set_labels_on(&mut session, uids, add, remove).await;
        self.finish(session, &result);
        result
    }

    async fn move_to(
        &self,
        folder: &str,
        uids: &[u64],
        target: &str,
    ) -> Result<Moved, SourceError> {
        let mut session = self.open().await?;
        let result = self.move_on(&mut session, folder, uids, target).await;
        self.finish(session, &result);
        result
    }

    async fn append(
        &self,
        folder: &str,
        raw: &[u8],
        flags: &[Flag],
        date: Timestamp,
    ) -> Result<Option<u64>, SourceError> {
        self.check_flags(flags)?;
        let mut session = self.open().await?;
        let result = self.append_on(&mut session, folder, raw, flags, date).await;
        self.finish(session, &result);
        result
    }

    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError> {
        let mut session = self.open().await?;
        let result = self.delete_on(&mut session, folder, uids, expunge).await;
        self.finish(session, &result);
        result
    }
}

/// System flags, and keywords that are IMAP atoms and not `\`-flags such as `\Deleted`.
fn keyword_ok(flag: &Flag) -> bool {
    match flag {
        Flag::Keyword(name) => {
            !name.is_empty()
                && !name.starts_with('\\')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && !b"(){%*\"\\]".contains(&b))
        }
        _ => true,
    }
}

fn imap_uids(uids: &[u64]) -> Vec<u64> {
    let mut out: Vec<u64> = uids
        .iter()
        .copied()
        .filter(|uid| (1..=u64::from(u32::MAX)).contains(uid))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

fn expand(set: &[UidSetMember]) -> Vec<u64> {
    let mut out = Vec::new();
    for member in set {
        match member {
            UidSetMember::Uid(uid) => out.push(u64::from(*uid)),
            UidSetMember::UidRange(range) => {
                let (a, b) = (*range.start(), *range.end());
                out.extend((a.min(b)..=a.max(b)).map(u64::from));
            }
        }
    }
    out
}

/// What the responses to one command said that a writer needs.
#[derive(Debug, Default)]
struct Answer {
    /// Source UID to target UID from `COPYUID`.
    copied: BTreeMap<u64, u64>,
    /// The UID from `APPENDUID`.
    appended: Option<u64>,
    /// The UIDs a conditional `STORE` left alone (RFC 7162 `MODIFIED`).
    modified: BTreeSet<u64>,
}

impl Answer {
    fn absorb(&mut self, response: &Response<'_>) {
        let (Response::Done { outcome, .. } | Response::Data { outcome, .. }) = response else {
            return;
        };
        match &outcome.code {
            Some(ResponseCode::CopyUid(_, from, to)) => {
                self.copied.extend(expand(from).into_iter().zip(expand(to)));
            }
            Some(ResponseCode::AppendUid(_, uids)) => {
                self.appended = expand(uids).first().copied();
            }
            _ => {}
        }
        if let Some(text) = outcome.information.as_deref() {
            self.modified.extend(modified(text));
        }
    }
}

/// The set in a `[MODIFIED <set>]` code, which `imap-proto` does not parse and leaves in the
/// text.
fn modified(text: &str) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    let Some(start) = text.to_ascii_uppercase().find("[MODIFIED ") else {
        return out;
    };
    let rest = &text[start + "[MODIFIED ".len()..];
    let set = rest.split(']').next().unwrap_or_default();
    for part in set.split(',') {
        let bounds: Vec<u64> = part
            .split(':')
            .filter_map(|n| n.trim().parse().ok())
            .collect();
        match bounds[..] {
            [one] => {
                out.insert(one);
            }
            [a, b] => out.extend(a.min(b)..=a.max(b)),
            _ => {}
        }
    }
    out
}

fn refused(
    command: &str,
    status: &Status,
    outcome_text: Option<&str>,
    code: Option<&ResponseCode<'_>>,
) -> Failure {
    let verb = command
        .split_whitespace()
        .take(if command.starts_with("UID ") { 2 } else { 1 })
        .collect::<Vec<_>>()
        .join(" ");
    let trycreate = matches!(code, Some(ResponseCode::TryCreate));
    let text = format!("{status:?} {}", outcome_text.unwrap_or_default());
    Failure::Refused {
        verb,
        text,
        trycreate,
    }
}

async fn finish_command(
    session: &mut Session,
    command: &str,
    id: &async_imap::imap_proto::RequestId,
) -> Result<Answer, Failure> {
    let mut answer = Answer::default();
    loop {
        let response = session
            .read_response()
            .await
            .map_err(|e| SourceError::Connection(e.to_string()))?
            .ok_or_else(|| SourceError::Connection("connection lost".into()))?;
        if let Response::Done {
            tag,
            status,
            outcome,
        } = response.parsed()
            && tag == id
        {
            if *status != Status::Ok {
                return Err(refused(
                    command,
                    status,
                    outcome.information.as_deref(),
                    outcome.code.as_ref(),
                ));
            }
            answer.absorb(response.parsed());
            return Ok(answer);
        }
        answer.absorb(response.parsed());
    }
}

/// Sends `command` and reads every response up to its tagged answer, which comes last.
async fn exchange(session: &mut Session, command: &str) -> Result<Answer, Failure> {
    let id = session.run_command(command).await.map_err(convert)?;
    finish_command(session, command, &id).await
}

/// `APPEND` with `raw` as a synchronizing literal; a server that refuses before the
/// continuation answers with its tagged `NO` instead.
async fn append(session: &mut Session, command: &str, raw: &[u8]) -> Result<Answer, Failure> {
    let id = session.run_command(command).await.map_err(convert)?;
    loop {
        let response = session
            .read_response()
            .await
            .map_err(|e| SourceError::Connection(e.to_string()))?
            .ok_or_else(|| SourceError::Connection("connection lost".into()))?;
        match response.parsed() {
            Response::Continue(_) => break,
            Response::Done {
                tag,
                status,
                outcome,
            } if tag == &id => {
                return Err(refused(
                    command,
                    status,
                    outcome.information.as_deref(),
                    outcome.code.as_ref(),
                ));
            }
            _ => {}
        }
    }
    let stream = session.get_mut();
    let io = |e: std::io::Error| SourceError::Connection(e.to_string());
    stream.write_all(raw).await.map_err(io)?;
    stream.write_all(b"\r\n").await.map_err(io)?;
    stream.flush().await.map_err(io)?;
    finish_command(session, command, &id).await
}
