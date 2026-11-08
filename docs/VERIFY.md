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

## Email aliases

Detectors were written from the services' source code and documentation and tested on invented messages; directories against fakes of their APIs. SimpleLogin is also proved end to end against a self-hosted v4.82.4 by `tests/integration/tests/aliases.rs` (profile `aliases`).

- [x] SimpleLogin, self-hosted: a forwarded message with `X-SimpleLogin-Original-From` and the default sender format, DKIM checked by OpenDKIM, a custom alias with a fresh signed suffix, mailboxes, name, note, switch, delete through the API (aliases.rs)
- [ ] SimpleLogin hosted: the DKIM domain of forwards (`simplelogin.co`), the other sender formats and the account setting that drops `X-SimpleLogin-Original-From`, listing over several pages with a real account, a Proton Pass alias
- [ ] addy.io: a forwarded message with "replies via Reply-To" on and off; the DKIM domain of forwards; `POST /api/v1/alias-recipients` with `alias_id` and `recipient_ids` (its body was not read in addy.io's source); a soft-deleted alias deleted again
- [ ] Firefox Relay: a forwarded message from a random and a premium subdomain mask; the DKIM domain (sent through SES); `domainaddresses` for a free account
- [ ] Fastmail: Masked Email with a real API token (session, `emailPrefix`, deleted masks)
- [ ] DuckDuckGo: the `From` of a forwarded message (only the reply address form is documented)
- [ ] Apple: `X-Icloud-Hme` on a Hide My Email forward (documented only by users), whether `From` is rewritten, and Sign in with Apple relay mail
- [x] A reply through SimpleLogin, self-hosted: routed to the sender's reverse alias from the alias's mailbox, handed to SimpleLogin's SMTP, delivered with the alias as `From`; the leak guard refusing a plain `Cc` (alias_reply.rs)
- [ ] Replies through the real services: SimpleLogin hosted (DKIM of the mailbox's provider required for replies, a first message to a new contact through `POST /api/aliases/{id}/contacts`), addy.io and DuckDuckGo (an encoded reverse address delivered with the alias as `From`), Apple Hide My Email (`reverse_alias` to Apple's rewritten `From`, from the iCloud address the alias forwards to; whether Apple accepts that reply and delivers it with the alias as `From`), Firefox Relay premium (`reply_header`: the `Reply-To` it sets is trusted only with its DKIM); Fastmail Masked Email and own domains (`direct` through a transport whose `allowed_from` covers the alias)
- [ ] Compose by eye: the alias path on the From line ("To SimpleLogin's reverse address for …, from your mailbox …") and the reasons under it, the leak notice with _Send through the alias service_, _Send as_ the alias identity (and the line when there is none), _Remove_ and _Send anyway…_, the send-anyway confirmation from the button and from ⌘/Ctrl+Enter; Settings → Aliases: the Replies choice per alias and its "set for the domain/service" note, _Replies through alias services_ (the three explanations, By service, By domain with the suggested domains and Remove), in both themes and at narrow widths
- [ ] People after a SimpleLogin archive: senders behind reverse aliases joined with their contacts, aliases not listed as people; a first message to a new contact through a made reverse alias (and a Cc fixed with _Send through the alias service_) shows the contact, not the reverse address, in People and the reader once the sent copy is archived
- [ ] Compose by eye after the review fixes: a send refused by the server with leaks the draft did not show (the alias service gave another reverse address) opens the leak notice and the send-anyway confirmation; _Send as <alias>_ on a reply to a plain message in an alias thread; the `no_alias_identity` line on a `direct` reply without an alias identity
- [ ] Settings → Aliases: connect a service, list, create (random and custom), edit, switch, delete at the service, add by hand, filters; the reader's alias line with signed, known and not verified mail

## Message insights (Phases 35–42)

Extractors were tested on 185 invented messages and their fixtures; the walk against SQLite and a counting blob store and, in `tests/integration/tests/insights.rs`, killed and resumed in the real binary.

- [ ] One-time codes from real senders (banks, Google, Apple, Microsoft, Telegram, local services) in several languages: the code found, nothing found in their newsletters
- [ ] `List-Unsubscribe` shapes of real bulk senders (ESP-sent, Mailman, Google Groups)
- [ ] The walk over the maintainer's archive: time, memory, the `analyse` task, `holtmail insights explain` on a wrong guess
- [ ] Copy over plain HTTP in Safari and Firefox (the `execCommand` path and the manual field)
- [ ] The fuzz target in `crates/auth-results/fuzz` built and run once (the `insights` and `links` targets ran on 2026-11-06, and `html-scan` in Phase 37, all with the PATH/RUSTC workaround in their READMEs)

Phase 36 (action links and parcels) was tested on 327 invented messages (65 links, 27 parcels), the extractor fixtures, proptests and short fuzz runs; nothing below was checked against real mail or live sites.

- [ ] Action links from real senders (password resets, sign-in links, order and check-in buttons) in several languages: the right button chosen, none in their newsletters
- [ ] Real Safe Links, Google `/url` and Proofpoint v1–v3 wrappers from live tenants decode to the right destination
- [ ] `trackers.toml` against real click hosts of the listed ESPs; branded Salesforce MC, Mailgun and Braze hosts are not recognised by design
- [ ] `display_host` on real IDN domains and look-alikes against Chromium's address bar (no per-TLD script rules, no top-domain skeleton list)
- [ ] Carrier tracking URL templates in `carriers.toml` open the right page for a real number of each carrier (the S10 CN template least certain)
- [ ] The FedEx 12-digit check (weights 3,1,7 from the left) and the other check digits against real tracking numbers; the FedEx samples were recalled from memory
- [ ] Release timing of stage A on an idle machine: the 20k-anchor message took 34–36 ms against 40 ms under load
- [ ] Advisories for `idna`, `unicode-security` and `publicsuffix` with `cargo-deny` or `cargo-audit` (not installed here)

Phase 37 (reservations, orders and passes) was tested on 364 invented messages (37 new cases, 156 hard negatives), extractor fixtures, built-in-test pkpass archives, proptests and 60 s fuzz runs; no real sender markup or real pass was seen.

- [ ] Real airline, hotel, rail, event and retail JSON-LD and microdata, including Gmail-style `EmailMessage` with `potentialAction`: the right items, merge keys and times; the microdata rules of html-scan (hidden subtrees kept, `itemref`, depth) against real sender markup
- [ ] Real `ParcelDelivery` carrier names and URLs against `insight-schemaorg/data/carriers.toml`, and their merge with the heuristic parcel (check digit and tracking link kept)
- [ ] `insight-schemaorg/data/currencies.toml` against the current official ISO 4217 list (written from memory); whether real senders put prices in JSON floats
- [ ] Real `.pkpass` files from airlines, event and store issuers: `semantics` coverage, `flightNumber` as number or string, seat shapes, `relevantDate` formats; pass signatures are never verified
- [ ] `zip` 9.2.2: `evaluate_dependency` only knows npm, so it found nothing; licence MIT checked by hand; advisories unchecked (no `cargo-deny` or RustSec run)
- [ ] The Phase 37 fuzz targets built and ran only locally with the PATH/RUSTC workaround: `html-scan/scan` 381,550 runs, `insight-schemaorg/extract` 38,687, `insight-pass/extract` 601,434 and 414,659, `insights/extract` 33,373 (seeded with the corpus), all 60 s, no crash; `insight-pass` has no seed corpus
- [ ] The nightly `.github/workflows/fuzz.yml` on its first scheduled run (never run on GitHub; it relies on `rustup toolchain install` reading `rust-toolchain.toml`, rustup 1.28 or later)
- [ ] Release timings of the four 2 MiB microdata bombs (41–55 ms scan, 29–44 ms tokenizer alone on a loaded machine) on an idle machine

Phase 38 (trust and categories) was tested with the category conformance suite, 8 invented category fixtures, stage-B unit tests on hand-written auth facts, the store conformance area `insight_categories`, counting blob stores in ingest and the server, and kind and topic labels on all 364 corpus messages (kind accuracy 1.000, topic precision and recall 1.000). The labels follow the §4.2 rules, so they prove the rules are applied, not that they fit real mail. No real `Authentication-Results` field was read.

- [ ] Provider presets for `trusted_authserv_ids`: for Gmail, Fastmail, Outlook, iCloud and Yandex, the `Authentication-Results` shape, the authserv-id, whether `dkim=pass` records `header.d` or only `header.i`, and whether the provider strips forged fields that carry its own id (RFC 8601 §5). `trusted_authserv_preset` returns an empty list for all five until this is done; fill in each one only after its check
- [ ] The suggested authserv-id on a real synced account: what it suggests, and whether the 95% share of 200 messages (at least 50) holds when a forwarder or a list MX stamps some of the mail
- [ ] Categories spot-checked on 100 messages of the maintainer's archive: kind (automated mail from carriers or check-in senders with few headers comes out `personal` by the rules), topics, trust state, and a domain override changing the list and counts at once
- [ ] An alias service's mail (`relayed`) and a forged bank sender (`unverified`, no Finance) seen in the real reader
- [ ] Stage-B pass speed on an idle machine: 19,650 rows/s in release here (slowest batch of 500 at 44 ms) against the 20,000 rows/s target; `category_counts` about 75 ms and an override on a 50,000-message domain about 1.9 s on 500,000 messages
- [x] The integration `access.rs` matrix as a whole: the four category rows answered as documented for all five roles only with the route-coverage assert turned off, because 12 routes from earlier phases (alias services, aliases, alias providers, `POST /api/push/dav/{id}`) have no rows; add them and run `mise run it -- access` Done 2026-11-07: every route now has a row and the suite passes with the assert on.
- [ ] "Everyone at \<domain\>" in the chip menu shows the From host (`news.example.com`) while the server stores the registrable domain (`example.com`); check it reads well, or add `sender_domain` to the DTO

Phase 39 (domain data) was tested with format tests and proptests in `holtmail-category-data` (round trip, deterministic bytes, a 40 MiB gzip bomb, every header and line rule, errors that never carry row text), the category conformance suite on two invented tables and the bundled one in `holtmail-category-domains`, a reload racing 4 reader threads, the builder's recorded SPARQL fixtures with invented QIDs and reserved example domains plus a loopback HTTP server (a truncated 200, a 429 with `Retry-After`, giving up after 5 attempts), and server stage-B tests where a domain table votes and a reload mid-pass converges without reading a blob. The first snapshot was built by hand against the live Wikidata endpoint with `--first-release` and rebuilt after review (mailbox providers excluded, subdomain sites keyed by their own host): version `2026110701`, 28,370 rows, 347,649 bytes of gzip (cap 2 MiB), purchases 14,902, travel 12,715, social 746, jobs 341, events 104, finance, health and government 0; 860 s, 575 queries, 579 requests, no truncation and no failure; 24 mailbox-provider sites skipped; `check` passes and golden is 193 of 193 (present and `absent` entries). The first build (`20261107`, 27,879 rows) loaded in 7.8 ms in release.

- [ ] Sensitive rows reviewed by the maintainer: `apps/category-builder/data/sensitive-reviewed.toml` ships empty, so the first snapshot has no `finance`, `health` or `government` topic (73,593 unreviewed sensitive rows stripped, 73,078 dropped). Until rows are reviewed, every `mise run categories-snapshot` fails the more-than-50 gate on purpose (with `--previous`, every sensitive topic the stripped earlier snapshot lacks counts as unreviewed); the report lists the rows. The government agency tree pulls in municipalities, US cities, embassies and school districts (the most crowded domains, such as `kejaksaan.go.id` with 532 organisations, are all government), so look at those classes before approving any
- [ ] A second human spot check of the snapshot rows (30 were checked by eye at build time; chains such as `hilton.com` and `marriott.com` carry both `purchases` and `travel`) and of `data/golden.toml` (non-sensitive present domains and the `absent` mailbox-provider list), and of `mailbox_domains` in `data/industries.toml` (about 170 providers; a missing one would get the topics of its parent company)
- [ ] Snapshot coverage: about 90k domains before the sensitive gate against §8's ~132k, because 36,506 websites have a path and are skipped; decide whether that is enough
- [ ] `flate2` 1.1.10 and `reqwest` 0.13.5: `evaluate_dependency` only knows npm and Go, so it found nothing; licences (MIT or Apache-2.0, `miniz_oxide` and `zlib-rs` with Zlib) checked by hand; advisories unchecked (no `cargo audit` or `cargo-deny` installed)
- [ ] The gzip bytes depend on the `flate2` backend (`zlib-rs` when `zip` is in the same build, `miniz_oxide` for the builder alone); always refresh the snapshot with `mise run categories-snapshot` so the reviewed diff compares like with like
- [ ] Memory of the loaded table (not measured; §8 estimates about 6 MB for 132k rows) and the start-up log line `domain table loaded`, which every command logs because `Services::open` loads the table

Phase 40 (search) was tested with the index conformance cases, query and store queue tests, walk and deletion-guard unit tests in `holtmail-server`, and `tests/integration/tests/insights_search.rs` against the real binary with 60 invented messages, where the reindex queue drained in under a second.

- [ ] Reindex throughput on a real-size archive: an override or data-file change touching tens of thousands of messages, and `category:` search catching up at about 4,000 messages a minute on SSD (§13), with the time, the `analyse` task and memory recorded
- [ ] The first `serve` after upgrading a real data volume from Phase 39: the index of the older schema emptied and rebuilt in the background, insight searches and deletion reviews refused until `index_rebuild` clears, then exact (the integration suite fakes the older schema by editing `meta.json`; the `docker` upgrade suite was not extended)
- [ ] `holtmail delete` with an insight operator during a backfill prints the 409 message and wakes the walk

Phase 41 (one-click unsubscribe) was tested with the `OneClick` cases against an in-process HTTPS server with fixed resolver answers, the `unsubscribe_requests` store conformance area, API tests with a counting fake sender, and `tests/integration/tests/insights_coverage.rs` and `insights_one_click.rs`: OpenDKIM in `sl-mx` and an nginx on 443 with the stack's own CA, reached on private test subnets allowed only by the `insights-test-data` build. No real sender was contacted.

- [ ] One real one-click unsubscribe from the release image against a real sender: public DNS and a public CA, the stored outcome (`accepted` for a 2xx, `redirected` for a 3xx; senders that answer 3xx to a correct one-click POST would read as "finish on their site"), and that the list stops
- [ ] `HttpOneClick` against real providers (ESPs, Mailchimp-, SendGrid- and Mailgun-style endpoints): TLS with real chains and SNI, whether any refuse the hand-built `multipart/form-data` body or need a `User-Agent`, the 10 s timeout, IPv6-only and mixed A/AAAA hosts
- [ ] The sender sees the server's public address, by design (INSIGHTS §17.6): confirm it is the expected egress address, and that `[insights] one_click = false` stops every request and shows `disabled`
- [ ] Real `Authentication-Results` from providers and OpenDKIM versions other than the one in `sl-mx` (`List-Unsubscribe` and `List-Unsubscribe-Post` in `h=`, oversigned and prepended fields) giving the expected one-click reason
- [x] The integration `access.rs` matrix with the new `POST /api/messages/{id}/unsubscribe` row: it answered as documented only with the route-coverage assert off, for the same 12 missing earlier routes as Phase 38 Done 2026-11-07: every route now has a row and the suite passes with the assert on.
- [ ] A re-read failure in `fresh_insights` (blob missing or no longer parsing) answers 500 and logs the message id, without marking the row `unreadable`; decide whether that is enough

Phase 42 (data file updates) was tested with manifest unit tests and proptests in `holtmail-category-data`, signature, expiry, rollback and file checks in `holtmail-category-domains` with an internal key list and the `test-key` feature, the `DataDownload` cases against an in-process HTTPS server, the builder's release tests with the committed test seed and invented UT1 fixtures, server `Updater` tests with a fake downloader under `insights-test-data`, and `tests/integration/tests/insights_data.rs`: an nginx with the stack's own CA serving releases signed with the test key to the containerised server, a 200 then a 304, refusals, a pinned and an unpinned redirect. No real GitHub release was fetched and no production key exists.

- [ ] Production signing keys. Both slots in `crates/category-domains/src/manifest.rs` are `None`, so a release build refuses every manifest with `no_release_key` and `update = true` changes nothing. On a trusted machine: run `holtmail-category-builder keygen --out <seed file> --key-id <id>` (seed as hex, mode 0600, never overwrites); put the printed `ReleaseKey { id, public: [..] }` into `CURRENT_KEY`; store the seed in the GitHub Actions secret `CATEGORIES_SIGNING_KEY` (only the `sign` job sees it) and keep an offline copy; set the repository variable `CATEGORIES_KEY_ID` to the id; delete the seed file. Generate the next pair the same way, put its public key into `NEXT_KEY` and keep its seed offline. Ship a release with both keys before the current key signs anything; to rotate later, switch the secret to the next seed only after a release with `NEXT_KEY` has shipped (category-domains README, "Keys and rotation")
- [ ] The first real run of `.github/workflows/categories.yml` (never run on GitHub): the `build` job without secrets, `gh release download` of `categories-latest` on the first run (no release yet, the bundled snapshot as the previous Wikidata file) and later, the `manifest` job, the `sign` job with only the key (OpenSSL 3 on the runner signing with `scripts/sign-manifest.sh`; the script was checked locally against OpenSSL 3.6 to give the builder's exact signature), the `publish` job's asset comparison, the tag `categories-YYYYMMDD` and the rolling `categories-latest` upload order (data files, then signature and manifest). A second run on the same UTC day fails at `gh release create` because the dated tag exists
- [ ] The weekly `wikidata --previous` build fails the more-than-50 sensitive gate until the maintainer reviews the stripped sensitive rows into `sensitive-reviewed.toml` (about 62,900 in Phase 39 count as new against a previous file that carries none), so no release is published until then
- [ ] A real server with `update = true` fetching the signed release from GitHub: the redirect from `github.com` to `objects.githubusercontent.com` or `release-assets.githubusercontent.com` (only these are pinned; a new GitHub asset host would be refused with `download_refused`), whether the asset host honours `If-None-Match` and answers 304 with a stable `ETag`, the About page showing the downloaded version, and the walk re-deriving categories without a blob read
- [ ] The real UT1 build at the pinned commit `03501c64d230df9d84124176558d758351027be1` (committed upstream 2026-10-12): built here by hand as 97,980 rows (purchases 36,909, jobs 60,362, social 709) in 679,428 bytes of gzip in 0.44 s, 421 shared-platform, 15 mailbox-provider and 7 other hosts skipped; spot-check rows by eye, and watch whether the mirror keeps updating (drop UT1 after 3 months without updates, INSIGHTS §17.7)
- [ ] `ed25519-dalek` 3.0.0 (with `curve25519-dalek` 5.0.0, `signature` 3.0.0): `evaluate_dependency` only knows npm and Go, so it found nothing; licence BSD-3-Clause checked by hand; advisories unchecked (no `cargo audit` or `cargo-deny`)
- [ ] Settings → About by eye in both themes and at narrow widths: the version, the file list with bundled and downloaded badges, the UT1 attribution, the update line, the empty-state box, loading and error states
- [x] The integration `access.rs` matrix with the new `GET /api/insights/data` row: it answered as documented only with the route-coverage assert off, for the same 12 missing earlier routes as Phase 38 Done 2026-11-07: every route now has a row and the suite passes with the assert on.

## Sending (Phases 55–56)

Submission is proved against GreenMail 2.1.14 and Stalwart 0.16.27 by `tests/integration/tests/send.rs`; the rest against an in-process SMTP server.

- [x] SMTP without TLS to a local server and STARTTLS submission with the account's own password, per-recipient refusal, delivered bytes ending in the archived bytes, undo, `holtmail send --wait` (send.rs)
- [ ] Gmail: an `account` transport of a Gmail OAuth account sends through `smtp.gmail.com:465` with XOAUTH2; a "Send mail as" alias as `From`, and the rewritten `From` when the alias is not set up
- [ ] Microsoft: an `smtp` transport to `smtp.office365.com:587` with a password where the tenant allows SMTP AUTH
- [ ] Fastmail, Proton Bridge, Postfix with SASL and a company relay as `smtp` transports: the check, limits (`SIZE`), 8-bit and internationalized mail
- [ ] The server's Sent copy after sync replaces the `$HoltmailSent` location's flags (Gmail stores its own copy; Dovecot and Stalwart do not without `append`)
- [ ] A restart while a message is `sending` leaves it `uncertain`, and retry sends it once
- [x] Who may do what on every writing route (capabilities, grants, audit, transports, identities, the default identity, drafts, replies, forwards, send and the outbox) for the owner, an admin without a grant, a reader member, an ungranted member and anonymous: tests/integration/tests/access.rs
- [ ] A real client that drops the connection while `POST /api/drafts/{id}/send` runs (a phone going offline): retrying with the same `Idempotency-Key` gets the first answer or runs again, never `409` for long; after a crash the key frees itself within 10 minutes
- [ ] A transient outage of Google's token endpoint leaves Gmail messages `retrying`, not `failed`; a revoked sign-in fails them with "sign in again"
- [ ] Taking away a member's `sender` grant, removing them from an identity or narrowing a transport's `allowed_from` while a scheduled message waits: the message fails with the reason at its send time instead of leaving
- [ ] Turning `[write] send` off and restarting with messages queued: they wait as `retrying` (for up to 24 hours) instead of failing at once

## More transports and sent copies (Phase 58)

JMAP submission and a bounce are proved against Stalwart 0.16.27 by `tests/integration/tests/send_jmap.rs`; Graph, the Microsoft consent and the rewritten `From` only against in-process fakes.

- [x] An `account` transport of a JMAP account submits through Stalwart's `EmailSubmission/set`; the recipient gets it; the sent copy is in Sent without `$draft` (`onSuccessUpdateEmail`); the next sync archives it as the same message, once, and links nothing (send_jmap.rs)
- [x] A delivery status notification in the sender's inbox, read by a sync of a folder synced before, marks the outbox message `bounced` for that recipient with `5.1.1`, audited (send_jmap.rs; the report is imported into the mailbox by the test)
- [ ] A bounce Stalwart makes itself: the stack's Stalwart has no outbound route or DNS and refuses unknown local users at `RCPT`, so it never writes a report; needs an MTA route in the seed to a server that refuses with `550` after the hand-over
- [ ] Postfix, Exchange Online and Gmail bounces reaching a real mailbox are read (the fixtures in `holtmail-mime` are invented after their shape)
- [ ] Microsoft over IMAP: turning on `mail.send` and sending marks the account "Sign in again to send" (the message waits, the `account.needs_sign_in` webhook fires once, syncs go on); signing in again asks for `SMTP.Send`, and the waiting message then leaves through `smtp.office365.com:587` with XOAUTH2
- [ ] Microsoft Graph account: the same with `Mail.Send`; the message leaves through `sendMail`, Graph keeps the copy in Sent Items, the synced copy (Graph rewrites the bytes) is linked to the outbox row as a second copy
- [ ] Microsoft's consent screen with Thunderbird's client for `SMTP.Send` and `Mail.Send` (incremental consent for a borrowed client is not checked)
- [ ] Gmail without "Send mail as" for the identity's address: the synced Sent copy carries the account address, the identity shows the warning in Settings → Sending, and it disappears after the alias is set up and the next message is synced
- [ ] A `jmap` transport with a Fastmail API token; a `graph` transport with a hand-made token
- [ ] By eye: the transport form for each kind (JMAP with password and token sign-in, Graph with and without an endpoint and mailbox, an account transport of a JMAP and of a Graph account without server fields, a new JMAP address refused without the token again), shared transports of kind JMAP and Graph; the identity form's sent-copy choices with their lines and `append` greyed out with each reason and open once `mail.append` is on; the outbox's bounced recipients and notes; the account row reading "Sign in again to send" with the server's explanation under it

## Mailbox changes (Phase 60)

Flags, moves, deletes to the trash and for good, restores, sent copies and the server winning are proved on Dovecot 2.4.5 over IMAP, and keywords, deletes and restores on Stalwart 0.16.27 over JMAP, by `tests/integration/tests/mailbox_write.rs`; Graph only against the in-process fake.

- [x] A flags change through the API is held, sent by the writeback, seen on the server over IMAP, and `applied` after the next sync; a flag another client took away after it was sent ends `superseded_by_server` and the archive shows the server's flags (Dovecot)
- [x] A move, a delete into `Trash`, an expunge, and a restore of the archived bytes with `\Seen` and the `APPENDUID` UID (Dovecot); keywords, a delete into the trash and a restore over JMAP (Stalwart)
- [x] An identity with `append:Sent` leaves its sent copy, byte for byte with `\Seen`, in Dovecot's `Sent`; the outbox message's `sent_copy` is `applied`
- [ ] Gmail: labels through `X-GM-LABELS` in All Mail, a move between labels, a delete into `[Gmail]/Trash`, and `append:Sent` (Gmail already keeps a copy, so `provider` is right there)
- [x] A star asked for right after a move, before any sync, follows the message to its new folder and is confirmed there by the next sync (Dovecot)
- [ ] Microsoft Graph: turning on a mailbox capability shows "Sign in again to allow it"; signing in again asks for `Mail.ReadWrite` (with your own client and with Thunderbird's, which may not be granted it), the account then lists its `writable` capabilities, and held changes leave; then `isRead`/`flag` patches, `move`, `permanentDelete`, and whether an uploaded restore or sent copy shows as a draft and gets the upload time as `receivedDateTime`
- [ ] The retention job on a real account: the dry-run count in `GET /api/accounts/{id}/retention` matches what the next hourly pass moves to the trash (or expunges with `mail.expunge`), and nothing young or already in the trash is touched
- [ ] Gmail refuses nothing Holtmail sends as a label now that `Inbox`, `Sent`, `Drafts`, `Trash` and `Spam` are refused before the server; whether Gmail itself would have accepted `+X-GM-LABELS (\Trash)` was never checked
- [ ] An IMAP server without `MOVE` (and with or without `UIDPLUS`): a move of a message another client marked `\Deleted` copies it before expunging; a move that cannot expunge copies nothing
- [ ] A wrong password on an account with many held changes: one sign-in attempt per backoff step (30 s, 1 min, 2 min, …), not one per change, in the server's log and the mail server's
- [ ] A restart while a change is `sending`: it goes again after start; an append may be saved twice (documented)
- [ ] Fastmail and Cyrus over JMAP (only Stalwart is checked), and IMAP servers without `UIDPLUS` or `MOVE` besides GreenMail
- [ ] By eye: the reader's read, star, move, trash and restore buttons appear only with the capability; the held mark ("Not yet on the server") and its clearing once sync confirms; a failed change with Try again (only while the account allows that kind of change) and Dismiss (editors, owners, admins), with neither for a read-only member; a refused request's error staying visible; the list overlay (bold, star, clock, dimmed row) and its fading after confirmation; the shortcuts `u`, `s`, `v`, `#`; mark as read when opened (off by default) on Settings → Reading; the retention section in an account's Writing dialog (count asked after typing stops, warning, confirmation, turn off, the field waiting while the stored period loads, an error when the count fails); the sent-copy line on outbox rows

## Contacts and calendars from the web (Phase 61)

Creates, edits and deletes of an event and a contact in `write_through` mode, a held edit that never leaves, a conflict after upstream moved the same event, and "keep mine" overwriting upstream are proved on Radicale 3.8.3 and Stalwart 0.16.27 (CardDAV and CalDAV) by `tests/integration/tests/writeback.rs`; who may call each new route by `access.rs`.

- [x] An event and a contact created through the API reach the server; an edited title and a deleted contact land there; the item's timeline shows a `writeback` version (Radicale, Stalwart)
- [x] An edit in `held` mode stays off the server; upstream moving the same event makes it `conflict/overlap` at the next sync, and `keep_mine` with `write_through` overwrites upstream (Radicale, Stalwart)
- [ ] The JMAP writer against a real server (Stalwart JMAP contacts and calendars, Fastmail): field patches with `ifInState`, creates found again by `UID`, `stateMismatch` retries
- [ ] Microsoft Graph: turning on `pim.edit` on a Graph account shows "Sign in again"; signing in again asks for `Contacts.ReadWrite` and `Calendars.ReadWrite` (own client and Thunderbird's, which may not be granted them); then `If-Match` with `W/"changeKey"`, `transactionId` deduplication, the extended-property `$filter` for contact creates, clearing an address with `null`, IANA zone names in `dateTimeTimeZone`, and an edit of one occurrence
- [ ] Google CardDAV and CalDAV (OAuth): turning on `pim.edit` on a Google account signed in with its own client shows "Sign in again", which asks for `auth/calendar` instead of `auth/calendar.readonly`; then an edit, a create and a delete written through; iCloud and Fastmail CalDAV with `If-Match`
- [ ] Microsoft Graph: a write to a calendar shared read-only (or the holidays calendar) is `rejected/refused` without pausing the account's other edits, and `/me/calendars` reports `canEdit: false` for it; Graph answers a write `403` only for such calendars once the ReadWrite scopes are granted
- [ ] Radicale and Stalwart: whether a `PUT` that rewrites the item (Radicale's vobject re-serializing) still answers with a strong `ETag`; Holtmail keeps the sent bytes only for a strong one
- [ ] An event edited and then deleted at once in `write_through` (the delete recorded while the edit's `PUT` is in flight) ends with the item deleted upstream, not a conflict (unit-tested in the store conformance suite; not seen against a server)
- [ ] With `pim.edit` on and `pim.respond` off, changing only one's own answer on an invitation in an auto-scheduling CalDAV calendar (Stalwart) stays held with `respond_off`, and the organizer gets nothing; turning `pim.respond` on sends it
- [ ] A JMAP create (raw tab) whose `UID` another item of the address book already has ends `conflict/exists` on Stalwart, and the API refuses such a create when the item is already archived
- [ ] A rebased edit (upstream changed another field) sent to a CalDAV server keeps upstream's change (the post-batch rebase is unit-tested; not seen against a server)
- [ ] A restart while an edit is `sending`: it goes again after start, a create without a duplicate
- [ ] By eye (Phase 61 web part, checked by types, lint and unit tests only): the event and contact dialogs on desktop and phone widths (New event in the calendar, New contact on People, Edit card on a person, Add sender to contacts in the reader, Edit and Delete on the item page), the raw iCalendar and vCard tab, the time zone field's suggestions, the Local edit mark in its four states, the item timeline with changes and the field diff, Settings → Contacts and calendars (modes, the confirm before sending, open changes with Keep mine, Take theirs, Discard and Retry), the nav count, and Russian strings
- [ ] Saving an unchanged event or contact form against Radicale and Stalwart records `noop` (the form sends served times as they were; unit-tested in the web app and pim-edit, not seen end to end in a browser)

## Invitations and organizing (Phase 62)

Answering an invitation from mail with an iMIP `REPLY` through GreenMail, an organizer's event in a `write_through` Radicale calendar mailing `REQUEST` and `CANCEL`, and an attendee's `REPLY` landing in Radicale are proved by `tests/integration/tests/invitations.rs`; who may call the RSVP routes by `access.rs`; the routes, the copy and the organizer messages by the API's `tests/invitations.rs`.

- [x] An invitation delivered to Dovecot answered through the API: GreenMail's organizer mailbox gets `METHOD:REPLY` with the user's `PARTSTAT`, `From` the identity and `In-Reply-To` the invitation; the answer shows once the reply is archived
- [x] A Radicale `write_through` event with an invitee mails a `REQUEST` with the user's identity as `ORGANIZER`; the invitee's `REPLY` arriving by IMAP sets their `PARTSTAT` in Radicale; deleting mails a `CANCEL`
- [ ] Apple Calendar (macOS and iOS) receiving our `REQUEST`: shown as an invitation with Accept/Decline, accepting replies to the identity, a later update with a raised `SEQUENCE` replaces it and a `CANCEL` removes it
- [ ] Outlook (desktop, web and Microsoft 365) receiving our `REQUEST` and `CANCEL`; our `REPLY` to an Outlook invitation updates the organizer's tracking
- [ ] Gmail (web and Google Calendar) receiving our `REQUEST` and `REPLY`: the invitation card, the RSVP buttons, and the organizer's guest list updated by our reply
- [ ] Our `REPLY` to invitations sent by Apple Calendar and Google Calendar, also for one occurrence of a repeating event (a changed instance and one made from the series)
- [ ] An invitation received through an alias (SimpleLogin, addy.io, Apple Hide My Email, a catch-all domain) answered with the alias as attendee and the alias strategy for the mail; the organizer sees the alias, never the real mailbox
- [ ] The CalDAV RFC 6638 route: on an auto-scheduling server (Stalwart, iCloud, Fastmail) answering a copy through the API sends the server's own reply (`respond` with `notify`), Holtmail mails nothing, and an iMIP answer followed by a later write of the copy (`Schedule-Reply: F`) makes the server tell nobody again; only the organizer's side on Stalwart is in the integration stack (`invitations`), the attendee's `respond` with `notify` and `Schedule-Reply: F` are not
- [ ] Graph `accept`/`tentativelyAccept`/`decline` with `sendResponse` through the API, and organizer edits on Graph sending their own invitations (Holtmail mailing nobody, and only `SCHEDULE-AGENT=CLIENT` attendees while the calendar is `held`); on Fastmail's JMAP calendars an organizer's event reaches attendees once, by Holtmail's mail, with no second copy from the server
- [ ] An organizer's event in a calendar whose server schedules but with attendees marked `SCHEDULE-AGENT=CLIENT` (raw tab): only they get Holtmail's mail
- [ ] Undoing an answer inside the undo window (the web's _Undo_: the outbox message cancelled and the recorded change on the copy discarded) leaves the organizer untold, "From mail" without the answer and the copy as it was
- [ ] A `REPLY` that arrives while the calendar's server is down is applied once it is back (Stop Radicale, deliver a reply, start Radicale, wait a few minutes: the attendee's `PARTSTAT` lands); the queue itself is proved by the store's conformance suite
- [ ] Killing the server between recording an answer and releasing its mail (hard to time; a debugger or a sleep patched in): after the restart the reply is queued in the undo window
- [ ] By eye: invitation emails Holtmail sends (subject, readable part, the calendar part as the last alternative) in Apple Mail, Outlook, Gmail and Thunderbird

## Devices (Phase 65)

- [ ] Settings → Devices by eye, in English and Russian, light and dark: the form with every protocol ticked, the app password dialog, the `Confirm it's you` step after five minutes, the notice line in the mail and settings layouts; and the shown server, port and user name accepted by a real mail client
- [ ] A certificate renewed by certbot or Caddy while the server runs is served by the next new connection, with open connections keeping the old one (the reload itself is covered by `holtmail-serve-net` tests; the first listener lands in Phase 66)
- [ ] `trusted_proxies` behind a real proxy: the sign-in limit counts clients, not the proxy (a proxy that appends `X-Forwarded-For`, covered for the header itself by the `devices` suite)
- [ ] PROXY v2 from a real HAProxy (`send-proxy-v2-ssl`) and Caddy L4 in front of 465 and 587: the sign-in limit and the connection counts see the client's address, a connection that ended TLS at the proxy counts as TLS (the SSL TLV reading in `holtmail-proxy-protocol`, proved only against the crate's own encoder), a connection that did not is refused sign-in
- [ ] Clients behind one IPv6 /64 share one sign-in limit, as the `Limiter` unit tests say, on a real IPv6 network

## SMTP submission (Phase 66)

Sending through Holtmail with `submit-smtp` and curl, the refusals, retries and the loop guard are proved against GreenMail by `tests/integration/tests/serve_submit.rs`. By hand:

- [ ] Apple Mail, iOS Mail, Thunderbird, Outlook and K-9 send through 465 and 587 with an app password
- [ ] A client that does not repeat `EHLO` after `AUTH` sends fine within its transport's limits, and gets a readable refusal over them
- [ ] A certificate renewed by certbot is picked up by a running listener
- [ ] A delivery report shows up in a mail client's inbox once Phase 67 places it there
- [ ] A client whose open session ends with `421` after its device lost a right reconnects without showing the person an error

## The web app, by eye

None of these screens has been opened in a browser; they were checked with types, lint, tests and builds only.

- [ ] Invitations (Phase 62): the answer buttons on an invitation card and the event page with each route line (email, alias service, server's calendar, no answers wanted), _Whole series_ and _One occurrence_ with the occurrence preselected from the calendar, the countdown with _Undo_ and the link to the outbox, the held notice and the Local edit mark next to the buttons, nothing shown to a member without the right or on an event the user organizes; in the event form the invitations note and _Send invitations_ switch for each route; the count of answers on an event the user organizes; the answer buttons disabled during the countdown, the _open edit_ notice, the note and switch on the raw tab and after removing the last invitee, _Save_ waiting for the check, the unreachable-server note, and the delete dialog naming who is told with its switch; a screen reader announcing the answer once rather than every second

- [ ] First run: create the owner, create an archive, connect mail (IMAP, Gmail and Outlook with OAuth, JMAP, Graph), upload a file
- [ ] Reading: message list, labels in the sidebar and in rows, the reader with attachments and thumbnails, the remote image banner and modes
- [ ] Search: operators (`label:`, `attachment:`), the "In attachment" chip, ⌘K
- [ ] Settings: archives (save images, older mail, export), sources (edit connection, sign in again, check for new mail, what to archive), people and access, reading, tasks (including the interrupted state), network
- [ ] Light and dark themes, narrow windows, keyboard navigation
- [ ] Settings → Integrity, Webhooks (the secret shown once, deliveries refreshing) and SMTP; the deletion dialog from search results and the reader (review, sample, typed count, back to the list); Download PDF in the reader and Zip of PDFs in the export menu
- [ ] Insights card: a fresh code grouped and copied without spaces, an old code collapsed with Show and still copyable, arrow keys and `Enter` on its rows, `c` and `Shift+U` in a thread, the unsubscribe link opening without a referrer and its reason tooltip, the key glyph in list rows
- [ ] Insights card, Phase 36: the action row (label, arrow, mono host, "via", outline and warning badges, a look-alike host shown as its A-label), the parcel row (number copied, Track link), `Enter` on both rows, `c` copying a tracking number when there is no code, `Shift+L` opening the action link else the tracking link, without a referrer, in both themes and at narrow widths
- [ ] Insights card, Phase 37: reservation, pass, order and invoice rows (wrapping of the " · " parts, the Ref value with its copy button, "Open → host" without a referrer, the "Forwarded" badge), times in their own offset with the right zone abbreviation, `c` copying the booking reference when there is no code (the right copy button marked), an event reservation hidden when the invitation card shows the same event, in both themes and at narrow widths
- [ ] Categories, Phase 38, in both themes and at narrow widths: the card footer (kind and topic chips, the category menu button, the trust note in muted or warning colour, the unsubscribe link and its reason tooltip wrapping); the chip menu (radio kinds and checkbox topics keeping the menu open, address or domain scope, the held-back note, Save updating the open message at once, Reset); `x` opening the menu with focus inside; the list glyphs for bulk and notification and the topic chip fitting the row height; sidebar Categories with counts and links; the palette "Show …" entries; Settings → Categories (archive picker, overrides, Delete); the account editor's "Trusted authentication servers…" dialog and the suggestion link on the account row
- [ ] Unsubscribe dialog, Phase 41, in both themes and at narrow widths: the button opening the consent dialog with the domain (long IDN and A-label domains wrapping), the confirm button disabled while the request runs, each outcome's colour and the "Open their unsubscribe page" link, the outcome staying visible while the refetch turns the button into a link, the "Unsubscribe requested on …" line next to the `recently_requested` tooltip, `Shift+U` opening the dialog with one-click and the link without it, and focus returning after the dialog closes
- [ ] Search notice, Phase 40, in both themes and at narrow widths: "Some messages are still being read (N left)" under the search field in search mode and under Categories in the sidebar during a backfill and after an override, counting down every 5 s and gone at 0; the deletion dialog showing the 409 message for a `category:` review while the notice shows, and the 400 message for one with insights off
- [ ] Writing and compose, Phases 55 and 57, in both themes and at narrow widths: the Writing dialog of an account (consequences wrapping, Erase for good locked until Delete, the note when `[write]` is off); Access in People with one role per archive, locked for admins on writing grants; Settings → Sending (suggestions with Set up, the identity form with the signature editor, an activation refused by the server, the transport form, Check showing the server's words, shared transports and their archive checkboxes); the compose pane over the reader, full size and full screen on a phone (recipient chips from typing and pasting, People suggestions with the arrow keys, the From line and its reasons, the identity menu, a route error disabling Send, rich text and plain text both ways, the quoted original in its frame without remote images, signature insertion, uploads one after another, Send later, autosave and the Saved line, Esc and ⌘/Ctrl+Enter); the undo toast counting down and Undo reopening the draft; the reader's Reply, Reply all, Forward and the menu (Reply to list only for list mail), `r`, `a`, `f`, `n` and the palette entries; no compose button, reply actions or outbox for a reader; the outbox groups, the Unclear explanation and the confirmations; review fixes of 2026-11-07: Attach from the archive (search, pick a message, pick a file), Escape closing only the recipient suggestions, a second Reply while a draft is open saving the first, switching archives closing the draft, a reopened draft showing its newest text, Retry and Send again hidden for a manager without the compose right, Send again asking first when someone already accepted, the identities and transports error lines in Settings → Sending, the browser's leave-page prompt with unsaved changes, compose on plain HTTP from another machine

## Log

- 2026-11-06: email aliases added (Phases 32–34); SimpleLogin proved against a self-hosted instance (TESTPLAN step 9).
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
- 2026-11-06: message insights added (Phase 35).
- 2026-11-06: message insights Phase 36 (action links and parcels) added.
- 2026-11-06: message insights Phase 37 (reservations, orders and passes) added.
- 2026-11-07: message insights Phase 38 (trust and categories) added.
- 2026-11-07: message insights Phase 39 (domain data) added.
- 2026-11-07: message insights Phase 40 (search) added.
- 2026-11-07: message insights Phase 41 (one-click unsubscribe) added.
- 2026-11-07: message insights Phase 42 (data file updates) added.
- 2026-11-07: sending (Phases 55–56) added.
- 2026-11-07: writing and compose in the web app (Phases 55 and 57) added.
- 2026-11-07: Phase 58 (JMAP and Graph transports, Microsoft consent, sent-copy links, bounces) added; JMAP submission and a bounce proved against Stalwart by send_jmap.rs.
- 2026-11-07: Phases 58 and 59 in the web app (transport form for every kind, sent-copy explanations, reply strategies by service and domain, alias paths and leak fixes in compose) added.
- 2026-11-07: Phase 59 review fixes (Apple `reverse_alias`, made reverse aliases in People, the server's leaks in compose) added.
- 2026-11-08: Phase 60 (mailbox changes: writers, writeback, confirmation by sync, the API, restore, retention, sent copies) added; Dovecot and Stalwart JMAP proved by mailbox_write.rs.
- 2026-11-08: Phase 60 web part built (reader actions, held mark, retry and dismiss, overlay, mark as read when opened, retention); checked by types, lint and unit tests only, the by-eye item stays open.
- 2026-11-08: Phase 61 without its web part (post-batch step, write-through wiring, Graph consent, the edit API) added; Radicale and Stalwart CardDAV/CalDAV proved by writeback.rs.
- 2026-11-08: Phase 61 web part built (forms, raw editing, contacts from senders, Local edit marks, timeline with changes, Settings → Contacts and calendars, `CollectionDto.editable`, `GET /api/pim/changes/{id}/diff`); checked by types, lint, unit tests and the access suite, the by-eye item stays open.
- 2026-11-07: Phase 61 review fixes added the Google calendar write scope, Graph read-only calendars, weak DAV tags, edit-then-delete in flight, replies without `pim.respond` and JMAP `UID` clashes; checked by unit tests and the `writeback` and `access` suites.
- 2026-11-07: Phase 60 review fixes: the follow-a-move case proved on Dovecot by mailbox_write.rs; Graph write consent, servers without `MOVE`, the account pause and the new web states added as open items.
- 2026-11-08: invitations and organizing added (Phase 62); the iMIP reply, the Radicale organizer path and an attendee's reply landing upstream proved by tests/integration/tests/invitations.rs.
- 2026-11-07: Phase 62 review fixes: a forged reply ignored, a JMAP calendar mailing once and an RFC 6638 attendee on Stalwart proved by tests/integration/tests/invitations.rs; a reply retried after an outage, recovery after a crash and the new web states added as open items.
- 2026-11-07: Phases 65 and 66 review fixes: SMTP sign-in over the real binary (web password refused, the limit, revoking and lowering ending an open session) proved by tests/integration/tests/devices.rs; PROXY v2 from a real proxy and clients reconnecting after `421` added as open items.
- 2026-11-08: SMTP submission (Phase 66) added; `submit-smtp` and curl proved it into GreenMail by tests/integration/tests/serve_submit.rs. Open by hand: Apple Mail, iOS Mail, Thunderbird, Outlook and K-9 sending through port 465 and 587 with an app password (a client that does not repeat `EHLO` after `AUTH` sees 50 MiB and no `SMTPUTF8` until the limits refuse at `MAIL`); a real certificate from certbot picked up by a running listener; the delivery report, once Phase 67 places it in `INBOX`, showing up in a mail client.
