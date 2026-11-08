//! Conformance suite every [`DeviceAuth`] implementation runs, and an in-memory fake.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, UserId};
use tokio::sync::mpsc;

use crate::{DeviceAuth, DeviceAuthError, Principal, RevocationFeed};

/// A device for the suite to set up.
#[derive(Debug, Clone)]
pub struct DeviceSpec {
    /// The email of a user made with [`Harness::user`].
    pub email: String,
    /// Its mail archive.
    pub mail_archive: Option<ArchiveId>,
    /// The archives it may see; `None` for every one the user can read.
    pub archives: Option<Vec<ArchiveId>>,
    /// The protocols it may use.
    pub protocols: Vec<DeviceProtocol>,
    /// What it may do.
    pub scopes: Vec<DeviceScope>,
}

/// A device that was set up, with its app password.
#[derive(Debug, Clone)]
pub struct Enrolled {
    /// The device.
    pub device: DeviceId,
    /// Its owner.
    pub user: UserId,
    /// The app password.
    pub secret: String,
}

/// How many failures an implementation under test allows. `per_address` must be at least four
/// times `per_login`, and `per_login` at least 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Failures per client address and login.
    pub per_login: u32,
    /// Failures per client address across all logins.
    pub per_address: u32,
}

/// Sets up what the suite needs.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Makes an archive.
    async fn archive(&self) -> ArchiveId;
    /// Makes a user who can read `readable` and signs in to the web app with `web_password`.
    async fn user(&self, email: &str, web_password: &str, readable: &[ArchiveId]);
    /// Makes a device.
    async fn enroll(&self, spec: DeviceSpec) -> Enrolled;
    /// The limits in force.
    fn limits(&self) -> Limits;
}

fn peer(block: u8, host: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(198, 51, block, host))
}

fn jmap(email: &str, mail: Option<ArchiveId>) -> DeviceSpec {
    DeviceSpec {
        email: email.into(),
        mail_archive: mail,
        archives: None,
        protocols: vec![DeviceProtocol::Jmap],
        scopes: vec![DeviceScope::Read],
    }
}

fn assert_invalid(result: Result<Principal, DeviceAuthError>, what: &str) {
    assert!(
        matches!(result, Err(DeviceAuthError::Invalid)),
        "{what}: {result:?}"
    );
}

fn assert_too_many(result: Result<Principal, DeviceAuthError>, what: &str) {
    match result {
        Err(DeviceAuthError::TooMany { retry_after }) => {
            assert!(retry_after >= Duration::from_secs(1), "{what}")
        }
        other => panic!("{what}: {other:?}"),
    }
}

/// Checks that `auth` keeps the [`DeviceAuth`] contract. Panics on the first violation.
///
/// `auth` and `harness` must work on the same state, and start with no users.
pub async fn conformance(auth: Arc<dyn DeviceAuth>, harness: &dyn Harness) {
    let limits = harness.limits();
    assert!(limits.per_login >= 2 && limits.per_address >= 4 * limits.per_login);
    principal(&*auth, harness).await;
    wrong_credentials(&*auth, harness).await;
    protocols_and_scopes(&*auth, harness).await;
    login_lockout(&*auth, harness, limits).await;
    address_lockout(&*auth, harness, limits).await;
    revocation(&*auth, harness).await;
    timing(&*auth, harness).await;
    secrets(&*auth);
}

fn secrets(auth: &dyn DeviceAuth) {
    let one = auth.new_secret().unwrap();
    let other = auth.new_secret().unwrap();
    assert_ne!(one.secret, other.secret, "a secret was handed out twice");
    assert_ne!(one.hash, other.hash);
    assert_eq!(one.secret.len(), 35, "hm_ and 32 characters");
    assert!(one.secret.starts_with("hm_"));
}

