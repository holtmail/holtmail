# holtmail-alias-fastmail

Fastmail Masked Email through JMAP (`https://www.fastmail.com/dev/maskedemail`). `FastmailDirectory` manages an account's masked addresses. There is no detector: Fastmail delivers mail to a masked address without rewriting it, so messages are matched to masked addresses by their recipients once the directory has listed them.

## Directory

`FastmailDirectory::new(session_url, token, roots)`, `DEFAULT_URL` = `https://api.fastmail.com/jmap/session`. The token is an API token with the Masked Email scope, sent as `Bearer`. The session is read on first use; its `apiUrl` must be https (or loopback) and the session must name a primary account for Masked Email, else `Invalid` says the token lacks the scope.

| Call | JMAP |
| --- | --- |
| `aliases` | `MaskedEmail/get` with `ids: null`; `deleted` ones left out; `pending` ones count as on |
| `options` | `MaskedEmail/get` with no ids, to check the token; one domain entry stands for Fastmail's own |
| `create` | `MaskedEmail/set` create with `state: enabled`, `description`, `forDomain` and, for a chosen prefix, `emailPrefix` (at most 64 of `a-z`, `0-9`, `_`; Fastmail adds a word) |
| `update` | `MaskedEmail/set` update of `state` (`enabled`/`disabled`), `description`, `forDomain` |
| `delete` | `MaskedEmail/set` with `state: deleted`: mail to it bounces |

Capabilities: `create_random`, `create_custom`, `note`, `for_domain`, `switch`, `delete`. SetErrors: `notFound` → `NotFound`, `rateLimit` → `Unavailable`, `forbidden` → `Auth`, others → `Invalid`.

## Tests

The conformance suite against an in-process JMAP fake, plus deleted masks, prefixes Fastmail would refuse and a token without the scope.
