use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Context;
use bytes::Bytes;
use holtmail_core::{AccountId, ArchiveId, FolderId, Timestamp};
use holtmail_import::{CollectionFile, CollectionFormat};
use holtmail_ingest::{IncomingItem, Ingest, IngestError, Placement};
use holtmail_pim::{ItemKind, RawItem};
use holtmail_store::{AccountKind, CollectionKind, FolderRole, NewAccount, NewCollection, Store};

const COMMIT_EVERY: u64 = 500;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub new: u64,
    pub known: u64,
    pub failed: u64,
    pub deleted: u64,
    pub items: ItemTotals,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ItemTotals {
    pub new: u64,
    pub changed: u64,
    pub known: u64,
    pub removed: u64,
    pub unreadable: u64,
}

impl Totals {
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} new, {} already archived, {} failed",
            self.new, self.known, self.failed
        );
        if self.deleted > 0 {
            out.push_str(&format!(", {} deleted earlier left out", self.deleted));
        }
        let items = &self.items;
        if items.new + items.changed + items.known + items.removed > 0 {
            out.push_str(&format!(
                "; contacts and events: {} new, {} changed, {} unchanged, {} gone",
                items.new, items.changed, items.known, items.removed
            ));
            if items.unreadable > 0 {
                out.push_str(&format!(", {} unreadable", items.unreadable));
            }
        }
        out
    }
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
    upload_name: Option<&str>,
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
            Err(IngestError::Deleted) => totals.deleted += 1,
            Err(IngestError::Mime(error)) => {
                tracing::warn!(%error, "skipped bytes that are not a message");
                totals.failed += 1;
            }
            Err(error) => return Err(error.into()),
        }
        let seen = totals.new + totals.known + totals.failed + totals.deleted;
        if seen % COMMIT_EVERY == 0 {
            ingest.commit().await?;
            progress(&totals);
        }
    }
    ingest.commit().await?;
    let mut files = holtmail_import::collections(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let single = matches!(
        holtmail_import::detect(path),
        Some(holtmail_import::Format::VCard | holtmail_import::Format::ICalendar)
    );
    if let (true, Some(name), [file]) = (single, upload_name, files.as_mut_slice()) {
        file.path = name.to_owned();
        file.name = Path::new(name)
            .file_stem()
            .map(|s| s.to_string_lossy().trim().to_owned())
            .unwrap_or_else(|| name.to_owned());
    }
    for file in files {
        import_collection(store, ingest, archive, account, file, &mut totals.items).await?;
        progress(&totals);
    }
    ingest.commit().await?;
    progress(&totals);
    Ok(totals)
}

/// Archives one `.vcf` or `.ics` file as a collection of the import account, named after the
/// calendar's `X-WR-CALNAME` or else the file, and known by the file's path. Each object becomes an
/// item known by its `UID`; an object without one by its name, else by its bytes. Items of the
/// collection missing from a file that holds at least one item are marked removed.
async fn import_collection(
    store: &dyn Store,
    ingest: &Ingest,
    archive: ArchiveId,
    account: AccountId,
    file: CollectionFile,
    totals: &mut ItemTotals,
) -> anyhow::Result<()> {
    let kind = match file.format {
        CollectionFormat::VCard => CollectionKind::Contacts,
        CollectionFormat::ICalendar => CollectionKind::Calendar,
    };
    let name = match file.format {
        CollectionFormat::ICalendar => holtmail_pim::calendar_name(&file.bytes),
        CollectionFormat::VCard => None,
    }
    .unwrap_or_else(|| file.name.clone());
    let collection = store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: Some(account),
            kind,
            remote_id: file.path.clone(),
            name,
            color: None,
        })
        .await?;
    let items = holtmail_pim::split(&file.bytes);
    let seen_at = Timestamp::now();
    let mut seen = HashSet::new();
    for item in &items {
        let remote_id = unique(&mut seen, remote_id(item));
        let done = ingest
            .ingest_item(IncomingItem {
                collection: collection.id,
                remote_id,
                kind: item.kind,
                format: holtmail_pim::Format::of(item.kind),
                raw: Bytes::from(item.raw.clone()),
                etag: None,
                seen_at,
            })
            .await?;
        match (done.created_item, done.new_version) {
            (true, _) => totals.new += 1,
            (false, true) => totals.changed += 1,
            (false, false) => totals.known += 1,
        }
        if done.new_version
            && let Some(summary) = store.get_item(done.item_id).await?
            && summary.index.is_some_and(|i| !i.readable)
        {
            totals.unreadable += 1;
        }
    }
    if !items.is_empty() {
        let gone: Vec<String> = store
            .live_remote_ids(collection.id)
            .await?
            .into_iter()
            .filter(|id| !seen.contains(id))
            .collect();
        totals.removed += store
            .mark_items_removed(collection.id, &gone, seen_at)
            .await?;
    }
    tracing::info!(collection = %collection.id, items = items.len(), "collection imported");
    Ok(())
}

