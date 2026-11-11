use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, DeviceScope, FolderId, Timestamp};
use holtmail_store::FolderRole;

use super::{Harness, Scenario, Setup, World, Write, mail};
use crate::{
    EmailRef, Filter, FlagChange, Lock, MailViewError, Mailbox, MailboxId, NewEmail, Outcome,
    Refusal, TextField, Viewer,
};

const INBOX: MailboxId = MailboxId::Role(FolderRole::Inbox);
const TRASH: MailboxId = MailboxId::Role(FolderRole::Trash);
const SENT: MailboxId = MailboxId::Role(FolderRole::Sent);

/// Checks that `harness`'s instances keep the [`MailView`](crate::MailView) and
/// [`MailActions`](crate::MailActions) contracts. Panics on the first violation.
pub async fn conformance(harness: &dyn Harness) {
    layouts(harness).await;
    access(harness).await;
    listing(harness).await;
    fetching(harness).await;
    flags(harness).await;
    flags_without_local(harness).await;
    moves(harness).await;
    copies(harness).await;
    removing(harness).await;
    appending(harness).await;
    subscriptions(harness).await;
    changes(harness).await;
    search(harness).await;
}

async fn two(harness: &dyn Harness) -> World {
    harness
        .start(&Scenario::two_accounts(), Setup::default())
        .await
}

fn folder(w: &World, account: &str, path: &str) -> MailboxId {
    MailboxId::Folder(w.folder(account, path))
}

fn by_path<'a>(tree: &'a [Mailbox], path: &[&str]) -> &'a Mailbox {
    tree.iter()
        .find(|m| m.path == path)
        .unwrap_or_else(|| panic!("no mailbox {path:?}"))
}

async fn members(w: &World, viewer: &Viewer, mailbox: &MailboxId) -> Vec<EmailRef> {
    w.view
        .select(viewer, mailbox)
        .await
        .unwrap()
        .members
        .into_iter()
        .map(|m| m.email)
        .collect()
}

async fn uid(w: &World, viewer: &Viewer, mailbox: &MailboxId, email: EmailRef) -> Option<u32> {
    w.view
        .select(viewer, mailbox)
        .await
        .unwrap()
        .members
        .into_iter()
        .find(|m| m.email == email)
        .map(|m| m.uid)
}

async fn modseq(w: &World, viewer: &Viewer, mailbox: &MailboxId, email: EmailRef) -> u64 {
    w.view
        .select(viewer, mailbox)
        .await
        .unwrap()
        .members
        .into_iter()
        .find(|m| m.email == email)
        .expect("member")
        .modseq
}

async fn shown(
    w: &World,
    viewer: &Viewer,
    mailbox: Option<&MailboxId>,
    email: EmailRef,
) -> Vec<String> {
    w.view
        .flags(viewer, mailbox, &[email])
        .await
        .unwrap()
        .remove(&email)
        .unwrap_or_default()
}

fn change(add: &[&str], remove: &[&str]) -> FlagChange {
    FlagChange {
        add: add.iter().map(|f| f.to_string()).collect(),
        remove: remove.iter().map(|f| f.to_string()).collect(),
    }
}

fn has(flags: &[String], flag: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
}

async fn set(
    w: &World,
    viewer: &Viewer,
    mailbox: Option<&MailboxId>,
    email: EmailRef,
    add: &[&str],
    remove: &[&str],
) -> Outcome {
    let results = w
        .actions
        .set_flags(viewer, mailbox, &[email], &change(add, remove), None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].email, email);
    results[0].outcome
}

fn not_found<T: std::fmt::Debug>(result: Result<T, MailViewError>, what: &str, context: &str) {
    match result {
        Err(MailViewError::NotFound(w)) if w == what => {}
        other => panic!("{context}: expected NotFound({what}), got {other:?}"),
    }
}

fn fresh(subject: &str, id: &str) -> Bytes {
    Bytes::from(mail(
        "Gina Park <gina@example.org>",
        "Ada Lovelace <ada@example.com>",
        subject,
        id,
        "Sat, 07 Nov 2026 09:00:00 +0000",
        "",
        "A new message.\r\n",
    ))
}

fn new_email(raw: Bytes) -> NewEmail {
    NewEmail {
        raw,
        flags: vec!["\\Seen".into()],
        received_at: Some(Timestamp::from_secs(1_794_042_000)),
    }
}

