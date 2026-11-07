import type {
  CategoryCount,
  CategoryOverride,
  CategoryOverrideRequest,
  CategoryTopic,
  MessageInsights,
  MessageKind,
  OverrideScope,
  Topic,
  Trust,
} from '@holtmail/api-client'

import { strings } from '../strings.ts'

export const kinds: readonly MessageKind[] = ['personal', 'list', 'bulk', 'notification']

const topics: readonly Topic[] = [
  'purchases',
  'travel',
  'finance',
  'health',
  'social',
  'jobs',
  'events',
  'government',
]

export const shownTopics: readonly Topic[] = topics.filter((topic) => topic !== 'health')

const sensitive = new Set<string>(['finance', 'health', 'government'])

export type Category = MessageKind | Topic

function isKind(value: unknown): value is MessageKind {
  return typeof value === 'string' && (kinds as readonly string[]).includes(value)
}

function isTopic(value: unknown): value is Topic {
  return typeof value === 'string' && (topics as readonly string[]).includes(value)
}

export function isCategory(value: unknown): value is Category {
  return isKind(value) || isTopic(value)
}

export function kindLabel(id: string): string {
  return strings.categories.kinds[id] ?? id
}

export function topicLabel(id: string): string {
  return strings.categories.topics[id] ?? id
}

export function categoryLabel(id: string): string {
  return isKind(id) ? kindLabel(id) : topicLabel(id)
}

export interface Chip {
  id: string
  label: string
  overridden: boolean
}

export interface CardChips {
  kind: Chip | undefined
  topics: Chip[]
}

/** The kind and topic chips of a message: applied topics only, never `health`. */
export function cardChips(insights: MessageInsights | null | undefined): CardChips {
  const kind = insights?.kind
  return {
    kind: kind
      ? { id: kind.id, label: kindLabel(kind.id), overridden: kind.overridden }
      : undefined,
    topics: (insights?.topics ?? [])
      .filter((topic) => !topic.held_back && topic.id !== 'health')
      .map((topic) => ({
        id: topic.id,
        label: topicLabel(topic.id),
        overridden: topic.overridden,
      })),
  }
}

export function heldBackText(
  list: readonly CategoryTopic[] | null | undefined,
): string | undefined {
  const held = (list ?? []).filter((topic) => topic.held_back && topic.id !== 'health')
  if (held.length === 0) return undefined
  return strings.categories.heldBack(held.map((topic) => topicLabel(topic.id)))
}

/** The list row's topic chip; `undefined` for none and for `health`. */
export function listTopic(topic: string | null | undefined): Chip | undefined {
  if (!topic || topic === 'health') return undefined
  return { id: topic, label: topicLabel(topic), overridden: false }
}

/** The kinds a list row marks with a glyph. */
export function listKindGlyph(
  kind: string | null | undefined,
): 'bulk' | 'notification' | undefined {
  return kind === 'bulk' || kind === 'notification' ? kind : undefined
}

/** Counts for the sidebar: kinds then topics in taxonomy order, without `health` and zeros. */
export function sidebarCounts(categories: {
  kinds: readonly CategoryCount[]
  topics: readonly CategoryCount[]
}): { kinds: CategoryCount[]; topics: CategoryCount[] } {
  return {
    kinds: categories.kinds.filter((entry) => entry.count > 0),
    topics: categories.topics.filter((entry) => entry.id !== 'health' && entry.count > 0),
  }
}

export interface PaletteEntry {
  id: Category
  text: string
  keywords: string[]
}

export function paletteEntries(): PaletteEntry[] {
  return [...kinds, ...shownTopics].map((id) => {
    const label = categoryLabel(id)
    return { id, text: strings.categories.show(label), keywords: ['category', 'show', id, label] }
  })
}

export interface CategorySearch {
  category?: Category
}

export function parseCategorySearch(search: Record<string, unknown>): CategorySearch {
  const value = typeof search.category === 'string' ? search.category.trim().toLowerCase() : ''
  return isCategory(value) ? { category: value } : {}
}

export interface SenderMessage {
  from: readonly { email: string }[]
  alias?: { sender?: { email: string } | null } | null
}

/** The sender overrides apply to: the alias's claimed sender, else the only From address. */
export function overrideSender(message: SenderMessage): string | undefined {
  const claimed = message.alias?.sender?.email
  if (claimed) return claimed.trim().toLowerCase()
  if (message.from.length !== 1) return undefined
  return message.from[0]?.email.trim().toLowerCase() || undefined
}

function senderDomain(address: string): string | undefined {
  const at = address.lastIndexOf('@')
  const domain = at === -1 ? '' : address.slice(at + 1).replace(/\.$/, '')
  return domain ? domain.toLowerCase() : undefined
}

function coversDomain(key: string, domain: string): boolean {
  return domain === key || domain.endsWith(`.${key}`)
}

export interface SenderOverrides {
  address: CategoryOverride | undefined
  domain: CategoryOverride | undefined
}

/** The stored overrides that reach a sender: its address and the domain that covers it. */
export function senderOverrides(
  overrides: readonly CategoryOverride[] | undefined,
  sender: string,
): SenderOverrides {
  const domain = senderDomain(sender)
  const list = overrides ?? []
  return {
    address: list.find((item) => item.scope === 'address' && item.key === sender),
    domain: domain
      ? list
          .filter((item) => item.scope === 'domain' && coversDomain(item.key, domain))
          .toSorted((a, b) => b.key.length - a.key.length)[0]
      : undefined,
  }
}

