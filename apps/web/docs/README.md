# @holtmail/web

The Holtmail web app: a calm, keyboard-first mail client for an archive, and the place where the owner sets everything up: archives, mail sources, uploads and people. Vite, React 19, TypeScript, Tailwind CSS v4, TanStack Router, Query and Virtual, Motion. It talks to the server only through [`@holtmail/api-client`](../../../packages/api-client/docs/README.md), takes components from [`@holtmail/ui`](../../../packages/ui/docs/README.md) and every color, size and duration from [`@holtmail/tokens`](../../../packages/tokens/docs/README.md).

## Commands

From the repository root:

- `bun run dev:web` — dev server; `/api` is proxied to `HOLTMAIL_SERVER` (default `http://127.0.0.1:8080`)
- `bun run build` — production build into `apps/web/dist`
- `bun run typecheck`, `bun run lint`, `bun run test`

## Routes

Code-based TanStack Router routes in `src/router.tsx`. Every mail route accepts `?m=<messageId>`, the open message, so a URL is a shareable view and back and forward move between messages.

| Path                               | Shows                                                                                                                                    |
| ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `/`                                | Redirects to the first readable archive; with none, owners and admins go to `/welcome`, members see that nothing is shared with them yet |
| `/welcome`                         | First run: name the archive, then connect a mailbox or upload files, then watch the first sync                                           |
| `/settings`                        | Goes to Archives (owners, admins) or Profile (members)                                                                                   |
| `/settings/archives?archive=&add=` | Archives with their mail sources; `add=connect` or `add=import` opens that dialog for `archive`                                          |
| `/settings/people`                 | People, roles and the archives members may read                                                                                          |
| `/settings/tasks`                  | Syncs, imports and image downloads with progress, logs and Stop (owners, admins)                                                         |
| `/settings/network`                | The outbound proxy for remote images (owners, admins)                                                                                    |
| `/settings/reading`                | How remote images are shown in this browser                                                                                              |
| `/settings/profile`                | Your name, email and password                                                                                                            |
| `/a/$archiveId`                    | All mail of the archive                                                                                                                  |
| `/a/$archiveId/f/$folderId`        | One folder                                                                                                                               |
| `/a/$archiveId/label?name=`        | Messages with one label (a search param, since label names contain `/`)                                                                  |
| `/a/$archiveId/search?q=`          | Search results for `q` in the [query language](../../../crates/query/docs/README.md)                                                     |
| `/login?next=`                     | Sign-in; returns to `next` (a path on this site) afterwards                                                                              |
| `/setup?token=`                    | Creates the owner with the one-time token from the setup link                                                                            |

Opening a message by click or `Enter` pushes a history entry; moving through the list with `j`/`k` replaces it.

## Sign-in

The mail routes check `GET /api/session` before they load (cached for the visit). On `401` the app asks `GET /api/setup`: while the instance has no users it goes to `/setup`, otherwise to `/login?next=<the requested path>`. `/setup` without a token explains that the setup link is in the server log; `/setup` once users exist goes to `/login`, and `/login` while signed in goes straight to `next`. A `401` from any data query later (an expired session) clears the cached session and goes to `/login?next=<current path>`; queries are never retried on a `4xx`.

Both screens are one narrow centered column: the wordmark with a hairline that draws itself, then title, lead and form rising in one after another with the expressive curve (fades only under reduced motion). Fields are Carbon-style (label above, filled field with a bottom rule), focus starts in the first field, `Enter` submits, the password has a visibility toggle, and the autocomplete hints are `username`, `current-password` and `new-password`. Setup shows a live hint until the password has 10 characters. Errors from the API show under the form and move focus back to the field to fix; a `429` reads "Too many attempts, try again in N minutes" from `Retry-After`. `next` is accepted only as a same-site path that is not `/login` or `/setup`.

The user menu at the right of the top bar shows the name and email, the theme (light, dark, system) and Sign out, which posts `/api/logout`, drops every cached query and goes to `/login`.

## Setting up

