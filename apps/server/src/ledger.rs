use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use holtmail_api::{LedgerVerification, VerifyLedger};
use holtmail_blob::{BlobId, BlobStore};
use holtmail_core::Timestamp;
use holtmail_ledger::{ChainCheck, ChainProblem, MAX_PROBLEMS};
use holtmail_store::{LedgerGaps, MessageQuery, Store};

pub const VERIFICATION_SETTING: &str = "ledger_verification";
const PAGE: u32 = 1_000;

/// Enters every live message the ledger does not have yet, such as those archived before it
/// existed. Returns how many were entered.
pub async fn enter_unentered(store: &dyn Store) -> anyhow::Result<u64> {
    let mut entered = 0;
    loop {
        let done = store.enter_unentered(PAGE).await?;
        entered += done;
        if done == 0 {
            return Ok(entered);
        }
    }
}

/// Reads the anchor as `<seq>:<hash>`.
pub fn parse_anchor(text: &str) -> anyhow::Result<(u64, holtmail_ledger::LedgerHash)> {
    let (seq, hash) = text
        .trim()
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("an anchor is <position>:<hash>"))?;
    let seq: u64 = seq
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("the position is a number"))?;
    anyhow::ensure!(seq > 0, "positions start at 1");
    Ok((seq, hash.parse()?))
}

/// Checks the chain, compares it with the live messages and, when asked, every message's bytes
/// with its id. `progress` gets the entries or messages done so far and what is being done.
pub async fn verify(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    request: VerifyLedger,
    cancel: &AtomicBool,
    progress: impl Fn(u64, &str),
) -> anyhow::Result<LedgerVerification> {
    let started_at = Timestamp::now();
    let mut check = ChainCheck::new();
    if let Some((seq, hash)) = request.anchor {
        check = check.with_anchor(seq, hash);
    }
    let mut after = 0;
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
        let page = store.ledger_entries(after, PAGE).await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        for entry in &page {
            check.push(entry);
        }
        progress(after, "Checking the chain");
    }
    let chain = check.finish();
    let mut problems: Vec<String> = chain.problems.iter().map(describe).collect();
    let mut problem_count = chain.problem_count;

    progress(chain.entries, "Comparing with the messages");
    let gaps = store.ledger_gaps(MAX_PROBLEMS as u32).await?;
    problem_count += gaps.unentered_count + gaps.changed_count + gaps.vanished_count;
    describe_gaps(&gaps, &mut problems);

    let mut blobs_checked = None;
    if request.blobs {
        let mut checked = 0u64;
        for archive in store.list_archives().await? {
            let mut cursor = None;
            loop {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
                let page = store
                    .list_messages(&MessageQuery {
                        archive_id: archive.id,
                        folder_id: None,
                        label: None,
                        after: cursor,
                        limit: 200,
                    })
                    .await?;
                for message in &page.items {
                    checked += 1;
                    let altered = || {
                        format!(
                            "message {}: its bytes no longer match their hash",
                            message.id
                        )
                    };
                    let problem = match blobs.get(&message.blob_id).await {
                        Ok(None) => Some(format!("message {}: its bytes are missing", message.id)),
                        Ok(Some(bytes)) if BlobId::of(&bytes) != message.blob_id => Some(altered()),
                        Ok(Some(_)) => None,
                        Err(holtmail_blob::BlobError::Corrupt(_)) => Some(altered()),
                        Err(error) => return Err(error.into()),
                    };
                    if let Some(problem) = problem {
                        problem_count += 1;
                        if problems.len() < MAX_PROBLEMS {
                            problems.push(problem);
                        }
                    }
                }
                progress(checked, "Reading message bytes");
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
        }
        blobs_checked = Some(checked);
    }
    problems.truncate(MAX_PROBLEMS);
    Ok(LedgerVerification {
        started_at,
        finished_at: Timestamp::now(),
        ok: problem_count == 0,
        entries: chain.entries,
        head_seq: chain.head.map(|(seq, _)| seq),
        head_hash: chain.head.map(|(_, hash)| hash),
        anchor_seq: request.anchor.map(|(seq, _)| seq),
        blobs_checked,
        problems,
        problem_count,
    })
}

fn describe(problem: &ChainProblem) -> String {
    match problem {
        ChainProblem::Gap { expected, found } => {
            format!("entry {expected} is missing; entry {found} came in its place")
        }
        ChainProblem::BrokenLink { seq } => {
            format!("entry {seq} does not follow the entry before it")
        }
        ChainProblem::Altered { seq } => format!("entry {seq} was changed after it was written"),
        ChainProblem::AnchorMismatch { seq } => {
            format!("entry {seq} has another hash than the one written down")
        }
        ChainProblem::AnchorMissing { seq } => {
            format!("the ledger ends before entry {seq}, which was written down")
        }
    }
}