async fn principal(auth: &dyn DeviceAuth, harness: &dyn Harness) {
    let readable = harness.archive().await;
    let second = harness.archive().await;
    let unreadable = harness.archive().await;
    harness
        .user("ada@example.com", "web password", &[readable, second])
        .await;
    let mut expected_all = vec![readable, second];
    expected_all.sort();

    let all = harness
        .enroll(DeviceSpec {
            protocols: vec![DeviceProtocol::Imap, DeviceProtocol::Jmap],
            scopes: vec![DeviceScope::Read, DeviceScope::WriteMail],
            ..jmap("ada@example.com", Some(readable))
        })
        .await;
    let got = auth
        .verify(
            "ada@example.com",
            &all.secret,
            peer(0, 1),
            DeviceProtocol::Imap,
        )
        .await
        .unwrap();
    assert_eq!(got.user, all.user);
    assert_eq!(got.device, all.device);
    assert_eq!(got.mail_archive, Some(readable));
    assert_eq!(got.archives, expected_all, "every readable archive, sorted");
    assert_eq!(
        got.scopes,
        BTreeSet::from([DeviceScope::Read, DeviceScope::WriteMail])
    );
    let upper = auth
        .verify(
            "ADA@Example.COM",
            &all.secret,
            peer(0, 1),
            DeviceProtocol::Jmap,
        )
        .await
        .unwrap();
    assert_eq!(upper.device, all.device, "the login ignores letter case");

    let limited = harness
        .enroll(DeviceSpec {
            archives: Some(vec![readable, unreadable]),
            ..jmap("ada@example.com", None)
        })
        .await;
    let got = auth
        .verify(
            "ada@example.com",
            &limited.secret,
            peer(0, 1),
            DeviceProtocol::Jmap,
        )
        .await
        .unwrap();
    assert_eq!(
        got.archives,
        vec![readable],
        "a device never sees more than its user"
    );
    assert_eq!(got.mail_archive, None);
}

async fn wrong_credentials(auth: &dyn DeviceAuth, harness: &dyn Harness) {
    harness
        .user("wrong-a@example.com", "correct horse", &[])
        .await;
    harness.user("wrong-b@example.com", "battery", &[]).await;
    let a = harness.enroll(jmap("wrong-a@example.com", None)).await;
    let oversized = "x".repeat(1 << 20);
    let cases: [(&str, &str, &str); 6] = [
        ("wrong-a@example.com", "hm_wrong", "a wrong secret"),
        ("wrong-a@example.com", "", "an empty secret"),
        ("wrong-a@example.com", &oversized, "an oversized secret"),
        ("nobody@example.com", &a.secret, "an unknown login"),
        ("wrong-b@example.com", &a.secret, "another user's secret"),
        ("wrong-a@example.com", "correct horse", "the web password"),
    ];
    for (host, (login, secret, what)) in cases.into_iter().enumerate() {
        assert_invalid(
            auth.verify(login, secret, peer(1, host as u8 + 1), DeviceProtocol::Jmap)
                .await,
            what,
        );
    }
}

async fn protocols_and_scopes(auth: &dyn DeviceAuth, harness: &dyn Harness) {
    let archive = harness.archive().await;
    let elsewhere = harness.archive().await;
    harness
        .user("proto@example.com", "web password", &[archive])
        .await;
    let login = "proto@example.com";
    let check = async |spec: DeviceSpec, protocol, host: u8| {
        let device = harness.enroll(spec).await;
        auth.verify(login, &device.secret, peer(2, host), protocol)
            .await
    };

    assert_invalid(
        check(jmap(login, Some(archive)), DeviceProtocol::Imap, 1).await,
        "a protocol the device does not list",
    );
    let smtp = |scopes: Vec<DeviceScope>| DeviceSpec {
        protocols: vec![DeviceProtocol::Smtp],
        scopes,
        ..jmap(login, Some(archive))
    };
    assert_invalid(
        check(smtp(vec![DeviceScope::Read]), DeviceProtocol::Smtp, 2).await,
        "SMTP without send:mail",
    );
    check(smtp(vec![DeviceScope::SendMail]), DeviceProtocol::Smtp, 3)
        .await
        .expect("SMTP with send:mail");
    assert_invalid(
        check(
            DeviceSpec {
                protocols: vec![DeviceProtocol::Imap],
                scopes: vec![DeviceScope::SendMail],
                ..jmap(login, Some(archive))
            },
            DeviceProtocol::Imap,
            4,
        )
        .await,
        "IMAP without read",
    );
    assert_invalid(
        check(
            DeviceSpec {
                protocols: vec![DeviceProtocol::Imap],
                ..jmap(login, Some(elsewhere))
            },
            DeviceProtocol::Imap,
            5,
        )
        .await,
        "a mail archive the user cannot read",
    );
}

