use std::collections::BTreeSet;

use async_trait::async_trait;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, Timestamp, UserId};

use crate::{SessionHash, StoreError};

const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// The UTC midnight that starts the day of `at`.
pub fn start_of_day(at: Timestamp) -> Timestamp {
    Timestamp::from_millis(at.as_millis().div_euclid(DAY_MILLIS) * DAY_MILLIS)
}

/// What can be chosen about a device, at creation and later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSettings {
    /// Chosen by the user, shown in lists and audit entries.
    pub name: String,
    /// The one archive IMAP and SMTP serve to it; required with either protocol.
    pub mail_archive: Option<ArchiveId>,
    /// The archives JMAP and DAV may see; `None` means every archive the user can read.
    pub archives: Option<BTreeSet<ArchiveId>>,
    /// The protocols it may sign in over.
    pub protocols: BTreeSet<DeviceProtocol>,
    /// What it may do.
    pub scopes: BTreeSet<DeviceScope>,
    /// Whether fetching a whole body marks a message read.
    pub implicit_seen: bool,
}

/// A device to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDevice {
    /// The owner; a device never acts as anyone else.
    pub user_id: UserId,
    /// What it may do and see.
    pub settings: DeviceSettings,
    /// The BLAKE3 hash of its app password; `None` for a device that signs in another way.
    pub secret_hash: Option<SessionHash>,
    /// When it was created.
    pub at: Timestamp,
}

/// A device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Id; later devices have greater ids.
    pub id: DeviceId,
    /// The owner; never changes.
    pub user_id: UserId,
    /// What it may do and see.
    pub settings: DeviceSettings,
    /// Whether it has an app password. The hash itself never leaves the store.
    pub has_secret: bool,
    /// When it was created.
    pub created_at: Timestamp,
    /// The UTC midnight of the last day it was used; days only, so use is not a tracking log.
    pub last_used_day: Option<Timestamp>,
    /// The protocol of its latest use.
    pub last_protocol: Option<DeviceProtocol>,
    /// When it was revoked. Permanent.
    pub revoked_at: Option<Timestamp>,
    /// When its rights were last raised after it was made; `None` if they never were.
    pub raised_at: Option<Timestamp>,
}

/// Devices and their app password hashes.
#[async_trait]
pub trait DeviceStore: Send + Sync {
    /// Creates a device. `NotFound` for an unknown user, mail archive or archive; `Invalid` when
    /// the protocols need a mail archive and it has none, or another device has the hash.
    async fn create_device(&self, device: NewDevice) -> Result<Device, StoreError>;

    /// The device, revoked or not, or `None`.
    async fn get_device(&self, id: DeviceId) -> Result<Option<Device>, StoreError>;

    /// The device with this app password hash, revoked or not, or `None`. An exact lookup of a
    /// hash, so it takes the same time for any wrong secret.
    async fn find_device_by_secret(&self, hash: &SessionHash)
    -> Result<Option<Device>, StoreError>;

    /// The user's devices, revoked ones too, oldest first.
    async fn list_devices(&self, user: UserId) -> Result<Vec<Device>, StoreError>;

    /// Replaces the settings, and records `raised_at` when one is given; `None` keeps the
    /// earlier value. `NotFound` for an unknown device, mail archive or archive; `Invalid` for a
    /// revoked device, or when the protocols need a mail archive and it has none.
    async fn update_device_settings(
        &self,
        id: DeviceId,
        settings: &DeviceSettings,
        raised_at: Option<Timestamp>,
    ) -> Result<Device, StoreError>;

    /// Records a use at `at`, kept only as its day and protocol. A use on an earlier day than
    /// the recorded one and any use of a revoked device change nothing. `NotFound` for an
    /// unknown device.
    async fn touch_device(
        &self,
        id: DeviceId,
        protocol: DeviceProtocol,
        at: Timestamp,
    ) -> Result<(), StoreError>;

    /// Revokes the device at `at`; true when this call revoked it, false when it already was,
    /// which keeps the first time. Permanent. `NotFound` for an unknown device.
    async fn revoke_device(&self, id: DeviceId, at: Timestamp) -> Result<bool, StoreError>;

    /// Revokes every device of the user that is not revoked yet and returns those, oldest first.
    /// Empty for an unknown user.
    async fn revoke_user_devices(
        &self,
        user: UserId,
        at: Timestamp,
    ) -> Result<Vec<DeviceId>, StoreError>;
}