fn describe_gaps(gaps: &LedgerGaps, problems: &mut Vec<String>) {
    for id in &gaps.unentered {
        problems.push(format!("message {id} is not in the ledger"));
    }
    if gaps.unentered_count > gaps.unentered.len() as u64 {
        problems.push(format!(
            "{} more messages are not in the ledger",
            gaps.unentered_count - gaps.unentered.len() as u64
        ));
    }
    for id in &gaps.changed {
        problems.push(format!(
            "message {id} points at other bytes than the ledger says"
        ));
    }
    for id in &gaps.vanished {
        problems.push(format!(
            "message {id} is gone without a deletion in the ledger"
        ));
    }
}

/// Keeps `verification` for the settings page and the API.
pub async fn keep(store: &dyn Store, verification: &LedgerVerification) -> anyhow::Result<()> {
    store
        .set_setting(
            VERIFICATION_SETTING,
            Some(&serde_json::to_string(verification)?),
        )
        .await?;
    Ok(())
}

/// Tells webhooks how a verification ended.
pub async fn announce(hooks: &crate::webhooks::Hooks, verification: &LedgerVerification) {
    hooks
        .emit(holtmail_hooks::Event::LedgerVerified {
            ok: verification.ok,
            entries: verification.entries,
            problems: verification.problem_count,
            ledger_seq: verification.head_seq,
            ledger_hash: verification.head_hash.map(|h| h.to_string()),
        })
        .await;
}

/// The verification kept last.
pub async fn last(store: &dyn Store) -> anyhow::Result<Option<LedgerVerification>> {
    Ok(match store.setting(VERIFICATION_SETTING).await? {
        Some(text) => serde_json::from_str(&text).ok(),
        None => None,
    })
}

/// Enters unentered messages once at start, logging only counts.
pub async fn enter_at_start(store: Arc<dyn Store>) {
    match enter_unentered(&*store).await {
        Ok(0) => {}
        Ok(entered) => tracing::info!(entered, "messages entered in the ledger"),
        Err(error) => tracing::warn!(%error, "messages not entered in the ledger"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use bytes::Bytes;
    use holtmail_blob_fs::FsBlobStore;
    use holtmail_store::{ArchiveStore as _, LedgerStore as _, MessageStore as _};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    fn walk(path: std::path::PathBuf) -> Vec<std::path::PathBuf> {
        if path.is_dir() {
            std::fs::read_dir(&path)
                .unwrap()
                .flat_map(|e| walk(e.unwrap().path()))
                .collect()
        } else {
            vec![path]
        }
    }

    #[tokio::test]
    async fn names_missing_bytes_and_a_wrong_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("db")).await.unwrap();
        let blobs = FsBlobStore::open(dir.path().join("blobs")).await.unwrap();
        let archive = store.create_archive("Ledger").await.unwrap().id;
        let mut ids = Vec::new();
        for n in 1..=3 {
            let new = holtmail_store::testing::message(archive, n, 1_741_600_000);
            blobs
                .put(Bytes::from(format!("message {n}")))
                .await
                .unwrap();
            ids.push(store.insert_message(new).await.unwrap());
        }
        let head = store.ledger_head().await.unwrap().unwrap();
        let cancel = AtomicBool::new(false);
        let run = |request| verify(&store, &blobs, request, &cancel, |_, _| {});

        let good = run(VerifyLedger {
            blobs: true,
            anchor: Some((head.seq, head.hash)),
        })
        .await
        .unwrap();
        assert!(good.ok, "{good:?}");
        assert_eq!((good.entries, good.blobs_checked), (3, Some(3)));
        assert_eq!(good.head_hash, Some(head.hash));

        let message = store.get_message(ids[1].message_id).await.unwrap().unwrap();
        blobs.delete(&message.blob_id).await.unwrap();
        let changed = store.get_message(ids[2].message_id).await.unwrap().unwrap();
        let path = std::fs::read_dir(dir.path().join("blobs"))
            .unwrap()
            .flat_map(|d| walk(d.unwrap().path()))
            .find(|p| {
                p.to_string_lossy()
                    .contains(&changed.blob_id.to_string()[2..])
            })
            .expect("the blob file");
        std::fs::write(&path, b"message 3, changed").unwrap();
        let bad = run(VerifyLedger {
            blobs: true,
            anchor: Some((2, head.hash)),
        })
        .await
        .unwrap();
        assert!(!bad.ok);
        assert_eq!(bad.problem_count, 3, "{bad:?}");
        assert!(bad.problems.iter().any(|p| p.contains("no longer match")));
        assert!(
            bad.problems
                .iter()
                .any(|p| p.contains("entry 2 has another hash"))
        );
        assert!(bad.problems.iter().any(|p| p.contains("bytes are missing")));

        let quick = run(VerifyLedger::default()).await.unwrap();
        assert!(quick.ok, "without reading bytes the ledger itself is fine");
        assert_eq!(quick.blobs_checked, None);

        keep(&store, &bad).await.unwrap();
        assert_eq!(last(&store).await.unwrap(), Some(bad));
        assert!(parse_anchor(&format!("3:{}", head.hash)).is_ok());
        for wrong in [
            "3",
            "0:00",
            &format!("x:{}", head.hash),
            &format!("0:{}", head.hash),
        ] {
            assert!(parse_anchor(wrong).is_err(), "{wrong}");
        }
    }
}
