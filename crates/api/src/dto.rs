use holtmail_core::Address;
use holtmail_store::{
    Account, Archive, ArchiveStats, Attachment, FolderSummary, Location, Message, MessageExtras,
    User,
};
use serde::{Deserialize, Serialize};

use crate::{AccountDetails, AccountStatus, ImageProxy, Task, TaskKind, TaskLevel, TaskState};
use utoipa::{IntoParams, ToSchema};

/// An archive with its totals.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArchiveDto {
    pub id: String,
    pub name: String,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Remote images linked from its messages are downloaded and kept with the archive.
    pub save_remote_images: bool,
    pub stats: StatsDto,
    pub remote_images: RemoteImagesDto,
}

/// Remote images an archive keeps.
#[derive(Debug, Serialize, ToSchema)]
pub struct RemoteImagesDto {
    /// When saving was turned on, Unix milliseconds; `null` while it is off.
    pub since: Option<i64>,
    /// Saved images, each counted once.
    pub images: u64,
    /// Their size.
    pub bytes: u64,
    /// Images that could not be saved.
    pub failed: u64,
    /// Messages archived before saving was turned on and not looked at yet.
    pub older_pending: u64,
}

/// Which remote image work to start.
#[derive(Deserialize, ToSchema)]
pub struct RemoteImagesRequest {
    /// `older`: images of mail archived before saving was turned on. `failed`: fetch again the
    /// images that could not be saved.
    pub job: String,
}

/// A background task.
#[derive(Debug, Serialize, ToSchema)]
pub struct TaskDto {
    pub id: String,
    /// `sync`, `import` or `remote_images`.
    pub kind: String,
    /// `running`, `done`, `failed` or `cancelled`.
    pub state: String,
    pub title: String,
    pub archive_id: Option<String>,
    pub account_id: Option<String>,
    /// Unix milliseconds.
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub done: u64,
    /// `null` when the amount of work is not known.
    pub total: Option<u64>,
    pub summary: String,
    pub cancellable: bool,
    pub log: Vec<TaskLineDto>,
}

/// One line of a task's log.
#[derive(Debug, Serialize, ToSchema)]
pub struct TaskLineDto {
    /// Unix milliseconds.
    pub at: i64,
    /// `info`, `warning` or `error`.
    pub level: String,
    pub text: String,
}

/// The outbound proxy for remote images.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImageProxyDto {
    /// `scheme://host:port`, without credentials; `null` without a proxy.
    pub address: Option<String>,
    /// Set in the server's config; the API cannot change it.
    pub from_config: bool,
}

/// A new outbound proxy for remote images.
#[derive(Deserialize, ToSchema)]
pub struct ImageProxyRequest {
    /// `http://`, `https://`, `socks5://` or `socks5h://` with an optional `user:password@`;
    /// `null` or empty removes the proxy.
    pub proxy: Option<String>,
}

/// Totals of an archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct StatsDto {
    pub messages: u64,
    pub attachments: u64,
    pub bytes: u64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

/// A mail account of an archive.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountDto {
    pub id: String,
    pub name: String,
    /// `imap`, `graph`, `jmap` or `import`.
    pub kind: String,
    pub address: Option<String>,
    pub enabled: bool,
    pub status: AccountStatusDto,
    /// How an IMAP account connects; `null` for imports.
    pub connection: Option<ImapConnectionDto>,
    /// Its own minutes between syncs, 0 for only on request; `null` follows the server's.
    pub sync_interval_minutes: Option<u64>,
}

/// How an IMAP account connects. The password is never returned.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImapConnectionDto {
    pub host: String,
    pub port: u16,
    /// `tls`, `starttls` or `plain`.
    pub security: String,
    pub username: String,
    /// Folder paths that are not archived.
    pub exclude: Vec<String>,
}

/// A new connection for an IMAP account, checked by signing in before it is stored.
#[derive(Deserialize, ToSchema)]
pub struct ConnectionRequest {
    pub host: String,
    /// The default of `security` when missing.
    pub port: Option<u16>,
    /// `tls` (default), `starttls` or `plain`.
    pub security: Option<String>,
    pub username: String,
    /// Folder paths not to archive.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// The sync interval of accounts without their own.
#[derive(Debug, Serialize, ToSchema)]
pub struct SyncScheduleDto {
    /// Minutes between syncs; 0 means only on request.
    pub minutes: u64,
    /// Set in the server's config; the API cannot change it.
    pub from_config: bool,
}

/// A new sync interval for accounts without their own.
#[derive(Deserialize, ToSchema)]
pub struct SyncScheduleRequest {
    /// Minutes between syncs, at most a week; 0 syncs only on request.
    pub minutes: u64,
}

/// What is happening to an account now and how its last sync or import ended.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountStatusDto {
    /// A sync or import is running.
    pub running: bool,
    /// Messages added by the running or last run.
    pub new_messages: u64,
    /// When the last run ended, Unix milliseconds.
    pub finished_at: Option<i64>,
    /// Why the last run failed.
    pub error: Option<String>,
    /// When the scheduler syncs it next, Unix milliseconds; `null` when it does not.
    pub next_sync_at: Option<i64>,
}

