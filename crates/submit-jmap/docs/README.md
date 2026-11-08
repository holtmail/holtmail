# holtmail-submit-jmap

A `holtmail_submit::Submitter` over JMAP (RFC 8620, RFC 8621 §7). It uploads the exact bytes of a prebuilt message as a blob, imports them with `Email/import` and creates an `EmailSubmission` with the explicit envelope in the same request. `onSuccessUpdateEmail` moves the email to Sent and clears `$draft`, so the provider keeps the sent copy itself (sent-copy policy `provider`). It never builds or rewrites a message.

## API

```rust
pub enum JmapSecret { Password(String), Token(String) }   // Debug hides the value
pub struct JmapSubmitConfig { url, username, secret, address, timeout }

JmapSubmitConfig::new(url, username, secret)   // no fixed address, 120 s timeout
JmapSubmitter::new(config)?                    // trusts the web PKI roots
    .with_roots(&TlsRoots)?                    // and extra roots from holtmail-core
session_url(input)?                            // the session resource for what someone typed
```

| Field | Meaning |
| --- | --- |
| `url` | The session resource (`https://api.fastmail.com/jmap/session`) or a server address, where `/.well-known/jmap` is asked and its redirects followed without credentials. `https` only, plain `http` only to loopback |
| `username`, `secret` | `Password` signs in with HTTP Basic as `username` (required); `Token` sends a bearer token and no user name |
| `address` | The address whose JMAP identity submits; `None` uses the envelope's return path, and the first identity for the null return path |
| `timeout` | The limit for every HTTP request |

`JmapSubmitter::new` refuses (`Invalid`) an unusable address, credentials inside the address, and a password without a user name.

## Guarantees

- **`check`** fetches the session and requires the `urn:ietf:params:jmap:submission` capability for the account, a mailbox with role `sent` and, when `address` is set, an identity for it. It sends and changes nothing and returns the limit.
- **Limits:** `max_size` is the smaller of the core `maxSizeUpload` and the account's `SIZE` submission extension. Every call asks for the session again, so `limits()` is what the latest call saw; a message over it is `TooLarge` before the upload.
- **Identity:** an identity whose `email` equals the address (ignoring case), else a wildcard identity `*@domain` for its domain. No identity is `Invalid` before anything is uploaded.
- **Steps of `submit`:** session; `Mailbox/get` and `Identity/get` in one request; upload of the bytes as `message/rfc822`; then one request with `Email/import` (into Drafts with `$draft` and `$seen`, or into Sent when the account has no Drafts) and `EmailSubmission/set` referring to it by creation id, with `mailFrom` and `rcptTo` from the envelope and no SMTP parameters. The null return path is sent as an empty `mailFrom`.
- **At most once:** the submission request is sent once. Recipients the server lists in an `invalidRecipients` error were not sent to; the submission is created again for the remaining recipients only, for the already imported email. Nothing else is repeated.
- **Copies:** on a definite failure after the import the imported email is destroyed again (best effort), so no unsent copy stays in Drafts. On `Uncertain` it is left alone. When every recipient is refused through `deliveryStatus` the submission exists and the email stays in Sent.
- **Per-recipient results:** recipients listed in `invalidRecipients` are refused with `550`; a `deliveryStatus` entry with `delivered: "no"` and a `4xx`/`5xx` `smtpReply` is refused with that code and reply. Others are accepted. `undoStatus: "canceled"` is a permanent `Refused` `554`.
- Logs: none. Errors carry server descriptions, which may name addresses; callers must not log them.

## Error mapping

| Where | Failure | `SubmitError` |
| --- | --- | --- |
| Session, mailboxes, identities, upload | not reachable, broken connection, timeout, `5xx`, malformed answer | `Unavailable` |
| Any request | `401`, `403` | `Auth` |
| Any request | `429`, `503` | `Temporary` |
| Session, upload | `404`, redirect of the session with credentials, other `4xx`; upload `413` | `Invalid`; `TooLarge` |
| Session | no mail or submission capability, no account, no Sent mailbox, no identity | `Invalid` |
| Submission request | not reachable before sending | `Unavailable` |
| Submission request | anything after it may have been sent: broken connection, timeout, other `5xx`, malformed or missing answer, method error `serverFail` | `Uncertain` |
| `EmailSubmission/set` `notCreated` | `forbiddenMailFrom`, `forbiddenFrom`, `forbiddenToSend`, `forbidden` | `Refused` permanent `550` |
| | `tooLarge` | `TooLarge` with `maxSize` or the session limit |
| | `tooManyRecipients`, `overQuota` | `Refused` permanent `552` |
| | `rateLimit` | `Temporary` |
| | `noRecipients`, `invalidProperties` | `Invalid` |
| | `invalidRecipients` | per-recipient refusals; all refused: `RecipientsRefused` |
| | anything else (`invalidEmail`, ...) | `Refused` permanent `554` |
| `Email/import` `notCreated` | the same table | as above |
| Method error | `serverUnavailable` / `forbidden`, `accountReadOnly` / other | `Temporary` / `Refused` `550` / `Invalid` |

## Quirks and limits

- The session code (well-known resolution, address templates, the https rule) is a small copy of `source-jmap`'s, since implementations never depend on each other.
- The identity is chosen by address, not by parsing the message's `From`; a provider that checks `From` against the identity answers `forbiddenFrom`, which is a permanent refusal.
- No delayed send (`FUTURERELEASE`), no SMTP parameters in the envelope, no undo through `EmailSubmission` `undoStatus`; Holtmail's outbox holds messages itself.
- Delivery status is read only from the `created` answer; later changes (bounces) are not followed here.

## Tests

`cargo test -p holtmail-submit-jmap`: the `holtmail-submit` conformance suite against an in-process axum JMAP fake (session, well-known redirect, upload, `Mailbox/get`, `Identity/get`, `Email/import`, `EmailSubmission/set` with `onSuccessUpdateEmail`, `Email/set` destroy; a dropped answer for the drop after the message), plus: the sent copy ends in Sent without `$draft` and with the exact bytes, import into Sent without Drafts, destroying the copy after a permanent refusal, keeping it after `serverFail`, refusals from `deliveryStatus`, bearer tokens and wrong passwords, `check` without submission and with matching, wildcard and missing identities, an unknown return path refused before the upload, an unreachable server, refused configurations and the set-error mapping.
