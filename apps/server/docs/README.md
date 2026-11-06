# holtmail (apps/server)

The `holtmail` binary and the only place that chooses implementations: it reads the config, opens the blob store, the metadata store and the search index, and hands them to the API and the commands as contracts.

## Commands

```bash
holtmail serve                                   # API and web app
holtmail import --archive Personal ~/mail.mbox   # mbox, Maildir, .eml, .vcf, .ics, zip (Google Takeout too) or a directory of them
holtmail archives                                # archives with totals
holtmail account add-imap --archive Personal --host imap.example.com --username ada@example.com
holtmail account add-oauth --archive Personal --email ada@gmail.com [--provider google|microsoft]
holtmail account add-graph --archive Work --email ada@contoso.example.com
holtmail account add-jmap --archive Personal --url https://api.fastmail.com/jmap/session --token-stdin
holtmail account add-jmap --archive Personal --url jmap.example.com --username ada@example.com
holtmail account list                            # accounts of every archive
holtmail account edit <id> [--name --host --port --security --username --url --auth --exclude ... | --clear-exclude] \
  [--interval <minutes>|0|default] [--min-age-days <days>|0] [--pause | --resume] [--password-stdin] \
  [--contacts-calendars on|off [--dav-url <url> | --carddav-url <url> --caldav-url <url>]]
holtmail sync [--account <id>]                   # archive new mail now, then contacts and calendars where on
holtmail user add --email ada@example.com --name "Ada Lovelace" [--role owner|admin|member]
holtmail user password --email ada@example.com   # set a password, end the user's sessions
holtmail user list                               # users with their roles
holtmail reindex [--refresh-attachments]         # rebuild the search index, the listed fields, calendar instances and search documents of contacts and events, events from invitations in mail, and people
holtmail export --archive Personal [--folder INBOX | --label Travel | --query "from:ada"] \
  [--format mbox|zip] --output personal.mbox     # write messages as mbox or a zip of .eml files
holtmail export --archive Personal --contacts [--collection Work | --person <id>] \
  [--include-removed] [--format vcf|zip] --output contacts.vcf   # contacts: latest versions, or every version
holtmail export --archive Personal --calendar [--collection Team] [--from 2025-03-01 --to 2025-04-01] \
  [--include-removed] [--format ics|zip] --output team.ics       # calendar items the same way
holtmail backup --output holtmail.tar [--skip-blobs] [--include-secret-key]
holtmail restore --input holtmail.tar --data-dir /srv/holtmail-new [--skip-reindex]
holtmail openapi                                 # the API description as JSON
```

While `serve` runs, `user`, `account list`, `account edit`, `export` (except `--query`), `backup` and `restore` (into another directory) work next to it, also through `docker exec`. `archives`, `import`, `sync`, `reindex` and the `account add-*` commands open the search index, which `serve` holds; they stop with "the search index is in use by another process" then. Use the web app for those, or stop `serve` first.

`import` reads mail and, from the same path, address books and calendars: a `.vcf` or `.ics` file, every `.vcf` and `.ics` in a directory, or those inside a zip, such as Google Takeout's `Takeout/Contacts/<group>/<group>.vcf` and `Takeout/Calendar/<calendar>.ics`. Each file becomes a collection of the import account (named after the path, as for mail; `--account` overrides), known by the file's path in what was imported (for an upload, the uploaded name) and named after the calendar's `X-WR-CALNAME`, else the file name. Each contact, event or task becomes an item known by its `UID` (a card without one by its lowercased name, anything else by the hash of its bytes; a repeated id gets `#2`, `#3`, …), split and read by [`holtmail-pim`](../../../crates/pim/docs/README.md) and stored by [`holtmail-ingest`](../../../crates/ingest/docs/README.md): bytes unchanged, a new version only when they differ, pictures of contacts as blobs of their own. Importing the same file again adds nothing; a changed file adds versions for the items that changed, and items of the collection that the file no longer holds are marked removed (never deleted) unless the file holds no items at all. The summary line counts new, changed, unchanged, gone and unreadable contacts and events next to the messages. Files are read whole, at most 256 MiB each.

