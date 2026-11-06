use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use holtmail_core::ArchiveId;
use holtmail_store::Store;

/// The setting that says the people of every archive were built at least once. Archives from
/// before people existed lack them, and `serve` builds them once at start.
pub(crate) const PEOPLE_SETTING: &str = "people";

type Waiter = tokio::sync::oneshot::Sender<bool>;
type Rebuild = Arc<
    dyn Fn(ArchiveId) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

#[derive(Default)]
struct Queued {
    again: bool,
    waiting: Vec<Waiter>,
}

/// Rebuilds the people of archives in the background, one rebuild per archive at a time. A
/// request while one runs makes it run once more when it ends, so a burst of changes costs at
/// most two rebuilds.
pub(crate) struct PeopleRefresh {
    run: Rebuild,
    running: Mutex<HashMap<ArchiveId, Queued>>,
}

/// Forgets a running archive when its loop stops without finishing, so later requests start
/// a new one; waiters left then hear that their rebuild failed when their sender drops.
struct Running {
    this: Arc<PeopleRefresh>,
    archive: ArchiveId,
    armed: bool,
}

impl Running {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.armed
            && let Ok(mut running) = self.this.running.lock()
        {
            running.remove(&self.archive);
        }
    }
}

impl PeopleRefresh {
    pub(crate) fn new(store: Arc<dyn Store>) -> Arc<Self> {
        Self::with(Arc::new(move |archive| {
            let store = store.clone();
            Box::pin(async move { rebuild(&*store, archive).await })
        }))
    }

    fn with(run: Rebuild) -> Arc<Self> {
        Arc::new(Self {
            run,
            running: Mutex::new(HashMap::new()),
        })
    }

    /// Rebuilds the archive's people soon, without waiting.
    pub(crate) fn request(self: &Arc<Self>, archive: ArchiveId) {
        self.enqueue(archive, None);
    }

    /// Rebuilds the archive's people and waits for a rebuild that started after this call;
    /// whether it succeeded.
    pub(crate) async fn rebuild_now(self: &Arc<Self>, archive: ArchiveId) -> bool {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.enqueue(archive, Some(sender));
        receiver.await.unwrap_or(false)
    }

    fn enqueue(self: &Arc<Self>, archive: ArchiveId, waiter: Option<Waiter>) {
        {
            let mut running = self.running.lock().expect("people lock");
            if let Some(queued) = running.get_mut(&archive) {
                queued.again = true;
                queued.waiting.extend(waiter);
                return;
            }
            running.insert(archive, Queued::default());
        }
        let mut guard = Running {
            this: self.clone(),
            archive,
            armed: true,
        };
        let mut current: Vec<Waiter> = waiter.into_iter().collect();
        tokio::spawn(async move {
            loop {
                let ok = match tokio::spawn((guard.this.run)(archive)).await {
                    Ok(ok) => ok,
                    Err(error) => {
                        if error.is_panic() {
                            tracing::error!(%archive, "people rebuild panicked");
                        }
                        false
                    }
                };
                for waiter in current.drain(..) {
                    let _ = waiter.send(ok);
                }
                let mut running = guard.this.running.lock().expect("people lock");
                match running.get_mut(&archive) {
                    Some(queued) if queued.again => {
                        queued.again = false;
                        current = std::mem::take(&mut queued.waiting);
                    }
                    _ => {
                        running.remove(&archive);
                        drop(running);
                        guard.disarm();
                        break;
                    }
                }
            }
        });
    }
}

/// Rebuilds the archive's people and logs the counts; whether it succeeded.
pub(crate) async fn rebuild(store: &dyn Store, archive: ArchiveId) -> bool {
    let started = std::time::Instant::now();
    match holtmail_people::rebuild(store, archive).await {
        Ok(rebuilt) => {
            tracing::info!(
                %archive,
                people = rebuilt.people,
                with_cards = rebuilt.with_cards,
                from_mail = rebuilt.from_mail,
                millis = started.elapsed().as_millis() as u64,
                "people rebuilt"
            );
            true
        }
        Err(error) => {
            tracing::warn!(%archive, %error, "people not rebuilt");
            false
        }
    }
}

/// Builds the people of every archive once, when [`PEOPLE_SETTING`] is not set yet.
pub(crate) async fn build_if_needed(store: &dyn Store) {
    match store.setting(PEOPLE_SETTING).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "could not tell whether people were built");
            return;
        }
    }
    tracing::info!("building people from contacts and mail");
    match holtmail_people::rebuild_all(store).await {
        Ok(rebuilt) => {
            tracing::info!(people = rebuilt.people, "people built");
            if let Err(error) = store.set_setting(PEOPLE_SETTING, Some("1")).await {
                tracing::warn!(%error, "people built but not recorded");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "people not built; it is tried again at the next start")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::*;

    fn counting(panics_on: u32) -> (Arc<PeopleRefresh>, Arc<AtomicU32>) {
        let runs = Arc::new(AtomicU32::new(0));
        let counter = runs.clone();
        let refresh = PeopleRefresh::with(Arc::new(move |_| {
            let counter = counter.clone();
            Box::pin(async move {
                let run = counter.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
                if run == panics_on {
                    panic!("rebuild failed");
                }
                true
            })
        }));
        (refresh, runs)
    }

    #[tokio::test]
    async fn a_panicking_rebuild_does_not_stop_later_ones() {
        let (refresh, runs) = counting(1);
        let archive = ArchiveId::new();
        assert!(
            !refresh.rebuild_now(archive).await,
            "the panic counts as a failure"
        );
        assert!(refresh.running.lock().unwrap().is_empty());
        assert!(refresh.rebuild_now(archive).await);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn waiting_sees_a_rebuild_that_started_after_the_call() {
        let (refresh, runs) = counting(0);
        let archive = ArchiveId::new();
        refresh.request(archive);
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (first, second) =
            tokio::join!(refresh.rebuild_now(archive), refresh.rebuild_now(archive));
        assert!(first && second);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "both waited for the one run after the running one"
        );
        assert!(refresh.running.lock().unwrap().is_empty());
    }
}
