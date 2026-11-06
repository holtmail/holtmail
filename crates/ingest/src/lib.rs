//! From raw message bytes to a stored, indexed message, and from raw vCard and iCalendar objects
//! to stored item versions.
//!
//! Sync and import both hand every message to [`Ingest::ingest`] and every contact, event or task
//! to [`Ingest::ingest_item`], so there is one path into the archive and one set of rules: bytes
//! first, then metadata, then the index.

mod pim;

use std::sync::Arc;

use bytes::Bytes;
use holtmail_blob::{BlobError, BlobId, BlobStore};
use holtmail_core::{ArchiveId, FolderId, MessageId, ThreadId, Timestamp};
use holtmail_extract::{ExtractError, Extractor, File};
use holtmail_index::{IndexDocument, IndexError, SearchIndex};
use holtmail_mime::{MimeError, ParsedMessage};
use holtmail_store::{Attachment, Derived, DerivedKind, NewMessage, Store, StoreError};

pub use pim::{IncomingItem, MAIL_COLLECTION, item_document, pim_index};

/// Plain text indexed per message; the stored bytes are never cut.
const MAX_INDEXED_TEXT: usize = 1 << 20;

/// Attachment text indexed per message, all attachments together.
pub const MAX_ATTACHMENT_TEXT: usize = 4 << 20;

/// Where a message was found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// The folder, when the message came from one.
    pub folder: Option<FolderId>,
    /// Its UID in the folder.
    pub uid: Option<u64>,
    /// Flags as the source reported them.
    pub flags: Vec<String>,
    /// Labels as the source reported them; they replace the message's labels. `None` leaves the
    /// labels as they are.
    pub labels: Option<Vec<String>>,
    /// When the server received it.
    pub received_at: Option<Timestamp>,
}

/// What [`Ingest::ingest`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ingested {
    /// The message.
    pub message_id: MessageId,
    /// Its thread.
    pub thread_id: ThreadId,
    /// `false` when the archive already had these bytes.
    pub created: bool,
}

