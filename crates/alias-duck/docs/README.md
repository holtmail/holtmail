# holtmail-alias-duck

DuckDuckGo Email Protection. `DuckDetector` recognises mail forwarded from a `@duck.com` address. DuckDuckGo has no public API for listing or managing addresses, so there is no directory; the owner can add addresses by hand.

## Detection

- `From` is a reply address `sender_at_example.com_user@duck.com` (DuckDuckGo's documented form for replying and composing): the part before `_at_` is the sender's local part, up to the last `_` its domain.
- The alias is a recipient on `duck.com` that is not itself a reply address. Without one the message is not recognised.
- The `From` name is kept as the sender's name unless it is an address or `… at …`.
- The reply address is the reverse alias. `signed`: DKIM of `duck.com`.
- `encode_reverse(alias, contact)` writes the reply address for a contact: `jane@duck.com` and `ada@example.com` give `ada_at_example.com_jane@duck.com`; an alias on another domain, or an address that would not read back the same (a `_` in the Duck user, `_at_` in the contact), gives `None`. `decode_reverse` reads the contact from a reply address on the detector's domain. Unit tests check both ways. Replies use the `reverse_alias` strategy.

The exact form of forwarded `From` fields is not documented by DuckDuckGo; see [VERIFY](../../../docs/VERIFY.md).