`export` writes the original bytes of the messages of an archive (by name or id), one folder (by path, `account name/path` when several accounts have it, or id), one label, or a search, newest first, with [`holtmail-export`](../../../crates/export/docs/README.md). `--format` defaults to `zip` for an output ending in `.zip` and `mbox` otherwise; `--output -` writes to standard output. It writes `<output>.part` and renames it when done, and never overwrites a file. It needs only the database and the blobs, so it works while `serve` runs, except `--query`, which needs the search index that `serve` holds: export searches from the web app then. Messages whose bytes are missing or damaged are left out and counted. The web app and `GET /api/archives/{id}/export` stream the same files.

`export --contacts` and `export --calendar` write an archive's contacts or calendar items instead (see [Contacts and calendars](../../../crates/export/docs/README.md#contacts-and-calendars)): the latest version of each as one `.vcf` or `.ics`, or every version as stored in a zip with `manifest.json`. `--format` defaults to `zip` for an output ending in `.zip`, else `vcf` or `ics`. `--collection` takes an address book's or calendar's name or id; `--person` a person's id (contacts only); `--from` and `--to` days in UTC, both or neither (calendars only), keep events with an instance in that span; `--include-removed` puts items removed from their source into the `.vcf` or `.ics` too. Graph and JMAP items are written as vCard or iCalendar text in the `.vcf` and `.ics`, and as their stored JSON in the zip. Works while `serve` runs; the API streams the same files.

`account add-imap` reads the password from `HOLTMAIL_IMAP_PASSWORD` or the first line of stdin, seals it with the instance key (`<data_dir>/secret.key`, bound to the new account's id) and stores only the sealed bytes. `--security tls|starttls|plain`, `--port`, `--name` and repeated `--exclude <folder path>` are optional. `sync` prints one line per folder: new, known, removed, failed and any error.

`account edit` changes one account (its id from `account list`) through the same path as the web app: a new server, port, security, user name, excluded folders (`--exclude` replaces the list, `--clear-exclude` empties it) or password (`--password-stdin`, the first line of standard input) is checked by signing in to the mail server first, and nothing is stored when that fails; an OAuth account gets a fresh access token for the check. A new `--security` without `--port` uses that security's default port. A stored password or token is never sent to another server: another host or port (by the effective port, so a new security that changes the default port counts), a move from TLS or STARTTLS to `plain`, or a JMAP `--url` on another origin (scheme, host or port) needs the secret again through `--password-stdin`, and is refused with "changing the server needs the password or token again" otherwise (the web app and API answer `400` "Enter the password again when changing the server."). A change of security on the same host and port that keeps encryption keeps the stored password, as does a JMAP address on the same origin. An OAuth (Gmail or Microsoft) IMAP account keeps the server, port and security its provider set; any change to them is refused, since the account's access token would go to that server. `--interval` sets the account's own minutes between syncs, `0` only on request, `default` follows the server's; `--min-age-days` archives only mail at least that many days old (at most 3650; `0` archives all mail); `--pause` and `--resume` stop and restart syncing. It opens only the database (and `secret.key` and the OAuth clients for a change that signs in), so it works while `serve` runs: `serve` picks the change up at its next 30-second check. A sync running in `serve` at that moment finishes with the settings it started with.

`account add-jmap` adds a JMAP account (Fastmail, Stalwart, Cyrus and others; see [holtmail-source-jmap](../../../crates/source-jmap/docs/README.md)). `--url` is the session address or just the server's address, where `/.well-known/jmap` is asked; https only. With `--token-stdin` it signs in with an API token (bearer) read from the first line of stdin and needs no `--username`; otherwise it signs in with HTTP Basic as `--username` and reads the password from `HOLTMAIL_JMAP_PASSWORD` or stdin. It signs in and lists the mailboxes before storing anything, and seals the password or token like IMAP passwords. Repeated `--exclude <mailbox path>` and `--name` are optional. For a JMAP account `account edit` takes `--url`, `--auth password|token`, `--username` and the excluded mailboxes instead of a host, port and security; changing `--auth` needs the new secret through `--password-stdin`, which also takes a new token for a token account. Accounts are stored with kind `jmap` and settings `{ "url", "username", "auth" }`; the scheduler, Sync now, tasks, minimum age and folder rules work as for IMAP.

`account add-oauth` prints the provider's sign-in page, then reads the address the browser lands on from stdin (see [OAuth](#gmail-and-microsoft-sign-in-oauth)); the provider is guessed from well-known domains unless `--provider` is given. With an own client whose redirect is Holtmail's callback, sign in from the web app instead.

`account add-graph` adds a Microsoft 365, Outlook.com or Hotmail mailbox read through Microsoft Graph ([holtmail-source-graph](../../../crates/source-graph/docs/README.md)) instead of IMAP, for tenants where IMAP is turned off. It runs the same sign-in as `add-oauth` with Microsoft and the Graph scopes, lists the mailbox's folders with the new token and reads the signed-in address from `/me` before storing anything; the account is stored with kind `graph`, the signed-in address and settings `{ "oauth": { "provider": "microsoft" } }`. Signing in again keeps the kind and refuses a different mailbox ("You signed in as …, but this account archives …"). The scheduler, Sync now, tasks, needs-sign-in, minimum age and `folders.exclude` work as for IMAP; `account edit` changes its name, interval, minimum age and pause, and refuses server settings and passwords.

## Contacts and calendars

An IMAP, JMAP or Microsoft Graph account can also archive its address books and calendars, synced by [holtmail-pim-sync](../../../crates/pim-sync/docs/README.md): over CardDAV and CalDAV ([holtmail-collection-dav](../../../crates/collection-dav/docs/README.md)), through Microsoft Graph for Graph accounts ([holtmail-collection-graph](../../../crates/collection-graph/docs/README.md)), or through the account's own JMAP session for JMAP accounts whose server offers contacts ([holtmail-collection-jmap](../../../crates/collection-jmap/docs/README.md)). It is off by default; turn it on in the web app (the account's menu → Archive contacts and calendars) or with `account edit <id> --contacts-calendars on`. The account's settings then hold `dav: { enabled, protocol, contacts_url, calendars_url }`, where `protocol` is `dav` (left out), `graph` or `jmap`.