async fn login_lockout(auth: &dyn DeviceAuth, harness: &dyn Harness, limits: Limits) {
    harness.user("lock-a@example.com", "web", &[]).await;
    harness.user("lock-b@example.com", "web", &[]).await;
    harness.user("lock-c@example.com", "web", &[]).await;
    let a = harness.enroll(jmap("lock-a@example.com", None)).await;
    let b = harness.enroll(jmap("lock-b@example.com", None)).await;
    let c = harness.enroll(jmap("lock-c@example.com", None)).await;
    let attacker = peer(3, 1);
    let fail = async |login: &str, from: IpAddr, times: u32| {
        for _ in 0..times {
            assert_invalid(
                auth.verify(login, "hm_wrong", from, DeviceProtocol::Jmap)
                    .await,
                "a wrong secret",
            );
        }
    };

    fail("lock-a@example.com", attacker, limits.per_login).await;
    assert_too_many(
        auth.verify(
            "lock-a@example.com",
            &a.secret,
            attacker,
            DeviceProtocol::Jmap,
        )
        .await,
        "the right secret from a blocked pair",
    );
    assert_too_many(
        auth.verify(
            "lock-a@example.com",
            &a.secret,
            attacker,
            DeviceProtocol::Imap,
        )
        .await,
        "every protocol counts together",
    );
    auth.verify(
        "lock-a@example.com",
        &a.secret,
        peer(3, 2),
        DeviceProtocol::Jmap,
    )
    .await
    .expect("another address is not blocked");
    auth.verify(
        "lock-b@example.com",
        &b.secret,
        attacker,
        DeviceProtocol::Jmap,
    )
    .await
    .expect("another login from the address is not blocked");

    let clean = peer(3, 3);
    fail("lock-c@example.com", clean, limits.per_login - 1).await;
    auth.verify("lock-c@example.com", &c.secret, clean, DeviceProtocol::Jmap)
        .await
        .expect("under the limit");
    fail("lock-c@example.com", clean, limits.per_login - 1).await;
    auth.verify("lock-c@example.com", &c.secret, clean, DeviceProtocol::Jmap)
        .await
        .expect("a success cleared the login's failures");
}

async fn address_lockout(auth: &dyn DeviceAuth, harness: &dyn Harness, limits: Limits) {
    harness.user("spray@example.com", "web", &[]).await;
    let target = harness.enroll(jmap("spray@example.com", None)).await;
    let sprayer = peer(4, 1);
    for n in 0..limits.per_address {
        let login = format!("sprayed-{n}@example.com");
        assert_invalid(
            auth.verify(&login, "hm_wrong", sprayer, DeviceProtocol::Jmap)
                .await,
            "a sprayed login",
        );
    }
    assert_too_many(
        auth.verify(
            "spray@example.com",
            &target.secret,
            sprayer,
            DeviceProtocol::Jmap,
        )
        .await,
        "a spraying address is blocked for every login",
    );
    auth.verify(
        "spray@example.com",
        &target.secret,
        peer(4, 2),
        DeviceProtocol::Jmap,
    )
    .await
    .expect("another address is not blocked");
}

async fn revocation(auth: &dyn DeviceAuth, harness: &dyn Harness) {
    harness.user("revoke@example.com", "web", &[]).await;
    let login = "revoke@example.com";
    let first = harness.enroll(jmap(login, None)).await;
    let second = harness.enroll(jmap(login, None)).await;
    let third = harness.enroll(jmap(login, None)).await;
    let from = peer(5, 1);
    auth.verify(login, &first.secret, from, DeviceProtocol::Jmap)
        .await
        .unwrap();

    let mut feed = auth.revocations();
    assert!(auth.revoke(first.device).await.unwrap());
    assert!(!auth.revoke(first.device).await.unwrap(), "already revoked");
    assert!(!auth.revoke(DeviceId::new()).await.unwrap(), "unknown");
    assert!(
        matches!(
            auth.verify(login, &first.secret, from, DeviceProtocol::Jmap)
                .await,
            Err(DeviceAuthError::Revoked)
        ),
        "revoked on the very next call"
    );
    assert_invalid(
        auth.verify(
            "nobody-revoked@example.com",
            &first.secret,
            from,
            DeviceProtocol::Jmap,
        )
        .await,
        "a revoked secret with the wrong login tells nothing",
    );
    auth.verify(login, &second.secret, peer(5, 2), DeviceProtocol::Jmap)
        .await
        .expect("other devices keep working");

    let wait = Duration::from_secs(1);
    let got = tokio::time::timeout(wait, feed.next()).await.expect("feed");
    assert_eq!(got, first.device);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), feed.next())
            .await
            .is_err(),
        "each revocation comes out once"
    );

    auth.end_sessions(second.device);
    let got = tokio::time::timeout(wait, feed.next()).await.expect("feed");
    assert_eq!(got, second.device, "a lowered device's sessions end");
    auth.verify(login, &second.secret, peer(5, 2), DeviceProtocol::Jmap)
        .await
        .expect("ending sessions does not revoke");

    assert_eq!(auth.revoke_user(second.user).await.unwrap(), 2);
    let mut seen = vec![
        tokio::time::timeout(wait, feed.next()).await.expect("feed"),
        tokio::time::timeout(wait, feed.next()).await.expect("feed"),
    ];
    seen.sort();
    assert_eq!(seen, vec![second.device, third.device]);
    assert!(matches!(
        auth.verify(login, &third.secret, peer(5, 3), DeviceProtocol::Jmap)
            .await,
        Err(DeviceAuthError::Revoked)
    ));
    assert_eq!(auth.revoke_user(second.user).await.unwrap(), 0);

    let mut late = auth.revocations();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), late.next())
            .await
            .is_err(),
        "a new feed starts empty"
    );
}

