use ed25519_dalek::{Signature, VerifyingKey};
use holtmail_category_data::{DataError, DomainTable, Manifest, ManifestError, data_file};
use sha2::{Digest, Sha256};

/// A public key that may sign data manifests, chosen by the manifest's `key_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReleaseKey {
    /// The id a manifest names in `key_id`, `[a-z0-9-]{1,32}`.
    pub id: &'static str,
    /// The 32-byte ed25519 public key.
    pub public: [u8; 32],
}

/// The key that signs releases now. `None` until the maintainer compiles one in; while both
/// release slots are empty every manifest not signed by `TEST_KEY` is refused with
/// [`UpdateError::NoReleaseKey`].
pub const CURRENT_KEY: Option<ReleaseKey> = None;

/// The key that signs after the next rotation. It ships in a release before it signs anything,
/// so servers already trust it when it starts signing.
pub const NEXT_KEY: Option<ReleaseKey> = None;

/// The test key, id `test`, compiled in only with the `test-key` feature. Its seed is public
/// (`tests/fixtures/test-signing-key.hex`), so a release build never has this feature.
#[cfg(feature = "test-key")]
pub const TEST_KEY: ReleaseKey = ReleaseKey {
    id: "test",
    public: TEST_PUBLIC,
};

#[cfg_attr(not(any(test, feature = "test-key")), allow(dead_code))]
const TEST_PUBLIC: [u8; 32] = [
    199, 67, 215, 157, 219, 98, 40, 11, 119, 182, 223, 74, 57, 26, 29, 185, 63, 195, 75, 55, 168,
    222, 59, 83, 122, 207, 223, 209, 219, 13, 138, 56,
];

/// How far after the check `not_after` may lie, in seconds (60 days).
pub const MAX_VALIDITY_SECONDS: i64 = 60 * 24 * 60 * 60;

/// The length of a manifest signature: the raw 64 bytes of an ed25519 signature.
pub const SIGNATURE_BYTES: usize = 64;

/// The keys a manifest may be signed with: [`CURRENT_KEY`], [`NEXT_KEY`] and, with the
/// `test-key` feature, `TEST_KEY`, in that order.
pub fn keys() -> Vec<ReleaseKey> {
    [CURRENT_KEY, NEXT_KEY, TEST_SLOT]
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(feature = "test-key")]
const TEST_SLOT: Option<ReleaseKey> = Some(TEST_KEY);
#[cfg(not(feature = "test-key"))]
const TEST_SLOT: Option<ReleaseKey> = None;

/// Why a downloaded or stored data release was refused. The message names the rule only,
/// never a value from the files, so it can be logged as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// The signature is not [`SIGNATURE_BYTES`] long.
    #[error("manifest signature is not 64 bytes")]
    SignatureLength,
    /// The manifest names a key that is not compiled in, and no release key is compiled in.
    #[error("no release signing key is compiled in")]
    NoReleaseKey,
    /// The manifest names a key that is not compiled in.
    #[error("manifest is signed with an unknown key")]
    UnknownKey,
    /// The signature does not verify under the named key.
    #[error("manifest signature does not verify")]
    Signature,
    /// The signed manifest breaks a manifest rule.
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    /// `not_after` is before the time of the check.
    #[error("manifest has expired")]
    Expired,
    /// `not_after` is more than [`MAX_VALIDITY_SECONDS`] after the time of the check.
    #[error("manifest expiry is too far ahead")]
    TooFarAhead,
    /// The version is not strictly newer than the loaded one.
    #[error("manifest version is not newer than the loaded one")]
    NotNewer,
    /// The file name is not one of the fixed names.
    #[error("data file name is outside the fixed set")]
    FileName,
    /// The manifest does not list the file.
    #[error("data file is not listed in the manifest")]
    NotListed,
    /// The file's size differs from the manifest.
    #[error("data file size does not match the manifest")]
    Size,
    /// The file's SHA-256 differs from the manifest.
    #[error("data file hash does not match the manifest")]
    Sha256,
    /// The file does not load.
    #[error("data file: {0}")]
    Data(#[from] DataError),
    /// The header's table name is not the one fixed for the file name.
    #[error("data file header names another table")]
    TableName,
    /// The header's version differs from the manifest's.
    #[error("data file header version does not match the manifest")]
    TableVersion,
    /// The number of data lines differs from the manifest.
    #[error("data file row count does not match the manifest")]
    Rows,
}

