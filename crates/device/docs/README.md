# holtmail-device

The contract for signing devices in. A device is one client installation a user set up; it signs in to IMAP, SMTP submission, JMAP, CalDAV and CardDAV with its app password. `DeviceAuth` turns a login, an app password, a client address and a protocol into a `Principal`, limits failures, and tells live sessions when a device is revoked. Only app passwords authenticate here: the web password, a passkey or a single sign-in never does. The design is in [SERVE.md](../../../docs/SERVE.md#devices-and-sign-in).

## Types

```rust
pub struct Principal {
    pub user: UserId,
    pub device: DeviceId,
    pub mail_archive: Option<ArchiveId>,
    pub archives: Vec<ArchiveId>,
    pub scopes: BTreeSet<DeviceScope>,
    pub implicit_seen: bool,
}
```

`mail_archive` and `archives` are limited to what the user can read at the moment of verifying, so a device never sees more than its user; a device with no `archives` list gets every readable archive. `archives` is sorted. `DeviceProtocol`, `DeviceScope` and `DeviceId` come from `holtmail-core`.

## DeviceAuth

```rust
#[async_trait]
pub trait DeviceAuth: Send + Sync {
    async fn verify(&self, login: &str, secret: &str, peer: IpAddr, protocol: DeviceProtocol)
        -> Result<Principal, DeviceAuthError>;
    async fn revoke(&self, device: DeviceId) -> Result<bool, DeviceAuthError>;
    async fn revoke_user(&self, user: UserId) -> Result<u32, DeviceAuthError>;
    fn end_sessions(&self, device: DeviceId);
    fn new_secret(&self) -> Result<NewSecret, DeviceAuthError>;
    fn revocations(&self) -> Box<dyn RevocationFeed>;
}
```

| Method | Guarantees |
| --- | --- |
| `verify` | The limits come first: a blocked client is refused even with the right secret. `login` is the user's email in any letter case, and a secret works only with its own user. The time taken does not depend on whether the login exists. The device must list `protocol`; IMAP and SMTP need a readable mail archive; IMAP, JMAP, DAV and the API need `read`, SMTP needs `send:mail`. |
| `revoke` | Permanent. Every later `verify` of the secret answers `Revoked`. True when this call revoked it, false for a device already revoked or unknown. |
| `revoke_user` | The same for every device of the user; returns how many this call revoked. Call it before removing a user, because a removed user's devices no longer exist to revoke. |
| `end_sessions` | Puts the device on the feed without revoking it, so its live sessions end and it signs in again under its current rights. Its secret keeps working. |
| `new_secret` | A new random app password (`hm_` plus 32 base32 characters), never returned twice, with the hash to store. A device stored with that hash verifies with it. The only way the rest of the system gets a secret, so the format and the hash stay the implementation's. |
| `revocations` | A feed of the devices revoked, or whose sessions were ended, from now on. |

`RevocationFeed::next` waits for the next device. Every revocation and every `end_sessions` after the feed was made comes out exactly once, within a second, however slowly the feed is read; dropping the future while it waits loses nothing, so it can sit in a `select!`.

**Limits.** Two counters: failures per client address and login, and failures per client address across all logins. Both count every protocol together. Every `Invalid` is one failure; `Revoked` and `Unavailable` are not. A success clears the first counter for that pair and never the second, so a valid account cannot launder a spraying address.

## Errors

| `DeviceAuthError` | Meaning |
| --- | --- |
| `Invalid` | The login, the secret or their pairing is wrong, the device may not use this protocol, or it lacks what the protocol needs. Deliberately not told which |
| `TooMany { retry_after }` | A limit is used up; `retry_after` is at least one second |
| `Revoked` | The login and secret are right and the device was revoked. Told only to a holder of the secret |
| `Unavailable(text)` | The check could not be made; try again |

## Writing an implementation

Enable the `testing` feature under `[dev-dependencies]` and run the suite with a `Harness` over the same state as the implementation:

```rust
holtmail_device::testing::conformance(auth, &harness).await;
```

`Harness` makes archives, users (with the web password and the archives they can read) and devices, and says the limits in force (`per_address` at least four times `per_login`, `per_login` at least 2). The suite covers the principal, every wrong-credential case including the web password, protocols and scopes, both lockouts, revocation on the next call and the feed, `end_sessions` reaching the feed without revoking, and that an unknown login is not told apart by time (a median over 15 rounds, within 50 ms, which catches a slow hash run only for known logins).

`testing::FakeDeviceAuth` keeps the whole contract in memory and is its own `Harness`; protocol front ends use it in their tests. Its app passwords are `hm_` plus 32 digits.
