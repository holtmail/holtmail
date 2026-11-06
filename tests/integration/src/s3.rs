//! The stack's S3 service (MinIO) over TLS with the stack's CA, and a client to look at and
//! remove objects behind Holtmail's back.

use futures::TryStreamExt;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

/// MinIO's HTTPS port on this machine.
pub const S3_PORT: u16 = 49000;
/// The bucket the stack creates; each test keeps its blobs under its own prefix.
pub const BUCKET: &str = "holtmail-it";
/// The access key.
pub const ACCESS_KEY_ID: &str = "it-minio-user";
/// The secret key; given to Holtmail only through its environment.
pub const SECRET_ACCESS_KEY: &str = "it-minio-password";

/// `https://localhost:49000`.
pub fn endpoint() -> String {
    format!("https://localhost:{S3_PORT}")
}

/// A `[blobs]` table keeping blobs in the stack's bucket under `prefix`.
pub fn config(prefix: &str) -> String {
    format!(
        "\n[blobs]\nkind = \"s3\"\nbucket = \"{BUCKET}\"\nendpoint = \"{}\"\npath_style = true\nprefix = \"{prefix}\"\n",
        endpoint()
    )
}

/// The environment that gives Holtmail the S3 credentials.
pub fn env() -> Vec<(String, String)> {
    vec![
        ("HOLTMAIL_S3_ACCESS_KEY_ID".into(), ACCESS_KEY_ID.into()),
        (
            "HOLTMAIL_S3_SECRET_ACCESS_KEY".into(),
            SECRET_ACCESS_KEY.into(),
        ),
    ]
}

fn store() -> AmazonS3 {
    let pem = std::fs::read(crate::ca_file()).expect("the stack's CA; run `mise run it`");
    let mut options = object_store::ClientOptions::new();
    for certificate in object_store::Certificate::from_pem_bundle(&pem).expect("a PEM CA") {
        options = options.with_root_certificate(certificate);
    }
    AmazonS3Builder::new()
        .with_bucket_name(BUCKET)
        .with_region("us-east-1")
        .with_endpoint(endpoint())
        .with_access_key_id(ACCESS_KEY_ID)
        .with_secret_access_key(SECRET_ACCESS_KEY)
        .with_virtual_hosted_style_request(false)
        .with_client_options(options)
        .build()
        .expect("an S3 client")
}

/// The object key of blob `id` under `prefix`, as `holtmail-blob-s3` lays them out.
pub fn key(prefix: &str, id: &str) -> String {
    format!("{prefix}/{}/{}/{id}", &id[0..2], &id[2..4])
}

/// Every object key under `prefix`.
pub async fn objects(prefix: &str) -> Vec<String> {
    let mut keys: Vec<String> = store()
        .list(Some(&Path::from(prefix)))
        .map_ok(|meta| meta.location.to_string())
        .try_collect()
        .await
        .expect("the bucket lists");
    keys.sort();
    keys
}

/// Removes blob `id` under `prefix`.
pub async fn delete(prefix: &str, id: &str) {
    store()
        .delete(&Path::from(key(prefix, id)))
        .await
        .expect("the object is removed");
}
