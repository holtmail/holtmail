# holtmail-alias-rules

Alias detection from rules in the config, for own domains with catch-all addresses, a self-hosted forwarder or any service without a detector of its own. Each `Rule` becomes one `RuleDetector` with the rule's provider id.

## Config

```toml
[[aliases.rules]]
provider = "own-domain"            # lowercase letters, digits and -
domains = ["alias.example.org"]    # a recipient here (or below) is the alias

[[aliases.rules]]
provider = "forwarder"
header = "X-Forwarder"             # only mail with this field
alias_header = "X-Forwarder-Alias" # tried before domains
sender_header = "X-Forwarder-From" # the original sender
signing_domains = ["fwd.example.org"]
```

`RuleDetector::new` refuses a provider id that is not lowercase letters, digits and `-`, and a rule with neither `domains` nor `alias_header`. Domains are compared lowercase, a leading `@` is ignored.

## Detection

- With `header`, mail without that field is not recognised.
- The alias is `alias_header`, else the first recipient on one of `domains`. Mail sent from the alias itself is not recognised.
- The sender is `sender_header` when given; there is no reverse alias.
- `signed` only with `signing_domains` and a passing DKIM signature of one of them.
- `version()` covers the whole rule, so changing a rule makes the server detect stored mail again.