/// Why a message could not be ingested.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The bytes are not an email message.
    #[error(transparent)]
    Mime(#[from] MimeError),
    /// Storing bytes failed.
    #[error(transparent)]
    Blob(#[from] BlobError),
    /// Storing metadata failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Indexing failed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// These bytes were deleted from the archive on purpose and are not archived again.
    #[error("deleted from this archive")]
    Deleted,
}

/// The ingest pipeline over the three storage contracts.
#[derive(Clone)]
pub struct Ingest {
    blobs: Arc<dyn BlobStore>,
    store: Arc<dyn Store>,
    index: Arc<dyn SearchIndex>,
    extractor: Option<Arc<dyn Extractor>>,
}

impl Ingest {
    /// A pipeline writing to these stores.
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        store: Arc<dyn Store>,
        index: Arc<dyn SearchIndex>,
    ) -> Self {
        Self {
            blobs,
            store,
            index,
            extractor: None,
        }
    }

    /// The same pipeline, also indexing text from attachments that are not inline.
    ///
    /// Text is extracted once per attachment blob and kept as a derived blob, so the same file in
    /// many messages, and later reindexing, reuse it. Files the extractor cannot read are
    /// recorded and not tried again; timeouts are tried again next time. Extraction never fails
    /// ingest.
    pub fn with_extractor(mut self, extractor: Arc<dyn Extractor>) -> Self {
        self.extractor = Some(extractor);
        self
    }

    /// Archives `raw` into `archive` at `placement`.
    ///
    /// A new message's calendar parts (iMIP invitations, updates, replies and cancellations, and
    /// attached `.ics` files) become versions of events and tasks in the archive's "From mail"
    /// calendar, keyed by `UID` and linked to the message; see [`Ingest::record_invitations`].
    /// Idempotent: the same bytes again only update where the message was seen. Bytes deleted
    /// from the archive on purpose are not stored again: [`IngestError::Deleted`], and their
    /// folder and UID are remembered so the source is not asked for them again. The raw bytes are
    /// durable before any metadata refers to them, so a crash never leaves a message without its
    /// bytes. The index change becomes searchable after [`Ingest::commit`]; callers commit once per
    /// batch.
    pub async fn ingest(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        placement: &Placement,
    ) -> Result<Ingested, IngestError> {
        let parsed = holtmail_mime::parse(&raw)?;
        let size = raw.len() as u64;
        if self.store.is_deleted(archive, &BlobId::of(&raw)).await? {
            if let (Some(folder), Some(uid)) = (placement.folder, placement.uid) {
                self.store.record_deleted_location(folder, uid).await?;
            }
            return Err(IngestError::Deleted);
        }
        let blob_id = self.blobs.put(raw).await?;

        let inserted = match self.store.find_by_blob(archive, &blob_id).await? {
            Some(message_id) => {
                let message = self
                    .store
                    .get_message(message_id)
                    .await?
                    .ok_or(StoreError::NotFound("message"))?;
                Ingested {
                    message_id,
                    thread_id: message.thread_id,
                    created: false,
                }
            }
            None => {
                let attachments = self.store_attachments(&parsed).await?;
                let inserted = self
                    .store
                    .insert_message(new_message(
                        archive,
                        blob_id,
                        size,
                        &parsed,
                        placement,
                        attachments,
                    ))
                    .await?;
                Ingested {
                    message_id: inserted.message_id,
                    thread_id: inserted.thread_id,
                    created: inserted.created,
                }
            }
        };

        if inserted.created {
            self.invitations(
                archive,
                inserted.message_id,
                parsed.sent_at.map(Timestamp::from_secs),
                &parsed.calendars,
            )
            .await?;
        }
        if let Some(folder) = placement.folder {
            self.store
                .record_location(
                    inserted.message_id,
                    folder,
                    placement.uid,
                    &placement.flags,
                    Timestamp::now(),
                )
                .await?;
        }
        if let Some(labels) = &placement.labels {
            self.store.set_labels(inserted.message_id, labels).await?;
        }
        self.index_parsed(inserted.message_id, &parsed).await?;
        Ok(inserted)
    }

    /// Rebuilds the index document of a message from its stored bytes, e.g. after its folders,
    /// flags or labels changed. Searchable after [`Ingest::commit`].
    pub async fn reindex(&self, message: MessageId) -> Result<(), IngestError> {
        let stored = self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?;
        let raw = self
            .blobs
            .get(&stored.blob_id)
            .await?
            .ok_or(StoreError::NotFound("message bytes"))?;
        let parsed = holtmail_mime::parse(&raw)?;
        self.index_parsed(message, &parsed).await
    }

    /// Reads the text of the attachments of `message` that `which` picks again, ignoring what
    /// was kept from an earlier reading, deletes kept text nothing else uses, and rebuilds the
    /// message's index document. Returns how many attachments were picked. Searchable after
    /// [`Ingest::commit`].
    pub async fn refresh_attachment_text(
        &self,
        message: MessageId,
        which: &(dyn Fn(&Attachment) -> bool + Send + Sync),
    ) -> Result<u32, IngestError> {
        let mut picked = 0;
        for attachment in self.store.attachments(message).await? {
            if attachment.inline || !which(&attachment) {
                continue;
            }
            picked += 1;
            if let Some(orphan) = self
                .store
                .forget_derived(&attachment.blob_id, DerivedKind::Text)
                .await?
            {
                self.blobs.delete(&orphan).await?;
            }
        }
        if picked > 0 {
            self.reindex(message).await?;
        }
        Ok(picked)
    }

    /// Makes everything ingested so far searchable.
    pub async fn commit(&self) -> Result<(), IngestError> {
        Ok(self.index.commit().await?)
    }

    async fn store_attachments(
        &self,
        parsed: &ParsedMessage,
    ) -> Result<Vec<Attachment>, IngestError> {
        let mut out = Vec::with_capacity(parsed.attachments.len());
        for (ordinal, part) in parsed.attachments.iter().enumerate() {
            let blob_id = self.blobs.put(part.content.clone()).await?;
            out.push(Attachment {
                ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                blob_id,
                filename: part.filename.clone(),
                content_type: part.content_type.clone(),
                size: part.content.len() as u64,
                content_id: part.content_id.clone(),
                inline: part.inline,
            });
        }
        Ok(out)
    }

    async fn index_parsed(
        &self,
        message: MessageId,
        parsed: &ParsedMessage,
    ) -> Result<(), IngestError> {
        let stored = self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?;
        let mut folders = Vec::new();
        let mut flags = Vec::new();
        for location in self.store.locations(message).await? {
            if location.removed_at.is_some() {
                continue;
            }
            if let Some(folder) = self.store.get_folder(location.folder_id).await? {
                folders.push(folder.path);
                if let Some(role) = folder.role {
                    folders.push(role.as_str().to_owned());
                }
            }
            flags.extend(location.flags);
        }
        folders.sort();
        folders.dedup();
        flags.sort();
        flags.dedup();

        let attachments: Vec<_> = parsed.attachments.iter().filter(|a| !a.inline).collect();
        let document = IndexDocument {
            message_id: message,
            archive_id: stored.archive_id,
            thread_id: stored.thread_id,
            subject: parsed.subject.clone().unwrap_or_default(),
            from: parsed.from.clone(),
            recipients: [&parsed.to, &parsed.cc, &parsed.bcc]
                .into_iter()
                .flatten()
                .cloned()
                .collect(),
            body: truncate(parsed.text.as_deref().unwrap_or_default()),
            attachment_names: attachments
                .iter()
                .filter_map(|a| a.filename.clone())
                .collect(),
            attachment_text: self.attachment_text(message, parsed).await?,
            folders,
            flags,
            labels: self.store.labels(message).await?,
            date: stored.date,
            has_attachments: !attachments.is_empty(),
        };
        self.index.put(vec![document]).await?;
        Ok(())
    }
}

