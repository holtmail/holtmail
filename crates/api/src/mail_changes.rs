use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use holtmail_core::{AccountId, FolderId, MailChangeId, MessageId, Timestamp};
use holtmail_store::{
    Account, AccountKind, AuditKind, Capability, Folder, FolderRole, GrantRole, MailChange,
    MailChangeOrigin, MailChangeQuery, MailChangeState, MailOp, NewMailChange, User,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::access::manages;
use crate::dto::ErrorDto;
use crate::write::{Scopes, audit, may_write, role_in};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// The most messages one bulk change takes.
pub const MAX_BULK: usize = 500;

/// A change to a message on its server, held until `mail-writeback` sends it.
#[derive(Debug, Serialize, ToSchema)]
pub struct MailChangeDto {
    pub id: String,
    pub account_id: String,
    /// The folder it works in; for an append, where the bytes go.
    pub folder_id: String,
    pub message_id: Option<String>,
    /// The message's UID in the folder; missing for an append.
    pub uid: Option<u64>,
    /// `flags`, `labels`, `move`, `delete` or `append`.
    pub kind: String,
    /// Flags or labels added.
    pub add: Vec<String>,
    /// Flags or labels removed.
    pub remove: Vec<String>,
    /// The folder a move goes to.
    pub target_folder_id: Option<String>,
    /// Whether a delete removes for good.
    pub expunge: Option<bool>,
    /// `user`, `rule`, `retention` or `sent_copy`.
    pub origin: String,
    /// `held` (not yet on the server), `sending`, `applied`, `superseded_by_server` (the
    /// server showed something else afterwards and wins) or `failed`.
    pub state: String,
    pub attempts: u32,
    /// A short code for why the last attempt did not succeed, such as `connection`,
    /// `unsupported`, `not_allowed` or `gone`.
    pub last_error: Option<String>,
    /// The outbox message a sent copy belongs to.
    pub outbox_id: Option<String>,
    /// Unix milliseconds when the server accepted a flags or labels change that waits for a
    /// sync to confirm it.
    pub sent_at: Option<i64>,
    /// The UID the server gave a moved or appended message, when it said.
    pub result_uid: Option<u64>,
    /// Unix milliseconds when it was asked for.
    pub created_at: i64,
    /// Unix milliseconds when it last changed.
    pub updated_at: i64,
}

impl From<MailChange> for MailChangeDto {
    fn from(change: MailChange) -> Self {
        let new = change.new;
        let (add, remove, target, expunge) = match &new.op {
            MailOp::Flags { add, remove } | MailOp::Labels { add, remove } => {
                (add.clone(), remove.clone(), None, None)
            }
            MailOp::Move { target } => (Vec::new(), Vec::new(), Some(target.to_string()), None),
            MailOp::Delete { expunge } => (Vec::new(), Vec::new(), None, Some(*expunge)),
            MailOp::Append { flags, .. } => (flags.clone(), Vec::new(), None, None),
        };
        Self {
            id: change.id.to_string(),
            account_id: new.account_id.to_string(),
            folder_id: new.folder_id.to_string(),
            message_id: new.message_id.map(|m| m.to_string()),
            uid: new.uid,
            kind: new.op.kind().into(),
            add,
            remove,
            target_folder_id: target,
            expunge,
            origin: new.origin.as_str().into(),
            state: change.state.as_str().into(),
            attempts: change.attempts,
            last_error: change.last_error,
            outbox_id: new.outbox_id.map(|o| o.to_string()),
            sent_at: change.sent_at.map(|t| t.as_millis()),
            result_uid: change.result_uid,
            created_at: new.at.as_millis(),
            updated_at: change.updated_at.as_millis(),
        }
    }
}

/// What to change on the server.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailChangeOp {
    /// Adds and removes flags and keywords as IMAP spells them (`\Seen`, `\Flagged`,
    /// `\Answered`, `\Draft`, `$Forwarded`); needs `mail.flags`. `\Deleted` and `\Recent` are
    /// refused: deleting is its own change.
    Flags {
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Adds and removes Gmail labels on the message in the account's all-mail folder; needs
    /// `mail.flags`.
    Labels {
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Moves the message to a folder of the same account; needs `mail.move`.
    Move { folder: String },
    /// Deletes the message: moves it to the account's trash (`mail.delete`), or with `expunge`
    /// removes it for good (`mail.expunge`). Without a trash folder a delete only marks it
    /// deleted. The archive keeps it either way.
    Delete {
        #[serde(default)]
        expunge: bool,
    },
}

/// A change to one message.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MessageChangeRequest {
    pub op: MailChangeOp,
    /// Only the message's places in this account; every account it is in by default.
    pub account: Option<String>,
}

/// The same change to many messages.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkChangeRequest {
    /// Message ids, at most 500.
    pub messages: Vec<String>,
    pub op: MailChangeOp,
    /// Only their places in this account.
    pub account: Option<String>,
}

