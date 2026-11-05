use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use bytes::Bytes;
use holtmail_blob::{BlobError, BlobId, BlobStore};
use holtmail_core::Timestamp;
use holtmail_store::{ArchiveStore, InventoryStore, SettingsStore};
use holtmail_store_sqlite::SqliteStore;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::{self, Config};

const FORMAT: &str = "holtmail-backup";
const VERSION: u32 = 1;
const MANIFEST: &str = "manifest.json";
const DATABASE: &str = "holtmail.db";
const SECRET_KEY: &str = "secret.key";
const BLOBS: &str = "blobs/";
const PAGE: u32 = 1000;

/// The first entry of every backup.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub holtmail: String,
    pub created_at: i64,
    pub archives: u64,
    pub messages: u64,
    /// Referenced blobs at the time of the snapshot.
    pub blobs: u64,
    pub blobs_included: bool,
    pub secret_key_included: bool,
}

pub struct BackupArgs {
    pub output: PathBuf,
    pub skip_blobs: bool,
    pub include_secret_key: bool,
}

enum Entry {
    File {
        name: String,
        path: PathBuf,
        mode: u32,
    },
    Bytes {
        name: String,
        data: Bytes,
        mode: u32,
    },
}

fn header(size: u64, mode: u32) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(mode);
    header.set_mtime(Timestamp::now().as_secs().max(0) as u64);
    header.set_entry_type(tar::EntryType::Regular);
    header
}

fn spawn_writer(
    out: Box<dyn Write + Send>,
) -> (
    mpsc::Sender<Entry>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (tx, mut rx) = mpsc::channel::<Entry>(8);
    let handle = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut builder = tar::Builder::new(out);
        while let Some(entry) = rx.blocking_recv() {
            match entry {
                Entry::File { name, path, mode } => {
                    let file =
                        File::open(&path).with_context(|| format!("reading {}", path.display()))?;
                    let size = file.metadata()?.len();
                    builder.append_data(&mut header(size, mode), &name, file)?;
                }
                Entry::Bytes { name, data, mode } => {
                    builder.append_data(&mut header(data.len() as u64, mode), &name, &data[..])?;
                }
            }
        }
        let mut out = builder.into_inner()?;
        out.flush()?;
        Ok(())
    });
    (tx, handle)
}

async fn count_blobs(store: &SqliteStore) -> anyhow::Result<u64> {
    let mut count = 0u64;
    let mut after: Option<BlobId> = None;
    loop {
        let page = store.referenced_blobs(after.as_ref(), PAGE).await?;
        count += page.len() as u64;
        match page.last() {
            Some(last) if page.len() == PAGE as usize => after = Some(*last),
            _ => return Ok(count),
        }
    }
}

