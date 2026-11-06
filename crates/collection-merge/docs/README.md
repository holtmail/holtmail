# holtmail-collection-merge

Two [`CollectionSource`](../../collection/docs/README.md)s presented as one: the address books of one and the calendars of the other. The server uses it for a JMAP account whose session offers contacts but no calendars: contacts come from JMAP, calendars from CalDAV. A library over the contract only; it depends on no implementation.

## API

```rust
let source = MergedCollections::new(Box::new(jmap), Box::new(caldav));
```

| Item | Guarantees |
| --- | --- |
| `MergedCollections::new(contacts, calendars)` | Lists the address books of `contacts` and the calendars of `calendars`; whatever else either lists is left out |
| `collections()` | Address books first, then calendars, each source in its own order. When both list the same id, the address book wins and the calendar is left out (logged without the id). Fails when either source fails |
| `changes`, `fetch`, `list`, `subscribe` | Go to the source that listed the collection; an id neither listed is looked for in a fresh listing, then answered `Gone`. Collection ids pass through unchanged, so collections already stored keep their ids |
| `watch()` | `None` when neither source watches; the one watch when one does; otherwise both, merged: a notice from either comes through (collections joined, or every collection when either cannot tell), and an error from either ends the merged watch |
| `unsubscribe()` | Goes to the source that made the registration; one this value did not make is handed to both |

## Tests

Unit tests run the collection conformance suite on two in-memory sources (one also listing a calendar and the other an address book, which are left out) and check that each fetch went to its own source, that unknown ids are `Gone`, that a failing source fails the listing, the watch conformance suite with one watching side, merged notices and an error ending the merged watch, and subscriptions going to the collection's source.