- **Addresses.** `--dav-url` sets one address for both (Nextcloud: `https://host/remote.php/dav`; Radicale and Baïkal: the server), `--carddav-url` and `--caldav-url` set them apart. Without any, they are detected from the account's address: Fastmail (`https://carddav.fastmail.com`, `https://caldav.fastmail.com`), iCloud (`https://contacts.icloud.com`, `https://caldav.icloud.com`) and Yandex (`https://carddav.yandex.ru`, `https://caldav.yandex.ru`) by domain, else the mail domain's well-known addresses (`https://<domain>/.well-known/carddav` and `caldav`). Turning it on signs in and lists the collections first; nothing is stored when that fails, and a failed detection asks for the address.
- **Sign-in.** Password accounts use the mail user name and the stored password (an app password where the provider needs one); a JMAP account with a token sends it as a bearer token. Google accounts that sign in with OAuth use Google's addresses (`https://www.googleapis.com/.well-known/carddav`, `https://apidata.googleusercontent.com/caldav/v2/`) with a bearer token; turning it on asks for the extra CardDAV and CalDAV scopes, so the account has to **Sign in again** once (the web app opens it right away). Until then mail keeps syncing and the account shows "sign in again to allow access"; `settings.oauth.access` records what the last sign-in granted (`imap` or `imap_dav`). **Microsoft Graph accounts** read `/me/contacts`, the contact folders and `/me/calendars` with the account's Microsoft sign-in; turning it on asks for `Contacts.Read` and `Calendars.Read` on top of the mail scopes (`Access::GraphPim`, recorded as `graph_pim`), so the account signs in again once, as for Google, and mail keeps syncing meanwhile. With your own client, add the delegated Graph permissions `Contacts.Read` and `Calendars.Read`. **Microsoft accounts over IMAP** (OAuth) are refused with a note to add the mailbox as a Microsoft Graph account: their sign-in is for Outlook's IMAP resource, and reading Graph would need a second Microsoft sign-in with its own refresh token for the same account.
- **JMAP accounts** without an address given ask the account's JMAP session first: when it offers `urn:ietf:params:jmap:contacts`, contacts (and calendars, when it offers `urn:ietf:params:jmap:calendars`) are read through JMAP with the account's own password or token, and `protocol` is `jmap`; otherwise the CardDAV and CalDAV addresses are detected as above. Giving an address always uses CardDAV and CalDAV. Moving an account from one protocol to the other marks the old collections removed and archives the new ones (their ids differ); nothing is lost.
- **When.** Right after every mail sync of the account (scheduled, Sync now, or `holtmail sync`), whatever the mail sync's outcome, as a task of its own ("Contacts and calendars", kind `contacts_calendars`) with one log line per address book or calendar: new, changed, unchanged, removed and failed items and how changes were read. Turning it on also starts one at once. The account's status carries the latest such task (`status.contacts_calendars`). Turning it off keeps everything archived; the collections stay browsable.

