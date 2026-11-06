# Conversations: a messenger view of the archive

Status: design, decided. Nothing here is built yet. The email alias work it builds on (phases 32–34: crates `alias`, `alias-*` and `aliases`, migration `0019_aliases.sql`, [ALIASES.md](ALIASES.md)) is committed, so the work starts with [Phase 0](#phase-0-prerequisites). The maintainer's decisions are recorded in [§16](#16-decisions).

Holtmail reads mail like a mail client. This document adds a second way to read the same archive: **Chats**, a view that looks and behaves like a messenger (Telegram is the reference). It has direct chats with a person, group chats, and forum chats whose topics are mail threads. Bubbles, albums, voice messages, reactions and read marks replace headers and quoted text.

The mail stays the truth. Every chat, topic, bubble and mark is derived data, built from the raw bytes, people, aliases and the owner's addresses. Any of it can be dropped and rebuilt.

## 1. Goals and non-goals

**Goals**

- A chat list, a chat, forum topics and an "All messages" stream that feel like a real messenger. That means bubbles (mine right, theirs left), day separators, reply previews that jump to the parent, albums, video, voice, files, stickers and GIFs, large emoji, event and contact cards, reactions, read and failure marks, forwarded bubbles, unread counts and pinned messages.
- Correct grouping. The same mail imported in any order, by any mix of IMAP, JMAP and file imports, gives the same chats, topics, ids and order.
- Correct identity. The owner's accounts and masked aliases count as "me", and a message is shown as mine only with evidence that I sent it. Reverse aliases map back to the real correspondent, but only when that mapping can be trusted.
- No lost content. Every heuristic fails toward showing more, and "Show original" is always one click away.
- Fast on large archives. Chat lists and streams are SQL keyset reads, with no blob reads or parsing per request.
- Safe. No email HTML enters the app DOM, remote content stays blocked, and logs never carry mail.

**Non-goals**

- **Sending.** Holtmail is an archiver, so there is no compose box, no reply and no reactions written back. Chats never change flags on the server; read state is shown, not set.
- **Replacing the mail reader.** Chats is a second view. The mail reader stays the place for full headers, raw source and every byte.
- **Changing threading.** Store threads (`References`/`In-Reply-To`) keep their semantics. Chats groups threads into topics on top of them.
- **Outlook reactions, recalls and subject-fallback threading in the first release.** They come in Phase 4 (§15): Outlook reactions only through `source-graph` (§7.2), the rest behind detectors proven by fixtures.

## 2. Vocabulary

| Term | Meaning |
|---|---|
| **Party** | One correspondent identity after alias and person resolution. Identified by a party key (§4.4). |
| **Me** | The owner's addresses: accounts, Sent senders and aliases (§4.1). |
| **Evidence** | Proof that I really sent one message: a Sent copy, an outgoing SMTP route, or an aligned DKIM pass (§4.2). A message from an own address without it is a **claim**, never mine. |
| **Chat** | A conversation with a fixed set of parties: direct, group, broadcast, list, notifications or saved (§3.2). |
| **Topic** | One conversation thread inside a chat. Usually one store thread; several when they share a root (§3.4). |
| **General** | The topic that collects one-message threads with no references, as in Telegram forums. |
| **Forum** | A chat shown as a list of topics instead of a single stream (§3.5). |
| **All messages** | The stream of every bubble of a chat across its topics, oldest at the top. |
| **Item** | One row in a stream: a bubble or a system line. In code it is always `ChatItem`, because "item" already means a contact or event (`holtmail_store::ItemQuery`, `/api/items/{id}`). |
| **Mark** | A status attached to a bubble rather than shown as one: reaction, read, delivered, delayed, failed. |
| **Facts** | What a single message is, derived only from its bytes (§5). |
| **Placement** | Where a message sits: chat, topic, author, direction, order. Derived from facts, threads, people and identity (§3, §4). |

## 3. Conversation model

### 3.1 The unit of placement is the thread

A message is never placed on its own. A **thread** (a store thread, or several threads sharing a root, §3.4) is placed as a whole into one chat. So:

- a thread never splits across chats, with one exception: off-list private replies (§3.7);
- a cc that comes and goes inside a thread does not fragment it;
- a thread merge only moves rows; it never leaves half of a conversation behind.

### 3.2 Chat kinds

Decided per thread, first match wins. "Parties" means the key parties of §3.3.

| # | Thread | Kind | Chat key | Shown as |
|---|---|---|---|---|
| 0 | No conversational message at all: only reactions, read receipts, delivery reports or RSVPs (a DSN from `mailer-daemon` usually has no `References`, so it is a thread of its own) | none | — | The thread gets no chat. Its marks attach to their targets (§7.5); a mark whose target is not in the archive becomes a system line in the `notifications` chat of its sender (`n:mailer-daemon@…`), never in Saved messages. |
| 1 | Any conversational message carries `List-Id` and was sent to the list | `list` | `l:{list-id}` | A channel. Every bubble shows its author. Posts from me are mine. Shown as `channel` in the list when I never posted, otherwise as a group with a list glyph. |
| 2 | Every non-own sender is automated (§4.5), and I never wrote in the thread | `notifications` | `n:{sender address}` | One bot chat per sender address, never per domain. All of them sit behind one collapsed "Notifications" row, like Telegram's archive folder. Inside that row the bot chats are grouped by sending domain; the grouping is a view only and never part of a key. |
| 3 | Parties are empty and at least one conversational message is a claim (`From` or `Sender` own without evidence, §4.2) | `notifications` | `n:{claimed address}` | Titled "Claims to be {address}", with the warning glyph. A forged "from yourself" message never lands in Saved messages. |
| 4 | Parties are empty and Bcc is empty | `saved` | `saved` | "Saved messages": notes to self and mail between my own addresses. One per archive. |
| 5 | Parties are empty and Bcc is not (I wrote to myself and Bcc'd others) | by Bcc count | the Bcc set, as in rows 6–8 | Treated like a normal send to the Bcc set. |
| 6 | One party | `direct` | `d:{email}` (the party key without its `e:`) | A chat with a person. |
| 7 | 2 to `chats_group_limit` parties (default 25) | `group` | `g:{sorted party emails}` | A group chat. |
| 8 | More than `chats_group_limit` parties | `broadcast` | `g:{sorted party emails}` | Titled "To N people" plus the root subject. |

**Group or broadcast.** `chats_group_limit` is an archive setting (`Archive::chats_group_limit`, default 25, changed through `PATCH /api/archives/{id}` and Settings → Archives). The difference between the two kinds is only how the chat renders: a broadcast is titled "To N people", shows no membership lines (§7.7) and shows the author on every bubble; a group shows "Ada added Bob" lines and the author once per cluster (§11.3). Both kinds share the `g:` key, so changing the limit never changes a chat id or the viewer's forum choice (§3.5); it changes `kind`, and saving a new value requests a chats rebuild (§8.6).

"Mine", "I wrote" and "I reply" in this section always mean a message with evidence (§4.2). When I reply to a thread from an automated sender, it stops being `notifications` and becomes `direct` (a human answered). An automated author never becomes a party when the thread has a human to talk to: if its message carries a `Reply-To` that is not automated and not own (contact forms, ticket systems, "via" relays), that `Reply-To` address stands in for it as the party. Otherwise my reply to `noreply@shop.example.com` would open a "direct chat" with a noreply address.

A thread whose messages have no usable `From` (malformed, or only an undisclosed group) and no other party goes to `n:unknown`, shown as "Unknown sender" in Notifications. It is never dropped.

An auto-reply (`Auto-Submitted: auto-replied`, out of office) is not automated in this sense. It is a muted bubble from that person in their chat.

### 3.3 Who is in a chat (CC churn)

Taking the union of every address in the thread moves a two-year chat with Ada into a group the moment Bob is cc'd once. Instead:

**Key parties of a thread** = (authors of its conversational messages ∪ recipients of its root message) − me.

- **Conversational** means a message whose role is `message` or `auto` (§5.1). Reactions, read receipts, delivery reports and their senders (`mailer-daemon@…`) never become parties.
- **Root message** is the earliest message we hold for the thread's root (§3.4). If the root itself is not in the archive, the earliest message we hold stands in for it.
- **Bcc** counts only in row 5 of §3.2. A Bcc copy that reached me never makes a stranger a party.
- **Bcc-only received copies.** When I am Bcc'd on a message between Ada and Bob (none of my addresses is in `To` or `Cc`), the thread is placed by its visible parties as usual, here in the {Ada, Bob} group. The bubble carries a "Bcc'd to you" marker, so a group I never "joined" explains itself. The marker is decided at placement (theirs, not a list post, no own address among the visible recipients) and stored on the `chat_messages` row.
- Someone who was only cc'd later and never wrote is **not** part of the key. They appear as a system line, "Ada added Bob", on the first message that includes them (§7.7).
- Someone who was cc'd later and *did* write is an author, so the thread moves to the group with them. That is a real group conversation.

To keep this understandable, a direct chat's header shows "Also in N groups", and the person pane lists every chat with that person.

### 3.4 Topics come from threads, with stable ids

Store `ThreadId`s are not stable. `insert_message` merges threads with one `UPDATE messages SET thread_id = …` into the smallest id; `ThreadId`s are UUIDv7 made at insert time, so the surviving id depends on import order (the contract promises only that *membership* does not). A message joins a thread when its `Message-ID` is a parent of a stored message or they share a parent id in `message_parents`. Byte-identical copies are already one `messages` row (`UNIQUE (archive_id, blob_id)`) with several locations; but a copy with different bytes (another account's `Received:` lines) and no `References` starts a separate thread until a reply links them. Topics therefore get their own identity.

- **Message root** (bytes only, in facts): `References[0]`, else `In-Reply-To`, else the message's own `Message-ID`, else `blob:{blob_id}`. RFC 5322 puts the thread root first in `References`, so the root is known even before the root message is imported.
- **Thread root** = the message root cited by the most messages in the thread. Ties go to the lexicographically smallest.
- **Topic** = every thread with the same thread root. Two copies of one root from two accounts land in one topic. A late reply that merges two store threads yields one topic, because both threads already pointed at the same root.
- **Topic id** = UUIDv8 of `blake3("topic" ‖ archive_id ‖ thread root ‖ split tag)`. The split tag is empty except for off-list replies (§3.7). It does not change on ordinary replies, merges or import order.
- **Title** = the normalized subject of the root message (or the earliest held one). Normalization strips repeated `Re:`, `RE[2]:`, `Fwd:`, `FW:`, `AW:`, `WG:`, `SV:`, `VS:`, `Odp:`, `Ответ:`, `Отв:`, `Пересл:`, `回复:`, `答复:`, `转发:`, `[list-tag]` and `*SPAM*`. An empty result falls back to the first 60 characters of the clean text.

A topic id can still disappear, for example when a thread later turns out to have an older root. Every chat URL also carries `m=<message id>`, and `GET /api/messages/{id}/chat` resolves a stale `c=`/`t=` to the current pair (§10.1).

### 3.5 Forum or plain chat

People who start a new subject each time would turn every direct chat into a forum of one-message topics. So:

- One-message threads **without** `References`/`In-Reply-To` go into the chat's **General** topic. The subject renders as a bold first line of the bubble. "One message" counts canonical conversational bubbles (§7.4): a Sent copy plus a received copy, or a message plus a reaction to it, is still one message. When such a thread later gets a reply, it leaves General and becomes its own topic.
- The General topic id is UUIDv8 of `blake3("topic" ‖ archive_id ‖ "general" ‖ chat id)`, so it is stable for as long as the chat is.
- A chat is a **forum** when it has at least 2 non-General topics with at least 2 messages each. Otherwise it opens as one stream with topic chips.
- The viewer can flip forum mode per chat. The choice is stored on the server per user, keyed by the chat key string (`d:ada@example.com`, `g:ada@example.com,bob@example.org`), not by the chat UUID, in `chat_prefs` (§8.2). It is user state, not derived data: a chats rebuild keeps it, and it follows the viewer across devices. A key that changes (a person join that changes the canonical email) loses the choice, which then falls back to the computed default.
- **All messages** is always available: the first row of the topic list in a forum, and the default view otherwise. A topic chip (mono, muted, hairline) appears above a bubble whose topic differs from the previous bubble's. Clicking it opens the topic.

### 3.6 The same person in many threads

- Every thread whose key parties are exactly {Ada} is a topic of the one direct chat with Ada. That chat becomes a forum once it has enough real topics (§3.5).
- Threads with Ada and others live in their group chats. The direct chat shows "Also in N groups", and the person pane lists them ("Groups in common").
- Ada writing through a masking service lands in the same chat as Ada writing directly, as long as the reverse alias is trusted (§4.3).

### 3.7 Mailing lists

- **List posts** go to the list chat, never into a group of hundreds.
- **Author on a list post.** Lists that rewrite `From` for DMARC (`"Ada via dev" <dev@lists.example.org>`) would make the list the author of every bubble. When `From` equals the list's post address, the author comes from `X-Original-From`, else from `Reply-To`.
- **Off-list private reply.** Ada answers a list post privately: `In-Reply-To` points at the post, there is no `List-Id`, and the list address is not among the recipients. That message is placed in the direct chat with Ada, as a topic marked "Reply to a post in dev" that links to the post. It is the one allowed split of a thread. The topic's split tag is `private:{chat key}`.

### 3.8 What is excluded

- **Drafts.** A message is a draft when every location it ever had (live or removed) is in a folder with role `drafts`, or carries the `\Draft` flag, or the message has the Gmail label `Drafts` and no location outside `drafts`/`all`. "Ever had" matters: when a draft is sent, the server deletes it from Drafts, its location gets `removed_at`, and a rule based on live locations only would turn the stale draft into a duplicate bubble next to the sent copy.
- **Junk.** A message whose every location (live or removed) is in a folder with role `junk`, or that carries only the Gmail label `Spam`. Spoofed junk would otherwise land in a real person's chat.
- File imports and SMTP ingestion also record locations: in folders of an account of kind `import` (`apps/server/src/import.rs` names folders after the file's folders with `FolderRole::guess`, so an mbox `Drafts` or `Spam` folder gets that role; the SMTP listener uses a role-less folder of the "Received by SMTP" account). The same rules apply to them. A message with no location at all is never excluded.
- Trash is kept: this is an archive.
- Moving a message out of Junk adds a location, and a label change rewrites `message_labels`. Both mark the thread dirty (§8.4), so it is placed again.

## 4. Identity

### 4.1 Who is "me"

There is one definition of "me", shared with People: `PeopleStore::mail_addresses(archive).own`. Since the alias commit (`crates/store-sqlite/src/people.rs`) it is the union of `accounts.address`, the `From` (read through the `resolved_addresses` view) of messages located in a folder with role `sent` or carrying the label `Sent`, and every address in `aliases` plus every `message_aliases.alias` (the same set as `AliasStore::own_alias_addresses`). The doc comment on `MailAddresses::own` in `crates/store/src/people.rs` still names only accounts and Sent senders; Phase 0 adds aliases to it. Chats reads that set and never builds a second one.

The own set says which addresses are mine. Whether one message is mine is a second question, answered by evidence (§4.2): an own address in `From` proves nothing on its own.

Phase 0 makes two changes to the committed alias code before Chats relies on it:

- An alias address seen only in mail counts as own only when trusted (§4.3). An unsigned forwarding header must not add a stranger to "me".
- Each own address carries a **source** and a **strength**:

| Source | Strength |
|---|---|
| Account address | strong |
| Configured alias (`aliases` row, from a service directory or added by hand) | strong |
| Alias seen in mail with a trusted hit | strong |
| `From` of a message in Sent only | weak |
| Own-address override set to `own: true` | strong |
| Inferred owner (below), only when the set is otherwise empty | weak, always `uncertain` |

A weak address that is also the `From` of many messages found only in the Inbox is flagged as **uncertain**. This is how a shared mailbox, or an import whose Sent folder holds someone else's mail, shows up. `GET /api/archives/{id}/identities` lists every own address with its source and strength.

**Own-address override.** `PeopleStore` stores overrides `(email, own: bool)` per archive, like `PersonOverride`: hand-made state that survives every rebuild, so People and Chats keep sharing one definition. `own: true` adds an address, `own: false` removes one that a weak source added. Phase 1 ships it in minimal form, enough to confirm the inferred owner or choose another address (`POST /api/archives/{id}/identities` with `{email, own}`); the identities page that lists and edits every override comes in Phase 4.

**When no own address is known** (an import-only archive with no account address, no Sent folder, no aliases and no override), grouping does **not** still work on its own: "parties − me" would keep the owner's address in every party set, so every direct chat with Ada would become a group {Ada, owner}, and all chat ids would change the day the owner becomes known. So the resolver infers an owner, carefully:

- **Sample.** Only conversational messages count (role `message` or `auto`); list posts, automated mail (§4.5) and marks are left out. With fewer than 20 of them, nothing is inferred.
- **Reply pairs first.** An address that is a recipient of message X and the `From` of a reply to X (its `reply_rfc_id` is X's `Message-ID`) is strongly owner-like: the owner receives mail and answers it. Candidates are ranked by reply-pair count when any address has reply pairs, otherwise by how many sampled messages list them as a recipient.
- **Margin.** The top candidate must be a recipient (`To`/`Cc`) of at least 50% of the sample **and** have at least 3× the count of the second candidate. Two near-equal candidates (a couple sharing an import) give no inference.
- **Hysteresis.** Once inferred, the address is kept in `chat_state.inferred_owner`, which a rebuild keeps, and stays the owner until its margin over the second candidate falls below 2×. So it never flips back and forth as mail arrives. Only an unconfirmed inference is kept this way; a confirmed one is an override.
- **Always `uncertain`.** It is listed with source `inferred` on the identities endpoint, and the chat list shows a banner "Is this you? Confirm / choose another". Confirming stores an `own: true` override; choosing another stores the chosen one and an `own: false` override for the inferred address. People never sees an unconfirmed inference; it applies to Chats only.
- **Change.** A change of the inferred owner (new, dropped or replaced) requests a full chats rebuild (§8.6) and is logged with the archive id and counts only, never the address.
- If no address passes, every bubble renders as theirs and parties keep every address; the chat list shows a banner that explains this and offers the same "choose an address" action.

### 4.2 Direction of a message

A message counts as mine only with **evidence** that I actually sent it. Any one of these is evidence:

1. **A Sent copy.** The message (any copy of the same `messages` row) has a location, live or removed, in a folder with role `sent` of an account of this archive, or carries the Gmail label `Sent`.
2. **The outgoing SMTP path.** It arrived through an `[[smtp.route]]` that the owner marked as their outgoing journal or submission copy (Postfix `sender_bcc_maps`, an outbound Exchange journal rule). Today a route has no such marker: every route stores whatever is delivered to it, and anyone can deliver to it, so a plain route is **not** evidence. Phase 1 adds `sent = true` to `RouteConfig`, which gives the route's folder in the "Received by SMTP" account the `sent` role; rule 1 then covers it, and the Sent mark of §7.3 follows. An Exchange journal report that arrives through a route marked `sent = true` also counts when the sender in its envelope is own; the listener keeps that fact before `journal_original` discards the report.
3. **An aligned DKIM pass.** The topmost `Authentication-Results`, read the same way as `holtmail_alias::Mail::dkim_pass`, records `dkim=pass` with a `header.d` equal to the own address's domain or a subdomain of it. This counts only on a copy located in an `imap`, `jmap` or `graph` account of the archive, because only there was the topmost field written by the owner's own provider; in a file import or an SMTP copy anyone could have written it. The domain must also be trusted: either a large shared provider in a fixed list in `holtmail-chats` (`gmail.com`, `googlemail.com`, `outlook.com`, `hotmail.com`, `live.com`, `icloud.com`, `me.com`, `yahoo.com`, `fastmail.com`, `proton.me`, `protonmail.com`, `yandex.ru`, `mail.ru`), whose DKIM key only signs mail from authenticated senders of that address, or the domain of a strong own address (§4.1: an account address or a configured alias). The domain of a weak own address never counts, because a small shared domain cannot be told apart from the owner's own. It covers the received copy of my own message (I cc'd myself, or another of my accounts received it).

The facts store what the bytes say (`dkim_pass`: the passing `header.d` values of the topmost field); locations and the own set are applied at placement, so a new Sent copy or a new own address re-places the thread through the dirty triggers (§8.4).

| Case | Direction | Author shown |
|---|---|---|
| `From` resolves to an own address (account or alias), with evidence | mine | me |
| `From` is not own, `Sender` is own (I sent on someone's behalf), with evidence | mine | "me, on behalf of {From}" |
| `From` and all visible recipients own, with evidence | mine, in Saved messages | me |
| `From` own, no evidence, the own address is strong (§4.1) | theirs: an **unconfirmed** copy | the `From` address with "Not confirmed as sent by you" and an info glyph, not the warning glyph: most often it is my own message sent from an account that is not in the archive |
| `From` own, no evidence, the own address is weak or the claim fails a check above | theirs: a **claim** | the `From` address with "claims to be you" and the warning glyph |
| `From` not own, `Sender` own, no evidence | theirs: a **claim** | the resolved `From` party, with "claims to be sent by you" and the warning glyph |
| Otherwise | theirs | the resolved `From` party |

A claim is never mine, never makes a thread "mine" for §3.2, can be unread like any bubble of theirs (§9.2), and never collapses with my real copy except under the narrow content rules of §7.4, which make it invisible. A thread whose only would-be parties are claims goes to `n:{claimed address}` (§3.2 row 3), never to Saved messages. The warning glyph is `TriangleAlertIcon` in the `warning` token; its tooltip says that the message uses my address but nothing shows that I sent it.

### 4.3 Masked aliases and reverse aliases

The committed alias work stores at most one `message_aliases` row per message (migration `0019_aliases.sql`): `provider`, `alias`, `sender_email`, `sender_name`, `reverse_alias`, `signed`, indexed by `(archive_id, alias)` and `(archive_id, reverse_alias)`. Through the contract it is `MessageAlias { provider, alias, sender: Option<Address>, reverse_alias, signed }`, read with `AliasStore::message_alias`. Detection runs only on the received copy; my own reply sent to a reverse alias has no row, so it is resolved through the reverse-alias → sender map built from other messages. Chats uses it like this.

**A message from me through an alias** (`From` is my alias, `ada@sl.example.com`) is mine, because the alias is in the own set.

**A message to me through an alias.** A forwarding service delivers Ada's mail with `From` = a reverse alias (`ada_at_example_org_xk2@sl.example.com`). The real sender is in `MessageAlias.sender` (`sender_email`, `sender_name` in SQLite). The alias itself is among the recipients and is own, so it is dropped from the parties.

**A reply I sent through the service** has `To` = the reverse alias. It resolves through `message_aliases` to the same person.

A reverse-alias mapping is **trusted** when any of these holds:

1. the hit is signed (`message_aliases.signed`: the topmost `Authentication-Results` records a passing DKIM signature of the provider's domain, as `docs/ALIASES.md` defines it);
2. the same reverse alias → sender mapping was seen signed at least once in the archive;
3. the reverse alias → sender mapping is listed by a configured service's reverse-alias directory (`alias_contacts`, below).

Old and imported mail often has no usable `Authentication-Results`, so rule 2 keeps one person's history together once a single signed message proves the mapping.

**Reverse-alias directory (Phase 0, done by the Chats work).** The committed alias work stores aliases (`aliases` table), not reverse aliases: the `alias_contacts` table that `docs/WORKPLAN.md` Phase 32 lists as done does not exist in `0019_aliases.sql`. Phase 0 adds it:

- `holtmail-alias` gains an optional `AliasDirectory::reverse_aliases(&self, alias_id: &str) -> Result<Vec<ReverseAlias>, AliasError>`, a default method that returns `Unsupported`, plus `Capabilities::reverse_aliases`. `ReverseAlias { address, contact: Address }`: the reverse alias, lowercase, and the contact it reaches. `alias_id` is a `RemoteAlias::id`. Every contact of the alias is listed once, in no particular order.
- `alias-simplelogin` implements it with the contacts endpoint, `GET /api/aliases/{alias_id}/contacts?page_id=N`, 20 contacts per page, each with the reverse alias address and the contact email. The field names and paging are checked against the official SimpleLogin `api.md` before the code is written, and against the SimpleLogin container of the `aliases` integration profile.
- A new table `alias_contacts` (`archive_id`, `reverse_alias`, `contact_email`, `contact_name`, `alias`, `provider`, `service_id`, `seen_at`), in a new migration `NNNN_alias_contacts.sql` (the next free number at merge), with `AliasStore` methods to replace a service's rows and to read them. It is refreshed in the same `holtmail-aliases` sync that calls `replace_service_aliases`, and its rows go with the service's aliases when the service is removed.
- addy.io and DuckDuckGo need no listing: they encode the sender in the reverse address (`alias+bob=example.org@…` in `crates/alias-addy`, `bob_at_example.org_user@duck.com` in `crates/alias-duck`), so the decoded sender plus a signed hit (rules 1 and 2) already suffices. Apple Hide My Email and Firefox Relay have no such listing, and Fastmail hides no sender. For those, rules 1 and 2 are the only ones.

**Reader wording (Phase 0).** `docs/ALIASES.md` and the web reader currently call a hit verified when it is signed **or** its alias is a known alias. A known alias proves the mail was addressed to the owner, not who sent it: anyone can mail a known alias with a forged `X-SimpleLogin-*` header. So the reader changes to match Chats: "verified" is used only for a signed hit; a hit to a known but unsigned alias says "delivered through your alias", with a tooltip: a signed hit means the alias service's DKIM signature proves who forwarded it; a known alias means it was addressed to you, but the sender is not proven. A known alias still makes the alias own (§4.1), but it never makes its reverse-alias mapping trusted for grouping.

| Mapping | Party key | Display |
|---|---|---|
| Trusted | the real sender's party | the real name |
| Untrusted | `e:{reverse alias}` | the name from the header, with an "unverified relay" hint |

An untrusted mapping never renames a person and never merges chats, so a forged forwarding header cannot move mail into someone else's chat.

Also changed in the committed alias code in Phase 0:

- The `resolved_addresses` view takes `sender_email` and `sender_name` from two independent `MAX()` calls over every row with that reverse alias, signed or not, so the name and the address can come from different rows, and one forged unsigned row can rename a person in People. Use the newest trusted hit instead, so both come from one row and an untrusted row never replaces an address.
- An optional `AliasDetector::decode_reverse(&self, address: &str) -> Option<String>` on the alias contract, a default method that returns `None`, for providers that encode the sender in the address: `alias-addy` (its `split_reverse`) and `alias-duck` (its `sender_of`). SimpleLogin's form (`ada_at_example_org_xk2@…`) loses the dots, so it is not decoded; its reverse aliases come from the directory above. It helps with reverse aliases in `Cc`, where the cc'd person never writes. It is a display hint only, shown as a name and never used for grouping. The one exception is a signed hit, where the decoded value only confirms the mapping that rule 1 already trusts.

### 4.4 Party keys

The obvious key, `p:{PersonId}`, breaks. An address without a contact card becomes a person only once the owner wrote to it or it is in at least two messages (`holtmail_people::is_correspondent`, `MIN_MESSAGES = 2`), so Ada's key changes from `e:ada@…` to `p:…` on her second mail, and every chat id with Ada changes with it. Person grouping also changes on every people rebuild after every sync. Keys are built from **emails** instead.

**Resolution of one address**, in order:

1. **Normalize** with the store's rule (trim, ASCII-lowercase, an `@` after at least one character). Today that rule is `address_key`, a `pub(crate)` function in `store-sqlite/src/people.rs`, and `message_addresses.email` is already stored in that form. Chats reads normalized emails from the store and does not need the function; the resolver normalizes only the few addresses it reads from facts (`sender`, `original_from`, `Reply-To`), with one shared helper moved to `holtmail-core` in Phase 0. Drop malformed addresses and `undisclosed-recipients:;` groups (mail-parser already flattens groups).
2. **Reverse alias.** If the mapping is trusted (§4.3), replace the address with the real sender.
3. **Me.** If the address is own, drop it from the parties.
4. **Canonical email.** If the address belongs to a person, the key is `e:{smallest mail email of that person}`. Otherwise it is `e:{address}`.

A person with one address has the same key before and after becoming a person, so a growing archive does not move chats. A person with several addresses (from a contact card, or a join override) gets one key for all of them.

The resolver also keeps `person_id` next to each party for display and links. That id is not part of any key.

**Chat id** = UUIDv8 of `blake3("chat" ‖ archive_id ‖ chat key)`, the same pattern as `person_id`. The chat key already carries its kind prefix (`l:`, `n:`, `saved`, `d:`, `g:`); `kind` is left out of the hash so that a group that becomes a broadcast after a `chats_group_limit` change keeps its id (§3.2).

### 4.5 Automated senders

A sender is automated when any of these holds:

- `Auto-Submitted` is present and not `no`, except `auto-replied`, which is an auto-reply (§3.2);
- `Precedence: bulk`, `junk` or `list`;
- `holtmail_people::is_automated_local(local)` is true: the local part starts with one of the prefixes `noreply`, `no-reply`, `no_reply`, `donotreply`, `do-not-reply`, `do_not_reply`, `mailer-daemon`, `postmaster`, `bounce`, without regard to letter case. The list stays private in `crates/people/src/group.rs`; the function is the exported API, so People and Chats share one list;
- `List-Unsubscribe` is present without `List-Id`.

This "automated" is not the message kind of [INSIGHTS.md](INSIGHTS.md) §4.2, and differs from it on purpose: Chats asks whether a human is on the other side of a thread, while the insights kind sorts mail into inbox views. `Precedence: bulk` alone or `List-Unsubscribe` alone makes a sender automated here but leaves the insights kind `personal`. Neither reads the other.

"I never wrote in the thread" is checked per thread, so one reply promotes that thread, not every mail from the sender.

The `notifications` chat key drops a `+tag` subaddress from the sender (`n:notify@example.com` for `notify+123@example.com`), so a sender that varies its tag per message is still one bot chat. This applies to the notifications key only; party keys never strip subaddresses, because `+` addresses are often deliberate identities (and the addy.io reverse-alias form).

## 5. Message facts

Facts depend only on a message's bytes. They are computed at ingest from the raw bytes and the `ParsedMessage` already in hand, and recomputed only when `holtmail_bubble::VERSION` changes.

`ParsedMessage` alone is not enough: it keeps `text` (mail-parser's `body_text(0)`, only the first text part), `html` (`body_html(0)`), `headers` (every field in order, added by the alias work), `attachments` (decoded, flattened) and root-level `calendars`, but not the MIME tree. So the entry point is `holtmail_bubble::facts(raw: &[u8], parsed: &ParsedMessage) -> MessageFacts`, and `holtmail_mime::body_parts(raw: &[u8])` re-walks the tree. mail-parser parses zero-copy, so the second walk is cheap next to the blob write; adding the parts to `ParsedMessage` would grow every parse (search, PDF, reader) for one consumer.

### 5.1 Role

| Role | Detected by | Becomes |
|---|---|---|
| `message` | default | a bubble |
| `auto` | `Auto-Submitted: auto-replied`, `X-Autoreply`, vacation markers | a muted, collapsed bubble labelled "Auto-reply" |
| `reaction` | a `text/vnd.google.email-reaction+json` part whose JSON validates as `{version: 1, emoji}`, with `emoji` non-empty, at most 64 bytes, and free of control characters, whitespace and ASCII letters or digits. The server adds no Unicode segmentation dependency for this; the client still renders it through `lib/emoji.ts` (§6.2) and shows a non-emoji value as text in the chip. | a reaction mark on the target |
| `read` | `multipart/report; report-type=disposition-notification` | a read mark on the target, when `Disposition` is `displayed`; nothing for `deleted` or `dispatched` |
| `delivery` | `multipart/report; report-type=delivery-status` | per-recipient `failed`, `delayed` or `delivered` marks on the target |
| `bounce` | non-RFC 3464 bounce: `From` is `mailer-daemon`/`postmaster` plus an attached message or a quoted `Message-ID:` | a `failed` mark with `heuristic: true` |
| `invite-reply` | an iMIP `METHOD:REPLY` with no meaningful text | a system line, e.g. "Ada accepted «Lunch»" |
| `encrypted` | `multipart/encrypted`, `application/pkcs7-mime` with `smime-type=enveloped-data`, or a text body that starts with `-----BEGIN PGP MESSAGE-----` (inline PGP) | a lock bubble, "Encrypted message", plus Show original |
| `digest` | a `multipart/digest` part (mailing-list digests) | one link-preview card ("Digest · N messages"), never split into bubbles |

Rules that keep this safe:

- An invalid or unknown reaction payload makes the message an ordinary bubble, built from its text or HTML fallback part.
- A plain emoji-only reply such as "👍" stays a big-emoji bubble. It is never converted into a reaction.
- Whether `mail-parser` puts the reaction part, the `message/disposition-notification` part and `text/rfc822-headers` among attachments or among body parts is **not verified**. Detection therefore walks the MIME tree through a new `holtmail_mime::body_parts(raw)` (Phase 0), and fixtures prove each case before any detector is written.
- A `multipart/signed` message (PGP/MIME or S/MIME) is an ordinary message; only its signature part is hidden (§6.4).

### 5.2 Targets and parents

- **`reply_rfc_id`**: the direct parent. `In-Reply-To`, else the last `References` entry. The store's `message_parents` cannot provide this, because it mixes ancestors in.
- **`target_rfc_id`**, for marks:
  - reaction: `reply_rfc_id`;
  - MDN: `Original-Message-ID` from the report part, else `In-Reply-To`;
  - DSN: the `Message-ID` from the attached `text/rfc822-headers` or `message/rfc822`, else `In-Reply-To`.
- **`root_rfc_id`**: the message root of §3.4.

### 5.3 Stream time

`at` must not depend on import order, so it is never clamped against `received_at`. That value is IMAP INTERNALDATE or SMTP arrival time, migration tools set it to the migration date, and it is stored only by whichever import came first.

- `at` = `sent_at` (the `Date` header).
- It falls back to the timestamp of the topmost `Received:` header (parsed from the bytes, so deterministic) when `Date` is missing, before 1990-01-01, or more than 1 day after that `Received:` timestamp.
- With neither usable, it falls back to `received_at`, then `archived_at`. This is the only order-dependent case, and it is rare.

Within a thread, refresh raises a reply's **sort time** to at least its parent's sort time + 1 ms when clocks are skewed, so a reply never sits above its parent. The whole thread is recomputed as one unit, so this never cascades across threads. The raised value is only for order (`chat_messages.at`); the bubble shows the message's own `at` from facts, so a skewed clock is visible rather than silently corrected. `Date` has second resolution, so equal times are common; the tie-break is described in §8.2.

Day separators and times are rendered in the viewer's local time zone; the server stores UTC milliseconds only.

### 5.4 Other facts

- **`list_id`**, **`list_post`** (the list's address, from `List-Post`), **`original_from`** (`X-Original-From`, for the DMARC rule in §3.7);
- **`automated`** (§4.5);
- **`sender`** (the `Sender` header, for §4.2);
- **`dkim_pass`** (`holtmail_alias::Mail::dkim_pass_domains`: the `header.d` values, and `header.i=@` domains, the topmost `Authentication-Results` records as `dkim=pass`, for the evidence rule of §4.2);
- **`text_hash`** (BLAKE3 of the full clean text before the cap, for the duplicate rule of §7.4);
- **`top_received_at`** (§5.3);
- **`reply_to`** (the first `Reply-To` address, for the automated-author rule in §3.2);
- **`links`**: up to 20 distinct `http(s)` URLs from the clean text after `holtmail_links::unwrap` and `holtmail_links::strip_tracking` (the Insights link library, [INSIGHTS.md](INSIGHTS.md) §3; there is no second unwrapper), for the Links tab. Links are never fetched: no link previews, no favicons, because a fetch would tell the sender the mail was read (§6.4);
- media flags (`has_photo`, `has_voice`, `has_video`, `has_file`, `has_contact`, `has_invite`, `has_link`, `forwarded`) for chat-row previews and shared-media tabs.

## 6. Bubble content

`holtmail_bubble::facts(raw, &parsed) -> MessageFacts` also builds the bubble payload. It is pure, has no I/O, and runs inside `catch_unwind`. A panic or error stores a plain text bubble made from `preview`, and the server logs the message id only.

### 6.1 Text extraction

1. **Choose a source.**
   - Collect **every** text body part in document order through `body_parts()`. This fixes Apple Mail's "text, image, text" layout, where `body_text(0)` drops the second caption.
   - If there is HTML, remove quote containers from the DOM *before* converting to text (`holtmail-quote`, built on `html5ever`, §8.1). The containers are:
     - `div.gmail_quote`, `blockquote.gmail_quote`;
     - `blockquote[type=cite]` (Apple Mail, Thunderbird);
     - `div.moz-cite-prefix` plus the blockquote that follows it;
     - Outlook `#divRplyFwdMsg`, `#appendonsend` and everything after them, plus a top-level `div` whose border-top starts a `From:`/`Von:`/`De:`/`От:` header block;
     - `hr#stopSpelling`, `.yahoo_quoted`, `.protonmail_quote`, Zimbra `hr#zwchr`, `#mail-editor-reference-message-container`.
   - The HTML path wins only if its output is non-empty and at least as long as the text part's new text. Otherwise the text part is used.
2. **Find quote boundaries in the text.**
   - Attribution lines, anchored to a line start and allowed to wrap onto one more line: `On … wrote:`, `Am … schrieb …:`, `Le … a écrit :`, `El … escribió:`, `Il … ha scritto:`, `Op … schreef …:`, `Em … escreveu:`, `W dniu … napisał(a):`, `… написал(а):`, `… пишет:`, `在 … 写道：`, `… 寫道：`, `… のメッセージ:`.
   - Separators: `-----Original Message-----` and its translations (`Ursprüngliche Nachricht`, `Message d'origine`, `Исходное сообщение`), a run of 30 or more `_`, and Outlook header blocks of 3 or more consecutive `Label: value` lines from a localized label set (From/Sent/To/Subject, Von/Gesendet/An/Betreff, De/Envoyé/À/Objet, От/Отправлено/Кому/Тема).
   - Runs of lines starting with `>`.
   - **An attribution line counts only when quoted text follows it.** "On Monday Ivan wrote: the plan is…" with nothing quoted after it stays.
3. **Decide the layout.**

   | Layout | Shape | Result |
   |---|---|---|
   | Top-posting | new text, then the quote at the tail | Cut the tail. The reply preview stands in for it. |
   | Bottom-posting | the quote first, then new text | Cut the leading quote. |
   | Inline reply | quoted runs and new text interleave, with at least 2 own-text runs | Keep everything. Quoted runs become `Quote` segments, rendered as collapsed quote blocks inside the bubble. A point-by-point answer is never deleted. |

4. **Cut the signature, from the tail only:**
   - the RFC 3676 `-- ` delimiter;
   - mobile footers: "Sent from my iPhone", "Отправлено с iPhone", "Von meinem iPhone gesendet", "Get Outlook for iOS/Android", "Sent from Mail for Windows".

   Disclaimers are not cut by keyword: "Please keep this confidential." is a real sentence. A repeated-footer rule (the same tail paragraph in 3 or more messages from the same author) runs at refresh time in Phase 4. It hides the footer behind a toggle and never deletes it.
5. **Invariants.**
   - If cleaning leaves no text and there is no media, the full text is kept and `cut.failed` is set. A bubble never silently empties.
   - `cut` records what was removed (`tail_quote`, `lead_quote`, `signature`, `failed`), so the bubble shows "Quoted text hidden" and "Signature" toggles.
   - The stored clean text is capped at 4 KiB with `truncated`. Most chat messages are far shorter. "Show more" expands in place by fetching `GET /api/messages/{id}/clean`, which returns the full clean text plus the cut parts. It never sends the reader to the mail view.
   - `preview` is the clean text on one line, at most 140 characters, for chat rows and reply previews.

### 6.2 Emoji-only

Decided on the client (`lib/emoji.ts`) with `Intl.Segmenter` and the `\p{RGI_Emoji}` property (the `v` regex flag). That covers flags (regional indicators) and keycaps (`1️⃣`), which `\p{Extended_Pictographic}` misses. One to three clusters render large, with no bubble fill and the time in a small pill beneath. Glyph sizes come from three new type tokens. The chat-row preview uses the same function, so no Rust dependency is added.

### 6.3 Forwards

| Shape | Detection | Bubble |
|---|---|---|
| Attached `message/rfc822` | the part, parsed again with `holtmail_mime::parse`, depth at most 2, at most 25 MiB | A **forwarded bubble**: "Forwarded from {from} · {date}", then the inner clean text and the inner attachments as media. |
| Inline forward | at the top of an otherwise empty body: `---------- Forwarded message ---------`, "Begin forwarded message:", "Weitergeleitete Nachricht", "Message transféré", "Пересылаемое сообщение", followed by a header block | The same forwarded bubble, built from the header block. |
| Forward with a comment | new text above the forward | Two items, as Telegram shows them: the comment bubble, then the forwarded bubble. |

Exchange journal reports are not forwards. The SMTP listener (`apps/server/src/smtp.rs`, journal mode `exchange`) already unwraps them with `holtmail_mime::journal_original` before ingest. A journal report that arrives some other way (a file import) is not unwrapped and shows as a forwarded bubble, which is honest about the bytes.

### 6.4 Attachments and media

Each attachment gets an `attachment_media` row at ingest with `kind`, `width`, `height`, `animated`, `cid_referenced` and `hidden`. Image dimensions come from the headers only (`image::ImageReader::into_dimensions`, no full decode), and the animated flag comes from a frame-count header scan for GIF and WebP. Dimensions ship in the first phase, because the stream needs `aspect-ratio` before an image loads to keep its scroll position stable.

| Kind | Detection | Rendered as |
|---|---|---|
| Layout image | inline `cid:` part referenced from the HTML that is under 200 px on its long side, or inside the cut signature or quote subtree | Hidden from the bubble; visible in Show original. |
| Photo | image part not hidden | One photo: a full-width media bubble, caption below. |
| Album | 2 or more photos in one message, or consecutive photo-only bubbles from the same author within 2 minutes (grouped on the client) | Mosaic from `lib/album-layout.ts` using the stored sizes, 1px hairline gutters, caption below, lightbox on click. |
| Sticker | exactly one image, no clean text, long side 512 px or less, and PNG/WebP with alpha or animated | No bubble box; at most 10rem. |
| GIF | animated GIF or WebP | Plays muted and looped only while in view (IntersectionObserver), paused offscreen. Under reduced motion: the first frame with a play overlay. Served as the original, which is already an inline type. |
| Video | `video/*` | Poster from `preload="metadata"`, duration chip, plays inline. Needs Range (§10.3). |
| Voice | `audio/*` of at most 10 minutes with no or tiny clean text (under 40 characters), or voicemail markers (`X-Asterisk-*`, `X-Voicemail-*`, `X-CiscoUnity*`; known subject templates as a weak signal) | **Voice bubble**: play/pause square, waveform (bars in `fg-muted`, the played part in `fg`), duration, 1×/1.5×/2× speed, Space to toggle. If `canPlayType` fails (AMR, GSM), the same shape with Download and "duration unknown". |
| Audio file | audio with a title, or longer than 10 minutes | A track row: filename, duration, scrubber. |
| File | anything else | A file row: kind icon from `attachmentKind()`, name, size, Download. |
| Contact | `text/vcard`, `text/x-vcard`, `.vcf`, read with `holtmail_pim::normalize(ItemKind::Contact)` | Contact card: initials square, name, emails, phones, "Open in Contacts". |
| Event | `text/calendar` with the existing `InvitationDto` | `invitation-card.tsx`, placed in the bubble. |
| Hidden | `smime.p7s`, `.asc` signatures, and `winmail.dat` when the rest of the message rendered | Not shown; a small "signed" mark for signatures. |
| HEIC, AVIF or other image without a thumbnail | image type `holtmail_thumbnail::supports` rejects (it takes PNG, JPEG, GIF and WebP; the workspace `image` crate has only those features) | A file row. AVIF is already served inline by the attachment route, but without stored dimensions it cannot reserve its box, so it stays a file row until the thumbnailer reads it. |

**Remote content.** Only MIME parts of the message become media. An `<img src="https://…">` in the HTML is never a photo bubble and is never fetched from the chat view; it exists only in Show original, behind the existing remote-image controls. A vCard `PHOTO` given as a URL is ignored; an embedded `PHOTO` is shown only after the same sniffing as attachments. Avatars are initials only: no Gravatar, BIMI or favicon lookups.

**Newsletters and rich HTML.** A message with more than 5 tables, less than 15% visible text by length, or many images becomes a **link-preview card**: subject, clean preview and "Open". This is the default rendering in `list`, `broadcast` and `notifications` chats. It is never inlined.

## 7. How mail becomes items

The product spec, row by row. Every bubble keeps Show original.

| Mail | Item | Notes |
|---|---|---|
| Plain or top-posted reply | text bubble | Reply preview above the text when the parent is known (§7.1). |
| Inline reply | text bubble with quote segments | §6.1 |
| HTML-only mail | text bubble | From the DOM path; the rich card when it is newsletter-like. |
| Photos, video, voice, files, stickers, GIFs | media bubble or album | §6.4 |
| Emoji-only | large emoji, no fill | §6.2 |
| Forward | forwarded bubble, plus a comment bubble | §6.3 |
| Event invitation | invitation card | |
| RSVP (iMIP REPLY) | system line "Ada accepted «Lunch»" | |
| Event cancelled (iMIP CANCEL) | invitation card showing the cancellation | |
| Contact attached | contact card | |
| Gmail reaction | chip under the target bubble | §7.2 |
| Outlook reaction (Graph accounts, Phase 4) | chip under the target bubble | §7.2 |
| Copy Bcc'd to me | bubble in the visible parties' chat with "Bcc'd to you" | §3.3 |
| `From` or `Sender` is me without evidence | bubble on the left, "claims to be you", warning glyph | §4.2 |
| Same `Message-ID`, different content | separate bubbles; the later one marked "differs from another copy with the same Message-ID" | §7.4 |
| Read receipt | double check on my bubble | §7.3 |
| Delivery report or bounce | failure mark, plus a system line | §7.3 |
| Auto-reply | muted collapsed bubble "Auto-reply" | |
| Encrypted | lock bubble | |
| Subject changes inside a topic | system line "Topic renamed to …" | |
| New participant in a group thread | system line "Ada added Bob" | first appearance only |
| `\Flagged` | pinned bar at the top of the chat | read time, §9.2 |
| `\Seen` missing | unread counts and a "New messages" divider | read time, §9.2 |

### 7.1 Replies

The reply preview is a hairline-left-bar block inside the bubble: author, plus a one-line snippet or media label ("Photo", "Voice message 0:12"). It resolves `reply_rfc_id` to the preferred copy (§7.4) in the same chat.

| Parent | Preview |
|---|---|
| In the same chat | Click jumps there with `around=`. |
| In another chat (thread moved, list post) | Click opens that chat at the message. |
| Not in the archive | "Reply to a message not in the archive", not clickable. |
| Thread root of a top-post with no parent text | No preview. |

### 7.2 Reactions

- A chip row under the target bubble shows each emoji with a count. Authors appear in a tooltip. A chip that includes my reaction has an `fg` hairline and medium-weight count instead of the `border` hairline; the accent is not used, because `docs/design.md` keeps it for focus, selection and links.
- Several reactions from one author to one message: the latest wins, as in Gmail.
- Reaction space under a bubble is reserved only when it has reactions, and marks are loaded together with the page (§9.1), so nothing shifts after render.

**Outlook reactions (Phase 4).** Outlook reactions are not in the MIME bytes, so no detector can find them. Holtmail reads Outlook mail through `source-graph` (Microsoft Graph, MIME from `/me/messages/{id}/$value`) and `source-jmap`. Graph has no documented reactions API; a third-party write-up (Glen Scales, "Using the Outlook Reaction's feature in the Microsoft Graph") reports that they are readable as an extended property, `ReactionsSummary`, holding JSON. JMAP has no standard for reactions, so `source-jmap` provides none.

- `source-graph` requests that extended property during sync and hands it over as server-side metadata carried by the `holtmail-source` contract next to flags (`RemoteMessage`, `RemoteFlags`). It is not part of the raw bytes. It is stored per location like flags and is rebuilt by a resync; a change marks the thread dirty, like a location change (§8.4).
- Placement turns it into reaction marks on that message, with the same chip rendering as Gmail reactions.
- Nothing is built until the exact property id and the JSON shape are verified against a test tenant. Fixtures are then invented from that shape; no real tenant data is committed.

None of this is in Phases 1–3.

### 7.3 Read and delivery marks

| Mark | Source | Shown |
|---|---|---|
| Sent | my bubble with a copy in a `sent` folder (an outgoing SMTP route marked `sent = true` included, §4.2) or with the Gmail `Sent` label. My bubble whose evidence is only an aligned DKIM pass, or whose copies are all elsewhere (an import folder without the role), shows no mark rather than a false one | `CheckIcon` (lucide-react) |
| Read | an MDN with `Disposition: displayed` for my message | `CheckCheckIcon`. "Read at …" in the bubble info. In groups, "Read by Ada, Bob". |
| Delivered | DSN `Action: delivered` | info only |
| Delayed | DSN `Action: delayed` | info only, never an alarm |
| Failed | DSN `Action: failed` (per recipient) or a heuristic bounce | `CircleAlertIcon` (the name the code base already uses) in the `danger` token on my bubble. The status code and diagnostic are in the bubble info. A system line "Not delivered to Bob" follows the bubble. In a group the mark is per recipient. |
| My MDN for their message | an MDN I sent | "You read this at …" in info |

### 7.4 Duplicates

The same `rfc_message_id` can exist several times: a Sent copy and a received copy, a list rewrite, two accounts. It can also be reused, by a buggy mailer or on purpose: anyone can send a message with the `Message-ID` of mine. So a shared `Message-ID` alone never merges anything.

- **Collapse rule.** Two copies collapse only when all of these hold:
  - the `rfc_message_id` matches, the normalized author (after the DMARC rule of §3.7) matches, and the `sent_at` values are within 120 s;
  - the content matches: equal `text_hash` (the full clean text) **and** the same multiset of visible attachment `blob_id`s (BLAKE3 of the decoded content; hidden parts of §6.4 such as signatures are left out); **or** the list-echo case below.
- **List echo.** One copy is my evidenced copy (§4.2), the other carries a `List-Id` and was sent to that list's post address, and the only difference is what the list added: the echo's clean text starts with my clean text and continues with at most one trailing paragraph block of at most 1 KiB, and its visible attachments are mine plus at most one added `text/plain` or `text/html` footer part. A changed word, a removed or replaced attachment, or text added anywhere but the tail does not match. Subjects are not compared, so a `[list-tag]` is fine.
- **Otherwise they stay separate bubbles.** The non-preferred copy (below) gets the mark "differs from another copy with the same Message-ID" in its bubble info and a small inline note, and Show original lists both.
- **Preferred copy.** Used as the canonical copy of a collapsed set, and to decide which of two separate copies carries the "differs" mark, which one a reply preview (§7.1) and a mark (§7.5) resolve to:
  1. my evidenced copy (§4.2);
  2. the earliest `at`;
  3. the copy without `List-Id`;
  4. the smallest `blob_id`, which is a content hash, so the choice does not depend on import order. The tie-break is never the `MessageId`, which is a UUIDv7 made at insert time.

  Evidence comes first, so nobody can replace my message with a modified copy by sending one with the same `Message-ID`, not even with an earlier `Date`: their copy stays a separate bubble on the left, marked "differs" and, when it claims my address, "claims to be you".
- A claim (§4.2) collapses into my evidenced copy only under these content rules, where it adds nothing visible. A claim that differs is never merged with my copy.
- The bubble info shows "2 copies" and lists every copy in Show original.

### 7.5 Marks before their target

A reaction, MDN or DSN can arrive before the message it points at, or the target may never be archived.

- While unresolved, the mark is stored in `unresolved_marks` and the source message shows as a muted system line in its own thread's chat: "Ada reacted 👍 to a message not in the archive", "Read receipt", "Delivery failed". Nothing is lost.
- When a message with that `rfc_message_id` is inserted, the waiting source threads are marked dirty in the same transaction (§8.4). The next refresh attaches the mark and removes the system line.
- **Ownership.** A mark row belongs to its *source* message: it is written and removed only when the source's thread is placed, and it stores the target by `target_rfc_id`, not by message id. The read path joins marks to a page's bubbles by their `rfc_message_id`, so a later change of the target's canonical copy (§7.4), a merge of the target's thread or a re-placement of the target never leaves a mark pointing at a stale row. When several non-collapsed messages share the `rfc_message_id`, the mark goes to the one in the same chat as the mark's source, else to the preferred copy (§7.4).
- An MDN or DSN whose own thread has no conversational message has no chat of its own (§3.2 row 0); while unresolved it shows in its sender's `notifications` chat.

### 7.6 Show original

Every bubble has a menu (`DropdownMenu`, right-click, or `Enter`/`o` on the focused bubble; `Shift+O` opens Show original directly). It opens a dialog with:

- **Message**: the existing sanitized HTML in the sandboxed iframe (`HtmlBody`), with the remote-content banners and controls;
- **Full text**: the uncut text, quotes and signature included;
- **Source**: `rawQuery`;
- Download .eml, PDF, "Open in mail" (`/a/$archiveId?m=`), and "Copy link".

### 7.7 Membership lines

These are derived at refresh, not at read time, so page boundaries never hide the previous message.

- Broadcasts get no membership lines (§3.2); their bubbles name the author instead.
- "Ada added Bob" is emitted on the first message of a group topic that includes a party who was not there before. It is emitted only on a first appearance, so Reply versus Reply All does not produce "Bob left" and "Bob added" pairs.
- "Topic renamed to …" is emitted when the normalized subject changes inside a topic. An empty subject, a subject that is a prefix of the current title (clients that truncate) and a change made by an automated or mark message do not count, so a phone client cutting the subject does not produce a rename line.

## 8. Modules and storage

### 8.1 Modules

| Module | Kind | Job |
|---|---|---|
| `crates/quote` (`holtmail-quote`) | pure library | Splits a body into new text, quote segments, signature and inline-forward block, for text and HTML. No I/O. Uses `html5ever` at the version `ammonia` already pulls in (run `evaluate_dependency` first). |
| `crates/bubble` (`holtmail-bubble`) | pure library | `facts(raw, &ParsedMessage) -> MessageFacts`: role, targets, root, `at`, list and automation markers, clean text and `cut`, forward, per-attachment media kind and dimensions. Exposes `VERSION`. Depends on `holtmail-core`, `holtmail-mime`, `holtmail-quote`, `holtmail-pim` and `image` (header-only reads). |
| `crates/chats` (`holtmail-chats`) | library over `&dyn Store`, like `holtmail-people` (whose `rebuild(store: &dyn Store, archive)` is the model) | Identity resolver, placement (`place(&TopicInput, &Resolver) -> TopicPlacement`, pure; not named `Placement`, which `holtmail_ingest::Placement` already is and `apps/server` imports both), `refresh(store, archive)` (incremental) and `rebuild(store, archive)` (whole). Exposes `VERSION`. |
| `crates/media` (`holtmail-media`) | pure library, Phase 3 | Audio duration and a 64-bucket waveform from container headers and a decode probe (`symphonia`, after `evaluate_dependency`). mp4/mov duration from boxes only. No ffmpeg. |
| `holtmail-core` | change | New ids `ChatId` and `TopicId` (UUIDv8 helpers like `person_id`); the shared address-normalization helper (§4.4). |
| `holtmail-mime` | change | `body_parts(raw: &[u8])`: ordered text, html and inline parts, with nested parts reported. |
| `holtmail-alias` | contract change, Phase 0 | Optional `AliasDetector::decode_reverse` and `AliasDirectory::reverse_aliases` (default methods), `Capabilities::reverse_aliases`, `ReverseAlias`; a `Mail::dkim_pass_domains()` accessor next to `Mail::dkim_pass` for the `dkim_pass` fact (§5.4); both parse through `holtmail-auth-results`, so `holtmail-alias` takes no `holtmail-mime` dependency (`dkim_pass_domains` landed with Insights Phase 35). Conformance in `holtmail_alias::testing` covers the defaults. |
| `alias-simplelogin`, `alias-addy`, `alias-duck` | impl, Phase 0 | SimpleLogin lists contacts through `reverse_aliases`; addy.io and DuckDuckGo implement `decode_reverse` with their existing `split_reverse` and `sender_of`. |
| `holtmail-aliases` | change, Phase 0 | The directory sync also stores each alias's reverse aliases in `alias_contacts` when the directory has the capability. |
| `holtmail-people` | change | Export `is_automated_local(&str) -> bool` (§4.5; landed with Insights Phase 35). |
| `holtmail-store` | contract change | New `chats.rs` with `ConversationStore`, added to `Store` and its blanket impl, plus a `testing` conformance area `chats`. `AliasStore` methods for `alias_contacts` (Phase 0). `PeopleStore` own-address overrides (Phase 1, §4.1). `Archive::chats_group_limit` (Phase 1). `DerivedKind::Waveform` (Phase 3). |
| `holtmail-store-sqlite` | impl | Migrations `NNNN_alias_contacts.sql` (Phase 0) and `NNNN_chats.sql` (Phase 1: chat tables, `chat_prefs`, own-address overrides, `archives.chats_group_limit`) and `src/chats.rs`. `NNNN` is the next free migration number at merge. |
| `holtmail-source`, `source-graph` | contract change and impl, Phase 4 | Server-side reaction metadata next to flags; `source-graph` reads the `ReactionsSummary` extended property (§7.2). |
| `holtmail-blob` | contract change, Phase 3 | `get_range(id, start, end)` as a default method that slices `get`. `blob-fs` and `blob-s3` override it. The conformance suite gains a range area. |
| `holtmail-ingest` | change | In `Ingest::ingest`, inside the existing `if inserted.created` block next to `set_message_alias`: calls `bubble::facts(&raw, &parsed)` and `set_message_facts`. Re-ingesting known bytes writes nothing. |
| `holtmail-api` | change | `chats.rs` handlers and DTOs, `identities` (list and the minimal override), chat preferences, `clean`; `chats_group_limit` in `ArchiveUpdateRequest`; media allowlist with sniffing and Range in `routes::get_attachment` and its shared `routes::download` response helper. |
| `apps/server` | composition root | `PeopleRefresh` (`apps/server/src/people.rs`) renamed to `DerivedRefresh` in Phase 0 as its own change, then the `chats_only` request kind; chats refresh chained after people in it, plus the extra refresh triggers of §8.8, start-up pass, `reindex` steps. `sent = true` on `[[smtp.route]]` (§4.2). |
| `packages/tokens`, `packages/ui`, `packages/api-client`, `apps/web/src/chats/` | web | Bubble tokens, owned components (avatar, voice player, lightbox), queries, the Chats UI. |

`holtmail-bubble` never depends on `holtmail-media`, so audio decoding never sits on the ingest path.

### 8.2 Tables (`NNNN_chats.sql`)

All tables are derived and rebuildable, except `chat_prefs` and the own-address overrides, which are user state (below). None of them references blobs, so `InventoryStore` is unchanged.

| Table | Key and columns | Written by |
|---|---|---|
| `message_facts` | `message_id` PK, `archive_id`, `version`, `role`, `root_rfc_id`, `reply_rfc_id`, `target_rfc_id`, `mark_json` (reaction emoji, disposition, per-recipient DSN), `list_id`, `list_post`, `original_from`, `sender`, `dkim_pass`, `automated`, `at`, `media_flags`, `text` (≤ 4 KiB), `text_hash`, `truncated`, `segments_json`, `cut_json`, `preview` (≤ 140 chars), `forward_json`. Indexes `(archive_id, root_rfc_id)`, `(archive_id, target_rfc_id)` partial, `(archive_id, version)`. | ingest, backfill |
| `attachment_media` | PK `(message_id, ordinal)`, `archive_id`, `kind`, `width`, `height`, `animated`, `cid_referenced`, `hidden` | ingest, backfill |
| `chat_dirty` | PK `(archive_id, thread_id)`, `seq` | triggers (§8.4) |
| `chat_identities` | PK `(archive_id, email)`, `canonical`, `person_id`, `own`, `strength`, `trusted_sender` | refresh: the resolver's email → key map from the last refresh |
| `chats` | `id` PK, `archive_id`, `kind`, `key`, `title`, `party_count`, `topic_count`, `forum`, `message_count`, `first_at`, `last_at`, `last_message_id`, `last_mine`, `generation`. Indexes `(archive_id, last_at DESC, id DESC)` and `(archive_id, kind, last_at DESC, id DESC)`. | refresh |
| `chat_members` | PK `(chat_id, party_key)`, `archive_id`, `person_id`, `email`, `name`, `ordinal`. Index `(archive_id, person_id)`. | refresh |
| `chat_topics` | `id` PK, `chat_id`, `archive_id`, `root_rfc_id`, `title`, `general`, `split_tag`, `message_count`, `first_at`, `last_at`, `last_message_id`. Index `(chat_id, last_at DESC, id DESC)`. | refresh |
| `chat_messages` | PK `message_id`, `chat_id`, `topic_id`, `archive_id`, `thread_id`, `at` (sort time, §5.3), `tie` (`blob_id`, the tie-break: a content hash, so independent of import order; named `tie` so it is not confused with `chat_dirty.seq`), `kind` (`bubble`, `system`, `mark`), `canonical_id`, `mine`, `evidence` (`sent`, `dkim` or null), `claim` (§4.2), `bcc` (§3.3), `differs` (§7.4), `author_key`, `system_json`. Indexes `(chat_id, at, tie)` and `(topic_id, at, tie)`, both partial on `kind <> 'mark' AND canonical_id IS NULL`, plus `(archive_id, thread_id)`. | refresh |
| `chat_marks` | `source_message_id`, `archive_id`, `target_rfc_id`, `kind`, `value`, `actor_key`, `recipient`, `at`. Indexes `(archive_id, target_rfc_id)` and `source_message_id`. Owned by the source (§7.5). | refresh |
| `unresolved_marks` | PK `(archive_id, target_rfc_id, source_message_id)` | refresh |
| `chat_state` | PK `archive_id`, `generation`, `building_generation` (unfinished rebuild, or null), `versions` (`"{bubble}.{chats}"`), `group_limit` (the `chats_group_limit` the generation was built with), `inferred_owner` (§4.1, kept by rebuilds), `identity_hash`, `dirty_seq` (monotonic, bumped by the dirty triggers, which create the row if missing), `refreshed_at` (for `chats/status`) | triggers, refresh, rebuild |
| `chat_prefs` | PK `(user_id, archive_id, chat_key)`, `forum` (null: the computed default). User state, not derived: no generation, never touched by refresh or rebuild; deleted with the user or the archive. | `PUT …/chats/{chat}/prefs` |
| `own_overrides` | PK `(archive_id, email)`, `own`, `created_at`. User state owned by `PeopleStore` (§4.1); deleted with the archive. | `POST …/identities` |

Notes:

- Unread and pinned are **not** stored. They are joined from `locations.flags` at read time (§9.2), because `set_flags` changes flags on every sync and does not dirty anything.
- `message_facts.text` holds mail content in SQLite, as `messages.subject` and `preview` already do. It is derived and rebuildable, and it never goes to logs.
- **Names are read, not stored.** `chat_members.name` and `chats.title` are fallbacks. Reads join `chat_members.person_id` to `people.name`, so a renamed contact shows its new name at once; the identity diff (§8.5) only follows changes that move keys, and a name change moves none.
- **Generations.** Every placement table (`chats`, `chat_members`, `chat_topics`, `chat_messages`, `chat_marks`, `unresolved_marks`) carries `generation`, and its indexes start with it. Reads filter on `chat_state.generation`; `finish_rebuild` flips it in one transaction, and the old generation's rows are deleted in batches afterwards. `message_facts` and `attachment_media` have no generation: they depend on bytes only.
- **Facts are per message and self-consistent**, so messages with facts of different `VERSION`s can be placed together. A new `holtmail_bubble::VERSION` therefore never forces a chats rebuild: re-written facts fire the facts trigger (§8.4), which re-places just those threads.
- **Cleanup.** `DeletionStore::delete_messages` removes the per-message rows of `message_facts`, `attachment_media`, `chat_messages`, `chat_marks` (by `source_message_id`) and `unresolved_marks` before it deletes the `messages` row (foreign keys are on), and marks the affected threads dirty. Marks *pointing at* a deleted message need no cleanup: they are joined by `rfc_message_id` and simply stop matching (the source thread is dirtied so its system line comes back). `delete_archive` deletes every table above by `archive_id`, **after** `messages`, because the `AFTER DELETE ON messages` trigger would otherwise leave fresh `chat_dirty` rows for a deleted archive. These are edits to `store-sqlite/src/deletion.rs` and `lib.rs`. `delete_archive` also deletes `chat_prefs`, `own_overrides` and `alias_contacts`, and deleting a user deletes their `chat_prefs`.

### 8.3 Contract: `ConversationStore`

Every method is `#[async_trait]`, object-safe, and returns `Result<_, StoreError>`. Doc comments state the guarantees below.

```rust
#[async_trait]
pub trait ConversationStore: Send + Sync {
    // facts
    async fn set_message_facts(&self, message: MessageId, facts: &MessageFacts) -> Result<(), StoreError>;
    async fn messages_needing_facts(&self, archive: ArchiveId, version: u32, after: Option<MessageId>, limit: u32) -> Result<Vec<MessageId>, StoreError>;

    // inputs
    async fn dirty_threads(&self, archive: ArchiveId, limit: u32) -> Result<Vec<DirtyThread>, StoreError>;
    async fn mark_dirty(&self, archive: ArchiveId, threads: &[ThreadId]) -> Result<(), StoreError>;
    async fn threads_with_emails(&self, archive: ArchiveId, emails: &[String]) -> Result<Vec<ThreadId>, StoreError>;
    async fn topic_inputs(&self, archive: ArchiveId, threads: &[ThreadId]) -> Result<Vec<TopicInput>, StoreError>;
    async fn thread_ids(&self, archive: ArchiveId, after: Option<ThreadId>, limit: u32) -> Result<Vec<ThreadId>, StoreError>;
    async fn identity_map(&self, archive: ArchiveId) -> Result<Vec<IdentityEntry>, StoreError>;

    // writes
    async fn apply_chats(&self, archive: ArchiveId, change: &ChatChange) -> Result<(), StoreError>;
    async fn begin_rebuild(&self, archive: ArchiveId) -> Result<Generation, StoreError>;
    async fn write_rebuild(&self, archive: ArchiveId, generation: Generation, part: &ChatChange) -> Result<(), StoreError>;
    async fn finish_rebuild(&self, archive: ArchiveId, generation: Generation, state: &ChatState) -> Result<(), StoreError>;
    async fn chat_state(&self, archive: ArchiveId) -> Result<Option<ChatState>, StoreError>;

    // reads
    async fn list_chats(&self, q: &ChatQuery) -> Result<Page<ChatSummary>, StoreError>;
    async fn chat(&self, archive: ArchiveId, id: ChatId) -> Result<ChatDetail, StoreError>;
    async fn topics(&self, q: &TopicQuery) -> Result<Page<TopicSummary>, StoreError>;
    async fn chat_items(&self, q: &ChatItemQuery) -> Result<ChatItemWindow, StoreError>;
    async fn marks(&self, archive: ArchiveId, targets: &[String]) -> Result<Vec<Mark>, StoreError>;
    async fn attachment_media(&self, messages: &[MessageId]) -> Result<Vec<(MessageId, Attachment, MediaFacts)>, StoreError>;
    async fn locate(&self, message: MessageId) -> Result<Option<ChatLocation>, StoreError>;
    async fn chat_days(&self, q: &DaysQuery) -> Result<Vec<DayCount>, StoreError>;
    async fn chat_media(&self, q: &MediaQuery) -> Result<Page<MediaItem>, StoreError>;
    async fn chats_of_person(&self, archive: ArchiveId, person: PersonId) -> Result<Vec<ChatSummary>, StoreError>;
}
```

Guarantees:

- **Dirty marks are durable.** Inserting a message, changing its thread (a merge), adding or removing a location, and deleting a message each mark the affected threads dirty in the same transaction (§8.4). A crash never loses pending work.
- **`apply_chats` is atomic.** Readers see the old placement of the covered threads or the new one, never a mix. It removes every row whose `message_id` now belongs to a covered thread, writes the new rows, recomputes the aggregates of every chat and topic touched before *or* after, and deletes chats and topics left empty. It clears a dirty mark only if the mark's `seq` is unchanged since `dirty_threads` returned it; a racing insert keeps its thread dirty.
- **Rebuild is a generation swap.** `finish_rebuild` swaps in one transaction. Readers see the whole old generation or the whole new one. `begin_rebuild` records the highest dirty `seq`; `finish_rebuild` clears only dirty marks at or below it, so a thread dirtied while the rebuild ran stays dirty and the next refresh places it on top of the new generation. Refresh and rebuild of one archive never run at the same time (they share one queue, §8.8); `apply_chats` during a rebuild is `Invalid`. `begin_rebuild` discards any unfinished generation first, so a rebuild cut short by a restart is simply started again by the start-up pass and never blocks refresh for good.
- **Names.** The contract's stream types are `ChatItem*` (`ChatItemQuery`, `ChatItemWindow`), not `Item*`: `holtmail_store::ItemQuery` and `/api/items/{id}` already mean contacts and events, and the OpenAPI schema names must not collide with `ItemSummaryDto`.
- **Order.** `chat_items` returns `(at, tie)` ascending. `list_chats` and `topics` return `last_at DESC, id DESC`.
- **Windows.** `chat_items` accepts `before`, `after` or `around` (a message id). `around` returns at most `limit` items centred on the anchor, built from two half-limit queries, with cursors in both directions.
- **Paging.** Cursors are opaque (`"{at_ms}.{tie}"` for items, `"{last_at_ms}.{id}"` for lists) and valid only for the query that issued them; any other cursor gives `StoreError::Invalid`. `limit` is clamped to 1..=200 for items and 1..=500 for lists. A page shorter than the limit is the last one.
- **Missing.** An unknown chat or topic, or a chat of another archive, gives `NotFound`. A message not yet placed gives `locate` = `None`.
- **Isolation.** Nothing crosses archives: the same mail in two archives gives different ids.

### 8.4 Dirty marking without changing `insert_message`

`insert_message` keeps its contract. Dirty marking comes from SQLite triggers in `NNNN_chats.sql`, so it happens inside every writer's transaction:

| Trigger | Marks dirty |
|---|---|
| `AFTER INSERT ON messages` | the new message's thread, plus the source threads in `unresolved_marks` waiting for `NEW.rfc_message_id` |
| `AFTER UPDATE OF thread_id ON messages WHEN OLD.thread_id IS NOT NEW.thread_id` | the new thread (the absorbed messages are now in it). `insert_message` merges with one `UPDATE … WHERE thread_id = ?`, so this fires once per moved row; the dirty upsert is idempotent. |
| `AFTER INSERT ON locations` | the message's thread (drafts, junk, Sent copy) |
| `AFTER UPDATE OF removed_at ON locations WHEN OLD.removed_at IS NOT NEW.removed_at` | the message's thread. The `WHEN` is required: `record_location` is an `INSERT … ON CONFLICT DO UPDATE` whose `SET` always names `removed_at`, so without it every re-seen location would dirty its thread. |
| `AFTER DELETE ON locations` | the message's thread. `delete_account` deletes the locations of its folders outright, which changes Sent copies, exclusion and unread. |
| `AFTER INSERT ON message_labels`, `AFTER DELETE ON message_labels` | the message's thread (Gmail `Sent`, `Drafts`, `Spam` labels) |
| `AFTER DELETE ON messages` | the message's thread |
| `AFTER INSERT ON message_facts`, `AFTER UPDATE ON message_facts` | the message's thread (facts arrive just after the message). SQLite has no `INSERT OR UPDATE` trigger, so these are two triggers. |

The dirty write is `INSERT INTO chat_dirty … ON CONFLICT DO UPDATE SET seq = <next seq>`, so a thread dirtied twice is one row with the newer `seq`. `<next seq>` comes from a monotonic counter (a `dirty_seq` column in `chat_state`, bumped in the same trigger), never from `MAX(seq) + 1`, which would reuse numbers after the table empties and break the "unchanged since read" check.

Flag-only changes (`set_flags`) do not fire triggers: unread and pinned are read live (§9.2). A `\Draft` flag only ever arrives with a new location, which does fire. The conformance suite proves each row through the contract, so another store implementation must provide the same guarantees by its own means.

### 8.5 Refresh: incremental

`holtmail_chats::refresh(store, archive)` runs after every people rebuild, in the same queue:

1. **Versions and settings.** If the `holtmail_chats::VERSION` part of `chat_state.versions` differs from the code, or `chat_state.group_limit` differs from the archive's `chats_group_limit`, run `rebuild` and stop. A changed `holtmail_bubble::VERSION` does not rebuild: the facts backfill rewrites facts and the facts trigger dirties those threads (§8.2).
2. **Identity diff.** Build the resolver from `mail_addresses().own` (with strengths), the own-address overrides, `people_by_email`, trusted alias mappings (signed hits and `alias_contacts`) and the inferred owner (§4.1). If the inferred owner changed, run `rebuild` and stop. Compare its email → (key, own, trusted sender) map with `chat_identities`. For every email whose entry changed, call `threads_with_emails` (it uses the `message_addresses_email` index) and mark those threads dirty. This replaces a whole-archive fingerprint: a new correspondent, a person join or a newly trusted alias dirties only the threads that mention them.
3. **Drain.** Take `dirty_threads` 500 at a time. Expand each batch to whole topic units: every thread sharing a thread root with a dirty one, through `message_facts(archive_id, root_rfc_id)`. Load `topic_inputs` (messages, addresses, facts, media, locations, labels, alias rows, existing placement), run `place`, and call `apply_chats`. A dirty thread that no longer has messages (it was absorbed by a merge, or its last message was deleted) is covered with an empty input, which removes its rows and clears the mark.
   - **A message without facts** (the process stopped between `insert_message` and `set_message_facts`, or ingest failed in between) is placed from its `messages` row alone: role `message`, `at` from `messages.date`, text from `messages.preview`; it adds no vote to the thread root and joins its store thread's topic (`message_parents` has no order, so it cannot say which reference is the root). When its facts are written later, the facts trigger re-places it. Nothing waits on it, and nothing is hidden.
4. **Save** the new identity map in the same `apply_chats` that covers the last batch.

A refresh after a normal sync touches the new threads plus the few whose identities changed.

### 8.6 Rebuild: whole

`rebuild(store, archive)` pages `thread_ids` 500 at a time, groups them into topic units, places them, and writes into a new generation. `finish_rebuild` swaps it in. It runs:

- when `holtmail_chats::VERSION` changes (start-up pass);
- when `chats_group_limit` changes (requested by `PATCH /api/archives/{id}`, and caught by refresh step 1 if that request is lost);
- when the inferred owner changes (§4.1), logged with the archive id and counts only;
- from `holtmail reindex`;
- when the identity diff would dirty more than half of the archive's threads, as a cheaper path.

**Property.** Incremental refresh after any ingest order equals a full rebuild, row for row (§14.4). The inferred owner is the one piece of kept state (its hysteresis, §4.1), so the property holds for a fixed `chat_state.inferred_owner`.

### 8.7 Facts backfill

When `holtmail_bubble::VERSION` changes, or for an archive created before Chats, facts are filled by the `chat-facts` step of the message walk (`apps/server/src/walk.rs`, INSIGHTS §6.4):

- Its `pending` is `messages_needing_facts` by `(archive, id)`, an anti-join on an index. The walk unions it with the other steps' pending ids, reads and parses each blob once for all of them, two at a time, and pauses between pages. It needs no cursor: the anti-join is the progress, so `chat_state.facts_done` is not kept.
- The walk runs after `people::build_if_needed` in the existing start-up task (`apps/server/src/main.rs`), then on `WalkWake` and hourly. It never blocks start-up or requests.
- Because every run evaluates the anti-join, even when the version is unchanged, messages whose facts were lost to a crash between ingest steps are found without a version bump.
- Only an archive whose chats were **never** built shows "Preparing chats · N%" instead of a half-built list. On a version bump the existing chats stay readable, and a thin progress line in the chat list says that bubbles are being refreshed; a full re-parse of a large archive takes hours, and hiding Chats for that long would be worse than showing old facts. Progress comes from the walk's `analyse` task.
- Every phase that adds a detector bumps `holtmail_bubble::VERSION`, so every phase upgrade re-parses the whole archive once. This is accepted; the walk is resumable and throttled, and a bump that coincides with an insights fingerprint change costs one parse, not two.

### 8.8 Server wiring

- **Ingest.** After `insert_message` of new bytes, `bubble::facts(&raw, &parsed)` under `catch_unwind`, then `set_message_facts`. These are separate store calls, not one transaction with the insert; §8.5 covers a crash in between. Triggers do the dirty marking. Exchange journal reports are unwrapped by `journal_original` before this point (in the SMTP path), so facts see the original message.
- **Queue.** Phase 0 first renames `PeopleRefresh` (`apps/server/src/people.rs`) to `DerivedRefresh`, as its own small change with no behavior change, and then adds the `chats_only` request kind. Phase 1 gives it a second step: people rebuild, then `chats::refresh`, with the same coalescing (at most two runs per burst). Chats always sees current people.
- **Triggers.** Today the people rebuild is requested only when a mail sync stored new messages (`report.new_messages() > 0` in `mailboxes.rs`), after imports, after contact changes, after deletions are confirmed (`operations.rs`) and after alias changes (`aliases.rs`, which from Phase 0 also covers a change in `alias_contacts`). That is not enough for Chats, whose dirty rows also come from moves, removals and label changes that add no message. So:
  - a mail sync whose report has `new + known + removed + flags_changed > 0` in any folder requests a refresh (labels arrive as `flags_changed`);
  - SMTP/journal ingestion (`apps/server/src/smtp.rs`, which requests nothing today) requests one per accepted batch;
  - account edits and account deletion request one (the own set changes), and so does an own-address override;
  - a change of `chats_group_limit` requests a chats rebuild;
  - when only Chats has work (`chat_dirty` not empty, people unchanged), the queue may skip the people step. A `chats_only` request kind carries this, so a move out of Junk does not rebuild people.
- **Start-up.** `chats_if_needed` after people, gated by settings key `chats` = `"{bubble::VERSION}.{chats::VERSION}"`. Facts come from the walk's `chat-facts` step (§8.7), which runs after it in the same start-up task.
- **`holtmail reindex`** fills missing or stale facts as it reads each message (as it does insights), and gains the step "chats rebuild" after people.
- **Logs** carry archive id, counts, timings and versions only, plus a message id on a classifier failure. Never text, subjects, addresses, emoji or file names.

## 9. Reads

### 9.1 A stream page

One page of 50 items is:

1. one keyset query on `chat_messages` joined with `message_facts` (text, segments, preview, `reply_rfc_id`, forward);
2. one batch `attachment_media` read (a new batch read, replacing one `attachments()` call per message);
3. one batch `marks` read;
4. one batch reply-preview lookup by `reply_rfc_id`;
5. one batch flags read from `locations` for unread and pinned.

No blob is read and nothing is parsed. Invitation cards reuse the existing iMIP tables, also batched.

### 9.2 Unread and pinned

- **Unread**: a canonical bubble from someone else that has at least one live location in an account of kind `imap`, `jmap` or `graph`, and no copy carrying `\Seen` (`locations.flags` is a JSON array, read with `json_each` as the store already does). My own bubbles are never unread. Locations in `import` accounts do not count: mbox and `.eml` imports record no flags and the SMTP listener records none either, so counting them would show a whole imported history and every journaled message as unread. Maildir imports do carry `\Seen`, but their mail is history too. Mail removed from every server folder is read. A chat row shows the count, capped at 99+ through a `LIMIT 100` subquery. The stream shows a "New messages" divider above the first unread bubble.
- **Pinned**: bubbles with `\Flagged` on any copy. A pinned bar at the top of the chat cycles through them on click.
- Both are computed at read time. Holtmail never writes flags from the chat view.

## 10. API (`crates/api/src/chats.rs`)

### 10.1 Routes

All routes sit under `/api/archives/{id}/…`, so `access::check` resolves the archive from the path, or under `/api/messages/{id}/…`, which the existing `messages` arm resolves. There is no top-level `/api/chats/…` and no `/api/people/{id}/…` route: the `people` arm matches two segments only, so a three-segment GET would go unchecked. Handlers check that a chat or topic belongs to the path's archive and answer 404 otherwise. Members get 404 for archives they cannot read.

| Route | Returns |
|---|---|
| `GET /api/archives/{id}/chats?filter=all\|people\|groups\|channels\|notifications\|saved&q=&person=&cursor=&limit=` | `ChatPage {items: ChatSummaryDto[], next}`. `all` leaves out notifications, which appear as one collapsed row with a count. `q` matches titles and member names or emails. `person` lists the chats with that person ("Groups in common"). |
| `GET /api/archives/{id}/chats/status` | `{ready, facts_progress, refreshed_at, pending}` for the "Preparing chats" state and for live updates: `refreshed_at` changes after every `apply_chats`, `pending` says whether `chat_dirty` has rows |
| `GET /api/archives/{id}/chats/{chat}` | `ChatDetailDto` |
| `GET /api/archives/{id}/chats/{chat}/topics?cursor=&limit=` | `TopicPage` |
| `GET /api/archives/{id}/chats/{chat}/items?topic=&before=&after=&around=&limit=` | `ChatItemWindow {items: ChatItemDto[], before, after}` |
| `GET /api/archives/{id}/chats/{chat}/days?topic=` | per-day counts for jump to date |
| `GET /api/archives/{id}/chats/{chat}/media?kind=photo\|video\|voice\|file\|link&cursor=` | shared-media tabs |
| `GET /api/archives/{id}/chats/{chat}/search?q=&cursor=` | Phase 4. Hits from `holtmail_query` intersected with `chat_messages`, returned in stream order, never through the index's stale `thread_id`. |
| `PUT /api/archives/{id}/chats/{chat}/prefs` | `{forum: bool \| null}` for the signed-in user, stored under the chat's key (§3.5); `ChatDetailDto.forum` reflects it |
| `GET /api/archives/{id}/identities` | the own addresses with source, strength and an `uncertain` flag |
| `POST /api/archives/{id}/identities` | `{email, own: bool \| null}`: sets or clears an own-address override (§4.1). Phase 1 uses it for "Confirm / choose another"; Phase 4 adds the page. |
| `PATCH /api/archives/{id}` (existing) | gains `chats_group_limit` in `ArchiveUpdateRequest`, at least 2; a change requests a chats rebuild |
| `GET /api/messages/{id}/chat` | `{chat_id, topic_id, message_id}` (the canonical copy), for "Show in chat" and stale URLs |
| `GET /api/messages/{id}/clean` | the full clean text, segments and cut parts |

Every handler has `#[utoipa::path]` and is listed in `ApiDoc`. DTOs derive `ToSchema` and query structs `IntoParams`. A bad cursor or an `around` id outside the chat gives 400.

### 10.2 DTOs

**`ChatSummaryDto`**:

- `id`, `kind`, `title`, `forum`
- `members` (at most 3), `member_count`
- `topic_count`, `message_count`, `last_at`
- `last: {author, mine, kind, preview, media_label}`
- `unread`, `muted_kind` (notifications), `domain` (the sending domain of a notifications chat, for the view-only grouping of §3.2)

**`ChatDetailDto`**: the summary fields plus

- `members[≤ 50] {key, person_id, name, email, unverified}`
- `also_in` (group count for direct chats)
- `pinned[] {message_id, preview}`
- `media_counts`

**`TopicSummaryDto`**:

- `id`, `title`, `general`, `split_note`
- `message_count`, `last_at`
- `last: {author, preview}`, `unread`

**`ChatItemDto`**:

- **identity:** `message_id`, `rfc_message_id`, `topic_id`, `topic_title`, `at`, `kind` (`bubble`, `system`), `role`, `mine`
- **author:** `author {key, person_id, name, email, unverified, on_behalf_of}`, `claim` (`from` or `sender` when the bubble claims my address without evidence, §4.2), `bcc_to_me` (§3.3)
- **text:** `text`, `truncated`, `segments[]`, `cut`, `subject_line` (General topic and topic starts)
- **reply:** `reply_to {message_id, chat_id, author, snippet, media_label} | {missing: true} | null`
- **forward:** `forward {from, date, subject, text, media[]}`
- **media:** `media[] {ordinal, kind, content_type, filename, size, width, height, animated, duration_ms, url, thumbnail_url}`
- **cards:** `invitations[]` (the existing `InvitationDto`), `contact {name, emails, phones}`, `rich {subject, preview}`
- **marks:** `reactions[] {emoji, count, mine, authors[]}`, `status {sent, read_by[], failed[] {recipient, code, at, heuristic}}`
- **system line:** `system {type, actor, target, value}`
- **flags and copies:** `unread`, `pinned`, `copies`, `differs` (another copy with the same Message-ID has other content, §7.4), `has_html`, `list_id`

### 10.3 Media serving

The attachment route (`GET /api/messages/{id}/attachments/{ordinal}`, handler `routes::get_attachment`, which builds its response with the shared `routes::download` helper) changes like this. Today it serves `image/{png,jpeg,gif,webp,avif}` and `application/pdf` inline and everything else as `application/octet-stream`, from one whole-blob `BlobStore::get`:

- **Allowlist.** Real types with `inline` disposition for `audio/{mpeg,mp4,aac,ogg,opus,wav,webm,flac}` and `video/{mp4,webm,quicktime}`, on top of today's images and PDF. Never SVG or HTML.
- **Sniffing.** The declared type is confirmed against magic bytes. On a mismatch the part falls back to a download.
- **Range.** `Accept-Ranges: bytes`, `206` with `Content-Range`, `416` for a bad range. Phase 3 serves this through `BlobStore::get_range`.
- **Unchanged.** `CSP: sandbox`, `nosniff` and `private, max-age=86400, immutable` caching stay. The thumbnail route keeps its semaphore of 2 (`THUMBNAILS`); the waveform reuses the same pattern.
- **Waveform.** `GET /api/messages/{id}/attachments/{n}/waveform` (Phase 3) returns `{duration_ms, peaks[64]}` from `DerivedKind::Waveform`, keyed by the attachment blob. It is computed lazily, for at most 20 MiB and 15 minutes, behind a semaphore of 2, with the `Nothing` sentinel on failure. The voice bubble reserves its height, so the waveform appearing never shifts layout.

## 11. Web

### 11.1 Routes and layout

- **Route.** `chatsRoute` is a lazy child of `archiveRoute`: `/a/$archiveId/chats?c=&t=&m=&q=&f=`.
  - `c` is the chat, `t` the topic id or `all`, `m` the anchor message, `q` the list search, `f` the list filter.
  - `parseChatsSearch` lives in `lib/search-params.ts`, with tests.
  - When `c` or `t` is unknown and `m` is present, the client calls `GET /api/messages/{m}/chat` and replaces the URL.
  - `archiveRoute` already validates `m` with `parseMailSearch`, and `MailLayout` treats `m` as "the message open in the Reader" in every mode except People and Calendar (`selectedId = isCalendar ? undefined : isPeople ? personId : messageId`). In Chats `m` is only the stream anchor, so the Chats row of the mode table must set `selectionKey: 'c'` and never render the Reader. A test in `search-params.test.ts` and one for the mode table pin this down.
- **`MailLayout`.** The `isPeople`/`isCalendar` booleans become a small mode table `{match, listPane, detailPane, emptyPane, selectionKey}`, refactored first with no behavior change. Chats is then one more row. `sidebar.tsx` also takes `isPeople`/`isCalendar` props and moves to the same table.
- **Sidebar and palette.** A "Chats" link with `MessagesSquareIcon`, and a palette item "Go to chats".

| Layout | Behavior |
|---|---|
| Wide | The chat list sits in the list pane. Opening a forum collapses the list into an avatar rail (new layout token `--hm-layout-pane-rail`, 4rem). A topic column (`w-pane-list`) slides in with the expressive preset, and the stream fills the rest. |
| Medium, narrow | The existing overlay stack, `m.section absolute inset-0`: list → topics → stream, each sliding over the previous one. `Esc` and Back go up one level. |

### 11.2 Components (`apps/web/src/chats/`)

| Component | Notes |
|---|---|
| `chat-list-pane.tsx`, `chat-row.tsx` | Virtualized 4rem rows. Filter tabs (All, People, Groups, Channels, Saved) with `ToggleGroup`, plus the collapsed Notifications row, which groups its bot chats by sending domain when opened (view only, `lib/chats.ts`). Each row has a square initials avatar (stacked squares for groups, a hash glyph for lists, a bookmark for Saved), title, preview with an author prefix ("You:", "Ada:") and a kind label ("Voice message 0:12", "Album · 4 photos", "Sticker"), a forum glyph, the date (`formatListDate`) and the unread count. Implements `ListController`, so `j`/`k`/`Enter` work unchanged. |
| `topic-list.tsx` | "All messages" first, then topics: title, last author and snippet, count, date, unread. General is pinned below "All messages". |
| `chat-pane.tsx`, `chat-header.tsx` | Title, members, "Also in N groups", pinned bar, forum toggle (saved through `prefs`), search and info buttons. |
| `owner-banner.tsx` | In the chat list: "Is this you? Confirm / choose another" for an inferred owner, or the empty-own explanation with "choose an address" (§4.1). |
| `chat-stream.tsx` | §11.3 |
| `bubble.tsx`, `reply-preview.tsx`, `quote-segment.tsx`, `system-line.tsx`, `topic-chip.tsx`, `reactions.tsx`, `status-marks.tsx` | Bubble parts. |
| `media/album.tsx`, `media/sticker.tsx`, `media/gif.tsx`, `media/video.tsx`, `media/voice-player.tsx`, `media/audio-row.tsx`, `media/file-row.tsx`, `media/lightbox.tsx` | Media. The voice player and lightbox are owned components built on tokens. |
| `cards/forwarded.tsx`, `cards/contact.tsx`, `cards/rich.tsx` | Invitations reuse `mail/invitation-card.tsx`. |
| `original-dialog.tsx` | §7.6 |
| `chat-info.tsx` | Members and shared-media tabs (Photos, Videos, Voice, Files, Links). |
| `chat-search.tsx`, `jump-to-date.tsx` | Phase 4. |

Pure logic sits in `src/lib/` with tests: `chats.ts` (clustering, day separators, album grouping, preview labels), `emoji.ts`, `album-layout.ts`.

### 11.3 The stream

- **Virtualizer.** TanStack Virtual with `measureElement`, anchored to the bottom. Data is a bidirectional `useInfiniteQuery` (`getPreviousPageParam` and `getNextPageParam`).
- **Stable heights.**
  - No iframe ever enters the stream.
  - Media boxes take `aspect-ratio` from the stored dimensions.
  - Marks arrive with the page.
  - The voice bubble has a fixed height.

  So a row's height is known when it first renders.
- **Prepending older pages.** A layout effect restores the offset of the anchor element, so nothing jumps.
- **Jumping.** `?m=` switches the query to an `around` window, and the target bubble gets a brief accent left bar (opacity only, removed under reduced motion). Reply previews, search hits, pinned messages and "Show in chat" all jump this way.
- **Clustering.**
  - Consecutive bubbles from one author within 5 minutes form a cluster: 0.125rem gaps inside, 0.75rem between clusters.
  - In groups the author name sits on the first bubble of a cluster and the avatar on the last. In broadcasts and lists every bubble names its author.
  - Bubble markers ("Bcc'd to you", "claims to be you", "differs from another copy") sit in the bubble footer before the time, in `fg-muted`, with the warning glyph for a claim; they are part of the row's first render, so they never shift layout.
- **Scrolling chrome.**
  - Day separators per day, reusing `dayGroup`.
  - A sticky day pill shows while scrolling and fades 1 s after it stops.
  - A square "scroll to bottom" button carries the unread count.
  - No animation while the list scrolls (`data-scrolling`).
- **Live updates.**
  - Today `use-live-archive.ts` invalidates the archive, folders, labels and messages keys every 4 s **only while an account sync is running**, and once more when the last one stops; `accountsQuery` polls every 2 s while a sync runs and every 60 s otherwise. Chats cannot rely on that alone: its rows appear only after the refresh that runs *after* the sync, so an invalidation at "sync finished" often reads the old placement.
  - So the chat views poll `GET /api/archives/{id}/chats/status` (every 4 s while a sync runs or `pending` is true, every 60 s otherwise) and invalidate `['archives', id, 'chats']` when `refreshed_at` changes. This also covers SMTP ingestion and watch-driven syncs, which the accounts poll may miss between ticks.
  - New bubbles are appended in place only when the view is already at the bottom. Otherwise a "New messages" pill appears.
- **Accessibility.** The stream is a `role="log"` region. Each bubble is labelled, e.g. "Ada, 14:02, voice message 12 seconds".

### 11.4 Design-system constraints

- **Shapes.** Square everywhere: no radius, no tails, no shadows.
  - Theirs: `bg-bubble-in` with a hairline `border-bubble-border`, left-aligned.
  - Mine: `bg-bubble-out` with `text-bubble-out-fg`, right-aligned.
  - Maximum width is the smaller of 36rem and 80%.
- **Tokens.** New semantic colors `bubble-in`, `bubble-out`, `bubble-out-fg` and `bubble-border`. Each goes into `semanticColors` (`packages/tokens/src/index.ts`), all three theme blocks in `tokens.css` (`:root`, the `prefers-color-scheme: dark` block for `:root:not([data-theme='light'])`, and `:root[data-theme='dark']`) and the `@theme` mapping in `theme.css`, so the token tests cover them. Also new: three emoji type sizes, `--hm-layout-pane-rail` (next to `--hm-layout-pane-list`) and `--hm-font-emoji` (`'Apple Color Emoji', 'Segoe UI Emoji', 'Noto Color Emoji', sans-serif`). IBM Plex has no emoji, and `--hm-font-sans` names no emoji family today, so large emoji would otherwise depend on the browser's fallback. Emoji are never loaded as images from a CDN.
- **Accent.** The accent is never a bubble fill. Following `docs/design.md` ("focus, selection and links only"), it stays for links, focus, selection and the jump highlight (a selection). My reactions and the unread count do not use it: the count is a small `fg`-on-`bg` inverted square.
- **Footer and type.** Time sits bottom-right in mono `text-xs tabular-nums`, followed by the status glyphs.
- **No box.** Emoji-only messages, stickers and GIFs have no bubble box, which also offsets the lack of roundness.
- **Text safety.** Text renders as React text through `linkify()` (`lib/text.ts`) with `whitespace-pre-wrap`. Links get `noopener noreferrer`, with the full URL in a tooltip.
- **Scripts and long lines.** Every bubble text block has `dir="auto"`, so Arabic or Hebrew mail aligns correctly inside a left- or right-aligned bubble, and `overflow-wrap: anywhere`, so a long URL or a base64 blob never widens the stream.

### 11.5 Keyboard

Added to `lib/keymap.ts` (`keymap` and `shortcutList`), `ShortcutsDialog` and `strings.shortcuts.items`. All other strings go into a `strings.chats` group.

The global keymap already binds `j`/`down` and `k`/`up` to next/previous, `enter`/`o` to open, `esc` to back and `/` to search, and the Calendar binds `t`, `a`, `w`, `m` and the arrows. Chats keeps those meanings instead of overloading them: the key acts on the **focused region** (the chat list, the topic list or the stream), the way it already acts on the focused list in mail.

| Key | Action |
|---|---|
| `g m` | go to chats (free today: `g i`, `g a`, `g c`, `g e` are taken) |
| `j` / `k`, `↓` / `↑` | next / previous row in the focused list; next / previous bubble in the stream |
| `Enter`, `o` | open the chat or topic; in the stream, open the focused bubble's menu |
| `Esc` | up one level (stream → topics → list) |
| `Tab` / `Shift+Tab` | move focus between list, topics and stream |
| `[` / `]` | previous / next topic |
| `r` | jump to the replied-to message |
| `Shift+O` | show original (`o` stays "open", as everywhere else) |
| `v` | open in mail |
| `Space` | play or pause the focused voice message |
| `End` | jump to latest |
| `/` | search: the chat list when it is focused, the open chat when the stream is focused (Phase 4; before that, always the chat list) |
| `n` / `N` | next / previous hit (Phase 4) |
| `d` | jump to date (Phase 4) |

### 11.6 Entry points

- The mail `Reader` toolbar gets "Show in chat".
- `PersonPane` gets "Open chat" and "Groups in common".
- The bubble menu gets "Open in mail" and "Copy link".

## 12. Failure modes and graceful degradation

| Failure | Behavior |
|---|---|
| Quote marker missed (new client or language) | The bubble is longer but complete. |
| Over-stripping | `cut.failed` restores the full text. "Quoted text" toggles are always present. |
| Malformed HTML | Falls back to mail-parser's html→text plus the text rules. |
| Classifier panic or error | Caught per message; stored as a text bubble from `preview`; logged by message id. |
| Unknown or invalid reaction payload | Ordinary bubble. |
| Mark before its target | Waits in `unresolved_marks` and shows as a muted system line; attaches when the target arrives. |
| Parent not in the archive | "Reply to a message not in the archive". |
| Thread merge | Rows are replaced by message id; the topic id comes from the root, so it usually stays the same. |
| Topic or chat id changed (person join or split, older root) | URLs carry `m=`; `GET /api/messages/{m}/chat` redirects. |
| Same message from two accounts, no references | One topic via the root; one bubble via the duplicate rule. |
| Same `Message-ID`, different content (a buggy mailer, or a copy crafted to replace mine) | Separate bubbles; the non-preferred one is marked "differs"; my evidenced copy is always preferred (§7.4). |
| List echo of my post with a footer | Collapses into my evidenced copy under the narrow list-echo rule; any other change keeps it separate. |
| Forged `From` or `Sender` of mine | No evidence, so a claim: on the left with "claims to be you", never mine, never in Saved messages, never merged with my copy (§4.2). |
| My real message held only as a received copy without evidence (an account not in the archive, no aligned DKIM) | Shown as a claim on the left. Honest about what can be proven; adding the account or an outgoing SMTP route fixes it on the next refresh. |
| Bcc'd copy | Placed with its visible parties, marked "Bcc'd to you". |
| Untrusted reverse alias | No renaming, no merge, no "own" addition; "unverified relay" hint. A decoded reverse address (`decode_reverse`) is a display hint only. |
| Own set empty | Inferred owner under the sample, margin and hysteresis rules of §4.1 (weak, `uncertain`, with a "Is this you?" banner); otherwise all bubbles theirs, parties keep every address, and a banner explains it. Chat ids change once the owner becomes known; `m=` redirects cover old links. |
| Inferred owner unstable (two near-equal candidates) | No inference below a 3× margin; once inferred, kept until the margin falls below 2×, so it never flips back and forth. |
| Own set polluted (shared mailbox) | Weak source flagged `uncertain` on the identities endpoint; an `own: false` override removes it (minimal in Phase 1, the page in Phase 4). |
| More than `chats_group_limit` (default 25) parties, lists, bots | Broadcast, list or notifications chat, never a giant group. Changing the limit rebuilds chats without changing ids. |
| DMARC-rewritten list `From` | Author from `X-Original-From` or `Reply-To`. |
| Absurd `Date` header | `at` from the top `Received:` header. |
| Unplayable audio or video | Same bubble shape with Download. |
| HEIC/AVIF without a thumbnail | File row. |
| Declared media type does not match the bytes | Served as a download. |
| Facts backfill still running | "Preparing chats · N%" for that archive; mail view unaffected. |
| Process killed between ingest and refresh | Dirty rows are durable; the next refresh picks them up. |
| Drafts and junk | Excluded by every location ever held, `\Draft` and Gmail labels; a sent draft whose Drafts location was removed stays excluded; moving out of Junk dirties the thread and re-places it. |
| Thread of only marks (a DSN without references) | No chat of its own; marks attach to targets, orphans show in the sender's notifications chat. |
| Automated root with a human `Reply-To` | The `Reply-To` is the party, so my reply lands in a direct chat with that person. |
| Facts missing after a crash | Placed from the `messages` row; the start-up check writes the facts and re-places it. |
| Location re-seen on every sync | The `WHEN OLD.removed_at IS NOT NEW.removed_at` guard keeps it from dirtying anything. |
| Imported or SMTP-journaled mail (locations in `import` accounts, usually without flags) | Read, never unread; excluded only if its import folder has the `drafts`/`junk` role; Sent mark and evidence only from a `sent` folder (an SMTP route marked `sent = true` included). |
| Outlook reactions | Not shown before Phase 4; then only from Graph accounts, after the property is verified on a test tenant. JMAP has none. |

## 13. Performance targets

Measured on a generated archive of 1M messages and 300k threads, SQLite on SSD, release build. The checks are an ignored test in `holtmail-chats`.

| Operation | Target |
|---|---|
| Chat list page (50 rows, with unread counts) | < 50 ms p95 |
| Stream page (50 items with media, marks, replies, flags) | < 30 ms p95, zero blob reads |
| `around` window | < 40 ms p95 |
| Incremental refresh of one sync batch (100 threads) | < 200 ms |
| Identity-diff step when nothing changed | < 100 ms |
| Full chats rebuild (facts already present) | < 60 s, memory bounded by one 500-thread page plus the identity map |
| Facts backfill | about 2 parses at a time, resumable; never blocks requests |
| Extra storage | about 0.6 KB per message on average (facts, clean text ≤ 4 KiB, placement), so about 600 MB per 1M messages, small next to the raw mail. Most messages are far below the cap; raising it from 2 KiB to 4 KiB adds bytes only for the long ones. |

## 14. Test plan

All fixtures are hand-written `.eml` files with invented names and addresses on `example.com`/`example.org`. Media payloads are tiny generated bytes: a 2×2 PNG, a minimal GIF with two frames, a minimal OGG header.

### 14.1 `holtmail-mime` (Phase 0)

`body_parts()` fixtures prove where each part lands:

- Gmail reaction inside `multipart/alternative`;
- MDN `multipart/report`;
- DSN with `text/rfc822-headers`;
- Apple text → image → text;
- inline PGP, `multipart/signed`, `multipart/digest`.

### 14.2 `holtmail-quote` and `holtmail-bubble` (`tests/fixtures/`)

- **Quoting:**
  - `gmail-top-post.eml`, `gmail-bottom-post.eml`, `inline-reply.eml`;
  - `outlook-divrplyfwdmsg.eml`, `outlook-header-block-de.eml`;
  - `apple-blockquote-cite.eml`, `thunderbird-moz-cite.eml`;
  - `ru-napisal.eml`, `fr-a-ecrit.eml`, `pl-w-dniu.eml`, `zh-xiedao.eml`, `ja-message.eml`;
  - `wrapped-attribution.eml`, `attribution-without-quote.eml` (must not cut).
- **Signatures:** `sig-dashdash.eml`, `sent-from-iphone-ru.eml`, `confidential-sentence.eml` (must not cut), `quote-only-body.eml` (must not empty).
- **HTML:** `html-only.eml`, `html-logo-cid.eml` (the logo is not a photo), `html-newsletter.eml` (rich card).
- **Marks:** `gmail-reaction.eml`, `gmail-reaction-bad-json.eml`, `mdn-displayed.eml`, `mdn-deleted.eml`, `dsn-failed-two-recipients.eml`, `dsn-delayed.eml`, `bounce-nonstandard.eml`.
- **Forwards:** `forward-rfc822.eml`, `forward-rfc822-with-comment.eml`, `forward-inline-gmail.eml`, `forward-inline-ru.eml`, `forward-nested-3.eml` (depth limit).
- **Media:** `album-4.eml`, `sticker-webp.eml`, `gif-animated.eml`, `voice-ogg.eml`, `voice-amr.eml`, `voicemail-asterisk.eml`, `video-mp4.eml`, `apple-text-image-text.eml`, `smime-signed.eml`, `winmail.eml`.
- **Cards:** `vcard.eml`, `invite-request.eml`, `invite-reply-accepted.eml`.
- **Senders and lists:** `list-post.eml`, `list-dmarc-rewrite.eml`, `list-offlist-reply.eml`, `list-digest.eml`, `auto-reply.eml`, `noreply-notification.eml`, `noreply-with-reply-to.eml`, `notify-plus-tag.eml`, `encrypted.eml`, `encrypted-inline-pgp.eml`, `pgp-signed.eml`.
- **Text direction and size:** `rtl-hebrew.eml`, `long-url-line.eml`, `html-remote-img-only.eml` (no photo bubble, nothing fetched).
- **Edge cases:** `emoji-only.eml`, `no-message-id.eml`, `date-1970.eml`, `date-future.eml`.
- **Evidence and copies** (facts only here; the placement tests are in §14.4): `own-dkim-pass.eml` (topmost `Authentication-Results` with `dkim=pass header.d=example.com`), `own-dkim-pass-lower-field.eml` (the pass only in a lower field: no `dkim_pass`), `forged-from-me.eml`, `forged-sender-me.eml`, `same-message-id-a.eml` and `same-message-id-b.eml` (one `Message-ID`, different bodies: different `text_hash`), `list-echo-footer.eml` (my post plus a list footer), `list-echo-edited.eml` (one word changed), `bcc-only-copy.eml`.

Table-driven unit tests cover subject normalization, attribution and separator recognition per language, and the property that cleaning never empties a message with text.

### 14.3 Store conformance area `chats` (`holtmail_store::testing`)

1. **Facts.** Idempotent; `messages_needing_facts` respects the version.
2. **Dirty marks.** Message insert, thread merge, location add, `mark_removed`, location delete through `delete_account`, label change, facts write and message delete each mark the thread dirty, durably. A target insert marks the waiting mark-source threads. Re-recording an unchanged location and `set_flags` mark nothing. `delete_archive` leaves no `chat_dirty` row behind.
3. **Dirty race.** A thread marked again after `dirty_threads` stays dirty after `apply_chats`.
4. **`apply_chats` atomicity.** It replaces rows by message id, recomputes the old and the new chat's aggregates, and deletes chats and topics left empty.
5. **Generation swap.** No mixed reads; `chat_state` is stored; a thread dirtied during the rebuild stays dirty after `finish_rebuild`; `apply_chats` during a rebuild is `Invalid`. `begin_rebuild` discards any unfinished generation first, so a rebuild cut short by a restart is simply started again by the start-up pass and never blocks refresh for good.
6. **Paging.** `chat_items` `before`/`after`/`around` with no gaps or overlaps; limit clamped; a foreign or garbled cursor gives `Invalid`; list order `last_at DESC, id DESC`.
7. **Unread and pinned** follow flag changes without a refresh.
8. **Deletion.** Deleting a message removes its rows and updates aggregates; deleting the last one removes the topic and chat; `delete_archive` clears every table.
9. **Isolation.** The same mail in two archives gives separate chats with different ids.
10. **`locate`.** `None` before refresh, the right pair after, and the canonical copy for a duplicate.
11. **User state.** `chat_prefs` and own-address overrides survive a rebuild and a generation swap; `delete_archive` removes them; deleting a user removes that user's `chat_prefs` only.

The `aliases` conformance area gains `alias_contacts`: replacing a service's rows is atomic, rows of another service or archive are untouched, and removing a service removes its rows. In the alias crates: `decode_reverse` unit tests in `alias-addy` and `alias-duck` (and `None` from the default), and `reverse_aliases` in `alias-simplelogin` against its in-process fake, with two pages of contacts.

### 14.4 `holtmail-chats` (in-memory SQLite store)

- **Order independence (property).** 40 messages across 8 threads, ingested in 50 seeded permutations. Incremental refresh after each permutation equals a full rebuild and equals every other permutation, row for row. The set includes:
  - a late root that merges two threads;
  - a root copy from a second account;
  - a reaction, an MDN and a DSN arriving before their targets;
  - an alias mapping that becomes trusted late;
  - a person join applied midway.
- **Kinds.** Direct, group, broadcast (26 recipients), list, notifications, saved, Bcc-only self send. With `chats_group_limit` = 30 the 26-recipient thread is a group with the same chat id, after a rebuild.
- **Bcc'd to me.** A copy where I am in no visible field lands in the {Ada, Bob} group with `bcc` set; it does not make a stranger a party.
- **CC churn.** One cc on a long direct thread keeps it direct and emits "added"; a cc'd person who writes moves the thread to a group.
- **Notifications.** A noreply sender becomes direct once I reply; an auto-reply stays in the person's chat; a noreply root with a human `Reply-To` becomes a direct chat with that person once I reply; `+tag` variants of one sender are one bot chat.
- **Marks-only threads.** A referenceless DSN whose target is archived attaches to it and creates no chat; one whose target is missing shows in `n:mailer-daemon@…`, never in Saved messages.
- **Owner unknown.**
  - Clear owner: 40 conversational messages, one address a recipient of 30 and the `From` of the replies; inferred as own, `uncertain`.
  - Two near-equal candidates (a couple sharing an import): no inference.
  - Small archive (19 conversational messages, many lists and notifications): no inference.
  - Reply pairs: a candidate with reply pairs wins over one that is only a frequent recipient.
  - Hysteresis: once inferred at a 3× margin, adding mail that brings the margin to 2.5× keeps it; below 2× it is dropped, the change requests a rebuild, and the log line carries no address.
  - Confirming stores an override, after which the address is strong and no longer inferred.
  - With no candidate at all, parties keep every address and nothing panics.
- **Evidence.**
  - A message from my address with a Sent copy is mine; with only an aligned DKIM pass on a copy in an IMAP account it is mine without the Sent mark; the same DKIM pass on a file-import copy is not evidence.
  - `forged-from-me.eml` (no evidence) is theirs, `claim = from`, shown on the left, and never lands in Saved messages: a forged "from me to me" goes to `n:{claimed address}`.
  - `forged-sender-me.eml` (`From` Ada, `Sender` me, no evidence) is Ada's bubble with `claim = sender`, not "me, on behalf of".
  - With a Sent copy, the same delegation shape is mine "on behalf of Ada".
  - An SMTP route without `sent = true` gives no evidence; with it, it does.
  - An Exchange journal report through a `sent = true` route with an own envelope sender is evidence; with a foreign one it is not.
  - An aligned DKIM pass counts for a listed large provider and for the domain of a strong own address, not for the domain of a weak one.
  - A copy from a strong own address without evidence is "unconfirmed"; from a weak one it is a claim.
- **Facts missing.** A message inserted without facts is placed from its row; writing facts later re-places it and the result equals a rebuild.
- **Lists.** DMARC author; off-list reply split into the direct chat.
- **Duplicates.**
  - Sent plus an identical received copy collapse; colliding Message-IDs with different authors stay separate; canonical choice by the preference order of §7.4, ending with `blob_id`.
  - Same Message-ID, different body (`same-message-id-a/b.eml`, same author): two bubbles, the later one marked `differs`.
  - A forged copy of my message with my Message-ID, an edited body and an earlier `Date`: my evidenced copy stays canonical and on the right; the forged one is a separate claim marked `differs`; reply previews and marks resolve to mine.
  - `list-echo-footer.eml` collapses into my Sent copy; `list-echo-edited.eml` does not.
- **Aliases.**
  - My alias is mine.
  - A signed reverse alias maps to the person.
  - An unsigned one stays `e:` with the unverified flag, and becomes the person once a signed hit for the same mapping exists.
  - An unsigned one listed in `alias_contacts` maps to the person (rule 3).
  - A `decode_reverse` result on an unsigned hit names the person but does not merge chats.
  - A reply through a reverse alias lands in the person's chat.
- **Identity diff.** A person with a second message keeps the same chat id; a new correspondent dirties only their threads.
- **Exclusion and time.** Drafts and junk excluded and re-placed on move; a sent draft whose Drafts location is removed stays excluded; a Gmail All Mail draft (label `Drafts`) is excluded; imported and SMTP mail in role-less `import` folders is included; `at` clamping; reply never above its parent, while the shown time stays the header's.
- **Unread.** Mail whose only locations are in `import` accounts is read; my own mail is never unread.

### 14.5 API (`crates/api/src/tests.rs`)

- Member access: 404 for another archive's chat, topic or message.
- OpenAPI lists every route.
- Range: 206, `Content-Range`, 416.
- Media type allowlist and sniff mismatch → download.
- Bad cursor → 400.
- `GET /api/messages/{id}/chat` for a stale topic.
- Chat prefs are per user: two users of one archive keep different forum choices; a member without access gets 404.
- `POST …/identities` stores an override and requests a refresh; `PATCH /api/archives/{id}` with a new `chats_group_limit` requests a rebuild, and a value below 2 gives 400.

### 14.6 Web

- `lib/chats.test.ts`: clustering, day separators, album grouping, preview labels, authors on every bubble in broadcasts, grouping notifications chats by domain.
- `lib/emoji.test.ts`: flags, keycaps, ZWJ sequences, skin tones, 1–3 vs 4 clusters.
- `lib/album-layout.test.ts`.
- `search-params.test.ts` for `parseChatsSearch`, including that `m` in Chats never selects a Reader message.
- The `MailLayout` mode table: each existing mode keeps its panes and selection key (the Phase 0 no-behavior-change refactor).
- `keymap` tests: no chats binding collides with a global binding in the same focus region.
- Token tests for the new semantic colors.

### 14.7 Integration (`tests/integration`, area `chats`)

Append the fixture set over IMAP to the existing container in shuffled order, sync, and assert the chat list and items DTOs. Then repeat with the set split across two syncs and part of it sent through the SMTP listener, and assert the same DTOs (ids included): mixed sources and import order must not change the result. Move one message out of Junk on the server and assert it appears after the next sync without new mail. The alias fixtures (signed and unsigned reverse aliases) are part of the set. The `aliases` suite gains a check that the SimpleLogin container's contacts endpoint fills `alias_contacts` and that an unsigned message through a listed reverse alias lands in the person's chat.

### 14.8 `docs/VERIFY.md`

What tests cannot prove:

- the bubble look in light, dark and forced themes;
- scroll anchoring on a 10k-message chat;
- voice and video playback with seeking in Safari and Firefox (`quicktime`, `opus`);
- real Gmail reaction shapes, and the Graph `ReactionsSummary` property id and JSON on a test tenant (Phase 4);
- the SimpleLogin contacts endpoint against the hosted service and its official `api.md`;
- evidence on real mail: the topmost `Authentication-Results` of Gmail, Fastmail and Microsoft 365 for a copy of my own message, and an outgoing Postfix or Exchange route with `sent = true`;
- real voicemail systems;
- the forum rail on wide screens;
- large emoji with `--hm-font-emoji` on macOS, Windows and a Linux desktop without Noto Color Emoji;
- right-to-left text inside bubbles on both sides.

## 15. Delivery plan

Each phase ships on its own and leaves `mise run check` green. Docs are updated in the same change:

- a `docs/README.md` for each new crate;
- `crates/store/docs`, `crates/store-sqlite/docs` and `crates/api/docs`;
- `packages/api-client/docs/README.md`;
- `apps/web/docs/README.md` (Routes, Layout, Chats, Keyboard, Motion);
- links from `docs/README.md` (this document is already linked there, next to `ALIASES.md`);
- the phases in `docs/WORKPLAN.md`.

### Phase 0: prerequisites

- [x] The alias work (phases 32–34) is committed (095b032).
- [ ] Migration numbers: `NNNN_alias_contacts.sql` here and `NNNN_chats.sql` in Phase 1 each take the next free number at merge (Insights migrations may land first); this document is renumbered in the same change.
- [ ] Rename `PeopleRefresh` to `DerivedRefresh` as its own small change with no behavior change; then add the `chats_only` request kind (§8.8).
- [ ] Changes to the committed alias code (§4.1, §4.3):
  - [ ] trusted-only own aliases;
  - [ ] the trust rule of §4.3 (signed, the same mapping seen signed once, or listed in `alias_contacts`);
  - [ ] one-row choice instead of independent `MAX()` in `resolved_addresses`;
  - [ ] the `MailAddresses::own` doc comment mentions aliases.
- [ ] Reverse-alias directory (§4.3): `AliasDirectory::reverse_aliases` and `Capabilities::reverse_aliases`; the SimpleLogin implementation, written only after checking the contacts endpoint against the official `api.md`; `alias_contacts` with its `AliasStore` methods and conformance; refreshed by the `holtmail-aliases` directory sync; `docs/ALIASES.md` and the crates' `docs/README.md` updated.
- [ ] `AliasDetector::decode_reverse` (default `None`), implemented in `alias-addy` and `alias-duck`.
- [x] `Mail::dkim_pass_domains()` in `holtmail-alias` for the `dkim_pass` fact (§4.2), parsing through `holtmail-auth-results` (Insights Phase 35).
- [ ] Reader wording (§4.3): in the web reader, "verified" only for a signed hit, "delivered through your alias" for a known but unsigned alias, with the tooltip that explains the difference; `docs/ALIASES.md` updated to the same words.
- [x] `holtmail-people` exports `is_automated_local(&str) -> bool` (Insights Phase 35).
- [ ] The address-normalization helper moves to `holtmail-core`.
- [ ] `holtmail_mime::body_parts(raw)` with the §14.1 fixtures proving where reaction, MDN, DSN and Apple multi-text parts land.
- [ ] `evaluate_dependency` for `html5ever` (pinned to ammonia's version).
- [ ] `MailLayout` mode-table refactor, with no behavior change.

### Phase 1: readable chats

The first version that reads like a messenger.

- [ ] `holtmail-quote`: text and HTML cleaning, layouts, signatures, the never-empty invariant.
- [ ] `holtmail-bubble`: roles `message`, `auto`, `encrypted` and `digest`, plus detection (not yet rendering) of the mark roles; root, parent, `at`, `reply_to`, `links`, list and automation facts; media kinds and dimensions for photo, file and layout image; `VERSION`.
- [ ] `ChatId` and `TopicId` in `holtmail-core`.
- [ ] `ConversationStore`, `NNNN_chats.sql` with triggers, deletion and archive cleanup, `chat_prefs`, conformance area `chats`.
- [ ] `holtmail-chats`: resolver with email keys, the robust inferred owner with hysteresis, identity diff, kinds (direct, group, broadcast, list, notifications, claims, saved, marks-only threads), `chats_group_limit`, the automated-author `Reply-To` rule, topics by root, General, CC rule, the "Bcc'd to you" marker, DMARC author, off-list split, evidence and claims (§4.2), content-checked duplicates with the preferred copy (§7.4), drafts and junk exclusion, refresh, rebuild, property test.
- [ ] Own-address overrides in `PeopleStore` in minimal form (§4.1), `POST …/identities`, and the "Is this you? Confirm / choose another" banner.
- [ ] `Archive::chats_group_limit` with `PATCH /api/archives/{id}` and a field in Settings → Archives; a change requests a rebuild.
- [ ] `sent = true` on `[[smtp.route]]`, giving the route's folder the `sent` role (§4.2), documented in `apps/server/docs/README.md`.
- [ ] `holtmail-bubble` in Phase 1 already recognises the mark roles (`reaction`, `read`, `delivery`, `bounce`, `invite-reply`) so they never render as ordinary bubbles: in Phase 1 they show as the muted system lines of §7.5 ("Read receipt", "Delivery failed"), and Phase 2 turns them into marks on their targets. Otherwise Phase 1 would show every read receipt and bounce as a full bubble with quoted text.
- [ ] Ingest facts, the chained refresh and its extra triggers (moves, removals, labels, SMTP, account changes; §8.8), start-up pass including the factless check, facts backfill with progress, `reindex` steps.
- [ ] API: chats, status, chat, topics, items, prefs, `messages/{id}/chat`, `messages/{id}/clean`, identities (list and minimal override).
- [ ] Web:
  - [ ] route, sidebar, palette, chat list with filters and unread, Notifications grouped by domain;
  - [ ] forum (rail and overlay stack) with the server-stored toggle, topics, All messages with topic chips;
  - [ ] bubble markers: "Bcc'd to you", "claims to be you", "differs from another copy";
  - [ ] stream with bubbles, clusters, day separators and pill, reply previews, quote segments;
  - [ ] emoji-only, photos and simple albums, file rows;
  - [ ] pinned bar, "New messages" divider, scroll-to-bottom;
  - [ ] Show original, mail ↔ chat links, live updates, the owner banner;
  - [ ] keyboard (`g m`, `j`/`k`/arrows by focused region, `Enter`/`o`, `Esc`, `Tab`, `[`/`]`, `r`, `Shift+O`, `v`, `End`, `/` on the chat list).
- [ ] Bubble tokens, emoji sizes and the rail token.

### Phase 2: messenger semantics

- [ ] Marks: Gmail reactions, MDN read marks, per-recipient DSN and heuristic bounces, `unresolved_marks`, orphan system lines.
- [ ] Forwards: attached and inline, plus the comment bubble.
- [ ] Cards: contact cards, invitation cards in bubbles, RSVP system lines, rich newsletter cards.
- [ ] System lines "added" and "renamed".
- [ ] Album mosaic with lightbox, stickers, animated GIFs (in-view playback, reduced motion).
- [ ] "Groups in common", "Open chat" in `PersonPane`, "Also in N groups".

### Phase 3: voice, video and audio

- [ ] `BlobStore::get_range` with conformance; overrides in `blob-fs` and `blob-s3`.
- [ ] Media allowlist with sniffing and Range in the attachment route.
- [ ] Voice bubble (speed, Space, unplayable fallback), audio rows, inline video.
- [ ] `holtmail-media` (after `evaluate_dependency` for `symphonia`), `DerivedKind::Waveform`, the waveform endpoint.
- [ ] Shared-media tabs and chat info panel.

### Phase 4: navigation and trust

- [ ] Search in chat, with hits in stream order and `/`, `n`/`N`.
- [ ] Jump to date (`days`, `d`).
- [ ] Identities page that lists every own address and edits the overrides (the override itself lands in Phase 1).
- [ ] Repeated-footer hiding.
- [ ] Subject-fallback topics: referenceless threads with the same normalized subject in the same chat within 90 days join one topic, flagged `inferred`.
- [ ] `decode_reverse` display hints for reverse aliases in `Cc` (the contract method lands in Phase 0).
- [ ] Outlook reactions through `source-graph` (§7.2): verify the `ReactionsSummary` property id and JSON shape on a test tenant, then carry it through the source contract next to flags, store it per location, place it as reaction marks; invented fixtures from the verified shape. No JMAP equivalent.
- [ ] Outlook recalls, only behind a detector proven by verified invented fixtures.
- [ ] The 1M-message performance run against §13.

## 16. Decisions

Decided by the maintainer on 2026-11-06. The body sections above already follow them.

1. **Bcc-only received copies** land in the chat of the visible parties, with a "Bcc'd to you" marker on the bubble (§3.3).
2. **Group threshold.** 25 stays the default, as the archive setting `chats_group_limit`; a change requests a chats rebuild. Group versus broadcast is a rendering difference (a broadcast is "To N people", has no membership lines and names the author on every bubble), so both share the `g:` key and keep their ids (§3.2, §4.4).
3. **Delegation and evidence.** `Sender` own with `From` not own shows as mine "on behalf of". A message counts as mine, delegated or not, only with evidence: a Sent copy, the outgoing SMTP path, or an aligned DKIM pass in the topmost `Authentication-Results`. Without it the message is a claim, shown on the left as "claims to be you" with a warning mark. Copies with one `Message-ID` collapse only when their content matches (or under the narrow list-echo rule); otherwise the non-preferred copy is marked "differs", and my evidenced copy is always preferred (§4.2, §7.4).
4. **Outlook reactions** come in Phase 4 through `source-graph` only, from the `ReactionsSummary` extended property as server-side metadata next to flags, after the property id and JSON shape are verified on a test tenant. JMAP has none (§7.2).
5. **`decode_reverse`** is added to the alias contract as an optional method. It is a display hint only and never groups, except that a signed hit's decoded value merely confirms the mapping rule 1 already trusts (§4.3).
6. **Forum toggle** is stored on the server per user, keyed by the chat key string, not the UUID (§3.5).
7. **Clean-text cap** is 4 KiB (§6.1, §13).
8. **Notifications** are grouped by sending domain inside the collapsed Notifications row, as a view only, never a key (§3.2).
9. **Inferred owner** is kept, with a minimum sample, a 50% recipient share, a 3× margin, reply pairs preferred, hysteresis down to 2×, always `uncertain` with a "Is this you?" banner; the own-address override moves to Phase 1 in minimal form; a change of inferred owner rebuilds chats (§4.1).
10. **Reverse aliases from service APIs** are done by the Chats work in Phase 0: `AliasDirectory::reverse_aliases`, SimpleLogin contacts, the `alias_contacts` table, which makes rule 3 of §4.3 available.
11. **Reader wording.** "Verified" is reserved for signed hits; a known but unsigned alias says "delivered through your alias", with a tooltip. Phase 0 changes the web reader and `docs/ALIASES.md` (§4.3).
12. **File name.** This document is `docs/CONVERSATIONS.md`, linked from `docs/README.md` like `ALIASES.md`.
13. **Refresh queue.** `PeopleRefresh` is renamed to `DerivedRefresh` first, as its own change; the `chats_only` request kind follows (§8.8).
14. **Show original** is `Shift+O`; `o` stays "open" (§11.5).

### Settled after review

1. **The outgoing SMTP route.** `sent = true` on `[[smtp.route]]` gives its folder the `sent` role. An Exchange journal report through such a route also counts as evidence when its envelope sender is own (§4.2).
2. **DKIM evidence on shared domains.** Counts only for the fixed list of large providers and for domains of strong own addresses; never for the domain of a weak own address (§4.2).
3. **My messages held only as received copies.** With a strong own address the bubble says "Not confirmed as sent by you" with an info glyph; "claims to be you" with the warning glyph stays for weak addresses (§4.2). Both stay on the left and never merge with an evidenced copy.
4. **List-echo footer size.** 1 KiB stays as the starting value; Phase 2 fixtures of invented Mailman, Google Groups and Sympa footers confirm or change it before the rule ships.
