use std::path::Path as FsPath;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use futures::StreamExt;
use holtmail_core::{AccountId, ArchiveId};
use holtmail_store::{Account, AccountKind, MessageQuery, NewAccount};
use tokio::io::AsyncWriteExt;

use crate::dto::*;
use crate::mailboxes::{MailboxError, NewImapAccount};
use crate::{ApiError, ApiResult, ApiState};

type AppState = State<Arc<ApiState>>;

const MAX_NAME: usize = 100;

fn id<T: std::str::FromStr>(raw: &str, what: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| ApiError::not_found(what))
}

fn name(raw: &str, what: &str) -> ApiResult<String> {
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

fn refused(error: MailboxError) -> ApiError {
    match error {
        MailboxError::Invalid(message) => ApiError::bad_request(message),
        MailboxError::Backend(message) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, message),
    }
}

fn account_dto(state: &ApiState, account: Account) -> AccountDto {
    let status = state.mailboxes.status(account.id);
    AccountDto::new(account, status)
}

async fn archive(state: &ApiState, raw: &str) -> ApiResult<ArchiveId> {
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
) -> ApiResult<Json<Vec<AccountDto>>> {
    let archive: ArchiveId = id(&raw, "archive")?;
    let accounts = state.store.list_accounts(archive).await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|a| account_dto(&state, a))
            .collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/api/archives",
    request_body = NewArchiveRequest,
    responses((status = 201, body = ArchiveDto), (status = 400, description = "An empty or too long name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn create_archive(
    State(state): AppState,
    Json(request): Json<NewArchiveRequest>,
) -> ApiResult<(StatusCode, Json<ArchiveDto>)> {
    let name = name(&request.name, "the name")?;
    let archive = state.store.create_archive(&name).await?;
    let stats = state.store.archive_stats(archive.id).await?;
    Ok((StatusCode::CREATED, Json(ArchiveDto::new(archive, stats))))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/accounts",
    params(("id" = String, Path, description = "Archive id")),
    request_body = ImapAccountRequest,
    responses((status = 201, description = "Stored; the first sync has started", body = AccountDto), (status = 400, description = "Bad settings, or the server refused to sign in", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
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
    Ok((StatusCode::CREATED, Json(account_dto(&state, account))))
}

#[utoipa::path(
    post,
    path = "/api/accounts/{id}/sync",
    params(("id" = String, Path, description = "Account id")),
    responses((status = 202, description = "Syncing in the background", body = AccountDto), (status = 400, description = "An import account, which has no server", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
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
    Ok((StatusCode::ACCEPTED, Json(account_dto(&state, account))))
}

#[utoipa::path(
    post,
    path = "/api/archives/{id}/imports",
    params(("id" = String, Path, description = "Archive id"), ImportParams),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "An mbox, .eml or zip file"),
    responses((status = 202, description = "Importing in the background into the import account named after the file", body = AccountDto), (status = 400, description = "An empty name", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such archive", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
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
    Ok((StatusCode::ACCEPTED, Json(account_dto(&state, account))))
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
        if save {
            state.mailboxes.save_remote_images(archive_id);
        }
    }
    let archive = state
        .store
        .get_archive(archive_id)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))?;
    let stats = state.store.archive_stats(archive_id).await?;
    Ok(Json(ArchiveDto::new(archive, stats)))
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
        if state.mailboxes.status(account.id).running {
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
    let orphans = state.store.delete_archive(archive_id).await?;
    for chunk in messages.chunks(500) {
        state.index.remove(chunk).await?;
    }
    state.index.commit().await?;
    for blob in orphans {
        state.blobs.delete(&blob).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn account(state: &ApiState, raw: &str) -> ApiResult<Account> {
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
    responses((status = 200, body = AccountDto), (status = 400, description = "An empty name, a password on an import account, or the server refused the new password", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "Not an owner or admin", body = ErrorDto), (status = 404, description = "No such account", body = ErrorDto), (status = 500, description = "The server failed", body = ErrorDto)),
)]
pub(crate) async fn update_account(
    State(state): AppState,
    Path(raw): Path<String>,
    Json(request): Json<AccountUpdateRequest>,
) -> ApiResult<Json<AccountDto>> {
    let mut account = account(&state, &raw).await?;
    if let Some(password) = request.password {
        if account.kind == AccountKind::Import {
            return Err(ApiError::bad_request("an import account has no password"));
        }
        if password.is_empty() {
            return Err(ApiError::bad_request("the password is empty"));
        }
        state
            .mailboxes
            .set_password(&account, password)
            .await
            .map_err(refused)?;
        account = state
            .store
            .get_account(account.id)
            .await?
            .ok_or_else(|| ApiError::not_found("account"))?;
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
    Ok(Json(account_dto(&state, account)))
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
    if state.mailboxes.status(account.id).running {
        return Err(conflict(
            "the account is syncing or importing; try again when it is done",
        ));
    }
    state.store.delete_account(account.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
