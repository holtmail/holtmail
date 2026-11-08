# holtmail-device-local

The [`DeviceAuth`](../../device/docs/README.md) implementation: app passwords checked against the metadata store, failures limited with `holtmail-auth`'s `Limiter`, and revocations fanned out to the live sessions of this process. The design is in [SERVE.md](../../../docs/SERVE.md#devices-and-sign-in).

## Public API

`LocalDeviceAuth::new(store: Arc<dyn Store>, options: Options)` implements `DeviceAuth` with the guarantees of the contract.

| Function | Guarantees |
| --- | --- |
| `new_app_password()` | `hm_` plus 32 base32 characters in lower case (160 random bits from the operating system). `Unavailable` when there is no randomness |
| `secret_hash(secret)` | What `DeviceStore` keeps for that secret: BLAKE3 of its exact bytes. Whoever creates a device stores this hash and shows the secret once |

## Options

| Field | Default |
| --- | --- |
| `max_failures_per_login` | 10, per client address and login |
| `max_failures_per_address` | 50, per client address across all logins |
| `failure_window` | 15 minutes |

## Behavior

- **Rights.** Owners and admins can read every archive, members their granted ones, looked up on every `verify`. A changed grant or role shows on the next call.
- **Time.** Every attempt past the limits makes the same two exact lookups, the login and the secret's hash, whether or not either exists. A login longer than 254 bytes cannot exist and skips the first. A random 160-bit secret needs no slow hash.
- **Use.** A success records the day and protocol, and writes only when one of them changed. A failure to record is logged with the device id and does not fail the sign-in.
- **Failures.** `Invalid` counts one failure against both limits. `Revoked` and `Unavailable` count none. Logins count without regard to letter case and only their first 254 characters are kept.

## Limits

- Failures count per client block: an IPv4 address or an IPv6 /64 (`holtmail_core::address_block`).
- Devices are revoked through this type. A revocation written directly to the store holds on the next `verify` but reaches no feed.
- Failure counts and feeds live in memory and end with the process, so one process serves a store.
- A front end takes its feed before `verify`, so a device revoked in between still ends the session.
- The address limit keys on the whole client address, so an IPv6 client with many addresses gets many budgets; the 160 bits of the secret are what stops guessing.
- Removing a user removes their devices, so `revoke_user` comes first.