async fn layouts(harness: &dyn Harness) {
    let w = two(harness).await;
    let tree = w.view.tree(&w.editor).await.unwrap();
    for (i, mailbox) in tree.iter().enumerate() {
        for len in 1..mailbox.path.len() {
            let parent = tree
                .iter()
                .position(|m| m.path[..] == mailbox.path[..len])
                .unwrap_or_else(|| panic!("no parent of {:?}", mailbox.path));
            assert!(parent < i, "parent after child: {:?}", mailbox.path);
        }
        assert!(
            mailbox
                .path
                .iter()
                .all(|level| !level.contains('/') && !level.is_empty())
        );
        let same = tree
            .iter()
            .filter(|m| {
                m.path
                    .join("/")
                    .eq_ignore_ascii_case(&mailbox.path.join("/"))
            })
            .count();
        assert_eq!(same, 1, "duplicate {:?}", mailbox.path);
    }
    let roles = [
        (["INBOX"], FolderRole::Inbox),
        (["Sent"], FolderRole::Sent),
        (["Junk"], FolderRole::Junk),
        (["Trash"], FolderRole::Trash),
        (["Archive"], FolderRole::Archive),
    ];
    for (path, role) in roles {
        let mailbox = by_path(&tree, &path);
        assert_eq!(mailbox.id, MailboxId::Role(role));
        assert_eq!(mailbox.role, Some(role));
        assert!(mailbox.subscribed && mailbox.selectable, "{path:?}");
    }
    let drafts = by_path(&tree, &["Drafts"]);
    assert_eq!(
        (drafts.id.clone(), drafts.role, drafts.subscribed),
        (MailboxId::Drafts, Some(FolderRole::Drafts), true)
    );
    let all = by_path(&tree, &["All Mail"]);
    assert_eq!(
        (all.id.clone(), all.role, all.subscribed),
        (MailboxId::All, Some(FolderRole::All), false)
    );
    assert!(!all.rights.add && !all.rights.remove);
    let kept = by_path(&tree, &["Only in Holtmail"]);
    assert_eq!(
        (kept.id.clone(), kept.role, kept.subscribed),
        (MailboxId::Kept, None, false)
    );
    for path in [
        &["Accounts"][..],
        &["Accounts", "Work"],
        &["Accounts", "Work", "Projects"],
    ] {
        let node = by_path(&tree, path);
        assert!(!node.selectable, "{path:?}");
        assert!(matches!(node.id, MailboxId::Node(_)));
        assert_eq!(node.rights, Default::default());
    }
    let work_inbox = by_path(&tree, &["Accounts", "Work", "INBOX"]);
    assert_eq!(work_inbox.id, folder(&w, "Work", "INBOX"));
    assert_eq!(work_inbox.role, None, "no second inbox");
    assert!(!work_inbox.subscribed);
    by_path(&tree, &["Accounts", "Work", "Drafts"]);
    let projects = by_path(&tree, &["Accounts", "Work", "Projects", "2026"]);
    assert_eq!(projects.id, folder(&w, "Work", "Projects/2026"));
    assert!(projects.subscribed && projects.role.is_none());
    assert!(projects.rights.add && projects.rights.remove && projects.rights.set_seen);
    assert!(!projects.rights.local_flags);
    let archive = by_path(&tree, &["Accounts", "Home", "Archive"]);
    assert!(!archive.rights.add && !archive.rights.remove);
    assert!(archive.rights.set_seen && archive.rights.set_keywords && archive.rights.local_flags);
    let inbox = by_path(&tree, &["INBOX"]);
    assert!(inbox.rights.add && inbox.rights.remove && inbox.rights.set_seen);
    let kept = by_path(&tree, &["Only in Holtmail"]);
    assert!(kept.rights.set_seen && kept.rights.local_flags && !kept.rights.add);

    let reader_tree = w.view.tree(&w.reader).await.unwrap();
    let projects = by_path(&reader_tree, &["Accounts", "Work", "Projects", "2026"]);
    assert!(!projects.rights.add && !projects.rights.remove);
    assert!(projects.rights.set_seen && projects.rights.local_flags);
    let member_tree = w.view.tree(&w.member).await.unwrap();
    assert!(!by_path(&member_tree, &["INBOX"]).rights.add);

    let w = harness
        .start(&Scenario::one_account(), Setup::default())
        .await;
    let tree = w.view.tree(&w.editor).await.unwrap();
    let inbox = by_path(&tree, &["INBOX"]);
    assert_eq!(
        (inbox.id.clone(), inbox.role),
        (folder(&w, "Work", "INBOX"), Some(FolderRole::Inbox))
    );
    let sent = by_path(&tree, &["Sent"]);
    assert_eq!(
        (sent.id.clone(), sent.role),
        (folder(&w, "Work", "Sent"), Some(FolderRole::Sent))
    );
    assert_eq!(by_path(&tree, &["Trash"]).role, Some(FolderRole::Trash));
    assert_eq!(by_path(&tree, &["Drafts"]).id, MailboxId::Drafts);
    assert!(!by_path(&tree, &["Projects"]).selectable);
    by_path(&tree, &["Projects", "2026"]);
    by_path(&tree, &["All Mail"]);
    by_path(&tree, &["Only in Holtmail"]);
    assert!(tree.iter().all(|m| m.path[0] != "Accounts"));
    assert!(!tree.iter().any(|m| matches!(m.id, MailboxId::Role(_))));
    assert!(
        !tree.iter().any(|m| m.id == folder(&w, "Work", "Drafts")),
        "the drafts folder is part of Drafts"
    );
    not_found(
        w.view.select(&w.editor, &INBOX).await,
        "mailbox",
        "a role mailbox with one account",
    );
    assert!(by_path(&tree, &["Sent"]).subscribed);

    let w = harness
        .start(&Scenario::two_accounts(), Setup { local_flags: false })
        .await;
    let tree = w.view.tree(&w.editor).await.unwrap();
    let archive = by_path(&tree, &["Accounts", "Home", "Archive"]);
    assert!(!archive.rights.set_seen && !archive.rights.local_flags);
    let kept = by_path(&tree, &["Only in Holtmail"]);
    assert!(!kept.rights.set_seen);
}

async fn access(harness: &dyn Harness) {
    let w = two(harness).await;
    let other = Viewer {
        archive: ArchiveId::new(),
        ..w.editor.clone()
    };
    let unscoped = Viewer {
        scopes: [DeviceScope::WriteMail].into_iter().collect(),
        ..w.editor.clone()
    };
    let welcome = w.email("welcome");
    for viewer in [&w.stranger, &other, &unscoped] {
        not_found(w.view.tree(viewer).await, "archive", "tree");
        not_found(w.view.status(viewer, &INBOX).await, "archive", "status");
        not_found(w.view.select(viewer, &INBOX).await, "archive", "select");
        not_found(
            w.view.mailbox_changes(viewer, &INBOX, 0).await,
            "archive",
            "mailbox_changes",
        );
        not_found(
            w.view.flags(viewer, None, &[welcome]).await,
            "archive",
            "flags",
        );
        not_found(
            w.view.envelopes(viewer, &[welcome]).await,
            "archive",
            "envelopes",
        );
        not_found(w.view.raw(viewer, &welcome).await, "archive", "raw");
        not_found(
            w.view.structure(viewer, &welcome).await,
            "archive",
            "structure",
        );
        not_found(
            w.view.search(viewer, &INBOX, &Filter::All).await,
            "archive",
            "search",
        );
        not_found(w.view.state(viewer).await, "archive", "state");
        not_found(w.view.changes(viewer, 0, 10).await, "archive", "changes");
        not_found(
            w.actions
                .set_flags(viewer, None, &[welcome], &change(&["\\Seen"], &[]), None)
                .await,
            "archive",
            "set_flags",
        );
        not_found(
            w.actions.move_to(viewer, &INBOX, &[welcome], &TRASH).await,
            "archive",
            "move",
        );
        not_found(
            w.actions.copy_to(viewer, &INBOX, &[welcome], &TRASH).await,
            "archive",
            "copy",
        );
        not_found(
            w.actions.remove(viewer, None, &[welcome]).await,
            "archive",
            "remove",
        );
        not_found(
            w.actions
                .append(viewer, &MailboxId::Drafts, new_email(fresh("Hi", "hi")))
                .await,
            "archive",
            "append",
        );
        not_found(
            w.actions.subscribe(viewer, &INBOX, false).await,
            "archive",
            "subscribe",
        );
    }
    let unknown = MailboxId::Folder(FolderId::new());
    not_found(
        w.view.select(&w.editor, &unknown).await,
        "mailbox",
        "unknown folder",
    );
    not_found(
        w.view
            .select(&w.editor, &MailboxId::Node("Accounts".into()))
            .await,
        "mailbox",
        "placeholder",
    );
    not_found(
        w.actions.subscribe(&w.editor, &unknown, true).await,
        "mailbox",
        "subscribe unknown",
    );
    not_found(
        w.actions
            .move_to(&w.editor, &INBOX, &[welcome], &unknown)
            .await,
        "mailbox",
        "move to unknown",
    );
}

