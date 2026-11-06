//! A 20 000-message archive through the real binary: uploaded as one mbox, exported as mbox and
//! as a zip through the API and the CLI with the peak memory of the process doing the work
//! sampled, the zip checked with `unzip -t` and the mbox read with Python's `mailbox`, the
//! exported mbox imported again without one new blob, and a backup restored into an empty
//! directory (every blob checked against its hash) where search finds what it found before.
//! Times and memory go to the output and `.state/archive-scale.txt`.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD};
use holtmail_integration::data::{Message, mbox};
use holtmail_integration::memory::{Peak, rss_kib};
use holtmail_integration::{Api, Holtmail, LogScanner, require_it};
use reqwest::StatusCode;

const COUNT: usize = 20_000;
const BOUND_MIB: f64 = 300.0;

fn messages() -> Vec<Message> {
    let attachment = base64::engine::general_purpose::STANDARD.encode(
        (0..16_384u32)
            .map(|i| (i * 7 % 251) as u8)
            .collect::<Vec<_>>(),
    );
    let lines: Vec<&str> = attachment
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).expect("base64"))
        .collect();
    (0..COUNT)
        .map(|i| {
            let message = Message::new("archive-scale@example.com");
            let text = format!(
                "Invented message number {i}.\n{}",
                "Some filler text for a message of an ordinary size.\n".repeat(20)
            );
            if i % 25 != 0 {
                return message.body(&text);
            }
            let boundary = "scale-boundary";
            let mut message = message.body(&format!(
                "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{text}\r\n--{boundary}\r\n\
                 Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"data-{i}.bin\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n",
                lines.join("\r\n")
            ));
            message.content_type = format!("multipart/mixed; boundary=\"{boundary}\"");
            message
        })
        .collect()
}

/// Runs a CLI command to completion while sampling its memory: (seconds, peak MiB, stderr).
async fn measured(command: &mut Command) -> (f64, f64, String) {
    let started = Instant::now();
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("holtmail runs");
    let peak = Peak::start(child.id());
    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .expect("joined")
        .expect("holtmail exits");
    let peak = peak.stop().await;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    (started.elapsed().as_secs_f64(), peak, stderr)
}

fn tool(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{program} {args:?}: {text}");
    text
}

