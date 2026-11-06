# holtmail-oauth

Signing in to Gmail and Microsoft mailboxes with OAuth 2.0 (authorization code with PKCE): provider presets, the authorize address, reading the answer the provider redirects to, exchanging the code and refreshing tokens. It does no I/O itself: the token endpoint is reached through the `TokenEndpoint` contract, implemented by [holtmail-oauth-http](../../oauth-http/docs/README.md), so everything here is tested without a network.

## Providers

| `Provider` | `as_str()` | Endpoints | Scope | IMAP |
| --- | --- | --- | --- | --- |
| `Google` | `google` | `https://accounts.google.com/o/oauth2/auth`, `https://www.googleapis.com/oauth2/v3/token` | `https://mail.google.com/` | `imap.gmail.com:993` |
| `Microsoft` | `microsoft` | `https://login.microsoftonline.com/common/oauth2/v2.0/{authorize,token}` | `https://outlook.office.com/IMAP.AccessAsUser.All offline_access` | `outlook.office365.com:993` |

The scopes above are for `Access::Imap`. `Provider::scope(access)` gives them per `Access`:

| `Access` | `as_str()` | Google | Microsoft |
| --- | --- | --- | --- |
| `Imap` (default) | `imap` | `https://mail.google.com/` | `https://outlook.office.com/IMAP.AccessAsUser.All offline_access` |
| `Graph` | `graph` | `None`: Google has no Graph | `https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/User.Read offline_access` |
| `ImapDav` | `imap_dav` | `https://mail.google.com/ https://www.googleapis.com/auth/carddav https://www.googleapis.com/auth/calendar.readonly` | `None`: Microsoft contacts and calendars come through Graph |

A code and a refresh token only work for the scopes they were granted for: an IMAP token is refused by Graph and the other way round, so a sign-in and every refresh of its token must ask for the same access. `Access::parse` reads `as_str` back, and `covers(other)` says whether a grant for one also grants the other (`ImapDav` covers `Imap`).

`ImapDav` is IMAP plus Google's CardDAV and CalDAV, for accounts that also archive contacts and calendars. Google has no read-only CardDAV scope (CardDAV accepts only `auth/carddav`), so contacts use it; Holtmail still only reads. Calendars use `auth/calendar.readonly` with an own client. With Thunderbird's client `for_access(ImapDav)` asks for `auth/calendar` instead, the scope Thunderbird itself requests: a borrowed client is verified by Google only for the scopes its owner registered, so asking it for a narrower one risks an "unverified app" block (not checked against Google).

Thunderbird's Microsoft client declares Graph scopes too (`User.Read`, `MailboxFolder.ReadWrite`, `Mail.ReadWrite`, `Mail.Send` with `offline_access`, behind its `mail.graph.enabled` preference in `OAuth2Providers.sys.mjs`). Holtmail asks for less: `Mail.Read` (it only reads) and `User.Read` (the mailbox address), relying on Microsoft's incremental consent for a scope the registration does not list. Whether Microsoft grants `Mail.Read` to the borrowed client has not been checked against Microsoft; an own client with those two delegated permissions certainly works.

`Provider::for_address` knows `gmail.com`, `googlemail.com` (Google) and `outlook.com`, `hotmail.com`, `live.com`, `msn.com`, `office365.com` (Microsoft); custom domains hosted by either return `None`. `parse` reads `as_str` back; `name()` is `Google` or `Microsoft`.

## Clients

```rust
pub struct Client { provider, client_id, client_secret, authorize_url, token_url, scope, redirect_url }
```

- `Client::thunderbird(provider)` is Thunderbird's public installed-app client, copied from `mailnews/base/src/OAuth2Providers.sys.mjs` in comm-central: the Google client id with its public client secret, the Microsoft client id without a secret. The redirect is `LOOPBACK_REDIRECT` (`http://127.0.0.1`), the default Thunderbird uses (Thunderbird adds a port of its own; loopback redirects are matched on any port). Nothing listens there, so the browser shows an error page whose address holds the answer.
- `Client::own(provider, client_id, client_secret, redirect_url)` is a client the owner registered; endpoints and scopes come from the provider.
- `for_access(access)` is the same client asking for the scopes of `access` (`None` for Google and `Graph`, and for Microsoft and `ImapDav`).
- `is_thunderbird()` tells them apart. `Debug` never prints the secret.