/// A new archive.
#[derive(Deserialize, ToSchema)]
pub struct NewArchiveRequest {
    /// Not empty, at most 100 characters.
    pub name: String,
}

/// An IMAP account to add.
#[derive(Deserialize, ToSchema)]
pub struct ImapAccountRequest {
    /// Shown in the sidebar; the user name when missing.
    pub name: Option<String>,
    pub host: String,
    /// 993 for `tls`, 143 otherwise when missing.
    pub port: Option<u16>,
    /// `tls` (default), `starttls` or `plain`.
    pub security: Option<String>,
    pub username: String,
    pub password: String,
}

/// A user as owners and admins see them.
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonDto {
    pub user: UserDto,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Archives granted to the user. Owners and admins read every archive regardless.
    pub archives: Vec<String>,
}

/// A user to add.
#[derive(Deserialize, ToSchema)]
pub struct NewUserRequest {
    pub email: String,
    pub name: String,
    /// At least 10 characters.
    pub password: String,
    /// `owner`, `admin` or `member` (default). Only an owner adds owners.
    pub role: Option<String>,
    /// Archive ids the user may read.
    #[serde(default)]
    pub archives: Vec<String>,
}

/// Changes to a user; missing fields stay as they are.
#[derive(Deserialize, ToSchema)]
pub struct UserUpdateRequest {
    pub email: Option<String>,
    pub name: Option<String>,
    pub role: Option<String>,
    /// Replaces the granted archives.
    pub archives: Option<Vec<String>>,
    /// A new password; ends the user's sessions.
    pub password: Option<String>,
}

/// Changes to your own profile; missing fields stay as they are.
#[derive(Deserialize, ToSchema)]
pub struct ProfileRequest {
    pub email: Option<String>,
    pub name: Option<String>,
}

/// A password change.
#[derive(Deserialize, ToSchema)]
pub struct PasswordRequest {
    pub current: String,
    /// At least 10 characters.
    pub new: String,
}

/// Changes to an archive.
#[derive(Deserialize, ToSchema)]
pub struct ArchiveUpdateRequest {
    /// Not empty, at most 100 characters.
    pub name: Option<String>,
    /// Download remote images of its messages and keep them with the archive. Turning it on
    /// starts saving the images of messages already archived.
    pub save_remote_images: Option<bool>,
}

/// Changes to an account; missing fields stay as they are.
#[derive(Deserialize, ToSchema)]
pub struct AccountUpdateRequest {
    pub name: Option<String>,
    /// Paused accounts are not synced.
    pub enabled: Option<bool>,
    /// A new password; checked by signing in before it is stored.
    pub password: Option<String>,
    /// A new server, user name and excluded folders; checked by signing in, with `password`
    /// when given and the stored one otherwise.
    pub connection: Option<ConnectionRequest>,
    /// Its own minutes between syncs, 0 for only on request; `null` follows the server's again.
    /// Missing keeps it.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<u64>)]
    pub sync_interval_minutes: Option<Option<u64>>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Query of an upload.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImportParams {
    /// The file's name; its extension picks the format (`.mbox`, `.eml`, `.zip`).
    pub name: String,
}

/// A folder with its message count.
#[derive(Debug, Serialize, ToSchema)]
pub struct FolderDto {
    pub id: String,
    pub account_id: String,
    /// Full path with `/` between levels.
    pub path: String,
    /// The last level of the path.
    pub name: String,
    /// `inbox`, `sent`, `drafts`, `trash`, `junk`, `archive`, `all`, `flagged` or `important`.
    pub role: Option<String>,
    pub messages: u64,
    /// Messages in the folder without the `\Seen` flag there.
    pub unread: u64,
}

/// One mailbox of an address header.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AddressDto {
    pub name: Option<String>,
    pub email: String,
}

/// What a message list shows.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessageSummary {
    pub id: String,
    pub thread_id: String,
    pub subject: Option<String>,
    pub from: Vec<AddressDto>,
    pub to: Vec<AddressDto>,
    /// Unix milliseconds; what lists sort by.
    pub date: i64,
    pub preview: String,
    pub attachment_count: u32,
    pub size: u64,
    /// IMAP flags and keywords such as `\Seen` and `\Flagged`: those in the listed folder, or
    /// the union over every folder the message is in.
    pub flags: Vec<String>,
    /// Messages in its thread, itself included.
    pub thread_count: u32,
    /// Folders the message is in now.
    pub folder_ids: Vec<String>,
}