async fn timing(auth: &dyn DeviceAuth, harness: &dyn Harness) {
    const ROUNDS: u8 = 15;
    const TOLERANCE: Duration = Duration::from_millis(50);
    harness.user("timing@example.com", "web", &[]).await;
    harness.enroll(jmap("timing@example.com", None)).await;
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for host in 1..=ROUNDS {
        let started = Instant::now();
        assert_invalid(
            auth.verify(
                "timing@example.com",
                "hm_wrong",
                peer(6, host),
                DeviceProtocol::Jmap,
            )
            .await,
            "a known login",
        );
        known.push(started.elapsed());
        let started = Instant::now();
        assert_invalid(
            auth.verify(
                "no-timing@example.com",
                "hm_wrong",
                peer(7, host),
                DeviceProtocol::Jmap,
            )
            .await,
            "an unknown login",
        );
        unknown.push(started.elapsed());
    }
    known.sort();
    unknown.sort();
    let (known, unknown) = (known[known.len() / 2], unknown[unknown.len() / 2]);
    assert!(
        known.abs_diff(unknown) < TOLERANCE,
        "an unknown login is told apart by time: {known:?} and {unknown:?}"
    );
}

struct FakeDevice {
    id: DeviceId,
    email: String,
    mail_archive: Option<ArchiveId>,
    archives: Option<Vec<ArchiveId>>,
    protocols: Vec<DeviceProtocol>,
    scopes: Vec<DeviceScope>,
    revoked: bool,
}

#[derive(Default)]
struct FakeState {
    users: HashMap<String, (UserId, Vec<ArchiveId>)>,
    devices: HashMap<String, FakeDevice>,
    per_login: HashMap<(IpAddr, String), u32>,
    per_address: HashMap<IpAddr, u32>,
    feeds: Vec<mpsc::UnboundedSender<DeviceId>>,
    next_secret: u32,
}

impl FakeState {
    fn revoke(&mut self, id: DeviceId) -> bool {
        let Some(device) = self.devices.values_mut().find(|d| d.id == id && !d.revoked) else {
            return false;
        };
        device.revoked = true;
        self.feeds.retain(|feed| feed.send(id).is_ok());
        true
    }
}

/// An in-memory [`DeviceAuth`] that keeps the whole contract, for tests of protocol front ends.
/// It is its own [`Harness`].
pub struct FakeDeviceAuth {
    limits: Limits,
    state: Mutex<FakeState>,
}

impl Default for FakeDeviceAuth {
    fn default() -> Self {
        Self::new(Limits {
            per_login: 10,
            per_address: 50,
        })
    }
}

impl FakeDeviceAuth {
    /// A fake with these limits and nothing in it.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            state: Mutex::new(FakeState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake device auth")
    }
}

struct FakeFeed(mpsc::UnboundedReceiver<DeviceId>);

#[async_trait]
impl RevocationFeed for FakeFeed {
    async fn next(&mut self) -> DeviceId {
        match self.0.recv().await {
            Some(id) => id,
            None => std::future::pending().await,
        }
    }
}

#[async_trait]
impl DeviceAuth for FakeDeviceAuth {
    fn new_secret(&self) -> Result<crate::NewSecret, DeviceAuthError> {
        let mut state = self.lock();
        state.next_secret += 1;
        let secret = format!("hm_{:032}", state.next_secret);
        let mut hash = [0u8; 32];
        hash[..4].copy_from_slice(&state.next_secret.to_le_bytes());
        Ok(crate::NewSecret { secret, hash })
    }

