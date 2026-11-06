# holtmail-blob-s3

A [`BlobStore`](../../blob/docs/README.md) on S3-compatible object storage: AWS S3, MinIO, Garage, Versity, Ceph RGW and others. Built on [`object_store`](https://docs.rs/object_store).

## Usage

```rust
let store = S3BlobStore::new(S3Config {
    bucket: "holtmail".into(),
    region: "eu-central-1".into(),
    endpoint: Some("https://s3.example.com".into()),
    access_key_id,
    secret_access_key,
    prefix: Some("archive".into()),
    path_style: true,
    allow_http: false,
    roots: TlsRoots::default(),
})?;
```

`roots` are CA certificates trusted for an https endpoint besides the system's roots (Holtmail's `tls_ca_file`), for a self-hosted service with a private CA; verification stays on. An unusable certificate fails `new`.

`new` does not touch the network; the first request reports a wrong endpoint or credentials. `S3Config`'s `Debug` leaves the keys out.

`S3BlobStore::from_object_store(store, prefix)` takes any `object_store` backend, which is how the tests run against `InMemory`. One test reads from a TLS server in the process whose test CA is trusted through `roots`.

## In the bucket

Blob `abcd…` is the object `<prefix>/ab/cd/abcd…`. A prefix lets several instances share one bucket.

## Guarantees and limits

- Each blob is one atomic PUT, so readers never see a partial object.
- `put` checks with HEAD first and skips the upload when the blob exists. Concurrent `put`s of the same bytes may both upload; they write identical bytes to the same key, so the result is the same and no conditional PUT support is needed from the service.
- `get` re-hashes the object and fails with `BlobError::Corrupt` when it does not match its key.
- The whole blob is buffered in memory on `put` and `get`. Messages are rarely larger than a few tens of megabytes; multipart upload is left for when attachments are stored separately.
- `path_style` is needed by most self-hosted services; AWS uses virtual-hosted style.
