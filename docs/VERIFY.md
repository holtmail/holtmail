# Still to verify

Everything that tests, builds and runs of the real binary against local fakes could not prove. Each item needs a real service, real data, a browser or a person. Tick an item when it has been checked, and note what was found in the Log at the end. Add new items here whenever a change leaves something unproven.

## Sign-in with Google and Microsoft (OAuth)

- [ ] Google accepts Thunderbird's client with the redirect `http://127.0.0.1` (no port) when the request comes from Holtmail; the code exchange and refresh work
- [ ] Microsoft accepts Thunderbird's client with the redirect `http://127.0.0.1`. Check this against the live registration: Thunderbird bug 2036505 shows Microsoft moving between `http://localhost` and `https://localhost`
- [ ] Consent screens: what the user sees, and whether Google limits a borrowed client (unverified app warning, user cap)
- [ ] `AUTHENTICATE XOAUTH2` against real Gmail and Outlook IMAP, including the error continuation for an expired token (against Dovecot 2.4.5's `oauth2` passdb: signs in, syncs, and a token for another user is refused with nothing stored: tests/integration/tests/oauth.rs)
- [ ] A revoked token (access removed in the Google or Microsoft account) marks the account "sign in again", and Sign in again restores it. Proved against mock-oauth2-server 6.1.2 (refresh token revoked at its `/revoke`; the scheduler does not try the account again, a requested sync is refused, Sign in again through the callback restores it) by tests/integration/tests/oauth.rs; still to see with Google's and Microsoft's own revocation
- [ ] A refresh token rotated by the provider is stored and keeps working after a restart. Proved against mock-oauth2-server 6.1.2, which rotates on every refresh and refuses the old token, through three syncs and a restart, by tests/integration/tests/oauth.rs; still to see with Microsoft, which rotates for real
- [ ] An own client in `[oauth.google]` and `[oauth.microsoft]` with `redirect_url` pointing at `/api/oauth/callback`: no paste step, the browser comes back to the page where sign-in started. The flow is proved against mock-oauth2-server 6.1.2 with `authorize_url`, `token_url` and the new `imap_host`/`imap_port` pointing at it and at Dovecot (XOAUTH2 checked by Dovecot's introspection): the authorize address, the callback's `303` to `return_to` with `oauth=done&account=`, a replayed callback refused, and the paste flow with an own client whose `redirect_url` is elsewhere, by tests/integration/tests/oauth.rs; the real providers' consent pages and registrations stay unproved
- [ ] Device code flow for Microsoft: whether Thunderbird's registration allows it (not built)
- [ ] The popup for the sign-in page is not blocked by browsers; if it is, the "Open the sign-in page" link works

## Microsoft Graph

- [ ] Thunderbird's Microsoft client is granted `Mail.Read` and `User.Read` through incremental consent; if not, document the own-client route
- [ ] A Graph sync end to end on Outlook.com and on a Microsoft 365 tenant where IMAP is off
- [ ] Delta paging sizes, delta link expiry, and the full relisting when a link has expired
- [ ] Hidden folders and well-known folder names on real tenants
- [ ] `$value` of large messages (tens of MB)
- [ ] Throttling: `429` with `Retry-After` on a big first sync

## JMAP

- [ ] Fastmail with an API token: session, mailboxes, a first sync, an incremental sync
- [ ] The settings path in the Fastmail token hint ("Privacy & Security → API tokens")
- [x] A real JMAP server with a password and with an API token: well-known discovery, mailboxes and roles, a first and an incremental sync, an email in two mailboxes stored once, moves, deletions kept in the archive, keywords as flags (`$seen`, `$flagged`, `$forwarded`, read once a day), refused passwords, users and tokens: proved against Stalwart 0.16.27 by tests/integration/tests/jmap.rs. Before 2026-11-06 a `/.well-known/jmap` that redirects to another host (as Fastmail's does) failed with "no mail account": the credentials were dropped on the redirect and the anonymous session had no accounts; fixed by following the well-known address without credentials and asking the session where it led
- [ ] Cyrus: session shape, `maxObjectsInGet`, `maxCallsInRequest`, `Email/query` limits and `total`, download content types (Stalwart 0.16.27: `maxObjectsInGet` 500, `maxCallsInRequest` 16, absolute `apiUrl` and `downloadUrl` on `STALWART_PUBLIC_URL`, raw bytes downloaded unchanged)
- [ ] A large mailbox: every sync lists all ids (`Email/changes` is not used); measure the time
- [ ] Mailbox names containing `/`

## IMAP, Gmail labels and flags

- [x] A real plain IMAP host (password): first sync, incremental sync, changing the connection from the web app and from `holtmail account edit`: proved against Dovecot 2.4.5 and GreenMail 2.1.14 by tests/integration/tests/imap.rs (the web app's change goes through the same `PATCH /api/accounts/{id}` the test uses; the dialog itself stays under "by eye"). Before 2026-11-06 moving an account to another server whose `UIDVALIDITY` happened to equal the old one (both servers derive it from the clock) skipped the new mailbox's mail; fixed by forgetting the account's folder positions on a change of server or user name
- [ ] The same on a public provider's IMAP (password or app password)
- [ ] Gmail: capabilities, `X-GM-LABELS` quoting of real system labels and non-ASCII labels, labels sent as literals (would fail the fetch today)
- [ ] Gmail: an account that synced label folders before Phase 8 moves to All Mail without storing anything twice
- [ ] `ENABLE CONDSTORE` and `CHANGEDSINCE` on Gmail All Mail: read and flagged changes reach the archive
- [x] `ENABLE CONDSTORE` and `CHANGEDSINCE` on another server with CONDSTORE: proved on Dovecot by tests/integration/tests/imap.rs
- [ ] The daily flag scan on a large mailbox without CONDSTORE: time and load (the path itself, nothing read before a day and every flag read after, is proved on GreenMail by tests/integration/tests/imap.rs)
- [x] "Archive only mail older than N days" on a real account: young mail waits and arrives later: proved on Dovecot with `APPEND` internal dates by tests/integration/tests/imap.rs
- [ ] IMAP timeouts through the binary: the two-minute limit is proved at library level against a stalled connection (tests/integration/tests/imap_faults.rs, with a 5 s limit); a server that stalls for two minutes under `serve` was not waited for
- [x] A sync interrupted by a restart resumes at start, also for an account that syncs only on request (`sync_interval_minutes = 0`; decided 2026-11-06: it resumes once, then waits for requests again; before, it was not resumed): proved by tests/integration/tests/imap_scheduler.rs

## Remote images

- [ ] Fetching images from the real internet: directly, through an HTTP proxy, through `socks5h` (before 2026-11-06 a configured proxy was silently dropped and images were fetched directly; fixed in `holtmail-fetch-http`, unit-tested through a SOCKS5 proxy in the process). Through real proxies to an image server in the stack: `socks5h`, `socks5` with a password (set through the API, stored sealed) and squid as an HTTP proxy, each request seen coming from the proxy, plus URL and byte deduplication, wrong types, oversized and slow images refused, private and loopback addresses refused when fetching directly: tests/integration/tests/images.rs. Still to see on the real internet: real image hosts, CDNs, redirects to other hosts, HTTPS images
- [ ] Saving images of older mail on a large archive: speed, failures, retry of failed images (failures and their retry once the server answers, through a proxy, proved on 16 and 5 images by tests/integration/tests/images.rs; since 2026-11-06 up to four messages are looked at together with the six downloads shared, so a slow sender no longer holds up the rest: a 20 KiB image at 2 KiB/s in the first message while the six messages after it were saved within a second, tests/integration/tests/images.rs and the unit test in holtmail-remote; a slow image still holds up the end of its batch of 50 messages)
- [x] Remote image work resumes after a real interruption (server stopped mid-task): the older-mail task killed with `SIGKILL` after 4 of 16 messages is shown as interrupted and resumed at start, every image saved and at most the one in flight fetched again: tests/integration/tests/images.rs

## Attachments

- [ ] Text extraction from real PDFs and office files made by other programs (Word, LibreOffice, Google Docs exports, scanners)
- [ ] Extraction speed and memory on a large archive; the start-up pass over old mail
- [ ] Thumbnails of real photos (EXIF orientation, large images, animated GIF)

## Export, backup and restore

- [x] Export and backup of a large archive: memory stays flat, time. 20 000 invented messages (43.6 MiB mbox, 800 with attachments), debug build: mbox and zip exports through the API and the CLI, `backup` while `serve` runs and `restore` (every blob's hash checked, search working afterwards) each peaked under 150 MiB, `unzip -t` and Python's `mailbox` read the files, and the exported mbox imported again added no blob: tests/integration/tests/archive_scale.rs (numbers in TESTPLAN.md's Log). Still to see: hundreds of thousands of messages and a release build on a small server
- [ ] Zip exports over 4 GiB (zip64 in stream mode) open in common unzip tools
- [x] Backup and restore with blobs on S3: against MinIO (Pigsty build `RELEASE.2026-08-04T00-00-00Z`) over TLS with a private CA (`tls_ca_file`): sync, import and reading with blobs in the bucket, `backup` with blobs and with `--skip-blobs`, restore into an empty directory with filesystem blobs (search and the stored password work), an object removed from the bucket named by the backup and refused by the reader with `404`, an unknown CA and a wrong secret key failing the sync with nothing stored: tests/integration/tests/s3.rs. Still to see: AWS S3 itself, and restoring into an S3 bucket
- [ ] Restore of a backup made by an older Holtmail version
- [ ] Exported mbox files open in Thunderbird and Apple Mail
- [ ] Contacts `.vcf` and calendar `.ics` exports open in Apple Contacts and Calendar, Google Contacts and Calendar, Outlook and Thunderbird; the `.ics` as one `VCALENDAR` mixing CRLF and LF lines (native components keep their bytes), zones written before first use and shared by `TZID`, events from mail written from the merged view (cancelled ones as `STATUS:CANCELLED`), and vCard 2.1/3.0/4.0 mixed in one `.vcf`. Radicale 3.8 takes both files as whole collections and serves them back, and Holtmail reads them again from there and from an upload with the same contacts and instances (a recurring series with a moved and an excluded instance, an all-day event): tests/integration/tests/dav_export.rs
- [ ] Graph and JMAP items written as text: Graph events in Windows zones without `VTIMEZONE` (written as IANA names when known), JSCalendar overrides with inherited fields, Graph contacts' addresses and birthdays, in the programs above
- [ ] A contacts or calendar zip of a large archive (every version of years of syncs): time, memory and the manifest's size

## Contacts and calendars

- [ ] Real exports read well: Google Contacts and Calendar Takeout, Apple Contacts and Calendar, Outlook, Thunderbird, Nextcloud and Fastmail `.vcf`/`.ics` files (names, phones, addresses, photos, time zones, all-day events, recurring events with moved instances, tasks); count unreadable items
- [ ] vCard 2.1 from old phones and Outlook with other charsets than UTF-8 (Windows-1251, ISO-8859-1) and quoted-printable soft line breaks
- [ ] Microsoft and custom `VTIMEZONE` names (`W. Europe Standard Time`, `X-LIC-LOCATION`) give the right UTC times; the same event exported by two programs gives the same `starts_at`. Proved through CalDAV on Radicale, Baïkal, Stalwart and Nextcloud by tests/integration/tests/dav_calendar.rs: `W. Europe Standard Time` with its `VTIMEZONE`, and a made-up `TZID` defined only by its `VTIMEZONE` across a change of offset; `X-LIC-LOCATION` and two programs' exports of one event remain
- [ ] Re-importing a newer export of the same address book or calendar under the same file name: only changed items get versions, items left out are marked removed, and the diffs read sensibly (no noise from fields programs rewrite on every export besides `DTSTAMP`)
- [ ] Contacts without `UID` (some phone exports): renaming one shows as a removed and a new contact instead of a version
- [ ] A large Takeout (thousands of contacts with photos, years of calendar): import time and memory (each `.vcf`/`.ics` file is read whole)
- [ ] Phone numbers written without a country code get no E.164 form; check how many real address books that leaves unmatched
- [ ] Uploading `.vcf` and `.ics` files and a Takeout zip from the web app (the file picker offers them; the task summary counts contacts and events). The upload API (`POST /api/archives/{id}/imports`) with a `.vcf` and an `.ics` is proved by tests/integration/tests/dav_export.rs; the web app's picker and a Takeout zip remain
- [ ] Invitations from real senders become events: Google Calendar, Outlook/Exchange (including `W. Europe Standard Time` zones, and messages where Exchange wraps the invitation in `winmail.dat`, which is not read), Apple Calendar, Thunderbird, Zoom and Teams links; updates with a raised `SEQUENCE`, replies (accepted, declined, tentative, delegated), cancellations of the event and of one instance of a series
- [ ] Senders that do not raise `SEQUENCE` on updates (some booking tools) or send the same `DTSTAMP`: versions still read in a sensible order (then by message date)
- [ ] Replies sent by Outlook and Google carry the attendee's `PARTSTAT` as the "your answer" and the merged attendee list expects; organizer copies of replies from several attendees
- [ ] "Your answer" on a synced IMAP/JMAP/Graph account with an address (imports have none, so it stays empty there); people who answer from an alias address
- [ ] The start-up pass on a large archive with years of invitations: time and memory, and that it runs once
- [ ] The invitation card and the item page by eye: times in the viewer's and the organizer's zones, cancelled state, history with diffs and links back to the messages

### People

- [ ] Grouping on real address books: how many cards join through a shared email or phone, whether any join wrongly (shared family or office numbers, a reused work address), and how many stay apart because phones lack a country code. Synthetic: cards with one email on Radicale, Baïkal and Stalwart join one person with the mail from that address in a Dovecot mailbox (tests/integration/tests/dav_people.rs)
- [ ] People from mail on a real archive: whether "wrote to them, or at least two messages" keeps newsletters and notifications out well enough beyond the `noreply`-style prefixes, and how many people it lists
- [ ] The owner's addresses on synced accounts: sent mail found through the Sent folder (IMAP, JMAP, Graph) and Gmail's Sent label, aliases, and imports of a Sent mbox whose folder gets no `sent` role
- [ ] Rebuild time and memory on a large archive (hundreds of thousands of messages, thousands of cards): the start-up pass, `reindex`, and a rebuild after each sync
- [ ] Migration 0013 on an existing database: the time `message_addresses` takes to fill from older messages, and addresses with non-ASCII capitals (filled with SQLite's ASCII `lower`, the same as new rows)
- [ ] The Contacts section by eye: the list with search and filters, keyboard navigation, a person with agreeing and disagreeing sources, cards and their history, correspondence and meetings, the links from the reader, the empty state, narrow windows
- [ ] Splits and joins by hand on real address books: whether "later cards that share keys with both sides stay with the rest" is what people expect (a new card with the shared family address joins the rest, a new card with only the moved card's phone joins it), and whether a split should also keep addresses apart (it moves cards only)
- [ ] The split and join dialogs, the overrides note with Undo, the _≠ Differences_ toggle and the export menus in a browser (light and dark, narrow windows, a member who sees notes without buttons)
- [ ] A split or join made while a background people rebuild runs (after an import or sync): the API's rebuild and the background one race, and the later one wins until the next rebuild

### Calendar

- [ ] Real calendars: Google, Apple, Outlook/Exchange, Nextcloud and Fastmail exports and syncs with years of recurring meetings; the agenda, week and month views show the same instances the source app shows (spot-check a few weeks around each spring and autumn change, and a few months a year or more ahead)
- [ ] Time zones in the wild: Exchange's Windows names (`W. Europe Standard Time`, `Eastern Standard Time`, `(UTC+01:00) Amsterdam, Berlin, …`), `X-LIC-LOCATION`, Lotus/Domino and other custom `VTIMEZONE` definitions with names no table knows (expanded from their own rules), vendor prefixes like `/mozilla.org/…/Europe/Berlin`, floating times; zones whose rules changed since an event was created (instances follow today's `chrono-tz` rules for the past and the future)
- [ ] Recurrence forms real clients write: Outlook's `BYDAY=…;BYSETPOS=…`, Google's `UNTIL` in UTC for timed and date `UNTIL` for all-day series, `EXDATE`s written as dates for timed series, `RDATE;VALUE=PERIOD`, Apple's split series, `RANGE=THISANDFUTURE` (rare; Google and Outlook split the series instead), changed instances whose `SEQUENCE` differs from the series'
- [ ] Copies across calendars: the same meeting in the organizer's and an attendee's calendar and in mail folds to one with the right leading copy; whether some servers rewrite `UID`s (Exchange `X-MICROSOFT-CDO-OWNERAPPTID`, Google event ids per calendar) so copies do not fold; the differences table on real copies (fields servers add or drop, attendees' answers). The same `UID` on Radicale, Baïkal and Stalwart folds into one event with three copies: tests/integration/tests/dav_people.rs
- [ ] Size: a shared calendar with thousands of endless series (the stored two-year window, the 5 000 instance cap per request, on-the-fly expansion for far spans), the time of the start-up rebuild after the upgrade (migration 0014 asks for a full reindex)
- [ ] The Calendar section by eye: agenda, week and month in light and dark, overlapping events side by side, all-day and multi-day events, events crossing midnight, the current-time line, calendar toggles and colors (servers' colors with alpha, calendars without color), _Show removed events_, jump to date, keyboard (`j`/`k`/`↑`/`↓`, `o`, `t`, `a`/`w`/`m`, `←`/`→`), narrow windows
- [ ] The event page by eye: when in two zones, recurrence in words for real rules (and in other locales: the wording is English with localized day and month names), attendees with answers per copy, copies and differences, mail and the merged history; the links from the invitation card, a person's meetings and search results
- [ ] `with:` and `is:event`/`is:contact` on a real archive: whether the five items above the mail results are the useful ones (events by start, then contacts)

### CardDAV and CalDAV servers

Radicale 3.8, Baïkal 0.10 (sabre/dav), Stalwart 0.16 and Nextcloud 35 are covered by tests/integration/tests/dav*.rs (TESTPLAN step 4). For the other servers: Against each real server: turn it on, first sync, edit and delete a contact and an event on the server (or another client), sync again, and check versions, removals and the task log; then wait long enough for the server to forget the sync token (or remove it from the database) and check that a sync recovers by ETags.

- [ ] Nextcloud (`https://host/remote.php/dav`, app password): discovery through the principal, `sync-collection`, shared and subscribed calendars, the birthday calendar (read-only, generated), deleted-items trash. Discovery through the well-known redirect and the principal, `sync-collection`, a refused token read in full by ETag, changes, removals, a removed address book and the calendar instances: proved against Nextcloud 35.0.1 with the `nextcloud` profile by tests/integration/tests/dav.rs and dav_calendar.rs; app passwords, shared and subscribed calendars, the birthday calendar and the trash remain
- [ ] iCloud (`https://contacts.icloud.com`, `https://caldav.icloud.com`, app-specific password): home sets on `pNN-contacts.icloud.com` and `pNN-caldav.icloud.com` (allowed as the iCloud family), whether the partition host changes over time (it would show as a removed and a new collection), `getctag`, multiget sizes
- [ ] Fastmail (`https://carddav.fastmail.com`, `https://caldav.fastmail.com`, app password): discovery, sync tokens, colors
- [ ] Google with OAuth: whether Thunderbird's client is granted `auth/carddav` and `auth/calendar` for Holtmail, whether `calendar.readonly` works for CalDAV with an own client, the well-known redirect of `https://www.googleapis.com/.well-known/carddav`, the CalDAV principal at `https://apidata.googleusercontent.com/caldav/v2/`, Google's sync-token expiry, contact groups as items, rate limits (`429`, `Retry-After`)
- [x ] Radicale and Baïkal (`https://host/` or `https://host/dav.php`): discovery without a well-known redirect, servers that answer `sync-collection` differently (Radicale's tokens, Baïkal's SabreDAV): proved against Radicale 3.8.3 and Baïkal 0.10.1 by tests/integration/tests/dav.rs (discovery from the server's address through their well-known redirects, and at Baïkal's `/dav.php/` without one, first and incremental syncs, a removed address book, a refused token read in full by ETag), dav_faults.rs (Baïkal without `sync-collection` listed by ETags, a `500`, wrong passwords, broken items), dav_calendar.rs, dav_people.rs, dav_export.rs and dav_scale.rs (2 000 cards). Found and fixed: sabre/dav answers a whole multiget with `500` when it cannot parse one stored card, which stopped the whole address book on every sync; now that batch is read item by item and the broken card counts as failed
- [ ] Yandex (`https://carddav.yandex.ru`, `https://caldav.yandex.ru`, app password): whether the presets are still right and `sync-collection` works
- [ ] A mail domain with a well-known redirect to a DAV host elsewhere (credentials follow only where the well-known lookup led), and one without well-known addresses (the dialog asks for the address). The first half is proved by tests/integration/tests/dav_faults.rs: a well-known address on another host (a server in the test process) leads to Baïkal, is asked without credentials and nothing else is sent there
- [ ] Servers that send CRLF raw versus `&#13;` inside `address-data`/`calendar-data`, servers that encode hrefs differently between answers, very large calendars (many multigets of 50), items over 16 MiB, `507` paging of `sync-collection`. Many multigets: 2 000 cards from Radicale in 40 multigets (tests/integration/tests/dav_scale.rs)
- [ ] JMAP accounts with an API token on a server without JMAP contacts: whether its DAV accepts the token as a bearer (it may need an app password; then the account needs its own DAV credentials, not built). JMAP accounts whose session offers contacts read them through JMAP since Phase 19
- [ ] The dialog, the account row's contacts line and the Google consent flow in a browser

### Microsoft Graph contacts and calendars

Only an in-process fake (Rust) and a local Python fake (token endpoint and Graph, reached through `[oauth.microsoft]` `authorize_url`, `token_url` and `graph_url` on loopback) were used.

- [ ] A real Microsoft 365 account and an Outlook.com account: turn it on, sign in again, first sync, edit and delete a contact and a calendar event in Outlook, sync again; check versions, removals and the task log
- [ ] Whether Thunderbird's Microsoft client is granted `Contacts.Read` and `Calendars.Read` for Holtmail (an own client needs them as delegated Graph permissions), and whether a tenant's admin consent is needed
- [ ] Contact delta: that `$select=id,changeKey` works on `/contacts/delta`, delta links survive weeks, and an expired one answers `410` (`syncStateNotFound` or `resyncRequired`) rather than `400`
- [ ] Series: that `$select=…,cancelledOccurrences&$expand=exceptionOccurrences` is accepted on `/me/calendars/{id}/events/{master id}`, that exceptions come back in full, and that changing or cancelling only one occurrence changes the master's `changeKey` (else such changes show only at the master's next change)
- [ ] Times: all-day events' `start`/`end` in UTC (the date kept as written), `originalStartTimeZone` values in the wild (Windows names, IANA names, `tzone://Microsoft/Custom`), events created in another zone than the recurrence's, and the weekly and monthly patterns against Outlook's own expansion
- [ ] Ranking: Graph has no `SEQUENCE`, so a Graph copy of a meeting ranks after copies that state one (an invitation in mail, a CalDAV copy). Reading Exchange's `PidLidAppointmentSequence` through `singleValueExtendedProperties` would fix it; not built
- [ ] Shared and group calendars, calendars of other people the account can read, and calendars with thousands of events (every sync lists all their ids and change keys)
- [ ] Throttling (`429` with `Retry-After`) on a large first sync; contact photos are not read
- [ ] The dialog and the Graph consent flow in a browser

### JMAP contacts and calendars

An in-process fake (Rust), a local Python fake (`scratchpad/e2e18/fakejmap.py`) and Stalwart 0.16.27 in containers (tests/integration/tests/jmap_pim.rs and jmap.rs) were used.

- [ ] Fastmail with an API token that has the contacts and calendars scopes: whether the session advertises `urn:ietf:params:jmap:contacts` (RFC 9610) rather than only its older vendor capability, and `urn:ietf:params:jmap:calendars`; first sync, a change and a deletion in Fastmail's web app, sync again
- [x] Stalwart: contacts and calendars through JMAP: first sync of two address books and a calendar, a changed, a removed and a new card, a removed and a new event (`Europe/Berlin`, `America/New_York`, an all-day event), a removed address book kept in the archive: proved by tests/integration/tests/jmap_pim.rs
- [ ] Stalwart: tasks in calendars
- [ ] Whether real JSContact cards read well through `calcard` (names with components, `phones` features, photos as `blobId` media, which are not fetched) and JSCalendar events from both RFC 8984 servers (`recurrenceRules`, `sendTo`, `replyTo`) and JSCalendar 2.0 servers (`recurrenceRule`, `calendarAddress`)
- [x] `cannotCalculateChanges` read in full without new versions, and whether `/get` answers serialize an unchanged object the same way every time: Stalwart answers `cannotCalculateChanges` for a state ahead of its own (the test rewrites the stored state; a server that forgot old states could not be provoked, Stalwart treats a state older than its log as "from the start") and writes the keys of an unchanged object in a different order on every answer. Before 2026-11-06 every unchanged item read again became a new version; fixed by storing JMAP items with sorted keys: proved by tests/integration/tests/jmap_pim.rs
- [ ] `maxObjectsInGet` and query limits on large address books, `hasMoreChanges` paging
- [x] A session at `/.well-known/jmap` that redirects to another host (credentials follow only to where it led; mail and contacts), a session address that redirects to another host (refused, no credentials sent there), and an `apiUrl` on another host than the session (used for mail and, since 2026-11-06, for contacts and calendars too, with the credentials going only to the session address and that API, https only): proved by tests/integration/tests/jmap.rs and jmap_pim.rs with redirecting servers in the test process
- [ ] The dialog for JMAP accounts in a browser (empty address reads through JMAP)

## Access and sign-in

- [x] Every API route answers owners, admins, members (granted and not) and anonymous requests as documented, and only with statuses its OpenAPI operation declares (the description lacked `403` on four writes; fixed): tests/integration/tests/access.rs
- [x] Sign-in limits, session expiry and sign-out, CSRF refusals and the one-time setup link through the real binary: tests/integration/tests/signin.rs

## Upgrade and deployment

- [x] The new image on an existing data volume: migrations 0006–0015 apply, the search index is rebuilt in the background at start (with contacts and events and the calendar's instances), search works again when it finishes. The image of `fed635b` archived an upload with an invitation and a Dovecot account into a volume; `holtmail:dev` applied migrations up to 0015, rebuilt the index, read the invitation into the calendar and built people at start, and counts, search, labels, people, the calendar and another sync answered as before: tests/integration/tests/upgrade.rs (`IT_PROFILES=docker`). The old version had no contacts, so their part of the rebuild is proved only by unit tests
- [x] Docker: `holtmail backup`, `restore` and `account edit` through `docker exec`, with `user add`, `user password` and `account list`, and `restore --input -` into a fresh volume served by a new container: tests/integration/tests/docker.rs (`IT_PROFILES=docker`). `archives`, `import`, `sync`, `reindex` and `account add-*` cannot run beside `serve` (they open the search index it holds); they now say so
- [x] The setup link, sign-in and writes behind a reverse proxy with TLS: Caddy 2.10.2 with the stack's CA and `public_url`: the link names the public address, the cookie is `Secure`, writes with the public origin pass and others are refused, and the sign-in limit, which sees the proxy's address for everyone, is per email: tests/integration/tests/caddy.rs. Still to see: nginx and Traefik, and a public URL under a path prefix
- [x] `tls_ca_file` with TLS and STARTTLS IMAP, refusal without it and for a wrong host name: proved against Dovecot by tests/integration/tests/imap.rs
- [x] `tls_ca_file` in the Linux Docker image: IMAP over TLS to Dovecot and CardDAV to Radicale with certificates from the stack's CA, through `HOLTMAIL_TLS_CA_FILE` in `holtmail:dev`, and the same IMAP server refused without it: tests/integration/tests/docker.rs (`IT_PROFILES=docker`). The reqwest clients share one TLS setup, so JMAP, Graph, OAuth, S3 and images on Linux follow from the CardDAV case
- [ ] `tls_ca_file` with a real private CA that has an intermediate (only the root in the file), and with a CA whose certificates lack `subjectAltName` or `extendedKeyUsage` (refused by rustls and by macOS)

## Deletion, ledger, webhooks, SMTP and PDF (Phases 21–26)

- [x] Deleted mail stays deleted against a real IMAP server: a sync, a copy of the same bytes in another server folder and a re-import leave it out, and the ledger enters the deletion: tests/integration/tests/deletion.rs on Dovecot. JMAP and Graph (UIDs fetched when they have no location) are proved by unit tests only (`never_brings_back_a_deleted_message` with unordered UIDs)
- [x] The ledger names a changed entry and a changed blob of a running instance, through the CLI beside `serve` and the API: tests/integration/tests/deletion.rs
- [ ] Deleting tens of thousands of messages at once: time, memory, and the search index after it
- [ ] Ledger heads kept outside the instance in practice (a webhook receiver, a printout) and checked with `--anchor` months later; `--blobs` on S3 over a large archive (time and cost)
- [x] Webhooks reach a receiver signed (HMAC-SHA256), retry after a refusal, use a renewed secret, stop when turned off, and carry no mail: tests/integration/tests/webhooks.rs with an in-test receiver
- [ ] Webhooks to real receivers (an automation service, a chat bot) and their signature checks; a receiver behind TLS with a private CA
- [x] SMTP through a real MTA: Postfix relays over STARTTLS it verifies; a journal report is unwrapped byte for byte, an unknown recipient and an oversized message are refused: tests/integration/tests/smtp.rs
- [ ] Real journaling: an Exchange Online or Exchange Server journal rule, a Google Workspace routing rule, Postfix `always_bcc` in production; real journal reports from Microsoft (the fixture is invented) and how Bcc recipients show
- [ ] The SMTP listener reachable from the internet: TLS with a public certificate, clients that require `SMTPUTF8` or `CHUNKING`, abuse (the listener never relays, but it accepts what routes allow)
- [x] PDFs read by real tools: qpdf accepts every PDF, pdftotext finds Latin and Cyrillic text and the page number, and the original comes back out of each: tests/integration/tests/pdf.rs
- [ ] PDFs by eye: layout of long headers, many attachments, long messages over many pages; Chinese, Japanese, Korean and emoji (shown as boxes), Hebrew and Arabic (not reordered)
- [x] A port mixed up with its security named quickly, both ways, against Dovecot: tests/integration/tests/imap_faults.rs
- [ ] Gmail and Outlook with `public_client = false` and no own client: the buttons gone in the web app, existing accounts asking to sign in again
- [ ] Calendar and vCard attachments from real senders (Outlook, Google Calendar, Apple) found by their words after the start-up pass

## Push and fast change tracking (Phases 27–31)

- [x] IMAP IDLE against real servers: new mail archived within seconds on Dovecot 2.4, GreenMail 2.1 and Stalwart 0.16 with syncs only on request; a connection Dovecot drops (`doveadm kick`) reconnects and catches up (tests/integration/tests/watch.rs)
- [x] JMAP push against Stalwart: mail and contacts through the event source (watch.rs, watch_pim.rs)
- [x] JMAP contacts with CalDAV calendars: a session without calendars plus Stalwart's CalDAV (watch_pim.rs)
- [ ] `NOTIFY` against a real server (only the in-process fake speaks it; check whether Dovecot 2.4 offers it with this config)
- [ ] IDLE on real Gmail (on `[Gmail]/All Mail`), Outlook and Yandex for an hour or more: NAT timeouts, the ten-minute renewal, OAuth watches reopened every 45 minutes
- [ ] Fastmail: JMAP push for mail and contacts, and its CardDAV/CalDAV with an app password as the separate DAV password
- [ ] WebDAV-Push from a real server (Nextcloud with its DAV Push app) to a Holtmail with a public https address: registration, a notice decrypted, renewal before expiry, `410` after a restart
- [ ] The CalDAV offer for an existing JMAP account without calendars, and its button in the web app
- [ ] The web app: Live, Reconnecting and Scheduled marks, the Watch for changes menu and its folder dialog, the contacts interval menu, the DAV password field
- [ ] Many watched accounts on one server: connection counts, memory, the four-run queue under a burst of changes

## The web app, by eye

None of these screens has been opened in a browser; they were checked with types, lint, tests and builds only.

- [ ] First run: create the owner, create an archive, connect mail (IMAP, Gmail and Outlook with OAuth, JMAP, Graph), upload a file
- [ ] Reading: message list, labels in the sidebar and in rows, the reader with attachments and thumbnails, the remote image banner and modes
- [ ] Search: operators (`label:`, `attachment:`), the "In attachment" chip, ⌘K
- [ ] Settings: archives (save images, older mail, export), sources (edit connection, sign in again, check for new mail, what to archive), people and access, reading, tasks (including the interrupted state), network
- [ ] Light and dark themes, narrow windows, keyboard navigation
- [ ] Settings → Integrity, Webhooks (the secret shown once, deliveries refreshing) and SMTP; the deletion dialog from search results and the reader (review, sample, typed count, back to the list); Download PDF in the reader and Zip of PDFs in the export menu

## Log

- 2026-11-05: list written from the "Not verified" notes of Phases 4–11, the JMAP and Graph sources and the review fixes.
- 2026-11-05: CardDAV and CalDAV servers added (Phase 15).
- 2026-11-05: People added (Phase 16).
- 2026-11-05: Calendar added (Phase 17).
- 2026-11-05: Microsoft Graph contacts and calendars added (Phase 18).
- 2026-11-06: IMAP items proved against Dovecot and GreenMail by tests/integration/tests/imap*.rs (step 2 of TESTPLAN.md): first and incremental syncs, folder names and roles, expunge and moves, renumbering, CONDSTORE and the daily flag scan, minimum age, TLS/STARTTLS/plain, connection changes, wrong and locked sign-ins, a server cut or stalled mid-fetch, malformed and 20 MB messages, unselectable folders, the scheduler, restarts and a killed sync, 5 000 messages. Fixed: a connection change to a server with the same `UIDVALIDITY` hid its mail.
- 2026-11-05: JMAP contacts and calendars added (Phase 19).
- 2026-11-05: People by hand and contacts and calendar exports added (Phase 20).
- 2026-11-05: review fixes: one-`VCALENDAR` `.ics` export.
- 2026-11-06: `tls_ca_file` items added (integration test harness, TESTPLAN step 1); note on the image proxy fix.
- 2026-11-06: JMAP mail, contacts and calendars proved against Stalwart 0.16.27 by tests/integration/tests/jmap.rs and jmap_pim.rs (step 3 of TESTPLAN.md); the IMAP suite also runs on Stalwart. Fixed: a well-known redirect to another host lost the credentials (JMAP mail), and Stalwart's key order made a new version of every unchanged JMAP card or event read again. Decided and done: an on-request account's interrupted sync resumes once at start.
- 2026-11-06: CardDAV and CalDAV proved against Radicale 3.8.3, Baïkal 0.10.1, Stalwart 0.16.27 and (profile `nextcloud`) Nextcloud 35.0.1 by tests/integration/tests/dav*.rs (step 4 of TESTPLAN.md): discovery, syncs, removals, refused tokens, ETag listings, faults, recurring events across offset changes in IANA, Windows and made-up zones, copies across servers, export round trips, 2 000 cards. Fixed: one card sabre/dav cannot parse stopped its whole address book. Changed (lead's request): JMAP contacts and calendars use an `apiUrl` on another host as mail does.
- 2026-11-06: OAuth (own client, XOAUTH2, refresh, rotation, revocation), remote images through real proxies and S3 backup and restore proved against mock-oauth2-server 6.1.2 with Dovecot 2.4.5, nginx with squid and two SOCKS5 proxies, and MinIO (step 5 of TESTPLAN.md: tests/integration/tests/oauth.rs, images.rs, s3.rs). Added `[oauth.<provider>] imap_host`/`imap_port` so OAuth IMAP accounts can use another server (national clouds, the tests). Real Google and Microsoft sign-in and images on the real internet stay open.
- 2026-11-06: step 6 of TESTPLAN.md: access matrix over every API route and role, sign-in limits, sessions, CSRF and the setup link, a 20 000-message archive exported, backed up and restored with flat memory, the `fed635b` image upgraded in place, the CLI through `docker exec`, `tls_ca_file` in the Linux image, and Caddy in front with `public_url` (tests/integration/tests/access.rs, signin.rs, archive_scale.rs, upgrade.rs, docker.rs, caddy.rs). Fixed: `403` missing from four operations of the OpenAPI description; a clear error for CLI commands that need the search index while `serve` runs. Changed: remote images of up to four messages are saved together.
