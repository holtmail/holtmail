//! Saving remote images linked from messages into the archive.
//!
//! Newsletters and notifications often show nothing but images hosted by the sender, and those
//! disappear within months. [`RemoteImages`] downloads them once, while archiving, and keeps them
//! as blobs next to the message so it looks the same years later. Every URL is fetched once for
//! the whole instance, and identical bytes behind different URLs are stored once, because blobs
//! are addressed by their hash.

use std::collections::HashSet;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use holtmail_blob::{BlobError, BlobStore};
use holtmail_core::{ArchiveId, MessageId, Timestamp};
use holtmail_fetch::{FetchError, Fetcher};
use holtmail_store::{RemoteCopy, Store, StoreError};

/// Remote images saved for one message at most; the rest are left out.
pub const MAX_IMAGES_PER_MESSAGE: usize = 100;

const FETCHES_AT_ONCE: usize = 6;
const MESSAGES_PER_BATCH: u32 = 50;

/// What [`RemoteImages::save_archive`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Saved {
    /// Messages looked at.
    pub messages: u64,
    /// Images downloaded and stored.
    pub fetched: u64,
    /// Images that were already known, saved or failed, and were not fetched again.
    pub known: u64,
    /// Images that could not be fetched.
    pub failed: u64,
}

/// Why saving stopped. Failures to fetch an image are recorded, not returned.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// Reading or writing metadata failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Reading a message or storing an image failed.
    #[error(transparent)]
    Blob(#[from] BlobError),
}

/// Downloads remote images of archived messages and keeps them with the archive.
#[derive(Clone)]
pub struct RemoteImages {
    fetcher: Arc<dyn Fetcher>,
    blobs: Arc<dyn BlobStore>,
    store: Arc<dyn Store>,
}

impl RemoteImages {
    /// Saves through `fetcher` into these stores.
    pub fn new(
        fetcher: Arc<dyn Fetcher>,
        blobs: Arc<dyn BlobStore>,
        store: Arc<dyn Store>,
    ) -> Self {
        Self {
            fetcher,
            blobs,
            store,
        }
    }

    /// Saves the remote images of every message in `archive` that was not looked at yet, when the
    /// archive has saving turned on; does nothing otherwise.
    ///
    /// A URL fetched or failed before, for any archive, is linked without fetching it again, so
    /// failures are not retried. The image bytes are durable before any message links to them.
    /// Safe to run while messages are being added: later messages are picked up by the next run.
    pub async fn save_archive(&self, archive: ArchiveId) -> Result<Saved, RemoteError> {
        let mut saved = Saved::default();
        let enabled = self
            .store
            .get_archive(archive)
            .await?
            .is_some_and(|archive| archive.save_remote_images);
        if !enabled {
            return Ok(saved);
        }
        loop {
            let batch = self
                .store
                .unscanned_messages(archive, MESSAGES_PER_BATCH)
                .await?;
            if batch.is_empty() {
                return Ok(saved);
            }
            for message in batch {
                self.save_message(message, &mut saved).await?;
            }
        }
    }

