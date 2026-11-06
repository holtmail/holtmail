//! A first sync of 5 000 messages in one Dovecot folder: time and the peak memory of the
//! `holtmail` process, written to `.state/imap-scale.txt` and the test output.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use holtmail_integration::data::Message;
use holtmail_integration::services::DOVECOT;
use holtmail_integration::{Api, Holtmail, LogScanner, require_it};
use serde_json::Value;

const COUNT: usize = 5_000;

fn rss_kib(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_five_thousand_messages() {
    require_it!();
    let user = DOVECOT.user("scale");
    let messages: Vec<Message> = (0..COUNT)
        .map(|i| {
            Message::new(&user).body(&format!(
                "Invented message number {i}.\n{}",
                "Some filler text for a message of an ordinary size.\n".repeat(30)
            ))
        })
        .collect();
    let bytes: Vec<Vec<u8>> = messages.iter().map(Message::bytes).collect();
    let total: usize = bytes.iter().map(Vec::len).sum();
    let seeding = Instant::now();
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append_many("INBOX", &bytes).await;
    seeder.logout().await;
    let seeded_in = seeding.elapsed();

    let holtmail = Holtmail::start_default().await;
    let pid = holtmail.pid().expect("running");
    let idle = rss_kib(pid).unwrap_or(0);
    let peak = Arc::new(AtomicU64::new(idle));
    let sampling = Arc::new(AtomicBool::new(true));
    let sampler = {
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
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Scale").await;
    let started = Instant::now();
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api
        .wait_for_run(&archive, &account, 1, Duration::from_secs(900))
        .await;
    let synced_in = started.elapsed();
    sampling.store(false, Ordering::Relaxed);
    let _ = sampler.await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    assert_eq!(settled["status"]["new_messages"], COUNT);
    assert_eq!(api.messages(&archive).await.len(), COUNT);

    let report = format!(
        "{COUNT} messages, {:.1} MiB: seeded in {:.1} s; first sync in {:.1} s ({:.0} messages/s); holtmail RSS {:.0} MiB idle, {:.0} MiB peak (debug build)\n",
        total as f64 / 1_048_576.0,
        seeded_in.as_secs_f64(),
        synced_in.as_secs_f64(),
        COUNT as f64 / synced_in.as_secs_f64(),
        idle as f64 / 1024.0,
        peak.load(Ordering::Relaxed) as f64 / 1024.0,
    );
    eprint!("{report}");
    let _ = std::fs::write(
        holtmail_integration::state_dir().join("imap-scale.txt"),
        &report,
    );
    LogScanner::new()
        .secret("the IMAP password", DOVECOT.password)
        .secret("the IMAP user", &user)
        .secrets(
            "a subject",
            messages.iter().take(50).map(|m| m.subject.as_str()),
        )
        .check(&holtmail.logs());
}
