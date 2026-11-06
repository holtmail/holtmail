use async_trait::async_trait;
use holtmail_api::{
    DeletionReview, DeletionSelection, LedgerStatus, MailboxError, Operations, Task, TaskKind,
    TaskLevel, TaskState, VerifyLedger,
};
use holtmail_core::{ArchiveId, UserId, WebhookId};
use holtmail_store::Webhook;

use crate::mailboxes::Runner;
use crate::tasks::NewTask;
use crate::{deletion, ledger};

fn backend(error: impl std::fmt::Display) -> MailboxError {
    MailboxError::Backend(error.to_string())
}

#[async_trait]
impl Operations for Runner {
    async fn ledger(&self) -> Result<LedgerStatus, MailboxError> {
        let store = &*self.services().store;
        Ok(LedgerStatus {
            head: store.ledger_head().await.map_err(backend)?,
            last: ledger::last(store).await.map_err(backend)?,
        })
    }

    async fn verify_ledger(&self, request: VerifyLedger) -> Result<Task, MailboxError> {
        if let Some(running) = self.task_list().running(TaskKind::LedgerVerify, None) {
            return Ok(running);
        }
        let task = self.task_list().start(NewTask {
            kind: TaskKind::LedgerVerify,
            archive: None,
            account: None,
            title: "Archive ledger".into(),
            cancellable: true,
            job: None,
        });
        task.log(
            TaskLevel::Info,
            if request.blobs {
                "Checking the ledger and reading every message's bytes"
            } else {
                "Checking the ledger"
            },
        );
        let snapshot = task.snapshot().expect("just started");
        let services = self.services().clone();
        tokio::spawn(async move {
            let result = ledger::verify(
                &*services.store,
                &*services.blobs,
                request,
                &task.cancel,
                |done, what| task.progress(done, None, what),
            )
            .await;
            match result {
                Ok(verification) => {
                    if let Err(error) = ledger::keep(&*services.store, &verification).await {
                        tracing::warn!(%error, "ledger verification not kept");
                    }
                    ledger::announce(&services.hooks, &verification).await;
                    for problem in &verification.problems {
                        task.log(TaskLevel::Error, problem.clone());
                    }
                    if verification.ok {
                        task.finish(
                            TaskState::Done,
                            format!("{} entries, nothing wrong", verification.entries),
                        );
                    } else {
                        task.finish(
                            TaskState::Failed,
                            format!("{} problems found", verification.problem_count),
                        );
                    }
                }
                Err(_) if task.cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                    task.finish(TaskState::Cancelled, "Cancelled")
                }
                Err(error) => {
                    task.log(TaskLevel::Error, error.to_string());
                    task.finish(TaskState::Failed, "The ledger could not be checked");
                }
            }
        });
        Ok(snapshot)
    }

    async fn review_deletion(
        &self,
        archive: ArchiveId,
        selection: DeletionSelection,
        actor: UserId,
    ) -> Result<DeletionReview, MailboxError> {
        self.reviews()
            .review(self.services(), archive, selection, Some(actor))
            .await
    }

    async fn confirm_deletion(
        &self,
        review: &str,
        count: u64,
        actor: UserId,
    ) -> Result<Task, MailboxError> {
        let confirmed = self.reviews().confirm(review, count, Some(actor))?;
        let task = self.task_list().start(NewTask {
            kind: TaskKind::DeleteMessages,
            archive: Some(confirmed.archive),
            account: None,
            title: confirmed.title,
            cancellable: true,
            job: None,
        });
        task.log(
            TaskLevel::Info,
            format!("Deleting {} messages", confirmed.ids.len()),
        );
        let snapshot = task.snapshot().expect("just started");
        let runner = self.clone();
        tokio::spawn(async move {
            let result = deletion::delete(
                runner.services(),
                runner.ingester(),
                confirmed.archive,
                &confirmed.ids,
                Some(actor),
                &task,
            )
            .await;
            runner.refresh_people(confirmed.archive);
            match result {
                Ok(done) => {
                    tracing::info!(archive = %confirmed.archive, messages = done.messages, blobs = done.blobs, "messages deleted");
                    let summary = format!(
                        "{} messages deleted, {} files removed",
                        done.messages, done.blobs
                    );
                    task.log(TaskLevel::Info, summary.clone());
                    if task.cancel.load(std::sync::atomic::Ordering::Relaxed)
                        && done.messages < confirmed.ids.len() as u64
                    {
                        task.finish(TaskState::Cancelled, summary);
                    } else {
                        task.finish(TaskState::Done, summary);
                    }
                }
                Err(error) => {
                    tracing::warn!(archive = %confirmed.archive, %error, "deletion failed");
                    task.log(TaskLevel::Error, error.to_string());
                    task.finish(TaskState::Failed, "The deletion stopped");
                }
            }
        });
        Ok(snapshot)
    }

    async fn create_webhook(
        &self,
        url: &str,
        events: &[String],
    ) -> Result<(Webhook, String), MailboxError> {
        holtmail_hooks::check(url, events).map_err(MailboxError::Invalid)?;
        self.services()
            .hooks
            .create(url, events)
            .await
            .map_err(backend)
    }

    async fn rotate_webhook_secret(&self, id: WebhookId) -> Result<String, MailboxError> {
        self.services()
            .store
            .get_webhook(id)
            .await
            .map_err(backend)?
            .ok_or_else(|| MailboxError::Invalid("There is no such webhook.".into()))?;
        self.services().hooks.rotate(id).await.map_err(backend)
    }

    async fn test_webhook(&self, id: WebhookId) -> Result<(), MailboxError> {
        self.services()
            .store
            .get_webhook(id)
            .await
            .map_err(backend)?
            .ok_or_else(|| MailboxError::Invalid("There is no such webhook.".into()))?;
        self.services().hooks.test(id).await.map_err(backend)
    }

    fn smtp(&self) -> Option<holtmail_api::SmtpStatus> {
        use std::sync::atomic::Ordering;
        let listener = self.services().smtp.get()?;
        let config = &listener.config;
        let counters = &listener.counters;
        Some(holtmail_api::SmtpStatus {
            listen: listener.address.to_string(),
            hostname: config.hostname.clone(),
            allow: config.allow.clone(),
            starttls: config.tls_cert.is_some(),
            require_tls: config.require_tls,
            auth: config.username.is_some(),
            max_size_mb: config.max_size_mb.unwrap_or(50).clamp(1, 1024),
            routes: listener
                .routes
                .iter()
                .map(|r| holtmail_api::SmtpRoute {
                    recipient: r.recipient.clone(),
                    archive_id: r.archive,
                    folder: r.folder_name.clone(),
                    journal: r.journal == crate::smtp::Journal::Exchange,
                })
                .collect(),
            received: counters.received.load(Ordering::Relaxed),
            deleted: counters.deleted.load(Ordering::Relaxed),
            refused_recipients: counters.refused_recipients.load(Ordering::Relaxed),
            failed: counters.failed.load(Ordering::Relaxed),
            last_received_at: *counters.last_received_at.lock().expect("counters lock"),
        })
    }
}
