//! A first sync of an address book of 2 000 cards on Radicale: time and the peak memory of the
//! `holtmail` process, written to `.state/dav-scale.txt` and the test output, then an unchanged
//! sync and one read in full after a forgotten token.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use holtmail_integration::data::vcard;
use holtmail_integration::dav::RADICALE;
use holtmail_integration::pim::{PimRun, line};
use holtmail_integration::require_it;

const COUNT: usize = 2_000;

fn rss_kib(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_two_thousand_cards() {
    require_it!();
    let user = RADICALE.user("davscale").await;
    let dav = RADICALE.client(&user);
    let homes = RADICALE.homes(&dav).await;
    let cards: String = (0..COUNT)
        .map(|i| {
            vcard(
                &format!("scale-card-{i:04}"),
                &format!("Person{i:04} Example"),
                &format!("person{i:04}@example.org"),
                Some(&format!("+1555{:07}", 1_000_000 + i)),
            )
        })
        .collect();
    let seeding = Instant::now();
    let book = format!("{}big/", homes.contacts);
    dav.put(&book, "text/vcard; charset=utf-8", &cards).await;
    let seeded_in = seeding.elapsed();

    let mut run = PimRun::start("DAV scale").await;
    run.secrets(
        "a contact",
        &[
            "Person0000 Example",
            "person0000@example.org",
            "Person1999 Example",
        ],
    );
    let pid = run.holtmail.pid().expect("running");
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
    let started = Instant::now();
    let account = run.add_dav(&RADICALE, &user, None).await;
    let synced_in = started.elapsed();
    sampling.store(false, Ordering::Relaxed);
    let _ = sampler.await;
    let log = run.log(&account).await;
    let name = format!("{user}/big (new)");
    assert!(
        line(&log, &name).ends_with(&format!(
            "{COUNT} new, 0 changed, 0 unchanged, 0 removed, 0 failed (read in full)"
        )),
        "{log:?}"
    );
    assert_eq!(run.api.items(&run.archive, "contacts").await.len(), COUNT);

    let again = Instant::now();
    let log = run.sync(&account).await;
    let unchanged_in = again.elapsed();
    assert!(
        line(&log, &format!("{user}/big")).ends_with("(unchanged)"),
        "{log:?}"
    );
    run.holtmail.rewrite_collection_tokens("szzzzzz").await;
    let full = Instant::now();
    let log = run.sync(&account).await;
    let full_in = full.elapsed();
    assert!(
        line(&log, &format!("{user}/big")).ends_with(
            "0 new, 0 changed, 0 unchanged, 0 removed, 0 failed (read in full, the server forgot the last sync)"
        ),
        "{log:?}"
    );

    let report = format!(
        "{COUNT} cards: put into Radicale in {:.1} s; account added and first contacts sync in {:.1} s ({:.0} cards/s); unchanged sync {:.1} s; full listing after a forgotten token {:.1} s; holtmail RSS {:.0} MiB idle, {:.0} MiB peak (debug build)\n",
        seeded_in.as_secs_f64(),
        synced_in.as_secs_f64(),
        COUNT as f64 / synced_in.as_secs_f64(),
        unchanged_in.as_secs_f64(),
        full_in.as_secs_f64(),
        idle as f64 / 1024.0,
        peak.load(Ordering::Relaxed) as f64 / 1024.0,
    );
    eprint!("{report}");
    let _ = std::fs::write(
        holtmail_integration::state_dir().join("dav-scale.txt"),
        &report,
    );
    run.check_logs();
}