/// A message, or a place of it, a change left alone.
#[derive(Debug, Serialize, ToSchema)]
pub struct SkippedDto {
    pub message_id: String,
    /// `not_on_server` (no live place with a UID), `no_server` (an import account),
    /// `already` (the server already shows it), `in_target` (already in the folder),
    /// `in_trash` (a delete of a message already in the trash), `no_labels` (not in an
    /// all-mail folder) or `other_account` (a move to a folder of another account).
    pub reason: String,
}

/// What a change request recorded.
#[derive(Debug, Serialize, ToSchema)]
pub struct MailChangesDto {
    /// The held changes, one per place of a message on a server.
    pub changes: Vec<MailChangeDto>,
    pub skipped: Vec<SkippedDto>,
}

struct Plan {
    account: Account,
    audit: AuditKind,
    change: NewMailChange,
}

fn has(flags: &[String], name: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(name))
}

/// The flags at a place as they will be once its open flags changes are sent, oldest first.
fn shown_flags(server: &[String], open: &[MailChange], folder: FolderId, uid: u64) -> Vec<String> {
    let mut flags = server.to_vec();
    for change in open {
        if change.new.folder_id != folder || change.new.uid != Some(uid) {
            continue;
        }
        if let MailOp::Flags { add, remove } = &change.new.op {
            flags.retain(|f| !has(remove, f));
            for name in add {
                if !has(&flags, name) {
                    flags.push(name.clone());
                }
            }
        }
    }
    flags
}

fn clean(names: &[String], what: &str, flags: bool) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
            return Err(ApiError::bad_request(format!(
                "an empty or malformed {what}"
            )));
        }
        if flags
            && (name.chars().any(char::is_whitespace)
                || name.eq_ignore_ascii_case("\\Deleted")
                || name.eq_ignore_ascii_case("\\Recent"))
        {
            return Err(ApiError::bad_request(format!(
                "{name:?} cannot be set as a flag"
            )));
        }
        if !out.iter().any(|o| o == name) {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

fn validate(op: &MailChangeOp) -> ApiResult<MailChangeOp> {
    Ok(match op {
        MailChangeOp::Flags { add, remove } => {
            let (add, remove) = (clean(add, "flag", true)?, clean(remove, "flag", true)?);
            if add.is_empty() && remove.is_empty() {
                return Err(ApiError::bad_request("add or remove at least one flag"));
            }
            MailChangeOp::Flags { add, remove }
        }
        MailChangeOp::Labels { add, remove } => {
            let (add, remove) = (clean(add, "label", false)?, clean(remove, "label", false)?);
            if let Some(name) = add
                .iter()
                .chain(&remove)
                .find(|name| holtmail_mail_writeback::is_folder_label(name))
            {
                return Err(ApiError::bad_request(format!(
                    "{name:?} is a mailbox, not a label: move or delete the message instead"
                )));
            }
            if add.is_empty() && remove.is_empty() {
                return Err(ApiError::bad_request("add or remove at least one label"));
            }
            MailChangeOp::Labels { add, remove }
        }
        other => other.clone(),
    })
}

struct Planner<'a> {
    state: &'a ApiState,
    user: &'a User,
    account_filter: Option<AccountId>,
    target: Option<Folder>,
    folders: HashMap<FolderId, Option<Folder>>,
    accounts: HashMap<AccountId, Option<Account>>,
    trash: HashMap<AccountId, Option<FolderId>>,
    plans: Vec<Plan>,
    skipped: Vec<SkippedDto>,
    touched: BTreeMap<AccountId, Account>,
}

impl<'a> Planner<'a> {
    async fn folder(&mut self, id: FolderId) -> ApiResult<Option<Folder>> {
        if let Some(folder) = self.folders.get(&id) {
            return Ok(folder.clone());
        }
        let folder = self.state.store.get_folder(id).await?;
        self.folders.insert(id, folder.clone());
        Ok(folder)
    }