/// Checks a freshly downloaded manifest and its detached signature.
///
/// In order: the signature is [`SIGNATURE_BYTES`] long; the manifest's `key_id` names one of
/// [`keys`]; the signature verifies with ed25519 in strict mode over exactly `manifest`; the
/// manifest passes [`Manifest::parse`]; `not_after` is not before `now` and not more than
/// [`MAX_VALIDITY_SECONDS`] after it; and its version is strictly newer than `loaded` when one
/// is given (equal, older or a `loaded` that is not a numeric version is
/// [`UpdateError::NotNewer`]). `now` is Unix seconds. Never panics and does no I/O.
pub fn verify_manifest(
    manifest: &[u8],
    signature: &[u8],
    now: i64,
    loaded: Option<&str>,
) -> Result<Manifest, UpdateError> {
    fresh(verify_stored(manifest, signature)?, now, loaded)
}

fn fresh(manifest: Manifest, now: i64, loaded: Option<&str>) -> Result<Manifest, UpdateError> {
    if manifest.not_after < now {
        return Err(UpdateError::Expired);
    }
    if manifest.not_after.saturating_sub(now) > MAX_VALIDITY_SECONDS {
        return Err(UpdateError::TooFarAhead);
    }
    if let Some(loaded) = loaded
        && !holtmail_category_data::is_newer(&manifest.version, loaded)
    {
        return Err(UpdateError::NotNewer);
    }
    Ok(manifest)
}

/// Checks a manifest already on disk when the server starts: the signature, the key and the
/// manifest rules exactly as [`verify_manifest`], but neither the expiry nor the version.
///
/// Expiry and rollback protect a fresh download against a replayed old release. Files on disk
/// were accepted with those checks when they were fetched, and must keep loading after
/// `not_after` passes while updates are off or the server is offline.
pub fn verify_stored(manifest: &[u8], signature: &[u8]) -> Result<Manifest, UpdateError> {
    verify_with(&keys(), manifest, signature)
}

fn verify_with(
    keys: &[ReleaseKey],
    manifest: &[u8],
    signature: &[u8],
) -> Result<Manifest, UpdateError> {
    let signature: [u8; SIGNATURE_BYTES] = signature
        .try_into()
        .map_err(|_| UpdateError::SignatureLength)?;
    let key_id = Manifest::peek_key_id(manifest)?;
    let key = keys.iter().find(|key| key.id == key_id).ok_or(
        if CURRENT_KEY.is_none() && NEXT_KEY.is_none() {
            UpdateError::NoReleaseKey
        } else {
            UpdateError::UnknownKey
        },
    )?;
    let verifying = VerifyingKey::from_bytes(&key.public).map_err(|_| UpdateError::Signature)?;
    verifying
        .verify_strict(manifest, &Signature::from_bytes(&signature))
        .map_err(|_| UpdateError::Signature)?;
    Ok(Manifest::parse(manifest)?)
}

/// Checks one data file against a verified manifest and loads it.
///
/// `name` must be one of the fixed file names and listed in `manifest`; `bytes` must have the
/// listed size and SHA-256; the file must load under the default caps of
/// `holtmail-category-data` (32 MiB decompressed, 500 000 rows); its header must carry the
/// table name fixed for `name` and the manifest's version; and its data lines must number the
/// listed `rows`. The size is compared before anything is hashed or decompressed. Never panics
/// and does no I/O.
pub fn check_file(
    manifest: &Manifest,
    name: &str,
    bytes: &[u8],
) -> Result<DomainTable, UpdateError> {
    let entry = data_file(name).ok_or(UpdateError::FileName)?;
    let listed = manifest.file(name).ok_or(UpdateError::NotListed)?;
    if bytes.len() as u64 != listed.size {
        return Err(UpdateError::Size);
    }
    if sha256_hex(bytes) != listed.sha256 {
        return Err(UpdateError::Sha256);
    }
    let table = holtmail_category_data::parse(bytes)?;
    if table.name() != entry.table {
        return Err(UpdateError::TableName);
    }
    if table.version() != manifest.version {
        return Err(UpdateError::TableVersion);
    }
    if (table.len() + table.skipped()) as u64 != listed.rows {
        return Err(UpdateError::Rows);
    }
    Ok(table)
}

/// The lowercase hex SHA-256 of `bytes`, as a manifest lists it.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