/// One page of a message list.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessagePage {
    pub items: Vec<MessageSummary>,
    /// Pass as `after` for the next page; absent on the last page.
    pub next: Option<String>,
}

/// One page of search results.
#[derive(Debug, Serialize, ToSchema)]
pub struct SearchResponse {
    pub items: Vec<MessageSummary>,
    pub total: u64,
    /// Pass as `offset` for the next page; absent on the last page.
    pub next_offset: Option<u32>,
}

/// An attachment of a message.
#[derive(Debug, Serialize, ToSchema)]
pub struct AttachmentDto {
    pub ordinal: u32,
    pub filename: Option<String>,
    pub content_type: String,
    pub size: u64,
    pub inline: bool,
    /// Where to download it.
    pub url: String,
}

/// A folder a message is in.
#[derive(Debug, Serialize, ToSchema)]
pub struct LocationDto {
    pub folder_id: String,
    pub flags: Vec<String>,
    /// Unix milliseconds when it disappeared from the folder on the server.
    pub removed_at: Option<i64>,
}

/// Everything the reader shows about a message, except its HTML body.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessageDetail {
    pub id: String,
    pub archive_id: String,
    pub thread_id: String,
    pub rfc_message_id: Option<String>,
    pub subject: Option<String>,
    pub from: Vec<AddressDto>,
    pub to: Vec<AddressDto>,
    pub cc: Vec<AddressDto>,
    pub bcc: Vec<AddressDto>,
    pub reply_to: Vec<AddressDto>,
    pub date: i64,
    pub sent_at: Option<i64>,
    pub received_at: Option<i64>,
    pub archived_at: i64,
    pub size: u64,
    /// One line from the start of the body.
    pub preview: String,
    /// `In-Reply-To` ids without angle brackets.
    pub in_reply_to: Vec<String>,
    /// `References` ids without angle brackets, oldest first.
    pub references: Vec<String>,
    /// Plain text body.
    pub text: Option<String>,
    /// Whether `GET /api/messages/{id}/body` has an HTML body to show.
    pub has_html: bool,
    pub attachments: Vec<AttachmentDto>,
    pub locations: Vec<LocationDto>,
}

/// A conversation, oldest message first.
#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadDto {
    pub id: String,
    pub messages: Vec<MessageDetail>,
}

/// An error.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDto {
    pub error: String,
}

/// A user who can sign in.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserDto {
    pub id: String,
    pub email: String,
    pub name: String,
    /// `owner`, `admin` or `member`.
    pub role: String,
}

impl From<User> for UserDto {
    fn from(user: User) -> Self {
        Self {
            id: user.id.to_string(),
            email: user.email,
            name: user.name,
            role: user.role.as_str().to_owned(),
        }
    }
}

/// The signed-in user.
#[derive(Debug, Serialize, ToSchema)]
pub struct SessionDto {
    pub user: UserDto,
}

/// Whether the instance still needs its owner.
#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatusDto {
    /// `true` while there are no users; the web app then shows the setup screen.
    pub needed: bool,
}

/// Creates the owner with the one-time setup token.
#[derive(Deserialize, ToSchema)]
pub struct SetupRequest {
    /// The `token` query parameter of the setup link.
    pub token: String,
    pub email: String,
    pub name: String,
    /// At least 10 characters.
    pub password: String,
}

/// Signs in with a password.
#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Paging of a message list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// Only messages in this folder.
    pub folder: Option<String>,
    /// `next` from the previous page.
    pub after: Option<String>,
    /// Page size, at most 500; 50 by default.
    pub limit: Option<u32>,
}

/// A search.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// The query, e.g. `from:ada@example.com has:attachment`.
    pub q: String,
    /// `newest` (default) or `relevance`.
    pub sort: Option<String>,
    pub offset: Option<u32>,
    /// Page size, at most 200; 50 by default.
    pub limit: Option<u32>,
}

/// How to render an HTML body.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BodyParams {
    /// What to do with remote images; `block` when missing.
    #[param(inline)]
    pub remote: Option<RemoteMode>,
    /// `false` loads images from the sender instead of the archive's saved copies, as `remote`
    /// says. Saved copies are shown when missing.
    pub saved: Option<bool>,
}

/// What to do with remote images in an HTML body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMode {
    /// Leave them out.
    #[default]
    Block,
    /// Load them through `/api/messages/{id}/remote/{key}`, so the reader's address stays
    /// private.
    Proxy,
    /// Keep the original addresses; the reader's browser contacts the sender's servers.
    Direct,
}

impl From<&Address> for AddressDto {
    fn from(address: &Address) -> Self {
        Self {
            name: address.name.clone(),
            email: address.email.clone(),
        }
    }
}

