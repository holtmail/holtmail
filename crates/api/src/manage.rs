use std::path::Path as FsPath;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use futures::StreamExt;
use holtmail_core::{AccountId, ArchiveId};
use holtmail_store::{Account, AccountKind, MessageQuery, NewAccount};
use tokio::io::AsyncWriteExt;

use crate::dto::*;
use crate::mailboxes::{
    AccountChange, ConnectionChange, ContactsCalendarsChange, JmapConnectionChange, MailboxError,
    NewImapAccount, NewJmapAccount, RemoteImageJob, WatchMode, WatchSettings,
};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

const MAX_NAME: usize = 100;

pub(crate) fn id<T: std::str::FromStr>(raw: &str, what: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| ApiError::not_found(what))
}

pub(crate) fn name(raw: &str, what: &str) -> ApiResult<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(format!("{what} is empty")));
    }
    if name.chars().count() > MAX_NAME {
        return Err(ApiError::bad_request(format!(
            "{what} is longer than {MAX_NAME} characters"
        )));
    }
    Ok(name.to_owned())
}

pub(crate) fn refused(error: MailboxError) -> ApiError {
    match error {
        MailboxError::Invalid(message) => ApiError::bad_request(message),
        MailboxError::Conflict(message) => ApiError(StatusCode::CONFLICT, message),
        MailboxError::Backend(message) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

fn bare_account_dto(state: &ApiState, account: Account) -> AccountDto {
    let status = state.mailboxes.status(account.id);
    let details = state.mailboxes.details(&account);
    AccountDto::new(account, status, details)
}

/// The account as owners and admins see it, trust list and suggestion included.
pub(crate) async fn account_dto(state: &ApiState, account: Account) -> ApiResult<AccountDto> {
    let lists = state.store.trusted_authserv_ids(account.archive_id).await?;
    let (ids, suggested) = crate::categories::account_trust(state, &account, &lists).await?;
    let mut dto = bare_account_dto(state, account);
    dto.trusted_authserv_ids = ids;
    dto.suggested_authserv_id = suggested;
    Ok(dto)
}

pub(crate) async fn archive(state: &ApiState, raw: &str) -> ApiResult<ArchiveId> {
    let archive: ArchiveId = id(raw, "archive")?;
    state
        .store
        .get_archive(archive)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))?;
    Ok(archive)
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/accounts",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 200, body = Vec<AccountDto>), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "Malformed archive id", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn list_accounts(
    State(state): AppState,
    Path(raw): Path<String>,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
) -> ApiResult<Json<Vec<AccountDto>>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let accounts = state.store.list_accounts(archive).await?;
    let manages = crate::access::manages(&user);
    let lists = if manages {
        state.store.trusted_authserv_ids(archive).await?
    } else {
        Default::default()
    };
    let mut out = Vec::with_capacity(accounts.len());
    for account in accounts {
        let trust = if manages {
            crate::categories::account_trust(&state, &account, &lists).await?
        } else {
            (None, None)
        };
        let mut dto = bare_account_dto(&state, account);
        (dto.trusted_authserv_ids, dto.suggested_authserv_id) = trust;
        if !manages {
            dto.connection = None;
            dto.jmap = None;
            dto.watch = None;
            if let Some(c) = dto.contacts_calendars.as_mut() {
                c.contacts_url = None;
                c.calendars_url = None;
                c.caldav_offer = None;
            }
        }
        out.push(dto);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/archives",
    request_body = NewArchiveRequest,
    responses((status = 201, body = ArchiveDto), (status = 400, description = "An empty or too long name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn create_archive(
    State(state): AppState,
    Json(request): Json<NewArchiveRequest>,
) -> ApiResult<(StatusCode, Json<ArchiveDto>)> {
    let name = name(&request.name, "the name")?;
    let archive = state.store.create_archive(&name).await?;
    Ok((
        StatusCode::CREATED,
        Json(crate::routes::archive_dto(&state, archive).await?),
    ))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/accounts",
    params(("id" = String, Path, description = "Archive id")),
    request_body = ImapAccountRequest,
    responses((status = 201, description = "Stored; the first sync has started", body = AccountDto), (status = 400, description = "Bad settings, or the server refused to sign in", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn add_imap_account(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<ImapAccountRequest>,
) -> ApiResult<(StatusCode, Json<AccountDto>)> {
    let archive = archive(&state, &raw).await?;
    let host = name(&request.host, "the server")?;
    let username = name(&request.username, "the user name")?;
    if request.password.is_empty() {
        return Err(ApiError::bad_request("the password is empty"));
    }
    let security = request.security.unwrap_or_else(|| "tls".to_owned());
    if !matches!(security.as_str(), "tls" | "starttls" | "plain") {
        return Err(ApiError::bad_request(
            "security is one of tls, starttls and plain",
        ));
    }
    let account = state
        .mailboxes
        .add_imap(
            archive,
            NewImapAccount {
                name: request
                    .name
                    .as_deref()
                    .map(|n| name(n, "the name"))
                    .transpose()?,
                host,
                port: request.port.filter(|p| *p != 0),
                security,
                username,
                password: request.password,
            },
        )
        .await
        .map_err(refused)?;
    crate::categories::seed_trust(&state, &account).await?;
    Ok((
        StatusCode::CREATED,
        Json(account_dto(&state, account).await?),
    ))
}

fn jmap_auth(raw: Option<String>) -> ApiResult<String> {
    let auth = raw.unwrap_or_else(|| "password".to_owned());
    if !matches!(auth.as_str(), "password" | "token") {
        return Err(ApiError::bad_request("auth is `password` or `token`"));
    }
    Ok(auth)
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/accounts/jmap",
    params(("id" = String, Path, description = "Archive id")),
    request_body = JmapAccountRequest,
    responses((status = 201, description = "Stored; the first sync has started", body = AccountDto), (status = 400, description = "Bad settings, or the server refused to sign in", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn add_jmap_account(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<JmapAccountRequest>,
) -> ApiResult<(StatusCode, Json<AccountDto>)> {
    let archive = archive(&state, &raw).await?;
    let url = request.url.trim().to_owned();
    if url.is_empty() || url.len() > 2000 {
        return Err(ApiError::bad_request("the address is empty or too long"));
    }
    let auth = jmap_auth(request.auth)?;
    let username = request.username.trim().to_owned();
    if auth == "password" && username.is_empty() {
        return Err(ApiError::bad_request("the user name is empty"));
    }
    if request.password.is_empty() {
        return Err(ApiError::bad_request(if auth == "token" {
            "the token is empty"
        } else {
            "the password is empty"
        }));
    }
    let account = state
        .mailboxes
        .add_jmap(
            archive,
            NewJmapAccount {
                name: request
                    .name
                    .as_deref()
                    .map(|n| name(n, "the name"))
                    .transpose()?,
                url,
                username,
                auth,
                secret: request.password,
            },
        )
        .await
        .map_err(refused)?;
    crate::categories::seed_trust(&state, &account).await?;
    Ok((
        StatusCode::CREATED,
        Json(account_dto(&state, account).await?),
    ))
}

#[utoipa::path(
    post,
    path = "/api/accounts/{id}/sync",
    params(("id" = String, Path, description = "Account id")),
    responses((status = 202, description = "Syncing in the background", body = AccountDto), (status = 400, description = "An import account, which has no server", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn sync_account(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<(StatusCode, Json<AccountDto>)> {
    let account = account(&state, &raw).await?;
    if account.kind == AccountKind::Import {
        return Err(ApiError::bad_request("an import account has no server"));
    }
    state.mailboxes.sync(&account).await.map_err(refused)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(account_dto(&state, account).await?),
    ))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/imports",
    params(("id" = String, Path, description = "Archive id"), ImportParams),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "An mbox, .eml, .vcf, .ics or zip file (Google Takeout too)"),
    responses((status = 202, description = "Importing in the background into the import account named after the file", body = AccountDto), (status = 400, description = "An empty name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn import_file(
    State(state): AppState,
    Path(raw): Path<String>,
    Query(params): Query<ImportParams>,
    body: Body,
) -> ApiResult<(StatusCode, Json<AccountDto>)> {
    let archive = archive(&state, &raw).await?;
    let file_name = name(&params.name, "the file name")?;
    let account = import_account(&state, archive, &file_name).await?;
    let extension = FsPath::new(&file_name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    let dir = state.mailboxes.upload_dir();
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{}{extension}", AccountId::new()));
    if let Err(error) = write(&path, body).await {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    state
        .mailboxes
        .import(&account, path)
        .await
        .map_err(refused)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(account_dto(&state, account).await?),
    ))
}

async fn import_account(state: &ApiState, archive: ArchiveId, name: &str) -> ApiResult<Account> {
    if let Some(account) = state
        .store
        .list_accounts(archive)
        .await?
        .into_iter()
        .find(|a| a.kind == AccountKind::Import && a.name == name)
    {
        return Ok(account);
    }
    Ok(state
        .store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: name.to_owned(),
            address: None,
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await?)
}

async fn write(path: &FsPath, body: Body) -> ApiResult<()> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ApiError::bad_request("the upload broke off"))?;
        written += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    if written == 0 {
        return Err(ApiError::bad_request("the file is empty"));
    }
    Ok(())
}

fn conflict(message: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.to_owned())
}

#[utoipa::path(
    patch,
    path = "/api/archives/{id}",
    params(("id" = String, Path, description = "Archive id")),
    request_body = ArchiveUpdateRequest,
    responses((status = 200, body = ArchiveDto), (status = 400, description = "An empty or too long name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_archive(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<ArchiveUpdateRequest>,
) -> ApiResult<Json<ArchiveDto>> {
    let archive_id = archive(&state, &raw).await?;
    if let Some(raw_name) = &request.name {
        let name = name(raw_name, "the name")?;
        state.store.rename_archive(archive_id, &name).await?;
    }
    if let Some(save) = request.save_remote_images {
        state.store.set_save_remote_images(archive_id, save).await?;
    }
    let archive = state
        .store
        .get_archive(archive_id)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))?;
    Ok(Json(crate::routes::archive_dto(&state, archive).await?))
}

#[utoipa::path(
    delete,
    path = "/api/archives/{id}",
    params(("id" = String, Path, description = "Archive id")),
    responses((status = 204, description = "Removed with its accounts and messages; bytes no other archive uses are deleted"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 409, description = "One of its accounts is syncing or importing", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn delete_archive(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let archive_id = archive(&state, &raw).await?;
    for account in state.store.list_accounts(archive_id).await? {
        if busy(&state.mailboxes.status(account.id)) {
            return Err(conflict(
                "an account of this archive is syncing or importing; try again when it is done",
            ));
        }
    }
    let mut messages = Vec::new();
    let mut after = None;
    loop {
        let page = state
            .store
            .list_messages(&MessageQuery {
                archive_id,
                folder_id: None,
                label: None,
                category: None,
                after,
                limit: 500,
            })
            .await?;
        messages.extend(page.items.iter().map(|m| m.id));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let mut items = Vec::new();
    let mut after = None;
    loop {
        let page = state
            .store
            .list_items(&holtmail_store::ItemQuery {
                archive_id,
                kind: None,
                collection_id: None,
                removed: holtmail_store::RemovedFilter::Include,
                order: holtmail_store::ItemOrder::Title,
                after,
                limit: 500,
            })
            .await?;
        items.extend(page.items.iter().map(|i| i.item.id));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let orphans = state.store.delete_archive(archive_id).await?;
    for chunk in messages.chunks(500) {
        state.index.remove(chunk).await?;
    }
    for chunk in items.chunks(500) {
        state.index.remove_items(chunk).await?;
    }
    state.index.commit().await?;
    for blob in orphans {
        state.blobs.delete(&blob).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn account(state: &ApiState, raw: &str) -> ApiResult<Account> {
    let account_id: AccountId = id(raw, "account")?;
    state
        .store
        .get_account(account_id)
        .await?
        .ok_or_else(|| ApiError::not_found("account"))
}

#[utoipa::path(
    patch,
    path = "/api/accounts/{id}",
    params(("id" = String, Path, description = "Account id")),
    request_body = AccountUpdateRequest,
    responses((status = 200, body = AccountDto), (status = 400, description = "An empty name, a connection or password on an import account, an interval above a week, a minimum age above ten years, a change while syncing, or the server refused the new connection or password", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_account(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<AccountUpdateRequest>,
) -> ApiResult<Json<AccountDto>> {
    let mut account = account(&state, &raw).await?;
    if request.password.as_deref() == Some("") {
        return Err(ApiError::bad_request("the password is empty"));
    }
    let trusted = request
        .trusted_authserv_ids
        .as_deref()
        .map(crate::categories::trusted_ids)
        .transpose()?;
    if trusted.is_some() && account.kind == AccountKind::Import {
        return Err(ApiError::bad_request(
            "an import account has no server; set [insights] import_authserv_ids instead",
        ));
    }
    let change = AccountChange {
        connection: request.connection.map(|c| ConnectionChange {
            host: c.host,
            port: c.port,
            security: c.security.unwrap_or_else(|| "tls".into()),
            username: c.username,
            exclude: c.exclude,
        }),
        jmap: request
            .jmap
            .map(|c| {
                Ok::<_, ApiError>(JmapConnectionChange {
                    url: c.url,
                    username: c.username,
                    auth: jmap_auth(c.auth)?,
                    exclude: c.exclude,
                })
            })
            .transpose()?,
        password: request.password,
        sync_interval_minutes: request.sync_interval_minutes,
        min_age_days: request
            .min_age_days
            .map(|days| days.filter(|days| *days > 0)),
        contacts_calendars: request.contacts_calendars.map(|c| ContactsCalendarsChange {
            enabled: c.enabled,
            contacts_url: c.contacts_url,
            calendars_url: c.calendars_url,
            dav_password: c.dav_password,
        }),
        contacts_interval_minutes: request.contacts_interval_minutes,
        watch: request
            .watch
            .map(|w| {
                let mode = WatchMode::parse(&w.mode).ok_or_else(|| {
                    ApiError::bad_request("watch.mode is `off`, `inbox` or `folders`")
                })?;
                let folders: Vec<String> = w
                    .folders
                    .into_iter()
                    .map(|f| f.trim().to_owned())
                    .filter(|f| !f.is_empty())
                    .collect();
                if mode == WatchMode::Folders && folders.is_empty() {
                    return Err(ApiError::bad_request("watch.folders names no folder"));
                }
                if folders.len() > 100 {
                    return Err(ApiError::bad_request(
                        "watch.folders names more than 100 folders",
                    ));
                }
                Ok(WatchSettings { mode, folders })
            })
            .transpose()?,
    };
    if change != AccountChange::default() {
        account = state
            .mailboxes
            .configure(&account, change)
            .await
            .map_err(refused)?;
    }
    if let Some(new_name) = request.name {
        account.name = name(&new_name, "the name")?;
    }
    if let Some(enabled) = request.enabled {
        if account.kind == AccountKind::Import && enabled {
            return Err(ApiError::bad_request("an import account has no server"));
        }
        account.enabled = enabled;
    }
    state.store.update_account(&account).await?;
    if let Some(ids) = trusted {
        let lists = state.store.trusted_authserv_ids(account.archive_id).await?;
        if lists.get(&account.id) != Some(&ids) {
            state
                .store
                .set_trusted_authserv_ids(account.id, &ids)
                .await?;
            state.mailboxes.insights_changed(account.archive_id);
        }
    }
    Ok(Json(account_dto(&state, account).await?))
}

#[utoipa::path(
    delete,
    path = "/api/accounts/{id}",
    params(("id" = String, Path, description = "Account id")),
    responses((status = 204, description = "Removed with its folders; its messages stay in the archive"), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 409, description = "The account is syncing or importing", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn delete_account(
    State(state): AppState,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let account = account(&state, &raw).await?;
    if busy(&state.mailboxes.status(account.id)) {
        return Err(conflict(
            "the account is syncing or importing; try again when it is done",
        ));
    }
    state.store.delete_account(account.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/remote-images",
    params(("id" = String, Path, description = "Archive id")),
    request_body = RemoteImagesRequest,
    responses((status = 202, description = "Started, or already running for this archive", body = TaskDto), (status = 400, description = "Saving is off, or an unknown job", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn start_remote_images(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<RemoteImagesRequest>,
) -> ApiResult<(StatusCode, Json<TaskDto>)> {
    let archive_id = archive(&state, &raw).await?;
    let job = match request.job.as_str() {
        "older" => RemoteImageJob::Older,
        "failed" => RemoteImageJob::Failed,
        _ => return Err(ApiError::bad_request("job is `older` or `failed`")),
    };
    let task = state
        .mailboxes
        .start_remote_images(archive_id, job)
        .await
        .map_err(refused)?;
    Ok((StatusCode::ACCEPTED, Json(task.into())))
}

fn busy(status: &crate::mailboxes::AccountStatus) -> bool {
    status.running
        || status
            .contacts_calendars
            .as_ref()
            .is_some_and(|c| c.running)
}
