//! [`DeviceAuth`] over the metadata store: app passwords stored as BLAKE3 hashes, failures limited
//! in memory, and revocations fanned out to live sessions of this process.
//!
//! Devices must be revoked through [`LocalDeviceAuth`]: a revocation made directly on the store
//! takes effect on the next [`DeviceAuth::verify`] but never reaches the feeds.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use holtmail_auth::Limiter;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, Timestamp, UserId};
use holtmail_device::{DeviceAuth, DeviceAuthError, NewSecret, Principal, RevocationFeed};
use holtmail_store::{Device, InstanceRole, SessionHash, Store, StoreError, User, start_of_day};
use tokio::sync::mpsc;

const SECRET_BYTES: usize = 20;
const SECRET_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const MAX_LOGIN_BYTES: usize = 254;
const ANY_LOGIN: &str = "";

/// A new app password: `hm_` plus 32 base32 characters in lower case, 160 random bits.
pub fn new_app_password() -> Result<String, DeviceAuthError> {
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|_| DeviceAuthError::Unavailable("no randomness available".into()))?;
    let mut secret = String::with_capacity(35);
    secret.push_str("hm_");
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            secret.push(char::from(
                SECRET_ALPHABET[((buffer >> bits) & 0x1f) as usize],
            ));
        }
    }
    Ok(secret)
}

/// The hash an app password is stored as: BLAKE3 of its exact bytes. The same secret always gives
/// the same hash.
pub fn secret_hash(secret: &str) -> SessionHash {
    *blake3::hash(secret.as_bytes()).as_bytes()
}

/// Tunables of [`LocalDeviceAuth`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Failures allowed per client address and login within `failure_window`. Default 10.
    pub max_failures_per_login: u32,
    /// Failures allowed per client address across all logins within `failure_window`. Default 50.
    pub max_failures_per_address: u32,
    /// How long a failure counts. Default 15 minutes.
    pub failure_window: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_failures_per_login: 10,
            max_failures_per_address: 50,
            failure_window: Duration::from_secs(15 * 60),
        }
    }
}

/// [`DeviceAuth`] over a [`Store`]. Safe to share between tasks.
///
/// Failure counts and feeds live in memory and end with the process; devices and revocations are
/// durable. Only one process may serve a store, or revocations would not reach the other's feeds.
pub struct LocalDeviceAuth {
    store: Arc<dyn Store>,
    per_login: Limiter,
    per_address: Limiter,
    feeds: Mutex<Vec<mpsc::UnboundedSender<DeviceId>>>,
}

impl LocalDeviceAuth {
    /// Verifies against `store` with these limits.
    pub fn new(store: Arc<dyn Store>, options: Options) -> Self {
        Self {
            store,
            per_login: Limiter::new(options.max_failures_per_login, options.failure_window),
            per_address: Limiter::new(options.max_failures_per_address, options.failure_window),
            feeds: Mutex::new(Vec::new()),
        }
    }

    fn feeds(&self) -> std::sync::MutexGuard<'_, Vec<mpsc::UnboundedSender<DeviceId>>> {
        self.feeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, revoked: &[DeviceId]) {
        self.feeds()
            .retain(|feed| revoked.iter().all(|id| feed.send(*id).is_ok()));
    }

    async fn readable(&self, user: &User) -> Result<BTreeSet<ArchiveId>, DeviceAuthError> {
        Ok(match user.role {
            InstanceRole::Owner | InstanceRole::Admin => self
                .store
                .list_archives()
                .await
                .map_err(unavailable)?
                .into_iter()
                .map(|archive| archive.id)
                .collect(),
            InstanceRole::Member => self
                .store
                .grants(user.id)
                .await
                .map_err(unavailable)?
                .into_iter()
                .collect(),
        })
    }

    async fn check(
        &self,
        login: &str,
        secret: &str,
        protocol: DeviceProtocol,
    ) -> Result<(Principal, Device), DeviceAuthError> {
        let user = if login.len() > MAX_LOGIN_BYTES {
            None
        } else {
            self.store
                .find_user_by_email(login)
                .await
                .map_err(unavailable)?
        };
        let device = self
            .store
            .find_device_by_secret(&secret_hash(secret))
            .await
            .map_err(unavailable)?;
        let (Some(user), Some(device)) = (user, device) else {
            return Err(DeviceAuthError::Invalid);
        };
        if device.user_id != user.id {
            return Err(DeviceAuthError::Invalid);
        }
        if device.revoked_at.is_some() {
            return Err(DeviceAuthError::Revoked);
        }

        let readable = self.readable(&user).await?;
        let settings = &device.settings;
        let mail_archive = settings.mail_archive.filter(|a| readable.contains(a));
        let needed = match protocol {
            DeviceProtocol::Smtp => DeviceScope::SendMail,
            DeviceProtocol::Imap
            | DeviceProtocol::Jmap
            | DeviceProtocol::Dav
            | DeviceProtocol::Api => DeviceScope::Read,
        };
        if !settings.protocols.contains(&protocol)
            || !settings.scopes.contains(&needed)
            || (protocol.needs_mail_archive() && mail_archive.is_none())
        {
            return Err(DeviceAuthError::Invalid);
        }

        let archives = match &settings.archives {
            None => readable.into_iter().collect(),
            Some(list) => list.intersection(&readable).copied().collect(),
        };
        let principal = Principal {
            user: user.id,
            device: device.id,
            mail_archive,
            archives,
            scopes: settings.scopes.clone(),
            implicit_seen: settings.implicit_seen,
        };
        Ok((principal, device))
    }

    async fn record_use(&self, device: &Device, protocol: DeviceProtocol) {
        let now = Timestamp::now();
        if device.last_used_day == Some(start_of_day(now)) && device.last_protocol == Some(protocol)
        {
            return;
        }
        if self
            .store
            .touch_device(device.id, protocol, now)
            .await
            .is_err()
        {
            tracing::warn!(device = %device.id, "could not record a device's use");
        }
    }
}

