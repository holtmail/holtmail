# holtmail-alias-apple

Apple Hide My Email (iCloud+) and Sign in with Apple relay addresses. `AppleDetector` recognises mail Apple forwarded. Apple has no public API for either, so there is no directory; the owner copies the addresses from iCloud settings and adds them by hand, and mail to them is matched by recipient.

## Detection

- Hide My Email adds `X-Icloud-Hme: p=<alias>; d=<site>; f=<real address>; r=to; s=<sender>`. The alias is `p`, the sender `s`, named from `From` when Apple kept a name.
- When Apple rewrote `From` to an address of its own (on the relay domain or a signing domain) that is not the sender, it is the reverse alias.
- Sign in with Apple: a recipient on `privaterelay.appleid.com` is the alias, `From` stays the sender.
- `signed`: DKIM of `icloud.com`, `privaterelay.appleid.com` or `apple.com`.

The header and its fields come from people who looked at their mail, not from Apple; see [VERIFY](../../../docs/VERIFY.md).
