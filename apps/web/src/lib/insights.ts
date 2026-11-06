import type {
  CodeInsight,
  Insight,
  InsightConfidence,
  MessageInsights,
  OneClickReason,
} from '@holtmail/api-client'

import { strings } from '../strings.ts'

export const STALE_CODE_MS = 15 * 60_000

const shownStatuses = new Set(['ready', 'partial', 'outdated'])

const typeRank: Record<string, number> = { code: 0 }

const confidenceRank: Record<InsightConfidence, number> = { certain: 2, likely: 1, possible: 0 }

export function orderInsights(items: readonly Insight[]): Insight[] {
  return items
    .filter((item) => item.type in typeRank)
    .map((item, index) => ({ item, index }))
    .toSorted(
      (a, b) =>
        (typeRank[a.item.type] ?? 99) - (typeRank[b.item.type] ?? 99) ||
        confidenceRank[b.item.confidence] - confidenceRank[a.item.confidence] ||
        b.item.score - a.item.score ||
        a.index - b.index,
    )
    .map(({ item }) => item)
}

export function groupCode(value: string): string {
  const compact = copyValue(value)
  if (!/^\d+$/.test(compact)) return compact
  if (compact.length === 6) return `${compact.slice(0, 3)} ${compact.slice(3)}`
  if (compact.length === 8) return `${compact.slice(0, 4)} ${compact.slice(4)}`
  return compact
}

export function copyValue(value: string): string {
  return value.replace(/\s+/g, '')
}

export interface Timestamps {
  received_at?: number | null
  sent_at?: number | null
}

export function codeTime({ received_at, sent_at }: Timestamps): number | undefined {
  const known = [received_at, sent_at].filter((t): t is number => typeof t === 'number')
  return known.length === 0 ? undefined : Math.min(...known)
}

export function isStale(time: number | undefined, now: number): boolean {
  return time === undefined || now - time > STALE_CODE_MS
}

const ageUnits: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 365 * 24 * 3_600_000],
  ['month', 30 * 24 * 3_600_000],
  ['week', 7 * 24 * 3_600_000],
  ['day', 24 * 3_600_000],
  ['hour', 3_600_000],
  ['minute', 60_000],
]

export function codeAge(time: number | undefined, now: number): string {
  if (time === undefined) return strings.insights.codeUndated
  const elapsed = Math.max(0, now - time)
  const format = new Intl.RelativeTimeFormat('en', { numeric: 'auto' })
  for (const [unit, size] of ageUnits) {
    if (elapsed >= size)
      return strings.insights.codeFrom(format.format(-Math.floor(elapsed / size), unit))
  }
  return strings.insights.codeFrom(format.format(0, 'minute'))
}

export function reasonText(reason: OneClickReason | null | undefined): string | undefined {
  return reason ? strings.insights.reasons[reason] : undefined
}

export interface UnsubscribeLink {
  href: string
  kind: 'https' | 'mailto'
}

export interface CardView {
  codes: CodeInsight[]
  unsubscribe: UnsubscribeLink | undefined
  reason: string | undefined
  unchecked: boolean
}

export function unsubscribeLink(insights: MessageInsights): UnsubscribeLink | undefined {
  const links = insights.unsubscribe
  if (links?.https) return { href: links.https, kind: 'https' }
  if (links?.mailto) return { href: links.mailto, kind: 'mailto' }
  return undefined
}

export function cardView(insights: MessageInsights | null | undefined): CardView | undefined {
  if (!insights || !shownStatuses.has(insights.status)) return undefined
  const codes = orderInsights(insights.items).filter(
    (item): item is CodeInsight => item.type === 'code',
  )
  const unsubscribe = unsubscribeLink(insights)
  if (codes.length === 0 && !unsubscribe) return undefined
  return {
    codes,
    unsubscribe,
    reason: unsubscribe ? reasonText(insights.unsubscribe?.one_click.reason) : undefined,
    unchecked: insights.trust.state === 'unknown',
  }
}

export interface TargetMessage extends Timestamps {
  id: string
  insights?: MessageInsights | null
}

export function topCode(message: TargetMessage): CodeInsight | undefined {
  return cardView(message.insights)?.codes[0]
}

export function codeTarget<M extends TargetMessage>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
  now: number,
): M | undefined {
  const focused = messages.find((message) => message.id === focusedId)
  if (focused) return topCode(focused) ? focused : undefined
  const candidates = messages.filter((message) => expanded(message.id) && topCode(message))
  return candidates.findLast((message) => !isStale(codeTime(message), now)) ?? candidates.at(-1)
}

export function unsubscribeTarget<M extends TargetMessage>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
): M | undefined {
  const has = (message: M) => Boolean(cardView(message.insights)?.unsubscribe)
  const focused = messages.find((message) => message.id === focusedId)
  if (focused) return has(focused) ? focused : undefined
  return messages.findLast((message) => expanded(message.id) && has(message))
}
