//! Contract for sealing credentials before they are stored.
//!
//! Account passwords and OAuth tokens are sealed by a [`SecretBox`] and stored as opaque bytes. A
//! sealed value is bound to a context, such as the account id, so it cannot be moved to another
//! account and opened there.

/// Why sealing or opening failed.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The sealed bytes were changed, belong to another context or were sealed with another key.
    #[error("sealed value cannot be opened: wrong key, wrong context or tampered")]
    Invalid,
    /// The backend failed, e.g. the key could not be read.
    #[error("secret backend failed: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Authenticated encryption of small secrets.
pub trait SecretBox: Send + Sync {
    /// Seals `plaintext` for `context`. Sealing the same value twice gives different bytes.
    fn seal(&self, context: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError>;

    /// Opens what [`SecretBox::seal`] returned for the same `context`; fails with
    /// [`SecretError::Invalid`] for any other context or any change to the bytes.
    fn open(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, SecretError>;
}

/// Conformance suite every [`SecretBox`] implementation runs in its own tests.
#[cfg(feature = "testing")]
pub mod testing {
    use super::*;

    /// Checks that `secrets` keeps the [`SecretBox`] contract. Panics on the first violation.
    pub fn conformance(secrets: &dyn SecretBox) {
        let sealed = secrets.seal("account-1", b"correct horse").unwrap();
        assert_ne!(sealed, b"correct horse", "the plaintext was stored");
        assert_eq!(
            secrets.open("account-1", &sealed).unwrap(),
            b"correct horse"
        );
        assert_ne!(
            secrets.seal("account-1", b"correct horse").unwrap(),
            sealed,
            "sealing is deterministic"
        );
        assert!(matches!(
            secrets.open("account-2", &sealed),
            Err(SecretError::Invalid)
        ));
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(matches!(
            secrets.open("account-1", &tampered),
            Err(SecretError::Invalid)
        ));
        assert!(matches!(
            secrets.open("account-1", b""),
            Err(SecretError::Invalid)
        ));
        assert_eq!(
            secrets.open("x", &secrets.seal("x", b"").unwrap()).unwrap(),
            b""
        );
    }
}
