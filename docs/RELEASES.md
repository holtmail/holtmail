# Release notes

Changes people notice when they upgrade: new behavior, changed meaning and one-time work after the upgrade.

## Unreleased

### Writing and sending

- Holtmail can send mail: compose, reply, reply all, reply to list, forward inline or as an attachment, and edit as new, through identities and transports set up in Settings → Sending. Every message waits out an undo window (`[write] undo_seconds`, 10 s by default), is archived before it leaves, and is listed in the archive's Outbox with what became of every recipient.
- New config section `[write]` with `enabled`, `send` and `rules` (all `true` by default). Nothing is written to a mail server unless the owner also turns the capability on for the account; every account starts with all capabilities off.
- Archive grants gain roles: `reader` (every existing grant), `editor` and `sender`. Only the instance owner gives or takes away `editor` and `sender`; admins write and send only with such a grant, and may no longer change the email or password of someone who holds one.
- Holtmail can change mail on the servers it archives from, account by account and only with the matching switch in the account's Writing dialog: mark read or unread, star, move, move to the trash, erase for good, and restore an archived message into a mailbox. Changes show at once with a "not yet on the server" mark, go out in order, and are confirmed by the next sync; when another app changed the same flag meanwhile, the server wins. Failed changes say why and offer Try again and Dismiss to those who may. _Mark as read when opened_ (Settings → Reading) is off by default.
- An account can delete its old mail from the server after a number of days (to the server's trash, or for good with _Erase for good_), with a count of what would go before saving. The archive keeps every message.
- An identity with the sent-copy policy `append:<folder>` now saves each sent message into that folder.
- Invitations can be answered from the invitation card and the event page, by email through the outbox with an undo window, or through the account's calendar server where it schedules. An invitation that came through an alias is answered as the alias, or refused if that would show the real mailbox.
- Events you organize mail their invitations, updates and cancellations through the outbox unless the calendar's server sends them itself (Microsoft 365 and CalDAV servers that schedule). JMAP calendars are mailed by Holtmail. If the server cannot be reached, saving with invitations is refused until it can, or until you save without them. The event form and the delete dialog say who is told and let you leave them out.
- A reply from an attendee updates an event you organize only when the mail comes from that attendee, and is retried for a week if the calendar's server is away.
- Contacts and events can be created, edited and deleted from the web app (forms or raw iCalendar and vCard) in address books and calendars whose mode allows it (Settings → Contacts and calendars). By default edits stay in Holtmail; with _Send edits to the account_ they go to the server, and edits that clash with the server's own changes wait for a decision (Keep mine, Take theirs). Google accounts on their own OAuth client and Microsoft Graph accounts ask to sign in again once _Contacts and calendars_ is on under Writing, so the provider grants write access. Changing only your own answer to an invitation goes to the server only with _Answer invitations_ on.
- Microsoft Graph accounts need one more sign-in to change mail on the server: once a mailbox switch is on, the account asks to sign in again so Microsoft grants `Mail.ReadWrite`.
- Every write is recorded in an audit log the owner reads at `GET /api/audit` (ids and counts only).
- Mutating requests accept an `Idempotency-Key` header; repeating a request with the same key returns the first answer instead of running it twice.
- Replies to mail that came through an alias service (SimpleLogin, addy.io, DuckDuckGo, Apple Hide My Email, Firefox Relay, Fastmail Masked Email, own domains) leave so that the other side sees the alias, never the real mailbox; a draft whose recipients would see the real address is held with one-click fixes until you fix it or explicitly send anyway. How replies leave is set per alias, domain or service on Settings → Aliases.
- Replying as the alias itself (`direct`, the default for own domains, catch-all rules and Fastmail Masked Email) needs an identity whose address is the alias or that is linked to it; a transport allowed to send as the alias is no longer enough.
- A message queued by someone who later loses the right to send (their grant, the identity, the transport's allowed addresses) fails at its send time instead of leaving.
- Rules (Settings → Rules, `holtmail rules`): flows that label, flag, move, delete, reply, forward, send, answer invitations, create events and contacts, notify or call a webhook when new mail arrives, an invitation comes in, on a schedule, before an event, by hand on a selection or a search, or while you send. New flows start in test mode and only log what they would do. Automatic replies follow RFC 3834 and go only to the sender, at most once per sender in a number of days; forwards of automatic mail are skipped; flows are rate limited and paused by a circuit breaker. A flow acts with the rights of whoever saved it, and sending-time flows apply only to that person's own mail. A webhook can be named by a rule only once it subscribes to the new `flow.run` event.

### Mail clients

- Settings → Devices makes an app password per mail client, with its own protocols, archives and rights; it is shown once and can be revoked at any time, which ends the client's open connections. Taking a right away from a device also ends its open connections, and the client signs in again under the new rights. Making a device or giving it more rights asks for a recent sign-in, and a notice appears for seven days.
- Mail clients can send through Holtmail over SMTP submission with an app password: the new config section `[serve.submit]` opens ports such as 465 and 587, off until configured. Messages go through the same identities, limits and outbox as the web app, and a failure after the client was told `250` shows on the outbox row and as a delivery report in the archive.
- Calendar apps that schedule over CalDAV (Apple Calendar, iOS, Thunderbird) can organize and answer meetings through Holtmail (`[serve.dav] scheduling`, on by default): Holtmail mails invitations, updates, cancellations and answers itself where the calendar's server does not, and a mail client's own copy of the same message within ten minutes is dropped. Invitation mail now waits until its change is saved, so nothing leaves for a change that failed, even with `undo_seconds = 0`.
- An attendee's answer by mail now updates an event you organize only when a receiving server you trust for that account stamped a DKIM or DMARC pass for the attendee's domain (the account's trusted authserv-ids, as for unsubscribe links); otherwise the invitation card offers _Apply anyway_ to editors. Accounts without a trusted authserv-id need one set before answers apply on their own.
- Settings → Devices offers a one-time link to a profile for iPhone, iPad and Mac that sets up mail, calendars and contacts; opening the link shows a page whose button downloads the profile, and only that gives the device a new app password, so the old one stops working. Link previews in chat and mail scanners no longer use the link up. On Android the page opens DAVx5 with the server filled in. `[serve] mail_domains` lists the mail domains you control: Thunderbird autoconfig answers for those only, and `holtmail dns` prints their SRV records. See [DEPLOY.md](DEPLOY.md).
- New config section `[serve]` with `tls_cert`, `tls_key`, `proxy_protocol`, `trusted_proxies` and `members`. Behind a proxy listed in `trusted_proxies`, the web sign-in limit now counts the client's address from `X-Forwarded-For` instead of the proxy's.
- Settings → Devices gains _In mail apps_: how far Holtmail has prepared each archive for mail apps (it starts in the background once a device with IMAP or JMAP exists), and the read and starred marks kept only in Holtmail for accounts that may not be changed upstream, with Clear and Clear all. `[serve] local_flags = false` turns those Holtmail-only marks off.
- Holtmail now keeps a log of what changed for mail apps and prunes it to 90 days every six hours; a delivery report for mail a client sent that later failed shows in that user's inbox in mail apps.
- Mail apps can read and change mail over IMAP (`[serve.imap]`, ports such as 993 and 143) and JMAP (`[serve.jmap]`, on the web listener, needs an https `public_url`), and calendars and contacts over CalDAV and CardDAV (`[serve.dav]`, on the web listener at `/dav/`). Each needs a device with that protocol. Drafts started in the web app do not show in mail apps' Drafts yet.
- JMAP uploads need a device that may write or send mail, are limited to 200 MiB per user at a time, are kept in `<data_dir>/jmap-uploads` for a day and are gone after a restart.
- A calendar moved from _Keep edits in Holtmail_ to read-only still takes edits from calendar apps for 24 hours; this grace starts with the first mode change after the upgrade.

### New search operators

- `has:code`, `has:link`, `has:unsubscribe`, `has:reservation`, `has:order`, `has:invoice`, `has:parcel` and `has:pass` find messages with that kind of [insight](INSIGHTS.md) at `Likely` or higher, outside forwarded parts.
- `category:<topic>` (`purchases`, `travel`, `finance`, `health`, `social`, `jobs`, `events`, `government`) and `category:<kind>` (`personal`, `list`, `bulk`, `notification`) find messages by their effective category. Any other `category:` value is an error that lists the allowed ones.
- `is:newsletter` (kind `bulk`), `is:notification`, `is:personal` and `is:list` find messages of that kind.

Values are never searchable: `has:code` finds messages that contain a code, not the code itself.

### Changed meaning

`has:code`, `is:newsletter`, `category:travel` and the other values above used to be searched as plain text (the words `has` and `code`, anywhere). They now select by insights and categories, so the same query finds other messages. A deletion review typed with one of them shows the new selection in its count and sample before anything is deleted, and is refused with 409 "Some messages are still being read" while the archive's insights or search index are still catching up. With insights turned off, such a review is refused with 400. Unknown `has:` and `is:` values are still searched as plain text.

### One-click unsubscribe

- When a newsletter's sender signed its `List-Unsubscribe` and `List-Unsubscribe-Post` fields with an aligned DKIM signature that a trusted server checked, the insights card offers Unsubscribe. After you confirm, the server sends one RFC 8058 POST to the sender's https link, directly and never through `image_proxy`, so the sender sees the server's public address. Owners and admins only; members still see no unsubscribe links.
- A request accepted or redirected in the last 24 hours is not sent again for the same list; after a failure, a new try is allowed after a minute. Every request is recorded, and the card shows "Unsubscribe requested on …".
- New config key `[insights] one_click` (default `true`). Set it to `false` to turn one-click off: no request is ever sent and the card offers only the sender's link.

### Category data updates

- New config key `[insights.categories] update` (default `false`). Set it to `true` to check weekly for a signed release of the domain category data on the project's GitHub release `categories-latest`. The server connects directly, never through `image_proxy`, reaches only the GitHub release hosts, and loads a release only if its ed25519 signature, key, expiry and file hashes check out and its version is newer. It needs `domains` in `[insights.categories] sources`.
- New config key `[insights.categories] data_sources` (default `["wikidata"]`) names the files to download. Add `"ut1"` to also use the UT1-derived file (shopping, jobs and social sites, CC BY-SA 4.0); it is never bundled. The bundled Wikidata table always loads.
- Accepted releases are stored in `{data_dir}/categories/` and loaded at start; with `update = false`, deleting the folder goes back to the bundled table. A downloaded table is used only while it is newer than the one bundled with the running version.
- A newly loaded release re-derives categories in the background without reading any message.
- New route `GET /api/insights/data` and a new Settings → About page for every signed-in user: the Holtmail version, the loaded data files with versions, licenses and attribution, and the update state.
- No release signing key ships yet, so with `update = true` every release is refused (`no_release_key` in the log) until one is compiled in; nothing else changes.

### After the upgrade

The search index gains two fields, so the first start after the upgrade deletes it and rebuilds it from the archive once. Search results are incomplete until the rebuild finishes; the rebuild also fills any missing insights.

Migration `0023_unsubscribe.sql` adds the empty `unsubscribe_requests` table on the first start; nothing is backfilled. Deleting a message keeps its recorded requests with the message link cleared.

Category data updates add no migration; four settings rows (`categories_data_*`) appear after the first check, and the `categories` folder only after the first accepted release.
