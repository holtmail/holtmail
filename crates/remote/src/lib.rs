//! Saving remote images linked from messages into the archive.
//!
//! Newsletters and notifications often show nothing but images hosted by the sender, and those
//! disappear within months. [`RemoteImages`] downloads them once, while archiving, and keeps them
//! as blobs next to the message so it looks the same years later. Every URL is fetched once for
//! the whole instance, and identical bytes behind different URLs are stored once, because blobs
//! are addressed by their hash.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::stream::{self, FuturesUnordered, StreamExt};
use holtmail_blob::{BlobError, BlobStore};
use holtmail_core::{ArchiveId, MessageId, Timestamp};
use holtmail_fetch::{FetchError, Fetcher};
use holtmail_store::{ArchivedWhen, RemoteCopy, Store, StoreError};
use tokio::sync::Semaphore;

/// Remote images saved for one message at most; the rest are left out.
pub const MAX_IMAGES_PER_MESSAGE: usize = 100;

const FETCHES_AT_ONCE: usize = 6;
/// Messages looked at together, so one slow sender does not hold up the others.
pub const MESSAGES_AT_ONCE: usize = 4;
const MESSAGES_PER_PAGE: u32 = 50;

/// Which messages [`RemoteImages::save_archive`] looks at, relative to when saving was turned on
/// for the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Messages archived since saving was turned on: what runs after every sync and import.
    New,
    /// Messages archived before saving was turned on: run only on request, since an archive can
    /// hold years of mail.
    Older,
}

/// What [`RemoteImages::save_archive`] did so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Saved {
    /// Messages to look at when the run started; more may arrive during a `New` run.
    pub total: u64,
    /// Messages looked at.
    pub messages: u64,
    /// Images downloaded and stored.
    pub fetched: u64,
    /// Images that were already known, saved or failed, and were not fetched again.
    pub known: u64,
    /// Images that could not be fetched.
    pub failed: u64,
    /// The run stopped early because it was cancelled.
    pub cancelled: bool,
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

impl Saved {
    fn add(&mut self, other: Saved) {
        self.messages += other.messages;
        self.fetched += other.fetched;
        self.known += other.known;
        self.failed += other.failed;
    }
}

/// What one run shares between the messages it looks at together.
struct Run {
    fetches: Semaphore,
    urls: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Run {
    fn new() -> Self {
        Self {
            fetches: Semaphore::new(FETCHES_AT_ONCE),
            urls: Mutex::default(),
        }
    }

    fn url(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.urls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key.to_owned())
            .or_default()
            .clone()
    }
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

    /// Saves the remote images of the messages in `scope` that were not looked at yet, when the
    /// archive has saving turned on; does nothing otherwise.
    ///
    /// Up to [`MESSAGES_AT_ONCE`] messages are looked at together, with at most six downloads
    /// at a time across them, so a slow sender does not hold up the others; a URL two of them
    /// link is still fetched once. `progress` is called after every message. The run checks
    /// `cancel` before starting each message and stops with `cancelled` set once the messages
    /// already started are done; messages done so far stay done. A URL fetched or failed before,
    /// for any archive, is linked without fetching it again, so failures are not retried. The image
    /// bytes are durable before any message links to them. Safe to run while messages are being
    /// added.
    pub async fn save_archive(
        &self,
        archive: ArchiveId,
        scope: Scope,
        cancel: &AtomicBool,
        progress: impl Fn(&Saved) + Send + Sync,
    ) -> Result<Saved, RemoteError> {
        let mut saved = Saved::default();
        let Some(since) = self
            .store
            .get_archive(archive)
            .await?
            .and_then(|archive| archive.save_remote_images_since)
        else {
            return Ok(saved);
        };
        let when = match scope {
            Scope::New => ArchivedWhen::Since(since),
            Scope::Older => ArchivedWhen::Before(since),
        };
        saved.total = self.store.count_unscanned(archive, when).await?;
        progress(&saved);
        let run = Run::new();
        let mut working = FuturesUnordered::new();
        let mut queue: VecDeque<MessageId> = VecDeque::new();
        let mut busy: HashSet<MessageId> = HashSet::new();
        let mut exhausted = false;
        loop {
            while working.len() < MESSAGES_AT_ONCE && !cancel.load(Ordering::Relaxed) {
                if queue.is_empty() {
                    if exhausted && !working.is_empty() {
                        break;
                    }
                    queue = self
                        .store
                        .unscanned_messages(archive, when, MESSAGES_PER_PAGE)
                        .await?
                        .into_iter()
                        .filter(|id| !busy.contains(id))
                        .collect();
                    exhausted = queue.is_empty();
                    if exhausted {
                        break;
                    }
                }
                let Some(id) = queue.pop_front() else { break };
                busy.insert(id);
                let run = &run;
                working.push(async move { (id, self.save_message(id, run).await) });
            }
            let Some((id, done)) = working.next().await else {
                saved.cancelled = cancel.load(Ordering::Relaxed);
                return Ok(saved);
            };
            busy.remove(&id);
            saved.add(done?);
            saved.total = saved.total.max(saved.messages);
            progress(&saved);
        }
    }

