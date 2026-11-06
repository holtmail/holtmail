//! Peak resident memory of a process, sampled with `ps` every 100 ms.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// The resident set of `pid` in KiB, `None` once it has exited.
pub fn rss_kib(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Samples a process until [`Peak::stop`]; the peak includes the first sample.
pub struct Peak {
    peak: Arc<AtomicU64>,
    sampling: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Peak {
    /// Starts sampling `pid`.
    pub fn start(pid: u32) -> Self {
        let peak = Arc::new(AtomicU64::new(rss_kib(pid).unwrap_or(0)));
        let sampling = Arc::new(AtomicBool::new(true));
        let task = {
            let peak = peak.clone();
            let sampling = sampling.clone();
            tokio::spawn(async move {
                while sampling.load(Ordering::Relaxed) {
                    if let Some(rss) = rss_kib(pid) {
                        peak.fetch_max(rss, Ordering::Relaxed);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
        };
        Self {
            peak,
            sampling,
            task,
        }
    }

    /// Stops sampling and returns the peak in MiB.
    pub async fn stop(self) -> f64 {
        self.sampling.store(false, Ordering::Relaxed);
        let _ = self.task.await;
        self.peak.load(Ordering::Relaxed) as f64 / 1024.0
    }
}
