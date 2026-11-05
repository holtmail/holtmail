use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use holtmail_core::{AccountId, ArchiveId, FolderId};
use holtmail_ingest::{Ingest, IngestError, Placement};
use holtmail_store::{AccountKind, FolderRole, NewAccount, Store};

const COMMIT_EVERY: u64 = 500;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub new: u64,
    pub known: u64,
    pub failed: u64,
}

pub async fn find_or_create_archive(store: &dyn Store, name: &str) -> anyhow::Result<ArchiveId> {
    if let Some(archive) = store
        .list_archives()
        .await?
        .into_iter()
        .find(|a| a.name == name)
    {
        return Ok(archive.id);
    }
    Ok(store.create_archive(name).await?.id)
}

pub async fn find_or_create_import_account(
    store: &dyn Store,
    archive: ArchiveId,
    name: &str,
) -> anyhow::Result<AccountId> {
    if let Some(account) = store
        .list_accounts(archive)
        .await?
        .into_iter()
        .find(|a| a.kind == AccountKind::Import && a.name == name)
    {
        return Ok(account.id);
    }
    Ok(store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: name.to_owned(),
            address: None,
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await?
        .id)
}

pub async fn import_path(
    store: &dyn Store,
    ingest: &Ingest,
    archive: ArchiveId,
    account: AccountId,
    path: &Path,
    mut progress: impl FnMut(&Totals),
) -> anyhow::Result<Totals> {
    let messages =
        holtmail_import::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut folders: HashMap<String, FolderId> = HashMap::new();
    let mut totals = Totals::default();
    for message in messages {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "skipped an unreadable message");
                totals.failed += 1;
                continue;
            }
        };
        let folder_path = message.folder.unwrap_or_else(|| "INBOX".to_owned());
        let folder = match folders.get(&folder_path) {
            Some(id) => *id,
            None => {
                let id = store
                    .upsert_folder(account, &folder_path, FolderRole::guess(&folder_path))
                    .await?
                    .id;
                folders.insert(folder_path, id);
                id
            }
        };
        let placement = Placement {
            folder: Some(folder),
            uid: None,
            flags: message.flags,
            labels: None,
            received_at: message.received_at,
        };
        match ingest.ingest(archive, message.raw, &placement).await {
            Ok(done) if done.created => totals.new += 1,
            Ok(_) => totals.known += 1,
            Err(IngestError::Mime(error)) => {
                tracing::warn!(%error, "skipped bytes that are not a message");
                totals.failed += 1;
            }
            Err(error) => return Err(error.into()),
        }
        let seen = totals.new + totals.known + totals.failed;
        if seen % COMMIT_EVERY == 0 {
            ingest.commit().await?;
            progress(&totals);
        }
    }
    ingest.commit().await?;
    progress(&totals);
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use holtmail_blob_fs::FsBlobStore;
    use holtmail_index_tantivy::TantivyIndex;
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    #[test]
    fn guesses_common_roles() {
        assert_eq!(FolderRole::guess("INBOX"), Some(FolderRole::Inbox));
        assert_eq!(
            FolderRole::guess("[Gmail]/Sent Mail"),
            Some(FolderRole::Sent)
        );
        assert_eq!(FolderRole::guess("[Gmail]/All Mail"), Some(FolderRole::All));
        assert_eq!(FolderRole::guess("Отправленные"), Some(FolderRole::Sent));
        assert_eq!(FolderRole::guess("INBOX.Junk"), Some(FolderRole::Junk));
        assert_eq!(FolderRole::guess("Projects/2025"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn imports_a_directory_twice_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mail = dir.path().join("mail");
        std::fs::create_dir_all(mail.join("Sent")).unwrap();
        std::fs::write(
            mail.join("a.eml"),
            b"From: ada@example.com\nSubject: A\n\nOne\n",
        )
        .unwrap();
        std::fs::write(
            mail.join("Sent/b.eml"),
            b"From: bob@example.org\nSubject: B\n\nTwo\n",
        )
        .unwrap();
        std::fs::write(mail.join("broken.eml"), b"not a message at all").unwrap();

        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let ingest = Ingest::new(
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap()),
            store.clone(),
            Arc::new(TantivyIndex::in_memory().unwrap()),
        );
        let archive = find_or_create_archive(&*store, "Personal").await.unwrap();
        let account = find_or_create_import_account(&*store, archive, "mail")
            .await
            .unwrap();

        let first = import_path(&*store, &ingest, archive, account, &mail, |_| {})
            .await
            .unwrap();
        assert_eq!(
            first,
            Totals {
                new: 2,
                known: 0,
                failed: 1
            }
        );
        let second = import_path(&*store, &ingest, archive, account, &mail, |_| {})
            .await
            .unwrap();
        assert_eq!(
            second,
            Totals {
                new: 0,
                known: 2,
                failed: 1
            }
        );

        assert_eq!(
            find_or_create_archive(&*store, "Personal").await.unwrap(),
            archive
        );
        let folders = store.list_folders(archive).await.unwrap();
        let sent = folders.iter().find(|f| f.folder.path == "Sent").unwrap();
        assert_eq!(sent.folder.role, Some(FolderRole::Sent));
        assert_eq!(sent.messages, 1);
    }
}