impl Ingest {
    async fn attachment_text(
        &self,
        message: MessageId,
        parsed: &ParsedMessage,
    ) -> Result<String, IngestError> {
        let Some(extractor) = &self.extractor else {
            return Ok(String::new());
        };
        let mut out = String::new();
        for attachment in self.store.attachments(message).await? {
            if attachment.inline || out.len() >= MAX_ATTACHMENT_TEXT {
                continue;
            }
            let cached = match self
                .store
                .derived(&attachment.blob_id, DerivedKind::Text)
                .await?
            {
                Some(Derived::Nothing) => continue,
                Some(Derived::Blob(blob)) => self.blobs.get(&blob).await?,
                None => None,
            };
            let text = match cached {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => match self.extract(&**extractor, &attachment, parsed).await? {
                    Some(text) => text,
                    None => continue,
                },
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text);
        }
        Ok(holtmail_extract::cut(out, MAX_ATTACHMENT_TEXT))
    }

    async fn extract(
        &self,
        extractor: &dyn Extractor,
        attachment: &Attachment,
        parsed: &ParsedMessage,
    ) -> Result<Option<String>, IngestError> {
        if attachment.size > holtmail_extract::MAX_INPUT as u64 {
            self.store
                .set_derived(&attachment.blob_id, DerivedKind::Text, &Derived::Nothing)
                .await?;
            return Ok(None);
        }
        let content = match parsed.attachments.get(attachment.ordinal as usize) {
            Some(part) => part.content.clone(),
            None => match self.blobs.get(&attachment.blob_id).await? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
        };
        let file = File {
            content_type: attachment.content_type.clone(),
            filename: attachment.filename.clone(),
            content,
        };
        let derived = match extractor.extract(&file).await {
            Ok(None) | Err(ExtractError::TimedOut) => return Ok(None),
            Ok(Some(text)) if !text.trim().is_empty() => {
                let blob = self.blobs.put(Bytes::from(text.clone())).await?;
                self.store
                    .set_derived(&attachment.blob_id, DerivedKind::Text, &Derived::Blob(blob))
                    .await?;
                return Ok(Some(text));
            }
            Ok(Some(_)) | Err(ExtractError::TooLarge | ExtractError::Unreadable(_)) => {
                Derived::Nothing
            }
        };
        self.store
            .set_derived(&attachment.blob_id, DerivedKind::Text, &derived)
            .await?;
        Ok(None)
    }
}