| Method | Guarantees |
| --- | --- |
| `authorize(login_hint)` | A fresh random `state` and PKCE verifier (32 random bytes each, base64url) and the sign-in address: `response_type=code`, `client_id`, `redirect_uri`, `scope`, `state`, `code_challenge` (S256), `login_hint` when given; Google also gets `access_type=offline&prompt=consent` so it always returns a refresh token. The verifier never appears in the address. |
| `exchange(endpoint, code, verifier)` | Posts `grant_type=authorization_code` with the code, redirect and verifier (and the client id and secret). |
| `refresh(endpoint, refresh_token)` | Posts `grant_type=refresh_token` (Microsoft also gets the scope). The answer may carry a new refresh token that replaces the old one. |

`Tokens { access_token, refresh_token, expires_in }`: `refresh_token` is `None` when the provider sent none; `expires_in` is one hour when the provider does not say. `Debug` never prints the tokens.

## Reading the redirect

`read_redirect(pasted)` reads `code` and `state` from what the person pasted: the whole address (`http://127.0.0.1/?state=…&code=…&scope=…`), only its query (`?code=…` or `code=…`), with or without spaces, quotes or angle brackets around it, from the query or the fragment. An answer with `error` is `OAuthError::Denied` with the provider's description (`access_denied` reads "access was refused at the sign-in page"); anything without both values is `OAuthError::Redirect`. The returned `Redirect` hides its values from `Debug`.

## TokenEndpoint

```rust
#[async_trait]
pub trait TokenEndpoint: Send + Sync {
    async fn post_form(&self, url: &str, form: &[(String, String)]) -> Result<TokenAnswer, OAuthError>;
}
```

An implementation posts the form as `application/x-www-form-urlencoded` and returns the status and body whatever the status. It contacts only `https` URLs, plain `http` only on loopback (for tests), and fails with `OAuthError::Connection` when no answer came. No error repeats the form, which holds codes, secrets and tokens.

Run the conformance suite from an implementation's tests with the `testing` feature: `holtmail_oauth::testing::conformance(&endpoint).await`. It checks, without leaving the machine, that a closed loopback port, a plain-`http` address elsewhere, another scheme and a non-URL all fail with `Connection` and that no error repeats the form. `testing::FakeTokenEndpoint` answers from a queue (`tokens`, `error`, `answer`, `unreachable`) and records every request (`requests`, `last(key)`).

## Errors

| `OAuthError` | Meaning | Retry |
| --- | --- | --- |
| `Redirect` | The pasted text is not the provider's answer | Paste again |
| `Denied` | Refused at the sign-in page | Sign in again |
| `Revoked` | `invalid_grant`: the code or refresh token is no longer accepted (revoked, expired, password changed) | Only a new sign-in helps |
| `Rejected` | Any other OAuth error, e.g. `invalid_client` when a borrowed client is blocked | No |
| `Connection` | No answer from the token endpoint | Yes, later |
| `Protocol` | An unexpected answer: a non-JSON error, a 5xx, no access token, `server_error`, `temporarily_unavailable` | Yes, later |
| `Random` | The system had no random bytes | No |

`is_transient()` is true for `Connection` and `Protocol`. Messages never hold tokens, codes or secrets; provider descriptions are cut to their first line.

## Risks of the borrowed client

The consent screen names Thunderbird, the provider can block or rate-limit the borrowed client at any time, and whether this fits the provider's terms is the owner's call. Registering an own client is described in the [server docs](../../../apps/server/docs/README.md#gmail-and-microsoft-sign-in-oauth).