fn mbox_count(file: &Path) -> usize {
    tool(
        "python3",
        &[
            "-I",
            "-c",
            "import mailbox, sys\nbox = mailbox.mbox(sys.argv[1], create=False)\nn = 0\nfor m in box:\n    assert m['Message-ID'] and m['Subject'], m.keys()\n    n += 1\nprint(n)",
            file.to_str().expect("UTF-8"),
        ],
    )
    .trim()
    .parse()
    .expect("a count")
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

async fn search_total(api: &Api, archive: &str, word: &str) -> u64 {
    api.get(&format!("/api/archives/{archive}/search?q={word}"))
        .await
        .ok()["total"]
        .as_u64()
        .expect("a total")
}

#[tokio::test(flavor = "multi_thread")]
async fn exports_backs_up_and_restores_twenty_thousand_messages() {
    require_it!();
    let mut report = String::new();
    let generating = Instant::now();
    let messages = messages();
    let upload = mbox(&messages);
    writeln!(
        report,
        "{COUNT} messages ({} with a 16 KiB attachment), mbox {:.1} MiB generated in {:.1} s",
        COUNT / 25,
        mib(upload.len() as u64),
        generating.elapsed().as_secs_f64()
    )
    .unwrap();

    let holtmail = Holtmail::start_default().await;
    let pid = holtmail.pid().expect("running");
    let idle = rss_kib(pid).unwrap_or(0) as f64 / 1024.0;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Large").await;

    let peak = Peak::start(pid);
    let started = Instant::now();
    let (_, task) = api
        .import_within(&archive, "large.mbox", upload, Duration::from_secs(1_800))
        .await;
    let import_peak = peak.stop().await;
    assert_eq!(task["state"], "done", "{task}");
    let stats = api.get(&format!("/api/archives/{archive}")).await.ok();
    assert_eq!(stats["stats"]["messages"], COUNT, "{stats}");
    writeln!(
        report,
        "import through the API: {:.1} s ({:.0} messages/s), serve RSS {idle:.0} MiB idle, {import_peak:.0} MiB peak",
        started.elapsed().as_secs_f64(),
        COUNT as f64 / started.elapsed().as_secs_f64()
    )
    .unwrap();

    let probe = messages[COUNT / 2]
        .subject
        .rsplit('-')
        .next()
        .expect("a token")
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(600);
    while search_total(&api, &archive, "filler").await < COUNT as u64 {
        assert!(Instant::now() < deadline, "the index did not catch up");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(search_total(&api, &archive, &probe).await, 1);

    let dir = tempfile::tempdir().expect("temp dir");
    let mut peaks = Vec::new();
    for format in ["mbox", "zip"] {
        let file = dir.path().join(format!("api.{format}"));
        let before = rss_kib(pid).unwrap_or(0) as f64 / 1024.0;
        let peak = Peak::start(pid);
        let started = Instant::now();
        let (status, written) = api
            .download(
                &format!("/api/archives/{archive}/export?format={format}"),
                &file,
            )
            .await;
        let peak = peak.stop().await;
        assert_eq!(status, StatusCode::OK);
        writeln!(
            report,
            "export {format} through the API: {:.1} MiB in {:.1} s, serve RSS {before:.0} MiB before, {peak:.0} MiB peak",
            mib(written),
            started.elapsed().as_secs_f64()
        )
        .unwrap();
        peaks.push((format!("API {format}"), peak));
    }
    for format in ["mbox", "zip"] {
        let file = dir.path().join(format!("cli.{format}"));
        let (seconds, peak, _) = measured(holtmail.cli().args([
            "export",
            "--archive",
            "Large",
            "--output",
            file.to_str().expect("UTF-8"),
        ]))
        .await;
        writeln!(
            report,
            "export {format} with the CLI while serve runs: {:.1} MiB in {seconds:.1} s, CLI RSS peak {peak:.0} MiB",
            mib(std::fs::metadata(&file).expect("written").len()),
        )
        .unwrap();
        peaks.push((format!("CLI {format}"), peak));
    }

    for which in ["api", "cli"] {
        let zip = dir.path().join(format!("{which}.zip"));
        let tested = tool("unzip", &["-tq", zip.to_str().expect("UTF-8")]);
        assert!(tested.contains("No errors detected"), "{tested}");
        let listed = tool("unzip", &["-Z1", zip.to_str().expect("UTF-8")]);
        assert_eq!(
            listed.lines().filter(|l| l.ends_with(".eml")).count(),
            COUNT,
            "{which}.zip"
        );
        assert_eq!(mbox_count(&dir.path().join(format!("{which}.mbox"))), COUNT);
    }
    assert_eq!(
        std::fs::read(dir.path().join("api.mbox")).expect("read"),
        std::fs::read(dir.path().join("cli.mbox")).expect("read"),
        "the API and the CLI wrote different mbox files"
    );

    let blobs = walk(&holtmail.data_dir().join("blobs")).len();
    let again = std::fs::read(dir.path().join("api.mbox")).expect("read");
    let started = Instant::now();
    let (_, task) = api
        .import_within(&archive, "exported.mbox", again, Duration::from_secs(1_800))
        .await;
    assert_eq!(task["state"], "done", "{task}");
    let summary = task["summary"].as_str().unwrap_or_default().to_owned();
    assert_eq!(
        walk(&holtmail.data_dir().join("blobs")).len(),
        blobs,
        "importing the export stored new blobs: {summary}"
    );
    let stats = api.get(&format!("/api/archives/{archive}")).await.ok();
    assert_eq!(stats["stats"]["messages"], COUNT, "{stats}");
    writeln!(
        report,
        "the exported mbox imported again: {:.1} s, {blobs} blobs before and after ({summary})",
        started.elapsed().as_secs_f64()
    )
    .unwrap();

    let backup = dir.path().join("backup.tar");
    let (seconds, backup_peak, _) = measured(holtmail.cli().args([
        "backup",
        "--include-secret-key",
        "--output",
        backup.to_str().expect("UTF-8"),
    ]))
    .await;
    writeln!(
        report,
        "backup while serve runs: {:.1} MiB in {seconds:.1} s, CLI RSS peak {backup_peak:.0} MiB",
        mib(std::fs::metadata(&backup).expect("written").len())
    )
    .unwrap();
    peaks.push(("backup".into(), backup_peak));

    let mut restored = Holtmail::start_default().await;
    restored.stop().await;
    std::fs::remove_dir_all(restored.data_dir()).expect("removed");
    let data_dir = restored.data_dir();
    let (seconds, restore_peak, stderr) = measured(restored.cli().args([
        "restore",
        "--input",
        backup.to_str().expect("UTF-8"),
        "--data-dir",
        data_dir.to_str().expect("UTF-8"),
    ]))
    .await;
    assert!(
        stderr.contains(&format!("Search index rebuilt ({COUNT} messages)")),
        "{stderr}"
    );
    writeln!(
        report,
        "restore with hash checks and reindex: {seconds:.1} s, CLI RSS peak {restore_peak:.0} MiB"
    )
    .unwrap();
    peaks.push(("restore".into(), restore_peak));

    let mut corrupted = 0;
    for entry in walk(&data_dir.join("blobs")) {
        let bytes = std::fs::read(&entry).expect("a blob");
        let name = entry.file_name().unwrap().to_string_lossy().into_owned();
        if blake3_hex(&bytes) != name {
            corrupted += 1;
        }
    }
    assert_eq!(corrupted, 0, "restored blobs do not match their names");

    restored.start_again().await;
    let restored_api = Api::sign_in(&restored, OWNER_EMAIL, OWNER_PASSWORD).await;
    assert_eq!(search_total(&restored_api, &archive, &probe).await, 1);
    assert_eq!(
        search_total(&restored_api, &archive, "filler").await,
        COUNT as u64
    );
    let restored_stats = restored_api
        .get(&format!("/api/archives/{archive}"))
        .await
        .ok();
    assert_eq!(restored_stats["stats"], stats["stats"]);

    for (what, peak) in &peaks {
        writeln!(report, "peak {what}: {peak:.0} MiB").unwrap();
    }
    eprint!("{report}");
    let _ = std::fs::write(
        holtmail_integration::state_dir().join("archive-scale.txt"),
        &report,
    );
    for (what, peak) in &peaks {
        assert!(
            *peak < BOUND_MIB,
            "{what} peaked at {peak:.0} MiB, over {BOUND_MIB} MiB"
        );
    }
    let scanner = LogScanner::new().secrets(
        "a subject",
        messages.iter().step_by(400).map(|m| m.subject.as_str()),
    );
    scanner.check(&holtmail.logs());
    scanner.check(&restored.logs());
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("a directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn blake3_hex(bytes: &[u8]) -> String {
    holtmail_blob::BlobId::of(bytes).to_string()
}
