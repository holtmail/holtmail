use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use holtmail_api::{Task, TaskKind, TaskLevel, TaskLine, TaskState};
use holtmail_core::{AccountId, ArchiveId, Timestamp};

const KEEP_FINISHED: usize = 50;
const LOG_HEAD: usize = 50;
const LOG_LINES: usize = 500;

/// Background tasks with progress and logs, in memory.
#[derive(Default)]
pub struct Tasks {
    next: AtomicU64,
    entries: Mutex<VecDeque<Entry>>,
}

struct Entry {
    task: Task,
    cancel: Arc<AtomicBool>,
}

pub struct NewTask {
    pub kind: TaskKind,
    pub archive: Option<ArchiveId>,
    pub account: Option<AccountId>,
    pub title: String,
    pub cancellable: bool,
}

/// A running task, updated by the work it describes.
#[derive(Clone)]
pub struct TaskHandle {
    tasks: Arc<Tasks>,
    id: String,
    pub cancel: Arc<AtomicBool>,
}

impl Tasks {
    pub fn start(self: &Arc<Self>, new: NewTask) -> TaskHandle {
        let id = format!("t{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let cancel = Arc::new(AtomicBool::new(false));
        let now = Timestamp::now();
        let task = Task {
            id: id.clone(),
            kind: new.kind,
            archive_id: new.archive,
            account_id: new.account,
            title: new.title,
            state: TaskState::Running,
            started_at: now,
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

    pub fn running(&self, kind: TaskKind, archive: ArchiveId) -> Option<Task> {
        self.entries
            .lock()
            .expect("tasks lock")
            .iter()
            .find(|e| {
                e.task.state == TaskState::Running
                    && e.task.kind == kind
                    && e.task.archive_id == Some(archive)
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

    fn update(&self, id: &str, change: impl FnOnce(&mut Task)) {
        if let Some(entry) = self
            .entries
            .lock()
            .expect("tasks lock")
            .iter_mut()
            .find(|e| e.task.id == id)
        {
            change(&mut entry.task);
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
        self.tasks.update(&self.id, |task| {
            task.done = done;
            task.total = total;
            task.summary = summary;
        });
    }

    pub fn log(&self, level: TaskLevel, text: impl Into<String>) {
        let line = TaskLine {
            at: Timestamp::now(),
            level,
            text: text.into(),
        };
        self.tasks.update(&self.id, |task| {
            if task.log.len() >= LOG_LINES {
                task.log.remove(LOG_HEAD);
            }
            task.log.push(line);
        });
    }

    pub fn finish(&self, state: TaskState, summary: impl Into<String>) {
        let summary = summary.into();
        self.tasks.update(&self.id, |task| {
            task.state = state;
            task.summary = summary;
            task.finished_at = Some(Timestamp::now());
            if task.total.is_some() || state == TaskState::Done {
                task.total = Some(task.total.unwrap_or(task.done).max(task.done));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(kind: TaskKind, archive: ArchiveId) -> NewTask {
        NewTask {
            kind,
            archive: Some(archive),
            account: None,
            title: "News".into(),
            cancellable: true,
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
        for n in 0..LOG_LINES + 20 {
            task.log(TaskLevel::Info, format!("line {n}"));
        }
        let log = task.snapshot().unwrap().log;
        assert_eq!(log.len(), LOG_LINES);
        assert_eq!(log[0].text, "line 0", "the start of the log was lost");
        assert_eq!(log.last().unwrap().text, format!("line {}", LOG_LINES + 19));

        for _ in 0..KEEP_FINISHED + 10 {
            tasks
                .start(new(TaskKind::Sync, archive))
                .finish(TaskState::Done, "ok");
        }
        tasks.start(new(TaskKind::Sync, archive));
        assert!(tasks.list().len() <= KEEP_FINISHED + 2);
        assert!(tasks.get(task.id()).is_some(), "a running task was dropped");
    }
}
