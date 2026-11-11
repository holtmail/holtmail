use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64};

use holtmail_blob::{BlobError, BlobId};
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{ItemId, Timestamp, UserId};
use holtmail_index::{
    Clause, IndexDocument, IndexError, ItemDocument, ItemResults, SearchIndex, SearchQuery,
    SearchResults, SortOrder,
};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::Placement;
use holtmail_store::{
    ArchiveStore, CategoryOverride, CategoryOverrideId, CategoryStore, DeletionStore,
    InsightOutcome, InsightStore, MessageStore, OverrideScope,
};
use holtmail_store_sqlite::SqliteStore;

use super::*;
use crate::insights::InsightsConfig;

type Hook = Box<dyn Fn(u64) + Send + Sync>;

struct Counting {
    inner: Arc<dyn BlobStore>,
    reads: Mutex<Vec<BlobId>>,
    count: AtomicU64,
    on_read: Mutex<Option<Hook>>,
}

#[async_trait]
impl BlobStore for Counting {
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError> {
        self.reads.lock().unwrap().push(*id);
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(hook) = &*self.on_read.lock().unwrap() {
            hook(n);
        }
        self.inner.get(id).await
    }

    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError> {
        self.inner.contains(id).await
    }

    async fn delete(&self, id: &BlobId) -> Result<(), BlobError> {
        self.inner.delete(id).await
    }
}

/// A second step that wants every message once, to show the walk reads each blob once for both.
#[derive(Default)]
struct Wanting {
    pending: Mutex<BTreeSet<MessageId>>,
    applied: Mutex<Vec<MessageId>>,
}

#[async_trait]
impl Step for Wanting {
    fn name(&self) -> &'static str {
        "wanting"
    }

    async fn pending(
        &self,
        _store: &dyn Store,
        _archive: ArchiveId,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError> {
        Ok(self
            .pending
            .lock()
            .unwrap()
            .iter()
            .filter(|id| after.is_none_or(|a| **id > a))
            .take(limit as usize)
            .copied()
            .collect())
    }

    async fn apply(&self, item: &WalkItem) -> anyhow::Result<StepOutcome> {
        self.pending.lock().unwrap().remove(&item.message);
        self.applied.lock().unwrap().push(item.message);
        Ok(StepOutcome::Written)
    }

    async fn unusable(&self, message: MessageId, _why: Unusable) -> anyhow::Result<()> {
        self.pending.lock().unwrap().remove(&message);
        Ok(())
    }
}

/// The index, recording how many messages were still queued for reindexing at each commit, and
/// failing the next commit on request.
struct Watching {
    inner: Arc<TantivyIndex>,
    store: Arc<SqliteStore>,
    archive: ArchiveId,
    queued_at_commit: Mutex<Vec<u64>>,
    fail_commit: AtomicBool,
}

#[async_trait]
impl SearchIndex for Watching {
    async fn put(&self, documents: Vec<IndexDocument>) -> Result<(), IndexError> {
        self.inner.put(documents).await
    }

    async fn remove(&self, ids: &[MessageId]) -> Result<(), IndexError> {
        self.inner.remove(ids).await
    }

    async fn put_items(&self, documents: Vec<ItemDocument>) -> Result<(), IndexError> {
        self.inner.put_items(documents).await
    }

    async fn remove_items(&self, ids: &[ItemId]) -> Result<(), IndexError> {
        self.inner.remove_items(ids).await
    }

    async fn commit(&self) -> Result<(), IndexError> {
        if self.fail_commit.swap(false, Ordering::SeqCst) {
            return Err(IndexError::Backend("commit refused".into()));
        }
        let queued = self.store.count_reindex_queue(self.archive).await.unwrap();
        self.queued_at_commit.lock().unwrap().push(queued);
        self.inner.commit().await
    }