async fn listing(harness: &dyn Harness) {
    let w = two(harness).await;
    let (welcome, shared, reply) = (w.email("welcome"), w.email("shared"), w.email("reply"));
    let snapshot = w.view.select(&w.editor, &INBOX).await.unwrap();
    let emails: Vec<EmailRef> = snapshot.members.iter().map(|m| m.email).collect();
    assert_eq!(
        emails,
        vec![welcome, shared, reply],
        "archive order, one place per message"
    );
    assert!(snapshot.members.windows(2).all(|p| p[0].uid < p[1].uid));
    let status = snapshot.status;
    assert_eq!((status.messages, status.unseen), (3, 2));
    assert!(status.uidvalidity != 0);
    assert!(status.uidnext > snapshot.members.last().unwrap().uid);
    assert!(
        snapshot
            .members
            .iter()
            .all(|m| m.modseq <= status.highest_modseq)
    );
    assert_eq!(snapshot.mailbox.path, vec!["INBOX".to_string()]);
    assert_eq!(w.view.status(&w.editor, &INBOX).await.unwrap(), status);
    let again = w.view.select(&w.editor, &INBOX).await.unwrap();
    assert_eq!(again.members, snapshot.members);
    let theirs = w.view.select(&w.member, &INBOX).await.unwrap();
    assert_eq!(theirs.members, snapshot.members, "one shared view");

    assert_eq!(
        members(&w, &w.editor, &MailboxId::Kept).await,
        vec![w.email("gone")]
    );
    assert_eq!(members(&w, &w.editor, &MailboxId::All).await.len(), 7);
    assert_eq!(
        members(&w, &w.editor, &folder(&w, "Home", "Archive")).await,
        vec![w.email("old")]
    );
    assert_eq!(
        members(&w, &w.editor, &folder(&w, "Work", "INBOX")).await,
        vec![welcome, shared, reply]
    );
    assert_eq!(members(&w, &w.editor, &SENT).await, vec![w.email("sent")]);
    assert!(members(&w, &w.editor, &MailboxId::Drafts).await.is_empty());

    assert_eq!(
        shown(&w, &w.editor, Some(&INBOX), shared).await,
        vec!["\\Flagged", "\\Seen"]
    );
    assert_eq!(
        shown(&w, &w.editor, Some(&folder(&w, "Work", "INBOX")), shared).await,
        vec!["\\Seen"]
    );
    assert_eq!(
        shown(&w, &w.editor, None, shared).await,
        vec!["\\Flagged", "\\Seen"]
    );
    assert_eq!(
        shown(&w, &w.editor, Some(&MailboxId::Kept), w.email("gone")).await,
        vec!["\\Seen"]
    );
    assert!(shown(&w, &w.editor, Some(&INBOX), welcome).await.is_empty());

    let before = w.view.status(&w.editor, &INBOX).await.unwrap();
    let raw = mail(
        "Hank Lee <hank@example.org>",
        BOB_ADDR,
        "Fresh delivery",
        "fresh",
        "Sat, 07 Nov 2026 08:00:00 +0000",
        "",
        "Just arrived.\r\n",
    );
    let id = w.driver.deliver("Work", "INBOX", &raw).await;
    let after = w.view.select(&w.editor, &INBOX).await.unwrap();
    let last = after.members.last().unwrap();
    assert_eq!(last.email, EmailRef::Message(id));
    assert!(last.uid >= before.uidnext);
    assert!(after.status.uidnext > last.uid);
    assert!(after.status.highest_modseq > before.highest_modseq);
    assert_eq!(after.status.uidvalidity, before.uidvalidity);
    assert_eq!((after.status.messages, after.status.unseen), (4, 3));
}

const BOB_ADDR: &str = "Bob Builder <bob@example.org>";

