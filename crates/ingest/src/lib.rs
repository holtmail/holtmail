//! From raw message bytes to a stored, indexed message.
//!
//! Sync and import both hand every message to [`Ingest::ingest`], so there is one path into the
//! archive and one set of rules: bytes first, then metadata, then the index.

use std::sync::Arc;

use bytes::Bytes;
use holtmail_blob::{BlobError, BlobStore};
use holtmail_core::{ArchiveId, FolderId, MessageId, ThreadId, Timestamp};
use holtmail_extract::{ExtractError, Extractor, File};
use holtmail_index::{IndexDocument, IndexError, SearchIndex};
use holtmail_mime::{MimeError, ParsedMessage};
use holtmail_store::{Attachment, Derived, DerivedKind, NewMessage, Store, StoreError};

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
    /// Idempotent: the same bytes again only update where the message was seen. The raw bytes are
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
}