Everything the commands do can also be done in the web app by the owner or an admin: archives, IMAP accounts, uploads of mailbox files, syncing now, people and their access. The CLI stays for scripts and headless installs.

`serve` checks every 30 seconds which enabled accounts are due and syncs them one after another, and removes expired sessions every hour. An account is due every `sync_interval_minutes` after its last run ended, at once when it never ran or a restart interrupted its sync (also an account that syncs only on request: the interrupted sync runs once at start, then it waits for requests again); each account can have its own interval (`sync_interval_minutes` in its settings, set from the web app), `0` meaning only on request. The server-wide interval comes from the config when set there, else from the web app (stored in the settings), else 15 minutes. Changing an account's server, user name or excluded folders from the web app signs in with the new settings first; the account keeps its archive, and messages it already has are recognized by their bytes, so nothing is stored twice. A change to another server (host, port, or a drop to plain text) or another user name forgets the UIDs and cursors of the account's folders, so the next sync looks at every message of the new mailbox: its `UIDVALIDITY` and UIDs can equal the old ones by chance (Dovecot and GreenMail both derive `UIDVALIDITY` from the clock) and would otherwise hide its mail. Syncs and imports started from the web and from the schedule share one runner that keeps at most one run per account. How the last run of each account ended (when, new messages, the error) is stored, so the web app shows it after a restart and the schedule counts from it. Uploaded files wait in `<data_dir>/uploads` and are removed once imported; files of an import a restart interrupted are removed at start.

An account can archive only mail older than a number of days (`min_age_days` in its settings, set from the web app or `account edit --min-age-days`): a folder's sync stops at its first message that is younger, by the server's received time or else its `Date` header, and that message and the later ones are archived by a sync once they are old enough. See [holtmail-sync](../../../crates/sync/docs/README.md).

Attachment text is read by [`holtmail-extract-basic`](../../../crates/extract-basic/docs/README.md) (plain text, CSV, HTML, PDF, DOCX, XLSX, PPTX, ODT, ODS, ODP) while mail is archived, once per file, and kept as a derived blob so reindexing and flag changes reuse it. `reindex` also rebuilds the listed fields of every contact and event from its latest raw version, and with them what is derived from those fields: each event's group of copies and stored instances (see [holtmail-calendar](../../../crates/calendar/docs/README.md)) and the search documents of contacts and events. `reindex` reuses those texts; `--refresh-attachments` drops them (and their blobs) and reads every attachment again, e.g. after an upgrade that reads more types. At start, `serve` indexes the attachment text of mail archived before this existed: in the background, only messages with attachments, once (the setting `attachment_text` records it is done); search finds the text as it goes. Image thumbnails for the reader are made on first view and kept as derived blobs too.

Invitations, updates, replies and cancellations in mail (`text/calendar` and `application/ics` parts) become events in each archive's "From mail" calendar as mail is archived (see [holtmail-ingest](../../../crates/ingest/docs/README.md#invitations-in-mail)). At start, `serve` reads them from mail archived before this existed: in the background, after the index pass, only messages with a calendar part among their attachment rows, once (the setting `invitations_from_mail` records it is done). `reindex` reads them again too and prints how many messages had calendar parts and how many versions were new. The log carries counts and message ids only.

