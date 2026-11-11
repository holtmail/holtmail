# holtmail-submission-outbox

`Submission` over the outbox: every send, from the web app, SMTP submission and later JMAP, meets the same locks, identities and limits and goes into the same durable queue. It holds no transport and sends nothing itself. An implementation of [holtmail-submission](../../submission/docs/README.md), built by `apps/server`. Design in [SERVE.md](../../../docs/SERVE.md#smtp-submission).

## API

```rust
OutboxSubmission::new(store: Arc<dyn Store>, blobs: Arc<dyn BlobStore>, access: Access,
                      outbox: Arc<Outbox>, options: Options) -> Self
pub struct Options { pub messages_per_hour: u32, pub recipients_per_day: u32 } // default 200, 2000
```

Every answer reflects the store at the moment of the call.

## What it guarantees

- **Who may send.** An identity counts when it is active, the user is among its users, its transport is enabled and usable by the archive, and `holtmail_access::Access::may_send` allows it (the instance switches, a `sender` grant, `send:mail`, the account's `mail.send`). `identities`, `limits` and `sender` also need the transport's `allowed_from` to cover the address. A store failure is `Unavailable`, never a yes.
- **Addresses.** `sender` and the `From` check accept the identity's address or, when the identity allows them, a plus address of it, ignoring letter case. With a `route`, the return path and the `From` must be the route's `from`, which may be an alias of the identity, and the route must name the identity. A repeated `From` field is `FromMismatch`, a repeated `Sender` field `Invalid`.
- **Limits** count device submissions only, in the store, so they survive restarts: `messages_per_hour` in any hour, `recipients_per_day` in any day, per user across devices and archives. A person in the web app is not counted, and an undone message gives its place back. `retry_after` is when the oldest counted message leaves the window. The count and the queueing are not one step, so concurrent submissions can overshoot by as many as are in flight.
- **The leak guard** runs for a message without a `route` and without `allow_leaks`. It finds the thread through `In-Reply-To` and the last 50 `References` by `Message-ID` in the archive; a message that answers nothing archived is not checked. Each recipient counts as the header field it is listed in, and as Bcc otherwise. Reverse aliases come from signed alias hits in the thread (with those messages' `Reply-To`) and from the alias store.
- **The order** of `queue` is the contract's, and a repeated idempotency key returns first, whatever changed since. The message is held in the outbox while its `outbox_changed` audit entry is stored (`queued`, or `queued_despite_leaks`), then released, so nothing can leave unrecorded. If the audit fails the call is `Unavailable` and nothing leaves: a message with an idempotency key stays held, and repeating the request resumes it; one without a key, which nothing can resume, is cancelled. With `Submit::hold` the audited message stays held (`draft`) until `release` moves it into the queue with a new undo window from then; a held message of a recorded item change that a stopped process never released is released by `Outbox::recover_held` at the next start, and one whose change was never recorded stays held.
- **Reasons** carry no addresses or content, so a front end may show them to the client.

## Limits and quirks

- `undo` works only from `queued`; a message being retried counts as having left the queue (`TooLate`).
- Without a route, the stored route is `direct` with no reasons: there was no router to explain.

## Tests

`cargo test -p holtmail-submission-outbox` runs the contract's conformance suite over real SQLite and the filesystem blob store with a transport that takes everything, plus a route skipping the leak guard, a person not being limited, the alias address being no leak, and an undone message freeing its place in the limit.
