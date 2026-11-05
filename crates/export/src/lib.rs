//! Writing archived messages out as one mbox file or as a zip of `.eml` files.
//!
//! An export streams the original stored bytes of each message unchanged, one message at a time,
//! so archives far larger than memory export with about one message held at once. Both formats
//! read back with `holtmail-import` to the same bytes, so importing an export into an archive that
//! already holds its messages adds nothing.

mod mbox;
mod names;
mod time;
mod zipped;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use holtmail_blob::{BlobError, BlobStore};
use holtmail_core::{ArchiveId, FolderId, MessageId};
use holtmail_index::{IndexError, SearchIndex, SearchQuery, SortOrder};
use holtmail_store::{Archive, Message, MessageQuery, PageCursor, Store, StoreError};
use tokio::io::{AsyncWrite, AsyncWriteExt};

const PAGE: u32 = 500;

/// How the messages are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One mbox file in mboxrd form: each message after a `From ` separator line carrying its
    /// sender and date, lines starting with `From ` (after any `>`) quoted with one more `>`, an
    /// empty line after each message. Folders are not kept.
    Mbox,
    /// A zip of `.eml` files, one per message, in directories named after the folder each
    /// message was found in.
    Zip,
}

impl Format {
    /// `mbox` or `zip`, in any letter case; `None` otherwise.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mbox" => Some(Self::Mbox),
            "zip" => Some(Self::Zip),
            _ => None,
        }
    }

    /// The file name extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mbox => "mbox",
            Self::Zip => "zip",
        }
    }

    /// The media type of the written file.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Mbox => "application/mbox",
            Self::Zip => "application/zip",
        }
    }
}

/// Which messages of the archive are exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every message.
    All,
    /// The messages located in this folder, removed from the server or not.
    Folder(FolderId),
    /// The messages carrying this label.
    Label(String),
    /// The messages matching this search.
    Search(SearchQuery),
}

/// What an export wrote so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Messages written.
    pub messages: u64,
    /// Original bytes of the written messages.
    pub bytes: u64,
    /// Messages left out because their bytes are missing from the blob store or no longer match
    /// their hash.
    pub skipped: u64,
}