    async fn search(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        order: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<SearchResults, IndexError> {
        self.inner
            .search(archive, query, order, offset, limit)
            .await
    }

    async fn matching(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        cap: u32,
    ) -> Result<holtmail_index::Matches, IndexError> {
        self.inner.matching(archive, query, cap).await
    }

    async fn search_items(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        order: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<ItemResults, IndexError> {
        self.inner
            .search_items(archive, query, order, offset, limit)
            .await
    }
}

struct Fixture {
    store: Arc<SqliteStore>,
    fs: Arc<dyn BlobStore>,
    blobs: Arc<Counting>,
    index: Arc<Watching>,
    ingest: Ingest,
    fingerprint: String,
    tasks: Arc<Tasks>,
    archive: ArchiveId,
    ids: Vec<MessageId>,
    _dir: tempfile::TempDir,
}

fn mail(n: usize) -> Bytes {
    Bytes::from(format!(
        "From: Shop <no-reply@shop.example.com>\r\nTo: ada@example.org\r\nSubject: Your code {n}\r\n\
         Message-ID: <walk{n}@shop.example.com>\r\nDate: Mon, 10 Mar 2025 09:00:00 +0000\r\n\
         List-Unsubscribe: <https://shop.example.com/u/{n}>\r\n\r\n\
         Your verification code is 4829{n:02}.\r\n"
    ))
}

async fn fixture(messages: usize) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let fs: Arc<dyn BlobStore> =
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
    let index = Arc::new(TantivyIndex::open(dir.path().join("index")).unwrap());
    let archive = store.create_archive("Mail").await.unwrap().id;
    let plain = Ingest::new(fs.clone(), store.clone(), index.clone());
    let mut ids = Vec::new();
    for n in 0..messages {
        ids.push(
            plain
                .ingest(archive, mail(n), &Placement::default())
                .await
                .unwrap()
                .message_id,
        );
    }
    plain.commit().await.unwrap();
    let blobs = Arc::new(Counting {
        inner: fs.clone(),
        reads: Mutex::new(Vec::new()),
        count: AtomicU64::new(0),
        on_read: Mutex::new(None),
    });
    let runner = Arc::new(InsightsConfig::default().runner());
    let fingerprint = runner.extract_fp();
    let index = Arc::new(Watching {
        inner: index,
        store: store.clone(),
        archive,
        queued_at_commit: Mutex::new(Vec::new()),
        fail_commit: AtomicBool::new(false),
    });
    let ingest = Ingest::new(blobs.clone(), store.clone(), index.clone()).with_insights(runner);
    Fixture {
        store,
        fs,
        blobs,
        index,
        ingest,
        fingerprint,
        tasks: Arc::new(Tasks::default()),
        archive,
        ids,
        _dir: dir,
    }
}

impl Fixture {
    fn walk(&self, extra: Option<Arc<dyn Step>>) -> (Arc<Walk>, Arc<InsightProgress>) {
        let mut steps: Vec<Arc<dyn Step>> = vec![Arc::new(InsightsStep::new(
            self.ingest.clone(),
            self.fingerprint.clone(),
            self.store.clone(),
        ))];
        steps.extend(extra);
        let progress = Arc::new(InsightProgress::default());
        let mut walk = Walk::new(
            self.store.clone(),
            self.blobs.clone(),
            self.ingest.clone(),
            steps,
            self.tasks.clone(),
        )
        .with_progress(progress.clone());
        walk.page = 3;
        walk.pause = Duration::ZERO;
        (Arc::new(walk), progress)
    }

    async fn pending(&self) -> u64 {
        self.store
            .count_needing_insights(self.archive, &self.fingerprint)
            .await
            .unwrap()
    }

    fn index_step(&self) -> Arc<dyn Step> {
        Arc::new(IndexStep::new(self.ingest.clone(), self.store.clone()))
    }

    async fn queued(&self) -> Vec<(MessageId, u64)> {
        self.store
            .reindex_queue(self.archive, None, 100)
            .await
            .unwrap()
    }

    fn reads(&self) -> usize {
        self.blobs.reads.lock().unwrap().len()
    }

