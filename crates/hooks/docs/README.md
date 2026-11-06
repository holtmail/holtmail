# holtmail-hooks

What Holtmail tells webhooks: the events, the JSON body of each delivery, its signature and the retry schedule. A library of pure functions; the server stores deliveries and sends them through [`holtmail-webhook`](../../webhook/docs/README.md).

## Events

`EVENTS` lists what a webhook can subscribe to; `webhook.test` is sent on request whatever it subscribed to. Payloads carry ids, counts, states and hashes, never message content, subjects or addresses.

| Event | `data` |
| --- | --- |
| `task.finished` | `task_id`, `kind` (`sync`, `import`, `remote_images`, `contacts_calendars`, `ledger_verify`, `delete_messages`, `analyse`), `state` (`done`, `failed`, `cancelled`), `archive_id`, `account_id`, `done`, `total` |
| `account.needs_sign_in` | `account_id`, `archive_id`, `provider` (`google`, `microsoft`) |
| `messages.deleted` | `archive_id`, `messages`, `by` (user id, `null` from the CLI), `ledger_seq`, `ledger_hash` |
| `ledger.verified` | `ok`, `entries`, `problems`, `ledger_seq`, `ledger_hash` |
| `webhook.test` | `{}` |

## API

- `payload(id, at, &event) -> Vec<u8>`: `{ "id", "event", "at" (Unix milliseconds), "data" }`. `id` is the same for every webhook told about the event and on every attempt.
- `sign(secret, timestamp, body)`: `t=<Unix seconds>,v1=<hex HMAC-SHA256 of "<timestamp>.<body>">`, sent as `Holtmail-Signature` (`SIGNATURE_HEADER`) with `Holtmail-Event` and `Holtmail-Delivery` (the payload's `id`).
- `verify(secret, header, body, now, tolerance)`: what a receiver does, in constant time; accepts any of several `v1` values.
- `new_secret()`: `whsec_` and 64 hex digits from the system's random source.
- `retry_after(attempts)`: 30 s, 2 min, 10 min, 1 h, 6 h after the first five failures; `None` after `MAX_ATTEMPTS` (6). `delivered(status)` is any `2xx`.
- `check(url, events)`: an `http`/`https` address with a host, no user name or password, at most 2 000 characters, and known events; returns them trimmed, sorted and deduplicated, or a sentence saying why not.

## Checking a delivery (receiver side)

```python
import hmac, hashlib, time

def valid(secret: str, header: str, body: bytes, tolerance=300) -> bool:
    parts = dict(p.split("=", 1) for p in header.split(","))
    t = int(parts["t"])
    if abs(time.time() - t) > tolerance:
        return False
    mac = hmac.new(secret.encode(), f"{t}.".encode() + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(mac, parts["v1"])
```

## Tests

`cargo test -p holtmail-hooks`: payloads without mail content, signatures as receivers check them (age, wrong key, changed body, several `v1`), the retry schedule, distinct secrets.