    /// Fetches again every image of `archive` that could not be saved before, once each, and
    /// records the new result. Progress and cancelling work as in
    /// [`RemoteImages::save_archive`]; `messages` counts images here.
    pub async fn retry_failed(
        &self,
        archive: ArchiveId,
        cancel: &AtomicBool,
        progress: impl Fn(&Saved) + Send + Sync,
    ) -> Result<Saved, RemoteError> {
        let started = Timestamp::now();
        let mut saved = Saved {
            total: self.store.remote_copy_totals(archive).await?.failed,
            ..Saved::default()
        };
        progress(&saved);
        loop {
            let batch = self
                .store
                .failed_remote_copies(archive, started, MESSAGES_PER_PAGE)
                .await?;
            if batch.is_empty() {
                return Ok(saved);
            }
            for copy in batch {
                if cancel.load(Ordering::Relaxed) {
                    saved.cancelled = true;
                    return Ok(saved);
                }
                match self.fetch(copy.key, copy.url).await? {
                    Outcome::Fetched => saved.fetched += 1,
                    _ => saved.failed += 1,
                }
                saved.messages += 1;
                saved.total = saved.total.max(saved.messages);
                progress(&saved);
            }
        }
    }

    async fn save_message(&self, id: MessageId, run: &Run) -> Result<Saved, RemoteError> {
        let mut saved = Saved {
            messages: 1,
            ..Saved::default()
        };
        let Some(message) = self.store.get_message(id).await? else {
            return Ok(saved);
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
            .map(|(key, url)| self.save_url(key, url, run))
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
        Ok(saved)
    }

    async fn save_url(&self, key: String, url: String, run: &Run) -> Result<Outcome, RemoteError> {
        let same_url = run.url(&key);
        let _only_one = same_url.lock().await;
        if self.store.remote_copy(&key).await?.is_some() {
            return Ok(Outcome::Known);
        }
        let _permit = run.fetches.acquire().await.ok();
        self.fetch(key, url).await
    }

    async fn fetch(&self, key: String, url: String) -> Result<Outcome, RemoteError> {
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
        back: AtomicBool,
    }

    #[async_trait]
    impl Fetcher for FakeFetcher {
        async fn image(&self, url: &str) -> Result<RemoteImage, FetchError> {
            self.asked.lock().unwrap().push(url.to_owned());
            if url.contains("gone") && !self.back.load(std::sync::atomic::Ordering::Relaxed) {
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

    async fn run(remote: &RemoteImages, archive: ArchiveId, scope: Scope) -> Saved {
        remote
            .save_archive(archive, scope, &AtomicBool::new(false), |_| {})
            .await
            .unwrap()
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
        let mail = |n: u32, body: &'static str| {
            let ingest = ingest.clone();
            async move {
                ingest
                    .ingest(archive, newsletter(n, body), &Placement::default())
                    .await
                    .unwrap()
                    .message_id
            }
        };

        let old = mail(
            1,
            r#"<img src="https://example.com/logo.png"><img src="https://example.com/old.png">"#,
        )
        .await;
        assert_eq!(
            run(&remote, archive, Scope::Older).await,
            Saved::default(),
            "saved without being turned on"
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        store.set_save_remote_images(archive, true).await.unwrap();
        let first = mail(
            2,
            r#"<img src="https://example.com/logo.png"><img src="https://example.com/a.png"><img src="https://example.com/gone.png">"#,
        )
        .await;
        assert_eq!(
            run(&remote, archive, Scope::New).await,
            Saved {
                total: 1,
                messages: 1,
                fetched: 2,
                known: 0,
                failed: 1,
                cancelled: false,
            },
            "new mail was not saved, or older mail was"
        );

        let second = mail(
            3,
            r#"<img src="https://example.com/logo.png"><img src="https://cdn.example.org/logo-copy.png"><img src="https://example.com/gone.png">"#,
        )
        .await;
        let report = run(&remote, archive, Scope::New).await;
        assert_eq!((report.fetched, report.known), (1, 2));
        assert_eq!(run(&remote, archive, Scope::New).await.messages, 0);
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
        assert_eq!(
            blobs
                .get(copies[0].blob_id.as_ref().unwrap())
                .await
                .unwrap(),
            Some(Bytes::from_static(b"the logo"))
        );
        assert_eq!(store.message_remote_copies(first).await.unwrap().len(), 2);
        assert!(store.message_remote_copies(old).await.unwrap().is_empty());

        let cancel = AtomicBool::new(true);
        let stopped = remote
            .save_archive(archive, Scope::Older, &cancel, |_| {})
            .await
            .unwrap();
        assert!(stopped.cancelled && stopped.messages == 0);

        let seen = std::sync::Mutex::new(Vec::new());
        let older = remote
            .save_archive(archive, Scope::Older, &AtomicBool::new(false), |p| {
                seen.lock().unwrap().push((p.messages, p.total));
            })
            .await
            .unwrap();
        assert_eq!((older.messages, older.fetched, older.known), (1, 1, 1));
        assert_eq!(*seen.lock().unwrap(), [(0, 1), (1, 1)]);
        assert_eq!(store.message_remote_copies(old).await.unwrap().len(), 2);

        fetcher
            .back
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let retried = remote
            .retry_failed(archive, &AtomicBool::new(false), |_| {})
            .await
            .unwrap();
        assert_eq!((retried.total, retried.fetched, retried.failed), (1, 1, 0));
        assert_eq!(store.message_remote_copies(first).await.unwrap().len(), 3);
        assert_eq!(
            store.remote_copy_totals(archive).await.unwrap().failed,
            0,
            "a retried image still counts as failed"
        );
    }

    #[derive(Default)]
    struct GatedFetcher {
        asked: Mutex<Vec<String>>,
        in_flight: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
        open: tokio::sync::Notify,
        released: AtomicBool,
    }

    #[async_trait]
    impl Fetcher for GatedFetcher {
        async fn image(&self, url: &str) -> Result<RemoteImage, FetchError> {
            self.asked.lock().unwrap().push(url.to_owned());
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            if url.contains("slow") {
                while !self.released.load(Ordering::SeqCst) {
                    self.open.notified().await;
                }
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(RemoteImage {
                content_type: "image/png".into(),
                bytes: Bytes::copy_from_slice(url.as_bytes()),
            })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_sender_does_not_hold_up_other_messages() {
        let dir = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> =
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
        let ingest = Ingest::new(blobs.clone(), store.clone(), index);
        let archive = store.create_archive("News").await.unwrap().id;
        store.set_save_remote_images(archive, true).await.unwrap();
        let fetcher = Arc::new(GatedFetcher::default());
        let remote = RemoteImages::new(fetcher.clone(), blobs.clone(), store.clone());
        let mut bodies = vec![r#"<img src="https://slow.example.com/a.png">"#.to_owned()];
        for n in 1..6 {
            let many: String = (0..20)
                .map(|i| format!(r#"<img src="https://example.com/{n}/{i}.png">"#))
                .collect();
            bodies.push(format!(
                r#"{many}<img src="https://example.com/shared.png">"#
            ));
        }
        for (n, body) in bodies.iter().enumerate() {
            ingest
                .ingest(archive, newsletter(n as u32, body), &Placement::default())
                .await
                .unwrap();
        }

        let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let running = {
            let remote = remote.clone();
            let done = done.clone();
            tokio::spawn(async move {
                remote
                    .save_archive(archive, Scope::New, &AtomicBool::new(false), |p| {
                        done.store(p.messages, Ordering::SeqCst);
                    })
                    .await
                    .unwrap()
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while done.load(Ordering::SeqCst) < 5 {
            assert!(
                std::time::Instant::now() < deadline,
                "the other messages waited for the slow one"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!running.is_finished());
        fetcher.released.store(true, Ordering::SeqCst);
        fetcher.open.notify_waiters();
        let saved = running.await.unwrap();
        assert_eq!((saved.messages, saved.fetched), (6, 102));
        assert_eq!(saved.known, 4, "{saved:?}");
        let asked = fetcher.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 102, "a URL was fetched twice");
        assert_eq!(
            asked.iter().filter(|u| u.ends_with("shared.png")).count(),
            1
        );
        assert!(
            fetcher.most.load(Ordering::SeqCst) <= FETCHES_AT_ONCE,
            "{} downloads at once",
            fetcher.most.load(Ordering::SeqCst)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_sender_does_not_hold_up_the_next_page() {
        let dir = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> =
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
        let ingest = Ingest::new(blobs.clone(), store.clone(), index);
        let archive = store.create_archive("News").await.unwrap().id;
        store.set_save_remote_images(archive, true).await.unwrap();
        let fetcher = Arc::new(GatedFetcher::default());
        let remote = RemoteImages::new(fetcher.clone(), blobs.clone(), store.clone());
        let total = MESSAGES_PER_PAGE + 20;
        for n in 0..total {
            let body = if n == 0 {
                r#"<img src="https://slow.example.com/a.png">"#.to_owned()
            } else {
                format!(r#"<img src="https://example.com/{n}.png">"#)
            };
            ingest
                .ingest(archive, newsletter(n, &body), &Placement::default())
                .await
                .unwrap();
        }

        let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let running = {
            let remote = remote.clone();
            let done = done.clone();
            tokio::spawn(async move {
                remote
                    .save_archive(archive, Scope::New, &AtomicBool::new(false), |p| {
                        done.store(p.messages, Ordering::SeqCst);
                    })
                    .await
                    .unwrap()
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while done.load(Ordering::SeqCst) < u64::from(total - 1) {
            assert!(
                std::time::Instant::now() < deadline,
                "later pages waited for the slow message: {} done",
                done.load(Ordering::SeqCst)
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!running.is_finished());
        fetcher.released.store(true, Ordering::SeqCst);
        fetcher.open.notify_waiters();
        let saved = running.await.unwrap();
        assert_eq!(
            (saved.messages, saved.fetched),
            (u64::from(total), u64::from(total))
        );
    }
}
