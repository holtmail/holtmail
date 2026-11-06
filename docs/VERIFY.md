# Still to verify

Everything that tests, builds and runs of the real binary against local fakes could not prove. Each item needs a real service, real data, a browser or a person. Tick an item when it has been checked, and note what was found in the Log at the end. Add new items here whenever a change leaves something unproven.

## Sign-in with Google and Microsoft (OAuth)

- [ ] Google accepts Thunderbird's client with the redirect `http://127.0.0.1` (no port) when the request comes from Holtmail; the code exchange and refresh work
- [ ] Microsoft accepts Thunderbird's client with the redirect `http://127.0.0.1`. Check this against the live registration: Thunderbird bug 2036505 shows Microsoft moving between `http://localhost` and `https://localhost`
- [ ] Consent screens: what the user sees, and whether Google limits a borrowed client (unverified app warning, user cap)
- [ ] `AUTHENTICATE XOAUTH2` against real Gmail and Outlook IMAP, including the error continuation for an expired token
- [ ] A revoked token (access removed in the Google or Microsoft account) marks the account "sign in again", and Sign in again restores it
- [ ] A refresh token rotated by the provider is stored and keeps working after a restart
- [ ] An own client in `[oauth.google]` and `[oauth.microsoft]` with `redirect_url` pointing at `/api/oauth/callback`: no paste step, the browser comes back to the page where sign-in started
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
- [ ] Stalwart and Cyrus: session shape, `maxObjectsInGet`, `maxCallsInRequest`, `Email/query` limits and `total`, download content types
- [ ] A large mailbox: every sync lists all ids (`Email/changes` is not used); measure the time
- [ ] Mailbox names containing `/`

## IMAP, Gmail labels and flags

- [ ] A real plain IMAP host (password): first sync, incremental sync, changing the connection from the web app and from `holtmail account edit`
- [ ] Gmail: capabilities, `X-GM-LABELS` quoting of real system labels and non-ASCII labels, labels sent as literals (would fail the fetch today)
- [ ] Gmail: an account that synced label folders before Phase 8 moves to All Mail without storing anything twice
- [ ] `ENABLE CONDSTORE` and `CHANGEDSINCE` on Gmail All Mail and on another server with CONDSTORE: read and flagged changes reach the archive
- [ ] The daily flag scan on a large mailbox without CONDSTORE: time and load
- [ ] "Archive only mail older than N days" on a real account: young mail waits and arrives later

## Remote images

- [ ] Fetching images from the real internet: directly, through an HTTP proxy, through `socks5h`
- [ ] Saving images of older mail on a large archive: speed, failures, retry of failed images
- [ ] Remote image work resumes after a real interruption (server stopped mid-task)

## Attachments

- [ ] Text extraction from real PDFs and office files made by other programs (Word, LibreOffice, Google Docs exports, scanners)
- [ ] Extraction speed and memory on a large archive; the start-up pass over old mail
- [ ] Thumbnails of real photos (EXIF orientation, large images, animated GIF)

## Export, backup and restore

- [ ] Export and backup of a large archive: memory stays flat, time
- [ ] Zip exports over 4 GiB (zip64 in stream mode) open in common unzip tools
- [ ] Backup and restore with blobs on S3
- [ ] Restore of a backup made by an older Holtmail version
- [ ] Exported mbox files open in Thunderbird and Apple Mail
- [ ] Contacts `.vcf` and calendar `.ics` exports open in Apple Contacts and Calendar, Google Contacts and Calendar, Outlook and Thunderbird; the `.ics` as one `VCALENDAR` mixing CRLF and LF lines (native components keep their bytes), zones written before first use and shared by `TZID`, events from mail written from the merged view (cancelled ones as `STATUS:CANCELLED`), and vCard 2.1/3.0/4.0 mixed in one `.vcf`
- [ ] Graph and JMAP items written as text: Graph events in Windows zones without `VTIMEZONE` (written as IANA names when known), JSCalendar overrides with inherited fields, Graph contacts' addresses and birthdays, in the programs above
- [ ] A contacts or calendar zip of a large archive (every version of years of syncs): time, memory and the manifest's size

## Contacts and calendars

