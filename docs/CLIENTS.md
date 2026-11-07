# Plan: native clients

Planned 2026-11-06 and revised the same day: autopush-rs as the relay, no paid developer accounts at first, and edits from native calendar and contacts apps. The steps are WORKPLAN Phases 43–54. Nothing here is built yet.

Holtmail gets apps for macOS, Windows, Linux, Android and iOS. Each app is a thin shell around the SPA that the user's own server already serves. It adds what a browser tab cannot do:

- instant notifications, also when the app is closed;
- a badge, a tray or menu bar icon, deep links, share into the app;
- contacts and calendars in the system apps, **editable there**;
- an offline copy of a chosen part of the archive.

**Edits from native calendar and contacts apps** are accepted from Phase 51 and held in Holtmail; see [Edits from native apps](#edits-from-native-apps). Sending them to the upstream server, replying to invitations by mail and writing mail come later. Their names are reserved here.

## Decisions

**One shell technology: Tauri 2 on all five platforms.** It uses the system webview:

- WKWebView on macOS and iOS;
- WebView2 on Windows;
- WebKitGTK on Linux;
- Android System WebView.

No Chromium ships with the app, and installers are 3–10 MB. The shell core is Rust, so it reuses crates from this repository.

**Platform code where the OS allows nothing else** (about 300 lines, written when native mobile apps ship):

- a Swift Notification Service Extension on iOS, which decrypts a push and shows it while the app is not running;
- a Kotlin UnifiedPush receiver on Android, which shows a notification without starting the webview or Rust.

Neither depends on Tauri.

**Fallback: Capacitor, for mobile only**, used if Tauri mobile fails the spike. The bridge is a contract and the native pieces do not depend on the shell, so switching rewrites only the glue.

**No paid developer accounts at first.** There is no Apple Developer Program, Google Play or Windows signing account yet. Every platform starts with what needs none:

- the SPA as an installable PWA with Web Push;
- contacts and calendars through DAV;
- the Tauri desktop shell as an unsigned preview build, with Linux fully supported.

Native iOS and Android apps, signing and notarisation wait for the accounts (see [Distribution](#distribution)).

| Rejected | Why |
| --- | --- |
| Electron, CEF | They bundle Chromium |
| Capacitor and Tauri as the default | Two shells and two plugin systems, so the glue is written twice |
| Wails v3 | Adds Go; its mobile support is experimental |
| Electrobun, Neutralino | Desktop only; Electrobun recommends CEF on Linux |
| Dioxus | The same engine as Tauri, without its plugins, ACL and updater |
| Flutter, KMP, .NET MAUI | A third language only to host a webview; MAUI has no official Linux |
| Raw shells per OS | Four codebases |
| Universal Links, App Links | The domain must be known at build time; self-hosted domains are not |
| Writing into EventKit, Contacts, Windows Appointments, EDS | Per-OS code and duplicate entries; served DAV gives the same result with a standard write path |
| Our own push relay | autopush-rs does the job and stays replaceable (see [The relay](#the-relay-autopush-rs)) |

**Principles**

- **The SPA always loads from the user's server, at its own origin.** It is then same-origin with its API, so the session cookie, `guard_writes`, the body iframe and inline images work unchanged. The SPA updates with the server; shell releases are rare.
- **The server speaks one push protocol: Web Push** (RFC 8030, RFC 8291, VAPID RFC 8292). Browsers, the iOS Home Screen PWA and UnifiedPush receive it directly. Native apps reach APNs and FCM through autopush-rs, which only forwards ciphertext.
- **Contacts and calendars reach the OS through standards:** an ICS feed and CalDAV/CardDAV served by Holtmail. Edits made there are held in Holtmail, and are sent upstream only once write-through exists.
- **The SPA never imports Tauri.** It talks to `@holtmail/host`, which detects the host and degrades to the browser.
- **A push is a hint; the API stays the truth**, as in [PUSH.md](PUSH.md). A lost push makes a notification late, never wrong.

## Architecture

```
                  ┌──────────────────────── user's server (apps/server) ─────────────────────────┐
                  │ SPA + CSP   /api/*   /api/live (SSE)   /dav/** (read + held edits)   /ics/*   │
                  │ auth: cookie | Bearer (device token) | Basic (app password, DAV only)          │
                  │ store `changes` log ─► live stream, sync-tokens, /changes feed                 │
                  │ events ─► notify-policy ─► notify-webpush (VAPID + RFC 8291) ─► durable queue  │
                  └──────┬──────────────┬──────────────────┬──────────────────────┬───────────────┘
                   HTTPS same origin   SSE (bearer)     Web Push (encrypted)    CalDAV / CardDAV / ICS
                         │              │      ┌───────────┼─────────────┐              │
┌──── apps/shell (Tauri) ┼──────────────┼─┐    ▼           ▼             ▼              ▼
│ launcher (bundled)     │              │ │  browser     ntfy,       autopush-rs     iOS/macOS accounts,
│ webview ─ online ─► https://server/   │ │  push        UnifiedPush (optional) ─►   DAVx5, GNOME, KDE,
│         └ offline ─► holtmail:// ─► embedded holtmail-api   APNs / FCM          Thunderbird; Outlook (ICS)
│ bridge: plugin:holtmail|* (origin-checked)  (PWA,                │
│ core: client (HTTP + SSE), keystore,     │   iOS PWA)           ▼
│       replica pump, notifications, tray  │              iOS NSE, Android receiver (later)
└──────────────────────────────────────────┘
apps/web ─► @holtmail/host ─► ./web (browser, PWA) | ./tauri (invoke)
```

## Layout

```
apps/
  shell/                 Tauri app, the client-side composition root; its own Cargo workspace
    src/                 profiles, enrolment, bridge commands, offline origin, tray, menu, scheduler
    launcher/            bundled first-run page (Vite, @holtmail/ui, @holtmail/tokens)
    plugins/             push, pim, badge, share: Tauri glue only
    native/apple/        NotificationService (Swift; crypto in a SwiftPM package)       later
    native/android/      UnifiedPushService.kt                                          later
deploy/
  autopush/              compose and docs for a self-run autopush-rs (third-party service, no code)
crates/
  notify/                contract: deliver an encrypted payload to a push subscription
  notify-webpush/        RFC 8030 delivery with holtmail-webpush and VAPID; endpoint allowlist
  notify-policy/         pure: event + device rules → notification or nothing
  dav-serve/             CalDAV, CardDAV and ICS over the store and people; writes through pim-edit
  pim-edit/              pure: validate, classify and rebase edits from devices; field diff
  blob-encrypted/        AES-256-GCM wrapper over any BlobStore (already named in PLAN.md)
  client/                async HTTP client for the Holtmail API: bearer, SSE via holtmail-sse, changes
  keystore/              contract: get, put, delete small secrets
  keystore-os/           Keychain, Credential Manager, Secret Service, Android Keystore
  replica/               library: pulls /changes into a local Store, BlobStore and Index; pruning
  -- grown, not new --
  webpush/               + vapid (ES256 JWT with aws-lc-rs)
  auth/, store/, store-sqlite/   + devices, scopes, enrolment codes, app passwords, `changes` log,
                                   `item_changes`, `write_mode`, version origin
  api/                   + /api/meta, /api/live, devices, push, changes, app passwords, /api/pim/changes
packages/
  host/                  @holtmail/host: contract (.), ./web, ./tauri, ./testing
```

The shell embeds the `holtmail-api` crate for offline use; see [Offline subset](#offline-subset). The crate itself does not move.

**AGENTS.md changes in Phase 44**, before any client crate lands:

- `apps/shell` is a second composition root, for the client side. It is excluded from the root workspace, so WebKitGTK, WebView2 and the NDK never touch `cargo test --workspace`.
- The client crates (`client`, `keystore*`, `replica`) live in the root workspace. Their tests therefore run in `mise run check`, network-free and webview-free.
- `deploy/autopush` holds configuration for a third-party service, not code.
- Swift and Kotlin code is documented in the shell's `docs/`, with its limits and how to run its tests.

No contract crate lands without an implementation and a conformance suite. Write-side contracts and the relay client are described here, and become crates with their first implementation.

## Host bridge (`@holtmail/host`)

The package has four entry points:

- the root holds the contract and `detectHost()`;
- `./web` is for the browser and the PWA;
- `./tauri` is a code-split `invoke` binding, loaded only in the shell;
- `./testing` has `conformance(host)`, mirroring the Rust `testing` features.

```ts
export const HOST_PROTOCOL = { major: 1, minor: 0 } as const

export interface Host {
  readonly kind: 'browser' | 'pwa' | 'shell'
  readonly platform: 'web' | 'macos' | 'windows' | 'linux' | 'ios' | 'android'
  readonly protocol: { major: number; minor: number }
  has<K extends keyof Capabilities>(name: K, minVersion?: number): boolean
  get<K extends keyof Capabilities>(name: K): Capabilities[K] | undefined
}

export interface Capabilities {
  device: DeviceCap               // status, enrol, profiles, switch
  notifications: NotificationsCap // permission, request, setFocus, onActivate
  badge: BadgeCap
  push: PushCap                   // which channel: sse | webpush | unifiedpush | relay
  links: LinksCap                 // openExternal, openAuth, onDeepLink
  share: ShareCap                 // share out; files shared in go to the import flow
  downloads: DownloadsCap
  window: WindowCap               // native menu commands, visibility
  pim: PimCap                     // methods, connect, disconnect, status, addEvent
  offline: OfflineCap             // config, status, wipe
  // Reserved, never advertised until write lands: outbox, 'write.mail'
}

export function detectHost(): Promise<Host>
```

- **Detection is an async handshake.** Tauri does not inject its globals reliably into remote pages on Android. So `detectHost()` probes `__TAURI_INTERNALS__` and retries `invoke('plugin:holtmail|hello')` for about a second after `DOMContentLoaded`. With no answer it uses `./web`, as `pwa` when `display-mode: standalone` matches.
- **The SPA gates features on capabilities, never on `kind`.** Each capability has its own integer version, as in `has('pim', 2)`.
- **Versioning works in both directions.** A breaking call convention bumps `protocol.major`, and shells implement N and N−1. `/api/meta` names `host_protocol.min`, below which the shell asks for an update. It also lists `features`, so a newer shell hides what an older server lacks.
- **The SPA plugs in at two places.** `apps/web/src/main.tsx` wraps the app in `HostProvider`, and `apps/web/src/host/` is the only place that imports an implementation.

## Loading and sign-in

**First run**

1. The launcher (or the browser, for a PWA) takes a server URL, or a `holtmail://pair?server=…&code=…` link or QR code from web Settings → Devices.
2. The core calls `GET {url}/api/meta`. Without a session it answers only `product`, `public_url`, `host_protocol` and `features`. The version, the build and the VAPID key need a session. `public_url` becomes the profile's origin.
3. Only HTTPS is accepted.
   - Plain HTTP is allowed for `localhost`, private addresses and `.local`/`.lan`, behind a warning.
   - Self-signed certificates go through OS trust; the app has no bypass.
4. The core grants a runtime capability to that origin and the `main` webview only (`CapabilityBuilder::new(..).remote(origin).local(false)`, Tauri feature `dynamic-acl`, Tauri 2.11.1 or later). Then it navigates there.
   - A separate local capability covers the launcher and the offline origin.
   - The init script checks `location.origin` itself, because it runs on every navigation.

**Signing in inside a webview**

- **Password:** works in the webview as it does today.
- **Passkeys and OIDC:** do not work in the webview, because WKWebView and Android WebView allow WebAuthn only for domains fixed at build time.
  - They run in a system browser session through `links.openAuth`: `ASWebAuthenticationSession` on Apple, Custom Tabs on Android, the default browser on desktop.
  - The browser login ends with a one-time enrolment code, returned by a `holtmail://pair` link.
- **Gmail and Microsoft account OAuth** (`crates/oauth`): also goes through `openAuth`, because Google refuses embedded webviews.
  - The callback is keyed by `state` and needs no cookie, so it completes in the browser.
  - The webview learns the result from the live stream.

**Devices**

- **Enrolment**
  1. After sign-in, the SPA sees an unenrolled shell and asks `POST /api/devices/enroll-code` (cookie) for a single-use code valid for 60 s.
  2. `host.device.enroll(code)` redeems it at `POST /api/devices/enroll {code, name, platform}`.
  3. The server refuses a code whose user differs from the cookie session's user.

  A code that arrives through a `holtmail://pair` link first shows a native confirmation: "Pair with X as Y?".
- **Token**
  - It is opaque, stored hashed on the server and in the OS keystore on the device. JavaScript never sees it.
  - Scopes are `read`, `notify` and `dav:read`. `dav:write` allows held edits. The reserved scopes `write:mail`, `write:pim` and `write:contacts` are granted by nothing yet.
  - `require_session` accepts `Authorization: Bearer`. `guard_writes` skips its CSRF checks only for bearer requests.
- **Cookie renewal**
  - Sessions created on an enrolled device are sliding.
  - When one ends anyway, the core sets a fresh cookie through the platform cookie store or an auto-submitted POST. It never uses a `GET ?code=` URL, which a reverse proxy would log.
- **Revocation**
  - `DELETE /api/devices/{id}` makes the next call answer `401 device_revoked`.
  - The core then wipes the replica and the keystore and returns to the launcher.
  - Push subscriptions and app passwords die with the device, and its SSE streams are closed.

**Profiles (several servers)**

A profile is `{origin, user, device}`. Each profile has its own:

- webview data store: Tauri `data_store_identifier` on Apple (which needs iOS 17 and macOS 14), `data_directory` on WebView2 and WebKitGTK;
- keystore entries;
- replica;
- push subscription.

How profiles work per platform:

- Desktop switches profiles from the tray or menu.
- Android uses `androidx.webkit` profiles only when `MULTI_PROFILE` is supported. Until then, mobile has one profile.
- A notification tap carries its profile and switches to it before navigating.

## Live updates

**One change log**

- A new append-only table, `changes(seq, archive_id, kind, entity_id, op)`, is written in the same transaction as ingest, pim-sync, people writes and held edits. It is pruned after 90 days.
- It serves the live stream, the DAV sync-tokens and the offline feed. It survives restarts.
- The store contract gains `changes_since(seq, limit)`, with conformance cases.
- A sync-token older than the pruned window answers the RFC 6578 `valid-sync-token` error, so DAV clients fall back to a full resync instead of looping.

**`GET /api/live` (SSE).** `/api/events` is already the calendar route.

- Accepts a cookie or a bearer token.
- Grants are checked per event, cached and invalidated on grant change. Streams close when a grant or a device is revoked.
- Each event carries `id: <seq>`. `Last-Event-ID` resumes from the table; a position older than the retained window gets `event: reset`.
- Sends a heartbeat every 25 s, with `X-Accel-Buffering: no` and `Cache-Control: no-cache`. `proxy_buffering off` is documented for nginx and Caddy.
- Events carry the kind, the archive, at most 200 ids and counts. They never carry content.

A `live` flag set by ingest marks arrivals from a watch or an incremental poll. Backfill and import publish changes but never notify.

**The SPA** gets `useLiveEvents()`. It maps event kinds to query invalidations and replaces the 4-second polling in `use-live-archive.ts`. The existing `refetchInterval`s stay only while the stream is down. Browsers get this before any shell exists.

**The desktop core** keeps its own bearer SSE connection, which drives notifications, the badge, the tray and replica pulls. It reconnects with backoff and after sleep. On mobile it runs only in the foreground.

## Notifications

**Server pipeline**

1. **Policy.** `notify-policy` is pure: it applies the per-device rules from Settings → This device, with the [defaults below](#notification-defaults).
2. **Coalescing.** More than 5 notifications to one device within 60 s become one summary. The Web Push `Topic` is the thread id, so a later message replaces an earlier one.
3. **Delivery** is a durable queue (the pattern of `webhooks.rs`) that sends through `Notify`.
   - Endpoints must be HTTPS and on the allowlist (`[notify]`):
     - built in: `fcm.googleapis.com`, `updates.push.services.mozilla.com`, `web.push.apple.com`, `*.notify.windows.com`;
     - configured: `relays` (a self-run autopush) and `distributors` (ntfy and others).
   - Private address ranges are refused unless allowed.
   - A 404 or 410 drops the subscription. A 413 resends without the preview. A 429 honours `Retry-After`.
4. **Payload.** It is encrypted end to end with RFC 8291, with at most 2 KiB of plaintext: APNs caps the base64 body at 4 KiB.

   ```json
   {"v":1,"kind":"mail","archive":"…","archive_label":"Work","ids":["…"],"thread":"…","seq":123,"count":1,
    "preview":{"from":"Alice Example","subject":"…"}}
   ```

   The device builds the text from `kind`, `count` and the labels, so it is localised there.
5. **Badge.** The server keeps a `seen_seq` per device. `POST /api/devices/{id}/seen` clears it and updates the user's other devices, except iOS PWAs (see below).
6. **VAPID key.** It is generated at first start and stored with `holtmail-secret`. It is never rotated silently, because autopush and browsers bind each endpoint to it; a rotation is an explicit admin action that re-registers every device.

### Notification defaults

| What | Default |
| --- | --- |
| New mail | On, for `live` arrivals only (never backfill or import). Inbox role only, and the message date must be within 24 h. Mail with `List-Id`, and mail the insights categories mark as newsletters or promotions, is skipped. "Only people I have written to" is off |
| Content | Sender display name and subject, the subject cut to 80 characters. Never addresses, body text or attachment names. On iOS and Android the lock screen follows the OS "Show previews" setting. Over the budget, the subject goes first, then the whole preview |
| Preview turned off | "New message in <archive label>". The device may fetch `GET /api/notify/summary?ids=…` with its `notify` token |
| One-time codes (`insight-code`) | Off. Turning them on shows a warning about the lock screen. On iOS, "Copy code" needs an unlocked device |
| Urgency and TTL | Mail: `high`, 4 h. Reminders: `high`, until the event starts. Account and integrity notices: `normal`, 24 h. Edit notices: `low`, 7 days |
| Calendar reminders | Off on devices with a Holtmail DAV account, which fire the same alarms themselves. Elsewhere on, from the VALARMs of events the user has not declined |
| Held RSVP | One notice per answer: "Your answer to <event> is saved in Holtmail but the organizer has not been told". It has an action that opens a mail reply |
| Account needs sign-in | Always on |
| Integrity failures | Admins only |
| Quiet hours | Off by default. When set, everything except reminders waits until they end |
| Badge | Unseen notified items, cleared on open |

### Per platform

| Platform | Now (no paid accounts) | Later |
| --- | --- | --- |
| iOS, iPadOS | **Home Screen PWA** (iOS 16.4+): Web Push through `web.push.apple.com`, no relay and no account. The badge uses `navigator.setAppBadge` | Native app: the relay to APNs, then the NSE decrypts and shows the notification even after force-quit |
| Android | **PWA** in Chrome: Chrome's push endpoint with our VAPID key, no account of ours | Native app: UnifiedPush (`fdroid` flavor) without starting the webview, and FCM through the relay (`play` flavor) |
| macOS | PWA (Safari "Add to Dock"), or the **unsigned shell**: SSE in a menu bar agent → `UNUserNotificationCenter`, Dock badge | Signed and notarised builds |
| Windows | PWA (Edge, Chrome), or the **unsigned shell**: SSE in the tray → toasts with the installer's AUMID shortcut, taskbar badge | Signed builds, winget |
| Linux | The **shell**: SSE in a StatusNotifierItem tray → `org.freedesktop.Notifications` (a portal under Flatpak); also the PWA | — |

**iOS PWA rules.** WebKit revokes a subscription after a few pushes that show nothing. So:

- the service worker calls `showNotification` inside `event.waitUntil` for every push;
- every payload sent to an iOS PWA can be shown on its own;
- badge-only updates are never sent to iOS PWAs.

WebKit can also revoke a subscription without firing `pushsubscriptionchange`. So the PWA checks its subscription on every open and registers it again.

**Desktop shell notifications.** Desktop notification actions (Open, Copy code), Windows toasts with an AUMID and macOS `UNUserNotificationCenter` are custom code in the `push` plugin, because the official notification plugin covers actions only on mobile. A notification shown in the window the user is looking at is suppressed (`notifications.setFocus`).

### The relay: autopush-rs

The relay matters only for native mobile apps, which need APNs or FCM. To the server a relay is just another Web Push endpoint, so nothing on the server depends on which relay is used.

- **`deploy/autopush/`**
  - It runs only `autoendpoint`, built with the Redis backend (release 1.78 or later, pinned by tag), plus Valkey.
  - There is no `autoconnect`, because no Firefox WebSocket clients are needed.
  - The `apns` and `fcm` routers are configured only when credentials exist.
  - Config keys come from the source of the pinned tag, because the upstream docs are incomplete.
- **Licence.** autopush-rs is MPL-2.0 and runs as a separate service; it is not linked into Holtmail.
- **What it sees.** It stores registration ids and tokens. It sees sizes and timing, never plaintext: the payload stays RFC 8291 ciphertext all the way into the app.
- **Risk.** The Redis backend is new upstream. Holtmail's volume is low, and the relay can be replaced.
- **Phase 49** ships only the deployment and an integration suite. In it, autoendpoint with the `stub` router receives a push from `notify-webpush`; the suite checks that the ciphertext is unchanged and that an unregistered endpoint answers 410, after which the subscription is dropped.

**Client side, written with the first native app.** A `holtmail-relay` contract with `relay-autopush`:

```rust
#[async_trait]
pub trait RelayClient: Send + Sync {
    /// Turns a platform token (APNs, FCM) into a Web Push endpoint bound to `vapid_public`.
    /// Calling it again with the same token returns a working endpoint.
    async fn register(&self, token: &PlatformToken, vapid_public: &[u8]) -> Result<Registration, RelayError>;
    async fn update_token(&self, reg: &Registration, token: &PlatformToken) -> Result<(), RelayError>;
    /// Keeps the registration alive; `Gone` means register again.
    async fn check_in(&self, reg: &Registration) -> Result<CheckIn, RelayError>;
    async fn unregister(&self, reg: &Registration) -> Result<(), RelayError>;
}
```

- `Registration` holds the endpoint and an opaque handle (autopush's `uaid` and secret), kept in the keystore.
- The device generates its own keys with `holtmail-webpush` and sends `{endpoint, p256dh, auth}` to `POST /api/devices/{id}/push`.
- A different relay (ntfy's upstream, Sygnal behind an adapter, a small forwarder) is a new `relay-*` crate that passes the same conformance suite.

## Contacts and calendars in the system

There are three server layers. `pim.methods()` tells the SPA what the platform offers.

1. **ICS feed:** `GET /ics/{token}.ics`, built on the existing export.
   - The token is revocable, read-only and scoped to chosen calendars.
   - The feed carries `REFRESH-INTERVAL` hints.
2. **`dav-serve`: CalDAV and CardDAV.**
   - **Discovery:** `/.well-known/caldav` and `/.well-known/carddav`, the principal and the home set.
   - **Reads:**
     - `PROPFIND`, `getctag`, `sync-token` (from the `changes` log);
     - `sync-collection`, `*-multiget` and `calendar-query` with a time range;
     - `GET` with a strong ETag over the served bytes.
   - **Writes:** privileges follow each collection's `write_mode` (see below), and `max-resource-size` is advertised.
   - **Collections:**
     - one per source collection;
     - a merged "People" address book;
     - "From mail", holding only the invitations whose UID no other served calendar has.
   - **Auth:** Basic, with a per-device app password.
   - **Live updates:** WebDAV-Push is served through `notify`, so DAVx5 updates within seconds.
   - **Tests:** syncing `dav-serve` with `collection-dav` in memory.
   - **Code:** a thin protocol layer of our own on axum, `quick-xml` and `calcard`. The `dav-server` crate is built around files and has no ACL. Stalwart's DAV crates are AGPL, so they serve only as a reference.
3. **Shell glue**, only where the OS needs it.

| Platform | Primary | Also | "Add to calendar" from an invitation |
| --- | --- | --- | --- |
| iOS | `.mobileconfig` with CalDAV and CardDAV accounts and a fresh app password, handed to Settings | ICS | `.ics` link, or `EKEventEditViewController` in the native app later |
| macOS | `.mobileconfig` through System Settings | ICS, Thunderbird | `.ics` with the OS handler |
| Android | DAVx5 with the address filled in; WebDAV-Push makes it near-instant | ICS | `.ics` link, or `Intent.ACTION_INSERT` in the native app later |
| Linux | GNOME Online Accounts over D-Bus if it can be prefilled (spike), else a wizard for GNOME, KDE and Thunderbird | ICS | `.ics` with the OS handler |
| Windows | ICS subscription in Outlook | Thunderbird over DAV; the in-app calendar | `.ics` with the OS handler |

iOS Contacts shows only one address book per CardDAV account. So the `.mobileconfig` adds one CardDAV account for "People", and more accounts per book on request.

These come later, on demand: an Android SyncAdapter, so DAVx5 is not needed, and the iOS 18 ContactProvider extension, for contacts without an account.

## Edits from native apps

Users edit events and contacts, create and delete them, and answer invitations in their own apps. Holtmail accepts every valid edit immediately and serves it back to all of the user's devices. It sends the edit to the upstream server only when write-through exists for that collection.

Native apps behave well when the server answers like a normal CalDAV/CardDAV server. They behave badly with errors on a collection they believe is writable:

- Apple shows a sticky "Revert to Server" dialog;
- DAVx5 retries forever and stalls;
- Thunderbird asks the user to choose.

So a collection that advertises writing never refuses a valid write.

### Collection modes

Each collection has a `write_mode`:

| Mode | Privileges advertised | PUT and DELETE | Sent upstream |
| --- | --- | --- | --- |
| `read_only` | `read` | 403. Clients see this up front, so they show a lock and offer no editing | — |
| `held` | `read`, `write-content`, `bind`, `unbind` | Accepted as a held change | Never automatically |
| `write_through` (Phase 53) | the same | Accepted as a held change | Yes, by the writer, in order |

**Defaults:**

- Upstream calendars and address books are `held`.
- "From mail" and the merged "People" book are `read_only` until replies can really be sent (Phase 54) and People edits can be routed (Phase 53). Without that, an RSVP on an invitation that exists only in mail would never reach the organizer.
- Imported files are `read_only`.
- The server setting `[dav] edits = false` makes everything `read_only`.

**Who may write.** The advertised privilege is the lowest of three: the collection's `write_mode`, the user's role on the archive, and the device's scope (`dav:write`). Only the archive's owner may write until grants gain a read-only role. Each change records its user and device.

**Mode changes:**

- **`held` to `read_only`:** writes are still accepted as held changes for 24 h, until every client has refreshed its privileges. The new privileges are advertised at once.
- **`held` to `write_through`:** first shows the open changes with their count and age, and asks the user to confirm sending them.

### Storage

**Rules that keep the archive whole:**

- Upstream versions stay what they are today: `item_versions` records what a source showed.
- Device edits live in their own append-only table, `item_changes`:
  - the item (or the client's href, for a create), the operation, and a class (described below);
  - the exact bytes the client sent, as a blob that is never rewritten;
  - the patch against its base;
  - the base version and its upstream ETag;
  - `supersedes`, the user, the device, the state and the reason.
- A second edit to the same item adds a row that supersedes the first. The payload of a row never changes; only its state does.
- Nothing is deleted:
  - a DAV DELETE adds a delete change, and the archived item and its versions stay;
  - "Discard" keeps the row and its blob;
  - blobs referenced by changes count as referenced for retention and deletion.
- Every change writes a `changes` log row in the same transaction. The live stream, sync-tokens and the offline replica therefore see it.
- **Version origin:** `item_versions` gains `origin` and `change_id`. After a successful write-through, the upstream state is recorded with `origin = writeback`. The history can then say "Edited on Anna's iPhone · sent to Google on 7 Nov".
- **Derived data follows what is served.** The PIM index, people, event series and occurrences are rebuilt from the served version whenever a change opens or closes. Search, the calendar and the people pages then show what the user's devices show, with a mark. Derived data can still be rebuilt: its inputs are now versions plus open changes.

**`pim-edit`** is a pure library over `holtmail-pim`, used by `dav-serve` and, later, the API. It does four things:

1. **Validation:** parses with `calcard`, and checks that there is one UID per resource, the same UID as the resource it replaces, the right component type, and the size limit (2 MiB for iCalendar, 5 MiB for vCard).
2. **Classification**, by comparing normalized models with the base:

   | Class | What changed | Becomes a change row that needs attention |
   | --- | --- | --- |
   | `noop` | Nothing that matters: clients rewrite `DTSTAMP`, `PRODID`, `LAST-MODIFIED` on any save | No; the bytes are stored and served |
   | `personal` | Only alarms, acknowledgements and client properties (`X-MOZ-*`, `X-APPLE-*`) | No; always rebased and never sent upstream |
   | `reply` | Only the user's own `PARTSTAT` and `RSVP` (per `RECURRENCE-ID`), plus what RFC 6638 §3.2.2.1 lets an attendee change. The attendee is matched against the editing user's addresses | Yes |
   | `edit`, `create`, `delete` | Anything else | Yes |

3. **Rebase:** re-applies a change's patch on top of a newer upstream version.
4. **Field diff** of base, mine and theirs, for the conflict view.

The store contract gains `record_change` (an atomic compare-and-set against the ETag the client saw), `effective`, `open_changes`, `changes_of` and `resolve_change`, with conformance cases. The store only flags an open change when a newer upstream version lands or the item disappears upstream. That is a version-number check. Rebasing and convergence run in pim-sync's post-batch step through `pim-edit`, because the store must not depend on `holtmail-pim`.

### What is served

A held change is served as **its patch re-applied on top of the latest upstream version**, not as frozen client bytes. So a meeting the organizer moves or cancels still moves or disappears on the user's devices, and the user's own change stays on top of it.

- **Href:** a created item keeps the client's href (`items.dav_name`) forever, even after the upstream id is known. Changing it would make the device delete and re-create the item, with duplicates and lost alarms.
- **ETag:** strong, from BLAKE3 over the served bytes.
  - A PUT returns it only while the served bytes equal the client's bytes.
  - After a rebase there is no ETag, and clients refetch (RFC 4791 §5.3.4).
  - Graph and JMAP items converted to iCalendar or vCard need a deterministic converter, which a conformance case checks.
- **CTag and sync-token:** move with every served change, from the `changes` log.

| Request | Answer |
| --- | --- |
| `If-None-Match: *` on a free href | 201: a held create |
| `If-None-Match: *` on a taken href | 412 |
| `If-Match` equal to the served ETag, or no precondition | 204: a held change (the last writer wins locally; the superseded row stays in the history) |
| `If-Match` different from the served ETag | 412. This is the only reason for a 412: a mismatch with Holtmail's own current version, never upstream state |
| Malformed data, wrong component, UID changed or taken, too large | 403 with the RFC precondition element (`valid-calendar-data`, `no-uid-conflict`, `max-resource-size`), or 415 |
| Store unavailable | 503 with `Retry-After`; this is the only 5xx |
| DELETE | 204: a held delete. The href then answers 404, and `sync-collection` reports it removed |

An item removed upstream while a change is open stays served, never answering 404 or 412. That avoids DAVx5's 412 loop on deleted resources.

### When upstream changes the same item

pim-sync records the upstream version as it does now, and the post-batch step decides what happens to the open change:

| Case | What happens |
| --- | --- |
| The upstream version now matches the change (the same edit was also made upstream) | `applied` |
| `noop`, `personal` or `reply` | Rebased silently. A reply whose event was cancelled, or whose attendee was removed, becomes `conflict/reply_obsolete` |
| An edit whose fields do not overlap the upstream change | Rebased silently |
| Overlapping fields | Upstream wins for `DTSTART`, `DTEND`, `RRULE`, `STATUS:CANCELLED` and the attendee list. The change becomes `conflict` and keeps the user's version for "Keep mine" |
| Item deleted upstream | `conflict/upstream_deleted`; still served until the user decides |

The user resolves conflicts in the web UI:

- **Keep mine:** a new change on the latest base. With write-through it overwrites upstream, or re-creates the item there.
- **Take theirs:** discards the change. Devices see a new ETag and refetch.
- **Discard:** the same, from the open-changes list.

Nothing is dropped silently. Every terminal state except `applied` and `superseded` needs a user action.

### Invitations and RSVP

**Phase 51: replies are held.** Holtmail does not advertise `calendar-auto-schedule`, because advertising it obliges the server to deliver replies.

- **Thunderbird and Evolution** send the iMIP REPLY themselves through the user's mail account. The sent reply reaches the archive through Sent and updates the invitation's status. Holtmail sees that the held reply matches it (same UID, recurrence and answer) and marks it `applied`, sent by the client.
- **Apple and DAVx5** PUT the changed `PARTSTAT` to the attendee's copy in an upstream calendar, and it is held as a `reply`.
  - It is linked to the invitation: the UID and the event series lead to the newest REQUEST message, whose `Message-ID` is kept for the later reply.
  - The device gets the held RSVP notice.
  - The web invitation card says: "You answered **Accepted** on Anna's iPhone. The organizer has not been told yet."
- **Upstream calendars that already contain the invitation** (Google, Exchange, Fastmail) are where these answers happen, since "From mail" is read-only.

**Phase 54: replies are sent.** Each reply goes by the route its collection supports:

1. **CalDAV upstreams with RFC 6638** (iCloud, Fastmail, Nextcloud): a write-through PUT of the attendee copy. The upstream server then sends the reply.
2. **Graph:** `POST /events/{id}/accept|tentativelyAccept|decline` with `sendResponse: true`.
3. **JMAP:** a `participationStatus` patch with `sendSchedulingMessages: true`.
4. **"From mail", or a source with no scheduling:** an iMIP REPLY through `submit`, from the matched account address, with `In-Reply-To` set to the invitation. It respects `SCHEDULE-AGENT=CLIENT`.

"From mail" becomes writable for replies at this point. `calendar-auto-schedule` is advertised only when every writable calendar of the principal has a reply route **and** the organizer path exists: REQUEST and CANCEL for events the user organizes, `Schedule-Tag` with `If-Schedule-Tag-Match`, and the schedule inbox and outbox. Until then Apple may hide adding invitees.

### What the user sees

- **Marks.** A "Local edit" mark (hairline, square, as in [design.md](design.md)) appears on events, contacts and invitation cards whose served version comes from a change. It reads:
  - "not sent to <account>" while the change is held;
  - "sending";
  - "changed in <account> too" on a conflict;
  - "<account> refused".
- **Timeline.** The item's version history interleaves changes with upstream versions. Each change opens as raw bytes or as a diff against its base.
- **Settings → Contacts and calendars → Edits:**
  - the mode per collection;
  - open changes grouped by collection, with Keep mine, Take theirs and Discard (plus Retry once write-through exists);
  - counts in the Settings navigation.
- **Notices:**
  - the first held edit in a collection: "Edits from your devices stay in Holtmail until sending to <account> is turned on";
  - conflicts and refusals (low urgency);
  - the held RSVP notice.
- **API:** `GET /api/pim/changes?state=` and `POST /api/pim/changes/{id}/resolve`. Editing from the web later writes the same changes, with origin `api`.

### Limits

- **A held edit or reply does not exist upstream** until write-through sends it:
  - organizers are not told about answers given on Apple or Android devices until Phase 54;
  - colleagues see no change to a shared calendar.
- **Devices and apps that sync directly with the upstream** (Google Calendar on the same phone, Outlook) do not see held edits. A device should use either Holtmail or the upstream account, not both, to avoid duplicates.
- **Without server scheduling,** events organized on Apple devices send no invitations.
- **Graph and JMAP collections are edited as iCalendar or vCard.** Write-through sends them as a field patch against the base, never as a full replace. Alarms, categories and photos the client never saw are therefore not lost.
- **Recurring series:** editing one occurrence is a PUT of the whole resource. It is classified and rebased as a whole.

### Write-through (Phase 53)

The `CollectionWriter` trait lives in `holtmail-collection`, separate from `CollectionSource`. It is object-safe, uses `#[async_trait]`, and ships a fake and a conformance suite:

```rust
#[async_trait]
pub trait CollectionWriter: Send + Sync {
    fn capabilities(&self, collection: &str) -> WriteCaps;
    async fn put(&self, collection: &str, id: Option<&str>, change: &ItemPatch,
                 precondition: Precondition, idempotency: &str) -> Result<Written, CollectionError>;
    async fn delete(&self, collection: &str, id: &str, if_match: Option<&str>) -> Result<(), CollectionError>;
    async fn respond(&self, collection: &str, id: &str, recurrence_id: Option<&str>,
                     answer: Partstat, notify_organizer: bool) -> Result<Written, CollectionError>;
}
```

- **Implementations, in order:**
  1. `collection-dav`, where the bytes pass through;
  2. `collection-jmap`, with `ifInState` and a per-item compare;
  3. `collection-graph`, as a PATCH of changed fields.

  `collection-merge` routes writes the same way it routes reads.
- **`pim-writeback`, the drain loop:**
  - it runs first in, first out per collection, with one change in flight per item, and only the head of a supersede chain is sent;
  - creates are idempotent: a deterministic href with `If-None-Match: *` on DAV, `transactionId` on Graph, a lookup by UID on JMAP;
  - on success it records the upstream state as a `writeback` version and marks the change `applied`;
  - a 412 from upstream records upstream as a version, then rebases and retries a reply once; anything else becomes a conflict;
  - a refusal marks the change `rejected` and notifies the user; the change is still served;
  - an `Auth` failure pauses the account, as `needs_sign_in` does today;
  - network errors back off for up to 24 h. A change is never dropped.
- **Gate:** `apps/server` hands out a writer only for `write_through` collections on accounts with the per-account write switch and the `write:pim` scope.

## Offline subset

**What is kept** is set per device:

| Setting | Default |
| --- | --- |
| Desktop | off |
| Mobile | Inbox of the last 30 days, with bodies, without attachments; people; calendar −30/+180 days; Wi-Fi only |

Custom settings cover archives and folders, the number of days, attachments up to a size, and a space budget with least-recently-used eviction. Pinned threads are always kept.

**How it works: the shell runs the real API locally**, so there is no second backend.

- **The embedded API.** The shell embeds `holtmail-api` over `store-sqlite`, `blob-fs` behind `blob-encrypted`, and `index-tantivy`.
  - The offline origin is `holtmail://<profile>/`, and `http://holtmail.localhost/` on Windows and Android.
  - It hands each request to the axum router in process, with no TCP port.
- **The pull.** `replica` pulls `GET /api/archives/{id}/changes?since=` from the `changes` log; a `410` means start over. It stores **raw `.eml` bytes** plus derived rows, so bodies, inline images and search behave as they do on the server.
- **The SPA cache.** When `/api/meta` reports a new build, the core downloads the files listed in `/app/manifest.json`. Offline, it navigates to the offline origin; when the server is reachable again, it navigates back.
- **What does not work offline.** Routes the local API cannot answer return `503 offline`, which the SPA shows as an offline empty state. Writes return `501` until the outbox exists.
- **Security.** The blob key lives in the keystore, and a wipe or a revocation deletes the key first. Everything else relies on OS data protection. Biometric unlock is an option on mobile.
- **When it pulls.** On every wake-up, app start and network regain. Android adds WorkManager, and iOS best-effort background refresh.

The spike checks that `aws-lc-rs`, `sqlx` and `tantivy` build for iOS and Android. If they do not, offline falls back to bodies only, without search.

## Write later

These names are reserved now, so that the write phases fit without reshaping anything:

- **`holtmail-submit`** is the contract for outbound mail, with `submit-smtp`, `submit-jmap` and `submit-graph`. `crates/smtp` stays inbound only.
- **`Source::set_flags` and `Source::append`** (to Sent) are optional methods with an `Unsupported` default.
- **Opt-in.** Every upstream write needs a scope (`write:mail`, `write:pim` or `write:contacts`) and a per-account switch. "Read-only towards the mail server" stays the default.
- **API.** Plain REST mutations through `api-client`, with an `Idempotency-Key` table.
- **Client.** An outbox in the local store, replayed in order through the same embedded API.
- **Bridge.** The capabilities `outbox` and `write.mail`, and the notification categories Reply and RSVP.

## Security

- **SPA headers:**

  ```
  default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
  img-src 'self' data: blob:; frame-src 'self'; object-src 'none'; base-uri 'self';
  connect-src 'self' ipc: http://ipc.localhost; frame-ancestors 'none'
  ```

  Plus `Referrer-Policy: no-referrer` and `X-Content-Type-Options`. The inline theme script in `index.html` moves to a file.
- **IPC:**
  - only one origin, the `main` webview and the `plugin:holtmail|*` commands; no fs, shell, http or generic opener;
  - every command checks the webview label and the current origin;
  - through the bridge an XSS can at most show a notification, set the badge, open a checked `https:`, `mailto:` or `webcal:` URL, change device preferences, share or download.
- **Body iframe:** its sandbox already lacks `allow-scripts`, and a test keeps it that way. A scripted same-origin iframe would reach IPC on Linux and Android.
- **Navigation:** only the profile origin stays in the app.
  - Other origins, `target=_blank` and new windows open in the system browser.
  - `mailto:` and `webcal:` go to the OS.
- **DAV:**
  - app passwords are per device and revocable;
  - write privileges are limited by role and scope;
  - every change records its user and device.
- **Logs:** device tokens, app passwords, enrolment codes, push payloads and DAV bodies are never logged; only ids and counts are.

## Distribution

| Platform | Now (no paid accounts) | Later |
| --- | --- | --- |
| Linux | Shell on Flathub (the GNOME runtime brings WebKitGTK), .deb, .rpm, AppImage; PWA | — |
| macOS | PWA. Unsigned shell on GitHub Releases, with the Gatekeeper "Open Anyway" step documented; Homebrew has refused casks that fail Gatekeeper since 2026-09-01, so there is no cask | Developer ID, notarisation, Homebrew cask |
| Windows | PWA. Unsigned NSIS shell on Releases, with the SmartScreen warning documented | Signing (Azure Trusted Signing or an OV certificate), winget |
| Android | PWA, plus DAVx5 for contacts and calendars | Native app: `fdroid` flavor with UnifiedPush on IzzyOnDroid and F-Droid, then `play` with FCM |
| iOS | Home Screen PWA, plus the `.mobileconfig` for Calendar and Contacts | Native app with the NSE through autopush; App Store and TestFlight, with a demo server holding invented `example.com` data |

**Costs.** Nothing now. Later:

- Apple Developer Program, $99 a year, for native iOS, notarisation and APNs;
- Google Play or Android developer verification, $25;
- Windows signing;
- a small host for autopush and Valkey.

**CI**

- `mise run check` stays network-free and webview-free. It gains the client crates and `@holtmail/host/testing`:
  - `./web` under jsdom;
  - `./tauri` over a mocked `invoke`;
  - N−1 protocol and server version skew.
- `shell-check` runs on Ubuntu with WebKitGTK, and only when `apps/shell`, `packages/host` or the client crates change.
- Release builds run on `shell-v*` tags and a weekly schedule, because macOS runners are expensive.
- Later, the NSE crypto becomes a SwiftPM package on the swift-crypto API, tested on Linux with the RFC 8291 Appendix A vector.
- New mise tasks: `shell-dev`, `shell-build`, and later `shell-android` and `shell-ios`.

## Localisation and accessibility

- Native strings (launcher, tray, menu, notification texts, later the NSE fallback and the Android channels) come from one JSON file next to `apps/web/src/strings.ts`. That file is generated into the platform formats.
- VERIFY covers each engine with VoiceOver, Narrator, TalkBack and Orca.
- Native menus, the tray and notification actions carry text labels.
- The shell honours reduced motion and the system text size.

## Risks

1. **Tauri mobile maturity.** Native mobile comes later, the spike checks it first, the native pieces do not depend on Tauri, and the bridge is a contract. The fallback is Capacitor for mobile.
2. **A runtime remote capability or the Android init script fails somewhere.** Then use a wildcard remote capability with an origin check in every command. As a last resort, always load the cached SPA from the offline origin, which needs CORS and bearer requests from the SPA.
3. **The iOS PWA is the only iOS client for now.** It must show a notification for every push, and WebKit sometimes revokes subscriptions. The PWA re-registers on every open.
4. **Held edits diverge from upstream.** Mitigated by:
   - rebasing on every upstream change;
   - the marks and the first-edit notice;
   - read-only "From mail";
   - the held RSVP notice;
   - conflicts that never drop an edit.
5. **autopush's Redis backend is new.** The release is pinned, the volume is low, and the relay is replaceable.
6. **Three webview engines**, of which WebKitGTK is the weakest (NVIDIA DMABUF). Each is checked by eye in VERIFY.
7. **Notification storms from imports.** Prevented by the `live` flag and coalescing, and checked by an integration test where a 10k backfill sends no push.
8. **Windows has no system contacts or calendar open to third parties.** Accepted: ICS, Thunderbird and in-app reminders.

## Open questions

All of these go into VERIFY.md when their phase lands.

1. How Apple Calendar handles a 412, and whether iOS shows RSVP buttons on a CalDAV account without scheduling.
2. GNOME Online Accounts: can a WebDAV account be prefilled over D-Bus under Flatpak?
3. Android webview profiles: are they reliable enough for several servers on one phone?
4. Does Safari's push service honour the Web Push `Topic` header for collapsing?
5. Does autopush's APNs router set `mutable-content`, which the NSE needs?
