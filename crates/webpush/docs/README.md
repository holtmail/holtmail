# holtmail-webpush

Receiving Web Push messages: a subscriber's keys and the decryption of a message encrypted for them as RFC 8291 says, with the `aes128gcm` content coding of RFC 8188. Holtmail uses it as its own push service for WebDAV-Push: contacts and calendars servers post notices straight to the server's public address. No I/O. The cryptography (P-256 ECDH, HMAC-SHA-256, AES-128-GCM, random bytes) comes from `aws-lc-rs`, which the binary already carries for rustls.

## API

| Item | Guarantees |
| --- | --- |
| `Keys::generate()` | A fresh P-256 key pair and a random 16-byte auth secret |
| `Keys::from_parts(private, auth)` | Keys from a 32-byte big-endian private scalar and an auth secret; `InvalidKey` for wrong lengths or a scalar that is not a P-256 key |
| `public_key()`, `auth_secret()` | What the sender needs: the uncompressed public key (65 bytes) and the auth secret |
| `decrypt(body)` | The plaintext of an `aes128gcm` message in a single record (header with salt, record size and the sender's 65-byte key, then the sealed record) encrypted for these keys, with the padding and its `0x02` delimiter removed. `Undecryptable` for anything else: another subscriber's message, a changed byte, a body that is too short or has another key length |
| `encrypt(public_key, auth_secret, plaintext)` | `plaintext` sealed for a subscriber in one record of 4096 bytes with a fresh sender key and salt; for tests and senders |
| `random_id()` | 128 random bits as 32 lowercase hex digits, for push addresses nobody can guess |
| `WebPushError` | `Undecryptable`, `InvalidKey`, `Random` (the system's random source failed) |

`Debug` of `Keys` shows neither secret.

## Tests

Unit tests decrypt and re-encrypt the example of RFC 8291 Appendix A byte for byte (the same sender key and salt give the same message), round-trip fresh keys, and refuse another subscriber's keys, a changed byte and plain text.