async fn fetching(harness: &dyn Harness) {
    let w = two(harness).await;
    let scenario = Scenario::two_accounts();
    let (welcome, reply, report) = (w.email("welcome"), w.email("reply"), w.email("report"));
    let envelopes = w
        .view
        .envelopes(
            &w.editor,
            &[
                reply,
                welcome,
                EmailRef::Message(holtmail_core::MessageId::new()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(envelopes.len(), 2, "unknown left out");
    let (r, e) = (&envelopes[0], &envelopes[1]);
    assert_eq!((r.email, e.email), (reply, welcome), "order asked");
    let raw = scenario.raw("welcome");
    assert_eq!(e.subject.as_deref(), Some("Welcome aboard"));
    assert_eq!(e.from.len(), 1);
    assert_eq!(e.from[0].email, "ada@example.com");
    assert_eq!(e.from[0].name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(e.to[0].email, "bob@example.org");
    assert!(e.sender.is_empty() && e.reply_to.is_empty() && e.cc.is_empty());
    assert_eq!(e.rfc_message_id.as_deref(), Some("welcome@example.com"));
    assert_eq!(e.sent_at, Some(Timestamp::from_secs(1_793_610_000)));
    assert_eq!(e.received_at, Timestamp::from_secs(1_793_610_000));
    assert_eq!((e.size, e.crlf_size), (raw.len() as u64, raw.len() as u64));
    assert_eq!(e.blob, BlobId::of(raw.as_bytes()));
    assert_eq!(e.preview, "Glad to have you here.");
    assert!(!e.has_attachment);
    assert!(e.thread.is_some());
    assert_eq!(r.thread, e.thread, "a reply joins the thread");
    assert_eq!(r.in_reply_to.as_deref(), Some("welcome@example.com"));
    for mailbox in [INBOX, folder(&w, "Work", "INBOX"), MailboxId::All] {
        assert!(e.mailboxes.contains(&mailbox), "{mailbox}");
    }
    assert!(!e.mailboxes.contains(&MailboxId::Kept));
    let thread = w.view.thread(&w.editor, e.thread.unwrap()).await.unwrap();
    assert_eq!(thread, vec![welcome, reply]);
    not_found(
        w.view
            .thread(&w.editor, holtmail_core::ThreadId::new())
            .await,
        "thread",
        "unknown thread",
    );

    assert_eq!(
        w.view.raw(&w.editor, &welcome).await.unwrap(),
        raw.as_bytes()
    );
    assert_eq!(
        w.view.blob(&w.editor, &e.blob).await.unwrap(),
        raw.as_bytes()
    );
    not_found(
        w.view.blob(&w.editor, &BlobId::of(b"nothing")).await,
        "blob",
        "unknown blob",
    );
    not_found(
        w.view
            .raw(
                &w.editor,
                &EmailRef::Message(holtmail_core::MessageId::new()),
            )
            .await,
        "email",
        "unknown email",
    );

    let structure = w.view.structure(&w.editor, &welcome).await.unwrap();
    assert_eq!(structure.id, "");
    assert_eq!(structure.content_type, "text/plain");
    assert!(structure.parts.is_empty());
    assert_eq!(structure.header.start, 0);
    let body = &raw.as_bytes()[structure.body.start as usize..structure.body.end as usize];
    assert_eq!(body, b"Glad to have you here.\r\n");
    assert!(raw.as_bytes()[..structure.header.end as usize].ends_with(b"\r\n\r\n"));

    let raw = scenario.raw("report");
    let bytes = raw.as_bytes();
    let cut = |r: crate::ByteRange| &bytes[r.start as usize..r.end as usize];
    let structure = w.view.structure(&w.editor, &report).await.unwrap();
    assert_eq!(structure.content_type, "multipart/mixed");
    assert!(structure.params.contains(&("boundary".into(), "b1".into())));
    assert_eq!(structure.parts.len(), 2);
    let (text, csv) = (&structure.parts[0], &structure.parts[1]);
    assert_eq!((text.id.as_str(), csv.id.as_str()), ("1", "2"));
    assert_eq!(text.content_type, "text/plain");
    assert!(text.params.contains(&("charset".into(), "utf-8".into())));
    assert_eq!(cut(text.body), b"Here are the numbers.");
    assert_eq!(
        cut(text.header),
        b"Content-Type: text/plain; charset=utf-8\r\n\r\n"
    );
    assert_eq!(csv.content_type, "text/csv");
    assert_eq!(csv.disposition.as_deref(), Some("attachment"));
    assert!(
        csv.disposition_params
            .contains(&("filename".into(), "report.csv".into()))
    );
    assert_eq!(csv.transfer_encoding, "base64");
    assert_eq!(text.transfer_encoding, "7bit");
    assert_eq!(cut(csv.body), b"YSxiLGMKMSwyLDMK");
    let envelope = &w.view.envelopes(&w.editor, &[report]).await.unwrap()[0];
    assert!(envelope.has_attachment);
    assert_eq!(envelope.preview, "Here are the numbers.");
}

async fn flags(harness: &dyn Harness) {
    let w = two(harness).await;
    let (welcome, shared, reply, old, gone) = (
        w.email("welcome"),
        w.email("shared"),
        w.email("reply"),
        w.email("old"),
        w.email("gone"),
    );
    let home_archive = folder(&w, "Home", "Archive");
    let work_inbox = folder(&w, "Work", "INBOX");

    let before = modseq(&w, &w.editor, &INBOX, welcome).await;
    let results = w
        .actions
        .set_flags(
            &w.editor,
            Some(&INBOX),
            &[welcome, welcome],
            &change(&["\\Flagged"], &[]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1, "one result per email");
    assert_eq!(results[0].outcome, Outcome::Held);
    assert!(has(&results[0].flags, "\\Flagged"));
    assert!(results[0].modseq > before);
    assert_eq!(
        results[0].modseq,
        modseq(&w, &w.editor, &INBOX, welcome).await
    );
    assert!(
        has(
            &shown(&w, &w.editor, Some(&work_inbox), welcome).await,
            "\\Flagged"
        ),
        "held shows at once"
    );
    assert!(
        has(
            &shown(&w, &w.member, Some(&INBOX), welcome).await,
            "\\Flagged"
        ),
        "held is shared"
    );
    let at = modseq(&w, &w.editor, &INBOX, welcome).await;
    assert_eq!(
        set(&w, &w.editor, Some(&INBOX), welcome, &["\\flagged"], &[]).await,
        Outcome::Applied
    );
    assert_eq!(
        modseq(&w, &w.editor, &INBOX, welcome).await,
        at,
        "nothing to do writes nothing"
    );

    let theirs = modseq(&w, &w.member, &home_archive, old).await;
    assert_eq!(
        set(&w, &w.editor, Some(&home_archive), old, &["\\Flagged"], &[]).await,
        Outcome::Applied
    );
    assert!(has(
        &shown(&w, &w.editor, Some(&home_archive), old).await,
        "\\Flagged"
    ));
    assert!(
        has(&shown(&w, &w.reader, None, old).await, "\\Flagged"),
        "local flags are the user's"
    );
    assert!(
        !has(
            &shown(&w, &w.member, Some(&home_archive), old).await,
            "\\Flagged"
        ),
        "not another user's"
    );
    assert!(
        modseq(&w, &w.member, &home_archive, old).await > theirs,
        "MODSEQ rises for everyone"
    );

    assert_eq!(
        set(&w, &w.editor, Some(&INBOX), shared, &[], &["\\Seen"]).await,
        Outcome::Held
    );
    assert!(
        !has(&shown(&w, &w.editor, Some(&INBOX), shared).await, "\\Seen"),
        "every place, local where closed"
    );
    assert!(has(
        &shown(&w, &w.member, Some(&folder(&w, "Home", "INBOX")), shared).await,
        "\\Seen"
    ));

    assert_eq!(
        set(&w, &w.reader, Some(&INBOX), reply, &["\\Seen"], &[]).await,
        Outcome::Applied
    );
    assert!(has(
        &shown(&w, &w.reader, Some(&INBOX), reply).await,
        "\\Seen"
    ));
    assert!(!has(
        &shown(&w, &w.member, Some(&INBOX), reply).await,
        "\\Seen"
    ));
    assert_eq!(
        set(
            &w,
            &w.member,
            Some(&work_inbox),
            reply,
            &["$Important"],
            &[]
        )
        .await,
        Outcome::Applied
    );
    assert!(has(
        &shown(&w, &w.member, Some(&work_inbox), reply).await,
        "$Important"
    ));
    assert!(!has(
        &shown(&w, &w.editor, Some(&work_inbox), reply).await,
        "$Important"
    ));

    assert_eq!(
        set(
            &w,
            &w.editor,
            Some(&MailboxId::Kept),
            gone,
            &[],
            &["\\Seen"]
        )
        .await,
        Outcome::Applied
    );
    assert!(
        !has(
            &shown(&w, &w.editor, Some(&MailboxId::Kept), gone).await,
            "\\Seen"
        ),
        "a cleared local flag hides a synced one"
    );
    assert_eq!(
        set(
            &w,
            &w.editor,
            Some(&MailboxId::Kept),
            gone,
            &["\\Deleted"],
            &[]
        )
        .await,
        Outcome::Refused(Refusal::Cannot)
    );
    assert_eq!(
        set(&w, &w.editor, Some(&home_archive), old, &["\\Deleted"], &[]).await,
        Outcome::Refused(Refusal::Locked(Lock::Capability))
    );
    assert!(!has(
        &shown(&w, &w.editor, Some(&home_archive), old).await,
        "\\Deleted"
    ));
    assert_eq!(
        set(&w, &w.editor, Some(&work_inbox), reply, &["\\Deleted"], &[]).await,
        Outcome::Held
    );
    assert_eq!(
        set(
            &w,
            &w.editor,
            Some(&home_archive),
            welcome,
            &["\\Seen"],
            &[]
        )
        .await,
        Outcome::Refused(Refusal::NotFound)
    );

    let current = modseq(&w, &w.editor, &INBOX, welcome).await;
    let results = w
        .actions
        .set_flags(
            &w.editor,
            Some(&INBOX),
            &[welcome],
            &change(&["\\Answered"], &[]),
            Some(current - 1),
        )
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Refused(Refusal::Modified));
    assert!(!has(&results[0].flags, "\\Answered"));
    let results = w
        .actions
        .set_flags(
            &w.editor,
            Some(&INBOX),
            &[welcome],
            &change(&["\\Answered"], &[]),
            Some(current),
        )
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);

    assert!(matches!(
        w.actions
            .set_flags(
                &w.editor,
                None,
                &[welcome],
                &change(&["\\Seen"], &["\\seen"]),
                None
            )
            .await,
        Err(MailViewError::Invalid(_))
    ));

    let before = modseq(&w, &w.editor, &INBOX, welcome).await;
    w.driver.settle(false).await;
    let flags = shown(&w, &w.editor, Some(&INBOX), welcome).await;
    assert!(
        !has(&flags, "\\Flagged") && !has(&flags, "\\Answered"),
        "the server wins"
    );
    assert!(modseq(&w, &w.editor, &INBOX, welcome).await > before);
    assert_eq!(
        set(&w, &w.editor, Some(&INBOX), welcome, &["\\Flagged"], &[]).await,
        Outcome::Held
    );
    w.driver.settle(true).await;
    assert!(has(
        &shown(&w, &w.editor, Some(&work_inbox), welcome).await,
        "\\Flagged"
    ));
    assert!(
        has(
            &shown(&w, &w.editor, Some(&home_archive), old).await,
            "\\Flagged"
        ),
        "local flags stay"
    );
}

async fn flags_without_local(harness: &dyn Harness) {
    let w = harness
        .start(&Scenario::two_accounts(), Setup { local_flags: false })
        .await;
    let home_archive = folder(&w, "Home", "Archive");
    assert_eq!(
        set(
            &w,
            &w.editor,
            Some(&home_archive),
            w.email("old"),
            &["\\Flagged"],
            &[]
        )
        .await,
        Outcome::Refused(Refusal::Locked(Lock::Capability))
    );
    assert_eq!(
        set(
            &w,
            &w.reader,
            Some(&INBOX),
            w.email("welcome"),
            &["\\Seen"],
            &[]
        )
        .await,
        Outcome::Refused(Refusal::Locked(Lock::Scope))
    );
    assert_eq!(
        set(
            &w,
            &w.member,
            Some(&INBOX),
            w.email("welcome"),
            &["\\Seen"],
            &[]
        )
        .await,
        Outcome::Refused(Refusal::Locked(Lock::Grant))
    );
    assert!(!has(
        &shown(&w, &w.editor, None, w.email("old")).await,
        "\\Flagged"
    ));
    assert_eq!(
        set(
            &w,
            &w.editor,
            Some(&INBOX),
            w.email("welcome"),
            &["\\Seen"],
            &[]
        )
        .await,
        Outcome::Held
    );
}

async fn moves(harness: &dyn Harness) {
    let w = two(harness).await;
    let (welcome, reply, old) = (w.email("welcome"), w.email("reply"), w.email("old"));
    let projects = folder(&w, "Work", "Projects/2026");
    let inbox = w.view.select(&w.editor, &INBOX).await.unwrap();
    let old_uid = uid(&w, &w.editor, &INBOX, welcome).await.unwrap();

    let results = w
        .actions
        .move_to(&w.editor, &INBOX, &[welcome], &projects)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, Outcome::Held);
    let target = results[0].uid.expect("COPYUID");
    assert_eq!(uid(&w, &w.editor, &projects, welcome).await, Some(target));
    assert_eq!(uid(&w, &w.editor, &INBOX, welcome).await, None);
    let changes = w
        .view
        .mailbox_changes(&w.editor, &INBOX, inbox.status.highest_modseq)
        .await
        .unwrap();
    assert!(changes.vanished.contains(&old_uid));
    assert!(changes.status.highest_modseq > inbox.status.highest_modseq);
    w.driver.settle(true).await;
    assert_eq!(
        uid(&w, &w.editor, &projects, welcome).await,
        Some(target),
        "the UID is the message's"
    );
    assert_eq!(uid(&w, &w.editor, &INBOX, welcome).await, None);

    let results = w
        .actions
        .move_to(&w.editor, &INBOX, &[reply], &TRASH)
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);
    assert_eq!(uid(&w, &w.editor, &TRASH, reply).await, results[0].uid);
    assert!(results[0].uid.is_some());

    let report = w.email("report");
    assert_eq!(
        set(&w, &w.editor, Some(&projects), report, &["\\Flagged"], &[]).await,
        Outcome::Held
    );
    let results = w
        .actions
        .move_to(&w.editor, &projects, &[report], &INBOX)
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);
    assert_eq!(
        shown(&w, &w.editor, Some(&INBOX), report).await,
        vec!["\\Flagged", "\\Seen"],
        "a held move keeps the flags in its target"
    );
    let status = w.view.select(&w.editor, &INBOX).await.unwrap().status;
    let unseen_before = status.unseen;
    w.driver.settle(true).await;
    assert_eq!(
        shown(&w, &w.editor, Some(&INBOX), report).await,
        vec!["\\Flagged", "\\Seen"],
        "and after sync confirms it"
    );
    assert_eq!(
        w.view
            .select(&w.editor, &INBOX)
            .await
            .unwrap()
            .status
            .unseen,
        unseen_before,
        "a moved read message is not counted unseen"
    );

    let archive = folder(&w, "Home", "Archive");
    for (to, refusal) in [
        (INBOX, Refusal::Locked(Lock::Capability)),
        (MailboxId::All, Refusal::Cannot),
        (MailboxId::Kept, Refusal::Cannot),
        (TRASH, Refusal::Cannot),
    ] {
        let results = w
            .actions
            .move_to(&w.editor, &archive, &[old], &to)
            .await
            .unwrap();
        assert_eq!(results[0].outcome, Outcome::Refused(refusal), "to {to}");
        assert_eq!(results[0].uid, None);
    }
    for from in [MailboxId::All, MailboxId::Kept] {
        let email = members(&w, &w.editor, &from).await[0];
        let results = w
            .actions
            .move_to(&w.editor, &from, &[email], &projects)
            .await
            .unwrap();
        assert_eq!(
            results[0].outcome,
            Outcome::Refused(Refusal::Cannot),
            "from {from}"
        );
    }
    let shared = w.email("shared");
    let results = w
        .actions
        .move_to(&w.editor, &INBOX, &[old], &projects)
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Refused(Refusal::NotFound));
    let work_inbox = folder(&w, "Work", "INBOX");
    for (viewer, lock) in [(&w.reader, Lock::Scope), (&w.member, Lock::Grant)] {
        let results = w
            .actions
            .move_to(viewer, &work_inbox, &[shared], &projects)
            .await
            .unwrap();
        assert_eq!(results[0].outcome, Outcome::Refused(Refusal::Locked(lock)));
    }
    assert!(
        members(&w, &w.editor, &work_inbox).await.contains(&shared),
        "a refusal writes nothing"
    );

    let w = two(harness).await;
    let welcome = w.email("welcome");
    let projects = folder(&w, "Work", "Projects/2026");
    let first = uid(&w, &w.editor, &INBOX, welcome).await.unwrap();
    w.actions
        .move_to(&w.editor, &INBOX, &[welcome], &projects)
        .await
        .unwrap();
    assert_eq!(uid(&w, &w.editor, &INBOX, welcome).await, None);
    w.driver.settle(false).await;
    let back = uid(&w, &w.editor, &INBOX, welcome)
        .await
        .expect("back after a refused move");
    assert!(back > first, "a new UID, never a reused one");
    assert_eq!(uid(&w, &w.editor, &projects, welcome).await, None);
}

