# holtmail-auth

Signing in to Holtmail: Argon2id password hashes, session tokens, the one-time owner setup link and login rate limiting. Everything durable goes through a [`UserStore`](../../store/docs/README.md); the setup token and the failure counts live in memory and end with the process. This crate is not a contract: it has one implementation and depends only on `holtmail-store`.

## Usage

```rust
let auth = Arc::new(Auth::new(store.clone(), Options::default())?);
if let Some(token) = auth.setup_token().await? {
    // show `<public url>/setup?token=<token>` to the operator
}
let signed_in = auth.login(client_ip, "ada@example.com", "analytical engine").await?;
let user = auth.session(&signed_in.token).await?;
```

Methods hash on Tokio's blocking pool, so they run inside a Tokio runtime.

## Auth

| Method | Guarantees |
| --- | --- |
| `setup_needed()` | `true` while the store has no users |
| `setup_token()` | While there are no users: a random token, created on the first call and the same on later calls of this process. `None` once a user exists |
| `complete_setup(token, signup)` | Creates the `owner` and signs them in. The token is compared in constant time, works once, only while there are no users and only after `setup_token` created it; concurrent calls create at most one owner. Otherwise `InvalidSetupToken` |
| `create_user(signup, role)` | Validates and hashes; `EmailTaken` for a known email (any letter case), `Invalid` for bad input |
| `set_password(user, password)` | Validates, replaces the hash and ends every session of the user |
| `login(client_ip, email, password)` | Starts a session. Unknown emails, users without a password and wrong passwords all give `InvalidCredentials` and take the same time (a decoy hash is checked). After `max_failures` failures for the same client address and email within `failure_window`, every attempt gives `TooManyAttempts { retry_after }` without checking the password until the oldest failure leaves the window; a success clears the count |
| `session(token)` | The user of a live session; `None` for unknown, malformed or expired tokens |
| `logout(token)` | Ends the session; idempotent |
| `prune()` | Removes expired sessions and stale failure counts |

`Signup { email, name, password }`: the email is trimmed and needs one `@` with text on both sides and no spaces (at most 254 bytes); the name is trimmed, 1 to 100 characters; the password needs at least 10 characters and at most 1024 bytes (`MIN_PASSWORD_CHARS`, `MAX_PASSWORD_BYTES`).

`SignedIn { user, token, expires_at }`: `token` is what the client keeps; it is never stored or logged.

## Options

| Field | Default |
| --- | --- |
| `session_lifetime` | 30 days, fixed from sign-in |
| `max_failures` | 10 |
| `failure_window` | 15 minutes |
| `hash_cost` | Argon2id, 19 MiB, 2 passes, 1 lane (OWASP) |

Stored hashes are PHC strings and carry their own cost, so changing `hash_cost` only affects new hashes; old ones still verify.

## Tokens

Session and setup tokens are 32 bytes from the operating system's random source, encoded as 43 characters of unpadded base64url. The store keeps only the BLAKE3 hash of a session token (`token_hash`), so a copy of the database cannot be used to sign in.

## Limits

- Failure counts and the setup token are per process: a restart forgets them and prints a new setup link.
- The rate limit keys on the client address the caller passes. Behind a reverse proxy that is the proxy's address, so the limit then applies per email.
- Sessions do not slide: a session ends `session_lifetime` after sign-in.

## Errors

`AuthError`: `InvalidCredentials`, `TooManyAttempts { retry_after }`, `InvalidSetupToken`, `EmailTaken`, `Invalid(reason)`, `Hashing`, `Random`, `Store(StoreError)`. Messages never contain passwords or tokens.