    fn analyse_tasks(&self) -> Vec<holtmail_api::Task> {
        self.tasks
            .list()
            .into_iter()
            .filter(|t| t.kind == TaskKind::Analyse)
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fills_rows_reading_each_blob_once() {
    let f = fixture(8).await;
    assert_eq!(f.pending().await, 8);
    let wanting = Arc::new(Wanting::default());
    wanting
        .pending
        .lock()
        .unwrap()
        .extend(f.ids.iter().copied());
    let (walk, progress) = f.walk(Some(wanting.clone()));

    let ran = walk.run().await.unwrap();
    assert_eq!((ran.messages, ran.written, ran.failed), (8, 8, 0));
    assert!(!ran.cancelled);
    assert_eq!(f.pending().await, 0);
    let mut reads = f.blobs.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 8, "a blob was read more than once");
    reads.sort();
    reads.dedup();
    assert_eq!(reads.len(), 8);
    let mut applied = wanting.applied.lock().unwrap().clone();
    applied.sort();
    assert_eq!(applied, f.ids, "the second step missed messages");

    let view = f.store.insight_view(f.ids[0]).await.unwrap().unwrap();
    assert_eq!(view.insights.fingerprint, f.fingerprint);
    assert_eq!(view.insights.outcome, InsightOutcome::Ok);
    assert_ne!(view.insights.kinds, 0, "no code or unsubscribe found");

    let [task] = f.analyse_tasks().try_into().unwrap();
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.done, 8);
    assert_eq!(task.archive_id, None);
    assert!(!progress.running());
    assert_eq!(progress.done(), 8);

