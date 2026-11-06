use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use holtmail_blob::{BlobError, BlobId};
use holtmail_blob_fs::FsBlobStore;
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::Placement;
use holtmail_store::{ArchiveStore, DeletionStore, InsightOutcome, InsightStore, MessageStore};
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

struct Fixture {
    store: Arc<SqliteStore>,
    fs: Arc<dyn BlobStore>,
    blobs: Arc<Counting>,
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
    let ingest = Ingest::new(blobs.clone(), store.clone(), index).with_insights(runner);
    Fixture {
        store,
        fs,
        blobs,
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
        let mut steps: Vec<Arc<dyn Step>> = vec![Arc::new(InsightsStep {
            ingest: self.ingest.clone(),
            fingerprint: self.fingerprint.clone(),
        })];
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