    async fn account(&mut self, id: AccountId) -> ApiResult<Option<Account>> {
        if let Some(account) = self.accounts.get(&id) {
            return Ok(account.clone());
        }
        let account = self.state.store.get_account(id).await?;
        self.accounts.insert(id, account.clone());
        Ok(account)
    }

    async fn trash(&mut self, account: &Account) -> ApiResult<Option<FolderId>> {
        if let Some(trash) = self.trash.get(&account.id) {
            return Ok(*trash);
        }
        let trash = holtmail_mail_writeback::trash_folder(&*self.state.store, account).await?;
        self.trash.insert(account.id, trash);
        Ok(trash)
    }

    fn skip(&mut self, message: MessageId, reason: &str) {
        self.skipped.push(SkippedDto {
            message_id: message.to_string(),
            reason: reason.into(),
        });
    }

    async fn plan(&mut self, message: MessageId, op: &MailChangeOp) -> ApiResult<()> {
        let Some(stored) = self.state.store.get_message(message).await? else {
            return Err(ApiError::not_found("message"));
        };
        if role_in(self.state, self.user, stored.archive_id)
            .await?
            .is_none()
        {
            return Err(ApiError::not_found("message"));
        }
        let mut planned = false;
        let mut reasons: BTreeSet<&'static str> = BTreeSet::new();
        let open = match op {
            MailChangeOp::Flags { .. } => self.state.store.pending_mail_changes(message).await?,
            _ => Vec::new(),
        };
        for location in self.state.store.locations(message).await? {
            let (Some(uid), None) = (location.uid, location.removed_at) else {
                continue;
            };
            let Some(folder) = self.folder(location.folder_id).await? else {
                continue;
            };
            if self.account_filter.is_some_and(|a| a != folder.account_id) {
                continue;
            }
            let Some(account) = self.account(folder.account_id).await? else {
                continue;
            };
            if account.kind == AccountKind::Import {
                reasons.insert("no_server");
                continue;
            }
            self.touched.insert(account.id, account.clone());
            let (op, audit) = match op {
                MailChangeOp::Flags { add, remove } => {
                    let flags = shown_flags(&location.flags, &open, folder.id, uid);
                    if add.iter().all(|f| has(&flags, f)) && remove.iter().all(|f| !has(&flags, f))
                    {
                        reasons.insert("already");
                        continue;
                    }
                    let op = MailOp::Flags {
                        add: add.clone(),
                        remove: remove.clone(),
                    };
                    (op, AuditKind::MailFlags)
                }
                MailChangeOp::Labels { add, remove } => {
                    if folder.role != Some(FolderRole::All) {
                        reasons.insert("no_labels");
                        continue;
                    }
                    let op = MailOp::Labels {
                        add: add.clone(),
                        remove: remove.clone(),
                    };
                    (op, AuditKind::MailFlags)
                }
                MailChangeOp::Move { .. } => {
                    let target = self.target.clone().expect("a move has its target");
                    if target.account_id != account.id {
                        reasons.insert("other_account");
                        continue;
                    }
                    if target.id == folder.id {
                        reasons.insert("in_target");
                        continue;
                    }
                    (MailOp::Move { target: target.id }, AuditKind::MailMove)
                }
                MailChangeOp::Delete { expunge: true } => {
                    (MailOp::Delete { expunge: true }, AuditKind::MailDelete)
                }
                MailChangeOp::Delete { expunge: false } => match self.trash(&account).await? {
                    Some(trash) if trash == folder.id => {
                        reasons.insert("in_trash");
                        continue;
                    }
                    Some(trash) => (MailOp::Move { target: trash }, AuditKind::MailDelete),
                    None => (MailOp::Delete { expunge: false }, AuditKind::MailDelete),
                },
            };
            planned = true;
            self.plans.push(Plan {
                change: NewMailChange {
                    account_id: account.id,
                    folder_id: folder.id,
                    message_id: Some(message),
                    uid: Some(uid),
                    op,
                    origin: MailChangeOrigin::User,
                    user_id: Some(self.user.id),
                    device_id: None,
                    outbox_id: None,
                    at: Timestamp::now(),
                },
                account,
                audit,
            });
        }
        if !planned {
            let reason = reasons.into_iter().next().unwrap_or("not_on_server");
            self.skip(message, reason);
        }
        Ok(())
    }
}

