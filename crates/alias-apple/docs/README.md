# holtmail-alias-apple

Apple Hide My Email (iCloud+) and Sign in with Apple relay addresses. `AppleDetector` recognises mail Apple forwarded. Apple has no public API for either, so there is no directory; the owner copies the addresses from iCloud settings and adds them by hand, and mail to them is matched by recipient.

## Detection

- Hide My Email adds `X-Icloud-Hme: p=<alias>; d=<site>; f=<real address>; r=to; s=<sender>`. The alias is `p`, the sender `s`, named from `From` when Apple kept a name.
- When Apple rewrote `From` to an address of its own (on the relay domain or a signing domain) that is not the sender, it is the reverse alias.
- Sign in with Apple: a recipient on `privaterelay.appleid.com` is the alias, `From` stays the sender.
- `signed`: DKIM of `icloud.com`, `privaterelay.appleid.com` or `apple.com`.
- Replies use the `reverse_alias` strategy: to Apple's rewritten address (the hit's `reverse_alias`, used only when Apple's signature passed), from the iCloud address the alias forwards to. Apple puts no `Reply-To` on forwarded mail, so `reply_header` would find nothing to answer.

The header and its fields come from people who looked at their mail, not from Apple; see [VERIFY](../../../docs/VERIFY.md).