pub fn addresses(list: &[Address]) -> Vec<AddressDto> {
    list.iter().map(AddressDto::from).collect()
}

impl From<Task> for TaskDto {
    fn from(task: Task) -> Self {
        Self {
            id: task.id,
            kind: match task.kind {
                TaskKind::Sync => "sync",
                TaskKind::Import => "import",
                TaskKind::RemoteImages => "remote_images",
            }
            .into(),
            state: match task.state {
                TaskState::Running => "running",
                TaskState::Done => "done",
                TaskState::Failed => "failed",
                TaskState::Cancelled => "cancelled",
            }
            .into(),
            title: task.title,
            archive_id: task.archive_id.map(|id| id.to_string()),
            account_id: task.account_id.map(|id| id.to_string()),
            started_at: task.started_at.as_millis(),
            finished_at: task.finished_at.map(|t| t.as_millis()),
            done: task.done,
            total: task.total,
            summary: task.summary,
            cancellable: task.cancellable,
            log: task
                .log
                .into_iter()
                .map(|line| TaskLineDto {
                    at: line.at.as_millis(),
                    level: match line.level {
                        TaskLevel::Info => "info",
                        TaskLevel::Warning => "warning",
                        TaskLevel::Error => "error",
                    }
                    .into(),
                    text: line.text,
                })
                .collect(),
        }
    }
}

impl From<ImageProxy> for ImageProxyDto {
    fn from(proxy: ImageProxy) -> Self {
        Self {
            address: proxy.address,
            from_config: proxy.from_config,
        }
    }
}

impl ArchiveDto {
    pub fn new(archive: Archive, stats: ArchiveStats, remote_images: RemoteImagesDto) -> Self {
        Self {
            id: archive.id.to_string(),
            name: archive.name,
            created_at: archive.created_at.as_millis(),
            save_remote_images: archive.save_remote_images,
            remote_images,
            stats: StatsDto {
                messages: stats.messages,
                attachments: stats.attachments,
                bytes: stats.bytes,
                oldest: stats.oldest.map(|t| t.as_millis()),
                newest: stats.newest.map(|t| t.as_millis()),
            },
        }
    }
}

impl AccountDto {
    pub fn new(account: Account, status: AccountStatus, details: AccountDetails) -> Self {
        Self {
            id: account.id.to_string(),
            name: account.name,
            kind: account.kind.as_str().to_owned(),
            address: account.address,
            enabled: account.enabled,
            status: AccountStatusDto {
                running: status.running,
                new_messages: status.new_messages,
                finished_at: status.finished_at.map(|t| t.as_millis()),
                error: status.error,
                next_sync_at: status.next_sync_at.map(|t| t.as_millis()),
            },
            connection: details.connection.map(|c| ImapConnectionDto {
                host: c.host,
                port: c.port,
                security: c.security,
                username: c.username,
                exclude: c.exclude,
            }),
            sync_interval_minutes: details.sync_interval_minutes,
        }
    }
}

impl From<FolderSummary> for FolderDto {
    fn from(summary: FolderSummary) -> Self {
        let folder = summary.folder;
        Self {
            id: folder.id.to_string(),
            account_id: folder.account_id.to_string(),
            name: folder
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&folder.path)
                .to_owned(),
            path: folder.path,
            role: folder.role.map(|r| r.as_str().to_owned()),
            messages: summary.messages,
            unread: summary.unread,
        }
    }
}

impl MessageSummary {
    pub fn new(message: &Message, extras: Option<&MessageExtras>) -> Self {
        Self {
            id: message.id.to_string(),
            thread_id: message.thread_id.to_string(),
            subject: message.subject.clone(),
            from: addresses(&message.from),
            to: addresses(&message.to),
            date: message.date.as_millis(),
            preview: message.preview.clone(),
            attachment_count: message.attachment_count,
            size: message.size,
            flags: extras.map(|e| e.flags.clone()).unwrap_or_default(),
            thread_count: extras.map_or(1, |e| e.thread_count),
            folder_ids: extras
                .map(|e| e.folder_ids.iter().map(ToString::to_string).collect())
                .unwrap_or_default(),
        }
    }
}

pub fn attachment_dto(message: &Message, attachment: &Attachment) -> AttachmentDto {
    AttachmentDto {
        ordinal: attachment.ordinal,
        filename: attachment.filename.clone(),
        content_type: attachment.content_type.clone(),
        size: attachment.size,
        inline: attachment.inline,
        url: format!(
            "/api/messages/{}/attachments/{}",
            message.id, attachment.ordinal
        ),
    }
}

impl From<Location> for LocationDto {
    fn from(location: Location) -> Self {
        Self {
            folder_id: location.folder_id.to_string(),
            flags: location.flags,
            removed_at: location.removed_at.map(|t| t.as_millis()),
        }
    }
}
