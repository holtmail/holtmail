# holtmail-submit

The contract for handing a prebuilt message to a submission server. Holtmail is not an MTA: it never delivers to recipient hosts. A `Submitter` gives the exact bytes of a message and an explicit envelope to a submission server (SMTP submission, JMAP, Graph), which signs, queues and delivers it. The bytes are built and archived before they are handed over, so the transport must not change them. See [WRITE.md](../../../docs/WRITE.md) for where this sits in sending.

## Types

```rust
pub struct Envelope { pub mail_from: String, pub recipients: Vec<String> }
pub struct Refusal { pub permanent: bool, pub code: u16, pub text: String }
pub enum RecipientOutcome { Accepted, Refused(Refusal) }
pub struct RecipientResult { pub address: String, pub outcome: RecipientOutcome }
pub struct SubmitReport { pub recipients: Vec<RecipientResult> }
pub struct Limits { pub max_size: Option<u64>, pub smtputf8: bool }
```

- `Envelope::mail_from` is the return path; empty means the null return path `<>`. Bcc recipients are in `recipients` and never in the bytes.
- `Envelope::validate(message)` is what every submitter accepts, so callers can refuse a message before queueing it: at least one recipient; addresses with an `@` and no whitespace, control characters or angle brackets; a message that is not empty, has no NUL, uses `CRLF` for every line break and ends with `CRLF`. Anything else is `SubmitError::Invalid`.
- `Refusal::code` is the SMTP reply code, or the closest one for transports that do not speak SMTP; `permanent` is `true` for `5xx`. `text` is the server's words and may name addresses: never log it.
- `SubmitReport::recipients` has one entry per envelope recipient in envelope order, at least one of them accepted. `refused()` lists the refused ones.

## Submitter

```rust
#[async_trait]
pub trait Submitter: Send + Sync {
    async fn check(&self) -> Result<Limits, SubmitError>;
    async fn submit(&self, envelope: &Envelope, message: &[u8]) -> Result<SubmitReport, SubmitError>;
    fn limits(&self) -> Limits;
}
```

| Method | Guarantees |
| --- | --- |
| `check` | Connects and signs in, sends nothing, returns what the server said about itself and refreshes `limits`. |
| `submit` | **At most once**: one call offers the message once and never retries on its own. **Bytes unchanged**: the server receives exactly `message`, no header added or rewritten, line endings untouched. Input that fails `Envelope::validate` is `Invalid` before connecting; a message over a known `max_size` is `TooLarge` before it is sent. `Ok` means at least one recipient was taken; when every recipient is refused nothing is sent and the result is `RecipientsRefused`. |
| `limits` | What the most recent connection learned; empty before the first. |

Implementations are safe to share; concurrent calls do not interfere.

## Errors

| `SubmitError` | Meaning | Retryable |
| --- | --- | --- |
| `Auth(text)` | Credentials refused, or no usable sign-in mechanism | no |
| `Refused { permanent, code, text }` | The server refused the whole message (at `MAIL`, `DATA` or after the data) | when not permanent |
| `RecipientsRefused(results)` | Every recipient was refused; nothing sent | when one refusal is temporary |
| `Temporary(text)` | The session broke before the message was complete | yes |
| `Uncertain(text)` | Lost after the whole message was sent and before a clear answer | **never automatically** |
| `TooLarge { limit }` | Larger than the server takes (`limit` when it said one) | no |
| `Invalid(text)` | Envelope, message or configuration cannot be submitted (also: 8-bit or internationalized mail to a server without `8BITMIME` or `SMTPUTF8`) | no |
| `Unavailable(text)` | Server unreachable or would not start a session (refused connection, TLS failure, `421` greeting) | yes |

Every error except `Uncertain` guarantees the server did not take the message for any recipient, so sending it again cannot deliver it twice. `is_retryable()` says which are worth trying again later unchanged; `may_have_been_sent()` is true only for `Uncertain`, which a person resolves (the outbox state `uncertain` in WRITE.md).

## Implementing the contract

An implementation hands bytes over unchanged, maps its failures into `SubmitError` as in the table above, keeps per-recipient results in envelope order, and passes the conformance suite.

The suite is behind the `testing` feature:

```toml
[dev-dependencies]
holtmail-submit = { workspace = true, features = ["testing"] }
```

The implementation brings a fake server by implementing `testing::Receiver`: `start(Behavior) -> Fake` starts a fresh fake and returns a submitter pointed at it plus the `Deliveries` the fake records. Behaviors: `AcceptAll`, `RefuseRecipient(address)`, `TemporaryFailure`, `PermanentFailure` (both refuse once the message is complete), `DropAfterData` (records the message, then drops the connection without answering) and `SizeLimit(bytes)` (advertises and enforces the limit). `Receiver::profile()` says what the transport can express (`Profile { return_path, per_recipient }`, `Profile::FULL` by default): without `return_path` (Graph) the return path is not checked and the null one not sent; without `per_recipient` a refused recipient must refuse the whole message definitely and permanently, with nothing sent, instead of a partial result.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_submit_contract() {
    holtmail_submit::testing::conformance(&MyReceiver).await;
}
```

The suite checks that bytes with leading dots, a lone dot line, trailing whitespace and 8-bit text arrive unchanged; envelope order and the null return path; invalid input refused before sending; a partial and a total recipient refusal; the temporary, permanent and uncertain mappings and that an uncertain send was not repeated; and the size limit before sending, with a smaller message still going through.

## Tests

`cargo test -p holtmail-submit`: envelope and line-ending validation, and which errors are retryable.
