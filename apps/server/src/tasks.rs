use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use holtmail_api::{Task, TaskKind, TaskLevel, TaskLine, TaskState};
use holtmail_core::{AccountId, ArchiveId, Timestamp};
use holtmail_store::{
    StoreError, StoredTask, TASK_LOG_HEAD, TASK_LOG_LINES, TaskRecord, TaskStore,
};
use tokio::sync::Notify;

/// Finished tasks kept, in memory and in the store.
pub const KEEP_FINISHED: usize = 200;
const WRITE_EVERY: Duration = Duration::from_secs(1);

/// Background tasks with progress and logs. Kept in memory and, with a store, written behind
/// to it at most once a second, so finished tasks are listed again after a restart.
#[derive(Default)]
pub struct Tasks {
    entries: Mutex<VecDeque<Entry>>,
    store: Option<Arc<dyn TaskStore>>,
    pending: Mutex<Pending>,
    wake: Notify,
    writing: tokio::sync::Mutex<()>,
    hooks: std::sync::OnceLock<Arc<crate::webhooks::Hooks>>,
}

struct Entry {
    task: Task,
    job: Option<String>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Pending {
    records: HashSet<String>,
    lines: Vec<(String, TaskLine)>,
    finished: bool,
}

pub struct NewTask {
    pub kind: TaskKind,
    pub archive: Option<ArchiveId>,
    pub account: Option<AccountId>,
    pub title: String,
    pub cancellable: bool,
    /// What to start again if a restart interrupts the task.
    pub job: Option<String>,
}

/// A running task, updated by the work it describes.
#[derive(Clone)]
pub struct TaskHandle {
    tasks: Arc<Tasks>,
    id: String,
    pub cancel: Arc<AtomicBool>,
}

fn from_stored(stored: StoredTask) -> Entry {
    let record = stored.record;
    Entry {
        task: Task {
            id: record.id,
            kind: record.kind,
            archive_id: record.archive_id,
            account_id: record.account_id,
            title: record.title,
            state: record.state,
            started_at: record.started_at,
            finished_at: record.finished_at,
            done: record.done,
            total: record.total,
            summary: record.summary,
            cancellable: record.cancellable,
            log: stored.log,
        },
        job: record.job,
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

fn record(entry: &Entry) -> TaskRecord {
    let task = &entry.task;
    TaskRecord {
        id: task.id.clone(),
        kind: task.kind,
        archive_id: task.archive_id,
        account_id: task.account_id,
        title: task.title.clone(),
        state: task.state,
        started_at: task.started_at,
        finished_at: task.finished_at,
        done: task.done,
        total: task.total,
        summary: task.summary.clone(),
        cancellable: task.cancellable,
        job: entry.job.clone(),
    }
}

impl Tasks {
    /// Loads the recent tasks from `store` after marking the ones a previous process left
    /// running as interrupted, which are returned too.
    pub async fn open(
        store: Arc<dyn TaskStore>,
    ) -> Result<(Arc<Self>, Vec<TaskRecord>), StoreError> {
        let interrupted = store.interrupt_tasks(Timestamp::now()).await?;
        let entries = store
            .recent_tasks(KEEP_FINISHED as u32)
            .await?
            .into_iter()
            .map(from_stored)
            .collect();
        let tasks = Self {
            entries: Mutex::new(entries),
            store: Some(store),
            ..Self::default()
        };
        Ok((Arc::new(tasks), interrupted))
    }

    /// Writes changes to the store at most once a second; runs until the process ends.
    pub async fn write_behind(self: Arc<Self>) {
        loop {
            self.wake.notified().await;
            tokio::time::sleep(WRITE_EVERY).await;
            self.flush().await;
        }
    }

    /// Writes every change made so far to the store.
    pub async fn flush(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let _writing = self.writing.lock().await;
        let pending = std::mem::take(&mut *self.pending.lock().expect("pending lock"));
        let records: Vec<TaskRecord> = self
            .entries
            .lock()
            .expect("tasks lock")
            .iter()
            .filter(|e| pending.records.contains(&e.task.id))
            .map(record)
            .collect();
        for task in &records {
            if let Err(error) = store.save_task(task).await {
                tracing::warn!(task = task.id, %error, "task not saved");
            }
        }
        let mut lines = pending.lines.into_iter().peekable();
        while let Some((id, line)) = lines.next() {
            let mut batch = vec![line];
            while let Some((_, line)) = lines.next_if(|(next, _)| *next == id) {
                batch.push(line);
            }
            match store.append_task_log(&id, &batch).await {
                Ok(()) | Err(StoreError::NotFound(_)) => {}
                Err(error) => tracing::warn!(task = id, %error, "task log not saved"),
            }
        }
        if pending.finished
            && let Err(error) = store.prune_tasks(KEEP_FINISHED as u32).await
        {
            tracing::warn!(%error, "old tasks not removed");
        }
    }

    fn changed(&self, id: &str, line: Option<TaskLine>, finished: bool) {
        if self.store.is_none() {
            return;
        }
        let mut pending = self.pending.lock().expect("pending lock");
        pending.records.insert(id.to_owned());
        if let Some(line) = line {
            pending.lines.push((id.to_owned(), line));
        }
        pending.finished |= finished;
        drop(pending);
        self.wake.notify_one();
    }

    pub fn start(self: &Arc<Self>, new: NewTask) -> TaskHandle {
        let id = uuid::Uuid::now_v7().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Task {
            id: id.clone(),
            kind: new.kind,
            archive_id: new.archive,
            account_id: new.account,
            title: new.title,
            state: TaskState::Running,
            started_at: Timestamp::now(),
            finished_at: None,
            done: 0,
            total: None,
            summary: "Starting".into(),
            cancellable: new.cancellable,
            log: Vec::new(),
        };
        let mut entries = self.entries.lock().expect("tasks lock");
        entries.push_front(Entry {
            task,
            job: new.job,
            cancel: cancel.clone(),
        });
        let finished: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.task.state != TaskState::Running)
            .map(|(i, _)| i)
            .skip(KEEP_FINISHED)
            .collect();
        for index in finished.into_iter().rev() {
            entries.remove(index);
        }
        drop(entries);
        self.changed(&id, None, false);
        TaskHandle {
            tasks: self.clone(),
            id,
            cancel,
        }
    }

    pub fn list(&self) -> Vec<Task> {
        let entries = self.entries.lock().expect("tasks lock");
        let mut list: Vec<Task> = entries.iter().map(|e| e.task.clone()).collect();
        list.sort_by_key(|t| {
            (
                t.state != TaskState::Running,
                std::cmp::Reverse(t.started_at),
            )
        });
        list
    }

    pub fn cancel(&self, id: &str) -> Result<(), &'static str> {
        let entries = self.entries.lock().expect("tasks lock");
        let entry = entries
            .iter()
            .find(|e| e.task.id == id)
            .ok_or("There is no such task.")?;
        if entry.task.state != TaskState::Running {
            return Err("The task has already finished.");
        }
        if !entry.task.cancellable {
            return Err("This task cannot be stopped.");
        }
        entry.cancel.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Tells `hooks` when a task finishes.
    pub fn set_hooks(&self, hooks: Arc<crate::webhooks::Hooks>) {
        let _ = self.hooks.set(hooks);
    }

    pub fn running(&self, kind: TaskKind, archive: impl Into<Option<ArchiveId>>) -> Option<Task> {
        let archive = archive.into();
        self.entries
            .lock()
            .expect("tasks lock")
            .iter()
            .find(|e| {
                e.task.state == TaskState::Running
                    && e.task.kind == kind
                    && e.task.archive_id == archive
            })
            .map(|e| e.task.clone())
    }

    pub fn get(&self, id: &str) -> Option<Task> {
        self.entries
            .lock()
            .expect("tasks lock")
            .iter()
            .find(|e| e.task.id == id)
            .map(|e| e.task.clone())
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Task)) -> bool {
        match self
            .entries
            .lock()
            .expect("tasks lock")
            .iter_mut()
            .find(|e| e.task.id == id)
        {
            Some(entry) => {
                change(&mut entry.task);
                true
            }
            None => false,
        }
    }
}

impl TaskHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn snapshot(&self) -> Option<Task> {
        self.tasks.get(&self.id)
    }

