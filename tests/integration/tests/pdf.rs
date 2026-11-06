//! PDF export of a synced archive, read by real PDF tools (qpdf and poppler) in a container:
//! every PDF is valid, its text is there in Cyrillic too, and the original is inside.

use std::process::Command;

use holtmail_integration::data::Message;
use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{DOVECOT_IMAP, DOVECOT_IMAPS, DOVECOT_PASSWORD, local};
use holtmail_integration::{Api, Holtmail, require_it, state_dir, unique, workspace_root};
use serde_json::json;

fn tool(args: &[&str]) -> (bool, Vec<u8>, String) {
    let output = Command::new("docker")
        .args(["compose", "-f"])
        .arg(workspace_root().join("tests/integration/compose.yaml"))
        .args(["exec", "-T", "pdftools"])
        .args(args)
        .output()
        .expect("docker runs");
    (
        output.status.success() || output.status.code() == Some(3),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn real_pdf_tools_read_the_export() {
    require_it!();
    let user = format!("{}@example.com", unique("pdf"));
    let latin = Message::new(&user).body("Harbour figures for March.\r\nSecond line.");
    let cyrillic = Message::new(&user).body("Квартальный отчёт по гавани готов.\r\nВторая строка.");
    let mut seeder = Seeder::login(&local(DOVECOT_IMAP), &user, DOVECOT_PASSWORD).await;
    for message in [&latin, &cyrillic] {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    seeder.logout().await;

    let holtmail = Holtmail::start_default().await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Papers").await;
    let account = api
        .add_imap(
            &archive,
            json!({
                "host": "localhost",
                "port": DOVECOT_IMAPS,
                "security": "tls",
                "username": user,
                "password": DOVECOT_PASSWORD,
            }),
        )
        .await;
    api.settle(&archive, &account).await;

    let run = unique("export");
    let dir = state_dir().join("pdf").join(&run);
    std::fs::create_dir_all(&dir).unwrap();
    let zip_path = dir.join("all.zip");
    let (status, _) = api
        .download(
            &format!("/api/archives/{archive}/export?format=pdf"),
            &zip_path,
        )
        .await;
    assert!(status.is_success());
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
    assert_eq!(zip.len(), 2);
    zip.extract(&dir).unwrap();

    let listed = api.messages(&archive).await;
    let single = listed
        .iter()
        .find(|m| m["subject"] == cyrillic.subject.as_str())
        .unwrap();
    let id = single["id"].as_str().unwrap();
    let (status, bytes) = api.get_bytes(&format!("/api/messages/{id}/pdf")).await;
    assert!(status.is_success());
    std::fs::write(dir.join("single.pdf"), &bytes).unwrap();

    let mut texts = String::new();
    let mut pdfs: Vec<_> = walk(&dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .collect();
    pdfs.sort();
    assert_eq!(pdfs.len(), 3);
    for pdf in &pdfs {
        let inside = format!("/pdf/{run}/{}", pdf.strip_prefix(&dir).unwrap().display());
        let (ok, _, err) = tool(&["qpdf", "--check", &inside]);
        assert!(ok, "qpdf refused {inside}: {err}");
        let (ok, text, err) = tool(&["pdftotext", "-enc", "UTF-8", &inside, "-"]);
        assert!(ok, "{err}");
        texts.push_str(&String::from_utf8_lossy(&text));
        let (ok, original, err) = tool(&["qpdf", "--show-attachment=message.eml", &inside]);
        assert!(ok, "{err}");
        assert!(
            original == latin.bytes() || original == cyrillic.bytes(),
            "the original inside {inside} is not the message"
        );
    }
    for words in [
        "Harbour figures for March.",
        "Квартальный отчёт по гавани готов.",
        latin.subject.as_str(),
        "1 of 1",
    ] {
        assert!(texts.contains(words), "{words} not in the text:\n{texts}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn walk(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    if path.is_dir() {
        std::fs::read_dir(path)
            .unwrap()
            .flat_map(|e| walk(&e.unwrap().path()))
            .collect()
    } else {
        vec![path.to_path_buf()]
    }
}
