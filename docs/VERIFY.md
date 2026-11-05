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

## Upgrade and deployment

- [ ] The new image on an existing data volume: migrations 0006–0009 apply, the search index is rebuilt in the background at start, search works again when it finishes
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