export function scopeKey(scope: OverrideScope, sender: string, existing?: SenderOverrides): string {
  if (scope === 'address') return sender
  return existing?.domain?.key ?? senderDomain(sender) ?? sender
}

export function scopeText(scope: OverrideScope, sender: string, existing?: SenderOverrides) {
  const key = scopeKey(scope, sender, existing)
  return scope === 'address'
    ? strings.categories.scopeAddress(key)
    : strings.categories.scopeDomain(key)
}

export interface Draft {
  kind: string | undefined
  topics: readonly string[]
}

/** What the chip menu starts from: the effective kind and every topic the user chose. */
export function initialDraft(insights: MessageInsights | null | undefined): Draft {
  return {
    kind: insights?.kind?.id,
    topics: (insights?.topics ?? []).map((topic) => topic.id),
  }
}

function sameSet(a: readonly string[], b: readonly string[]): boolean {
  const left = new Set(a)
  return left.size === new Set(b).size && b.every((item) => left.has(item))
}

function inOrder(list: readonly string[]): Topic[] {
  return topics.filter((topic) => list.includes(topic))
}

/** The PUT body for a draft: changed axes, plus the axes the replaced override already set;
 * `undefined` when there is nothing to change. */
export function overrideRequest(
  scope: OverrideScope,
  key: string,
  initial: Draft,
  draft: Draft,
  existing: CategoryOverride | undefined,
): CategoryOverrideRequest | undefined {
  const kindChanged = draft.kind !== initial.kind && isKind(draft.kind)
  const topicsChanged = !sameSet(draft.topics, initial.topics)
  const kind = kindChanged ? draft.kind : existing?.kind
  const chosen = topicsChanged ? draft.topics : existing?.topics
  const request: CategoryOverrideRequest = { scope, key }
  if (isKind(kind)) request.kind = kind
  if (chosen) request.topics = inOrder(chosen)
  return request.kind === undefined && request.topics === undefined ? undefined : request
}

/** The message's insights as they look once a stored override applies, for an optimistic update. */
export function applyOverride(
  insights: MessageInsights,
  request: Pick<CategoryOverrideRequest, 'kind' | 'topics'>,
): MessageInsights {
  const aligned = insights.trust.state === 'aligned'
  return {
    ...insights,
    kind: request.kind ? { id: request.kind, source: 'override', overridden: true } : insights.kind,
    topics: request.topics
      ? inOrder(request.topics).map((id): CategoryTopic => {
          const topic: CategoryTopic = { id, source: 'override', overridden: true }
          if (sensitive.has(id) && !aligned) topic.held_back = 'not_verified'
          return topic
        })
      : insights.topics,
  }
}

export type TrustNote = { tone: 'muted' | 'warning'; text: string } | undefined

export function trustNote(trust: Trust, providerName: (id: string) => string): TrustNote {
  const t = strings.categories.trust
  if (trust.state === 'unknown') return { tone: 'muted', text: t.unknown }
  if (trust.state === 'unverified') return { tone: 'warning', text: t.unverified }
  if (trust.state === 'relayed') {
    return { tone: 'warning', text: t.relayed(providerName(trust.provider ?? '')) }
  }
  return undefined
}

export interface CategoryTarget {
  id: string
  insights?: MessageInsights | null
  from: readonly { email: string }[]
  alias?: SenderMessage['alias']
}

const shownStatuses = new Set(['ready', 'partial', 'outdated'])

export function canCategorize(message: CategoryTarget): boolean {
  return Boolean(
    message.insights && shownStatuses.has(message.insights.status) && overrideSender(message),
  )
}

/** The message `x` acts on: the focused one when it can be categorised, else the newest expanded
 * one that can. */
export function categoryTarget<M extends CategoryTarget>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
): M | undefined {
  const focused = messages.find((message) => message.id === focusedId)
  if (focused) return canCategorize(focused) ? focused : undefined
  return messages.findLast((message) => expanded(message.id) && canCategorize(message))
}

export function trustedIdError(value: string, list: readonly string[]): string | undefined {
  const id = value.trim()
  const t = strings.trustedServers
  if (!id) return t.empty
  if (id.length > 255 || !/^[\x21-\x7e]+$/.test(id)) return t.invalid
  if (list.some((item) => item.toLowerCase() === id.toLowerCase())) return t.duplicate
  if (list.length >= 16) return t.tooMany
  return undefined
}

export interface OverrideSummary {
  scope: string
  kind: string
  topics: string
}

/** How Settings → Categories shows an override; `health` is never listed. */
export function overrideSummary(
  item: Pick<CategoryOverride, 'scope' | 'kind' | 'topics'>,
): OverrideSummary {
  const t = strings.categories
  const shown = item.topics?.filter((topic) => topic !== 'health')
  return {
    scope: t.scopes[item.scope] ?? item.scope,
    kind: item.kind ? kindLabel(item.kind) : t.derived,
    topics: !shown ? t.derived : shown.length === 0 ? t.noTopics : shown.map(topicLabel).join(', '),
  }
}