async fn copies(harness: &dyn Harness) {
    let w = two(harness).await;
    let (welcome, old) = (w.email("welcome"), w.email("old"));
    let projects = folder(&w, "Work", "Projects/2026");
    let results = w
        .actions
        .copy_to(&w.editor, &INBOX, &[welcome], &projects)
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);
    let copied = results[0].uid.expect("COPYUID");
    assert_eq!(uid(&w, &w.editor, &projects, welcome).await, Some(copied));
    assert!(
        uid(&w, &w.editor, &INBOX, welcome).await.is_some(),
        "a copy keeps the original"
    );
    let results = w
        .actions
        .copy_to(&w.editor, &INBOX, &[welcome], &projects)
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Applied, "already there");
    assert_eq!(results[0].uid, Some(copied));

    let results = w
        .actions
        .copy_to(&w.editor, &MailboxId::All, &[old], &projects)
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held, "copy out of All Mail");
    assert!(uid(&w, &w.editor, &projects, old).await.is_some());

    for (to, refusal) in [
        (
            folder(&w, "Home", "Archive"),
            Refusal::Locked(Lock::Capability),
        ),
        (MailboxId::All, Refusal::Cannot),
        (MailboxId::Kept, Refusal::Cannot),
        (MailboxId::Drafts, Refusal::Cannot),
    ] {
        let results = w
            .actions
            .copy_to(&w.editor, &INBOX, &[welcome], &to)
            .await
            .unwrap();
        assert_eq!(results[0].outcome, Outcome::Refused(refusal), "to {to}");
    }
    let results = w
        .actions
        .copy_to(&w.reader, &INBOX, &[w.email("reply")], &projects)
        .await
        .unwrap();
    assert_eq!(
        results[0].outcome,
        Outcome::Refused(Refusal::Locked(Lock::Scope))
    );
}