fn remote_id(item: &RawItem) -> String {
    if let Some(uid) = &item.uid {
        return uid.clone();
    }
    if item.kind == ItemKind::Contact
        && let Some(name) = holtmail_pim::normalize(item.kind, &item.raw).title()
    {
        return format!("name:{}", name.trim().to_lowercase());
    }
    format!("blake3:{}", holtmail_blob::BlobId::of(&item.raw))
}

fn unique(seen: &mut HashSet<String>, id: String) -> String {
    let mut candidate = id.clone();
    let mut n = 1;
    while !seen.insert(candidate.clone()) {
        n += 1;
        candidate = format!("{id}#{n}");
    }
    candidate
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

        let first = import_path(&*store, &ingest, archive, account, &mail, None, |_| {})
            .await
            .unwrap();
        assert_eq!(
            first,
            Totals {
                new: 2,
                known: 0,
                failed: 1,
                deleted: 0,
                items: ItemTotals::default(),
            }
        );
        let second = import_path(&*store, &ingest, archive, account, &mail, None, |_| {})
            .await
            .unwrap();
        assert_eq!(
            second,
            Totals {
                new: 0,
                known: 2,
                failed: 1,
                deleted: 0,
                items: ItemTotals::default(),
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

    async fn services(dir: &Path) -> (Arc<dyn Store>, Ingest) {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.join("db")).await.unwrap());
        let ingest = Ingest::new(
            Arc::new(FsBlobStore::open(dir.join("blobs")).await.unwrap()),
            store.clone(),
            Arc::new(TantivyIndex::in_memory().unwrap()),
        );
        (store, ingest)
    }

    const PEOPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada Quill\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Bo Linden\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nUID:cleo\r\nFN:Cleo Marsh\r\nEND:VCARD\r\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn imports_address_books_and_calendars_with_versions() {
        let dir = tempfile::tempdir().unwrap();
        let (store, ingest) = services(dir.path()).await;
        let archive = find_or_create_archive(&*store, "Personal").await.unwrap();
        let account = find_or_create_import_account(&*store, archive, "people.vcf")
            .await
            .unwrap();
        let file = dir.path().join("people.vcf");
        std::fs::write(&file, PEOPLE).unwrap();

        let first = import_path(&*store, &ingest, archive, account, &file, None, |_| {})
            .await
            .unwrap();
        assert_eq!(first.items.new, 3);
        let again = import_path(&*store, &ingest, archive, account, &file, None, |_| {})
            .await
            .unwrap();
        assert_eq!(
            again.items,
            ItemTotals {
                known: 3,
                ..ItemTotals::default()
            },
            "the same file adds nothing"
        );

        std::fs::write(
            &file,
            PEOPLE
                .replace("TEL:+1 555 0100", "TEL:+1 555 0199")
                .replace(
                    "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:cleo\r\nFN:Cleo Marsh\r\nEND:VCARD\r\n",
                    "",
                ),
        )
        .unwrap();
        let changed = import_path(&*store, &ingest, archive, account, &file, None, |_| {})
            .await
            .unwrap();
        assert_eq!(
            changed.items,
            ItemTotals {
                changed: 1,
                known: 1,
                removed: 1,
                ..ItemTotals::default()
            }
        );

        let collections = store.list_collections(archive).await.unwrap();
        assert_eq!(collections.len(), 1);
        let book = &collections[0];
        assert_eq!(book.collection.name, "people");
        assert_eq!(book.collection.kind, CollectionKind::Contacts);
        assert_eq!(book.collection.account_id, Some(account));
        assert_eq!((book.items, book.removed_items), (2, 1));
        let ids = store.live_remote_ids(book.collection.id).await.unwrap();
        assert_eq!(ids, ["ada", "name:bo linden"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn imports_a_takeout_zip() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let (store, ingest) = services(dir.path()).await;
        let archive = find_or_create_archive(&*store, "Takeout").await.unwrap();
        let account = find_or_create_import_account(&*store, archive, "takeout.zip")
            .await
            .unwrap();
        let path = dir.path().join("takeout.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("Takeout/Contacts/My Contacts/My Contacts.vcf", options)
            .unwrap();
        zip.write_all(PEOPLE.as_bytes()).unwrap();
        zip.start_file("Takeout/Calendar/ada@example.com.ics", options)
            .unwrap();
        zip.write_all(b"BEGIN:VCALENDAR\r\nX-WR-CALNAME:Ada Quill\r\nBEGIN:VEVENT\r\nUID:e1\r\nSUMMARY:Review\r\nDTSTART:20250310T090000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:e1\r\nRECURRENCE-ID:20250311T090000Z\r\nSUMMARY:Review\r\nDTSTART:20250311T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
        zip.start_file("Takeout/Mail/All mail.mbox", options)
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let totals = import_path(&*store, &ingest, archive, account, &path, None, |_| {})
            .await
            .unwrap();
        assert_eq!(
            totals.items.new, 4,
            "three cards and one event with its instance"
        );
        let names: Vec<_> = store
            .list_collections(archive)
            .await
            .unwrap()
            .into_iter()
            .map(|c| (c.collection.kind, c.collection.name))
            .collect();
        assert_eq!(
            names,
            [
                (CollectionKind::Calendar, "Ada Quill".to_owned()),
                (CollectionKind::Contacts, "My Contacts".to_owned())
            ]
        );
    }
}