/// Writes a backup of the instance: the manifest, a snapshot of the database, `secret.key` when
/// asked, and every referenced blob. Safe while `serve` runs.
pub async fn backup(config: &Config, args: BackupArgs) -> anyhow::Result<()> {
    let db = config.data_dir.join(DATABASE);
    anyhow::ensure!(db.is_file(), "no database at {}", db.display());
    let scratch = tempfile::Builder::new()
        .prefix(".backup-")
        .tempdir_in(&config.data_dir)
        .with_context(|| {
            format!(
                "creating a scratch directory in {}",
                config.data_dir.display()
            )
        })?;
    let snapshot = scratch.path().join(DATABASE);
    {
        let live = SqliteStore::open(&db).await?;
        live.snapshot(&snapshot).await?;
        live.close().await;
    }
    let store = SqliteStore::open(&snapshot).await?;
    let archives = store.list_archives().await?;
    let mut messages = 0;
    for archive in &archives {
        messages += store.archive_stats(archive.id).await?.messages;
    }
    let blob_count = count_blobs(&store).await?;
    store.close().await;

    let key_path = config.data_dir.join(SECRET_KEY);
    let include_key = args.include_secret_key && key_path.is_file();
    if args.include_secret_key && !include_key {
        eprintln!("warning: there is no {} to include", key_path.display());
    }
    let manifest = Manifest {
        format: FORMAT.into(),
        version: VERSION,
        holtmail: env!("CARGO_PKG_VERSION").into(),
        created_at: Timestamp::now().as_millis(),
        archives: archives.len() as u64,
        messages,
        blobs: blob_count,
        blobs_included: !args.skip_blobs,
        secret_key_included: include_key,
    };

    let to_stdout = args.output == Path::new("-");
    let partial = partial_path(&args.output);
    let out: Box<dyn Write + Send> = if to_stdout {
        Box::new(std::io::BufWriter::new(std::io::stdout()))
    } else {
        anyhow::ensure!(
            !args.output.exists(),
            "{} exists; not overwriting it",
            args.output.display()
        );
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Box::new(std::io::BufWriter::new(
            options
                .open(&partial)
                .with_context(|| format!("creating {}", partial.display()))?,
        ))
    };
    let (tx, writer) = spawn_writer(out);
    let result = send_entries(
        config,
        &tx,
        &manifest,
        &snapshot,
        include_key,
        args.skip_blobs,
    )
    .await;
    drop(tx);
    let written = writer.await.context("the backup writer stopped")?;
    let missing = match result.and_then(|missing| written.map(|()| missing)) {
        Ok(missing) => missing,
        Err(error) => {
            if !to_stdout {
                let _ = std::fs::remove_file(&partial);
            }
            return Err(error);
        }
    };
    if !to_stdout {
        File::open(&partial)?.sync_all()?;
        std::fs::rename(&partial, &args.output)?;
    }
    eprintln!(
        "Backed up {} archives, {} messages{} to {}.",
        manifest.archives,
        manifest.messages,
        if manifest.blobs_included {
            format!(", {} blobs", manifest.blobs - missing.len() as u64)
        } else {
            String::new()
        },
        if to_stdout {
            "standard output".into()
        } else {
            args.output.display().to_string()
        }
    );
    if args.skip_blobs {
        eprintln!(
            "warning: blobs were left out. Back up the blob store (e.g. the S3 bucket) separately; without it the archive has no messages."
        );
    }
    if !include_key {
        eprintln!(
            "warning: secret.key was left out. Without it, mailbox passwords and sign-in tokens in this backup cannot be opened; keep a copy of {} somewhere safe, or pass --include-secret-key and protect the backup like the key.",
            key_path.display()
        );
    }
    if !missing.is_empty() {
        let shown: Vec<String> = missing.iter().take(10).map(BlobId::to_string).collect();
        bail!(
            "{} referenced blobs were missing or damaged and are not in the backup: {}{}",
            missing.len(),
            shown.join(", "),
            if missing.len() > shown.len() {
                ", ..."
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn partial_path(path: &Path) -> PathBuf {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    PathBuf::from(partial)
}

async fn send(tx: &mpsc::Sender<Entry>, entry: Entry) -> anyhow::Result<()> {
    tx.send(entry)
        .await
        .map_err(|_| anyhow::anyhow!("the backup writer stopped"))
}

async fn send_entries(
    config: &Config,
    tx: &mpsc::Sender<Entry>,
    manifest: &Manifest,
    snapshot: &Path,
    include_key: bool,
    skip_blobs: bool,
) -> anyhow::Result<Vec<BlobId>> {
    send(
        tx,
        Entry::Bytes {
            name: MANIFEST.into(),
            data: Bytes::from(serde_json::to_vec_pretty(manifest)?),
            mode: 0o644,
        },
    )
    .await?;
    send(
        tx,
        Entry::File {
            name: DATABASE.into(),
            path: snapshot.to_owned(),
            mode: 0o600,
        },
    )
    .await?;
    if include_key {
        send(
            tx,
            Entry::File {
                name: SECRET_KEY.into(),
                path: config.data_dir.join(SECRET_KEY),
                mode: 0o600,
            },
        )
        .await?;
    }
    let mut missing = Vec::new();
    if skip_blobs {
        return Ok(missing);
    }
    let blobs = config::open_blobs(config).await?;
    let store = SqliteStore::open(snapshot).await?;
    let mut after: Option<BlobId> = None;
    let mut done = 0u64;
    loop {
        let page = store.referenced_blobs(after.as_ref(), PAGE).await?;
        for id in &page {
            match blobs.get(id).await {
                Ok(Some(data)) => {
                    send(
                        tx,
                        Entry::Bytes {
                            name: format!("{BLOBS}{id}"),
                            data,
                            mode: 0o644,
                        },
                    )
                    .await?
                }
                Ok(None) | Err(BlobError::Corrupt(_)) => missing.push(*id),
                Err(error) => return Err(error.into()),
            }
        }
        done += page.len() as u64;
        if done > 0 && done % 10_000 < u64::from(PAGE) {
            eprintln!("  {done} of {} blobs", manifest.blobs);
        }
        match page.last() {
            Some(last) if page.len() == PAGE as usize => after = Some(*last),
            _ => break,
        }
    }
    store.close().await;
    Ok(missing)
}

pub struct RestoreArgs {
    pub input: PathBuf,
    pub skip_reindex: bool,
}

enum Restored {
    Manifest(Manifest),
    Database,
    SecretKey,
    Blob(BlobId, Vec<u8>),
}

fn is_empty_dir(dir: &Path) -> anyhow::Result<bool> {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => Ok(entries.next().is_none()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("reading {}", dir.display())),
    }
}

fn write_new(path: &Path, mode: u32, mut from: impl Read) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    std::io::copy(&mut from, &mut file)?;
    file.sync_all()?;
    Ok(())
}

fn read_backup(
    input: Box<dyn Read + Send>,
    data_dir: PathBuf,
    tx: mpsc::Sender<Restored>,
) -> anyhow::Result<()> {
    let mut archive = tar::Archive::new(input);
    let mut first = true;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        anyhow::ensure!(
            entry.header().entry_type().is_file(),
            "unexpected entry {name:?} in the backup: not a regular file"
        );
        let item = if first {
            anyhow::ensure!(
                name == MANIFEST,
                "not a Holtmail backup: it does not start with {MANIFEST}"
            );
            let manifest: Manifest =
                serde_json::from_reader(&mut entry).context("reading the manifest")?;
            anyhow::ensure!(manifest.format == FORMAT, "not a Holtmail backup");
            anyhow::ensure!(
                manifest.version <= VERSION,
                "the backup is format {} from Holtmail {}; this Holtmail reads format {VERSION} and older",
                manifest.version,
                manifest.holtmail
            );
            Restored::Manifest(manifest)
        } else if name == DATABASE {
            write_new(&data_dir.join(DATABASE), 0o600, &mut entry)?;
            Restored::Database
        } else if name == SECRET_KEY {
            write_new(&data_dir.join(SECRET_KEY), 0o600, &mut entry)?;
            Restored::SecretKey
        } else if let Some(hex) = name.strip_prefix(BLOBS) {
            let id: BlobId = hex
                .parse()
                .with_context(|| format!("unexpected entry {name:?} in the backup"))?;
            let mut data = Vec::with_capacity(entry.size().min(64 << 20) as usize);
            entry.read_to_end(&mut data)?;
            Restored::Blob(id, data)
        } else {
            bail!("unexpected entry {name:?} in the backup");
        };
        first = false;
        if tx.blocking_send(item).is_err() {
            return Ok(());
        }
    }
    anyhow::ensure!(!first, "the backup is empty");
    Ok(())
}

/// Restores a backup into an empty data directory, checking every blob against its hash, and
/// rebuilds the search index unless told not to.
pub async fn restore(config: &Config, args: RestoreArgs) -> anyhow::Result<()> {
    let data_dir = config.data_dir.clone();
    anyhow::ensure!(
        is_empty_dir(&data_dir)?,
        "{} is not empty; restore only into a new or empty directory",
        data_dir.display()
    );
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let result = restore_into(config, args).await;
    if result.is_err() {
        eprintln!(
            "The restore did not finish; remove {} before trying again.",
            data_dir.display()
        );
    }
    result
}

async fn restore_into(config: &Config, args: RestoreArgs) -> anyhow::Result<()> {
    let data_dir = config.data_dir.clone();
    let input: Box<dyn Read + Send> = if args.input == Path::new("-") {
        Box::new(std::io::BufReader::new(std::io::stdin()))
    } else {
        Box::new(std::io::BufReader::new(
            File::open(&args.input).with_context(|| format!("opening {}", args.input.display()))?,
        ))
    };
    let blobs = config::open_blobs(config).await?;
    let (tx, mut rx) = mpsc::channel(4);
    let reader = {
        let data_dir = data_dir.clone();
        tokio::task::spawn_blocking(move || read_backup(input, data_dir, tx))
    };
    let mut manifest = None;
    let mut database = false;
    let mut secret_key = false;
    let mut restored = 0u64;
    let checked = async {
        while let Some(item) = rx.recv().await {
            match item {
                Restored::Manifest(m) => manifest = Some(m),
                Restored::Database => database = true,
                Restored::SecretKey => secret_key = true,
                Restored::Blob(id, data) => {
                    anyhow::ensure!(
                        BlobId::of(&data) == id,
                        "blob {id} in the backup is damaged: its bytes do not match its hash"
                    );
                    let stored = blobs.put(Bytes::from(data)).await?;
                    anyhow::ensure!(stored == id, "blob {id} was stored as {stored}");
                    restored += 1;
                    if restored.is_multiple_of(10_000) {
                        eprintln!("  {restored} blobs");
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    rx.close();
    let read = reader.await.context("the backup reader stopped")?;
    checked?;
    read?;
    let manifest = manifest.context("the backup has no manifest")?;
    anyhow::ensure!(database, "the backup has no {DATABASE}");

    let store = std::sync::Arc::new(SqliteStore::open(data_dir.join(DATABASE)).await?);
    let missing = missing_blobs(&store, &*blobs).await?;
    store
        .set_setting(crate::REINDEX_SETTING, Some("needed"))
        .await?;
    eprintln!(
        "Restored {} archives, {} messages and {restored} blobs into {}.",
        manifest.archives,
        manifest.messages,
        data_dir.display()
    );
    if !secret_key {
        eprintln!(
            "warning: the backup has no secret.key. Copy the original secret.key into {} before starting Holtmail, or sign in to every mailbox again.",
            data_dir.display()
        );
    }
    if missing > 0 {
        if manifest.blobs_included {
            bail!(
                "{missing} blobs the database references are missing from the blob store after the restore"
            );
        }
        eprintln!(
            "warning: {missing} blobs the database references are not in the blob store. The backup was made without blobs: restore the blob store (e.g. the S3 bucket) from its own backup."
        );
    }
    if args.skip_reindex {
        eprintln!("The search index is rebuilt in the background when `holtmail serve` starts.");
        return Ok(());
    }
    let index = std::sync::Arc::new(holtmail_index_tantivy::TantivyIndex::open(
        data_dir.join("index"),
    )?);
    let ingest = holtmail_ingest::Ingest::new(blobs, store.clone(), index).with_extractor(
        std::sync::Arc::new(holtmail_extract_basic::BasicExtractor::new()),
    );
    let done = crate::reindex(&*store, &ingest, crate::Scope::All, |done| {
        eprintln!("  {done} messages indexed")
    })
    .await?;
    eprintln!("Search index rebuilt ({done} messages).");
    Ok(())
}

async fn missing_blobs(store: &SqliteStore, blobs: &dyn BlobStore) -> anyhow::Result<u64> {
    let mut missing = 0;
    let mut after: Option<BlobId> = None;
    loop {
        let page = store.referenced_blobs(after.as_ref(), PAGE).await?;
        for id in &page {
            if !blobs.contains(id).await? {
                missing += 1;
            }
        }
        match page.last() {
            Some(last) if page.len() == PAGE as usize => after = Some(*last),
            _ => return Ok(missing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_empty_dir(dir.path()).unwrap());
        assert!(is_empty_dir(&dir.path().join("missing")).unwrap());
        std::fs::write(dir.path().join("x"), b"x").unwrap();
        assert!(!is_empty_dir(dir.path()).unwrap());
    }

    #[test]
    fn refuses_foreign_and_unsafe_archives() {
        let dir = tempfile::tempdir().unwrap();
        let tar_of = |entries: &[(&str, &[u8])]| {
            let mut builder = tar::Builder::new(Vec::new());
            for (name, data) in entries {
                let mut header = header(data.len() as u64, 0o644);
                builder.append_data(&mut header, name, *data).unwrap();
            }
            builder.into_inner().unwrap()
        };
        let read = |bytes: Vec<u8>| {
            let (tx, mut rx) = mpsc::channel(16);
            let result = read_backup(
                Box::new(std::io::Cursor::new(bytes)),
                dir.path().to_owned(),
                tx,
            );
            rx.close();
            result
        };
        assert!(read(tar_of(&[("notes.txt", b"x")])).is_err());
        let manifest = serde_json::to_vec(&Manifest {
            format: FORMAT.into(),
            version: VERSION,
            holtmail: "0.0.0".into(),
            created_at: 0,
            archives: 0,
            messages: 0,
            blobs: 0,
            blobs_included: true,
            secret_key_included: false,
        })
        .unwrap();
        let error = read(tar_of(&[(MANIFEST, &manifest), ("etc/passwd", b"x")]))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("unexpected entry"), "{error}");
        assert!(read(tar_of(&[(MANIFEST, &manifest), ("blobs/not-hex", b"x")])).is_err());
        let mut newer = serde_json::from_slice::<serde_json::Value>(&manifest).unwrap();
        newer["version"] = (VERSION + 1).into();
        let newer = serde_json::to_vec(&newer).unwrap();
        assert!(read(tar_of(&[(MANIFEST, &newer)])).is_err());
    }
}
