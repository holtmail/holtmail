//! Blobs on S3 (MinIO over TLS with the stack's CA, credentials only in the environment):
//! import, sync and read; a backup with the blobs and one without; a restore into an empty
//! directory with blobs on the filesystem; and an object removed from the bucket, named by the
//! backup (written without it, then failing) and refused clearly by the reader.

use std::io::Read;
use std::path::Path;

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD, id};
use holtmail_integration::data::{Message, mbox};
use holtmail_integration::s3::{self, ACCESS_KEY_ID, SECRET_ACCESS_KEY};
use holtmail_integration::services::{DOVECOT, DOVECOT_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use serde_json::Value;

struct OnS3 {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    prefix: String,
    user: String,
    seeded: Vec<Message>,
}

async fn on_s3() -> OnS3 {
    let prefix = unique("s3");
    let holtmail = Holtmail::start(Options {
        extra_config: s3::config(&prefix),
        env: s3::env(),
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("On S3").await;
    OnS3 {
        holtmail,
        api,
        archive,
        prefix,
        user: DOVECOT.user("s3"),
        seeded: Vec::new(),
    }
}

impl OnS3 {
    async fn archive_mail(&mut self, synced: usize, imported: usize) -> String {
        let mut seeder = DOVECOT.seeder(&self.user).await;
        for _ in 0..synced {
            let message = Message::new(&self.user);
            seeder.append("INBOX", &[], None, &message.bytes()).await;
            self.seeded.push(message);
        }
        seeder.logout().await;
        let account = self
            .api
            .add_imap(&self.archive, DOVECOT.account(&self.user))
            .await;
        let listed = self.api.settle(&self.archive, &account).await;
        assert_eq!(listed["status"]["error"], Value::Null, "{listed}");
        if imported > 0 {
            let uploaded: Vec<Message> = (0..imported).map(|_| Message::new(&self.user)).collect();
            let (_, task) = self
                .api
                .import(&self.archive, "older.mbox", mbox(&uploaded))
                .await;
            assert_eq!(task["state"], "done", "{task}");
            self.seeded.extend(uploaded);
        }
        account
    }

    /// `(message id, blob id, subject)` of every message.
    async fn messages(&self) -> Vec<(String, String, String)> {
        self.holtmail
            .query(
                "SELECT id, blob_id, subject FROM messages ORDER BY subject",
                &[],
            )
            .await
            .into_iter()
            .map(|row| (row[0].clone(), row[1].clone(), row[2].clone()))
            .collect()
    }

    fn scanner(&self) -> LogScanner {
        LogScanner::new()
            .secret("the S3 secret key", SECRET_ACCESS_KEY)
            .secret("the IMAP password", DOVECOT_PASSWORD)
            .secret("the user", &self.user)
            .secrets("a subject", self.seeded.iter().map(|m| m.subject.as_str()))
            .secrets("a sender", self.seeded.iter().map(|m| m.from.as_str()))
    }

    fn backup(&self, file: &Path, extra: &[&str]) -> std::process::Output {
        let mut args = vec!["backup", "--output", file.to_str().expect("UTF-8")];
        args.extend_from_slice(extra);
        self.holtmail.command(&args, "")
    }
}

/// The names in a backup and the manifest it starts with.
fn entries(file: &Path) -> (Vec<String>, Value) {
    let mut archive = tar::Archive::new(std::fs::File::open(file).expect("the backup"));
    let mut names = Vec::new();
    let mut manifest = Value::Null;
    for entry in archive.entries().expect("a tar file") {
        let mut entry = entry.expect("an entry");
        let name = entry.path().expect("a name").to_string_lossy().into_owned();
        if name == "manifest.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text).expect("the manifest");
            manifest = serde_json::from_str(&text).expect("JSON");
        }
        names.push(name);
    }
    (names, manifest)
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_backs_up_and_restores_with_blobs_on_s3() {
    require_it!();
    let mut it = on_s3().await;
    it.archive_mail(3, 2).await;

    let messages = it.messages().await;
    assert_eq!(messages.len(), 5);
    let objects = s3::objects(&it.prefix).await;
    for (_, blob, _) in &messages {
        assert!(
            objects.contains(&s3::key(&it.prefix, blob)),
            "blob {blob} is not in the bucket: {objects:?}"
        );
    }
    assert!(
        !it.holtmail.data_dir().join("blobs").exists(),
        "blobs were written to the data directory"
    );
    for message in &it.seeded {
        let (mid, _, _) = messages
            .iter()
            .find(|(_, _, subject)| subject == &message.subject)
            .expect("archived");
        let raw = it.api.raw(mid).await;
        assert_eq!(
            String::from_utf8_lossy(&raw).replace("\r\n", "\n"),
            String::from_utf8_lossy(&message.bytes()).replace("\r\n", "\n"),
            "{}",
            message.subject
        );
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let full = dir.path().join("full.tar");
    let output = it.backup(&full, &["--include-secret-key"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let (names, manifest) = entries(&full);
    let blobs: Vec<&String> = names.iter().filter(|n| n.starts_with("blobs/")).collect();
    assert_eq!(manifest["blobs_included"], true, "{manifest}");
    assert_eq!(manifest["messages"], 5, "{manifest}");
    assert_eq!(
        manifest["blobs"].as_u64(),
        Some(blobs.len() as u64),
        "{manifest}"
    );
    for (_, blob, _) in &messages {
        assert!(
            names.contains(&format!("blobs/{blob}")),
            "{blob} is not in the backup"
        );
    }
    assert!(names.contains(&"secret.key".to_owned()));

    let lean = dir.path().join("lean.tar");
    let output = it.backup(&lean, &["--skip-blobs"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("blobs were left out"),
        "{}",
        stderr(&output)
    );
    let (names, manifest) = entries(&lean);
    assert_eq!(manifest["blobs_included"], false, "{manifest}");
    assert!(names.iter().all(|n| !n.starts_with("blobs/")), "{names:?}");
    assert!(!names.contains(&"secret.key".to_owned()));

    let mut restored = Holtmail::start_default().await;
    restored.stop().await;
    std::fs::remove_dir_all(restored.data_dir()).expect("the new instance's data removed");
    let data_dir = restored.data_dir();
    let output = restored.command(
        &[
            "restore",
            "--input",
            full.to_str().unwrap(),
            "--data-dir",
            data_dir.to_str().unwrap(),
        ],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Search index rebuilt (5 messages)"),
        "{}",
        stderr(&output)
    );
    for (_, blob, _) in &messages {
        let file = data_dir
            .join("blobs")
            .join(&blob[0..2])
            .join(&blob[2..4])
            .join(blob);
        assert!(
            file.is_file(),
            "{} is not on the filesystem",
            file.display()
        );
    }
    restored.start_again().await;
    let api = Api::sign_in(&restored, OWNER_EMAIL, OWNER_PASSWORD).await;
    let listed = api.messages(&it.archive).await;
    assert_eq!(listed.len(), 5);
    for message in &it.seeded {
        let found = listed
            .iter()
            .find(|m| m["subject"] == message.subject.as_str())
            .expect("restored");
        let raw = api.raw(&id(found)).await;
        assert_eq!(
            String::from_utf8_lossy(&raw).replace("\r\n", "\n"),
            String::from_utf8_lossy(&message.bytes()).replace("\r\n", "\n")
        );
        let token = &message.subject[message.subject.len() - 12..];
        let hits = api
            .get(&format!("/api/archives/{}/search?q={token}", it.archive))
            .await
            .ok();
        assert_eq!(hits["total"], 1, "search for {token}: {hits}");
    }
    let accounts = api.accounts(&it.archive).await;
    let imap = accounts
        .iter()
        .find(|a| a["kind"] == "imap")
        .expect("the IMAP account");
    let synced = api.sync(&it.archive, &id(imap)).await;
    assert_eq!(
        synced["status"]["error"],
        Value::Null,
        "the restored password does not open: {synced}"
    );

    let mut lean_restore = Holtmail::start_default().await;
    lean_restore.stop().await;
    std::fs::remove_dir_all(lean_restore.data_dir()).expect("removed");
    let lean_dir = lean_restore.data_dir();
    let output = lean_restore.command(
        &[
            "restore",
            "--input",
            lean.to_str().unwrap(),
            "--data-dir",
            lean_dir.to_str().unwrap(),
            "--skip-reindex",
        ],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("5 blobs the database references are not in the blob store"),
        "{}",
        stderr(&output)
    );

    for logs in [it.holtmail.logs(), restored.logs(), lean_restore.logs()] {
        it.scanner()
            .secret("the S3 access key", ACCESS_KEY_ID)
            .check(&logs);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_object_is_reported_by_the_backup_and_the_reader() {
    require_it!();
    let mut it = on_s3().await;
    it.archive_mail(2, 0).await;
    let messages = it.messages().await;
    let (gone_message, gone_blob, _) = messages[0].clone();
    let (kept_message, _, _) = messages[1].clone();
    s3::delete(&it.prefix, &gone_blob).await;
    assert!(
        !s3::objects(&it.prefix)
            .await
            .contains(&s3::key(&it.prefix, &gone_blob))
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("backup.tar");
    let output = it.backup(&file, &[]);
    assert!(
        !output.status.success(),
        "a backup missing a blob succeeded"
    );
    let reason = stderr(&output);
    assert!(
        reason.contains("1 referenced blobs were missing or damaged")
            && reason.contains(&gone_blob),
        "{reason}"
    );
    assert!(!reason.contains("panicked"), "{reason}");
    assert!(
        !dir.path().join("backup.tar.part").exists(),
        "the partial file was left"
    );
    let (names, manifest) = entries(&file);
    assert_eq!(manifest["blobs"], 2, "{manifest}");
    assert!(!names.contains(&format!("blobs/{gone_blob}")));
    let kept_blob = &messages[1].1;
    assert!(
        names.contains(&format!("blobs/{kept_blob}")),
        "the other blob is missing: {names:?}"
    );

    let (status, body) = it
        .api
        .get_bytes(&format!("/api/messages/{gone_message}/raw"))
        .await;
    assert_eq!(status.as_u16(), 404, "{}", String::from_utf8_lossy(&body));
    assert!(
        String::from_utf8_lossy(&body).contains("message bytes"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    let detail = it.api.get(&format!("/api/messages/{gone_message}")).await;
    assert_eq!(detail.status.as_u16(), 404, "{}", detail.body);
    assert!(!it.api.raw(&kept_message).await.is_empty());
    assert!(
        !it.holtmail.logs().contains("panicked"),
        "{}",
        it.holtmail.logs()
    );
    it.scanner().check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bucket_behind_an_unknown_ca_or_wrong_keys_stores_nothing() {
    require_it!();
    for (trust, secret) in [(false, SECRET_ACCESS_KEY), (true, "not-the-secret-key")] {
        let prefix = unique("s3-refused");
        let holtmail = Holtmail::start(Options {
            trust_stack_ca: trust,
            extra_config: s3::config(&prefix),
            env: vec![
                ("HOLTMAIL_S3_ACCESS_KEY_ID".into(), ACCESS_KEY_ID.into()),
                ("HOLTMAIL_S3_SECRET_ACCESS_KEY".into(), secret.into()),
            ],
            ..Default::default()
        })
        .await;
        let api = Api::setup_owner(&holtmail).await;
        let archive = api.create_archive("Refused").await;
        let user = DOVECOT.user("s3-refused");
        let message = Message::new(&user);
        let mut seeder = DOVECOT.seeder(&user).await;
        seeder.append("INBOX", &[], None, &message.bytes()).await;
        seeder.logout().await;
        let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
        let listed = api.settle(&archive, &account).await;
        assert!(
            listed["status"]["error"].is_string(),
            "a sync stored mail in a bucket it should not reach (trust {trust}): {listed}"
        );
        assert!(api.messages(&archive).await.is_empty());
        assert!(s3::objects(&prefix).await.is_empty());
        LogScanner::new()
            .secret("the S3 secret key", secret)
            .secret("the IMAP password", DOVECOT_PASSWORD)
            .secret("the user", &user)
            .secret("a subject", &message.subject)
            .check(&holtmail.logs());
    }
}
