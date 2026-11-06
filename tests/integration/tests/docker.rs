//! The CLI inside a running container (`IT_PROFILES=docker`), as an operator would use it with
//! `docker exec`: users added and given a new password, an IMAP account added and changed, a
//! backup written next to the data, and that backup restored with `docker run` into a fresh
//! volume that a new container then serves with the same users and mail. Also `tls_ca_file` in
//! the Linux image: IMAP over TLS (rustls) and CardDAV (reqwest's platform verifier) against
//! servers with certificates from the stack's CA, and the same IMAP server refused without it.

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD};
use holtmail_integration::data::{Message, vcard};
use holtmail_integration::dav::RADICALE;
use holtmail_integration::docker::{Container, DEV_IMAGE, Volume, docker, image_present};
use holtmail_integration::services::{DOVECOT, DOVECOT_PASSWORD};
use holtmail_integration::{Api, LogScanner, profile, require_it, unique};
use serde_json::{Value, json};

const MEMBER: &str = "docker-member@example.com";
const FIRST: &str = "it-docker-member-1";
const SECOND: &str = "it-docker-member-2";

#[track_caller]
fn ok(output: std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_works_through_docker_exec() {
    require_it!();
    if !profile("docker") {
        eprintln!("skipped: needs IT_PROFILES=docker");
        return;
    }
    assert!(image_present(DEV_IMAGE), "it.sh builds {DEV_IMAGE}");
    let volume = Volume::new("holtmail-it-exec");
    let container = Container::start(DEV_IMAGE, &volume, &[("HOLTMAIL_SYNC_INTERVAL", "0")]).await;

    ok(container.exec(
        &[
            "user",
            "add",
            "--email",
            OWNER_EMAIL,
            "--name",
            "Owner Example",
        ],
        &format!("{OWNER_PASSWORD}\n"),
    ));
    ok(container.exec(
        &[
            "user",
            "add",
            "--email",
            MEMBER,
            "--name",
            "Member Example",
            "--role",
            "member",
        ],
        &format!("{FIRST}\n"),
    ));
    let users = ok(container.exec(&["user", "list"], ""));
    assert!(
        users.contains(OWNER_EMAIL) && users.contains("owner"),
        "{users}"
    );
    assert!(
        users.contains(MEMBER) && users.contains("member"),
        "{users}"
    );

    let member = Api::sign_in_url(&container.url, MEMBER, FIRST).await;
    ok(container.exec(
        &["user", "password", "--email", MEMBER],
        &format!("{SECOND}\n"),
    ));
    assert_eq!(
        member.get("/api/session").await.status,
        401,
        "a session outlived a new password"
    );
    let refused = Api::for_url(&container.url)
        .post("/api/login", json!({ "email": MEMBER, "password": FIRST }))
        .await;
    assert_eq!(refused.status, 401);
    Api::sign_in_url(&container.url, MEMBER, SECOND).await;

    let user = DOVECOT.user("docker");
    let message = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &message.bytes()).await;
    seeder.logout().await;
    let refused = container.exec(
        &[
            "account",
            "add-imap",
            "--archive",
            "Docker",
            "--host",
            "dovecot",
            "--port",
            "31143",
            "--security",
            "plain",
            "--username",
            &user,
        ],
        &format!("{DOVECOT_PASSWORD}\n"),
    );
    let reason = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        !refused.status.success() && reason.contains("in use by another process"),
        "add-imap next to serve: {reason}"
    );
    let owner = Api::sign_in_url(&container.url, OWNER_EMAIL, OWNER_PASSWORD).await;
    let archive = owner.create_archive("Docker").await;
    let account = owner
        .add_imap(
            &archive,
            json!({ "host": "dovecot", "port": 31143, "security": "plain", "username": user, "password": DOVECOT_PASSWORD }),
        )
        .await;
    owner.settle(&archive, &account).await;
    let listed = ok(container.exec(&["account", "list"], ""));
    assert!(listed.contains(&account), "{listed}");
    let synced = owner.sync(&archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    assert_eq!(owner.messages(&archive).await.len(), 1);

    ok(container.exec(
        &[
            "account",
            "edit",
            &account,
            "--name",
            "Renamed in Docker",
            "--interval",
            "15",
            "--exclude",
            "Junk",
        ],
        "",
    ));
    let listed = owner.account(&archive, &account).await;
    assert_eq!(listed["name"], "Renamed in Docker", "{listed}");
    assert_eq!(listed["sync_interval_minutes"], 15, "{listed}");
    assert_eq!(listed["connection"]["exclude"], json!(["Junk"]), "{listed}");
    let wrong = container.exec(
        &["account", "edit", &account, "--password-stdin"],
        "it-wrong-password\n",
    );
    assert!(!wrong.status.success(), "a wrong password was stored");
    let synced = owner.sync(&archive, &account).await;
    assert_eq!(
        synced["status"]["error"],
        Value::Null,
        "the stored password changed: {synced}"
    );

    ok(container.exec(
        &[
            "backup",
            "--include-secret-key",
            "--output",
            "/data/backup.tar",
        ],
        "",
    ));
    let dir = tempfile::tempdir().expect("temp dir");
    let backup = dir.path().join("backup.tar");
    container.copy_out("/data/backup.tar", &backup);
    let entries = tar::Archive::new(std::fs::File::open(&backup).expect("the backup"))
        .entries()
        .expect("a tar file")
        .map(|e| {
            e.expect("an entry")
                .path()
                .expect("a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert!(entries.contains(&"holtmail.db".to_owned()), "{entries:?}");
    assert!(entries.contains(&"secret.key".to_owned()), "{entries:?}");
    assert!(
        entries.iter().any(|e| e.starts_with("blobs/")),
        "{entries:?}"
    );

    let into_existing = container.exec(
        &[
            "restore",
            "--input",
            "/data/backup.tar",
            "--data-dir",
            "/data",
        ],
        "",
    );
    assert!(
        !into_existing.status.success(),
        "a restore over the live data directory ran"
    );
    ok(container.exec(
        &[
            "restore",
            "--input",
            "/data/backup.tar",
            "--data-dir",
            "/data/restored",
        ],
        "",
    ));

    let restored = Volume::new("holtmail-it-restored");
    let bytes = std::fs::read(&backup).expect("the backup");
    let mount = format!("{}:/data", restored.name);
    let output = docker(
        &[
            "run",
            "--rm",
            "-i",
            "-v",
            &mount,
            DEV_IMAGE,
            "restore",
            "--input",
            "-",
            "--data-dir",
            "/data",
        ],
        &bytes,
    );
    let restored_log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{restored_log}");
    assert!(
        restored_log.contains("Search index rebuilt (1 messages)"),
        "{restored_log}"
    );

    let second = Container::start(DEV_IMAGE, &restored, &[("HOLTMAIL_SYNC_INTERVAL", "0")]).await;
    let owner = Api::sign_in_url(&second.url, OWNER_EMAIL, OWNER_PASSWORD).await;
    Api::sign_in_url(&second.url, MEMBER, SECOND).await;
    assert_eq!(owner.messages(&archive).await.len(), 1);
    let synced = owner.sync(&archive, &account).await;
    assert_eq!(
        synced["status"]["error"],
        Value::Null,
        "the restored password: {synced}"
    );
    assert_eq!(synced["status"]["new_messages"], 0, "{synced}");

    let scanner = LogScanner::new()
        .secret("the member's first password", FIRST)
        .secret("the member's second password", SECOND)
        .secret("the IMAP password", DOVECOT_PASSWORD)
        .secret("the IMAP user", &user)
        .secret("a subject", &message.subject);
    scanner.check(&container.logs());
    scanner.check(&second.logs());
    scanner.check(&restored_log);
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_ca_file_works_in_the_linux_image() {
    require_it!();
    if !profile("docker") {
        eprintln!("skipped: needs IT_PROFILES=docker");
        return;
    }
    assert!(image_present(DEV_IMAGE), "it.sh builds {DEV_IMAGE}");
    let user = RADICALE.user("docker-tls").await;
    let dav = RADICALE.client(&user);
    let homes = RADICALE.homes(&dav).await;
    let book = dav
        .create_address_book(&homes.contacts, "friends", "Friends")
        .await;
    let uid = unique("tls-card");
    dav.put_card(
        &book,
        &uid,
        &vcard(&uid, "Tess Example", "tess.example@example.org", None),
    )
    .await;
    let message = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &message.bytes()).await;
    seeder.logout().await;
    let imaps = json!({ "host": "dovecot", "port": 31993, "security": "tls", "username": user, "password": DOVECOT_PASSWORD });

    let certs = format!("{}:/certs:ro", holtmail_integration::state_dir().display());
    let volume = Volume::new("holtmail-it-tls");
    let trusting = Container::start_with(
        DEV_IMAGE,
        &volume,
        &[
            ("HOLTMAIL_SYNC_INTERVAL", "0"),
            ("HOLTMAIL_TLS_CA_FILE", "/certs/ca.crt"),
        ],
        &["-v", &certs],
    )
    .await;
    let api = Api::for_url(&trusting.url);
    api.post(
        "/api/setup",
        json!({ "token": trusting.setup_token().expect("a setup link"), "email": OWNER_EMAIL, "name": "Owner Example", "password": OWNER_PASSWORD }),
    )
    .await
    .ok();
    let archive = api.create_archive("TLS").await;
    let account = api.add_imap(&archive, imaps.clone()).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(
        settled["status"]["error"],
        Value::Null,
        "IMAP over TLS: {settled}"
    );
    assert_eq!(api.messages(&archive).await.len(), 1);
    let enabled = api
        .enable_contacts(&account, Some("https://radicale:5232/"))
        .await;
    assert_eq!(enabled.status, 200, "CardDAV over TLS: {}", enabled.body);
    let synced = api.sync_contacts(&archive, &account).await;
    assert_eq!(
        synced["status"]["contacts_calendars"]["error"],
        Value::Null,
        "{synced}"
    );
    let contacts = api.items(&archive, "contacts").await;
    assert_eq!(contacts.len(), 1, "{contacts:?}");
    assert_eq!(contacts[0]["title"], "Tess Example");

    let other = Volume::new("holtmail-it-notls");
    let untrusting = Container::start(DEV_IMAGE, &other, &[("HOLTMAIL_SYNC_INTERVAL", "0")]).await;
    let api = Api::for_url(&untrusting.url);
    api.post(
        "/api/setup",
        json!({ "token": untrusting.setup_token().expect("a setup link"), "email": OWNER_EMAIL, "name": "Owner Example", "password": OWNER_PASSWORD }),
    )
    .await
    .ok();
    let archive = api.create_archive("No CA").await;
    let refused = api.add_imap_account(&archive, imaps).await;
    assert_eq!(
        refused.status, 400,
        "the stack's CA was trusted without tls_ca_file: {}",
        refused.body
    );

    let scanner = LogScanner::new()
        .secret("the IMAP password", DOVECOT_PASSWORD)
        .secret("the user", &user)
        .secret("a subject", &message.subject);
    scanner.check(&trusting.logs());
    scanner.check(&untrusting.logs());
}