async fn removing(harness: &dyn Harness) {
    let w = two(harness).await;
    let (welcome, reply, old, gone) = (
        w.email("welcome"),
        w.email("reply"),
        w.email("old"),
        w.email("gone"),
    );
    let results = w
        .actions
        .remove(&w.editor, Some(&INBOX), &[welcome])
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);
    assert!(!members(&w, &w.editor, &INBOX).await.contains(&welcome));
    assert!(
        members(&w, &w.editor, &TRASH).await.contains(&welcome),
        "to the trash"
    );
    assert!(
        members(&w, &w.editor, &MailboxId::All)
            .await
            .contains(&welcome),
        "All Mail keeps it"
    );
    let results = w
        .actions
        .remove(&w.editor, Some(&TRASH), &[welcome])
        .await
        .unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);
    assert!(!members(&w, &w.editor, &TRASH).await.contains(&welcome));
    assert!(
        members(&w, &w.editor, &MailboxId::Kept)
            .await
            .contains(&welcome),
        "only in Holtmail"
    );

    let results = w.actions.remove(&w.editor, None, &[reply]).await.unwrap();
    assert_eq!(results[0].outcome, Outcome::Held);
    assert!(
        members(&w, &w.editor, &MailboxId::Kept)
            .await
            .contains(&reply)
    );
    assert!(
        members(&w, &w.editor, &MailboxId::All)
            .await
            .contains(&reply)
    );
    assert!(!members(&w, &w.editor, &INBOX).await.contains(&reply));
    assert!(
        w.view.raw(&w.editor, &reply).await.is_ok(),
        "never deleted from the archive"
    );
    let results = w.actions.remove(&w.editor, None, &[reply]).await.unwrap();
    assert_eq!(
        results[0].outcome,
        Outcome::Refused(Refusal::Cannot),
        "no place left"
    );

    for (mailbox, email) in [(MailboxId::All, old), (MailboxId::Kept, gone)] {
        let results = w
            .actions
            .remove(&w.editor, Some(&mailbox), &[email])
            .await
            .unwrap();
        assert_eq!(
            results[0].outcome,
            Outcome::Refused(Refusal::Cannot),
            "in {mailbox}"
        );
    }
    let results = w
        .actions
        .remove(&w.editor, Some(&folder(&w, "Home", "Archive")), &[old])
        .await
        .unwrap();
    assert_eq!(
        results[0].outcome,
        Outcome::Refused(Refusal::Locked(Lock::Capability))
    );
    let results = w
        .actions
        .remove(&w.reader, Some(&SENT), &[w.email("sent")])
        .await
        .unwrap();
    assert_eq!(
        results[0].outcome,
        Outcome::Refused(Refusal::Locked(Lock::Scope))
    );
    assert!(
        members(&w, &w.editor, &SENT)
            .await
            .contains(&w.email("sent"))
    );
}

