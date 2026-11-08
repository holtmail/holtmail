import type { Identity, Leak, Route, RouteError, RouteReason } from '@holtmail/api-client'

import type { WritingStrings } from '../strings-writing.ts'
import { strings } from '../strings.ts'
import { aliasProviderName } from './aliases.ts'

/** The `From` as recipients see it: `Name <address>` or the address. */
export const routeFrom = (route: Route): string =>
  route.name?.trim() ? `${route.name.trim()} <${route.from}>` : route.from

/** One line saying how the draft leaves: from which address, through which transport and, for
 * replies through an alias service, to whom: "To SimpleLogin's reverse address for
 * bob@example.org, from your mailbox ada@example.com through Fastmail SMTP". */
export function routeSummary(
  route: Route,
  transportName: string | undefined,
  s: WritingStrings = strings.writing,
): string {
  const transport = transportName ?? route.transport
  const service = route.provider ? aliasProviderName(route.provider) : null
  switch (route.strategy) {
    case 'reverse_alias':
      return route.to && route.contact
        ? s.route.reverseAliasFor(service, route.contact, route.from, transport)
        : s.route.reverseAlias(route.from, transport)
    case 'reply_header':
      return route.to
        ? s.route.replyHeaderTo(service, route.to, route.from, transport)
        : s.route.replyHeader(route.from, transport)
    default:
      return route.alias && same(route.alias, route.from)
        ? s.route.asAlias(route.from, transport)
        : s.route.via(route.from, transport)
  }
}

const same = (a: string, b: string) => a.trim().toLowerCase() === b.trim().toLowerCase()

/** The active identity that sends as the alias the original came through: its address is the
 * alias, or it is linked to it. */
export function aliasIdentity(
  route: Route | null,
  identities: readonly Identity[],
): Identity | undefined {
  const alias = route?.alias
  if (!alias) return undefined
  return identities.find(
    (i) => i.active && (same(i.address, alias) || (i.alias != null && same(i.alias, alias))),
  )
}

/** A one-click fix for a leak. */
export type LeakFix =
  | { kind: 'reverse' }
  | { kind: 'identity'; identity: Identity }
  | { kind: 'remove' }

/** The fixes offered for a leak, best first: the reverse address when the server says the
 * service can make one, the alias's identity when the server suggests sending as the alias and
 * one exists, and always taking the recipient off. */
export function leakFixes(
  leak: Leak,
  route: Route | null,
  identities: readonly Identity[],
): LeakFix[] {
  const fixes: LeakFix[] = []
  if (leak.fix === 'use_reverse_alias') fixes.push({ kind: 'reverse' })
  if (leak.fix === 'use_alias_identity') {
    const identity = aliasIdentity(route, identities)
    if (identity) fixes.push({ kind: 'identity', identity })
  }
  fixes.push({ kind: 'remove' })
  return fixes
}

/** Why the route picked what it did, in words; an unknown code reads as a sending rule. */
export function reasonText(reason: RouteReason, s: WritingStrings = strings.writing): string {
  const r = s.route.reasons
  switch (reason.code) {
    case 'chosen':
      return r.chosen()
    case 'original_recipient':
      return r.original_recipient(reason.address ?? '')
    case 'plus_address':
      return r.plus_address(reason.address ?? '')
    case 'alias':
      return r.alias(reason.alias ?? reason.address ?? '')
    case 'account':
      return r.account()
    case 'default':
      return r.default()
    case 'plus_address_dropped':
      return r.plus_address_dropped()
    case 'alias_mailbox':
      return r.alias_mailbox(reason.address ?? '')
    case 'no_alias_mailbox':
      return r.no_alias_mailbox(reason.alias ?? '')
    case 'not_alias_mailbox':
      return r.not_alias_mailbox(reason.address ?? '')
    case 'alias_from':
      return r.alias_from(reason.alias ?? '')
    case 'reverse_alias':
      return r.reverse_alias(reason.address ?? '')
    case 'computed_reverse_alias':
      return r.computed_reverse_alias(reason.address ?? '')
    case 'unsigned_reverse_alias':
      return r.unsigned_reverse_alias()
    case 'reply_header':
      return r.reply_header(reason.address ?? '')
    default:
      return s.route.unknownReason
  }
}

/** Why the draft cannot be sent as it is. */
export function routeErrorText(
  error: RouteError,
  transportName: (id: string) => string | undefined,
  s: WritingStrings = strings.writing,
): string {
  const e = s.route.errors
  switch (error.code) {
    case 'no_identity':
      return e.no_identity
    case 'unknown_identity':
      return e.unknown_identity
    case 'unknown_transport':
      return e.unknown_transport
    case 'not_allowed_from': {
      const id = error.transport ?? ''
      return e.not_allowed_from(error.address ?? '', transportName(id) ?? id)
    }
    case 'needs_reverse_alias':
      return e.needs_reverse_alias(error.alias ?? '')
    case 'no_reply_header':
      return e.no_reply_header(error.alias ?? '')
    case 'no_alias_identity':
      return e.no_alias_identity(error.alias ?? '')
    default:
      return e.no_identity
  }
}

/** Why a recipient would see the real mailbox behind an alias. */
export function leakText(leak: Leak, s: WritingStrings = strings.writing): string {
  const r = s.route.leaks.reasons
  switch (leak.reason) {
    case 'mixed_recipients':
      return r.mixed_recipients(leak.recipient)
    case 'cc_not_reverse_alias':
      return r.cc_not_reverse_alias(leak.recipient)
    case 'bcc_not_reverse_alias':
      return r.bcc_not_reverse_alias(leak.recipient)
    case 'forward_from_real_address':
      return r.forward_from_real_address(leak.recipient)
    default:
      return r.real_address_from(leak.recipient)
  }
}

/** Whether `needs_reverse_alias` still lets the draft be sent: the server makes the address. */
export const sendableDespite = (error: RouteError | null): boolean =>
  error?.code === 'needs_reverse_alias'

/** The recipient lists without `address`, compared case-insensitively. */
export function withoutRecipient<T extends { address: string }>(list: T[], address: string): T[] {
  const lower = address.trim().toLowerCase()
  return list.filter((m) => m.address.trim().toLowerCase() !== lower)
}