fn new_message(
    archive: ArchiveId,
    blob_id: holtmail_blob::BlobId,
    size: u64,
    parsed: &ParsedMessage,
    placement: &Placement,
    attachments: Vec<Attachment>,
) -> NewMessage {
    let mut parent_ids = parsed.references.clone();
    parent_ids.extend(parsed.in_reply_to.iter().cloned());
    NewMessage {
        archive_id: archive,
        blob_id,
        size,
        rfc_message_id: parsed.message_id.clone(),
        parent_ids,
        subject: parsed.subject.clone(),
        from: parsed.from.clone(),
        to: parsed.to.clone(),
        cc: parsed.cc.clone(),
        bcc: parsed.bcc.clone(),
        sent_at: parsed.sent_at.map(Timestamp::from_secs),
        received_at: placement.received_at,
        preview: parsed.preview.clone(),
        attachments,
    }
}

fn truncate(text: &str) -> String {
    if text.len() <= MAX_INDEXED_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_INDEXED_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use holtmail_blob_fs::FsBlobStore;
    use holtmail_core::CollectionId;
    use holtmail_index::{Clause, SearchQuery, SortOrder};
    use holtmail_index_tantivy::TantivyIndex;
    use holtmail_store::{AccountKind, NewAccount};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    struct Fixture {
        ingest: Ingest,
        store: Arc<dyn Store>,
        index: Arc<dyn SearchIndex>,
        blobs: Arc<dyn BlobStore>,
        archive: ArchiveId,
        inbox: FolderId,
        _dir: tempfile::TempDir,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> =
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
        let archive = store.create_archive("Test").await.unwrap().id;
        let account = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Import,
                name: "Import".into(),
                address: None,
                settings: Default::default(),
                sealed_secret: None,
                enabled: false,
            })
            .await
            .unwrap();
        let inbox = store
            .upsert_folder(account.id, "INBOX", Some(holtmail_store::FolderRole::Inbox))
            .await
            .unwrap()
            .id;
        Fixture {
            ingest: Ingest::new(blobs.clone(), store.clone(), index.clone()),
            store,
            index,
            blobs,
            archive,
            inbox,
            _dir: dir,
        }
    }

    const REPLY: &[u8] = b"From: Bob <bob@example.org>\r\nTo: ada@example.com\r\nSubject: Re: Lunch\r\nMessage-ID: <r1@example.org>\r\nIn-Reply-To: <l1@example.com>\r\nDate: Fri, 14 Mar 2025 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nNoon works.\r\n--b\r\nContent-Type: text/plain; name=menu.txt\r\nContent-Disposition: attachment; filename=menu.txt\r\n\r\nSoup\r\n--b--\r\n";
    const ROOT: &[u8] = b"From: Ada <ada@example.com>\r\nTo: bob@example.org\r\nSubject: Lunch\r\nMessage-ID: <l1@example.com>\r\nDate: Fri, 14 Mar 2025 09:00:00 +0000\r\n\r\nShall we?\r\n";

    fn in_inbox(f: &Fixture, uid: u64, flags: &[&str]) -> Placement {
        Placement {
            folder: Some(f.inbox),
            uid: Some(uid),
            flags: flags.iter().map(|s| s.to_string()).collect(),
            labels: None,
            received_at: None,
        }
    }

    async fn find(f: &Fixture, clauses: Vec<Clause>) -> Vec<MessageId> {
        f.index
            .search(
                f.archive,
                &SearchQuery { clauses },
                SortOrder::Newest,
                0,
                50,
            )
            .await
            .unwrap()
            .hits
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stores_threads_and_indexes_a_message() {
        let f = fixture().await;
        let reply = f
            .ingest
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 2, &[]))
            .await
            .unwrap();
        let root = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(ROOT),
                &in_inbox(&f, 1, &["\\Seen"]),
            )
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();

        assert!(reply.created && root.created);
        assert_eq!(
            reply.thread_id,
            f.store
                .get_message(root.message_id)
                .await
                .unwrap()
                .unwrap()
                .thread_id
        );
        let stored = f
            .store
            .get_message(reply.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.attachment_count, 1);
        assert_eq!(stored.date, Timestamp::from_secs(1_741_946_400));
        assert_eq!(
            f.blobs.get(&stored.blob_id).await.unwrap().unwrap(),
            Bytes::from_static(REPLY)
        );
        let attachment = &f.store.attachments(reply.message_id).await.unwrap()[0];
        assert_eq!(
            f.blobs.get(&attachment.blob_id).await.unwrap().unwrap(),
            Bytes::from_static(b"Soup")
        );

        assert_eq!(
            find(&f, vec![Clause::Text("noon".into())]).await,
            [reply.message_id]
        );
        assert_eq!(
            find(&f, vec![Clause::Filename("menu".into())]).await,
            [reply.message_id]
        );
        assert_eq!(find(&f, vec![Clause::In("inbox".into())]).await.len(), 2);
        assert_eq!(
            find(&f, vec![Clause::Flag("\\Seen".into())]).await,
            [root.message_id]
        );
    }

    struct Counting(
        std::sync::atomic::AtomicUsize,
        holtmail_extract_basic::BasicExtractor,
    );

    #[async_trait::async_trait]
    impl Extractor for Counting {
        async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.1.extract(file).await
        }
    }

    const FORWARD: &[u8] = b"From: Carol <carol@example.org>\r\nTo: ada@example.com\r\nSubject: Fwd: menu\r\nMessage-ID: <f1@example.org>\r\nDate: Sat, 15 Mar 2025 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--b\r\nContent-Type: application/octet-stream; name=menu.txt\r\nContent-Disposition: attachment; filename=menu.txt\r\n\r\nSoup\r\n--b\r\nContent-Type: application/pdf; name=broken.pdf\r\nContent-Disposition: attachment; filename=broken.pdf\r\n\r\nnot a pdf\r\n--b--\r\n";

    struct Fixed(&'static str);

    #[async_trait::async_trait]
    impl Extractor for Fixed {
        async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError> {
            Ok(Some(format!(
                "{} {}",
                self.0,
                file.filename.as_deref().unwrap_or_default()
            )))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_picked_attachments_again() {
        let f = fixture().await;
        let old = f.ingest.clone().with_extractor(Arc::new(Fixed("oldword")));
        let forward = old
            .ingest(
                f.archive,
                Bytes::from_static(FORWARD),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap();
        old.commit().await.unwrap();
        let menu = f.store.attachments(forward.message_id).await.unwrap()[0].clone();
        let Some(Derived::Blob(kept)) = f
            .store
            .derived(&menu.blob_id, DerivedKind::Text)
            .await
            .unwrap()
        else {
            panic!("no text kept");
        };

        let new = f.ingest.clone().with_extractor(Arc::new(Fixed("newword")));
        let picked = new
            .refresh_attachment_text(forward.message_id, &|a| {
                a.filename.as_deref() == Some("menu.txt")
            })
            .await
            .unwrap();
        new.commit().await.unwrap();
        assert_eq!(picked, 1);
        assert!(
            !f.blobs.contains(&kept).await.unwrap(),
            "old text left behind"
        );
        assert_eq!(
            find(&f, vec![Clause::Text("newword".into())]).await,
            [forward.message_id]
        );
        let pdf = &f.store.attachments(forward.message_id).await.unwrap()[1];
        assert!(
            matches!(
                f.store
                    .derived(&pdf.blob_id, DerivedKind::Text)
                    .await
                    .unwrap(),
                Some(Derived::Blob(_))
            ),
            "an attachment not picked was forgotten"
        );
        assert_eq!(
            find(&f, vec![Clause::Text("oldword".into())]).await,
            [forward.message_id],
            "the attachment not picked lost its text"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn indexes_attachment_text_once_per_file() {
        let f = fixture().await;
        let counter = Arc::new(Counting(Default::default(), Default::default()));
        let ingest = f.ingest.clone().with_extractor(counter.clone());
        let reply = ingest
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        let forward = ingest
            .ingest(
                f.archive,
                Bytes::from_static(FORWARD),
                &in_inbox(&f, 2, &[]),
            )
            .await
            .unwrap();
        ingest.commit().await.unwrap();
        let calls = || counter.0.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(calls(), 2, "the shared file was read twice");

        let results = f
            .index
            .search(
                f.archive,
                &SearchQuery {
                    clauses: vec![Clause::Text("soup".into())],
                },
                SortOrder::Newest,
                0,
                50,
            )
            .await
            .unwrap();
        assert_eq!(results.hits, [forward.message_id, reply.message_id]);
        assert_eq!(results.attachment_hits, results.hits);

        let broken = &f.store.attachments(forward.message_id).await.unwrap()[1];
        assert_eq!(
            f.store
                .derived(&broken.blob_id, DerivedKind::Text)
                .await
                .unwrap(),
            Some(Derived::Nothing)
        );

        ingest.reindex(reply.message_id).await.unwrap();
        ingest.reindex(forward.message_id).await.unwrap();
        ingest.commit().await.unwrap();
        assert_eq!(calls(), 2, "reindexing read the files again");
        assert_eq!(
            find(&f, vec![Clause::Text("soup".into())]).await.len(),
            2,
            "reindexing lost the attachment text"
        );

        f.ingest.reindex(reply.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::Text("soup".into())]).await,
            [forward.message_id],
            "without an extractor no attachment text is indexed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_bytes_only_add_a_location() {
        let f = fixture().await;
        let first = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        let account = f
            .store
            .get_folder(f.inbox)
            .await
            .unwrap()
            .unwrap()
            .account_id;
        let archive_folder = f
            .store
            .upsert_folder(account, "Archive", None)
            .await
            .unwrap()
            .id;
        let again = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(ROOT),
                &Placement {
                    folder: Some(archive_folder),
                    uid: Some(7),
                    ..Placement::default()
                },
            )
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(again.message_id, first.message_id);
        assert!(!again.created);
        assert_eq!(f.store.locations(first.message_id).await.unwrap().len(), 2);
        assert_eq!(
            find(&f, vec![Clause::In("archive".into())]).await,
            [first.message_id]
        );
        assert_eq!(f.store.archive_stats(f.archive).await.unwrap().messages, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_bytes_that_are_not_mail() {
        let f = fixture().await;
        let result = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(b"just text"),
                &Placement::default(),
            )
            .await;
        assert!(matches!(result, Err(IngestError::Mime(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reindex_picks_up_new_flags() {
        let f = fixture().await;
        let ingested = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        f.store
            .record_location(
                ingested.message_id,
                f.inbox,
                Some(1),
                &["\\Flagged".into()],
                Timestamp::now(),
            )
            .await
            .unwrap();
        f.ingest.reindex(ingested.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::Flag("\\Flagged".into())]).await,
            [ingested.message_id]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stores_and_indexes_labels() {
        let f = fixture().await;
        let mut placement = in_inbox(&f, 1, &[]);
        placement.labels = Some(vec!["Inbox".into(), "Projects/2025".into()]);
        let ingested = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &placement)
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::Label("projects/2025".into())]).await,
            [ingested.message_id]
        );

        let mut again = in_inbox(&f, 1, &[]);
        again.labels = None;
        f.ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &again)
            .await
            .unwrap();
        assert_eq!(
            f.store.labels(ingested.message_id).await.unwrap(),
            ["Inbox", "Projects/2025"],
            "a placement without labels dropped them"
        );

        f.store
            .set_labels(ingested.message_id, &["Done".into()])
            .await
            .unwrap();
        f.ingest.reindex(ingested.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert!(
            find(&f, vec![Clause::Label("inbox".into())])
                .await
                .is_empty()
        );
        assert_eq!(
            find(&f, vec![Clause::Label("done".into())]).await,
            [ingested.message_id]
        );
    }

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn card(name: &str) -> Bytes {
        Bytes::from(format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{name}\r\nFN:{name}\r\nEMAIL:{name}@example.com\r\nPHOTO;ENCODING=b;TYPE=PNG:{PNG}\r\nEND:VCARD\r\n"
        ))
    }

    fn item(collection: CollectionId, remote: &str, raw: Bytes) -> IncomingItem {
        IncomingItem {
            collection,
            remote_id: remote.into(),
            kind: holtmail_pim::ItemKind::Contact,
            format: holtmail_pim::Format::of(holtmail_pim::ItemKind::Contact),
            raw,
            etag: None,
            seen_at: Timestamp::from_secs(1_700_000_000),
        }
    }

    #[tokio::test]
    async fn items_keep_raw_bytes_and_share_pictures() {
        let f = fixture().await;
        let book = f
            .store
            .upsert_collection(holtmail_store::NewCollection {
                archive_id: f.archive,
                account_id: None,
                kind: holtmail_store::CollectionKind::Contacts,
                remote_id: "people.vcf".into(),
                name: "People".into(),
                color: None,
            })
            .await
            .unwrap()
            .id;
        let ada = f
            .ingest
            .ingest_item(item(book, "ada", card("Ada")))
            .await
            .unwrap();
        let bo = f
            .ingest
            .ingest_item(item(book, "bo", card("Bo")))
            .await
            .unwrap();
        let ada_versions = f.store.item_versions(ada.item_id).await.unwrap();
        let bo_versions = f.store.item_versions(bo.item_id).await.unwrap();
        let photo = ada_versions[0].photo_blob_id.expect("picture split off");
        assert_eq!(
            bo_versions[0].photo_blob_id,
            Some(photo),
            "one blob per picture"
        );
        assert!(
            f.blobs
                .get(&photo)
                .await
                .unwrap()
                .unwrap()
                .starts_with(b"\x89PNG")
        );
        assert_eq!(
            f.blobs
                .get(&ada_versions[0].blob_id)
                .await
                .unwrap()
                .unwrap(),
            card("Ada"),
            "raw stays whole"
        );
        assert_eq!(ada_versions[0].content_type, "text/vcard");

        let again = f
            .ingest
            .ingest_item(item(book, "ada", card("Ada")))
            .await
            .unwrap();
        assert!(!again.new_version);

        let summary = f.store.get_item(ada.item_id).await.unwrap().unwrap();
        let index = summary.index.unwrap();
        assert_eq!(index.title.as_deref(), Some("Ada"));
        assert_eq!(index.emails, ["ada@example.com"]);
        assert_eq!(summary.item.key.as_deref(), Some("Ada"));

        let broken = f
            .ingest
            .ingest_item(item(book, "broken", Bytes::from_static(b"garbage")))
            .await
            .unwrap();
        let broken = f.store.get_item(broken.item_id).await.unwrap().unwrap();
        assert!(!broken.index.unwrap().readable, "unreadable, but archived");

        f.store
            .set_pim_index(
                ada.item_id,
                &pim_index(&holtmail_pim::Normalized::Unreadable, 1),
            )
            .await
            .unwrap();
        f.ingest.rebuild_item(ada.item_id).await.unwrap();
        let rebuilt = f.store.get_item(ada.item_id).await.unwrap().unwrap();
        assert_eq!(rebuilt.index.unwrap().title.as_deref(), Some("Ada"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn events_join_the_calendar_and_items_the_index() {
        let f = fixture().await;
        let calendar = |name: &str| holtmail_store::NewCollection {
            archive_id: f.archive,
            account_id: None,
            kind: holtmail_store::CollectionKind::Calendar,
            remote_id: name.into(),
            name: name.into(),
            color: None,
        };
        let work = f
            .store
            .upsert_collection(calendar("work.ics"))
            .await
            .unwrap()
            .id;
        let home = f
            .store
            .upsert_collection(calendar("home.ics"))
            .await
            .unwrap()
            .id;
        let event = |seq: u32| {
            Bytes::from(format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:review@example.com\r\nSEQUENCE:{seq}\r\nSUMMARY:Lamp review\r\nLOCATION:Harbour office\r\nDTSTART;TZID=Europe/Berlin:20250310T090000\r\nDTEND;TZID=Europe/Berlin:20250310T100000\r\nORGANIZER;CN=Ada Quill:mailto:ada@example.com\r\nATTENDEE;CN=Bo Lind;PARTSTAT=ACCEPTED:mailto:bo@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ))
        };
        let mut incoming = item(work, "review", event(1));
        incoming.kind = holtmail_pim::ItemKind::Event;
        incoming.format = holtmail_pim::Format::ICalendar;
        let first = f.ingest.ingest_item(incoming.clone()).await.unwrap();
        incoming.collection = home;
        incoming.raw = event(2);
        let second = f.ingest.ingest_item(incoming).await.unwrap();
        let mut card_item = item(work, "ada", card("Ada"));
        card_item.collection = f
            .store
            .upsert_collection(holtmail_store::NewCollection {
                kind: holtmail_store::CollectionKind::Contacts,
                ..calendar("people.vcf")
            })
            .await
            .unwrap()
            .id;
        let ada = f.ingest.ingest_item(card_item).await.unwrap();
        f.ingest.commit().await.unwrap();

        let series = f.store.series(first.item_id).await.unwrap().unwrap();
        assert_eq!(series.window, None);
        let members = f.store.group_members(series.group_id).await.unwrap();
        assert_eq!(
            members.len(),
            2,
            "the same UID in two calendars is one group"
        );
        assert_eq!(f.store.series(ada.item_id).await.unwrap(), None);
        let found = f
            .store
            .occurrences(&holtmail_store::OccurrenceQuery {
                archive_id: f.archive,
                from: Timestamp::from_millis(1_741_593_600_000),
                to: Timestamp::from_millis(1_741_593_600_001),
                collections: None,
                removed: false,
            })
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            holtmail_calendar::fold(found)[0].leading().item_id,
            second.item_id,
            "the higher SEQUENCE leads"
        );

        let items = |clauses: Vec<Clause>| {
            let index = f.index.clone();
            let archive = f.archive;
            async move {
                let mut ids: Vec<holtmail_core::ItemId> = index
                    .search_items(archive, &SearchQuery { clauses }, SortOrder::Newest, 0, 10)
                    .await
                    .unwrap()
                    .hits
                    .into_iter()
                    .map(|h| h.item_id)
                    .collect();
                ids.sort();
                ids
            }
        };
        let mut both = vec![first.item_id, second.item_id];
        both.sort();
        assert_eq!(items(vec![Clause::Text("harbour".into())]).await, both);
        assert_eq!(items(vec![Clause::With("bo lind".into())]).await, both);
        assert_eq!(
            items(vec![Clause::Kind(holtmail_index::DocumentKind::Contact)]).await,
            [ada.item_id]
        );
        assert!(
            find(&f, vec![Clause::Text("harbour".into())])
                .await
                .is_empty()
        );
    }

    #[test]
    fn events_index_their_start_and_people() {
        let raw = b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:e1@example.com\nSUMMARY:Review\nDTSTART;TZID=Europe/Berlin:20250310T090000\nDTEND;TZID=Europe/Berlin:20250310T100000\nRRULE:FREQ=DAILY\nSEQUENCE:3\nORGANIZER:mailto:Ada@Example.com\nATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.org\nEND:VEVENT\nEND:VCALENDAR\n";
        let normalized = holtmail_pim::normalize(holtmail_pim::ItemKind::Event, raw);
        let index = pim_index(&normalized, 2);
        assert_eq!(index.version, 2);
        assert_eq!(index.title.as_deref(), Some("Review"));
        assert_eq!(
            index.starts_at,
            Some(Timestamp::from_millis(1_741_593_600_000))
        );
        assert_eq!(
            index.ends_at,
            Some(Timestamp::from_millis(1_741_597_200_000))
        );
        assert_eq!(index.time_zone.as_deref(), Some("Europe/Berlin"));
        assert!(index.recurring && !index.all_day);
        assert_eq!(index.sequence, Some(3));
        assert_eq!(index.uid.as_deref(), Some("e1@example.com"));
        assert_eq!(index.organizer.as_deref(), Some("ada@example.com"));
        assert_eq!(index.emails, ["ada@example.com", "bo@example.org"]);
        assert_eq!(index.data["type"], "calendar");
    }
}
