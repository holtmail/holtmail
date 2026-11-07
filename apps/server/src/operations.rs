use async_trait::async_trait;
use holtmail_api::{
    DeletionReview, DeletionSelection, LedgerStatus, MailboxError, Operations, Task, TaskKind,
    TaskLevel, TaskState, VerifyLedger,
};
use holtmail_core::{ArchiveId, MessageId, UserId, WebhookId};
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

    async fn fresh_insights(&self, message: MessageId) -> Result<(), MailboxError> {
        let services = self.services();
        let Some(insights) = &services.insights else {
            return Ok(());
        };
        let store = &*services.store;
        let archive = store
            .get_message(message)
            .await
            .map_err(backend)?
            .ok_or_else(|| MailboxError::Invalid("There is no such message.".into()))?
            .archive_id;
        let ingest = self.ingester();
        let mut queued = false;
        let view = store.insight_view(message).await.map_err(backend)?;
        if view
            .as_ref()
            .is_none_or(|v| v.insights.fingerprint != insights.api.fingerprint)
        {
            queued |= ingest.refresh_insights(message).await.map_err(|error| {
                tracing::warn!(%message, "recomputing message insights failed");
                backend(error)
            })?;
        }
        let ctx = insights
            .deriver
            .context(store, archive)
            .await
            .map_err(backend)?;
        let categories = store
            .insight_view(message)
            .await
            .map_err(backend)?
            .and_then(|v| v.categories);
        if categories.is_none_or(|c| c.row.write.derive_fp != ctx.derive_fp()) {
            queued |= ingest.derive_categories(message).await.map_err(backend)?;
        }
        if queued {
            insights.changed();
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use holtmail_ingest::{Ingest, Placement};

    use super::*;

    const NEWS: &[u8] = b"From: News <news@news.example.com>\r\nTo: ada@example.org\r\nSubject: Issue 1\r\nMessage-ID: <fresh1@news.example.com>\r\nDate: Mon, 10 Mar 2025 09:00:00 +0000\r\nList-Unsubscribe: <https://news.example.com/u/1>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\r\nThree new features.\r\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_insights_recomputes_stale_rows_only() {
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
        let insights = services.insights.as_ref().unwrap();
        let archive = store.create_archive("Mail").await.unwrap().id;
        let bare = Ingest::new(
            services.blobs.clone(),
            store.clone(),
            services.index.clone(),
        );
        let id = bare
            .ingest(archive, Bytes::from_static(NEWS), &Placement::default())
            .await
            .unwrap()
            .message_id;
        assert!(store.insight_view(id).await.unwrap().is_none());

        runner.fresh_insights(id).await.unwrap();
        let ctx = insights.deriver.context(&*store, archive).await.unwrap();
        let view = store.insight_view(id).await.unwrap().unwrap();
        assert_eq!(view.insights.fingerprint, insights.api.fingerprint);
        let write = view.categories.unwrap().row.write;
        assert_eq!(write.derive_fp, ctx.derive_fp());
        assert_eq!(store.count_reindex_queue(archive).await.unwrap(), 1);
        let computed_at = view.insights.computed_at;

        let mut stale = write.clone();
        stale.derive_fp = "derive-old".into();
        store
            .set_message_categories(&[stale], insights.policy())
            .await
            .unwrap();
        runner.fresh_insights(id).await.unwrap();
        let view = store.insight_view(id).await.unwrap().unwrap();
        assert_eq!(
            view.insights.computed_at, computed_at,
            "insights were current"
        );
        assert_eq!(
            view.categories.unwrap().row.write.derive_fp,
            ctx.derive_fp()
        );

        let mut old = view.insights.clone();
        old.fingerprint = "fp-old".into();
        old.items = r#"{"v":1,"items":[]}"#.into();
        store.set_message_insights(id, &old).await.unwrap();
        store.invalidate_derived(&[id]).await.unwrap();
        runner.fresh_insights(id).await.unwrap();
        let view = store.insight_view(id).await.unwrap().unwrap();
        assert_eq!(view.insights.fingerprint, insights.api.fingerprint);
        assert!(view.insights.items.contains("unsubscribe"));
        assert!(view.categories.is_some());

        assert!(runner.fresh_insights(MessageId::new()).await.is_err());
    }
}
