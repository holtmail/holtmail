# @holtmail/web

The Holtmail web app: a calm, keyboard-first, read-only mail client for an archive. Vite, React 19, TypeScript, Tailwind CSS v4, TanStack Router, Query and Virtual, Motion. It talks to the server only through [`@holtmail/api-client`](../../../packages/api-client/docs/README.md), takes components from [`@holtmail/ui`](../../../packages/ui/docs/README.md) and every color, size and duration from [`@holtmail/tokens`](../../../packages/tokens/docs/README.md).

## Commands

From the repository root:

- `bun run dev:web` — dev server; `/api` is proxied to `HOLTMAIL_SERVER` (default `http://127.0.0.1:8080`)
- `bun run build` — production build into `apps/web/dist`
- `bun run typecheck`, `bun run lint`, `bun run test`

## Routes

Code-based TanStack Router routes in `src/router.tsx`. Every mail route accepts `?m=<messageId>`, the open message, so a URL is a shareable view and back and forward move between messages.

| Path                        | Shows                                                                                |
| --------------------------- | ------------------------------------------------------------------------------------ |
| `/`                         | Redirects to the first archive; an empty state when there is none                    |
| `/a/$archiveId`             | All mail of the archive                                                              |
| `/a/$archiveId/f/$folderId` | One folder                                                                           |
| `/a/$archiveId/search?q=`   | Search results for `q` in the [query language](../../../crates/query/docs/README.md) |
| `/login?next=`              | Sign-in; returns to `next` (a path on this site) afterwards                          |
| `/setup?token=`             | Creates the owner with the one-time token from the setup link                        |

Opening a message by click or `Enter` pushes a history entry; moving through the list with `j`/`k` replaces it.

## Sign-in

The mail routes check `GET /api/session` before they load (cached for the visit). On `401` the app asks `GET /api/setup`: while the instance has no users it goes to `/setup`, otherwise to `/login?next=<the requested path>`. `/setup` without a token explains that the setup link is in the server log; `/setup` once users exist goes to `/login`, and `/login` while signed in goes straight to `next`. A `401` from any data query later (an expired session) clears the cached session and goes to `/login?next=<current path>`; queries are never retried on a `4xx`.

Both screens are one narrow centered column: the wordmark with a hairline that draws itself, then title, lead and form rising in one after another with the expressive curve (fades only under reduced motion). Fields are Carbon-style (label above, filled field with a bottom rule), focus starts in the first field, `Enter` submits, the password has a visibility toggle, and the autocomplete hints are `username`, `current-password` and `new-password`. Setup shows a live hint until the password has 10 characters. Errors from the API show under the form and move focus back to the field to fix; a `429` reads "Too many attempts, try again in N minutes" from `Retry-After`. `next` is accepted only as a same-site path that is not `/login` or `/setup`.

The user menu at the right of the top bar shows the name and email, the theme (light, dark, system) and Sign out, which posts `/api/logout`, drops every cached query and goes to `/login`.

## Layout

Three panes: folders, message list, reader.

- From `68.75rem` (1100px): all three side by side.
- From `45rem` (720px): folders and list; the reader slides over the list and has a back button.
- Below: one pane at a time. The list fills the screen, the reader slides over it, the folders open in a `Sheet` from the menu button.

The list pane is `inert` while the reader covers it.

## Message list

Virtualized with `@tanstack/react-virtual` and loaded page by page as the end comes into view. Rows are grouped by day with a sticky header: "Today", "Yesterday", the weekday within the last week, "12 March" this year, "March 2019" for older years (one group per month). Each row has the sender, the date (time today, weekday this week, day this year, short date before) in tabular mono, the subject, a one-line preview and a paperclip for attachments. Unread rows (no `\Seen` in the folder shown, or in any folder for all mail and search) set sender and subject in medium weight and carry a small accent square at the left edge; read rows are regular weight with a muted sender. A star marks `\Flagged`, and a hairline badge with the count marks conversations of more than one message. The list is a `listbox`; the cursor is its `aria-activedescendant`, drawn as a selected background with a 2px accent bar that glides between rows and jumps without animation while the list scrolls.

## Reader

The whole conversation (`/api/threads/{id}`), oldest first. The open message and the latest one are expanded; others are one line (sender, the message's `preview`, date) and expand in place. HTML bodies render in an iframe with `srcdoc`, `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"` (no scripts), links opening in a new tab, resized to their content on load and on every `ResizeObserver` change. When the server blocked remote content, a banner offers to load it. Plain text keeps its line breaks and links. Attachments are square tiles with an image thumbnail or an icon by type. The toolbar has Reply (`mailto:` to Reply-To or From with a `Re:` subject, `In-Reply-To` and `References` for the clients that honor them, and a body quoting the first eight lines of the text under "Sender wrote:"), Download `.eml`, View source and Copy link.

## Search and commands

The search field sits in the list header. `Enter` opens the search route, an empty query goes back to all mail, and a `400` from the API shows its message under the field. While the field is focused or a search is shown, a row of operators (`from:` `to:` `subject:` `has:attachment` `before:` `after:` `in:` `is:unread`) inserts tokens at the caret. `⌘K` / `Ctrl+K` opens the command palette: run a search, jump to a folder or archive, toggle the theme, show shortcuts.

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

| Path             | What it does                                                                                             |
| ---------------- | -------------------------------------------------------------------------------------------------------- |
| `src/main.tsx`   | Providers: TanStack Query (with the global `401` handler), `MotionProvider`, `TooltipProvider`, router   |
| `src/router.tsx` | Routes and their search param validation                                                                 |
| `src/pages.tsx`  | No archives, archives failed to load, not found                                                          |
| `src/strings.ts` | Every user-facing string, ready for i18n                                                                 |
| `src/auth/`      | Sign-in and setup screens, their fields, session checks for routes                                       |
| `src/mail/`      | Layout, sidebar, list, search field, reader, thread, body, attachments, dialogs                          |
| `src/hooks/`     | `useHotkeys`, `useLayout`, `useNow`                                                                      |
| `src/lib/`       | Pure logic: day grouping, folder order, shortcuts, sizes and dates, links, query, flags, sign-in helpers |
| `src/theme.ts`   | `useTheme()`, `toggleTheme()` and `parseTheme()`                                                         |

Folders are grouped by account. Each shows its unread count emphasised next to the muted total; folders with nothing unread show the total only. Folders with a role come first (inbox, starred, important, drafts, sent, archive, all mail, junk, trash) and are never nested; the others follow alphabetically with natural number order and nest under their nearest existing parent path.

## Theme

Chosen in the user menu or toggled from the command palette. The choice is stored in `localStorage` under `holtmail.theme` as `light` or `dark`; `system` removes the key. `light` and `dark` set `data-theme` on `<html>`; `system` removes it and the tokens follow `prefers-color-scheme`. Every storage access is wrapped in `try`/`catch`.

## Tests

`bun test apps/web` covers day grouping, date and size formatting, folder ordering and nesting, shortcut parsing and sequences, route search params, link detection, reply `mailto:` URLs and quoting, attachment kinds, operator insertion, flags, and the sign-in helpers (`next` validation, password length, `Retry-After` minutes, error messages).
