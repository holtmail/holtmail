//! A [`SecretBox`] with XChaCha20-Poly1305 and a key kept in a local file.

use std::fs;
use std::io::Write;
use std::path::Path;

use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use holtmail_secret::{SecretBox, SecretError};

const VERSION: u8 = 1;
const NONCE_LEN: usize = 24;

/// Seals with a 256-bit key from a file.
///
/// Sealed bytes are `1 || nonce (24) || ciphertext and tag`; the context is the associated data.
pub struct LocalSecretBox {
    cipher: XChaCha20Poly1305,
}

impl LocalSecretBox {
    /// Reads the key at `path`, creating a random key readable only by the owner when the file
    /// does not exist. Losing the file makes every sealed secret unreadable.
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self, SecretError> {
        let path = path.as_ref();
        if !path.exists() {
            let key = Key::<XChaCha20Poly1305>::generate();
            write_private(path, key.as_slice()).map_err(backend)?;
        }
        let bytes = fs::read(path).map_err(backend)?;
        let cipher = XChaCha20Poly1305::new_from_slice(&bytes).map_err(|_| {
            SecretError::Backend(format!("{} is not a 32-byte key", path.display()).into())
        })?;
        Ok(Self { cipher })
    }

    /// A box with the given key, for tests.
    pub fn from_key(key: [u8; 32]) -> Self {
        Self {
            cipher: XChaCha20Poly1305::new(&key.into()),
        }
    }
}

impl SecretBox for LocalSecretBox {
    fn seal(&self, context: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let nonce = XNonce::generate();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| SecretError::Invalid)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        out.push(VERSION);
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn open(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        let Some((&VERSION, rest)) = sealed.split_first() else {
            return Err(SecretError::Invalid);
        };
        if rest.len() < NONCE_LEN {
            return Err(SecretError::Invalid);
        }
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).map_err(|_| SecretError::Invalid)?;
        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| SecretError::Invalid)
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn backend(error: std::io::Error) -> SecretError {
    SecretError::Backend(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_secret_box_contract() {
        holtmail_secret::testing::conformance(&LocalSecretBox::from_key([7; 32]));
    }

    #[test]
    fn creates_a_private_key_file_and_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys/secret.key");
        let sealed = LocalSecretBox::open_or_create(&path)
            .unwrap()
            .seal("a", b"pw")
            .unwrap();
        assert_eq!(fs::read(&path).unwrap().len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let again = LocalSecretBox::open_or_create(&path).unwrap();
        assert_eq!(again.open("a", &sealed).unwrap(), b"pw");
        assert!(matches!(
            LocalSecretBox::from_key([1; 32]).open("a", &sealed),
            Err(SecretError::Invalid)
        ));
    }

    #[test]
    fn rejects_a_short_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        fs::write(&path, b"short").unwrap();
        assert!(matches!(
            LocalSecretBox::open_or_create(&path),
            Err(SecretError::Backend(_))
        ));
    }
}
