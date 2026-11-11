use std::collections::BTreeSet;

use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol, DeviceScope, Timestamp, UserId};

use crate::*;

pub(super) async fn run(store: &dyn Store) {
    create_and_find(store).await;
    rules(store).await;
    settings_replace(store).await;
    usage_is_kept_in_days(store).await;
    revocation(store).await;
    secret_replaced(store).await;
    removal(store).await;
}

const DAY: i64 = 24 * 60 * 60 * 1000;

fn t(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

async fn owner(store: &dyn Store, email: &str) -> UserId {
    store
        .create_user(NewUser {
            email: email.into(),
            name: "Device owner".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id
}

fn settings(mail: Option<ArchiveId>) -> DeviceSettings {
    DeviceSettings {
        name: "Phone".into(),
        mail_archive: mail,
        archives: None,
        protocols: [DeviceProtocol::Jmap].into_iter().collect(),
        scopes: [DeviceScope::Read].into_iter().collect(),
        implicit_seen: false,
    }
}

fn new(user: UserId, hash: Option<u8>, settings: DeviceSettings) -> NewDevice {
    NewDevice {
        user_id: user,
        settings,
        secret_hash: hash.map(|b| [b; 32]),
        at: t(5000),
    }
}

async fn create_and_find(store: &dyn Store) {
    let user = owner(store, "devices-find@example.com").await;
    let archive = store.create_archive("Devices").await.unwrap().id;
    let other = store.create_archive("Devices two").await.unwrap().id;
    let full = DeviceSettings {
        name: "Thunderbird at work".into(),
        mail_archive: Some(archive),
        archives: Some([archive, other].into_iter().collect()),
        protocols: [
            DeviceProtocol::Imap,
            DeviceProtocol::Smtp,
            DeviceProtocol::Dav,
        ]
        .into_iter()
        .collect(),
        scopes: DeviceScope::ALL.into_iter().collect(),
        implicit_seen: true,
    };
    let created = store
        .create_device(new(user, Some(1), full.clone()))
        .await
        .unwrap();
    assert_eq!(created.user_id, user);
    assert_eq!(created.settings, full, "the settings are kept as given");
    assert!(created.has_secret);
    assert_eq!(created.created_at, t(5000));
    assert_eq!(created.last_used_day, None);
    assert_eq!(created.last_protocol, None);
    assert_eq!(created.revoked_at, None);
    assert_eq!(
        store.get_device(created.id).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        store.find_device_by_secret(&[1; 32]).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(store.find_device_by_secret(&[9; 32]).await.unwrap(), None);
    assert_eq!(store.get_device(DeviceId::new()).await.unwrap(), None);

    let enrolled = store
        .create_device(new(user, None, settings(None)))
        .await
        .unwrap();
    assert!(!enrolled.has_secret, "a device without an app password");
    assert_eq!(enrolled.settings.archives, None, "every readable archive");
    let second = store
        .create_device(new(user, None, settings(None)))
        .await
        .unwrap();
    assert_eq!(
        store.list_devices(user).await.unwrap(),
        vec![created, enrolled, second],
        "oldest first"
    );
    let stranger = owner(store, "devices-stranger@example.com").await;
    assert!(store.list_devices(stranger).await.unwrap().is_empty());
}

fn not_found<T: std::fmt::Debug>(result: Result<T, StoreError>) {
    assert!(matches!(result, Err(StoreError::NotFound(_))), "{result:?}");
}

fn invalid<T: std::fmt::Debug>(result: Result<T, StoreError>) {
    assert!(matches!(result, Err(StoreError::Invalid(_))), "{result:?}");
}

async fn rules(store: &dyn Store) {
    let user = owner(store, "devices-rules@example.com").await;
    let archive = store.create_archive("Device rules").await.unwrap().id;
    for protocol in [DeviceProtocol::Imap, DeviceProtocol::Smtp] {
        let mut without = settings(None);
        without.protocols.insert(protocol);
        invalid(store.create_device(new(user, None, without)).await);
    }
    not_found(
        store
            .create_device(new(UserId::new(), None, settings(None)))
            .await,
    );
    not_found(
        store
            .create_device(new(user, None, settings(Some(ArchiveId::new()))))
            .await,
    );
    let mut unknown = settings(None);
    unknown.archives = Some([ArchiveId::new()].into_iter().collect());
    not_found(store.create_device(new(user, None, unknown)).await);

    store
        .create_device(new(user, Some(2), settings(Some(archive))))
        .await
        .unwrap();
    invalid(
        store
            .create_device(new(user, Some(2), settings(Some(archive))))
            .await,
    );
    assert_eq!(
        store.list_devices(user).await.unwrap().len(),
        1,
        "a refused device leaves nothing"
    );
}

async fn settings_replace(store: &dyn Store) {
    let user = owner(store, "devices-settings@example.com").await;
    let archive = store.create_archive("Device settings").await.unwrap().id;
    let device = store
        .create_device(new(user, Some(3), settings(None)))
        .await
        .unwrap();
    let raised = DeviceSettings {
        name: "Renamed".into(),
        mail_archive: Some(archive),
        archives: Some([archive].into_iter().collect()),
        protocols: [DeviceProtocol::Imap].into_iter().collect(),
        scopes: [DeviceScope::Read, DeviceScope::WriteMail]
            .into_iter()
            .collect(),
        implicit_seen: true,
    };
    assert_eq!(device.raised_at, None);
    let updated = store
        .update_device_settings(device.id, &raised, Some(t(5 * DAY)))
        .await
        .unwrap();
    assert_eq!(updated.settings, raised);
    assert_eq!(updated.raised_at, Some(t(5 * DAY)));
    assert_eq!(updated.created_at, device.created_at);
    assert!(updated.has_secret, "settings leave the secret alone");

    let mut no_list = raised.clone();
    no_list.archives = None;
    let again = store
        .update_device_settings(device.id, &no_list, None)
        .await
        .unwrap();
    assert_eq!(again.settings.archives, None);
    assert_eq!(
        again.raised_at,
        Some(t(5 * DAY)),
        "no new raise keeps the last"
    );
    let mut empty = raised.clone();
    empty.archives = Some(BTreeSet::new());
    assert_eq!(
        store
            .update_device_settings(device.id, &empty, None)
            .await
            .unwrap()
            .settings
            .archives,
        Some(BTreeSet::new()),
        "no archives is not every archive"
    );

    let mut lost = raised.clone();
    lost.mail_archive = None;
    invalid(store.update_device_settings(device.id, &lost, None).await);
    let mut unknown = raised.clone();
    unknown.mail_archive = Some(ArchiveId::new());
    not_found(
        store
            .update_device_settings(device.id, &unknown, None)
            .await,
    );
    not_found(
        store
            .update_device_settings(DeviceId::new(), &raised, None)
            .await,
    );
}

async fn usage_is_kept_in_days(store: &dyn Store) {
    let user = owner(store, "devices-usage@example.com").await;
    let device = store
        .create_device(new(user, None, settings(None)))
        .await
        .unwrap();
    store
        .touch_device(device.id, DeviceProtocol::Jmap, t(3 * DAY + 1234))
        .await
        .unwrap();
    let used = store.get_device(device.id).await.unwrap().unwrap();
    assert_eq!(
        used.last_used_day,
        Some(t(3 * DAY)),
        "the day, not the time"
    );
    assert_eq!(start_of_day(t(3 * DAY + 1234)), t(3 * DAY));
    assert_eq!(used.last_protocol, Some(DeviceProtocol::Jmap));

    store
        .touch_device(device.id, DeviceProtocol::Dav, t(3 * DAY + 9999))
        .await
        .unwrap();
    let same_day = store.get_device(device.id).await.unwrap().unwrap();
    assert_eq!(same_day.last_used_day, Some(t(3 * DAY)));
    assert_eq!(same_day.last_protocol, Some(DeviceProtocol::Dav));

    store
        .touch_device(device.id, DeviceProtocol::Imap, t(DAY))
        .await
        .unwrap();
    assert_eq!(
        store.get_device(device.id).await.unwrap().unwrap(),
        same_day,
        "an earlier day changes nothing"
    );
    not_found(
        store
            .touch_device(DeviceId::new(), DeviceProtocol::Imap, t(DAY))
            .await,
    );
}

async fn revocation(store: &dyn Store) {
    let user = owner(store, "devices-revoke@example.com").await;
    let archive = store.create_archive("Device revoke").await.unwrap().id;
    let first = store
        .create_device(new(user, Some(4), settings(Some(archive))))
        .await
        .unwrap();
    let second = store
        .create_device(new(user, Some(5), settings(None)))
        .await
        .unwrap();
    let third = store
        .create_device(new(user, Some(6), settings(None)))
        .await
        .unwrap();
    assert!(store.revoke_device(first.id, t(7000)).await.unwrap());
    assert!(
        !store.revoke_device(first.id, t(8000)).await.unwrap(),
        "already revoked"
    );
    let revoked = store.get_device(first.id).await.unwrap().unwrap();
    assert_eq!(revoked.revoked_at, Some(t(7000)), "the first time stays");
    assert_eq!(
        store.find_device_by_secret(&[4; 32]).await.unwrap(),
        Some(revoked.clone()),
        "a revoked device is still found, so it can be told apart from a wrong secret"
    );
    invalid(
        store
            .update_device_settings(first.id, &settings(Some(archive)), None)
            .await,
    );
    store
        .touch_device(first.id, DeviceProtocol::Imap, t(9 * DAY))
        .await
        .unwrap();
    assert_eq!(
        store.get_device(first.id).await.unwrap().unwrap(),
        revoked,
        "a revoked device is not used"
    );
    not_found(store.revoke_device(DeviceId::new(), t(1)).await);

    assert_eq!(
        store.revoke_user_devices(user, t(9000)).await.unwrap(),
        vec![second.id, third.id],
        "only those not revoked yet, oldest first"
    );
    assert_eq!(
        store
            .get_device(first.id)
            .await
            .unwrap()
            .unwrap()
            .revoked_at,
        Some(t(7000))
    );
    assert!(
        store
            .revoke_user_devices(user, t(9001))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .revoke_user_devices(UserId::new(), t(9001))
            .await
            .unwrap()
            .is_empty()
    );
}

async fn secret_replaced(store: &dyn Store) {
    let user = owner(store, "devices-secret@example.com").await;
    let device = store
        .create_device(new(user, Some(21), settings(None)))
        .await
        .unwrap();
    let other = store
        .create_device(new(user, Some(22), settings(None)))
        .await
        .unwrap();
    store
        .replace_device_secret(device.id, &[23; 32])
        .await
        .unwrap();
    assert_eq!(
        store.find_device_by_secret(&[21; 32]).await.unwrap(),
        None,
        "the old secret is gone"
    );
    let found = store
        .find_device_by_secret(&[23; 32])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, device.id);
    assert_eq!(found.settings, device.settings, "nothing else changes");
    assert!(found.has_secret);
    store
        .replace_device_secret(device.id, &[23; 32])
        .await
        .expect("its own hash again is no clash");
    invalid(store.replace_device_secret(device.id, &[22; 32]).await);
    assert_eq!(
        store
            .find_device_by_secret(&[22; 32])
            .await
            .unwrap()
            .unwrap()
            .id,
        other.id
    );
    store.revoke_device(other.id, t(7000)).await.unwrap();
    invalid(store.replace_device_secret(other.id, &[24; 32]).await);
    not_found(
        store
            .replace_device_secret(DeviceId::new(), &[25; 32])
            .await,
    );
}

async fn removal(store: &dyn Store) {
    let user = owner(store, "devices-removal@example.com").await;
    let gone = store.create_archive("Device gone").await.unwrap().id;
    let kept = store.create_archive("Device kept").await.unwrap().id;
    let mut spec = settings(Some(gone));
    spec.protocols.insert(DeviceProtocol::Imap);
    spec.archives = Some([gone, kept].into_iter().collect());
    let device = store.create_device(new(user, Some(7), spec)).await.unwrap();
    store.delete_archive(gone).await.unwrap();
    let after = store.get_device(device.id).await.unwrap().unwrap();
    assert_eq!(after.settings.mail_archive, None, "the archive is gone");
    assert_eq!(
        after.settings.archives,
        Some([kept].into_iter().collect()),
        "only the deleted archive leaves the list"
    );

    store.delete_user(user).await.unwrap();
    assert_eq!(store.get_device(device.id).await.unwrap(), None);
    assert_eq!(store.find_device_by_secret(&[7; 32]).await.unwrap(), None);
}