/// Why an export could not start or stopped.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The archive or folder does not exist, or the folder is in another archive.
    #[error("{0} not found")]
    NotFound(&'static str),
    /// A search export was asked of an exporter without a search index.
    #[error("exporting a search needs the search index")]
    NoIndex,
    /// The search cannot run.
    #[error("invalid search: {0}")]
    InvalidSearch(String),
    /// Reading metadata failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Searching failed.
    #[error(transparent)]
    Index(IndexError),
    /// The blob store failed.
    #[error(transparent)]
    Blob(BlobError),
    /// Writing the output failed, e.g. the reader went away.
    #[error("writing the export failed: {0}")]
    Io(#[from] std::io::Error),
}

impl From<IndexError> for ExportError {
    fn from(error: IndexError) -> Self {
        match error {
            IndexError::Invalid(message) => Self::InvalidSearch(message),
            other => Self::Index(other),
        }
    }
}

/// Builds exports from the store, the blobs and, for searches, the index.
#[derive(Clone)]
pub struct Exporter {
    blobs: Arc<dyn BlobStore>,
    store: Arc<dyn Store>,
    index: Option<Arc<dyn SearchIndex>>,
}

impl Exporter {
    /// An exporter that can export everything except searches.
    pub fn new(blobs: Arc<dyn BlobStore>, store: Arc<dyn Store>) -> Self {
        Self {
            blobs,
            store,
            index: None,
        }
    }

    /// Adds the index, so searches can be exported.
    pub fn with_index(mut self, index: Arc<dyn SearchIndex>) -> Self {
        self.index = Some(index);
        self
    }

    /// Checks that the export can run and prepares it, before anything is written: the archive
    /// and folder exist and belong together, and searches have an index.
    pub async fn prepare(
        &self,
        archive: ArchiveId,
        selection: Selection,
        format: Format,
    ) -> Result<Export, ExportError> {
        let archive = self
            .store
            .get_archive(archive)
            .await?
            .ok_or(ExportError::NotFound("archive"))?;
        let accounts = self.store.list_accounts(archive.id).await?;
        let folders = self.store.list_folders(archive.id).await?;
        let several = folders
            .iter()
            .map(|f| f.folder.account_id)
            .collect::<HashSet<_>>()
            .len()
            > 1;
        let mut directories = HashMap::new();
        for (rank, summary) in folders.iter().enumerate() {
            let folder = &summary.folder;
            let path = match accounts.iter().find(|a| a.id == folder.account_id) {
                Some(account) if several => {
                    format!("{}/{}", account.name.replace('/', "_"), folder.path)
                }
                _ => folder.path.clone(),
            };
            directories.insert(folder.id, (rank, names::directory(&path)));
        }
        let scope_name = match &selection {
            Selection::All => None,
            Selection::Folder(id) => match directories.get(id) {
                Some(_) => folders
                    .iter()
                    .find(|f| f.folder.id == *id)
                    .map(|f| f.folder.path.clone()),
                None => return Err(ExportError::NotFound("folder")),
            },
            Selection::Label(label) => Some(label.clone()),
            Selection::Search(_) if self.index.is_none() => return Err(ExportError::NoIndex),
            Selection::Search(_) => Some("search".into()),
        };
        Ok(Export {
            exporter: self.clone(),
            archive,
            selection,
            format,
            directories,
            scope_name,
        })
    }
}

/// One prepared export.
pub struct Export {
    exporter: Exporter,
    archive: Archive,
    selection: Selection,
    format: Format,
    directories: HashMap<FolderId, (usize, String)>,
    scope_name: Option<String>,
}

#[derive(Default)]
struct Cursor {
    after: Option<PageCursor>,
    offset: u32,
    done: bool,
    seen: HashSet<MessageId>,
}

impl Export {
    /// A file name for the export: the archive's name, the folder, label or `search` when the
    /// export is narrowed, and the format's extension. Safe as a single path segment.
    pub fn file_name(&self) -> String {
        let base = match &self.scope_name {
            Some(scope) => format!("{} - {}", self.archive.name, scope),
            None => self.archive.name.clone(),
        };
        format!("{}.{}", names::segment(&base, 120), self.format.extension())
    }

    /// The format being written.
    pub fn format(&self) -> Format {
        self.format
    }

    /// Writes the export to `out` and flushes it, newest message first, calling `progress`
    /// after each page of messages.
    ///
    /// Messages whose bytes are missing or corrupt are left out and counted in
    /// [`Totals::skipped`]; any other failure stops the export, leaving `out` incomplete.
    pub async fn write(
        self,
        out: &mut (dyn AsyncWrite + Send + Unpin),
        progress: &(dyn Fn(&Totals) + Send + Sync),
    ) -> Result<Totals, ExportError> {
        let mut totals = Totals::default();
        let mut zip = (self.format == Format::Zip).then(zipped::StreamZip::new);
        let mut cursor = Cursor::default();
        while let Some(page) = self.next_page(&mut cursor).await? {
            let directories = match zip {
                Some(_) => self.directories_of(&page).await?,
                None => HashMap::new(),
            };
            for message in &page {
                let bytes = match self.exporter.blobs.get(&message.blob_id).await {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) | Err(BlobError::Corrupt(_)) => {
                        totals.skipped += 1;
                        continue;
                    }
                    Err(error) => return Err(ExportError::Blob(error)),
                };
                let size = bytes.len() as u64;
                match &mut zip {
                    Some(zip) => {
                        let name =
                            names::entry(directories.get(&message.id).map(String::as_str), message);
                        zip.add(out, name, message.date, bytes).await?;
                    }
                    None => {
                        let sender = mbox::envelope_sender(&message.from);
                        mbox::write_message(out, &bytes, &sender, message.date).await?;
                    }
                }
                totals.messages += 1;
                totals.bytes += size;
            }
            progress(&totals);
        }
        if let Some(zip) = zip {
            zip.finish(out).await?;
        }
        out.flush().await?;
        Ok(totals)
    }

    async fn next_page(&self, cursor: &mut Cursor) -> Result<Option<Vec<Message>>, ExportError> {
        if cursor.done {
            return Ok(None);
        }
        let store = &self.exporter.store;
        let (folder, label) = match &self.selection {
            Selection::Search(query) => {
                let index = self.exporter.index.as_ref().ok_or(ExportError::NoIndex)?;
                let results = index
                    .search(
                        self.archive.id,
                        query,
                        SortOrder::Newest,
                        cursor.offset,
                        PAGE,
                    )
                    .await?;
                cursor.offset = cursor.offset.saturating_add(PAGE);
                if results.hits.len() < PAGE as usize || u64::from(cursor.offset) >= results.total {
                    cursor.done = true;
                }
                let fresh: Vec<MessageId> = results
                    .hits
                    .into_iter()
                    .filter(|id| cursor.seen.insert(*id))
                    .collect();
                let messages = store.get_messages(&fresh).await?;
                return Ok(Some(
                    messages
                        .into_iter()
                        .filter(|m| m.archive_id == self.archive.id)
                        .collect(),
                ));
            }
            Selection::All => (None, None),
            Selection::Folder(id) => (Some(*id), None),
            Selection::Label(label) => (None, Some(label.clone())),
        };
        let page = store
            .list_messages(&MessageQuery {
                archive_id: self.archive.id,
                folder_id: folder,
                label,
                after: cursor.after.take(),
                limit: PAGE,
            })
            .await?;
        match page.next {
            Some(next) => cursor.after = Some(next),
            None => cursor.done = true,
        }
        Ok(Some(page.items))
    }

    async fn directories_of(
        &self,
        page: &[Message],
    ) -> Result<HashMap<MessageId, String>, ExportError> {
        if let Selection::Folder(id) = &self.selection {
            let dir = self
                .directories
                .get(id)
                .map(|(_, d)| d.clone())
                .unwrap_or_default();
            return Ok(page.iter().map(|m| (m.id, dir.clone())).collect());
        }
        let ids: Vec<MessageId> = page.iter().map(|m| m.id).collect();
        let extras = self.exporter.store.message_extras(&ids, None).await?;
        Ok(extras
            .into_iter()
            .filter_map(|(id, extra)| {
                extra
                    .folder_ids
                    .iter()
                    .filter_map(|f| self.directories.get(f))
                    .min_by_key(|(rank, _)| *rank)
                    .map(|(_, dir)| (id, dir.clone()))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
