use std::time::{Duration, Instant};

use holtmail_core::ArchiveId;
use holtmail_insights::{DeriveContext, Deriver};
use holtmail_store::{CategoryPolicy, Store};

const PAGE: u32 = 1000;
const BATCH: usize = 500;
const MAX_RESTARTS: u32 = 8;

/// What one stage-B pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Derived {
    /// Rows written, rewrites after a restart included.
    pub rows: u64,
    /// Messages whose effective kind or topics changed, each queued for reindexing in the same
    /// transaction as its row.
    pub changed: u64,
    /// Times an archive started again with fresh snapshots.
    pub restarts: u64,
    /// Archives left for the next pass after an error or too many restarts.
    pub failed: u64,
    /// Batches written.
    pub batches: u64,
    /// The longest single batch write.
    pub slowest_batch: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub page: u32,
    pub batch: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            page: PAGE,
            batch: BATCH,
        }
    }
}

/// Brings every stored categories row up to the deriver's current fingerprint, archive by
/// archive, from stored columns only: no blob is read. Each archive is derived under one context;
/// when a categorizer's snapshot changes, the archive starts again after the batch in flight with
/// a fresh context, so every row carries the fingerprint of the context its values came from.
/// Archives finished under a context that went stale are done again before it returns.
pub(crate) async fn derive_if_needed(
    store: &dyn Store,
    deriver: &Deriver,
    policy: CategoryPolicy,
) -> anyhow::Result<Derived> {
    derive_with(store, deriver, policy, Limits::default(), &|_| {}).await
}

/// [`derive_if_needed`] with other page and batch sizes, calling `after_batch` with the number
/// of batches written so far after each one.
pub(crate) async fn derive_with(
    store: &dyn Store,
    deriver: &Deriver,
    policy: CategoryPolicy,
    limits: Limits,
    after_batch: &(dyn Fn(u64) + Sync),
) -> anyhow::Result<Derived> {
    let mut derived = Derived::default();
    let mut todo: Vec<ArchiveId> = store
        .list_archives()
        .await?
        .into_iter()
        .map(|archive| archive.id)
        .collect();
    let mut rounds = 0;
    while !todo.is_empty() {
        let mut done = Vec::new();
        for archive in todo {
            match derive_archive(
                store,
                deriver,
                policy,
                limits,
                archive,
                &mut derived,
                after_batch,
            )
            .await
            {
                Ok(Some(ctx)) => done.push(ctx),
                Ok(None) => derived.failed += 1,
                Err(error) => {
                    tracing::warn!(%archive, %error, "categories not derived");
                    derived.failed += 1;
                }
            }
        }
        let stale: Vec<ArchiveId> = done
            .iter()
            .filter(|ctx| deriver.changed(ctx))
            .map(|ctx| ctx.archive())
            .collect();
        rounds += 1;
        if rounds > MAX_RESTARTS {
            derived.failed += stale.len() as u64;
            break;
        }
        derived.restarts += stale.len() as u64;
        todo = stale;
    }
    Ok(derived)
}

/// Derives one archive; `None` when its snapshots kept changing and it was left for later.
async fn derive_archive(
    store: &dyn Store,
    deriver: &Deriver,
    policy: CategoryPolicy,
    limits: Limits,
    archive: ArchiveId,
    derived: &mut Derived,
    after_batch: &(dyn Fn(u64) + Sync),
) -> anyhow::Result<Option<DeriveContext>> {
    let mut ctx = deriver.context(store, archive).await?;
    let mut restarts = 0;
    let mut after = None;
    loop {
        let rows = store
            .derive_inputs(archive, ctx.derive_fp(), after, limits.page)
            .await?;
        let Some(last) = rows.last().map(|row| row.message) else {
            return Ok(Some(ctx));
        };
        let mut restart = false;
        for chunk in rows.chunks(limits.batch) {
            let writes: Vec<_> = chunk.iter().map(|row| deriver.derive(&ctx, row)).collect();
            let started = Instant::now();
            let changed = store.set_message_categories_queued(&writes, policy).await?;
            derived.slowest_batch = derived.slowest_batch.max(started.elapsed());
            derived.rows += writes.len() as u64;
            derived.changed += changed.len() as u64;
            derived.batches += 1;
            after_batch(derived.batches);
            if deriver.changed(&ctx) {
                restart = true;
                break;
            }
        }
        if restart {
            if restarts == MAX_RESTARTS {
                tracing::warn!(%archive, restarts, "categories kept changing; left for the next pass");
                return Ok(None);
            }
            restarts += 1;
            derived.restarts += 1;
            ctx = deriver.context(store, archive).await?;
            after = None;
            continue;
        }
        if rows.len() < limits.page as usize {
            return Ok(Some(ctx));
        }
        after = Some(last);
    }
}

/// Runs [`derive_if_needed`], logs counts and timings, and returns what it did; nothing after an
/// error.
pub(crate) async fn derive_logged(
    store: &dyn Store,
    deriver: &Deriver,
    policy: CategoryPolicy,
) -> Derived {
    let started = Instant::now();
    match derive_if_needed(store, deriver, policy).await {
        Ok(derived) if derived.rows == 0 && derived.restarts == 0 && derived.failed == 0 => derived,
        Ok(derived) => {
            let elapsed = started.elapsed();
            tracing::info!(
                rows = derived.rows,
                changed = derived.changed,
                restarts = derived.restarts,
                failed = derived.failed,
                batches = derived.batches,
                slowest_batch_millis = derived.slowest_batch.as_millis() as u64,
                millis = elapsed.as_millis() as u64,
                rows_per_second = (derived.rows as f64 / elapsed.as_secs_f64().max(1e-6)) as u64,
                "categories derived"
            );
            derived
        }
        Err(error) => {
            tracing::warn!(%error, "categories pass stopped");
            Derived::default()
        }
    }
}

#[cfg(test)]
mod tests;
