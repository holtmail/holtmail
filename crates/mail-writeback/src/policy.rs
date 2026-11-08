use holtmail_core::{AccountId, FolderId, Timestamp, UserId};
use holtmail_store::{
    Account, Capabilities, Capability, FolderRole, MailChangeOrigin, MailOp, NewMailChange,
    RetentionCandidate, Store, StoreError,
};

/// The account setting that holds the retention period in days.
pub const RETENTION_SETTING: &str = "delete_after_days";

/// The longest retention period accepted, about a hundred years.
pub const MAX_RETENTION_DAYS: u32 = 36_500;

/// The capability whose lock a change needs: `mail.flags` for flags and labels, `mail.move`
/// for a move (or `mail.delete` for a move to the trash when the account lacks `mail.move`),
/// `mail.delete` for a delete that only marks, `mail.expunge` for one that removes for good
/// (which needs `mail.delete` too), and `mail.append` for an append.
pub fn capability_for(op: &MailOp, to_trash: bool, capabilities: &Capabilities) -> Capability {
    match op {
        MailOp::Flags { .. } | MailOp::Labels { .. } => Capability::MailFlags,
        MailOp::Move { .. } if to_trash && !capabilities.has(Capability::MailMove) => {
            Capability::MailDelete
        }
        MailOp::Move { .. } => Capability::MailMove,
        MailOp::Delete { expunge: false } => Capability::MailDelete,
        MailOp::Delete { expunge: true } => Capability::MailExpunge,
        MailOp::Append { .. } => Capability::MailAppend,
    }
}

/// Whether a label name stands for a mailbox rather than a tag: Gmail's `Inbox`, `Sent`,
/// `Drafts`, `Trash` and `Spam` (any case), or any name starting with `\`. Adding or removing
/// one would move, trash or report a message, so a labels change (which needs only
/// `mail.flags`) may not name it.
pub fn is_folder_label(name: &str) -> bool {
    name.starts_with('\\')
        || ["Inbox", "Sent", "Drafts", "Trash", "Spam"]
            .iter()
            .any(|system| name.eq_ignore_ascii_case(system))
}

/// Whether `capabilities` allow `op`: the capability of [`capability_for`], and `mail.delete`
/// besides `mail.expunge`.
pub fn allows(op: &MailOp, to_trash: bool, capabilities: &Capabilities) -> bool {
    let needed = capability_for(op, to_trash, capabilities);
    capabilities.has(needed)
        && (needed != Capability::MailExpunge || capabilities.has(Capability::MailDelete))
}

/// The account's retention period: delete messages from its server this many days after they
/// arrived. `None` when it keeps everything.
pub fn retention_days(account: &Account) -> Option<u32> {
    account
        .settings
        .get(RETENTION_SETTING)
        .and_then(serde_json::Value::as_u64)
        .and_then(|d| u32::try_from(d).ok())
        .filter(|d| *d > 0)
}

/// `settings` with the retention period set, or removed for `None`; other settings stay.
pub fn with_retention_days(settings: &serde_json::Value, days: Option<u32>) -> serde_json::Value {
    let mut settings = match settings {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    match days {
        Some(days) => settings.insert(RETENTION_SETTING.into(), days.into()),
        None => settings.remove(RETENTION_SETTING),
    };
    serde_json::Value::Object(settings)
}

/// What the retention job would do for an account now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPlan {
    /// The changes to record, oldest message first.
    pub changes: Vec<NewMailChange>,
    /// Whether it removes for good (`mail.expunge`) rather than moving to the trash.
    pub expunge: bool,
    /// Whether more messages are due than `limit` allowed.
    pub more: bool,
}

/// The changes that delete the account's messages older than `days` from its server, at most
/// `limit`: with `mail.expunge`, deletes that remove for good (the trash included); otherwise
/// moves to the account's trash folder, skipping messages already there. Only live locations
/// with a UID, no open change and no earlier retention change count, so planning again after
/// recording is idempotent and a failed retention change is never recorded again (it waits in
/// the reader for _Try again_).
/// Empty when the account lacks `mail.delete`, or has neither `mail.expunge` nor a trash
/// folder. The archive keeps every message.
pub async fn plan_retention(
    store: &dyn Store,
    account: &Account,
    days: u32,
    now: Timestamp,
    user: Option<UserId>,
    limit: u32,
) -> Result<RetentionPlan, StoreError> {
    let capabilities = store.account_capabilities(account.id).await?;
    let expunge = capabilities.has(Capability::MailExpunge);
    let empty = RetentionPlan {
        changes: Vec::new(),
        expunge,
        more: false,
    };
    if !capabilities.has(Capability::MailDelete) {
        return Ok(empty);
    }
    let trash = trash_folder(store, account).await?;
    if !expunge && trash.is_none() {
        return Ok(empty);
    }
    let before = Timestamp::from_millis(
        now.as_millis()
            .saturating_sub(i64::from(days.min(MAX_RETENTION_DAYS)) * 86_400_000),
    );
    let mut found = store
        .retention_candidates(account.id, before, !expunge, limit.saturating_add(1))
        .await?;
    let more = found.len() > limit as usize;
    found.truncate(limit as usize);
    let changes = found
        .into_iter()
        .map(|c| retention_change(account.id, c, expunge, trash, user, now))
        .collect();
    Ok(RetentionPlan {
        changes,
        expunge,
        more,
    })
}

fn retention_change(
    account: AccountId,
    candidate: RetentionCandidate,
    expunge: bool,
    trash: Option<FolderId>,
    user: Option<UserId>,
    now: Timestamp,
) -> NewMailChange {
    let op = match trash {
        Some(target) if !expunge => MailOp::Move { target },
        _ => MailOp::Delete { expunge: true },
    };
    NewMailChange {
        account_id: account,
        folder_id: candidate.folder_id,
        message_id: Some(candidate.message_id),
        uid: Some(candidate.uid),
        op,
        origin: MailChangeOrigin::Retention,
        user_id: user,
        device_id: None,
        outbox_id: None,
        at: now,
    }
}

/// The account's folder with the trash role, if it has one.
pub async fn trash_folder(
    store: &dyn Store,
    account: &Account,
) -> Result<Option<FolderId>, StoreError> {
    Ok(store
        .list_folders(account.archive_id)
        .await?
        .into_iter()
        .map(|f| f.folder)
        .find(|f| f.account_id == account.id && f.role == Some(FolderRole::Trash))
        .map(|f| f.id))
}