async fn appending(harness: &dyn Harness) {
    let w = two(harness).await;
    let projects = folder(&w, "Work", "Projects/2026");
    let raw = fresh("Appended", "appended");
    let appended = w
        .actions
        .append(&w.editor, &projects, new_email(raw.clone()))
        .await
        .unwrap();
    assert_eq!(appended.outcome, Outcome::Held);
    let email = appended.email.expect("email");
    let at = appended.uid.expect("APPENDUID");
    assert_eq!(uid(&w, &w.editor, &projects, email).await, Some(at));
    assert_eq!(w.view.raw(&w.editor, &email).await.unwrap(), raw);
    assert!(has(
        &shown(&w, &w.editor, Some(&projects), email).await,
        "\\Seen"
    ));
    let again = w
        .actions
        .append(&w.editor, &projects, new_email(raw.clone()))
        .await
        .unwrap();
    assert_eq!(
        (again.email, again.uid),
        (Some(email), Some(at)),
        "the same bytes are the same email"
    );
    assert!(again.outcome.accepted());

    let all = members(&w, &w.editor, &MailboxId::All).await.len();
    let refused = w
        .actions
        .append(
            &w.editor,
            &folder(&w, "Home", "Archive"),
            new_email(fresh("No", "no")),
        )
        .await
        .unwrap();
    assert_eq!(
        refused.outcome,
        Outcome::Refused(Refusal::Locked(Lock::Capability))
    );
    assert_eq!((refused.email, refused.uid), (None, None));
    for mailbox in [
        MailboxId::All,
        MailboxId::Kept,
        MailboxId::Node("Accounts".into()),
    ] {
        let refused = w
            .actions
            .append(&w.editor, &mailbox, new_email(fresh("No", "no")))
            .await
            .unwrap();
        assert_eq!(
            refused.outcome,
            Outcome::Refused(Refusal::Cannot),
            "into {mailbox}"
        );
    }
    let refused = w
        .actions
        .append(&w.reader, &projects, new_email(fresh("No", "no")))
        .await
        .unwrap();
    assert_eq!(
        refused.outcome,
        Outcome::Refused(Refusal::Locked(Lock::Scope))
    );
    assert_eq!(
        members(&w, &w.editor, &MailboxId::All).await.len(),
        all,
        "a refused append archives nothing"
    );

    let first = w
        .actions
        .append(
            &w.editor,
            &MailboxId::Drafts,
            new_email(fresh("Draft", "draft1")),
        )
        .await
        .unwrap();
    assert_eq!(first.outcome, Outcome::Applied);
    let draft = first.email.expect("draft");
    assert!(matches!(draft, EmailRef::Draft { version: 1, .. }));
    let second = w
        .actions
        .append(
            &w.editor,
            &MailboxId::Drafts,
            new_email(fresh("Draft", "draft2")),
        )
        .await
        .unwrap();
    assert!(second.uid > first.uid, "each save a new UID");
    assert_eq!(
        uid(&w, &w.editor, &MailboxId::Drafts, draft).await,
        first.uid
    );
    assert_eq!(
        shown(&w, &w.editor, Some(&MailboxId::Drafts), draft).await,
        vec!["\\Draft", "\\Seen"]
    );
    assert_eq!(
        set(
            &w,
            &w.editor,
            Some(&MailboxId::Drafts),
            draft,
            &["\\Flagged"],
            &[]
        )
        .await,
        Outcome::Applied
    );
    assert!(
        members(&w, &w.member, &MailboxId::Drafts).await.is_empty(),
        "drafts are per user"
    );
    assert!(
        !members(&w, &w.editor, &MailboxId::All)
            .await
            .contains(&draft),
        "drafts are not archived"
    );
    let removed = w
        .actions
        .remove(&w.editor, Some(&MailboxId::Drafts), &[draft])
        .await
        .unwrap();
    assert_eq!(removed[0].outcome, Outcome::Applied);
    assert!(
        !members(&w, &w.editor, &MailboxId::Drafts)
            .await
            .contains(&draft)
    );
    not_found(
        w.view.raw(&w.editor, &draft).await,
        "email",
        "a deleted draft",
    );

    let all_before = members(&w, &w.editor, &MailboxId::All).await;
    let sent = w
        .actions
        .append(&w.reader, &SENT, new_email(fresh("My copy", "copy")))
        .await
        .unwrap();
    assert_eq!(
        sent.outcome,
        Outcome::Refused(Refusal::Locked(Lock::Scope)),
        "a read-only device cannot put mail into the archive through Sent"
    );
    assert_eq!(
        members(&w, &w.editor, &MailboxId::All).await,
        all_before,
        "a refused Sent copy is not archived"
    );

    let limit = w.actions.append_limit(&w.editor).await.unwrap();
    let mut big = fresh("Big", "big").to_vec();
    big.resize(limit as usize + 1, b'x');
    assert!(matches!(
        w.actions
            .append(&w.editor, &projects, new_email(Bytes::from(big)))
            .await,
        Err(MailViewError::TooLarge { .. })
    ));

    let mut scenario = Scenario::two_accounts();
    scenario.accounts[0].writes = &[Write::Flags, Write::Move, Write::Delete, Write::Expunge];
    let w = harness.start(&scenario, Setup::default()).await;
    let sent = w
        .actions
        .append(&w.editor, &SENT, new_email(fresh("My copy", "copy")))
        .await
        .unwrap();
    assert_eq!(
        sent.outcome,
        Outcome::Applied,
        "Sent takes a copy no account may append as a local place"
    );
    let copy = sent.email.expect("email");
    assert_eq!(uid(&w, &w.editor, &SENT, copy).await, sent.uid);
    assert!(
        members(&w, &w.reader, &SENT).await.contains(&copy),
        "the same person"
    );
    assert!(
        !members(&w, &w.member, &SENT).await.contains(&copy),
        "a local place is the user's"
    );
    for (viewer, lock) in [(&w.reader, Lock::Scope), (&w.member, Lock::Grant)] {
        let refused = w
            .actions
            .append(viewer, &SENT, new_email(fresh("Not mine", "not-mine")))
            .await
            .unwrap();
        assert_eq!(refused.outcome, Outcome::Refused(Refusal::Locked(lock)));
    }
    let removed = w
        .actions
        .remove(&w.editor, Some(&SENT), &[copy])
        .await
        .unwrap();
    assert_eq!(removed[0].outcome, Outcome::Applied);
    assert!(!members(&w, &w.editor, &SENT).await.contains(&copy));
}

async fn subscriptions(harness: &dyn Harness) {
    let w = two(harness).await;
    assert!(
        w.actions
            .subscribe(&w.editor, &MailboxId::All, true)
            .await
            .unwrap()
    );
    assert!(
        !w.actions
            .subscribe(&w.editor, &MailboxId::All, true)
            .await
            .unwrap()
    );
    assert!(
        !w.actions.subscribe(&w.editor, &INBOX, true).await.unwrap(),
        "the default"
    );
    assert!(w.actions.subscribe(&w.editor, &INBOX, false).await.unwrap());
    let tree = w.view.tree(&w.editor).await.unwrap();
    assert!(by_path(&tree, &["All Mail"]).subscribed);
    assert!(!by_path(&tree, &["INBOX"]).subscribed);
    let reader = w.view.tree(&w.reader).await.unwrap();
    assert!(
        by_path(&reader, &["All Mail"]).subscribed,
        "per user, not per device"
    );
    let theirs = w.view.tree(&w.member).await.unwrap();
    assert!(!by_path(&theirs, &["All Mail"]).subscribed);
    assert!(by_path(&theirs, &["INBOX"]).subscribed);
}