    async fn verify(
        &self,
        login: &str,
        secret: &str,
        peer: IpAddr,
        protocol: DeviceProtocol,
    ) -> Result<Principal, DeviceAuthError> {
        let login = login.to_ascii_lowercase();
        let key = (peer, login.clone());
        let mut state = self.lock();
        if state.per_login.get(&key).copied().unwrap_or(0) >= self.limits.per_login
            || state.per_address.get(&peer).copied().unwrap_or(0) >= self.limits.per_address
        {
            return Err(DeviceAuthError::TooMany {
                retry_after: Duration::from_secs(60),
            });
        }
        let found = state.devices.get(secret).filter(|d| d.email == login);
        let (outcome, cleared) = match found {
            None => (Err(DeviceAuthError::Invalid), false),
            Some(device) if device.revoked => (Err(DeviceAuthError::Revoked), false),
            Some(device) => {
                let (user, readable) = state.users[&device.email].clone();
                let mail_archive = device.mail_archive.filter(|a| readable.contains(a));
                let mut archives: Vec<ArchiveId> = match &device.archives {
                    None => readable.clone(),
                    Some(list) => list
                        .iter()
                        .copied()
                        .filter(|a| readable.contains(a))
                        .collect(),
                };
                archives.sort();
                let needed = if protocol == DeviceProtocol::Smtp {
                    DeviceScope::SendMail
                } else {
                    DeviceScope::Read
                };
                let usable = device.protocols.contains(&protocol)
                    && device.scopes.contains(&needed)
                    && (mail_archive.is_some() || !protocol.needs_mail_archive());
                if usable {
                    let principal = Principal {
                        user,
                        device: device.id,
                        mail_archive,
                        archives,
                        scopes: device.scopes.iter().copied().collect(),
                        implicit_seen: false,
                    };
                    (Ok(principal), true)
                } else {
                    (Err(DeviceAuthError::Invalid), false)
                }
            }
        };
        if cleared {
            state.per_login.remove(&key);
        } else if matches!(outcome, Err(DeviceAuthError::Invalid)) {
            *state.per_login.entry(key).or_default() += 1;
            *state.per_address.entry(peer).or_default() += 1;
        }
        outcome
    }

    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError> {
        Ok(self.lock().revoke(device))
    }

    async fn revoke_user(&self, user: UserId) -> Result<u32, DeviceAuthError> {
        let mut state = self.lock();
        let ids: Vec<DeviceId> = state
            .devices
            .values()
            .filter(|d| !d.revoked && state.users.get(&d.email).is_some_and(|u| u.0 == user))
            .map(|d| d.id)
            .collect();
        let count = ids.len() as u32;
        for id in ids {
            state.revoke(id);
        }
        Ok(count)
    }

    fn end_sessions(&self, device: DeviceId) {
        self.lock().feeds.retain(|feed| feed.send(device).is_ok());
    }

    fn revocations(&self) -> Box<dyn RevocationFeed> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.lock().feeds.push(sender);
        Box::new(FakeFeed(receiver))
    }
}

#[async_trait]
impl Harness for FakeDeviceAuth {
    async fn archive(&self) -> ArchiveId {
        ArchiveId::new()
    }

    async fn user(&self, email: &str, _web_password: &str, readable: &[ArchiveId]) {
        self.lock().users.insert(
            email.to_ascii_lowercase(),
            (UserId::new(), readable.to_vec()),
        );
    }

    async fn enroll(&self, spec: DeviceSpec) -> Enrolled {
        let mut state = self.lock();
        let email = spec.email.to_ascii_lowercase();
        let user = state.users[&email].0;
        state.next_secret += 1;
        let secret = format!("hm_{:032}", state.next_secret);
        let id = DeviceId::new();
        state.devices.insert(
            secret.clone(),
            FakeDevice {
                id,
                email,
                mail_archive: spec.mail_archive,
                archives: spec.archives,
                protocols: spec.protocols,
                scopes: spec.scopes,
                revoked: false,
            },
        );
        Enrolled {
            device: id,
            user,
            secret,
        }
    }

    fn limits(&self) -> Limits {
        self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fake_keeps_the_contract() {
        let fake = Arc::new(FakeDeviceAuth::new(Limits {
            per_login: 3,
            per_address: 12,
        }));
        conformance(fake.clone(), &*fake).await;
    }
}