- [ ] Real exports read well: Google Contacts and Calendar Takeout, Apple Contacts and Calendar, Outlook, Thunderbird, Nextcloud and Fastmail `.vcf`/`.ics` files (names, phones, addresses, photos, time zones, all-day events, recurring events with moved instances, tasks); count unreadable items
- [ ] vCard 2.1 from old phones and Outlook with other charsets than UTF-8 (Windows-1251, ISO-8859-1) and quoted-printable soft line breaks
- [ ] Microsoft and custom `VTIMEZONE` names (`W. Europe Standard Time`, `X-LIC-LOCATION`) give the right UTC times; the same event exported by two programs gives the same `starts_at`
- [ ] Re-importing a newer export of the same address book or calendar under the same file name: only changed items get versions, items left out are marked removed, and the diffs read sensibly (no noise from fields programs rewrite on every export besides `DTSTAMP`)
- [ ] Contacts without `UID` (some phone exports): renaming one shows as a removed and a new contact instead of a version
- [ ] A large Takeout (thousands of contacts with photos, years of calendar): import time and memory (each `.vcf`/`.ics` file is read whole)
- [ ] Phone numbers written without a country code get no E.164 form; check how many real address books that leaves unmatched
- [ ] Uploading `.vcf` and `.ics` files and a Takeout zip from the web app (the file picker offers them; the task summary counts contacts and events)
- [ ] Invitations from real senders become events: Google Calendar, Outlook/Exchange (including `W. Europe Standard Time` zones, and messages where Exchange wraps the invitation in `winmail.dat`, which is not read), Apple Calendar, Thunderbird, Zoom and Teams links; updates with a raised `SEQUENCE`, replies (accepted, declined, tentative, delegated), cancellations of the event and of one instance of a series
- [ ] Senders that do not raise `SEQUENCE` on updates (some booking tools) or send the same `DTSTAMP`: versions still read in a sensible order (then by message date)
- [ ] Replies sent by Outlook and Google carry the attendee's `PARTSTAT` as the "your answer" and the merged attendee list expects; organizer copies of replies from several attendees
- [ ] "Your answer" on a synced IMAP/JMAP/Graph account with an address (imports have none, so it stays empty there); people who answer from an alias address
- [ ] The start-up pass on a large archive with years of invitations: time and memory, and that it runs once
- [ ] The invitation card and the item page by eye: times in the viewer's and the organizer's zones, cancelled state, history with diffs and links back to the messages

### People