People (contact cards grouped by a shared email or phone, plus frequent correspondents from mail; see [holtmail-people](../../../crates/people/docs/README.md)) are derived per archive and rebuilt whole. The runner rebuilds an archive's people in the background after an import, after a mail sync that archived new messages and after a contacts and calendars sync that added, changed or removed something; one rebuild per archive runs at a time, and requests while it runs make it run once more at the end. Splits, joins and undos made in the web app go through the same queue and the API answers once a rebuild that started after the change has finished. A rebuild that panics is logged (archive id only) and counts as failed; later requests still run, and a contacts sync that panics still frees its account for the next one. `holtmail import` and `holtmail sync` rebuild the archives they touched before they exit and print the counts. At start, `serve` builds the people of every archive once, after the index and invitation passes, until the setting `people` is set. `reindex` rebuilds every archive's people last and prints how many have cards and how many come from mail alone. Ids stay the same across rebuilds. The log carries archive ids, counts and milliseconds only.

Remote images go through [`holtmail-fetch-http`](../../../crates/fetch-http/docs/README.md), which only contacts public addresses, optionally through an outbound proxy (`image_proxy` in the config, or set by an owner or admin in the web app, stored sealed with `secret.key`; the config wins). The proxy can change while the server runs. For archives with "Save remote images" on, [`holtmail-remote`](../../../crates/remote/docs/README.md) saves the images of new messages after every sync or import that brought any and once at start; mail archived before saving was turned on, and failed images, only when asked from the web app. One remote image task per archive at a time. The web reader's image proxy uses the same fetcher.

Syncs, imports, contacts and calendars syncs and remote image work run as tasks with progress and a log (counts, folder names and reasons, never message content), listed in the web app under Settings → Tasks. Remote image tasks can be stopped between messages. Tasks are stored in the database (written behind the work at most once a second, and at shutdown on `SIGINT` or `SIGTERM`), so the list looks the same after a restart; the last 200 finished tasks are kept, and logs keep their first 50 and last 450 lines. Tasks a previous process left running are marked _interrupted_ at start: an interrupted sync is due again at once (also for an account that syncs only on request), and interrupted remote image work (older mail, or failed images) starts again and skips the messages it already looked at; images of new mail are picked up at start anyway.

## Back up and move an instance

An instance is its database, its blobs and `secret.key`; the search index, uploads and thumbnails can be made again.

```bash
holtmail backup --output /backups/holtmail-2026-11-05.tar
holtmail restore --input /backups/holtmail-2026-11-05.tar --data-dir /srv/holtmail
```

`backup` writes one tar file, front to back, so it can go to a pipe (`--output -`) and needs about one message of memory:

| Entry           | What                                                                                                                                                    |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `manifest.json` | First entry: format version, Holtmail version, `created_at`, counts of archives, messages and blobs, whether blobs and the key are in                   |
| `holtmail.db`   | A consistent snapshot of the database (SQLite `VACUUM INTO`), taken while `serve` keeps running; it carries the splits and joins of people made by hand |
| `secret.key`    | Only with `--include-secret-key`                                                                                                                        |
| `blobs/<hash>`  | Every blob the snapshot references (messages, attachments, saved images, derived files), read through the blob store, so S3 blobs are included too      |

It is safe while `serve` runs: mail archived after the snapshot is simply not in the backup. A blob that is missing or damaged in the blob store is left out, named, and makes the command fail after the file is written. It writes `<output>.part` and renames it when done, and never overwrites a file.

**`secret.key`.** Mailbox passwords, OAuth refresh tokens and the image proxy password are sealed with it. Without it a restored instance has all the mail but must sign in to every mailbox again. By default the key is left out and `backup` says so: keep a copy of `<data_dir>/secret.key` somewhere safe, apart from the backups. `--include-secret-key` puts it in, and then the backup file opens those credentials for anyone who has it; store it like the key.

**Blobs on S3.** By default the blobs are copied out of the bucket into the backup like any other. When the bucket has its own backup or versioning, `--skip-blobs` leaves them out and says so; a restore from such a backup checks that the blob store the config names holds every blob the database references and warns about the ones it does not.