async fn changes(harness: &dyn Harness) {
    let w = two(harness).await;
    let welcome = w.email("welcome");
    let start = w.view.state(&w.editor).await.unwrap();
    let theirs = w.view.state(&w.member).await.unwrap();
    let since = start.max();
    let inbox = w.view.status(&w.editor, &INBOX).await.unwrap();
    let nothing = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert!(nothing.emails.is_empty() && nothing.mailboxes.is_empty() && !nothing.more);
    assert!(nothing.upto >= since);

    set(&w, &w.editor, Some(&INBOX), welcome, &["\\Flagged"], &[]).await;
    let after = w.view.state(&w.editor).await.unwrap();
    assert!(after.email > start.email);
    let changes = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert_eq!(changes.emails.updated, vec![welcome]);
    assert!(changes.emails.created.is_empty() && changes.emails.destroyed.is_empty());
    assert!(changes.counts_changed);
    let mailbox = w
        .view
        .mailbox_changes(&w.editor, &INBOX, inbox.highest_modseq)
        .await
        .unwrap();
    assert_eq!(mailbox.changed.len(), 1);
    assert_eq!(mailbox.changed[0].member.email, welcome);
    assert!(has(&mailbox.changed[0].flags, "\\Flagged"));
    assert!(mailbox.vanished.is_empty());

    let id = w
        .driver
        .deliver(
            "Work",
            "INBOX",
            &mail(
                "Ivy Ross <ivy@example.org>",
                BOB_ADDR,
                "Hello there",
                "ivy",
                "Sat, 07 Nov 2026 10:00:00 +0000",
                "",
                "Hi.\r\n",
            ),
        )
        .await;
    let delivered = EmailRef::Message(id);
    let changes = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert!(changes.emails.created.contains(&delivered));
    let thread = w.view.envelopes(&w.editor, &[delivered]).await.unwrap()[0]
        .thread
        .unwrap();
    assert!(changes.threads.created.contains(&thread));

    let draft = w
        .actions
        .append(
            &w.editor,
            &MailboxId::Drafts,
            new_email(fresh("Draft", "d")),
        )
        .await
        .unwrap()
        .email
        .unwrap();
    w.actions
        .subscribe(&w.editor, &MailboxId::All, true)
        .await
        .unwrap();
    set(
        &w,
        &w.editor,
        Some(&folder(&w, "Home", "Archive")),
        w.email("old"),
        &["\\Flagged"],
        &[],
    )
    .await;
    let mine = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert!(mine.emails.created.contains(&draft));
    assert!(mine.mailboxes.updated.contains(&MailboxId::All));
    assert!(mine.emails.updated.contains(&w.email("old")));
    let other = w.view.changes(&w.member, theirs.max(), 100).await.unwrap();
    assert!(
        !other.emails.created.contains(&draft),
        "another user's draft"
    );
    assert!(
        !other.mailboxes.updated.contains(&MailboxId::All),
        "another user's subscription"
    );
    assert!(
        !other.emails.updated.contains(&w.email("old")),
        "another user's local flag"
    );
    assert!(other.emails.created.contains(&delivered));
    assert!(
        w.view.state(&w.member).await.unwrap().email
            <= w.view.state(&w.editor).await.unwrap().email
    );

    w.actions
        .remove(&w.editor, Some(&MailboxId::Drafts), &[draft])
        .await
        .unwrap();
    let mine = w.view.changes(&w.editor, since, 100).await.unwrap();
    assert!(
        !mine.emails.created.contains(&draft),
        "created and gone: never created, at most destroyed (RFC 8620 allows either)"
    );

    let mut cursor = since;
    let mut seen = Vec::new();
    let mut rounds = 0;
    loop {
        let page = w.view.changes(&w.editor, cursor, 1).await.unwrap();
        assert!(page.upto > cursor || !page.more);
        seen.extend(page.emails.created);
        seen.extend(page.emails.updated);
        cursor = page.upto;
        rounds += 1;
        if !page.more {
            break;
        }
        assert!(rounds < 1000);
    }
    assert!(rounds > 1, "small pages");
    assert!(seen.contains(&welcome) && seen.contains(&delivered));
    assert!(matches!(
        w.view.changes(&w.editor, u64::MAX, 10).await,
        Err(MailViewError::CannotCalculateChanges)
    ));
}

async fn search(harness: &dyn Harness) {
    let w = two(harness).await;
    let (welcome, shared, reply) = (w.email("welcome"), w.email("shared"), w.email("reply"));
    let run = |mailbox: MailboxId, filter: Filter| {
        let w = &w;
        async move {
            let found = w.view.search(&w.editor, &mailbox, &filter).await.unwrap();
            assert!(found.windows(2).all(|p| p[0].uid < p[1].uid));
            found.into_iter().map(|m| m.email).collect::<Vec<_>>()
        }
    };
    let text = |field, value: &str| Filter::Text(field, value.into());
    assert_eq!(run(INBOX, Filter::All).await, vec![welcome, shared, reply]);
    assert_eq!(
        run(INBOX, text(TextField::Subject, "WELCOME")).await,
        vec![welcome, reply]
    );
    assert_eq!(
        run(INBOX, text(TextField::Subject, "welcome numbers")).await,
        Vec::<EmailRef>::new()
    );
    assert_eq!(
        run(INBOX, text(TextField::Subject, "welco")).await,
        vec![welcome, reply],
        "the start of a word"
    );
    assert!(
        run(INBOX, text(TextField::Subject, "elcome"))
            .await
            .is_empty(),
        "words, not substrings inside them"
    );
    assert_eq!(
        run(INBOX, text(TextField::From, "carol")).await,
        vec![shared]
    );
    assert_eq!(
        run(INBOX, text(TextField::To, "ada@example.com")).await,
        vec![reply]
    );
    assert_eq!(
        run(INBOX, text(TextField::Body, "glad")).await,
        vec![welcome]
    );
    assert_eq!(run(INBOX, text(TextField::Any, "happy")).await, vec![reply]);
    assert_eq!(
        run(INBOX, text(TextField::Header("In-Reply-To".into()), "")).await,
        vec![reply]
    );
    assert_eq!(
        run(INBOX, Filter::Flag("\\seen".into())).await,
        vec![shared]
    );
    assert_eq!(
        run(INBOX, Filter::Not(Box::new(Filter::Flag("\\Seen".into())))).await,
        vec![welcome, reply]
    );
    assert_eq!(
        run(
            INBOX,
            Filter::Or(vec![
                text(TextField::From, "carol"),
                text(TextField::From, "bob")
            ])
        )
        .await,
        vec![shared, reply]
    );
    assert_eq!(
        run(
            INBOX,
            Filter::And(vec![
                text(TextField::Subject, "welcome"),
                Filter::SentSince(Timestamp::from_secs(1_793_923_200))
            ])
        )
        .await,
        vec![reply]
    );
    assert_eq!(
        run(
            INBOX,
            Filter::SentBefore(Timestamp::from_secs(1_793_664_000))
        )
        .await,
        vec![welcome]
    );
    assert_eq!(
        run(
            INBOX,
            Filter::ReceivedSince(Timestamp::from_secs(1_793_664_000))
        )
        .await,
        vec![shared, reply]
    );
    let welcome_uid = uid(&w, &w.editor, &INBOX, welcome).await.unwrap();
    assert_eq!(
        run(INBOX, Filter::Uids(vec![(welcome_uid, welcome_uid)])).await,
        vec![welcome]
    );
    assert_eq!(
        run(INBOX, Filter::Larger(u64::MAX - 1)).await,
        Vec::<EmailRef>::new()
    );
    assert_eq!(run(INBOX, Filter::Smaller(u64::MAX)).await.len(), 3);
    assert_eq!(
        run(MailboxId::All, Filter::HasAttachment).await,
        vec![w.email("report")]
    );
    let status = w.view.status(&w.editor, &INBOX).await.unwrap();
    set(&w, &w.editor, Some(&INBOX), welcome, &["\\Flagged"], &[]).await;
    assert_eq!(
        run(INBOX, Filter::ModseqSince(status.highest_modseq + 1)).await,
        vec![welcome]
    );
    assert_eq!(
        run(INBOX, Filter::Flag("\\Flagged".into())).await,
        vec![welcome, shared]
    );
}
