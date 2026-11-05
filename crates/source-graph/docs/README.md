# holtmail-source-graph

Reads mail from Microsoft 365 (work and school), Outlook.com and Hotmail mailboxes through Microsoft Graph. It implements the [`holtmail-source`](../../source/docs/README.md) contract: mail folders become folders, each message's original MIME bytes come from its `$value`, and `isRead`, `flag` and `isDraft` become flags. Graph's JSON rendering of a message is never used, so nothing is rebuilt from fields and attached messages stay as they were sent. It never changes anything in the mailbox: it only sends `GET` requests. It is useful where a tenant turned IMAP off.

## Public API

| Item | Guarantees |
| --- | --- |
| `GraphSettings { api_url }` | Stored in the account's settings JSON next to `oauth`; other keys are ignored. `api_url` is the Graph endpoint with its version, `GRAPH_URL` (`https://graph.microsoft.com/v1.0`) when missing; only `https`, or `http` to a loopback address. `from_json` fails with `Config` |
| `GraphSource::new(&settings, credentials)` | Takes only `Credentials::AccessToken`, an OAuth access token with `Mail.Read`; a password is `Config`, as is an unusable address |
| `GraphSource::address()` | The signed-in mailbox's primary SMTP address, else its user principal name (`GET /me`, needs `User.Read`); `None` when Graph names neither |

The token is sent only to the origin of `api_url`: next-page links, delta links and stored links that point anywhere else are refused (`Protocol`, or "changed" for a stored link), and redirects are not followed.

## Config

```json
{ "oauth": { "provider": "microsoft" } }
```

Accounts are created by the server's OAuth flow with Graph scopes (see [holtmail-oauth](../../oauth/docs/README.md)); the refresh token is sealed like a password. `folders.exclude`, `sync_interval_minutes` and `min_age_days` in the same JSON work as for IMAP accounts.

## How the contract maps onto Graph

- **Folders.** `GET /me/mailFolders?includeHiddenFolders=true`, then `childFolders` of every folder with children, breadth first, every page followed through `@odata.nextLink`. Paths join display names with `/`. Search folders (`#microsoft.graph.mailSearchFolder`) are left out, since their messages live in other folders. Roles come from the well-known names `inbox`, `sentitems`, `drafts`, `deleteditems`, `junkemail` and `archive` (one `GET /me/mailFolders/{name}` each per session; a `404` means the mailbox has none). Every folder is selectable and none is a label. At most 10 000 folders.
- **Ids.** Every request carries `Prefer: IdType="ImmutableId"`, so a message keeps its id when it moves between folders.
- **UIDs.** Graph has none, so each message gets `receivedDateTime` in whole seconds shifted left by 22 bits, with the low 22 bits from a 64-bit FNV-1a hash of its immutable id, as in [holtmail-source-jmap](../../source-jmap/docs/README.md): stable across sessions, below `2^62`, ascending with the received time but not with the time a message entered the folder, so `select` reports `uids_ascend: false` and the sync looks for every UID it has not archived yet. A clash (same second, same 22 bits) skips the message with the larger id and is counted in a warning.
- **`UIDVALIDITY`.** 32 bits of the hash of the folder id.
- **Listing.** `uids` runs a fresh delta query, `GET /me/mailFolders/{id}/messages/delta?$select=id,receivedDateTime,isRead,flag,isDraft` with `Prefer: odata.maxpagesize=500`, following `@odata.nextLink` to the `@odata.deltaLink`. Delta pages are tied to one snapshot, so a message moved while the folder is being listed is neither missed nor counted twice. `@removed` entries are skipped. `fetch` and `flags` answer from that listing.
- **State.** The delta link at the end of the listing is the folder's `state()`. The sync stores it with the folder's cursor, and the next sync calls `unchanged_since(link)`: an answer with no items and no next page means nothing was added, removed or changed, flags included, and the new delta link is kept; the folder is then not listed at all. Any change, an expired link (`410`, or any other refusal) or a link to another host makes the sync list the folder again with a fresh delta query. One request per quiet folder per sync.
- **Messages.** `GET /me/messages/{id}/$value`, four at a time. A `404` (deleted since the listing) is left out.
- **Flags.** `isRead` is `\Seen`, `flag.flagStatus == "flagged"` is `\Flagged`, `isDraft` is `\Draft`. Folders have no mod-sequences, so the sync takes in flag changes of archived mail once a day (from the listing, without extra requests); `flags` with `changed_since` is a `Protocol` error.

## Throttling and errors

`429`, `503` and `504` are waited out: as long as `Retry-After` says (at most two minutes), else 2, 4, 8, 16 seconds; after five tries the request fails with `Connection`. `401` is `Auth`; `403` is `Auth` for the mailbox as a whole (folders, `/me`) and `Protocol` for one folder's listing or one message; other `5xx`, timeouts and broken connections are `Connection`; other refusals and malformed answers are `Protocol`. Error texts name the host, never ids, addresses or the token. Logs carry status codes and wait times only.

## Limits and not done

- JSON answers larger than 64 MiB and message downloads larger than 256 MiB are refused as a protocol error.
- Delta links are used to skip quiet folders, not to read changes: a folder that changed is listed in full, like the JMAP source does on every sync. Applying the changes themselves would need the Graph id of every archived message (to fetch retries and messages left for later, and to map `@removed` entries, which carry only an id), which the store does not keep.
- `$batch` is not used; the six well-known folder lookups are separate requests.
- A folder name containing `/` reads as a deeper path.
- Only the global cloud's endpoints are built in; national clouds would need `api_url` and their own sign-in endpoints.
- Tested only against the in-process fake Graph server in `src/tests.rs` (folders with hidden and child folders, a search folder, paged delta, delta links, an expired link, `$value`, throttling, a wrong token); not yet against Microsoft.
