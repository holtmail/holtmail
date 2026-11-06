use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_api::{InsightProgress, TaskKind, TaskLevel, TaskState};
use holtmail_blob::BlobStore;
use holtmail_core::{ArchiveId, MessageId};
use holtmail_ingest::Ingest;
use holtmail_mime::ParsedMessage;
use holtmail_store::{Store, StoreError};
use tokio::sync::Notify;

use crate::tasks::{NewTask, TaskHandle, Tasks};

const PAGE: u32 = 200;
const PAUSE: Duration = Duration::from_millis(50);
const HOURLY: Duration = Duration::from_secs(60 * 60);

/// A stored message read and parsed once for every step that wants it.
pub(crate) struct WalkItem {
    pub message: MessageId,
    pub raw: Bytes,
    pub parsed: ParsedMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepOutcome {
    Written,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unusable {
    Unreadable,
    Unparsable,
}

/// Work derived from stored messages that the walk keeps current.
#[async_trait]
pub(crate) trait Step: Send + Sync {
    fn name(&self) -> &'static str;

    /// Messages of `archive` this step still has to do, ascending by id, strictly after `after`,
    /// at most `limit`. A message it did is never listed again until its own version changes, so
    /// the walk needs no cursor to resume.
    async fn pending(
        &self,
        store: &dyn Store,
        archive: ArchiveId,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError>;

    async fn apply(&self, item: &WalkItem) -> anyhow::Result<StepOutcome>;

    /// Records that the message's bytes cannot be read or parsed, so it is not listed again.
    async fn unusable(&self, message: MessageId, why: Unusable) -> anyhow::Result<()>;
}

/// Message insights under the runner's current fingerprint.
pub(crate) struct InsightsStep {
    pub ingest: Ingest,
    pub fingerprint: String,
}

#[async_trait]
impl Step for InsightsStep {
    fn name(&self) -> &'static str {
        "insights"
    }

    async fn pending(
        &self,
        store: &dyn Store,
        archive: ArchiveId,
        after: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<MessageId>, StoreError> {
        store
            .messages_needing_insights(archive, &self.fingerprint, after, limit)
            .await
    }

    async fn apply(&self, item: &WalkItem) -> anyhow::Result<StepOutcome> {
        let written = self
            .ingest
            .insights_for(item.message, &item.raw, &item.parsed)
            .await?;
        Ok(if written {
            StepOutcome::Written
        } else {
            StepOutcome::Unchanged
        })
    }

    async fn unusable(&self, message: MessageId, why: Unusable) -> anyhow::Result<()> {
        match why {
            Unusable::Unreadable => self.ingest.mark_insights_unreadable(message).await?,
            Unusable::Unparsable => self.ingest.mark_insights_unparsable(message).await?,
        }
        Ok(())
    }
}

/// Wakes the walk; signals while it runs or before it waits coalesce into one more run.
#[derive(Default)]
pub(crate) struct WalkWake(Notify);

impl WalkWake {
    pub(crate) fn signal(&self) {
        self.0.notify_one();
    }

    async fn wait(&self) {
        self.0.notified().await;
    }
}

/// What one run of the walk did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ran {
    pub messages: u64,
    pub written: u64,
    pub unreadable: u64,
    pub unparsable: u64,
    pub failed: u64,
    pub cancelled: bool,
}

enum Read {
    Gone,
    Unusable(Unusable),
    Done { written: bool, failed: bool },
    Failed,
}

/// Reads stored messages again for every step that lists them, archive by archive.
pub(crate) struct Walk {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
    steps: Vec<Arc<dyn Step>>,
    tasks: Arc<Tasks>,
    progress: Option<Arc<InsightProgress>>,
    page: u32,
    pause: Duration,
}

impl Walk {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        ingest: Ingest,
        steps: Vec<Arc<dyn Step>>,
        tasks: Arc<Tasks>,
    ) -> Self {
        Self {
            store,
            blobs,
            ingest,
            steps,
            tasks,
            progress: None,
            page: PAGE,
            pause: PAUSE,
        }
    }

    pub(crate) fn with_progress(mut self, progress: Arc<InsightProgress>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Runs every step over what it lists, until nothing is pending or the task is cancelled.
    /// Starts a task only when something is pending.
    pub(crate) async fn run(self: &Arc<Self>) -> anyhow::Result<Ran> {
        let mut ran = Ran::default();
        let mut task: Option<TaskHandle> = None;
        let result = self.walk(&mut ran, &mut task).await;
        if let Some(progress) = &self.progress {
            progress.finish();
        }
        if let Some(task) = &task {
            let summary = format!(
                "{} messages read: {} unreadable, {} unparsable, {} failed",
                ran.messages, ran.unreadable, ran.unparsable, ran.failed
            );
            task.log(TaskLevel::Info, summary.clone());
            let state = match &result {
                Err(error) => {
                    task.log(TaskLevel::Error, error.to_string());
                    TaskState::Failed
                }
                Ok(()) if ran.cancelled => TaskState::Cancelled,
                Ok(()) if ran.failed > 0 => TaskState::Failed,
                Ok(()) => TaskState::Done,
            };
            task.finish(state, summary);
        }
        result.map(|()| ran)
    }

    async fn walk(
        self: &Arc<Self>,
        ran: &mut Ran,
        task: &mut Option<TaskHandle>,
    ) -> anyhow::Result<()> {
        for archive in self.store.list_archives().await? {
            let mut after = None;
            loop {
                if task
                    .as_ref()
                    .is_some_and(|t| t.cancel.load(Ordering::Relaxed))
                {
                    ran.cancelled = true;
                    return Ok(());
                }
                let (page, more) = self.page(archive.id, after).await?;
                let Some(last) = page.last().map(|(id, _)| *id) else {
                    break;
                };
                let task = task.get_or_insert_with(|| self.start());
                for pair in page.chunks(2) {
                    let mut reads = tokio::task::JoinSet::new();
                    for (id, steps) in pair {
                        let this = self.clone();
                        let (id, steps) = (*id, steps.clone());
                        reads.spawn(async move { this.read(id, &steps).await });
                    }
                    while let Some(read) = reads.join_next().await {
                        ran.messages += 1;
                        match read.unwrap_or(Read::Failed) {
                            Read::Gone => {}
                            Read::Unusable(Unusable::Unreadable) => ran.unreadable += 1,
                            Read::Unusable(Unusable::Unparsable) => ran.unparsable += 1,
                            Read::Done { written, failed } => {
                                ran.written += u64::from(written);
                                ran.failed += u64::from(failed);
                            }
                            Read::Failed => ran.failed += 1,
                        }
                    }
                }
                self.ingest.commit().await?;
                if let Some(progress) = &self.progress {
                    progress.advance(page.len() as u64);
                }
                task.progress(
                    ran.messages,
                    None,
                    format!("{} messages read", ran.messages),
                );
                if !more {
                    break;
                }
                after = Some(last);
                tokio::time::sleep(self.pause).await;
            }
        }
        Ok(())
    }

    fn start(&self) -> TaskHandle {
        if let Some(progress) = &self.progress {
            progress.start();
        }
        self.tasks.start(NewTask {
            kind: TaskKind::Analyse,
            archive: None,
            account: None,
            title: "Reading archived mail again".into(),
            cancellable: true,
            job: None,
        })
    }

    /// The next ids after `after` that any step lists, with the steps that listed each, and
    /// whether more may follow.
    async fn page(
        &self,
        archive: ArchiveId,
        after: Option<MessageId>,
    ) -> Result<(Vec<(MessageId, Vec<usize>)>, bool), StoreError> {
        let mut union: BTreeMap<MessageId, Vec<usize>> = BTreeMap::new();
        let mut bound: Option<MessageId> = None;
        for (n, step) in self.steps.iter().enumerate() {
            let ids = step
                .pending(&*self.store, archive, after, self.page)
                .await?;
            if ids.len() >= self.page as usize
                && let Some(&last) = ids.last()
            {
                bound = Some(bound.map_or(last, |b| b.min(last)));
            }
            for id in ids {
                union.entry(id).or_default().push(n);
            }
        }
        let mut page: Vec<_> = union
            .into_iter()
            .filter(|(id, _)| bound.is_none_or(|b| *id <= b))
            .collect();
        let more = bound.is_some() || page.len() > self.page as usize;
        page.truncate(self.page as usize);
        Ok((page, more))
    }

    async fn read(&self, id: MessageId, steps: &[usize]) -> Read {
        let message = match self.store.get_message(id).await {
            Ok(Some(message)) => message,
            Ok(None) | Err(StoreError::NotFound(_)) => return Read::Gone,
            Err(error) => {
                tracing::warn!(message = %id, %error, "message not read again");
                return Read::Failed;
            }
        };
        let raw = match self.blobs.get(&message.blob_id).await {
            Ok(Some(raw)) => raw,
            Ok(None) | Err(_) => return self.unusable(id, steps, Unusable::Unreadable).await,
        };
        let parsed = match holtmail_mime::parse(&raw) {
            Ok(parsed) => parsed,
            Err(_) => return self.unusable(id, steps, Unusable::Unparsable).await,
        };
        let item = WalkItem {
            message: id,
            raw,
            parsed,
        };
        let mut written = false;
        let mut failed = false;
        for &n in steps {
            let step = &self.steps[n];
            match step.apply(&item).await {
                Ok(outcome) => written |= outcome == StepOutcome::Written,
                Err(error) if is_gone(&error) => return Read::Gone,
                Err(error) => {
                    tracing::warn!(message = %id, step = step.name(), %error, "walk step failed");
                    failed = true;
                }
            }
        }
        Read::Done { written, failed }
    }

    async fn unusable(&self, id: MessageId, steps: &[usize], why: Unusable) -> Read {
        tracing::info!(message = %id, reason = ?why, "stored message cannot be used");
        for &n in steps {
            let step = &self.steps[n];
            match step.unusable(id, why).await {
                Ok(()) => {}
                Err(error) if is_gone(&error) => return Read::Gone,
                Err(error) => {
                    tracing::warn!(message = %id, step = step.name(), %error, "walk step failed");
                    return Read::Failed;
                }
            }
        }
        Read::Unusable(why)
    }

    /// Runs the walk and logs what it did.
    pub(crate) async fn run_logged(self: &Arc<Self>) {
        let started = Instant::now();
        match self.run().await {
            Ok(ran) if ran.messages == 0 => {}
            Ok(ran) => tracing::info!(
                messages = ran.messages,
                written = ran.written,
                unreadable = ran.unreadable,
                unparsable = ran.unparsable,
                failed = ran.failed,
                cancelled = ran.cancelled,
                millis = started.elapsed().as_millis() as u64,
                "stored mail read again"
            ),
            Err(error) => tracing::warn!(%error, "walk over stored mail stopped"),
        }
    }
}

fn is_gone(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<holtmail_ingest::IngestError>(),
        Some(holtmail_ingest::IngestError::Store(StoreError::NotFound(_)))
    ) || matches!(
        error.downcast_ref::<StoreError>(),
        Some(StoreError::NotFound(_))
    )
}

/// Runs the walk now, then again on every wake.
pub(crate) async fn run(walk: Arc<Walk>, wake: Arc<WalkWake>) {
    loop {
        walk.run_logged().await;
        wake.wait().await;
    }
}

/// Wakes the walk every hour, so messages a crash left without rows are caught while the server
/// runs.
pub(crate) async fn wake_hourly(wake: Arc<WalkWake>) {
    let mut tick = tokio::time::interval(HOURLY);
    tick.tick().await;
    loop {
        tick.tick().await;
        wake.signal();
    }
}

#[cfg(test)]
mod tests;
