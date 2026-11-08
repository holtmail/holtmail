# holtmail-submission

The contract for accepting a message from a person or a client. A `Submission` checks that the sender may send this message as this identity and takes it into a durable queue; it can be undone until it leaves, and it reports what became of each recipient. It is the one way in for the web app, SMTP submission and JMAP, so all of them are held to the same locks. Nothing is delivered here. The design is in [SERVE.md](../../../docs/SERVE.md#smtp-submission).

## Types

```rust
pub struct Actor { pub user: UserId, pub device: Option<DeviceId>, pub scopes: BTreeSet<DeviceScope> }
pub struct SendIdentity { pub id: IdentityId, pub address: String, pub name: Option<String> }
pub struct SubmissionLimits { pub max_size: u64, pub smtputf8: bool }
pub struct Submit {
    pub actor: Actor,
    pub archive: ArchiveId,
    pub identity: IdentityId,
    pub mail_from: String,
    pub recipients: Vec<String>,
    pub message: Bytes,
    pub send_at: Option<Timestamp>,
    pub undo_window: Option<Duration>,
    pub idempotency_key: Option<String>,
    pub allow_leaks: bool,
    pub route: Option<Route>,
}
pub struct Submitted { pub id: OutboxId, pub archive: ArchiveId, pub identity: IdentityId,
    pub state: SubmissionState, pub send_at: Timestamp,
    pub recipients: Vec<RecipientStatus>, pub rfc_message_id: String }
```

- `Actor::web(user)` is a person in the web app, whose session carries every scope. Sending needs `send:mail`.
- `Submit::message` is the exact bytes: with `Date` and `Message-ID`, without `Bcc`, with one `From` field holding one address that is the identity's or a plus address of it, and at most one `Sender` field. Fixing up a client's message (RFC 6409, removing `Bcc`) is the protocol's job, before this call.
- `undo_window`: `None` is the instance's default, `Some(ZERO)` none.
- `allow_leaks` is only for a person who confirmed the leak in the web app; protocols leave it false.
- `route` is the `holtmail_route::Route` a front end already chose and showed to the person, stored with the message; protocols leave it `None`. With a route the leak guard is not run again, because that front end ran it with more than the message tells, and `allow_leaks` says it found leaks the person confirmed. `mail_from` and the header `From` must then be the route's `from`, which may be an alias of the identity, and the route must name that identity.
- `SubmissionState` is `Queued`, `Sending` (also while waiting for a retry), `Sent`, `Failed`, `Uncertain`, `Bounced`, `Cancelled`. `RecipientDelivery` is `Pending`, `Accepted`, `Refused { permanent, code, text }`, `Bounced { status, text }`; `text` may name addresses, never log it.

## Submission

| Method | Guarantees |
| --- | --- |
| `identities(actor, archive)` | The identities the actor may send as right now, by address. Empty when any lock is closed for them |
| `limits(actor, archive)` | `max_size`: the smallest limit among the transports of the identities the sender may use, or `DEFAULT_MAX_SIZE` (25 MiB). `smtputf8`: every one of those transports takes it. Answers for someone who may not send too |
| `sender(actor, archive, address)` | The active identity whose address is `address` or has it as a plus address, ignoring letter case, with every lock open. `Forbidden` otherwise and for the null return path; `RateLimited` when the limits are used up |
| `queue(request)` | A repeated idempotency key (per user) returns the first submission at once, whatever changed since, and counts once against the limits. Otherwise, in this order: `sender`'s checks for the named identity, the header `From`, the shape of the message and recipients, the size, the leak guard unless `allow_leaks`, the transport's own rules. A refused message leaves nothing behind. After `Unavailable` a request with a key may be repeated and picks up where it stopped; nothing leaves until then. Limits apply to device submissions only. It leaves no earlier than `send_at` and the end of its undo window |
| `undo(actor, id)` | The message becomes `Cancelled` and never leaves; undoing again returns it unchanged. `TooLate` once it left the queue, `NotFound` unless the actor's user submitted it |
| `status(actor, id)` | Its state and each recipient in submitted order. `NotFound` unless the actor's user submitted it |

Implementations are safe to share between tasks.

## Errors

Reasons name fields and rules, never addresses or content, so a front end may show them to the client.

| `SubmissionError` | Meaning | SMTP |
| --- | --- | --- |
| `Forbidden(reason)` | No such identity for the sender, a missing grant or scope, a closed switch or capability, a null return path | `553 5.7.1` |
| `FromMismatch` | The header `From` is not exactly one field with one address of the identity | `554 5.7.1` |
| `Leak` | A recipient would see the real mailbox behind the alias the thread came through | `550 5.7.1` |
| `TooLarge { limit }` | Larger than a transport of this identity takes | `552` |
| `RateLimited { retry_after }` | The sender's limits are used up | `451 4.7.1` |
| `Invalid(reason)` | The message or request cannot be sent as given | `554` |
| `NotFound(what)` | No such submission for this sender | |
| `TooLate(state)` | The message already left the queue | |
| `Unavailable(text)` | The queue could not be reached; a later try may pass | `451` |

## Writing an implementation

Enable the `testing` feature under `[dev-dependencies]` and run the suite with a `Harness`:

```rust
holtmail_submission::testing::conformance(&harness).await;
```

`Harness::start(setup)` makes a fresh instance as `Setup` says (per-user limits, the transport's `max_size` and `smtputf8`, whether sending is open) and returns a `World`: the implementation, a `Driver` that hands a message to its transport, the archive, a sender with the identity `ada@example.com`, a stranger who may not send as it, and a thread that arrived through an alias with a recipient who would see the real mailbox.

`testing::FakeSubmission::world(setup)` keeps the whole contract in memory for tests of protocol front ends; its time-based limits never reset.
