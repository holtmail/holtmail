# holtmail-rules

Rules as flows ([WRITE.md, Rules](../../../docs/WRITE.md#rules)): a trigger and a graph of conditions and actions, stored as versioned JSON per archive. The crate holds the flow model, the node catalogue with its JSON Schemas and documentation in English and Russian, validation, `evaluate` from a flow, an event and its facts to a plan, cron expressions for the `schedule` trigger and the template sandbox. It is pure: no I/O, no clock, no store. The server loads the facts, calls `evaluate` and carries out the plan through the outbox and the writers, where every safety check sits.

## Flow JSON

```json
{
  "format": 1,
  "trigger": { "type": "message.archived", "next": "shop" },
  "nodes": [
    { "id": "shop", "type": "query", "params": { "query": "from:*@shop.example.com" },
      "next": { "yes": "label", "no": "invite" } },
    { "id": "label", "type": "label", "params": { "name": "Orders" }, "next": { "next": "fwd" } },
    { "id": "fwd", "type": "forward", "params": { "to": ["accounting@example.org"], "identity": "work" } },
    { "id": "invite", "type": "has_invitation", "next": { "yes": "rsvp" } },
    { "id": "rsvp", "type": "rsvp", "params": { "answer": "tentative" }, "confirm": true }
  ]
}
```

- `Flow { format, trigger: TriggerNode, nodes: Vec<Node> }`; `Node { id, type, version, params, next: port -> node id, confirm, position }`. Unknown fields are refused. `format` is `FLOW_FORMAT` (1); a newer one is refused, not guessed at. Each node carries the `version` of its type's parameters (all 1 today); a version newer than the catalogue's is refused.
- A node has one target per output port, so a run follows a single path. Conditions have `yes` and `no`, actions and triggers `next`, `stop` none. A port left out ends the run there. `position` is the editor's and the engine ignores it.
- Ids are 1 to 64 of `A-Z a-z 0-9 _ -`; `trigger` is reserved for the trigger in issues. At most `MAX_NODES` (200) nodes.
- `Flow::from_json`, `to_json` (stable order, round-trips), `node(id)`, `queries()` (each `query` node's search, for the caller to check with `holtmail-query`'s parser; this crate does not depend on the search crates).

## Catalogue

`catalogue()`, `node_type(id)` and `describe()` (what `rules/catalogue` serves: id, version, role, ports, capability, triggers, template fields, `confirmable`, docs `{en, ru}` with `title` and `summary`, and the JSON Schema). Schemas come from `schemars`; fields carry an `x-holtmail` hint for the inspector: `template`, `query`, `identity`, `transport`, `folder`, `address`, `addresses`, `calendar`, `address_book`, `webhook`, `cron`, `timezone`, `time`.

| Role | Types |
| --- | --- |
| Trigger | `message.archived`, `message.sending`, `invitation.received`, `event.upcoming` (`minutes_before`), `schedule` (`cron`, `timezone`), `manual` |
| Condition | `query`, `header` (`exists`, `equals`, `contains`, `starts_with`, `ends_with`, case ignored), `alias` (optional service), `trust` (`aligned`, `relayed`, `unverified`, `unknown`), `insight` (kind), `sender_is_person`, `has_invitation`, `counter` (`sender` or `flow` over the last `hour`, `day` or `week`, `above` n), `time_window` (days, `HH:MM` to `HH:MM`, wraps midnight, time zone) |
| Action, local | `label`, `notify`, `webhook`, `stop`, and on `message.sending` `require_confirmation`, `block` |
| Action, `mail.flags` | `set_flags` (`seen`, `flagged`, `answered`) |
| Action, `mail.move` | `move` |
| Action, `mail.delete` | `delete` (to Trash) |
| Action, `mail.send` | `reply` (the auto-reply), `forward`, `send`, and on `message.sending` `set_identity`, `set_transport`, `set_alias_strategy`, `add_bcc` |
| Action, `pim.respond` | `rsvp` |
| Action, `pim.edit` | `create_event`, `create_contact` |

The `identity` of `reply`, `forward` and `send` is an identity id or `auto`: the server lets the route pick, as for mail written by hand (a reply through an alias leaves through it; `send` takes the saver's default identity). Each type lists the triggers it may follow: message conditions need a trigger with a message; actions on a stored message (`label`, `move`, `reply`, ...) do not follow `message.sending`; routing actions follow only `message.sending`; `send` and `create_event` never follow `message.sending`, so a rule cannot send from inside a send. `Capability` names the account capability (`mail.flags`, `mail.move`, `mail.delete`, `mail.send`, `pim.edit`, `pim.respond`); the server maps it to the five locks. `confirm` is allowed only on actions with a capability.

## Validation

`validate(&Flow) -> Result<(), Vec<Issue>>` reports every issue in flow order with a stable `IssueCode` (`format`, `too_many_nodes`, `bad_id`, `duplicate_id`, `unknown_type`, `not_a_trigger`, `trigger_in_graph`, `unsupported_version`, `bad_params`, `not_for_trigger`, `unknown_port`, `unknown_target`, `cycle`, `bad_template`, `bad_cron`, `bad_timezone`, `bad_time`, `confirm_not_allowed`) and a detail that never holds message content. Parameters are checked against their types (unknown fields refused) and their limits (ranges, non-empty fields, addresses, disjoint flags). Nodes no path reaches are allowed, so a half-drawn flow saves. `capabilities(&Flow)` lists what the actions need, for checking the saver's grants on save and again at run time; `counters_used(&Flow)` lists the counters the server must load.

## Evaluation

`evaluate(&Flow, &Event, &dyn Facts) -> Result<Plan, EvalError>`:

- **Pure and deterministic.** The same flow, event and facts give the same plan. Nothing runs; dry runs and "test with this message" are this call.
- **Gates.** An invalid flow is `EvalError::Invalid`. An event of another trigger plans nothing (`ignored: other_trigger`). `message.archived` and `invitation.received` need `Event.live`; otherwise `ignored: not_live`, so backfills, imports, resyncs and rebuilds never run a flow.
- **Path.** From `trigger.next`, one node at a time; `Plan.path` has each node with its `outcome`: `branch` (port), `planned` (index into `actions`), `skipped` (reason) or `stopped`. `stop` sets `Plan.stopped`: the server runs no later flow of the archive for the event.
- **Conditions** decide on `Facts`; on an event without a message they take `no`. A query the facts cannot answer is `EvalError::Facts`, and a counter missing from the sheet is `EvalError::MissingCounter`: nothing is planned rather than a guess.
- **Actions** are `PlannedAction { node, capability, needs_confirmation, action }` with templates rendered. One that cannot apply is skipped with a `Skip` code and the run goes on.
- **RFC 3834 for `reply`.** Skipped for `Auto-Submitted` other than `no` or `X-Auto-Response-Suppress` (`auto_submitted`), `List-Id`, `List-Post` or `List-Unsubscribe` (`mailing_list`), `Precedence: bulk|list|junk`, an empty `Return-Path`, no-reply, daemon, bounce and list-robot senders, the archive's own addresses, and when `FactSheet.last_reply` is less than `days` ago (`replied_recently` with `until`). A planned reply carries `to`, the one sender checked (the real sender behind an alias, else `From`; never `Reply-To`), `days` and `auto_submitted: auto-replied`; the server sends it to `to` only, records it as the flow's last reply to that sender once it is queued, and sets the header. Forwards and sends carry `auto-generated`; a forward of a message from an own address, or of a message with `Auto-Submitted` other than `no` (`auto_submitted`, which covers another rule's forwards and auto-replies), is skipped, so two archives forwarding to each other cannot loop.
- `rsvp` without an invitation, `create_event` whose `starts` does not render to RFC 3339, `create_contact` for an own address or a known person, and a failing template are skipped too.

`Event { id, trigger, at, live }`: the server keys a run by flow version and `id`. `Facts` is `sheet() -> &FactSheet` plus `matches(query)`, which `holtmail-query`'s `Matcher` answers on the message's stored facts; `WithMatcher(sheet, fn)` joins the two. `FactSheet` holds the message (`id`, sender, subject, date, headers, alias, trust, insight kinds and fields, `sender_is_person`), the invitation, the upcoming event, counters, `last_reply` and the archive's own addresses.

## Templates

`template::check` and `template::render` run `minijinja` (Apache-2.0) without the `multi_template`, `macros`, `debug` or loader features: no `include`, `extends`, `import` or macros, nothing reaches the filesystem or the network. Limits: source 16 KiB, output 64 KiB (cut off while rendering), 50 000 fuel, nesting 16. No auto-escaping (bodies are plain text); undefined and missing (`none`) values print as empty. Templates see only `TemplateContext`: `sender {name, address}`, `subject`, `date`, `alias`, `insights` (the server fills `<kind>_<field>`, such as `order_number` or `parcel_number`, from the first shown order, invoice, parcel, reservation and pass of the message; codes and links are never offered), `invitation` and `event` (`summary`, `starts`, `location`, `organizer`) and `now`, all times RFC 3339; `TemplateContext::new(event, sheet)` gives the editor's preview the same values.

## Cron

`cron::Cron::parse` takes five fields (minute, hour, day of month, month, day of week 0–7) with `*`, numbers, ranges, steps and lists; both day fields restricted means either matches. `matches(at, tz)` and `next_after(after, tz)` read the fields in an IANA zone; a minute a daylight-saving change skips never fires, and `next_after` gives up after about four years.

## Tests

Unit tests in `src/tests.rs` (catalogue completeness, validation codes, sandbox limits, cron, RFC 3834, conditions, skips). Golden plans in `tests/golden/<case>.json` (flow, event, facts, query answers) against `<case>.plan.json`; `UPDATE_GOLDEN=1 cargo test -p holtmail-rules --test golden` rewrites them after an intended change.
