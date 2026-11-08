use holtmail_auth::{HashCost, Passwords};
use holtmail_device::testing::{DeviceSpec, Enrolled, Harness, Limits, conformance};
use holtmail_store::{ArchiveStore, DeviceSettings, DeviceStore, NewDevice, NewUser, UserStore};
use holtmail_store_sqlite::SqliteStore;

use super::*;

const LIMITS: Limits = Limits {
    per_login: 3,
    per_address: 12,
};
const PEER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 7));

struct Fixture {
    store: Arc<SqliteStore>,
    passwords: Passwords,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let passwords = Passwords::new(HashCost {
        memory_kib: 64,
        iterations: 1,
        lanes: 1,
    })
    .unwrap();
    Fixture {
        store,
        passwords,
        _dir: dir,
    }
}

fn options() -> Options {
    Options {
        max_failures_per_login: LIMITS.per_login,
        max_failures_per_address: LIMITS.per_address,
        ..Options::default()
    }
}

impl Fixture {
    async fn add_user(&self, email: &str, role: InstanceRole, web_password: &str) -> UserId {
        self.store
            .create_user(NewUser {
                email: email.into(),
                name: "Invented Person".into(),
                role,
                password_hash: Some(self.passwords.hash(web_password).unwrap()),
            })
            .await
            .unwrap()
            .id
    }

    async fn add_device(&self, user: UserId, settings: DeviceSettings) -> (DeviceId, String) {
        let secret = new_app_password().unwrap();
        let device = self
            .store
            .create_device(NewDevice {
                user_id: user,
                settings,
                secret_hash: Some(secret_hash(&secret)),
                at: Timestamp::now(),
            })
            .await
            .unwrap();
        (device.id, secret)
    }
}

fn settings(protocols: &[DeviceProtocol], scopes: &[DeviceScope]) -> DeviceSettings {
    DeviceSettings {
        name: "Test client".into(),
        mail_archive: None,
        archives: None,
        protocols: protocols.iter().copied().collect(),
        scopes: scopes.iter().copied().collect(),
        implicit_seen: false,
    }
}

fn jmap() -> DeviceSettings {
    settings(&[DeviceProtocol::Jmap], &[DeviceScope::Read])
}

#[async_trait]
impl Harness for Fixture {
    async fn archive(&self) -> ArchiveId {
        self.store.create_archive("Invented").await.unwrap().id
    }

    async fn user(&self, email: &str, web_password: &str, readable: &[ArchiveId]) {
        let user = self
            .add_user(email, InstanceRole::Member, web_password)
            .await;
        self.store.set_grants(user, readable).await.unwrap();
    }

    async fn enroll(&self, spec: DeviceSpec) -> Enrolled {
        let user = self
            .store
            .find_user_by_email(&spec.email)
            .await
            .unwrap()
            .unwrap()
            .id;
        let scopes = spec.scopes.clone();
        let (device, secret) = self
            .add_device(
                user,
                DeviceSettings {
                    mail_archive: spec.mail_archive,
                    archives: spec.archives.map(|list| list.into_iter().collect()),
                    ..settings(&spec.protocols, &scopes)
                },
            )
            .await;
        Enrolled {
            device,
            user,
            secret,
        }
    }

    fn limits(&self) -> Limits {
        LIMITS
    }
}

#[tokio::test]
async fn keeps_the_contract() {
    let fixture = fixture().await;
    let auth = Arc::new(LocalDeviceAuth::new(fixture.store.clone(), options()));
    conformance(auth, &fixture).await;
}

#[test]
fn app_passwords_have_the_documented_shape() {
    let first = new_app_password().unwrap();
    let second = new_app_password().unwrap();
    assert_ne!(first, second);
    assert_eq!(first.len(), 35);
    let body = first.strip_prefix("hm_").unwrap();
    assert!(body.bytes().all(|b| SECRET_ALPHABET.contains(&b)), "{body}");
    assert_eq!(
        secret_hash(&first),
        *blake3::hash(first.as_bytes()).as_bytes()
    );
    assert_ne!(secret_hash(&first), secret_hash(&second));
}