Nothing needs the command line. After the owner account is created, `/` finds no archive and opens `/welcome`: an archive name (prefilled "Personal") and two large choices, Connect a mailbox and Import files, with a hairline that draws under the hovered one. Each step slides in from the right.

- **Connect a mailbox** asks only for the address and password. The domain picks a preset (Gmail, Outlook, iCloud, Yahoo, Fastmail, Yandex, Mail.ru, GMX, Proton Mail Bridge; `src/lib/providers.ts`), or `imap.<domain>` otherwise, and the password field explains where to get an app password, with a link. Server settings (host, port, TLS / STARTTLS / none, user name, sidebar name) fold open on demand and open by themselves when signing in fails for an unknown provider. The server signs in before saving, so a wrong password shows its reason under the form.
- **JMAP**: _Connect over JMAP instead (Fastmail and others)_ under the IMAP form switches to the JMAP form, and _Connect over IMAP instead_ back (`src/manage/connect-mailbox.tsx`). The JMAP form (`connect-jmap.tsx`, request built by `src/lib/jmap.ts`) starts with a provider choice: _Fastmail_ asks only for an API token (sent to `https://api.fastmail.com/jmap/session` as a bearer token, with a hint where Fastmail creates one) and an optional name (`Fastmail` when empty); _Other JMAP server_ asks for the JMAP address (session address or the server's address), _Password_ or _API token_, the user name for a password, the secret and a name. Empty fields are named under the form before anything is sent; the server signs in before saving and its reason shows under the form.
- **Gmail and Microsoft** addresses (`gmail.com`, `googlemail.com`, `outlook.com`, `hotmail.com`, `live.com`, `msn.com`, `office365.com`; `src/lib/oauth.ts`) get Sign in with Google / Microsoft instead of a password, with an optional sidebar name and a quiet note that the consent screen names Thunderbird, whose public sign-in Holtmail borrows unless the server owner registered their own. _Use an app password instead_ switches back to the IMAP form, and back again. Signing in calls `startOAuth` with the current path as `return_to`:
  - when the server needs a paste (Thunderbird's client, the default), the provider page opens in a new tab and the form becomes _Finish signing in_: a link to open the page again, the instruction to copy the address of the page that cannot be opened after allowing access, a field for it with a hint until it contains `code=` and `state=` (or `error=`), the time the sign-in expires, the server's error on failure, Start again, and Finish, which calls `finishOAuth` and then continues exactly like a connected IMAP account;
  - with the owner's own client the tab itself goes to the provider and comes back to the same page with `?oauth=done&account=<id>` or `?oauth_error=<reason>`. `useOAuthReturn` reads and strips those params. The first-run screen finds the archive of that account and shows the progress step, or returns to Connect with the error; Archives settings show a dismissible confirmation or error above the archives.
- **Microsoft Graph**: _Connect Microsoft 365 or Outlook.com through Microsoft Graph instead_ under the IMAP form (above the JMAP link, shown for every address, since Microsoft 365 mailboxes mostly use their own domains) switches to the Graph form (`connect-graph.tsx`), and _Connect over IMAP instead_ back. It asks for the address and an optional sidebar name, says it suits Microsoft 365 work and school mailboxes where IMAP is turned off as well as Outlook.com and Hotmail, shows the Thunderbird note, and Sign in with Microsoft calls `startOAuth` with `kind: 'graph'` (request built by `graphSignIn` in `src/lib/oauth.ts`, which refuses anything that is not an address). The paste step, own-client return and progress are the same as for Microsoft IMAP sign-in.
- **Import files** takes files dropped or chosen (`.mbox`, `.eml`, `.zip`; files without an extension are tried as mbox). They upload one after another, each with a thin progress bar that turns green when the server has it.
- The last step lists the sources with a live status: a pulsing accent square while syncing with the count of new messages, then "Synced 2 minutes ago". The archive can be opened right away; folders and messages appear while the sync runs.

## Settings

Reached from the user menu, the command palette or the "Add mail" link at the bottom of the sidebar. A section nav on the left (a top tab row on narrow screens) with an accent bar that glides between sections.

- **Archives**: each archive with its totals, a menu to rename it, export it (an _Export_ submenu with _mbox file_ and _Zip of .eml files_, shown when it has messages) or delete it (deleting asks to type its name), its mail sources and a _Save remote images_ switch, which explains that images are stored once and warns that downloading tells senders the message was fetched. When on, a panel shows saved images, their size and failures, offers _Save images of older mail_ (with the number of messages archived before) and _Retry failed images_, and while one runs shows its progress with Stop and a link to Tasks. A source row shows its kind, address and live status; Sync now spins while it runs; the menu renames, pauses or resumes, changes the password, or the API token of a JMAP token account (checked by signing in; not offered for OAuth accounts, whose connection dialog has no password either), edits the connection (server, port, security, user name, excluded folders, optionally a new password, required with a short note as soon as the host or port differs from the saved one or the security drops to none; for OAuth accounts server, port and security are read-only with a note that the provider sets them; for JMAP accounts the address, _Password_ or _API token_, user name, excluded mailboxes and optionally a new secret, required when the sign-in kind changes or the address moves to another origin; the comparison is `src/lib/connection.ts`, the same rule the server enforces; checked by signing in), sets how often it checks for new mail (like the server, an interval, or only on request), sets _What to archive_ (all mail as it arrives, or only mail older than a week, 30 days, 90 days or a year; a value set from the CLI shows as its own choice; younger mail is archived by a later sync once it is old enough) or removes it (archived messages stay). The row shows when the next sync is due. JMAP accounts show "JMAP" next to their name, Graph accounts "Graph" (and "Microsoft sign-in"; their menu has _Sign in again_ and no connection to edit). OAuth accounts show "Google sign-in" or "Microsoft sign-in" and offer _Sign in again_ in the menu; when the provider stopped accepting the sign-in the status reads "Sign-in expired" in red (also in the sidebar) and _Sign in again_ replaces Sync now. Password accounts on Gmail or Microsoft servers offer _Switch to Google / Microsoft sign-in_. Both open a dialog that runs the same start, paste and finish flow for that account. Above the archives, _Checking for new mail_ sets the interval for sources without their own, read-only when the server's config sets it. Connect a mailbox and Import files open the same forms as the first run in a dialog. New archive creates one and opens Connect for it.
- **People**: everyone who can sign in, with role and access. Add person sets email, name, a password to hand over, the role (members also get a checklist of archives). The row menu changes role, edits a member's archives, sets a new password (ending their sessions) or removes them. Only owners manage owners; nobody changes their own role or removes themselves.
- **Tasks**: the running tasks and the 200 most recent finished ones, also from before a restart, running first: kind and title, state badge (_Interrupted_ in the warning color, with a line saying that syncs run again on schedule and image downloads pick up where they stopped, for tasks a restart stopped), a progress bar (sliding while the amount of work is unknown), the summary, start time and duration, Stop for remote image tasks, and a collapsible log that follows new lines while scrolled to the end. The nav item shows how many are running, and so does a link at the bottom of the sidebar.
- **Network**: the image proxy. Shows the current address (credentials never come back), saves a new one after the server checks it accepts connections, removes it; read-only when the server's config sets it.
- **Reading**: remote images, kept in this browser (`localStorage`, `holtmail.remote-images`), because the address a sender sees depends on the device. _Hide, and ask each time_ (default), _Always show, through Holtmail_ (the server fetches them; senders see its address) or _Always show, directly_ (with a warning that senders see your IP address, rough location and when you open each message). Saved copies always show.
- **Profile**: name and email, and a password change that keeps this browser signed in and signs out the others.

While any source of the shown archive syncs or imports, the app refreshes its archive totals, folders and message list every four seconds, and everything once more when the run ends. An archive without messages shows "This archive is empty" with an Add mail button, or "Archiving mail…" while its first sync runs. The sidebar lists the archive's sources with their status squares.

## Layout

Three panes: folders, message list, reader.

- From `68.75rem` (1100px): all three side by side.
- From `45rem` (720px): folders and list; the reader slides over the list and has a back button.
- Below: one pane at a time. The list fills the screen, the reader slides over it, the folders open in a `Sheet` from the menu button.

The list pane is `inert` while the reader covers it.

## Message list

Virtualized with `@tanstack/react-virtual` and loaded page by page as the end comes into view. Rows are grouped by day with a sticky header: "Today", "Yesterday", the weekday within the last week, "12 March" this year, "March 2019" for older years (one group per month). Each row has the sender, the date (time today, weekday this week, day this year, short date before) in tabular mono, the subject, a one-line preview and a paperclip for attachments. In search results, a row whose words were found only inside an attachment (`attachment_match`) shows a quiet hairline chip "In attachment" with the paperclip instead. Unread rows (no `\Seen` in the folder shown, or in any folder for all mail and search) set sender and subject in medium weight and carry a small accent square at the left edge; read rows are regular weight with a muted sender. A star marks `\Flagged`, and a hairline badge with the count marks conversations of more than one message. Labels show at the right of the subject as up to two quiet hairline chips (muted text, square, no fill) with `+N` for more; Gmail's system labels (Inbox, Starred, Important, Sent, Drafts, Spam, Trash) and the label being shown are left out, so only labels that tell something appear. List chips are plain text, not links, to keep the list a single `listbox`. The list is a `listbox`; the cursor is its `aria-activedescendant`, drawn as a selected background with a 2px accent bar that glides between rows and jumps without animation while the list scrolls.

While the list has rows, a download button at the right of the header exports what it shows: all mail, the folder, the label or the search. Its menu names what is exported and offers _mbox file_ (one file for mail clients and archivers) and _Zip of .eml files_ (one file per message, in folders); each is a plain link to `GET /api/archives/{id}/export` with `download`, so the browser streams the file to disk and the session cookie goes along. The address comes from `exportHref` in `src/lib/export.ts`.

## Reader

Under the subject, the open message's labels as outline badges with a tag icon, system labels first; each is a link to that label's list.

The whole conversation (`/api/threads/{id}`), oldest first. The open message and the latest one are expanded; others are one line (sender, the message's `preview`, date) and expand in place. HTML bodies render in an iframe with `srcdoc`, `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"` (no scripts), links opening in a new tab, resized to their content on load and on every `ResizeObserver` change. When the server left remote images out, a banner offers Show (this message, through the server's proxy) and Always show…, which opens Reading settings. When saved copies are shown, a quiet line says so and offers Load from sender (through the proxy, or directly when Reading says so) and back. Plain text keeps its line breaks and links. Attachments are square tiles with a hairline border: PNG, JPEG, GIF and WebP images show the server's thumbnail (`thumbnail_url`, at most 320 px, lazy-loaded, cropped to the square with `object-cover`), falling back to the type icon when the thumbnail does not load; everything else shows an icon by type. Types the server shows inline (PNG, JPEG, GIF, WebP, AVIF, PDF) open full size in a new tab; the rest download. The toolbar has Reply (`mailto:` to Reply-To or From with a `Re:` subject, `In-Reply-To` and `References` for the clients that honor them, and a body quoting the first eight lines of the text under "Sender wrote:"), Download `.eml`, View source and Copy link.

## Search and commands

The search field sits in the list header. `Enter` opens the search route, an empty query goes back to all mail, and a `400` from the API shows its message under the field. While the field is focused or a search is shown, a row of operators (`from:` `to:` `subject:` `has:attachment` `attachment:` `before:` `after:` `in:` `label:` `is:unread`) inserts tokens at the caret. `⌘K` / `Ctrl+K` opens the command palette: run a search, jump to a folder, label or archive, open settings, connect a mailbox, import files, sync every source now, export the archive as mbox or as a zip of `.eml` files, toggle the theme, show shortcuts.

## Keyboard

| Keys            | Action                                                |
| --------------- | ----------------------------------------------------- |
| `j` / `↓`       | Next message (`↓` while the list has focus)           |
| `k` / `↑`       | Previous message                                      |
| `Enter` / `o`   | Open the message and focus the reader                 |
| `Esc`           | Back to the list; closes the reader on narrow screens |
| `/`             | Focus search                                          |
| `g i` / `g a`   | Inbox / all mail                                      |
| `⌘K` / `Ctrl+K` | Command palette                                       |
| `?`             | Shortcuts                                             |

Single keys are ignored while typing in a field and inside dialogs. The keymap lives in `src/lib/keymap.ts`; matching (modifiers, `mod` as `⌘` on macOS and `Ctrl` elsewhere, two-key sequences within one second) in `src/lib/shortcuts.ts`.

## Motion

Through `MotionProvider` and the token presets: the reader fades and rises 8px with the expressive curve when the selection changes; thread messages expand and collapse with height and opacity; the selection bar moves with the productive curve; panes and the folder sheet slide on narrow screens. Nothing in the list animates while it scrolls. With reduced motion, movement is dropped and height changes are instant.

## Structure

| Path              | What it does                                                                                                       |
| ----------------- | ------------------------------------------------------------------------------------------------------------------ |
| `src/main.tsx`    | Providers: TanStack Query (with the global `401` handler), `MotionProvider`, `TooltipProvider`, router             |
| `src/router.tsx`  | Routes and their search param validation                                                                           |
| `src/pages.tsx`   | Nothing shared yet, archives failed to load, not found                                                             |
| `src/onboarding/` | The first-run screen                                                                                               |
| `src/settings/`   | Settings layout and the archives, people and profile pages                                                         |
| `src/manage/`     | Shared forms: connect a mailbox, import files, source list with status, confirm and text dialogs                   |
| `src/strings.ts`  | Every user-facing string, ready for i18n                                                                           |
| `src/auth/`       | Sign-in and setup screens, their fields, session checks for routes                                                 |
| `src/mail/`       | Layout, sidebar, list, search field, reader, thread, body, attachments, dialogs                                    |
| `src/hooks/`      | `useHotkeys`, `useLayout`, `useNow`, `useLiveArchive`, `useOAuthReturn`                                            |
| `src/lib/`        | Pure logic: day grouping, folder and label order, shortcuts, sizes and dates, links, query, flags, sign-in helpers |
| `src/theme.ts`    | `useTheme()`, `toggleTheme()` and `parseTheme()`                                                                   |

Folders are grouped by account. Each shows its unread count emphasised next to the muted total; folders with nothing unread show the total only. Folders with a role come first (inbox, starred, important, drafts, sent, archive, all mail, junk, trash) and are never nested; the others follow alphabetically with natural number order and nest under their nearest existing parent path.

Archives with labels (Gmail) get a Labels section under the folders, with the same counts: Gmail's system labels first (Inbox, Starred, Important, Sent, Drafts, Spam, Trash), then the others by name with natural number order, nested under their nearest existing parent (`Work/Projects` shows as `Projects` under `Work`). The section is absent while the archive has no labels. Labels are refreshed with the folders while a source syncs.

## Theme

Chosen in the user menu or toggled from the command palette. The choice is stored in `localStorage` under `holtmail.theme` as `light` or `dark`; `system` removes the key. `light` and `dark` set `data-theme` on `<html>`; `system` removes it and the tokens follow `prefers-color-scheme`. Every storage access is wrapped in `try`/`catch`.

## Tests

`bun test apps/web` covers day grouping, date and size formatting, folder ordering and nesting, shortcut parsing and sequences, route search params, link detection, reply `mailto:` URLs and quoting, label order and nesting, list chips and reader label order, attachment kinds and which types open in the browser, operator insertion, flags, the sign-in helpers (`next` validation, password length, `Retry-After` minutes, error messages), provider detection and server guesses, export scopes and download addresses, when an edited connection needs the password again, OAuth provider detection by address and server, sign-in answer detection and the `oauth` return params, source status labels and relative times, upload file kinds and roles.

The sign-in screens, the first-run screen and settings are separate chunks loaded when their route opens.
