//! [`BlobStore`] on S3-compatible object storage.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::{BlobError, BlobId, BlobStore};
use holtmail_core::TlsRoots;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;
use object_store::{Certificate, ClientOptions};
use object_store::{ObjectStore, ObjectStoreExt};

/// Where and how to reach the bucket.
#[derive(Clone)]
pub struct S3Config {
    /// Bucket that holds the blobs.
    pub bucket: String,
    /// Region, e.g. `eu-central-1`; any value for services that ignore it.
    pub region: String,
    /// Endpoint of an S3-compatible service; `None` for AWS.
    pub endpoint: Option<String>,
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Key prefix inside the bucket, so several instances can share one bucket.
    pub prefix: Option<String>,
    /// Use `https://host/bucket` instead of `https://bucket.host`; most self-hosted services need it.
    pub path_style: bool,
    /// Allow a plain `http://` endpoint.
    pub allow_http: bool,
    /// Certificates trusted for an https endpoint besides the system's roots.
    pub roots: TlsRoots,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("allow_http", &self.allow_http)
            .field("roots", &self.roots)
            .finish_non_exhaustive()
    }
}

/// Stores each blob as one object at `<prefix>/ab/cd/abcd…`.
///
/// Objects are written with a single atomic PUT, so a reader never sees a partial blob.
pub struct S3BlobStore {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl S3BlobStore {
    /// Connects to the bucket described by `config`. Does not touch the network.
    pub fn new(config: S3Config) -> Result<Self, BlobError> {
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(config.bucket)
            .with_region(config.region)
            .with_access_key_id(config.access_key_id)
            .with_secret_access_key(config.secret_access_key)
            .with_virtual_hosted_style_request(!config.path_style)
            .with_allow_http(config.allow_http);
        if let Some(endpoint) = config.endpoint {
            builder = builder.with_endpoint(endpoint);
        }
        if !config.roots.is_empty() {
            let mut options = ClientOptions::new();
            for der in config.roots.iter() {
                options =
                    options.with_root_certificate(Certificate::from_der(der).map_err(backend)?);
            }
            builder = builder.with_client_options(options);
        }
        let store = builder.build().map_err(backend)?;
        Ok(Self::from_object_store(
            Arc::new(store),
            config.prefix.as_deref().unwrap_or_default(),
        ))
    }

    /// Uses any `object_store` backend, with keys under `prefix`.
    pub fn from_object_store(store: Arc<dyn ObjectStore>, prefix: &str) -> Self {
        Self {
            store,
            prefix: Path::from(prefix),
        }
    }

    fn path(&self, id: &BlobId) -> Path {
        let hex = id.to_string();
        self.prefix
            .clone()
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex.as_str())
    }
}

#[async_trait]
impl BlobStore for S3BlobStore {
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError> {
        let id = BlobId::of(&bytes);
        if self.contains(&id).await? {
            return Ok(id);
        }
        // Racing writers PUT identical bytes to the same key, so last-writer-wins is harmless and
        // works on services without conditional PUT.
        self.store
            .put(&self.path(&id), bytes.into())
            .await
            .map_err(backend)?;
        Ok(id)
    }

    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError> {
        let result = match self.store.get(&self.path(id)).await {
            Ok(result) => result,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(backend(error)),
        };
        let bytes = result.bytes().await.map_err(backend)?;
        if BlobId::of(&bytes) != *id {
            return Err(BlobError::Corrupt(*id));
        }
        Ok(Some(bytes))
    }

    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError> {
        match self.store.head(&self.path(id)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(backend(error)),
        }
    }

    async fn delete(&self, id: &BlobId) -> Result<(), BlobError> {
        match self.store.delete(&self.path(id)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(backend(error)),
        }
    }
}

fn backend(error: object_store::Error) -> BlobError {
    BlobError::Backend(Box::new(error))
}

#[cfg(test)]
mod tests {
    use object_store::memory::InMemory;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_blob_store_contract() {
        let store = S3BlobStore::from_object_store(Arc::new(InMemory::new()), "archive");
        holtmail_blob::testing::conformance(Arc::new(store)).await;
    }

    #[tokio::test]
    async fn keeps_blobs_under_the_prefix() {
        let memory = Arc::new(InMemory::new());
        let store = S3BlobStore::from_object_store(memory.clone(), "team-a");
        let id = store
            .put(Bytes::from_static(b"Subject: Hi\r\n\r\n"))
            .await
            .unwrap();
        let hex = id.to_string();
        let key = Path::from(format!("team-a/{}/{}/{hex}", &hex[0..2], &hex[2..4]));
        assert!(memory.head(&key).await.is_ok());
    }

    #[tokio::test]
    async fn reports_a_corrupt_blob() {
        let memory = Arc::new(InMemory::new());
        let store = S3BlobStore::from_object_store(memory.clone(), "");
        let id = store
            .put(Bytes::from_static(b"Subject: Hi\r\n\r\n"))
            .await
            .unwrap();
        memory
            .put(&store.path(&id), Bytes::from_static(b"tampered").into())
            .await
            .unwrap();
        assert!(matches!(store.get(&id).await, Err(BlobError::Corrupt(bad)) if bad == id));
    }

    fn config(endpoint: String, roots: TlsRoots) -> S3Config {
        S3Config {
            bucket: "mail".into(),
            region: "us-east-1".into(),
            endpoint: Some(endpoint),
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMIEXAMPLEKEY".into(),
            prefix: None,
            path_style: true,
            allow_http: false,
            roots,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trusts_extra_roots_for_the_endpoint() {
        let ca = holtmail_core::tls::testing::TestCa::new(&["localhost"]);
        let server = ca.serve_https(404, "").await;
        let endpoint = format!("https://localhost:{}", server.port());
        let store = S3BlobStore::new(config(endpoint, ca.roots())).unwrap();
        let id = BlobId::of(b"Subject: Hi\r\n\r\n");
        assert_eq!(store.get(&id).await.unwrap(), None);
        assert!(
            S3BlobStore::new(config(
                "https://localhost".into(),
                TlsRoots::from_der(vec![vec![1, 2, 3]])
            ))
            .is_err()
        );
    }

    #[test]
    fn debug_hides_credentials() {
        let config = S3Config {
            bucket: "mail".into(),
            region: "eu-central-1".into(),
            endpoint: None,
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMIEXAMPLEKEY".into(),
            prefix: None,
            path_style: false,
            allow_http: false,
            roots: TlsRoots::default(),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("AKIDEXAMPLE") && !debug.contains("EXAMPLEKEY"));
    }
}