    pub fn progress(&self, done: u64, total: Option<u64>, summary: impl Into<String>) {
        let summary = summary.into();
        if self.tasks.update(&self.id, |task| {
            task.done = done;
            task.total = total;
            task.summary = summary;
        }) {
            self.tasks.changed(&self.id, None, false);
        }
    }

    pub fn log(&self, level: TaskLevel, text: impl Into<String>) {
        let line = TaskLine {
            at: Timestamp::now(),
            level,
            text: text.into(),
        };
        let kept = line.clone();
        if self.tasks.update(&self.id, |task| {
            if task.log.len() >= TASK_LOG_LINES {
                task.log.remove(TASK_LOG_HEAD);
            }
            task.log.push(kept);
        }) {
            self.tasks.changed(&self.id, Some(line), false);
        }
    }

    pub fn finish(&self, state: TaskState, summary: impl Into<String>) {
        let summary = summary.into();
        let finished = self.tasks.update(&self.id, |task| {
            task.state = state;
            task.summary = summary;
            task.finished_at = Some(Timestamp::now());
            if task.total.is_some() || state == TaskState::Done {
                task.total = Some(task.total.unwrap_or(task.done).max(task.done));
            }
        });
        if !finished {
            return;
        }
        self.tasks.changed(&self.id, None, true);
        if let (Some(hooks), Some(task), Ok(runtime)) = (
            self.tasks.hooks.get().cloned(),
            self.snapshot(),
            tokio::runtime::Handle::try_current(),
        ) {
            runtime.spawn(async move {
                hooks
                    .emit(holtmail_hooks::Event::TaskFinished {
                        task_id: task.id,
                        kind: task.kind.as_str().to_owned(),
                        state: serde_json::to_value(task.state)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_default(),
                        archive_id: task.archive_id,
                        account_id: task.account_id,
                        done: task.done,
                        total: task.total,
                    })
                    .await
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    fn new(kind: TaskKind, archive: ArchiveId) -> NewTask {
        NewTask {
            kind,
            archive: Some(archive),
            account: None,
            title: "News".into(),
            cancellable: true,
            job: Some("older".into()),
        }
    }

    #[test]
    fn tracks_progress_logs_and_cancelling() {
        let tasks = Arc::new(Tasks::default());
        let archive = ArchiveId::new();
        let first = tasks.start(new(TaskKind::RemoteImages, archive));
        first.progress(2, Some(10), "2 of 10");
        first.log(TaskLevel::Info, "Started");
        assert_eq!(
            tasks.running(TaskKind::RemoteImages, archive).unwrap().id,
            first.id()
        );
        assert!(tasks.running(TaskKind::Sync, archive).is_none());

        tasks.cancel(first.id()).unwrap();
        assert!(first.cancel.load(Ordering::Relaxed));
        first.finish(TaskState::Cancelled, "Stopped");
        assert!(tasks.cancel(first.id()).is_err());
        assert!(tasks.running(TaskKind::RemoteImages, archive).is_none());

        let second = tasks.start(new(TaskKind::Sync, archive));
        let list = tasks.list();
        assert_eq!(list[0].id, second.id(), "running tasks come first");
        assert_eq!(list[1].log.len(), 1);
        assert_eq!((list[1].done, list[1].total), (2, Some(10)));
    }

    #[test]
    fn keeps_long_logs_and_old_tasks_bounded() {
        let tasks = Arc::new(Tasks::default());
        let archive = ArchiveId::new();
        let task = tasks.start(new(TaskKind::RemoteImages, archive));
        for n in 0..TASK_LOG_LINES + 20 {
            task.log(TaskLevel::Info, format!("line {n}"));
        }
        let log = task.snapshot().unwrap().log;
        assert_eq!(log.len(), TASK_LOG_LINES);
        assert_eq!(log[0].text, "line 0", "the start of the log was lost");
        assert_eq!(
            log.last().unwrap().text,
            format!("line {}", TASK_LOG_LINES + 19)
        );

        for _ in 0..KEEP_FINISHED + 10 {
            tasks
                .start(new(TaskKind::Sync, archive))
                .finish(TaskState::Done, "ok");
        }
        tasks.start(new(TaskKind::Sync, archive));
        assert!(tasks.list().len() <= KEEP_FINISHED + 2);
        assert!(tasks.get(task.id()).is_some(), "a running task was dropped");
    }

    #[tokio::test]
    async fn survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let archive = ArchiveId::new();
        let (tasks, interrupted) = Tasks::open(store.clone()).await.unwrap();
        assert!(interrupted.is_empty());

        let done = tasks.start(new(TaskKind::Sync, archive));
        done.log(TaskLevel::Info, "INBOX: 3 new");
        done.progress(1, Some(1), "3 new messages in 1 folders");
        done.finish(TaskState::Done, "3 new messages in 1 folders");
        let running = tasks.start(new(TaskKind::RemoteImages, archive));
        for n in 0..TASK_LOG_LINES + 5 {
            running.log(TaskLevel::Info, format!("line {n}"));
        }
        running.progress(4, Some(9), "4 of 9");
        let before = tasks.list();
        tasks.flush().await;
        drop(tasks);

        let (reopened, interrupted) = Tasks::open(store.clone()).await.unwrap();
        let [stopped] = interrupted.try_into().unwrap();
        assert_eq!(stopped.id, running.id());
        assert_eq!(stopped.job.as_deref(), Some("older"));
        let after = reopened.list();
        assert_eq!(after.len(), 2);
        let finished = after.iter().find(|t| t.id == done.id()).unwrap();
        assert_eq!(
            finished,
            before.iter().find(|t| t.id == done.id()).unwrap(),
            "a finished task changed across the restart"
        );
        let resumed = after.iter().find(|t| t.id == running.id()).unwrap();
        assert_eq!(resumed.state, TaskState::Interrupted);
        assert!(resumed.finished_at.is_some());
        assert_eq!((resumed.done, resumed.total), (4, Some(9)));
        assert_eq!(
            resumed.log,
            before.iter().find(|t| t.id == running.id()).unwrap().log,
            "the log differs after the restart"
        );
        assert!(reopened.cancel(running.id()).is_err());
    }

    #[tokio::test]
    async fn writes_behind_within_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let (tasks, _) = Tasks::open(store.clone()).await.unwrap();
        tokio::spawn(tasks.clone().write_behind());
        let task = tasks.start(new(TaskKind::Sync, ArchiveId::new()));
        for n in 0..100 {
            task.progress(n, Some(100), format!("{n} of 100"));
        }
        tokio::task::yield_now().await;
        assert!(
            store.recent_tasks(10).await.unwrap().is_empty(),
            "every progress tick was written at once"
        );
        let mut stored = Vec::new();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stored = store.recent_tasks(10).await.unwrap();
            if !stored.is_empty() {
                break;
            }
        }
        assert_eq!(stored[0].record.done, 99);
    }
}
