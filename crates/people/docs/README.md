# holtmail-people

Derives the people of an archive from its contacts and its mail. Contact cards from every address book are grouped into people by a shared email address or phone number; addresses of the mail join the person whose card has them, and frequent correspondents without a card become people from mail. The result is derived data: [`rebuild`] replaces an archive's people through the store's `PeopleStore` and gives the same people with the same ids whenever the contacts, the mail and the splits and joins made by hand are the same. Those overrides (`PersonOverride` in the store) are the only stored state about people. A library over the store contract, like `holtmail-export`: there is one grouping, nothing to swap. Nothing here logs.

## Usage

```rust
let rebuilt = holtmail_people::rebuild(&*store, archive_id).await?; // one archive
let total = holtmail_people::rebuild_all(&*store).await?;           // every archive
```

`Rebuilt { people, with_cards, from_mail }` counts what was stored. `preview(store, archive, overrides)` gives the people with a list of overrides without storing them; the API uses it to check a join before storing it. The server calls `rebuild` after imports, mail syncs with new messages and contacts syncs with changes, `holtmail reindex` calls `rebuild_all`, and `serve` builds every archive once at start until the setting `people` is set (see [apps/server](../../../apps/server/docs/README.md)).

## Grouping (`group`)

`group(archive, cards, mail, overrides) -> Vec<Person>`, pure:

- **Keys.** A card's keys are its normalized emails (`email:<address>`, see `holtmail_pim::normalize_email`) and its E.164 phones (`tel:+…`, see `normalize_phone`). Phones written without a country code have no E.164 form and join nothing.
- **Cards join transitively** (union-find): A and B share an email, B and C a phone, so A, B and C are one person. A name alone never joins cards. Removed cards are grouped like the others, so a person keeps their history.
- **Reasons.** For every key several cards share, the first card with it (by item id) links to each other one: `PersonLink { from, to, key }`. The person page lists them as "same email ada@example.com".
- **Mail.** Every address of `PeopleStore::mail_addresses` (counts per address) whose lowercase form is a card email joins that person. The owner's addresses (`MailAddresses::own`: account addresses and every `From` of mail in a Sent folder or with the Sent label) never bring mail to anyone and are never people from mail.
- **People from mail.** An address without a card is a person when `is_correspondent` says so: the owner wrote to it at least once, or it is in at least `MIN_MESSAGES` (2) messages and its local part does not start like an automatic sender (`noreply`, `no-reply`, `no_reply`, `donotreply`, `do-not-reply`, `do_not_reply`, `mailer-daemon`, `postmaster`, `bounce`). Variants of one address that differ only in letter case are one person.
- **Ids.** `person_id(archive, key)`: a UUIDv8 from the BLAKE3 hash of the archive id and the group's smallest key (`email:…` sorts before `tel:…`; a card with neither key is `item:<item id>`; a person from mail is `email:<address>`). Rebuilding gives the same ids; a card that appears for a person from mail keeps their id when its smallest key is that address; merging two groups keeps the id of the one with the smaller key.
- **Shown fields.** The name is the name most current cards give (`FN`, else `N`; ties go to the card with the smallest item id), else the display name their mail uses most, else none. The first email and phone come from the current cards, else the busiest mail address. `emails` holds the cards' and the mail's addresses; `mail_emails` the same without the owner's, which the API uses to find their mail and meetings.
- **Removed.** A person whose cards were all removed and whose addresses appear in no mail is `removed`; mail keeps them.
- **Differences.** `differences` is true when a field differs (see below) among the current cards.

People come out sorted by id; the input order of cards does not matter.

## Splits and joins made by hand

Owners and admins can split a person or join two (see the [API](../../api/docs/README.md)). Each is stored as a `PersonOverride` that names what survives every rebuild, never a person id: cards as `item:<item id>`, and the address of a person from mail as `email:<address>`. `anchor(person)` is the key a join names a person by (its first card by item id, else its address); `holds(person, key)` says whether a person has a card, address or phone. `group` applies the archive's overrides by id (oldest first) with a union-find that honours cannot-link constraints:

1. **A split always holds.** Its `keys` (the cards moved out) are never in one person with its `from` (the cards that stayed), whatever they share. The moved cards are one person together.
2. **A join** makes its keys one person even without a shared address or phone; an `email:` key no card has brings that address's mail. A join that would put the two sides of a split together is ignored (the API refuses to store one with `409`).
3. **Shared keys** then join cards as before, never across a split. Cards that arrive later and share keys with both sides join the side that was not moved out (the moved cards' only other way in is a key no card of the other side has). In practice a new card with the shared address stays with the rest; a new card with only the moved card's phone joins it.
4. **Ids.** The side that was not moved out keeps the person's id (its smallest key), and the mail of addresses both sides have goes to it. The moved side takes its smallest key no other person took first, else `item:<its first card>`, so it keeps that id through rebuilds too.
5. Keys of cards that no longer exist are ignored (cards are never deleted; removing an archive removes its overrides). The join's link appears among the reasons as `hand:<override id>` ("joined by hand").

Undoing an override (removing it) and rebuilding gives exactly the people there were without it.

## Merged fields (`fields`)

`fields(&[SourceCard]) -> Vec<Field>` merges cards' fields for the person page: `name`, `nickname`, `email`, `phone`, `organization`, `title`, `birthday`, `address`, `note`, each with every value once (`FieldValue { value, sources }`, as the first card writes it) and `differs`.

- Values compare by their normalized form: emails as `normalize_email`, phones by E.164 (else their digits), everything else without regard to letter case and runs of spaces.
- A field `differs` when two current cards that both fill it give different sets of values. A card that leaves a field empty does not disagree. Removed cards still give values (marked removed by the API) but never disagree. Nicknames and notes never count as disagreeing.

`contact_name(contact)` is the card's own name (`FN`, else `N`), never its organization or an address. `card(summary)` and `cards(store, archive)` read stored contacts as `Card`s.

## Limits

- Grouping is all or nothing per archive: a rebuild reads every contact and the per-address mail counts in one pass. That is a few SQL aggregates and an in-memory union-find; incremental updates are not done because one new card can merge any number of groups.
- A split moves cards, not addresses: a person from mail joined to a card is undone by removing the join.
- The API rebuilds the archive right after storing or removing an override; a background rebuild that read the overrides just before may write once more without it, until the next rebuild (any import, sync with changes or `reindex`).
- `Reply-To` is not stored with messages, so it does not count as mail with a person.