`restore` reads a backup into a data directory that does not exist or is empty, and refuses any other. It checks that the first entry is a manifest it understands, writes `holtmail.db` (and `secret.key` when present, mode `0600`), and puts every blob into the blob store the config names (`<data-dir>/blobs` by default, or `fs_at` / S3 from the config) after checking that its bytes hash to its name; one damaged blob stops the restore. It then checks that every blob the database references is in the blob store and rebuilds the search index (`--skip-reindex` leaves that to `serve`, which rebuilds it in the background at start). A restore that stops part way says so; remove the directory before trying again.

To move an instance to another machine: `holtmail backup` on the old one (or stop it and copy the data directory), copy the file and `secret.key`, `holtmail restore` on the new one with its config, copy `secret.key` into the new data directory if it was not in the backup, then start `serve`.

## Signing in

While there are no users, `serve` logs a one-time setup link at start:

```
No users yet. Open http://127.0.0.1:8080/setup?token=... to create the owner; the link works once.
```

Whoever opens it creates the owner account and is signed in. The link stops working once any user exists and a new one is printed on every start until then, so keep the log private. The link uses `public_url` when set, else the listen address (`localhost` for `0.0.0.0`).

On headless installs, `user add` creates users directly; the first user becomes `owner` unless `--role` says otherwise, later ones `member`. `user add` and `user password` read the password from `HOLTMAIL_PASSWORD` or the first line of stdin; passwords need at least 10 characters. Passwords are stored as Argon2id hashes and never logged. The `user` commands open only the metadata store, so they work while `serve` is running; in Docker, run them with `docker exec <container> holtmail user ...`.

`sync` prints per folder how many messages were new, already known, removed from the server, changed (flags or labels changed on the server since archiving) and failed; the Tasks log shows the same. Gmail accounts sync `[Gmail]/All Mail` with labels instead of each label folder, and pick up read, flagged and label changes through `CONDSTORE`; see [holtmail-sync](../../../crates/sync/docs/README.md).

`reindex` rebuilds every message's search document. The server also does it by itself: when the search index on disk was written by an older version with other fields (Tantivy cannot add a field, so the new version starts with an empty index), any command that opens it records the setting `index_rebuild`, and `serve` rebuilds the index in the background at start and clears the setting when done. Until then search finds only mail indexed since; lists, folders and labels come from the database and are complete. An interrupted rebuild runs again at the next start. Migration 0014 (the calendar) sets `index_rebuild` too, because the index schema gained contacts and events and the calendar's derived rows have to be filled for items stored before: the first `serve` after the upgrade rebuilds the index, every item's calendar rows and its search document, and commits after every page of items.

`import` creates the archive and an import account (named after the file, or `--account`) when missing, maps folder names to roles (`Sent`, `Отправленные`, `[Gmail]/All Mail`, ...), commits the index every 500 messages and prints progress. Running it again on the same files adds nothing: messages are recognised by their bytes. Files that are not messages are counted as failed and skipped.

## Gmail and Microsoft sign-in (OAuth)

Microsoft turned off password sign-in for IMAP, and Gmail only takes app passwords, so Gmail, Google Workspace, Outlook.com, Hotmail and Microsoft 365 accounts sign in with OAuth and IMAP `AUTHENTICATE XOAUTH2`.

**The default client is Thunderbird's.** Its client ids, and Google's client secret, are public in `mailnews/base/src/OAuth2Providers.sys.mjs` in comm-central, and Holtmail uses them unless the config names another client. Such an installed-app client can only send the browser back to a loopback address (`http://127.0.0.1`), not to a self-hosted Holtmail, so:

1. The web app (or `account add-oauth`) opens the provider's page in a new tab, with PKCE, a random `state` and the address as `login_hint`.
2. After the person allows access, that tab lands on `http://127.0.0.1/?code=…&state=…`, a page that does not load.
3. They copy that page's address and paste it into Holtmail, which reads `code` and `state`, exchanges the code and signs in to IMAP with the token before storing anything.

Risks of the borrowed client, also shown in the web app:

- The consent screen says Thunderbird, not Holtmail.
- Google or Microsoft can block or rate-limit the borrowed client at any time; sign-in then fails with "the provider refused" until you set your own client.
- Whether using it fits the provider's terms is your call.