fn unavailable(error: StoreError) -> DeviceAuthError {
    DeviceAuthError::Unavailable(error.to_string())
}

fn limiter_key(login: &str) -> String {
    login
        .chars()
        .take(MAX_LOGIN_BYTES)
        .collect::<String>()
        .to_lowercase()
}

struct LocalFeed(mpsc::UnboundedReceiver<DeviceId>);

#[async_trait]
impl RevocationFeed for LocalFeed {
    async fn next(&mut self) -> DeviceId {
        match self.0.recv().await {
            Some(id) => id,
            None => std::future::pending().await,
        }
    }
}

#[async_trait]
impl DeviceAuth for LocalDeviceAuth {
    fn new_secret(&self) -> Result<NewSecret, DeviceAuthError> {
        let secret = new_app_password()?;
        let hash = secret_hash(&secret);
        Ok(NewSecret { secret, hash })
    }

    async fn verify(
        &self,
        login: &str,
        secret: &str,
        peer: IpAddr,
        protocol: DeviceProtocol,
    ) -> Result<Principal, DeviceAuthError> {
        let key = limiter_key(login);
        let now = Instant::now();
        let by_login = self.per_login.blocked(peer, &key, now);
        let by_address = self.per_address.blocked(peer, ANY_LOGIN, now);
        if let Some(retry_after) = by_login.max(by_address) {
            return Err(DeviceAuthError::TooMany { retry_after });
        }
        match self.check(login, secret, protocol).await {
            Ok((principal, device)) => {
                self.per_login.clear(peer, &key);
                self.record_use(&device, protocol).await;
                Ok(principal)
            }
            Err(DeviceAuthError::Invalid) => {
                self.per_login.fail(peer, &key, now);
                self.per_address.fail(peer, ANY_LOGIN, now);
                Err(DeviceAuthError::Invalid)
            }
            Err(other) => Err(other),
        }
    }

    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError> {
        let revoked = match self.store.revoke_device(device, Timestamp::now()).await {
            Ok(revoked) => revoked,
            Err(StoreError::NotFound(_)) => false,
            Err(error) => return Err(unavailable(error)),
        };
        if revoked {
            self.publish(&[device]);
        }
        Ok(revoked)
    }

    async fn revoke_user(&self, user: UserId) -> Result<u32, DeviceAuthError> {
        let revoked = self
            .store
            .revoke_user_devices(user, Timestamp::now())
            .await
            .map_err(unavailable)?;
        self.publish(&revoked);
        Ok(u32::try_from(revoked.len()).unwrap_or(u32::MAX))
    }

    fn end_sessions(&self, device: DeviceId) {
        self.publish(&[device]);
    }

    fn revocations(&self) -> Box<dyn RevocationFeed> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut feeds = self.feeds();
        feeds.retain(|feed| !feed.is_closed());
        feeds.push(sender);
        Box::new(LocalFeed(receiver))
    }
}

#[cfg(test)]
mod tests;
