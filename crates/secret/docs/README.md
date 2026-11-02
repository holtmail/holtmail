# holtmail-secret

The contract for sealing credentials (IMAP passwords, OAuth tokens) before they are stored. The store keeps sealed bytes in `Account.sealed_secret` and never sees the plaintext.

## SecretBox

```rust
pub trait SecretBox: Send + Sync {
    fn seal(&self, context: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError>;
    fn open(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, SecretError>;
}
```

- Authenticated: any change to the sealed bytes makes `open` fail with `SecretError::Invalid`.
- Bound to a context: callers pass the account id, so a sealed password copied to another account cannot be opened there.
- Randomized: sealing the same value twice gives different bytes.

`SecretError::Invalid` for a wrong key, wrong context or tampered bytes; `SecretError::Backend` when the key cannot be loaded.

## Writing an implementation

Call `holtmail_secret::testing::conformance(&my_box)` from a test, with the `testing` feature in `[dev-dependencies]`. Candidates: a key in a KMS or in OpenBao/Vault transit.

## Implementations

- [holtmail-secret-local](../../secret-local/docs/README.md) — a key file next to the data
