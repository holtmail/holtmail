# holtmail (apps/server)

The `holtmail` binary and the only place that chooses implementations: it reads the config, opens the blob store, the metadata store and the search index, and hands them to the API and the commands as contracts.

## Commands

```bash
holtmail serve                                   # API and web app
holtmail import --archive Personal ~/mail.mbox   # mbox, Maildir, .eml, zip or a directory of them
holtmail archives                                # archives with totals
holtmail account add-imap --archive Personal --host imap.example.com --username ada@example.com
holtmail account list                            # accounts of every archive
holtmail sync [--account <id>]                   # archive new mail now
holtmail user add --email ada@example.com --name "Ada Lovelace" [--role owner|admin|member]
holtmail user password --email ada@example.com   # set a password, end the user's sessions
holtmail user list                               # users with their roles
holtmail reindex                                 # rebuild the search index from stored messages
holtmail openapi                                 # the API description as JSON
```

`account add-imap` reads the password from `HOLTMAIL_IMAP_PASSWORD` or the first line of stdin, seals it with the instance key (`<data_dir>/secret.key`, bound to the new account's id) and stores only the sealed bytes. `--security tls|starttls|plain`, `--port`, `--name` and repeated `--exclude <folder path>` are optional. `sync` prints one line per folder: new, known, removed, failed and any error.

Everything the commands do can also be done in the web app by the owner or an admin: archives, IMAP accounts, uploads of mailbox files, syncing now, people and their access. The CLI stays for scripts and headless installs.

`serve` checks every 30 seconds which enabled accounts are due and syncs them one after another, and removes expired sessions every hour. An account is due at start and then every `sync_interval_minutes` after its last run ended; each account can have its own interval (`sync_interval_minutes` in its settings, set from the web app), `0` meaning only on request. The server-wide interval comes from the config when set there, else from the web app (stored in the settings), else 15 minutes. Changing an account's server, user name or excluded folders from the web app signs in with the new settings first; the account keeps its archive, and messages it already has are recognized by their bytes, so nothing is stored twice. Syncs and imports started from the web and from the schedule share one runner that keeps at most one run per account and remembers the state of the last one (shown in the web app) until the server restarts. Uploaded files wait in `<data_dir>/uploads` and are removed once imported.

Remote images go through [`holtmail-fetch-http`](../../../crates/fetch-http/docs/README.md), which only contacts public addresses, optionally through an outbound proxy (`image_proxy` in the config, or set by an owner or admin in the web app, stored sealed with `secret.key`; the config wins). The proxy can change while the server runs. For archives with "Save remote images" on, [`holtmail-remote`](../../../crates/remote/docs/README.md) saves the images of new messages after every sync or import that brought any and once at start; mail archived before saving was turned on, and failed images, only when asked from the web app. One remote image task per archive at a time. The web reader's image proxy uses the same fetcher.

Syncs, imports and remote image work run as tasks with progress and a log (counts, folder names and reasons, never message content), listed in the web app under Settings → Tasks. Remote image tasks can be stopped between messages. The last 50 finished tasks are kept in memory until the server restarts; logs keep their first 50 and last 450 lines.

## Signing in

While there are no users, `serve` logs a one-time setup link at start:

```
No users yet. Open http://127.0.0.1:8080/setup?token=... to create the owner; the link works once.
```

Whoever opens it creates the owner account and is signed in. The link stops working once any user exists and a new one is printed on every start until then, so keep the log private. The link uses `public_url` when set, else the listen address (`localhost` for `0.0.0.0`).

On headless installs, `user add` creates users directly; the first user becomes `owner` unless `--role` says otherwise, later ones `member`. `user add` and `user password` read the password from `HOLTMAIL_PASSWORD` or the first line of stdin; passwords need at least 10 characters. Passwords are stored as Argon2id hashes and never logged. The `user` commands open only the metadata store, so they work while `serve` is running; in Docker, run them with `docker exec <container> holtmail user ...`.

`import` creates the archive and an import account (named after the file, or `--account`) when missing, maps folder names to roles (`Sent`, `Отправленные`, `[Gmail]/All Mail`, ...), commits the index every 500 messages and prints progress. Running it again on the same files adds nothing: messages are recognised by their bytes. Files that are not messages are counted as failed and skipped.

## Config

Optional TOML file (`-c holtmail.toml` or `HOLTMAIL_CONFIG`). Every key has a default:

```toml
listen = "127.0.0.1:8080" # HOLTMAIL_LISTEN
data_dir = "data" # HOLTMAIL_DATA_DIR: holtmail.db, index/, blobs/
web_dir = "apps/web/dist" # HOLTMAIL_WEB_DIR: serve the built web app; unknown paths get index.html
# sync_interval_minutes = 15 # HOLTMAIL_SYNC_INTERVAL: accounts without their own interval; 0 syncs them only on request; when set, the web app cannot change it
# image_proxy = "socks5h://127.0.0.1:9050" # HOLTMAIL_IMAGE_PROXY: outbound proxy for remote images

[blobs]
kind = "fs" # blobs in <data_dir>/blobs
# kind = "fs_at", path = "/mnt/mail"
# kind = "s3", bucket = "holtmail", region = "eu-central-1",
#   endpoint = "https://s3.example.com", prefix = "archive", path_style = true
```

S3 credentials come only from `HOLTMAIL_S3_ACCESS_KEY_ID` and `HOLTMAIL_S3_SECRET_ACCESS_KEY`, never from the file. Unknown keys are an error, so a typo does not silently fall back to a default.

## Data directory

| Path          | What                                                                     | Back up                            |
| ------------- | ------------------------------------------------------------------------ | ---------------------------------- |
| `holtmail.db` | Metadata (SQLite)                                                        | Yes                                |
| `blobs/`      | Message, attachment and saved remote image bytes, unless blobs are on S3 | Yes                                |
| `secret.key`  | The key sealing account passwords                                        | Yes, separately                    |
| `index/`      | Search index                                                             | No: `holtmail reindex` rebuilds it |
| `uploads/`    | Uploaded files waiting to be imported                                    | No                                 |

## Logs

`RUST_LOG` sets the level (`info` by default). Logs carry counts, ids and errors, never message content, addresses, passwords or session tokens. The one exception is the setup link above, which is how the owner is bootstrapped.

## Behind a reverse proxy

Terminate TLS in the proxy, pass `Host` through unchanged, set `X-Forwarded-Proto`, and set `public_url` to the https address. Writes are refused when the browser's `Origin` does not match `Host`. Sign-in attempts are rate-limited per client address and email; behind a proxy every client shares the proxy's address, so the limit is effectively per email.