async fn change_messages(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    messages: Vec<MessageId>,
    op: &MailChangeOp,
    account: Option<&str>,
) -> ApiResult<MailChangesDto> {
    let op = validate(op)?;
    let account_filter = account
        .map(|raw| {
            raw.parse::<AccountId>()
                .map_err(|_| ApiError::not_found("account"))
        })
        .transpose()?;
    let target = match &op {
        MailChangeOp::Move { folder } => {
            let id: FolderId = folder.parse().map_err(|_| ApiError::not_found("folder"))?;
            Some(
                state
                    .store
                    .get_folder(id)
                    .await?
                    .ok_or_else(|| ApiError::not_found("folder"))?,
            )
        }
        _ => None,
    };
    let mut planner = Planner {
        state,
        user,
        account_filter,
        target,
        folders: HashMap::new(),
        accounts: HashMap::new(),
        trash: HashMap::new(),
        plans: Vec::new(),
        skipped: Vec::new(),
        touched: BTreeMap::new(),
    };
    for message in messages {
        planner.plan(message, &op).await?;
    }
    let Planner {
        plans,
        skipped,
        touched,
        target,
        ..
    } = planner;
    let to_trash = target
        .as_ref()
        .is_some_and(|t| t.role == Some(FolderRole::Trash));
    for account in touched.values() {
        let capabilities = state.store.account_capabilities(account.id).await?;
        let capability = match (&op, &target) {
            (MailChangeOp::Move { .. }, Some(target)) => holtmail_mail_writeback::capability_for(
                &MailOp::Move { target: target.id },
                to_trash,
                &capabilities,
            ),
            (MailChangeOp::Move { .. }, None) => Capability::MailMove,
            (MailChangeOp::Delete { expunge: true }, _) => Capability::MailExpunge,
            (MailChangeOp::Delete { expunge: false }, _) => Capability::MailDelete,
            (MailChangeOp::Flags { .. } | MailChangeOp::Labels { .. }, _) => Capability::MailFlags,
        };
        may_write(state, user, scopes, account.id, capability).await?;
    }

    let mut audits: BTreeMap<(AccountId, &'static str), (Account, AuditKind, Vec<String>)> =
        BTreeMap::new();
    for plan in &plans {
        let entry = audits
            .entry((plan.account.id, plan.audit.as_str()))
            .or_insert_with(|| (plan.account.clone(), plan.audit, Vec::new()));
        if let Some(message) = plan.change.message_id {
            entry.2.push(message.to_string());
        }
    }
    for (account, kind, targets) in audits.into_values() {
        audit(
            state,
            user,
            kind,
            Some(account.archive_id),
            Some(account.id),
            targets,
        )
        .await?;
    }
    let mut changes = Vec::with_capacity(plans.len());
    for plan in plans {
        changes.push(state.store.enqueue_mail_change(&plan.change).await?.into());
    }
    Ok(MailChangesDto { changes, skipped })
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/changes",
    params(("id" = String, Path, description = "Message id")),
    request_body = MessageChangeRequest,
    responses((status = 200, body = MailChangesDto), (status = 400, description = "A malformed change", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock: the instance switch, the account's capability, the grant (editor) or the scope (write:mail)", body = ErrorDto), (status = 404, description = "No such message, account or folder", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn change_message(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<MessageChangeRequest>,
) -> ApiResult<Json<MailChangesDto>> {
    let message: MessageId = raw.parse().map_err(|_| ApiError::not_found("message"))?;
    Ok(Json(
        change_messages(
            &state,
            &user,
            &scopes,
            vec![message],
            &request.op,
            request.account.as_deref(),
        )
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/mail/changes",
    request_body = BulkChangeRequest,
    responses((status = 200, body = MailChangesDto), (status = 400, description = "A malformed change, or more than 500 messages", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock for one of the accounts; nothing is recorded", body = ErrorDto), (status = 404, description = "No such message, account or folder", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn change_bulk(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Json(request): Json<BulkChangeRequest>,
) -> ApiResult<Json<MailChangesDto>> {
    if request.messages.is_empty() || request.messages.len() > MAX_BULK {
        return Err(ApiError::bad_request("give 1 to 500 messages"));
    }
    let mut messages = Vec::with_capacity(request.messages.len());
    for raw in &request.messages {
        let id: MessageId = raw.parse().map_err(|_| ApiError::not_found("message"))?;
        if !messages.contains(&id) {
            messages.push(id);
        }
    }
    Ok(Json(
        change_messages(
            &state,
            &user,
            &scopes,
            messages,
            &request.op,
            request.account.as_deref(),
        )
        .await?,
    ))
}

/// Puts an archived message back on a server.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RestoreRequest {
    /// The folder it goes to, of an account in the message's archive; needs `mail.append` there.
    pub folder: String,
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/restore",
    params(("id" = String, Path, description = "Message id")),
    request_body = RestoreRequest,
    responses((status = 200, body = MailChangeDto), (status = 400, description = "The folder is in another archive or an import account", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock: the instance switch, mail.append, the grant (editor) or the scope (write:mail)", body = ErrorDto), (status = 404, description = "No such message or folder", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn restore(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<RestoreRequest>,
) -> ApiResult<Json<MailChangeDto>> {
    let message: MessageId = raw.parse().map_err(|_| ApiError::not_found("message"))?;
    let stored = state
        .store
        .get_message(message)
        .await?
        .ok_or_else(|| ApiError::not_found("message"))?;
    if role_in(&state, &user, stored.archive_id).await?.is_none() {
        return Err(ApiError::not_found("message"));
    }
    let folder_id: FolderId = request
        .folder
        .parse()
        .map_err(|_| ApiError::not_found("folder"))?;
    let folder = state
        .store
        .get_folder(folder_id)
        .await?
        .ok_or_else(|| ApiError::not_found("folder"))?;
    let account = may_write(
        &state,
        &user,
        &scopes,
        folder.account_id,
        Capability::MailAppend,
    )
    .await?;
    if account.archive_id != stored.archive_id {
        return Err(ApiError::bad_request(
            "the folder belongs to another archive",
        ));
    }
    if account.kind == AccountKind::Import {
        return Err(ApiError::bad_request("an import account has no server"));
    }
    let change = NewMailChange {
        account_id: account.id,
        folder_id: folder.id,
        message_id: Some(message),
        uid: None,
        op: MailOp::Append {
            blob_id: stored.blob_id,
            flags: vec!["\\Seen".into()],
            date: stored.received_at.unwrap_or(stored.date),
        },
        origin: MailChangeOrigin::User,
        user_id: Some(user.id),
        device_id: None,
        outbox_id: None,
        at: Timestamp::now(),
    };
    audit(
        &state,
        &user,
        AuditKind::MailAppend,
        Some(account.archive_id),
        Some(account.id),
        vec![message.to_string()],
    )
    .await?;
    Ok(Json(state.store.enqueue_mail_change(&change).await?.into()))
}

/// Which changes of an account to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MailChangeParams {
    /// Only changes in these states, comma-separated (`held,sending`); every state by default.
    pub state: Option<String>,
    /// At most this many, 100 by default and 500 at most.
    pub limit: Option<u32>,
}

async fn readable_account(state: &ApiState, user: &User, raw: &str) -> ApiResult<Account> {
    let id: AccountId = raw.parse().map_err(|_| ApiError::not_found("account"))?;
    let account = state
        .store
        .get_account(id)
        .await?
        .ok_or_else(|| ApiError::not_found("account"))?;
    if role_in(state, user, account.archive_id).await?.is_none() {
        return Err(ApiError::not_found("account"));
    }
    Ok(account)
}

#[utoipa::path(
    get,
    path = "/api/accounts/{id}/mail-changes",
    params(("id" = String, Path, description = "Account id"), MailChangeParams),
    responses((status = 200, body = Vec<MailChangeDto>), (status = 400, description = "An unknown state", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such account, or not readable", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_mail_changes(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Query(params): Query<MailChangeParams>,
) -> ApiResult<Json<Vec<MailChangeDto>>> {
    let account = readable_account(&state, &user, &raw).await?;
    let states = params
        .state
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            MailChangeState::parse(s)
                .ok_or_else(|| ApiError::bad_request(format!("unknown state {s:?}")))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let changes = state
        .store
        .list_mail_changes(&MailChangeQuery {
            account: account.id,
            states,
            limit: params.limit.unwrap_or(100).min(500),
        })
        .await?;
    Ok(Json(changes.into_iter().map(Into::into).collect()))
}

/// An account's retention: delete mail from its server some days after it arrived.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionDto {
    pub account: String,
    /// The stored period in days; `null` keeps everything on the server.
    pub days: Option<u32>,
    /// The period the counts below are for: `days`, or the one asked about.
    pub checked_days: Option<u32>,
    /// Messages the job would delete now (at most 10 000 are counted).
    pub due: u64,
    /// More are due than counted.
    pub more: bool,
    /// They would be removed for good (`mail.expunge`) rather than moved to the trash.
    pub expunge: bool,
    /// The job may run: `mail.delete` is on, the instance allows writing, and there is a
    /// trash folder or `mail.expunge`.
    pub active: bool,
}

/// A dry run for another period.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RetentionParams {
    /// Count for this many days instead of the stored period; nothing is saved.
    pub days: Option<u32>,
}

/// Sets an account's retention.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RetentionRequest {
    /// Days after arrival, 1 to 36 500; `null` turns retention off.
    pub days: Option<u32>,
}

const DRY_RUN_LIMIT: u32 = 10_000;

async fn retention_dto(
    state: &ApiState,
    account: &Account,
    days: Option<u32>,
) -> ApiResult<RetentionDto> {
    let stored = holtmail_mail_writeback::retention_days(account);
    let checked = days.or(stored);
    let capabilities = state.store.account_capabilities(account.id).await?;
    let expunge = capabilities.has(Capability::MailExpunge);
    let mut dto = RetentionDto {
        account: account.id.to_string(),
        days: stored,
        checked_days: checked,
        due: 0,
        more: false,
        expunge,
        active: false,
    };
    if account.kind == AccountKind::Import {
        return Ok(dto);
    }
    dto.active = state.write.enabled
        && capabilities.has(Capability::MailDelete)
        && (expunge
            || holtmail_mail_writeback::trash_folder(&*state.store, account)
                .await?
                .is_some());
    if let Some(days) = checked {
        let plan = holtmail_mail_writeback::plan_retention(
            &*state.store,
            account,
            days,
            Timestamp::now(),
            None,
            DRY_RUN_LIMIT,
        )
        .await?;
        dto.due = plan.changes.len() as u64;
        dto.more = plan.more;
    }
    Ok(dto)
}

#[utoipa::path(
    get,
    path = "/api/accounts/{id}/retention",
    params(("id" = String, Path, description = "Account id"), RetentionParams),
    responses((status = 200, body = RetentionDto), (status = 400, description = "Days out of range", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such account, or not readable", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn get_retention(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
    Query(params): Query<RetentionParams>,
) -> ApiResult<Json<RetentionDto>> {
    let account = readable_account(&state, &user, &raw).await?;
    check_days(params.days)?;
    Ok(Json(retention_dto(&state, &account, params.days).await?))
}

fn check_days(days: Option<u32>) -> ApiResult<()> {
    if days.is_some_and(|d| d == 0 || d > holtmail_mail_writeback::MAX_RETENTION_DAYS) {
        return Err(ApiError::bad_request("days is 1 to 36500"));
    }
    Ok(())
}

#[utoipa::path(
    put,
    path = "/api/accounts/{id}/retention",
    params(("id" = String, Path, description = "Account id")),
    request_body = RetentionRequest,
    responses((status = 200, body = RetentionDto), (status = 400, description = "Days out of range, or an import account", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin, or a closed lock for mail.delete", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn put_retention(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    Json(request): Json<RetentionRequest>,
) -> ApiResult<Json<RetentionDto>> {
    check_days(request.days)?;
    let account = readable_account(&state, &user, &raw).await?;
    if !manages(&user) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only the owner and admins can do this".into(),
        ));
    }
    if account.kind == AccountKind::Import {
        return Err(ApiError::bad_request("an import account has no server"));
    }
    if request.days.is_some() {
        may_write(&state, &user, &scopes, account.id, Capability::MailDelete).await?;
    }
    let mut account = account;
    if holtmail_mail_writeback::retention_days(&account) != request.days {
        audit(
            &state,
            &user,
            AuditKind::CapabilitiesChanged,
            Some(account.archive_id),
            Some(account.id),
            vec![format!(
                "{}:{}",
                holtmail_mail_writeback::RETENTION_SETTING,
                request.days.map_or_else(|| "off".into(), |d| d.to_string())
            )],
        )
        .await?;
        account.settings =
            holtmail_mail_writeback::with_retention_days(&account.settings, request.days);
        state.store.update_account(&account).await?;
    }
    Ok(Json(retention_dto(&state, &account, None).await?))
}

/// The open changes of a message, for the held mark.
pub(crate) async fn pending(state: &ApiState, message: MessageId) -> ApiResult<Vec<MailChangeDto>> {
    Ok(state
        .store
        .pending_mail_changes(message)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// The failed changes of a message nobody dismissed, for the reader.
pub(crate) async fn failed(state: &ApiState, message: MessageId) -> ApiResult<Vec<MailChangeDto>> {
    Ok(state
        .store
        .failed_mail_changes(message)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

async fn readable_change(
    state: &ApiState,
    user: &User,
    raw: &str,
) -> ApiResult<(MailChange, Account, GrantRole)> {
    let id: MailChangeId = raw.parse().map_err(|_| ApiError::not_found("change"))?;
    let change = state
        .store
        .get_mail_change(id)
        .await?
        .ok_or_else(|| ApiError::not_found("change"))?;
    let account = state
        .store
        .get_account(change.new.account_id)
        .await?
        .ok_or_else(|| ApiError::not_found("change"))?;
    let role = role_in(state, user, account.archive_id)
        .await?
        .ok_or_else(|| ApiError::not_found("change"))?;
    Ok((change, account, role))
}

fn not_failed() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "only a failed change that was not dismissed can be retried or dismissed".into(),
    )
}

#[utoipa::path(
    post,
    path = "/api/mail-changes/{id}/retry",
    params(("id" = String, Path, description = "Mail change id")),
    responses((status = 200, description = "The new held change", body = MailChangeDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock for the change's capability", body = ErrorDto), (status = 404, description = "No such change, or not readable", body = ErrorDto), (status = 409, description = "The change is not failed, or was dismissed", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn retry_change(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<MailChangeDto>> {
    let (change, account, _) = readable_change(&state, &user, &raw).await?;
    if change.state != MailChangeState::Failed {
        return Err(not_failed());
    }
    let to_trash = match &change.new.op {
        MailOp::Move { target } => state
            .store
            .get_folder(*target)
            .await?
            .is_some_and(|f| f.role == Some(FolderRole::Trash)),
        _ => false,
    };
    let capabilities = state.store.account_capabilities(account.id).await?;
    let capability =
        holtmail_mail_writeback::capability_for(&change.new.op, to_trash, &capabilities);
    may_write(&state, &user, &scopes, account.id, capability).await?;
    let now = Timestamp::now();
    if state
        .store
        .dismiss_mail_change(change.id, now)
        .await?
        .is_none()
    {
        return Err(not_failed());
    }
    let kind = match (&change.new.op, to_trash) {
        (MailOp::Flags { .. } | MailOp::Labels { .. }, _) => AuditKind::MailFlags,
        (MailOp::Move { .. }, true) | (MailOp::Delete { .. }, _) => AuditKind::MailDelete,
        (MailOp::Move { .. }, false) => AuditKind::MailMove,
        (MailOp::Append { .. }, _) => AuditKind::MailAppend,
    };
    audit(
        &state,
        &user,
        kind,
        Some(account.archive_id),
        Some(account.id),
        change
            .new
            .message_id
            .map(|m| m.to_string())
            .into_iter()
            .collect(),
    )
    .await?;
    let again = NewMailChange {
        user_id: Some(user.id),
        device_id: None,
        at: now,
        ..change.new
    };
    Ok(Json(state.store.enqueue_mail_change(&again).await?.into()))
}

#[utoipa::path(
    post,
    path = "/api/mail-changes/{id}/dismiss",
    params(("id" = String, Path, description = "Mail change id")),
    responses((status = 200, description = "The dismissed change", body = MailChangeDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Neither an editor of the archive nor an owner or admin", body = ErrorDto), (status = 404, description = "No such change, or not readable", body = ErrorDto), (status = 409, description = "The change is not failed, or was dismissed", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn dismiss_change(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Path(raw): Path<String>,
) -> ApiResult<Json<MailChangeDto>> {
    let (change, _, role) = readable_change(&state, &user, &raw).await?;
    if role < GrantRole::Editor && !manages(&user) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "dismissing needs an editor grant".into(),
        ));
    }
    state
        .store
        .dismiss_mail_change(change.id, Timestamp::now())
        .await?
        .map(|c| Json(c.into()))
        .ok_or_else(not_failed)
}
