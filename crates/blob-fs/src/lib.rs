//! [`BlobStore`] on a local filesystem.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::{BlobError, BlobId, BlobStore};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Stores each blob as one file under a root directory.
///
/// Blob `abcd…` lives at `<root>/ab/cd/abcd…`. Writes go to `<root>/tmp` first and are renamed into
/// place after `fsync`, so a crash never leaves a partial blob under its id. `<root>` and `<root>/tmp`
/// must be on the same filesystem.
pub struct FsBlobStore {
    root: PathBuf,
    next_temp: AtomicU64,
}

impl FsBlobStore {
    /// Opens the store at `root`, creating the directory when it does not exist.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, BlobError> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))
            .await
            .map_err(backend)?;
        Ok(Self {
            root,
            next_temp: AtomicU64::new(0),
        })
    }

    fn path(&self, id: &BlobId) -> PathBuf {
        let hex = id.to_string();
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(hex)
    }

    fn temp_path(&self, id: &BlobId) -> PathBuf {
        let n = self.next_temp.fetch_add(1, Ordering::Relaxed);
        self.root
            .join("tmp")
            .join(format!("{id}.{}.{n}", std::process::id()))
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError> {
        let id = BlobId::of(&bytes);
        let path = self.path(&id);
        if exists(&path).await? {
            return Ok(id);
        }
        let temp = self.temp_path(&id);
        let written = write_synced(&temp, &bytes).await;
        let moved = match written {
            Ok(()) => move_into_place(&temp, &path).await,
            Err(error) => Err(error),
        };
        if moved.is_err() {
            let _ = fs::remove_file(&temp).await;
        }
        moved.map_err(backend)?;
        Ok(id)
    }

    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError> {
        match fs::read(self.path(id)).await {
            Ok(bytes) if BlobId::of(&bytes) == *id => Ok(Some(bytes.into())),
            Ok(_) => Err(BlobError::Corrupt(*id)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(backend(error)),
        }
    }

    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError> {
        exists(&self.path(id)).await
    }

    async fn delete(&self, id: &BlobId) -> Result<(), BlobError> {
        match fs::remove_file(self.path(id)).await {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(backend(error)),
            _ => Ok(()),
        }
    }
}

async fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(path).await?;
    file.write_all(bytes).await?;
    file.sync_all().await
}

async fn move_into_place(temp: &Path, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::rename(temp, path).await
}

async fn exists(path: &Path) -> Result<bool, BlobError> {
    fs::try_exists(path).await.map_err(backend)
}

fn backend(error: std::io::Error) -> BlobError {
    BlobError::Backend(Box::new(error))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_blob_store_contract() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsBlobStore::open(dir.path()).await.unwrap();
        holtmail_blob::testing::conformance(Arc::new(store)).await;
    }

    #[tokio::test]
    async fn reports_a_corrupt_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsBlobStore::open(dir.path()).await.unwrap();
        let id = store
            .put(Bytes::from_static(b"Subject: Hi\r\n\r\n"))
            .await
            .unwrap();
        std::fs::write(store.path(&id), b"tampered").unwrap();
        assert!(matches!(store.get(&id).await, Err(BlobError::Corrupt(bad)) if bad == id));
    }
}
