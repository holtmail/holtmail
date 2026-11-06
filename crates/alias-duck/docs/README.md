# holtmail-alias-duck

DuckDuckGo Email Protection. `DuckDetector` recognises mail forwarded from a `@duck.com` address. DuckDuckGo has no public API for listing or managing addresses, so there is no directory; the owner can add addresses by hand.

## Detection

- `From` is a reply address `sender_at_example.com_user@duck.com` (DuckDuckGo's documented form for replying and composing): the part before `_at_` is the sender's local part, up to the last `_` its domain.
- The alias is a recipient on `duck.com` that is not itself a reply address. Without one the message is not recognised.
- The `From` name is kept as the sender's name unless it is an address or `… at …`.
- The reply address is the reverse alias. `signed`: DKIM of `duck.com`.

The exact form of forwarded `From` fields is not documented by DuckDuckGo; see [VERIFY](../../../docs/VERIFY.md).