**Your own client.** Register an app with the provider and put it in the config. Its redirect is `<public_url>/api/oauth/callback`, so set `public_url` to the address people open. The browser then comes back to Holtmail on its own and there is no paste step.

- Google: in Google Cloud Console create a project, enable the Gmail API, configure the OAuth consent screen (add the scope `https://mail.google.com/`; while the app is in testing, add the people who will sign in as test users, and note that refresh tokens of apps in testing expire after 7 days), then create an OAuth client of type "Web application" with the redirect URI above.
- Microsoft: in Microsoft Entra ID → App registrations register an app for "Accounts in any organizational directory and personal Microsoft accounts", add a Web redirect URI as above, create a client secret, and add the delegated permissions `IMAP.AccessAsUser.All` (Office 365 Exchange Online / `https://outlook.office.com`) and `offline_access`.

```toml
[oauth.google]
client_id = "123-abc.apps.googleusercontent.com" # HOLTMAIL_OAUTH_GOOGLE_CLIENT_ID
client_secret = "..." # HOLTMAIL_OAUTH_GOOGLE_CLIENT_SECRET
# redirect_url = "https://mail.example.com/api/oauth/callback"  # HOLTMAIL_OAUTH_GOOGLE_REDIRECT_URL

[oauth.microsoft]
client_id = "00000000-0000-0000-0000-000000000000" # HOLTMAIL_OAUTH_MICROSOFT_CLIENT_ID
client_secret = "..." # HOLTMAIL_OAUTH_MICROSOFT_CLIENT_SECRET
```

`authorize_url` and `token_url` (`HOLTMAIL_OAUTH_<PROVIDER>_AUTHORIZE_URL`, `_TOKEN_URL`) replace the provider's endpoints, for a single-tenant Microsoft authority (`https://login.microsoftonline.com/<tenant>/oauth2/v2.0/authorize` and `/token`) or a national cloud; `graph_url` (`HOLTMAIL_OAUTH_MICROSOFT_GRAPH_URL`) is the Graph address new Graph accounts are given (stored as their `api_url`), such as `https://graph.microsoft.us/v1.0`. `imap_host` and `imap_port` (`HOLTMAIL_OAUTH_<PROVIDER>_IMAP_HOST`, `_IMAP_PORT`) are the IMAP server new OAuth IMAP accounts of that provider connect to over TLS, instead of `imap.gmail.com` or `outlook.office365.com` on 993, for a national cloud such as `outlook.office365.us`; accounts keep the server they were created with, and their server, port and security cannot be changed. The integration tests point them at Dovecot, which checks the tokens at a mock provider. The three URLs must use https (plain http only to a loopback address, which is how the end-to-end checks point them at a local fake). `redirect_url` defaults to `<public_url>/api/oauth/callback` for an own client and `http://127.0.0.1` for Thunderbird's. Setting it without `client_id` changes only the redirect of Thunderbird's client, e.g. to `http://127.0.0.1:8080/api/oauth/callback` when Holtmail itself runs on that loopback address (untested against the providers). A redirect ending in `/api/oauth/callback` turns the paste step off. `client_secret` without `client_id` is an error. Prefer the environment variables for the secret.

**Microsoft Graph.** A Graph account signs in with the same Microsoft client but asks for `https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/User.Read offline_access` instead of the IMAP scope, and its refreshes ask for the same scopes: the account's kind decides, since an IMAP token is refused by Graph. With your own client, add the delegated Microsoft Graph permissions `Mail.Read` and `User.Read` (and `offline_access`) next to, or instead of, `IMAP.AccessAsUser.All`. Whether Thunderbird's client is granted `Mail.Read` has not been checked against Microsoft (Thunderbird itself asks for `Mail.ReadWrite` and more behind a preference).

