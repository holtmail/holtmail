# holtmail-submit-graph

A `holtmail_submit::Submitter` over Microsoft Graph: the prebuilt message goes base64-encoded as `text/plain` to `POST /me/sendMail` (or `/users/{id}/sendMail`), and Graph signs, queues, delivers and keeps the sent copy in Sent Items. It is the transport WRITE.md prefers for Microsoft accounts, whose tenants often turn SMTP AUTH off. It never builds a message; the only change it makes to the bytes is the `Bcc` field described below.

## API

```rust
pub const GRAPH_URL: &str;     // https://graph.microsoft.com/v1.0/
pub const MAX_SIZE: u64;       // 3 MiB, what fits in Graph's 4 MiB request after base64

#[async_trait]
pub trait TokenProvider: Send + Sync { async fn token(&self) -> Result<String, SubmitError>; }
pub struct StaticToken;        // StaticToken::new(token); Debug hides the value

pub struct GraphConfig { api_url, mailbox, max_size, timeout, roots }   // Default: GRAPH_URL, /me, MAX_SIZE, 120 s, no extra roots
GraphSubmitter::new(config, Arc<dyn TokenProvider>)?
```

| Field | Meaning |
| --- | --- |
| `api_url` | The Graph endpoint with its version; `https`, or plain `http` only to loopback (tests) |
| `mailbox` | `None` sends from the token's own mailbox (`/me`); `Some(id)` from `/users/{id}` (a shared mailbox, with `Mail.Send.Shared`) |
| `max_size` | The largest message handed over, counting the added `Bcc` field; capped at `MAX_SIZE` |
| `timeout` | The whole request; connecting is capped at 30 s |
| `roots` | Certificates trusted besides the built-in roots |

`GraphSubmitter::new` refuses (`Invalid`) another scheme, plain `http` to a non-loopback host, an empty mailbox or one with whitespace, a zero `max_size` and an extra root that does not parse.

`TokenProvider::token` is asked once per `check` and `submit`, before any request, so refreshing the token stays in `apps/server`. Its error is returned unchanged: `Auth` for a revoked sign-in, `Unavailable` for a token endpoint that is down. The token needs `Mail.Send`; `check` also needs read access to the mailbox's Sent Items (`Mail.ReadBasic` or `Mail.Read`).

## Guarantees

- **At most once.** One `submit` makes at most one `sendMail` request and never retries.
- **Bytes unchanged, except Bcc.** Graph takes the recipients from the To, Cc and Bcc fields of the MIME, not from an envelope. When the envelope has recipients those fields do not name (the Bcc recipients, which never are in Holtmail's bytes), the bytes handed to Graph get one `Bcc:` field in front, naming them in envelope order, folded one per line. Nothing else is added, removed or rewritten. The archived bytes and the outbox's blob stay without Bcc; Exchange removes the Bcc field from the copies recipients get. Exchange may rewrite the message it sends and stores in Sent Items; WRITE.md links that copy back by `Message-ID`.
- **Exact recipients or nothing.** A To, Cc or Bcc address that is not in the envelope would receive the message through Graph anyway, so `submit` refuses it with `Invalid` before sending (a "send again to the refused" with the original headers cannot go through Graph). A message that already has a `Bcc` field while the envelope names more recipients is `Invalid` too, and so is an address in those fields that cannot be read. Envelope addresses compare ASCII case-insensitively.
- **No return path.** Graph has no envelope sender: bounces go to the mailbox, whatever `Envelope::mail_from` says, and the null return path cannot be expressed. `mail_from` is ignored.
- **No per-recipient results.** Graph takes a message for all its recipients or refuses it. `Ok` reports every envelope recipient `Accepted`; recipients that do not exist come back later as bounces (DSN matching in the outbox), never as a `RecipientsRefused`.
- **Limits are known up front.** Graph advertises no size: `limits()` and `check()` return the configured `max_size` from the start. A message over it is `TooLarge` before the token is asked for.
- **Sent copy.** `saveToSentItems` is `true` by default and cannot be passed with a MIME body, so Graph always keeps the sent copy: the identity's sent-copy policy for a Graph transport is `provider`.
- `check` reads the Sent Items folder id (`GET …/mailFolders/sentitems?$select=id`) and sends nothing.
- Logs: none. Error texts carry Graph's error code and message, which may name addresses; callers must not log them.

## Error mapping

| Graph answer | `SubmitError` |
| --- | --- |
| `202` (any `2xx`) | `Ok`, every recipient accepted |
| Connection refused, TLS failure, connect timeout | `Unavailable` |
| Connection lost or timed out once the request was being sent | `Uncertain` |
| `401` | `Auth` |
| `403 ErrorSendAsDenied` / `ErrorMessageSubmissionBlocked` | `Refused` permanent `550` / `554` |
| other `403` (no `Mail.Send` consent, no access to the mailbox) | `Auth` |
| `413`, or the code `ErrorMessageSizeExceeded` | `TooLarge { limit: None }` |
| `429`, `503` | `Temporary`, with `retry after N s` in the text when `Retry-After` gives seconds |
| `500`, `502`, `504` and other `5xx` | `Uncertain`: Graph had the whole message and did not say whether it took it |
| `404` | `Invalid` (no such mailbox) |
| `400 ErrorInvalidRecipients` / other `4xx` | `Refused` permanent `550` / `554` |

`check` maps `401` and `403` to `Auth`, `404` to `Invalid`, `429` and `5xx` and connection failures to `Unavailable`.

## Conformance

`testing::Receiver` is implemented by an in-process fake `sendMail` on a raw TCP listener (`src/tests.rs`) that behaves like Graph: recipients from the header fields, the mailbox in the path as sender, the `Bcc` field removed from what it records, `400 ErrorInvalidRecipients` for a refused recipient, `503` with `Retry-After`, `403 ErrorSendAsDenied`, `413` over the limit, and for `DropAfterData` it records the message and closes the connection after reading the body.

The shared suite, `holtmail_submit::testing::conformance`, runs in `keeps_the_submit_contract` with `Profile { return_path: false, per_recipient: false }`: Graph has no envelope sender, and one refused recipient refuses the whole message.

## Limits

- Messages over 3 MiB need a draft and an upload session; not built.
- No `Prefer: IdType` or immutable ids: the sent message's Graph id is not returned by `sendMail`.
- Graph's `Retry-After` reaches the caller only as text, since `SubmitError::Temporary` has no field for it.

## Tests

`cargo test -p holtmail-submit-graph`: the Bcc reconstruction (`bcc.rs`: nothing added when the headers name everyone, one folded field otherwise, groups, refusing header recipients outside the envelope and a second `Bcc`), base64 MIME to `/users/{id}/sendMail` and `/me/sendMail`, bytes kept, invalid input never sent, every row of the error mapping with no retry, the size limit and its cap, a token asked for on every call (a stale token is `Auth`, a provider error sends nothing), `check` mappings and an unreachable endpoint, and refused configurations. The shared conformance suite runs with the Graph profile.