    async fn save_message(&self, id: MessageId, saved: &mut Saved) -> Result<(), RemoteError> {
        saved.messages += 1;
        let Some(message) = self.store.get_message(id).await? else {
            return Ok(());
        };
        let urls = match self.blobs.get(&message.blob_id).await? {
            Some(raw) => holtmail_mime::parse(&raw)
                .ok()
                .and_then(|parsed| parsed.html)
                .map(|html| holtmail_sanitize::remote_images(&html))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut seen = HashSet::new();
        let wanted: Vec<(String, String)> = urls
            .into_iter()
            .map(|url| (holtmail_sanitize::remote_key(&url), url))
            .filter(|(key, _)| seen.insert(key.clone()))
            .take(MAX_IMAGES_PER_MESSAGE)
            .collect();

        let outcomes: Vec<Result<Outcome, RemoteError>> = stream::iter(wanted.iter().cloned())
            .map(|(key, url)| self.save_url(key, url))
            .buffer_unordered(FETCHES_AT_ONCE)
            .collect()
            .await;
        for outcome in outcomes {
            match outcome? {
                Outcome::Known => saved.known += 1,
                Outcome::Fetched => saved.fetched += 1,
                Outcome::Failed => saved.failed += 1,
            }
        }
        let keys: Vec<String> = wanted.into_iter().map(|(key, _)| key).collect();
        self.store.record_remote_scan(id, &keys).await?;
        Ok(())
    }

    async fn save_url(&self, key: String, url: String) -> Result<Outcome, RemoteError> {
        if self.store.remote_copy(&key).await?.is_some() {
            return Ok(Outcome::Known);
        }
        let mut copy = RemoteCopy {
            key,
            url,
            blob_id: None,
            content_type: None,
            size: 0,
            fetched_at: Timestamp::now(),
            error: None,
        };
        let outcome = match self.fetcher.image(&copy.url).await {
            Ok(image) => {
                copy.size = image.bytes.len() as u64;
                copy.blob_id = Some(self.blobs.put(image.bytes).await?);
                copy.content_type = Some(image.content_type);
                Outcome::Fetched
            }
            Err(FetchError::Refused(reason) | FetchError::Unavailable(reason)) => {
                copy.error = Some(reason);
                Outcome::Failed
            }
        };
        self.store.save_remote_copy(&copy).await?;
        Ok(outcome)
    }
}

enum Outcome {
    Known,
    Fetched,
    Failed,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use bytes::Bytes;
    use holtmail_blob_fs::FsBlobStore;
    use holtmail_fetch::RemoteImage;
    use holtmail_index::SearchIndex;
    use holtmail_index_tantivy::TantivyIndex;
    use holtmail_ingest::{Ingest, Placement};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    #[derive(Default)]
    struct FakeFetcher {
        asked: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Fetcher for FakeFetcher {
        async fn image(&self, url: &str) -> Result<RemoteImage, FetchError> {
            self.asked.lock().unwrap().push(url.to_owned());
            if url.contains("gone") {
                return Err(FetchError::Unavailable("HTTP 404".into()));
            }
            let bytes = if url.contains("logo") {
                "the logo"
            } else {
                url
            };
            Ok(RemoteImage {
                content_type: "image/png".into(),
                bytes: Bytes::copy_from_slice(bytes.as_bytes()),
            })
        }
    }

    fn newsletter(n: u32, body: &str) -> Bytes {
        Bytes::from(format!(
            "From: News <news@example.com>\r\nTo: ada@example.org\r\nSubject: Issue {n}\r\n\
             Message-ID: <issue{n}@example.com>\r\nDate: Mon, 10 Mar 2025 09:00:00 +0000\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{body}\r\n"
        ))
    }

    #[tokio::test]
    async fn saves_each_image_once_and_links_every_message() {
        let dir = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> =
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
        let ingest = Ingest::new(blobs.clone(), store.clone(), index);
        let archive = store.create_archive("News").await.unwrap().id;
        let fetcher = Arc::new(FakeFetcher::default());
        let remote = RemoteImages::new(fetcher.clone(), blobs.clone(), store.clone());

        let first = ingest
            .ingest(
                archive,
                newsletter(
                    1,
                    r#"<img src="https://example.com/logo.png"><img src="https://example.com/a.png"><img src="https://example.com/gone.png">"#,
                ),
                &Placement::default(),
            )
            .await
            .unwrap()
            .message_id;
        assert_eq!(
            remote.save_archive(archive).await.unwrap(),
            Saved::default(),
            "saved without being turned on"
        );

        store.set_save_remote_images(archive, true).await.unwrap();
        let report = remote.save_archive(archive).await.unwrap();
        assert_eq!(
            report,
            Saved {
                messages: 1,
                fetched: 2,
                known: 0,
                failed: 1
            }
        );

        let second = ingest
            .ingest(
                archive,
                newsletter(
                    2,
                    r#"<img src="https://example.com/logo.png"><img src="https://cdn.example.org/logo-copy.png"><img src="https://example.com/gone.png">"#,
                ),
                &Placement::default(),
            )
            .await
            .unwrap()
            .message_id;
        let report = remote.save_archive(archive).await.unwrap();
        assert_eq!(
            report,
            Saved {
                messages: 1,
                fetched: 1,
                known: 2,
                failed: 0
            }
        );
        assert_eq!(
            remote.save_archive(archive).await.unwrap(),
            Saved::default()
        );
        assert_eq!(
            fetcher.asked.lock().unwrap().len(),
            4,
            "a known URL was fetched again"
        );

        let copies = store.message_remote_copies(second).await.unwrap();
        assert_eq!(copies.len(), 2);
        assert_eq!(
            copies[0].blob_id, copies[1].blob_id,
            "identical bytes behind two URLs were stored twice"
        );
        let logo = &copies[0];
        assert_eq!(
            blobs.get(logo.blob_id.as_ref().unwrap()).await.unwrap(),
            Some(Bytes::from_static(b"the logo"))
        );
        assert_eq!(store.message_remote_copies(first).await.unwrap().len(), 2);
    }
}