**Google contacts and calendars.** An IMAP account with contacts and calendars on signs in with `https://www.googleapis.com/auth/carddav` and `https://www.googleapis.com/auth/calendar.readonly` next to the mail scope (`https://www.googleapis.com/auth/calendar` with Thunderbird's client, which is what Thunderbird asks for). With your own client, enable the CalDAV API and the CardDAV API in the project and add those scopes to the consent screen.

**After sign-in.** The refresh token is sealed with `secret.key` (bound to the account id) like passwords; the account's settings record `oauth: { provider, needs_sign_in, access }`. Access tokens live only in memory: before each sync the server uses the cached one while it has more than two minutes left, else refreshes it, and stores a rotated refresh token when the provider sends one. When the provider answers `invalid_grant` (access revoked, password changed, token expired) the account is marked as needing sign-in: the scheduler skips it, "Sync now" is refused, and the web app offers "Sign in again", which replaces the stored token and clears the mark. A password account can move to OAuth the same way. Waiting sign-ins (`state`, PKCE verifier, provider, what they are for) are kept in memory for 10 minutes and used once; a restart drops them. Tokens, codes and verifiers are never logged.

## Config

Optional TOML file (`-c holtmail.toml` or `HOLTMAIL_CONFIG`). Every key has a default:

```toml
listen = "127.0.0.1:8080" # HOLTMAIL_LISTEN
data_dir = "data" # HOLTMAIL_DATA_DIR: holtmail.db, index/, blobs/
web_dir = "apps/web/dist" # HOLTMAIL_WEB_DIR: serve the built web app; unknown paths get index.html
# sync_interval_minutes = 15 # HOLTMAIL_SYNC_INTERVAL: accounts without their own interval; 0 syncs them only on request; when set, the web app cannot change it
# image_proxy = "socks5h://127.0.0.1:9050" # HOLTMAIL_IMAGE_PROXY: outbound proxy for remote images
# tls_ca_file = "/etc/holtmail/ca.pem" # HOLTMAIL_TLS_CA_FILE: extra CA certificates for outbound TLS

# [oauth.google] and [oauth.microsoft]: client_id, client_secret, redirect_url, authorize_url, token_url,
# imap_host, imap_port, and for Microsoft graph_url; see "Gmail and Microsoft sign-in"

[blobs]
kind = "fs" # blobs in <data_dir>/blobs
# kind = "fs_at", path = "/mnt/mail"
# kind = "s3", bucket = "holtmail", region = "eu-central-1",
#   endpoint = "https://s3.example.com", prefix = "archive", path_style = true
```

`tls_ca_file` names a PEM file of CA certificates that every outbound TLS connection trusts besides the built-in roots: IMAP (the Mozilla roots), and JMAP, CardDAV/CalDAV, Microsoft Graph, OAuth token endpoints, S3 and remote images (the system's roots). It is for self-hosted servers with a private CA. Verification stays on: a server still needs a certificate for the name it is reached by, signed by a trusted CA; there is no switch to accept any certificate. Every command reads it at start and refuses to run when the file is missing or holds no certificate.

S3 credentials come only from `HOLTMAIL_S3_ACCESS_KEY_ID` and `HOLTMAIL_S3_SECRET_ACCESS_KEY`, never from the file. Unknown keys are an error, so a typo does not silently fall back to a default.

## Data directory

| Path          | What                                                                                                                                   | Back up                            |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- |
| `holtmail.db` | Metadata (SQLite), with the recent tasks and each account's last run                                                                   | Yes, with `holtmail backup`        |
| `blobs/`      | Message, attachment and saved remote image bytes, unless blobs are on S3; also thumbnails and attachment text, which can be made again | Yes, with `holtmail backup`        |
| `secret.key`  | The key sealing account passwords and OAuth refresh tokens                                                                             | Yes, separately                    |
| `index/`      | Search index                                                                                                                           | No: `holtmail reindex` rebuilds it |
| `uploads/`    | Uploaded files waiting to be imported                                                                                                  | No                                 |
| `.backup-*/`  | The database snapshot while `holtmail backup` runs; removed when it ends                                                               | No                                 |

## Logs

`RUST_LOG` sets the level (`info` by default). Logs carry counts, ids and errors, never message content, addresses, passwords, session tokens or OAuth codes and tokens. The one exception is the setup link above, which is how the owner is bootstrapped.

## Behind a reverse proxy

Terminate TLS in the proxy, pass `Host` through unchanged, set `X-Forwarded-Proto`, and set `public_url` to the https address. Writes are refused when the browser's `Origin` does not match `Host`. Sign-in attempts are rate-limited per client address and email; behind a proxy every client shares the proxy's address, so the limit is effectively per email.