#[tokio::test]
async fn owners_and_admins_read_every_archive_and_members_their_grants() {
    let fixture = fixture().await;
    let auth = LocalDeviceAuth::new(fixture.store.clone(), options());
    let first = fixture.archive().await;
    let second = fixture.archive().await;
    let mut all = vec![first, second];
    all.sort();

    for (email, role) in [
        ("owner@example.com", InstanceRole::Owner),
        ("admin@example.com", InstanceRole::Admin),
    ] {
        let user = fixture.add_user(email, role, "web password").await;
        let (_, secret) = fixture.add_device(user, jmap()).await;
        let got = auth
            .verify(email, &secret, PEER, DeviceProtocol::Jmap)
            .await
            .unwrap();
        assert_eq!(got.archives, all, "{email}");
    }

    let member = fixture
        .add_user("member@example.com", InstanceRole::Member, "web password")
        .await;
    fixture.store.set_grants(member, &[first]).await.unwrap();
    let (_, secret) = fixture.add_device(member, jmap()).await;
    let verify = async || {
        auth.verify("member@example.com", &secret, PEER, DeviceProtocol::Jmap)
            .await
            .unwrap()
            .archives
    };
    assert_eq!(verify().await, vec![first]);
    fixture.store.set_grants(member, &[second]).await.unwrap();
    assert_eq!(
        verify().await,
        vec![second],
        "a changed grant shows at once"
    );
}

#[tokio::test]
async fn a_used_device_keeps_only_its_day_and_protocol() {
    let fixture = fixture().await;
    let auth = LocalDeviceAuth::new(fixture.store.clone(), options());
    let user = fixture
        .add_user("use@example.com", InstanceRole::Member, "web password")
        .await;
    let (device, secret) = fixture
        .add_device(
            user,
            DeviceSettings {
                implicit_seen: true,
                ..settings(
                    &[DeviceProtocol::Jmap, DeviceProtocol::Dav],
                    &[DeviceScope::Read],
                )
            },
        )
        .await;
    let before = fixture.store.get_device(device).await.unwrap().unwrap();
    assert_eq!(before.last_used_day, None);

    assert!(
        auth.verify("use@example.com", "hm_wrong", PEER, DeviceProtocol::Jmap)
            .await
            .is_err()
    );
    let failed = fixture.store.get_device(device).await.unwrap().unwrap();
    assert_eq!(failed.last_used_day, None, "a failure is no use");

    for protocol in [DeviceProtocol::Jmap, DeviceProtocol::Dav] {
        let got = auth
            .verify("use@example.com", &secret, PEER, protocol)
            .await
            .unwrap();
        assert!(got.implicit_seen);
        let used = fixture.store.get_device(device).await.unwrap().unwrap();
        assert_eq!(used.last_used_day, Some(start_of_day(Timestamp::now())));
        assert_eq!(used.last_protocol, Some(protocol));
    }
}

#[tokio::test]
async fn failures_stop_counting_after_the_window() {
    let fixture = fixture().await;
    let auth = LocalDeviceAuth::new(
        fixture.store.clone(),
        Options {
            failure_window: Duration::from_millis(100),
            ..options()
        },
    );
    let user = fixture
        .add_user("window@example.com", InstanceRole::Member, "web password")
        .await;
    let (_, secret) = fixture.add_device(user, jmap()).await;
    for _ in 0..LIMITS.per_login {
        assert!(matches!(
            auth.verify("WINDOW@example.com", "hm_wrong", PEER, DeviceProtocol::Jmap)
                .await,
            Err(DeviceAuthError::Invalid)
        ));
    }
    assert!(matches!(
        auth.verify("window@example.com", &secret, PEER, DeviceProtocol::Jmap)
            .await,
        Err(DeviceAuthError::TooMany { .. })
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    auth.verify("window@example.com", &secret, PEER, DeviceProtocol::Jmap)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_oversized_login_is_refused_and_counted() {
    let fixture = fixture().await;
    let auth = LocalDeviceAuth::new(fixture.store.clone(), options());
    let login = "a".repeat(1 << 20);
    for _ in 0..LIMITS.per_login {
        assert!(matches!(
            auth.verify(&login, "hm_wrong", PEER, DeviceProtocol::Jmap)
                .await,
            Err(DeviceAuthError::Invalid)
        ));
    }
    assert!(matches!(
        auth.verify(&login, "hm_wrong", PEER, DeviceProtocol::Jmap)
            .await,
        Err(DeviceAuthError::TooMany { .. })
    ));
}

#[tokio::test]
async fn every_feed_gets_each_revocation_and_closed_feeds_are_dropped() {
    let fixture = fixture().await;
    let auth = LocalDeviceAuth::new(fixture.store.clone(), options());
    let user = fixture
        .add_user("feed@example.com", InstanceRole::Member, "web password")
        .await;
    let (device, _) = fixture.add_device(user, jmap()).await;

    let mut first = auth.revocations();
    let mut second = auth.revocations();
    let dropped = auth.revocations();
    drop(dropped);
    assert_eq!(auth.feeds().len(), 3);
    let _ = auth.revocations();
    assert_eq!(auth.feeds().len(), 3, "closed feeds are dropped");

    assert!(auth.revoke(device).await.unwrap());
    assert_eq!(first.next().await, device);
    assert_eq!(second.next().await, device);
    assert_eq!(auth.feeds().len(), 2, "the last closed feed is gone too");
}
