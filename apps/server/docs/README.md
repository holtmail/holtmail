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

`serve` syncs every enabled account one after another, every `sync_interval_minutes`, and removes expired sessions every hour. Syncs and imports started from the web and from the schedule share one runner that keeps at most one run per account and remembers the state of the last one (shown in the web app) until the server restarts. Uploaded files wait in `<data_dir>/uploads` and are removed once imported.

## Signing in

While there are no users, `serve` logs a one-time setup link at start:

```
No users yet. Open http://127.0.0.1:8080/setup?token=... to create the owner; the link works once.
```

Whoever opens it creates the owner account and is signed in. The link stops working once any user exists and a new one is printed on every start until then, so keep the log private. The link uses `public_url` when set, else the listen address (`localhost` for `0.0.0.0`).

On headless installs, `user add` creates users directly; the first user becomes `owner` unless `--role` says otherwise, later ones `member`. `user add` and `user password` read the password from `HOLTMAIL_PASSWORD` or the first line of stdin; passwords need at least 10 characters. Passwords are stored as Argon2id hashes and never logged.

`import` creates the archive and an import account (named after the file, or `--account`) when missing, maps folder names to roles (`Sent`, `Отправленные`, `[Gmail]/All Mail`, ...), commits the index every 500 messages and prints progress. Running it again on the same files adds nothing: messages are recognised by their bytes. Files that are not messages are counted as failed and skipped.

## Config

Optional TOML file (`-c holtmail.toml` or `HOLTMAIL_CONFIG`). Every key has a default:

```toml
listen = "127.0.0.1:8080" # HOLTMAIL_LISTEN
data_dir = "data" # HOLTMAIL_DATA_DIR: holtmail.db, index/, blobs/
web_dir = "apps/web/dist" # HOLTMAIL_WEB_DIR: serve the built web app; unknown paths get index.html
sync_interval_minutes = 15 # 0 turns scheduled sync off

[blobs]
kind = "fs" # blobs in <data_dir>/blobs
# kind = "fs_at", path = "/mnt/mail"
# kind = "s3", bucket = "holtmail", region = "eu-central-1",
#   endpoint = "https://s3.example.com", prefix = "archive", path_style = true
```

S3 credentials come only from `HOLTMAIL_S3_ACCESS_KEY_ID` and `HOLTMAIL_S3_SECRET_ACCESS_KEY`, never from the file. Unknown keys are an error, so a typo does not silently fall back to a default.

## Data directory

| Path          | What                                                 | Back up                            |
| ------------- | ---------------------------------------------------- | ---------------------------------- |
| `holtmail.db` | Metadata (SQLite)                                    | Yes                                |
| `blobs/`      | Message and attachment bytes, unless blobs are on S3 | Yes                                |
| `secret.key`  | The key sealing account passwords                    | Yes, separately                    |
| `index/`      | Search index                                         | No: `holtmail reindex` rebuilds it |
| `uploads/`    | Uploaded files waiting to be imported                | No                                 |

## Logs

`RUST_LOG` sets the level (`info` by default). Logs carry counts, ids and errors, never message content, addresses, passwords or session tokens. The one exception is the setup link above, which is how the owner is bootstrapped.

## Behind a reverse proxy

Terminate TLS in the proxy, pass `Host` through unchanged, set `X-Forwarded-Proto`, and set `public_url` to the https address. Writes are refused when the browser's `Origin` does not match `Host`. Sign-in attempts are rate-limited per client address and email; behind a proxy every client shares the proxy's address, so the limit is effectively per email.