    let again = walk.run().await.unwrap();
    assert_eq!(again, Ran::default());
    assert_eq!(f.blobs.reads.lock().unwrap().len(), 8);
    assert_eq!(
        f.analyse_tasks().len(),
        1,
        "a run with nothing to do started a task"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_after_cancellation_without_redoing_messages() {
    let f = fixture(7).await;
    let (walk, _) = f.walk(None);
    let tasks = f.tasks.clone();
    *f.blobs.on_read.lock().unwrap() = Some(Box::new(move |n| {
        if n == 3
            && let Some(task) = tasks.running(TaskKind::Analyse, None)
        {
            tasks.cancel(&task.id).unwrap();
        }
    }));

    let first = walk.run().await.unwrap();
    assert!(first.cancelled);
    assert!(first.messages >= 3 && first.messages < 7, "{first:?}");
    assert_eq!(f.pending().await, 7 - first.messages);
    let tasks = f.analyse_tasks();
    assert_eq!(tasks[0].state, TaskState::Cancelled);

    *f.blobs.on_read.lock().unwrap() = None;
    let second = walk.run().await.unwrap();
    assert!(!second.cancelled);
    assert_eq!(first.messages + second.messages, 7);
    assert_eq!(f.pending().await, 0);
    let mut reads = f.blobs.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 7, "finished messages were read again");
    reads.sort();
    reads.dedup();
    assert_eq!(reads.len(), 7);
    let states: Vec<_> = f.analyse_tasks().iter().map(|t| t.state).collect();
    assert_eq!(states, [TaskState::Done, TaskState::Cancelled]);
}

#[tokio::test(flavor = "multi_thread")]
async fn marks_unreadable_messages_and_skips_deleted_ones() {
    let f = fixture(4).await;
    let unreadable = f.store.get_message(f.ids[1]).await.unwrap().unwrap();
    f.fs.delete(&unreadable.blob_id).await.unwrap();
    f.store
        .delete_messages(f.archive, &[f.ids[2]], None)
        .await
        .unwrap();
    let (walk, _) = f.walk(None);

    let ran = walk.run().await.unwrap();
    assert_eq!((ran.messages, ran.unreadable, ran.failed), (3, 1, 0));
    let view = f.store.insight_view(f.ids[1]).await.unwrap().unwrap();
    assert_eq!(view.insights.outcome, InsightOutcome::Unreadable);
    assert_eq!(view.insights.fingerprint, f.fingerprint);
    assert_eq!(view.insights.kinds, 0);
    assert!(view.insights.items.contains("\"items\":[]"));
    assert_eq!(f.pending().await, 0);

    let again = walk.run().await.unwrap();
    assert_eq!(again.messages, 0, "the unreadable message was listed again");
    assert_eq!(f.blobs.reads.lock().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_index_step_drains_the_queue_after_the_commit_reading_each_blob_once() {
    let f = fixture(5).await;
    f.store.queue_reindex(&f.ids[..3]).await.unwrap();
    let (walk, _) = f.walk(Some(f.index_step()));

    let ran = walk.run().await.unwrap();
    assert_eq!((ran.messages, ran.failed), (5, 0));
    assert_eq!(f.reads(), 5, "a blob was read twice for two steps");
    assert!(f.queued().await.is_empty());
    let seen = f.index.queued_at_commit.lock().unwrap().clone();
    assert_eq!(seen.first(), Some(&3), "dequeued before the index commit");
    assert_eq!(f.pending().await, 0);

    f.store.queue_reindex(&f.ids[4..]).await.unwrap();
    let again = walk.run().await.unwrap();
    assert_eq!((again.messages, again.written), (1, 1));
    assert_eq!(f.reads(), 6);
    assert!(f.queued().await.is_empty());
    let found = f
        .index
        .search(
            f.archive,
            &SearchQuery {
                clauses: vec![Clause::Insight("code".into())],
            },
            SortOrder::Newest,
            0,
            10,
        )
        .await
        .unwrap();
    assert_eq!(found.total, 5);
    assert_eq!(walk.run().await.unwrap(), Ran::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_requeue_before_the_commit_stays_queued() {
    let f = fixture(4).await;
    let (walk, _) = f.walk(None);
    walk.run().await.unwrap();
    f.store.queue_reindex(&f.ids).await.unwrap();
    let (walk, _) = f.walk(Some(f.index_step()));
    let store = f.store.clone();
    let ids = f.ids.clone();
    let before = f.reads() as u64;
    *f.blobs.on_read.lock().unwrap() = Some(Box::new(move |n| {
        if n == before + 1 {
            let (store, ids) = (store.clone(), ids.clone());
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.queue_reindex(&ids).await.unwrap() })
            });
        }
    }));

    let ran = walk.run().await.unwrap();
    assert_eq!(ran.messages, 4);
    let queued = f.queued().await;
    assert_eq!(
        queued.len(),
        3,
        "the first page was re-queued after it was listed and must stay; the second page \
         listed the new sequence"
    );
    assert!(queued.iter().all(|(_, seq)| *seq == 2));

    *f.blobs.on_read.lock().unwrap() = None;
    let again = walk.run().await.unwrap();
    assert_eq!(again.messages, 3);
    assert!(f.queued().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_commit_leaves_the_queue_and_the_next_run_resumes() {
    let f = fixture(7).await;
    let (walk, _) = f.walk(None);
    walk.run().await.unwrap();
    f.store.queue_reindex(&f.ids).await.unwrap();
    let (walk, _) = f.walk(Some(f.index_step()));

    f.index.fail_commit.store(true, Ordering::SeqCst);
    assert!(walk.run().await.is_err());
    assert_eq!(f.queued().await.len(), 7, "dequeued without a commit");

    let tasks = f.tasks.clone();
    let before = f.reads() as u64;
    *f.blobs.on_read.lock().unwrap() = Some(Box::new(move |n| {
        if n == before + 3
            && let Some(task) = tasks.running(TaskKind::Analyse, None)
        {
            tasks.cancel(&task.id).unwrap();
        }
    }));
    let first = walk.run().await.unwrap();
    assert!(first.cancelled);
    assert_eq!(f.queued().await.len() as u64, 7 - first.messages);

    *f.blobs.on_read.lock().unwrap() = None;
    let second = walk.run().await.unwrap();
    assert_eq!(first.messages + second.messages, 7);
    assert!(f.queued().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn written_insights_stay_queued_until_the_commit() {
    let f = fixture(4).await;
    let (walk, _) = f.walk(Some(f.index_step()));

    f.index.fail_commit.store(true, Ordering::SeqCst);
    assert!(walk.run().await.is_err());
    assert_eq!(f.pending().await, 1, "the first page has its rows");
    let queued: Vec<MessageId> = f.queued().await.into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        queued,
        f.ids[..3],
        "written but not committed, so still queued"
    );

    let ran = walk.run().await.unwrap();
    assert_eq!(ran.messages, 4);
    assert!(f.queued().await.is_empty());
    let found = f
        .index
        .search(
            f.archive,
            &SearchQuery {
                clauses: vec![Clause::Insight("code".into())],
            },
            SortOrder::Newest,
            0,
            10,
        )
        .await
        .unwrap();
    assert_eq!(found.total, 4);
    assert_eq!(walk.run().await.unwrap(), Ran::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_queued_message_leaves_the_queue() {
    let f = fixture(3).await;
    let (walk, _) = f.walk(None);
    walk.run().await.unwrap();
    let lost = f.store.get_message(f.ids[1]).await.unwrap().unwrap();
    f.fs.delete(&lost.blob_id).await.unwrap();
    f.store.queue_reindex(&f.ids[1..2]).await.unwrap();
    let (walk, _) = f.walk(Some(f.index_step()));

    let ran = walk.run().await.unwrap();
    assert_eq!((ran.messages, ran.unreadable, ran.failed), (1, 1, 0));
    assert!(f.queued().await.is_empty());
    assert_eq!(walk.run().await.unwrap().messages, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn category_search_follows_an_override_after_one_walk_run() {
    let dir = tempfile::tempdir().unwrap();
    let config: crate::config::Config =
        toml::from_str(&format!("data_dir = {:?}", dir.path())).unwrap();
    let services = Arc::new(crate::config::Services::open(&config).await.unwrap());
    let schedule = Arc::new(
        crate::schedule::Schedule::open(None, services.store.clone())
            .await
            .unwrap(),
    );
    let runner =
        crate::mailboxes::Runner::open(services.clone(), dir.path().join("uploads"), schedule)
            .await
            .unwrap();
    let store = services.store.clone();
    let policy = services.insights.as_ref().unwrap().policy();
    let archive = store.create_archive("Mail").await.unwrap().id;
    let ingest = services.ingest();
    let id = ingest
        .ingest(archive, mail(1), &Placement::default())
        .await
        .unwrap()
        .message_id;
    ingest.commit().await.unwrap();
    let (walk, _) = crate::walk(&services, &runner).unwrap();
    walk.run().await.unwrap();
    let travel = || async {
        services
            .index
            .search(
                archive,
                &SearchQuery {
                    clauses: vec![Clause::Category("topic:travel".into())],
                },
                SortOrder::Newest,
                0,
                10,
            )
            .await
            .unwrap()
            .hits
    };
    assert!(travel().await.is_empty());

    let (stored, changed) = store
        .put_category_override(
            &CategoryOverride {
                id: CategoryOverrideId::new(),
                archive_id: archive,
                scope: OverrideScope::Domain,
                key: "example.com".into(),
                kind: None,
                topics: Some(holtmail_category::TopicSet::from(
                    holtmail_category::Topic::Travel,
                )),
                created_at: Timestamp::from_secs(10),
                created_by: UserId::new(),
            },
            policy,
        )
        .await
        .unwrap();
    assert_eq!(changed, [id]);
    assert_eq!(store.count_reindex_queue(archive).await.unwrap(), 1);
    assert!(travel().await.is_empty(), "search moved before the walk");
    let ran = walk.run().await.unwrap();
    assert_eq!(ran.messages, 1);
    assert_eq!(travel().await, [id]);
    assert_eq!(store.count_reindex_queue(archive).await.unwrap(), 0);

    store
        .delete_category_override(archive, stored.id, policy)
        .await
        .unwrap();
    walk.run().await.unwrap();
    assert!(travel().await.is_empty());
    assert_eq!(store.count_reindex_queue(archive).await.unwrap(), 0);
}

#[tokio::test]
async fn wakes_coalesce() {
    let wake = WalkWake::default();
    wake.signal();
    wake.signal();
    tokio::time::timeout(Duration::from_millis(100), wake.wait())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), wake.wait())
            .await
            .is_err(),
        "two signals woke it twice"
    );
}
