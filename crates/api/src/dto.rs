use holtmail_core::Address;
use holtmail_store::{
    Account, Archive, ArchiveStats, Attachment, FolderSummary, Location, Message, MessageExtras,
    User,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// An archive with its totals.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArchiveDto {
    pub id: String,
    pub name: String,
    /// Unix milliseconds.
    pub created_at: i64,
    pub stats: StatsDto,
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
    /// Load remote images and styles.
    pub remote: Option<bool>,
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

impl ArchiveDto {
    pub fn new(archive: Archive, stats: ArchiveStats) -> Self {
        Self {
            id: archive.id.to_string(),
            name: archive.name,
            created_at: archive.created_at.as_millis(),
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

impl From<Account> for AccountDto {
    fn from(account: Account) -> Self {
        Self {
            id: account.id.to_string(),
            name: account.name,
            kind: account.kind.as_str().to_owned(),
            address: account.address,
            enabled: account.enabled,
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
