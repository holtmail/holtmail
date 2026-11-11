//! A [`Scheduler`] over iMIP: the iTIP objects and the choice of who sends them from
//! `holtmail-pim-edit` (Phase 62's `organize`, `reply`, `apply_reply`, `organizer_route` and
//! `reply_route`), the mail around them from `holtmail-compose`, and the outbox through
//! `holtmail_submission::Submission`.
//!
//! It keeps no store state: what it delivered in the last [`holtmail_schedule::RECENT`] lives
//! in memory, for dropping repeated messages and for [`Scheduler::just_sent`]. After a restart
//! the outbox's idempotency keys still catch a message repeated within the same window; a key
//! that names a cancelled message is followed by a fresh one. Every message is submitted held
//! and released by [`Scheduler::confirm`] once its change is recorded.

mod deliver;
mod ical;
mod incoming;
mod prepare;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, OutboxId};
use holtmail_schedule::{
    Change, ClientPut, Clock, Delivery, Incoming, IncomingItip, ItipKey, Prepared, RECENT,
    ScheduleError, Scheduler,
};
use holtmail_submission::{Actor, Submission, SubmissionError, SubmissionState};

struct Sent {
    digest: [u8; 32],
    key: ItipKey,
    recipients: Vec<String>,
    at: i64,
    id: OutboxId,
}

/// The iMIP scheduler. Safe to share between tasks.
pub struct ItipScheduler {
    submission: Arc<dyn Submission>,
    clock: Clock,
    undo_window: Option<Duration>,
    recent: Mutex<HashMap<ArchiveId, Vec<Sent>>>,
    generations: Mutex<HashMap<[u8; 32], u32>>,
}

impl ItipScheduler {
    /// A scheduler that submits through `submission` and reads time from `clock`. Messages are
    /// held until [`Scheduler::confirm`], then wait the instance's undo window.
    pub fn new(submission: Arc<dyn Submission>, clock: Clock) -> Self {
        Self {
            submission,
            clock,
            undo_window: None,
            recent: Mutex::default(),
            generations: Mutex::default(),
        }
    }

    /// Confirmed messages wait `window` instead of the instance's undo window. Until
    /// [`Scheduler::confirm`] they are held whatever the window.
    pub fn with_undo_window(mut self, window: Duration) -> Self {
        self.undo_window = Some(window);
        self
    }

    fn now(&self) -> i64 {
        (self.clock)().as_millis()
    }

    fn window() -> i64 {
        RECENT.as_millis() as i64
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ArchiveId, Vec<Sent>>> {
        self.recent.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a message with `digest` was delivered recently and not undone since, by
    /// [`Scheduler::withdraw`] or in the outbox.
    async fn was_sent(&self, actor: &Actor, archive: ArchiveId, digest: &[u8; 32]) -> bool {
        let now = self.now();
        let ids: Vec<OutboxId> = self
            .lock()
            .get(&archive)
            .map(|all| {
                all.iter()
                    .filter(|s| s.digest == *digest && now - s.at < Self::window())
                    .map(|s| s.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            match self.submission.status(actor, id).await {
                Ok(s) if s.state == SubmissionState::Cancelled => self.forget(&[id]),
                Err(SubmissionError::NotFound(_)) => self.forget(&[id]),
                _ => return true,
            }
        }
        false
    }

    fn remember(
        &self,
        archive: ArchiveId,
        digest: [u8; 32],
        key: ItipKey,
        recipients: Vec<String>,
        id: OutboxId,
    ) {
        let now = self.now();
        let mut recent = self.lock();
        for all in recent.values_mut() {
            all.retain(|s| now - s.at < Self::window());
        }
        recent.retain(|_, all| !all.is_empty());
        recent.entry(archive).or_default().push(Sent {
            digest,
            key,
            recipients,
            at: now,
            id,
        });
    }

    fn bump(&self, digest: &[u8; 32]) {
        *self
            .generations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(*digest)
            .or_default() += 1;
    }

    fn generation(&self, digest: &[u8; 32]) -> u32 {
        self.generations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(digest)
            .copied()
            .unwrap_or(0)
    }

    fn forget(&self, ids: &[OutboxId]) {
        let mut withdrawn = Vec::new();
        let mut recent = self.lock();
        for all in recent.values_mut() {
            all.retain(|s| {
                let keep = !ids.contains(&s.id);
                if !keep {
                    withdrawn.push(s.digest);
                }
                keep
            });
        }
        drop(recent);
        let mut generations = self.generations.lock().unwrap_or_else(|e| e.into_inner());
        for digest in withdrawn {
            *generations.entry(digest).or_default() += 1;
        }
    }

    async fn undo_all(&self, actor: &Actor, ids: &[OutboxId]) -> Result<(), ScheduleError> {
        let mut failure = None;
        let mut undone = Vec::new();
        for id in ids {
            match self.submission.undo(actor, *id).await {
                Ok(_) => undone.push(*id),
                Err(SubmissionError::TooLate(_) | SubmissionError::NotFound(_)) => {}
                Err(e) => failure = Some(e),
            }
        }
        self.forget(&undone);
        match failure {
            Some(e) => Err(ScheduleError::Unavailable(e.to_string())),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl Scheduler for ItipScheduler {
    async fn prepare(&self, put: &ClientPut) -> Result<Prepared, ScheduleError> {
        prepare::prepare(put)
    }

    async fn deliver(&self, change: &Change) -> Result<Delivery, ScheduleError> {
        deliver::deliver(self, change).await
    }

    async fn confirm(&self, actor: &Actor, delivery: &Delivery) -> Result<(), ScheduleError> {
        let mut failure = None;
        for id in &delivery.queued {
            match self.submission.release(actor, *id, self.undo_window).await {
                Ok(_) | Err(SubmissionError::NotFound(_)) => {}
                Err(e) => failure = Some(ScheduleError::Unavailable(e.to_string())),
            }
        }
        failure.map_or(Ok(()), Err)
    }

    async fn withdraw(&self, actor: &Actor, delivery: &Delivery) -> Result<(), ScheduleError> {
        self.undo_all(actor, &delivery.queued).await
    }

    async fn incoming(&self, mail: &IncomingItip) -> Result<Incoming, ScheduleError> {
        Ok(incoming::decide(mail))
    }

    async fn just_sent(
        &self,
        archive: ArchiveId,
        key: &ItipKey,
        recipients: &[String],
    ) -> Result<bool, ScheduleError> {
        let now = self.now();
        let wanted: Vec<String> = recipients
            .iter()
            .map(|r| holtmail_pim::normalize_email(r).unwrap_or_else(|| r.to_ascii_lowercase()))
            .collect();
        let recent = self.lock();
        let sent: Vec<&Sent> = recent
            .get(&archive)
            .map(|all| {
                all.iter()
                    .filter(|s| s.key == *key && now - s.at < Self::window())
                    .collect()
            })
            .unwrap_or_default();
        Ok(!wanted.is_empty()
            && wanted
                .iter()
                .all(|r| sent.iter().any(|s| s.recipients.contains(r))))
    }
}
