# holtmail-secret-local

A [`SecretBox`](../../secret/docs/README.md) using XChaCha20-Poly1305 with a 256-bit key in a local file.

## Usage

```rust
let secrets = LocalSecretBox::open_or_create("/var/lib/holtmail/secret.key")?;
```

The server keeps the key at `<data_dir>/secret.key`. On first use a random key is written with mode `0600`.

## Format

`1 || nonce (24 bytes) || ciphertext with tag`. The context is the associated data. The leading version byte leaves room for key rotation.

## Operating it

- **Back the key up separately from the data.** Without it no stored credential can be opened; with it, anyone holding a database copy can open them.
- Replacing the key file makes every account need its password again.
- A key file that is not exactly 32 bytes is an error, never silently replaced.
