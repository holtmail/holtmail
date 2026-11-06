use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use holtmail_api::{DeletionReview, DeletionSelection, MAX_DELETION, MailboxError, TaskLevel};
use holtmail_core::{ArchiveId, MessageId, Timestamp, UserId};
use holtmail_index::SortOrder;
use holtmail_ingest::Ingest;

use crate::config::Services;
use crate::tasks::TaskHandle;

const REVIEW_FOR: Duration = Duration::from_secs(30 * 60);
const MAX_REVIEWS: usize = 20;
const SAMPLE: usize = 20;
const SEARCH_PAGE: u32 = 1_000;
const DELETE_BATCH: usize = 200;

struct Pending {
    review: DeletionReview,
    ids: Vec<MessageId>,
    actor: Option<UserId>,
    expires: Instant,
}

/// Reviews waiting for their confirmation, in memory.
#[derive(Default)]
pub struct Reviews(Mutex<HashMap<String, Pending>>);

/// What a confirmed review deletes.
pub struct Confirmed {
    pub archive: ArchiveId,
    pub title: String,
    pub ids: Vec<MessageId>,
}

fn invalid(message: impl Into<String>) -> MailboxError {
    MailboxError::Invalid(message.into())
}

fn backend(error: impl std::fmt::Display) -> MailboxError {
    MailboxError::Backend(error.to_string())
}

impl Reviews {
    pub async fn review(
        &self,
        services: &Services,
        archive: ArchiveId,
        selection: DeletionSelection,
        actor: Option<UserId>,
    ) -> Result<DeletionReview, MailboxError> {
        let store = &*services.store;
        store
            .get_archive(archive)
            .await
            .map_err(backend)?
            .ok_or_else(|| invalid("There is no such archive."))?;
        let (ids, query) = match selection {
            DeletionSelection::Messages(ids) => {
                let mut seen = HashSet::new();
                let wanted: Vec<MessageId> =
                    ids.into_iter().filter(|id| seen.insert(*id)).collect();
                if wanted.len() as u64 > MAX_DELETION {
                    return Err(invalid(format!(
                        "At most {MAX_DELETION} messages can be deleted at once."
                    )));
                }
                let mut found = Vec::with_capacity(wanted.len());
                for chunk in wanted.chunks(500) {
                    found.extend(
                        store
                            .get_messages(chunk)
                            .await
                            .map_err(backend)?
                            .into_iter()
                            .filter(|m| m.archive_id == archive),
                    );
                }
                (found, None)
            }
            DeletionSelection::Query(text) => {
                let parsed = holtmail_query::parse(&text).map_err(|e| invalid(e.to_string()))?;
                if parsed.clauses.is_empty() {
                    return Err(invalid(
                        "Enter a search; deleting a whole archive is done by removing the archive.",
                    ));
                }
                let mut ids = Vec::new();
                let mut offset = 0;
                loop {
                    let page = services
                        .index
                        .search(archive, &parsed, SortOrder::Newest, offset, SEARCH_PAGE)
                        .await
                        .map_err(backend)?;
                    if page.total > MAX_DELETION {
                        return Err(invalid(format!(
                            "The search finds {} messages; at most {MAX_DELETION} can be deleted at once.",
                            page.total
                        )));
                    }
                    let last = page.hits.len() < SEARCH_PAGE as usize;
                    ids.extend(page.hits);
                    offset += SEARCH_PAGE;
                    if last || u64::from(offset) >= page.total {
                        break;
                    }
                }
                let mut seen = HashSet::new();
                ids.retain(|id| seen.insert(*id));
                let mut found = Vec::with_capacity(ids.len());
                for chunk in ids.chunks(500) {
                    found.extend(store.get_messages(chunk).await.map_err(backend)?);
                }
                (found, Some(text.trim().to_owned()))
            }
        };
        let mut messages = ids;
        if messages.is_empty() {
            return Err(invalid("Nothing to delete."));
        }
        messages.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
        let review = DeletionReview {
            id: uuid::Uuid::new_v4().simple().to_string(),
            archive_id: archive,
            query,
            count: messages.len() as u64,
            bytes: messages.iter().map(|m| m.size).sum(),
            oldest: messages.last().map(|m| m.date),
            newest: messages.first().map(|m| m.date),
            sample: messages.iter().take(SAMPLE).cloned().collect(),
            expires_at: Timestamp::from_millis(
                Timestamp::now().as_millis() + REVIEW_FOR.as_millis() as i64,
            ),
        };
        let now = Instant::now();
        let mut pending = self.0.lock().expect("reviews lock");
        pending.retain(|_, p| p.expires > now);
        while pending.len() >= MAX_REVIEWS {
            let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, p)| p.expires)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            pending.remove(&oldest);
        }
        pending.insert(
            review.id.clone(),
            Pending {
                review: review.clone(),
                ids: messages.into_iter().map(|m| m.id).collect(),
                actor,
                expires: now + REVIEW_FOR,
            },
        );
        Ok(review)
    }

    pub fn confirm(
        &self,
        id: &str,
        count: u64,
        actor: Option<UserId>,
    ) -> Result<Confirmed, MailboxError> {
        let mut pending = self.0.lock().expect("reviews lock");
        let stale = || invalid("This review has expired or was used; review the deletion again.");
        let review = pending
            .get(id)
            .filter(|p| p.expires > Instant::now())
            .ok_or_else(stale)?;
        if review.actor != actor {
            return Err(stale());
        }
        if review.review.count != count {
            return Err(invalid(format!(
                "The review deletes {} messages; confirm that number.",
                review.review.count
            )));
        }
        let review = pending.remove(id).ok_or_else(stale)?;
        let title = match &review.review.query {
            Some(query) => format!("{} messages matching {query}", review.review.count),
            None => format!("{} selected messages", review.review.count),
        };
        Ok(Confirmed {
            archive: review.review.archive_id,
            title,
            ids: review.ids,
        })
    }
}