- [ ] Grouping on real address books: how many cards join through a shared email or phone, whether any join wrongly (shared family or office numbers, a reused work address), and how many stay apart because phones lack a country code
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
- [ ] Copies across calendars: the same meeting in the organizer's and an attendee's calendar and in mail folds to one with the right leading copy; whether some servers rewrite `UID`s (Exchange `X-MICROSOFT-CDO-OWNERAPPTID`, Google event ids per calendar) so copies do not fold; the differences table on real copies (fields servers add or drop, attendees' answers)
- [ ] Size: a shared calendar with thousands of endless series (the stored two-year window, the 5 000 instance cap per request, on-the-fly expansion for far spans), the time of the start-up rebuild after the upgrade (migration 0014 asks for a full reindex)
- [ ] The Calendar section by eye: agenda, week and month in light and dark, overlapping events side by side, all-day and multi-day events, events crossing midnight, the current-time line, calendar toggles and colors (servers' colors with alpha, calendars without color), _Show removed events_, jump to date, keyboard (`j`/`k`/`↑`/`↓`, `o`, `t`, `a`/`w`/`m`, `←`/`→`), narrow windows
- [ ] The event page by eye: when in two zones, recurrence in words for real rules (and in other locales: the wording is English with localized day and month names), attendees with answers per copy, copies and differences, mail and the merged history; the links from the invitation card, a person's meetings and search results
- [ ] `with:` and `is:event`/`is:contact` on a real archive: whether the five items above the mail results are the useful ones (events by start, then contacts)

### CardDAV and CalDAV servers

Only an in-process fake (Rust) and a local Python fake were used. Against each real server: turn it on, first sync, edit and delete a contact and an event on the server (or another client), sync again, and check versions, removals and the task log; then wait long enough for the server to forget the sync token (or remove it from the database) and check that a sync recovers by ETags.

- [ ] Nextcloud (`https://host/remote.php/dav`, app password): discovery through the principal, `sync-collection`, shared and subscribed calendars, the birthday calendar (read-only, generated), deleted-items trash
- [ ] iCloud (`https://contacts.icloud.com`, `https://caldav.icloud.com`, app-specific password): home sets on `pNN-contacts.icloud.com` and `pNN-caldav.icloud.com` (allowed as the iCloud family), whether the partition host changes over time (it would show as a removed and a new collection), `getctag`, multiget sizes
- [ ] Fastmail (`https://carddav.fastmail.com`, `https://caldav.fastmail.com`, app password): discovery, sync tokens, colors
- [ ] Google with OAuth: whether Thunderbird's client is granted `auth/carddav` and `auth/calendar` for Holtmail, whether `calendar.readonly` works for CalDAV with an own client, the well-known redirect of `https://www.googleapis.com/.well-known/carddav`, the CalDAV principal at `https://apidata.googleusercontent.com/caldav/v2/`, Google's sync-token expiry, contact groups as items, rate limits (`429`, `Retry-After`)
- [ ] Radicale and Baïkal (`https://host/` or `https://host/dav.php`): discovery without a well-known redirect, servers that answer `sync-collection` differently (Radicale's tokens, Baïkal's SabreDAV)
- [ ] Yandex (`https://carddav.yandex.ru`, `https://caldav.yandex.ru`, app password): whether the presets are still right and `sync-collection` works
- [ ] A mail domain with a well-known redirect to a DAV host elsewhere (credentials follow only where the well-known lookup led), and one without well-known addresses (the dialog asks for the address)
- [ ] Servers that send CRLF raw versus `&#13;` inside `address-data`/`calendar-data`, servers that encode hrefs differently between answers, very large calendars (many multigets of 50), items over 16 MiB, `507` paging of `sync-collection`
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

Only an in-process fake (Rust) and a local Python fake (`scratchpad/e2e18/fakejmap.py`) were used.

- [ ] Fastmail with an API token that has the contacts and calendars scopes: whether the session advertises `urn:ietf:params:jmap:contacts` (RFC 9610) rather than only its older vendor capability, and `urn:ietf:params:jmap:calendars`; first sync, a change and a deletion in Fastmail's web app, sync again
- [ ] Stalwart: contacts and calendars through JMAP (`calcard` comes from the same authors), tasks in calendars
- [ ] Whether real JSContact cards read well through `calcard` (names with components, `phones` features, photos as `blobId` media, which are not fetched) and JSCalendar events from both RFC 8984 servers (`recurrenceRules`, `sendTo`, `replyTo`) and JSCalendar 2.0 servers (`recurrenceRule`, `calendarAddress`)
- [ ] `cannotCalculateChanges` after the server forgot old states, `maxObjectsInGet` and query limits on large address books, `hasMoreChanges` paging, and whether `/get` answers serialize an unchanged object the same way every time (else extra versions appear on recovery)
- [ ] A session at `/.well-known/jmap` that redirects to another host (credentials follow only to where it led), and an `apiUrl` on another host (refused)
- [ ] The dialog for JMAP accounts in a browser (empty address reads through JMAP)

## Upgrade and deployment

- [ ] The new image on an existing data volume: migrations 0006–0015 apply, the search index is rebuilt in the background at start (with contacts and events and the calendar's instances), search works again when it finishes
- [ ] Docker: `holtmail backup`, `restore` and `account edit` through `docker exec`
- [ ] The setup link, sign-in and writes behind a reverse proxy with TLS

## The web app, by eye

None of these screens has been opened in a browser; they were checked with types, lint, tests and builds only.

- [ ] First run: create the owner, create an archive, connect mail (IMAP, Gmail and Outlook with OAuth, JMAP, Graph), upload a file
- [ ] Reading: message list, labels in the sidebar and in rows, the reader with attachments and thumbnails, the remote image banner and modes
- [ ] Search: operators (`label:`, `attachment:`), the "In attachment" chip, ⌘K
- [ ] Settings: archives (save images, older mail, export), sources (edit connection, sign in again, check for new mail, what to archive), people and access, reading, tasks (including the interrupted state), network
- [ ] Light and dark themes, narrow windows, keyboard navigation

## Log

- 2026-11-05: list written from the "Not verified" notes of Phases 4–11, the JMAP and Graph sources and the review fixes.
- 2026-11-05: CardDAV and CalDAV servers added (Phase 15).
- 2026-11-05: People added (Phase 16).
- 2026-11-05: Calendar added (Phase 17).
- 2026-11-05: Microsoft Graph contacts and calendars added (Phase 18).
- 2026-11-05: JMAP contacts and calendars added (Phase 19).
- 2026-11-05: People by hand and contacts and calendar exports added (Phase 20).
- 2026-11-05: review fixes: one-`VCALENDAR` `.ics` export.
