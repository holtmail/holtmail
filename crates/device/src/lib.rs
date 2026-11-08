//! Contract for signing devices in: an app password becomes a [`Principal`] for one protocol,
//! failures are limited, and revocation reaches live sessions.
//!
//! Only app passwords authenticate here. A user's web password, passkey or single sign-in never
//! does, so a leaked app password is one device, not the account.

#[cfg(feature = "testing")]
pub mod testing;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, UserId};

/// A fresh app password and what a device is stored under so that it verifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSecret {
    /// The app password, handed to the person once and never again.
    pub secret: String,
    /// The hash to store with the device.
    pub hash: [u8; 32],
}

/// Who a verified app password signs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The device's owner.
    pub user: UserId,
    /// The device.
    pub device: DeviceId,
    /// The one archive IMAP and SMTP serve to it. Set whenever the protocol verified needs one,
    /// and only to an archive the user can read now.
    pub mail_archive: Option<ArchiveId>,
    /// The archives JMAP and DAV may see, each one the user can read now, sorted.
    pub archives: Vec<ArchiveId>,
    /// What the device may do. Never adds a right the user lacks.
    pub scopes: BTreeSet<DeviceScope>,
    /// Whether fetching a whole body marks a message read.
    pub implicit_seen: bool,
}

/// Why verifying failed.
#[derive(Debug, thiserror::Error)]
pub enum DeviceAuthError {
    /// The login, the secret, or their pairing is wrong, the device may not use this protocol,
    /// or it lacks what the protocol needs. Which one is deliberately not told.
    #[error("wrong login or app password")]
    Invalid,
    /// Too many failures for this login from this address, or from this address altogether.
    #[error("too many failed sign-ins, try again in {} seconds", .retry_after.as_secs())]
    TooMany {
        /// When the next attempt is allowed; at least one second.
        retry_after: Duration,
    },
    /// The login and secret are right and the device was revoked.
    #[error("the device was revoked")]
    Revoked,
    /// The check could not be made; try again. Counts as no attempt.
    #[error("sign-in is unavailable: {0}")]
    Unavailable(String),
}

/// Verifies app passwords and revokes devices. Safe to share between tasks.
#[async_trait]
pub trait DeviceAuth: Send + Sync {
    /// Verifies `login` (the user's email, in any letter case) and `secret` from the client at
    /// `peer` for `protocol`.
    ///
    /// The limits are checked first: a blocked client gets [`DeviceAuthError::TooMany`] even with
    /// the right secret. Both limits count every protocol together; every
    /// [`DeviceAuthError::Invalid`] is one failure. A success clears the failures of this login
    /// from this address, never the address's own count.
    ///
    /// A secret works only with its own user. The time taken does not depend on whether the
    /// login exists. The device must list `protocol`; IMAP and SMTP need a readable mail archive;
    /// IMAP, JMAP, DAV and the API need the `read` scope, SMTP the `send:mail` scope. Otherwise
    /// [`DeviceAuthError::Invalid`].
    async fn verify(
        &self,
        login: &str,
        secret: &str,
        peer: IpAddr,
        protocol: DeviceProtocol,
    ) -> Result<Principal, DeviceAuthError>;

    /// Revokes the device, for good. Every later [`DeviceAuth::verify`] of its secret answers
    /// [`DeviceAuthError::Revoked`]. True when this call revoked it; false for a device that
    /// was revoked already or does not exist.
    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError>;

    /// Revokes every device of the user, as [`DeviceAuth::revoke`] does, and returns how many
    /// this call revoked. Called before a user is removed, so their live sessions end.
    async fn revoke_user(&self, user: UserId) -> Result<u32, DeviceAuthError>;

    /// Puts the device on the feed without revoking it, so its live sessions end and the client
    /// signs in again under its current rights. Called when a device loses a right. Its secret
    /// keeps working.
    fn end_sessions(&self, device: DeviceId);

    /// A new random app password, never returned twice: `hm_` plus 32 base32 characters. A
    /// device stored with its hash verifies with it.
    fn new_secret(&self) -> Result<NewSecret, DeviceAuthError>;

    /// A feed of the devices revoked, or whose sessions were ended, from now on.
    fn revocations(&self) -> Box<dyn RevocationFeed>;
}

/// The devices whose live sessions must end, since the feed was made.
#[async_trait]
pub trait RevocationFeed: Send {
    /// Waits for the next device. Every revocation and every [`DeviceAuth::end_sessions`] after
    /// the feed was made comes out exactly once, however slowly it is read, within a second.
    /// Dropping the future while it waits loses nothing, so it can sit in a `select!`.
    async fn next(&mut self) -> DeviceId;
}