/// What a deletion task did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Done {
    pub messages: u64,
    pub blobs: u64,
}

/// Deletes `ids` from `archive` in batches, removing their index entries and orphaned blobs,
/// rebuilding calendar items that lost versions. Stops between batches when cancelled.
pub async fn delete(
    services: &Services,
    ingest: &Ingest,
    archive: ArchiveId,
    ids: &[MessageId],
    actor: Option<UserId>,
    task: &TaskHandle,
) -> anyhow::Result<Done> {
    let mut done = Done::default();
    let total = ids.len() as u64;
    for chunk in ids.chunks(DELETE_BATCH) {
        if task.cancel.load(Ordering::Relaxed) {
            break;
        }
        let deleted = services
            .store
            .delete_messages(archive, chunk, actor)
            .await?;
        services.index.remove(&deleted.messages).await?;
        if !deleted.removed_items.is_empty() {
            services.index.remove_items(&deleted.removed_items).await?;
        }
        for item in &deleted.changed_items {
            if let Err(error) = ingest.rebuild_item(*item).await {
                task.log(
                    TaskLevel::Warning,
                    format!("Event {item} could not be rebuilt: {error}"),
                );
            }
        }
        services.index.commit().await?;
        for blob in &deleted.orphans {
            services.blobs.delete(blob).await?;
        }
        done.messages += deleted.messages.len() as u64;
        done.blobs += deleted.orphans.len() as u64;
        task.progress(
            done.messages,
            Some(total),
            format!("{} of {total} messages deleted", done.messages),
        );
    }
    if done.messages > 0 {
        let head = services.store.ledger_head().await?;
        services
            .hooks
            .emit(holtmail_hooks::Event::MessagesDeleted {
                archive_id: archive,
                messages: done.messages,
                by: actor,
                ledger_seq: head.as_ref().map(|h| h.seq),
                ledger_hash: head.map(|h| h.hash.to_string()),
            })
            .await;
    }
    Ok(done)
}

#[cfg(test)]
mod tests;
