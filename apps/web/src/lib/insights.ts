import type {
  ActionInsight,
  BoardingLeg,
  CodeInsight,
  EventTime,
  Insight,
  InsightConfidence,
  InsightTime,
  Invitation,
  InvoiceInsight,
  LinkAction,
  MessageInsights,
  Money,
  OneClickReason,
  OrderInsight,
  ParcelInsight,
  PassDate,
  PassInsight,
  PassStyle,
  Reservation,
  ReservationInsight,
  ReservationKind,
  Trust,
} from '@holtmail/api-client'

import { strings } from '../strings.ts'

export const STALE_CODE_MS = 15 * 60_000

const shownStatuses = new Set(['ready', 'partial', 'outdated'])

/** `ready`, `partial` and `outdated` rows render; the others render nothing. */
export function shownStatus(status: string): boolean {
  return shownStatuses.has(status)
}

const typeRank: Record<string, number> = {
  code: 0,
  action: 1,
  reservation: 2,
  pass: 3,
  parcel: 4,
  order: 5,
  invoice: 6,
}

const confidenceRank: Record<InsightConfidence, number> = { certain: 2, likely: 1, possible: 0 }

export function orderInsights(items: readonly Insight[]): Insight[] {
  return items
    .filter((item) => item.type in typeRank)
    .map((item, index) => ({ item, index }))
    .toSorted(
      (a, b) =>
        (typeRank[a.item.type] ?? 99) - (typeRank[b.item.type] ?? 99) ||
        Number(a.item.forwarded) - Number(b.item.forwarded) ||
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

export function primaryAction(items: readonly Insight[]): ActionInsight | undefined {
  return orderInsights(items).find((item): item is ActionInsight => item.type === 'action')
}

export function actionLabel(action: LinkAction): string {
  return strings.insights.actions[action] ?? action
}

export function carrierName(carrier: string): string {
  return strings.insights.carriers[carrier] ?? carrier
}

export type BadgeKey = 'tracked' | 'mismatch' | 'differs' | 'confusable' | 'unverified'

export interface ActionBadge {
  key: BadgeKey
  text: string
  warning: boolean
}

export function isUnverified(trust: Trust): boolean {
  return trust.state === 'unverified' || trust.state === 'relayed'
}

export function actionBadges(action: ActionInsight, trust: Trust): ActionBadge[] {
  const t = strings.insights.badges
  const flags: [BadgeKey, boolean, boolean][] = [
    ['tracked', action.tracked, false],
    ['mismatch', action.text_domain_mismatch, true],
    ['differs', action.destination_differs_from_sender, true],
    ['confusable', action.confusable, true],
    ['unverified', isUnverified(trust), true],
  ]
  return flags.filter(([, on]) => on).map(([key, , warning]) => ({ key, text: t[key], warning }))
}

export type CopyKind = 'code' | 'reference' | 'tracking'

export interface TopCopy {
  kind: CopyKind
  value: string
}

export interface LinkTarget {
  href: string
  kind: 'action' | 'tracking'
}

export interface CardView {
  codes: CodeInsight[]
  action: ActionInsight | undefined
  reservations: ReservationInsight[]
  passes: PassInsight[]
  parcels: ParcelInsight[]
  orders: OrderInsight[]
  invoices: InvoiceInsight[]
  copy: TopCopy | undefined
  link: LinkTarget | undefined
  unsubscribe: UnsubscribeLink | undefined
  oneClick: boolean
  reason: string | undefined
  unchecked: boolean
  unverified: boolean
}

function topCopyOf(
  codes: CodeInsight[],
  reservations: ReservationInsight[],
  passes: PassInsight[],
  parcels: ParcelInsight[],
): TopCopy | undefined {
  if (codes[0]) return { kind: 'code', value: copyValue(codes[0].value) }
  const reference =
    reservations.map(bookingReference).find(Boolean) ??
    passes.map((pass) => pass.boarding?.pnr).find(Boolean)
  if (reference) return { kind: 'reference', value: reference }
  if (parcels[0]) return { kind: 'tracking', value: parcels[0].number }
  return undefined
}

/** The booking reference of a reservation, copied as written. */
export function bookingReference(item: ReservationInsight): string | undefined {
  return item.reservation.reservation_number ?? undefined
}

function linkOf(
  action: ActionInsight | undefined,
  parcels: ParcelInsight[],
): LinkTarget | undefined {
  if (action) return { href: action.url, kind: 'action' }
  const tracked = parcels.find((parcel) => parcel.track_url)
  return tracked?.track_url ? { href: tracked.track_url, kind: 'tracking' } : undefined
}

export function unsubscribeLink(insights: MessageInsights): UnsubscribeLink | undefined {
  const links = insights.unsubscribe
  if (links?.https) return { href: links.https, kind: 'https' }
  if (links?.mailto) return { href: links.mailto, kind: 'mailto' }
  return undefined
}

export function cardView(
  insights: MessageInsights | null | undefined,
  invitations: readonly Invitation[] | null | undefined = [],
): CardView | undefined {
  if (!insights || !shownStatuses.has(insights.status)) return undefined
  const ordered = orderInsights(insights.items)
  const codes = ordered.filter((item): item is CodeInsight => item.type === 'code')
  const action = ordered.find((item): item is ActionInsight => item.type === 'action')
  const reservations = ordered.filter(
    (item): item is ReservationInsight =>
      item.type === 'reservation' && !isDuplicateOfInvitation(item.reservation, invitations ?? []),
  )
  const passes = ordered.filter((item): item is PassInsight => item.type === 'pass')
  const parcels = ordered.filter((item): item is ParcelInsight => item.type === 'parcel')
  const orders = ordered.filter((item): item is OrderInsight => item.type === 'order')
  const invoices = ordered.filter((item): item is InvoiceInsight => item.type === 'invoice')
  const unsubscribe = unsubscribeLink(insights)
  const oneClick = insights.unsubscribe?.one_click.available === true
  const rows = [codes, reservations, passes, parcels, orders, invoices].some((list) => list.length)
  if (!rows && !action && !unsubscribe && !oneClick) return undefined
  return {
    codes,
    action,
    reservations,
    passes,
    parcels,
    orders,
    invoices,
    copy: topCopyOf(codes, reservations, passes, parcels),
    link: linkOf(action, parcels),
    unsubscribe,
    oneClick,
    reason: unsubscribe ? reasonText(insights.unsubscribe?.one_click.reason) : undefined,
    unchecked: insights.trust.state === 'unknown',
    unverified: isUnverified(insights.trust),
  }
}

export interface TargetMessage extends Timestamps {
  id: string
  insights?: MessageInsights | null
  invitations?: readonly Invitation[] | null
}

export function topCopy(message: TargetMessage): TopCopy | undefined {
  return cardView(message.insights, message.invitations)?.copy
}

export function copyTarget<M extends TargetMessage>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
  now: number,
): M | undefined {
  const focused = messages.find((message) => message.id === focusedId)
  if (focused) return topCopy(focused) ? focused : undefined
  const candidates = messages.filter((message) => expanded(message.id) && topCopy(message))
  return (
    candidates.findLast(
      (message) => topCopy(message)?.kind === 'code' && !isStale(codeTime(message), now),
    ) ?? candidates.at(-1)
  )
}

function lastWith<M extends TargetMessage>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
  has: (view: CardView) => boolean,
): M | undefined {
  const ok = (message: M) => {
    const view = cardView(message.insights, message.invitations)
    return view ? has(view) : false
  }
  const focused = messages.find((message) => message.id === focusedId)
  if (focused) return ok(focused) ? focused : undefined
  return messages.findLast((message) => expanded(message.id) && ok(message))
}

export function linkTarget<M extends TargetMessage>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
): M | undefined {
  return lastWith(messages, expanded, focusedId, (view) => Boolean(view.link))
}

/** What `Shift+U` does on a message: open the consent dialog, follow the link, or nothing. */
export function unsubscribeKey(view: CardView | undefined): 'dialog' | 'link' | undefined {
  if (view?.oneClick) return 'dialog'
  return view?.unsubscribe ? 'link' : undefined
}

export function unsubscribeTarget<M extends TargetMessage>(
  messages: readonly M[],
  expanded: (id: string) => boolean,
  focusedId: string | undefined,
): M | undefined {
  return lastWith(messages, expanded, focusedId, (view) => Boolean(unsubscribeKey(view)))
}

const wallPattern = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?/
const suffixPattern = /(Z|[+-]\d{2}:?\d{2})$/

function wallMs(local: string): number | undefined {
  const match = wallPattern.exec(local)
  if (!match) return undefined
  const n = (index: number) => Number(match[index] ?? 0)
  return Date.UTC(n(1), n(2) - 1, n(3), n(4), n(5), n(6))
}

function offsetMinutes(offset: string): number | undefined {
  if (offset === 'Z') return 0
  const match = /^([+-])(\d{2}):?(\d{2})$/.exec(offset)
  if (!match) return undefined
  const minutes = Number(match[2]) * 60 + Number(match[3])
  return match[1] === '-' ? -minutes : minutes
}

/** The instant of a time written with an offset, in UTC milliseconds; `undefined` for a Local time. */
export function insightInstant(time: InsightTime): number | undefined {
  const wall = wallMs(time.local)
  const offset = time.offset ? offsetMinutes(time.offset) : undefined
  return wall === undefined || offset === undefined ? undefined : wall - offset * 60_000
}

const zoneFormats = new Map<string, Intl.DateTimeFormat>()

function zoneWallMs(instant: number, zone: string): number | undefined {
  try {
    let format = zoneFormats.get(zone)
    if (!format) {
      format = new Intl.DateTimeFormat('en-US', {
        timeZone: zone,
        hourCycle: 'h23',
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
      })
      zoneFormats.set(zone, format)
    }
    const parts = new Map(format.formatToParts(new Date(instant)).map((p) => [p.type, p.value]))
    const part = (name: Intl.DateTimeFormatPartTypes) => Number(parts.get(name) ?? Number.NaN)
    const wall = Date.UTC(
      part('year'),
      part('month') - 1,
      part('day'),
      part('hour') % 24,
      part('minute'),
      part('second'),
    )
    return Number.isNaN(wall) ? undefined : wall + (instant % 1000)
  } catch {
    return undefined
  }
}

function zoneAbbreviation(zone: string, instant: number, offset: number): string | undefined {
  if (zoneWallMs(instant, zone) !== instant + offset * 60_000) return undefined
  for (const locale of ['en-US', 'en-GB']) {
    const name = new Intl.DateTimeFormat(locale, { timeZone: zone, timeZoneName: 'short' })
      .formatToParts(new Date(instant))
      .find((part) => part.type === 'timeZoneName')?.value
    if (name && /^[A-Z]{2,5}$/.test(name)) return name
  }
  return undefined
}

function offsetLabel(offset: number): string {
  if (offset === 0) return 'UTC'
  const sign = offset < 0 ? '-' : '+'
  const abs = Math.abs(offset)
  const hours = String(Math.floor(abs / 60)).padStart(2, '0')
  const minutes = String(abs % 60).padStart(2, '0')
  return `UTC${sign}${hours}:${minutes}`
}

const dayMonth = new Intl.DateTimeFormat('en-GB', {
  day: 'numeric',
  month: 'short',
  timeZone: 'UTC',
})
const dayMonthYear = new Intl.DateTimeFormat('en-GB', {
  day: 'numeric',
  month: 'short',
  year: 'numeric',
  timeZone: 'UTC',
})

function formatDay(wall: number, year: number | undefined): string {
  return (new Date(wall).getUTCFullYear() === year ? dayMonth : dayMonthYear).format(wall)
}

/** A `YYYY-MM-DD` date as "12 Mar", with the year when it is not `year`. */
export function formatInsightDate(date: string, year?: number): string {
  const wall = wallMs(`${date}T00:00:00`)
  return wall === undefined ? date : formatDay(wall, year)
}

/** A pass date: a full date as {@link formatInsightDate}, a month and day as "12 Mar". */
export function formatPassDate(date: PassDate, year?: number): string {
  if (typeof date === 'string') return formatInsightDate(date, year)
  return dayMonth.format(Date.UTC(2000, date.month - 1, date.day))
}

/**
 * A time as written, in its own offset and never converted to the viewer's zone: "12 Mar 07:15 CET".
 * The year shows when it is not `year` (the message's year) or `year` is unknown. A time with a zone
 * whose offset at that instant matches the written one gets the zone's abbreviation, else
 * "UTC+01:00" (or "UTC"); a Local time gets nothing after the clock time.
 */
export function formatInsightTime(time: InsightTime, year?: number): string {
  const wall = wallMs(time.local)
  if (wall === undefined) return time.local
  const clock = `${formatDay(wall, year)} ${time.local.slice(11, 16)}`
  const offset = time.offset ? offsetMinutes(time.offset) : undefined
  if (offset === undefined) return clock
  const instant = wall - offset * 60_000
  const zone = time.zone ? zoneAbbreviation(time.zone, instant, offset) : undefined
  return `${clock} ${zone ?? offsetLabel(offset)}`
}

/**
 * An amount in its currency with exactly the currency's exponent of fraction digits, formatted from
 * the exact decimal string so no precision is lost: "$1,234.50", "¥1,200". An unknown currency
 * falls back to "1234.50 XYZ".
 */
export function formatMoney(money: Money): string {
  const amount = money.amount
  if (!isDecimal(amount)) return `${amount} ${money.currency}`
  try {
    return new Intl.NumberFormat('en', {
      style: 'currency',
      currency: money.currency,
      minimumFractionDigits: money.exponent,
      maximumFractionDigits: money.exponent,
    }).format(amount)
  } catch {
    return `${money.amount} ${money.currency}`
  }
}

function isDecimal(value: string): value is Intl.StringNumericLiteral {
  return /^-?\d+(\.\d+)?$/.test(value)
}

const sameInstantMs = 60_000

function invitationWall(start: EventTime): number | undefined {
  const suffix = suffixPattern.exec(start.value.slice(10))
  if (suffix && start.time_zone && typeof start.utc === 'number') {
    const zoned = zoneWallMs(start.utc, start.time_zone)
    if (zoned !== undefined) return zoned
  }
  return wallMs(start.value)
}

function invitationInstant(start: EventTime): number | undefined {
  const floating = !start.time_zone && !suffixPattern.test(start.value.slice(10))
  return floating || typeof start.utc !== 'number' ? undefined : start.utc
}

/**
 * Whether an event reservation repeats an invitation of the same message: their starts are within
 * one minute of each other, inclusive. A time with an offset compares as an instant; a Local time
 * compares as wall time on the invitation's zone, or with the invitation's own wall time when it
 * has none. All-day invitations and other reservation kinds never match.
 */
export function isDuplicateOfInvitation(
  reservation: Reservation,
  invitations: readonly Invitation[],
): boolean {
  if (reservation.kind !== 'event') return false
  const instant = insightInstant(reservation.start)
  const wall = wallMs(reservation.start.local)
  return invitations.some((invitation) => {
    if (invitation.all_day || !invitation.start) return false
    const other = instant === undefined ? undefined : invitationInstant(invitation.start)
    const [a, b] =
      instant !== undefined && other !== undefined
        ? [instant, other]
        : [wall, invitationWall(invitation.start)]
    return a !== undefined && b !== undefined && Math.abs(a - b) <= sameInstantMs
  })
}

export function reservationLabel(kind: ReservationKind): string {
  return strings.insights.reservations[kind]
}

export function passLabel(style: PassStyle): string {
  return strings.insights.passStyles[style]
}

function seatsText(seats: readonly string[]): string | undefined {
  const t = strings.insights
  if (seats.length === 0) return undefined
  return seats.length === 1 ? `${t.seat} ${seats[0]}` : `${t.seats} ${seats.join(', ')}`
}

function route(from: string, to: string | null | undefined): string {
  return to ? `${from} → ${to}` : from
}

const present = (parts: (string | null | undefined | false)[]): string[] =>
  parts.filter((part): part is string => Boolean(part))

/**
 * The parts of a reservation's row, in display order, without its booking reference: for a flight
 * `["LH 1234", "FRA → LIS", "12 Mar 07:15 CET", "Seat 14C"]`. Times are shown as
 * {@link formatInsightTime} shows them.
 */
export function reservationParts(reservation: Reservation, year?: number): string[] {
  const t = strings.insights
  const time = (value: InsightTime | null | undefined) =>
    value ? formatInsightTime(value, year) : undefined
  switch (reservation.kind) {
    case 'flight':
      return present([
        [reservation.carrier, reservation.flight_number].filter(Boolean).join(' '),
        route(reservation.departure_airport, reservation.arrival_airport),
        time(reservation.departure),
        seatsText(reservation.seats),
      ])
    case 'lodging':
      return present([
        reservation.name,
        reservation.checkout
          ? `${time(reservation.checkin)} → ${time(reservation.checkout)}`
          : `${t.checkIn} ${time(reservation.checkin)}`,
      ])
    case 'train':
    case 'bus':
      return present([
        reservation.number,
        route(reservation.departure_station, reservation.arrival_station),
        time(reservation.departure),
        seatsText(reservation.seats),
      ])
    case 'event':
      return present([reservation.name, time(reservation.start), reservation.venue])
    case 'food':
      return present([
        reservation.name,
        time(reservation.start),
        typeof reservation.party_size === 'number' && t.partySize(reservation.party_size),
      ])
  }
  return present([reservation.company, time(reservation.pickup), reservation.pickup_location])
}

function legText(leg: BoardingLeg, year?: number): string {
  return present([
    [leg.carrier, leg.flight].filter(Boolean).join(' '),
    route(leg.from, leg.to),
    leg.date && formatPassDate(leg.date, year),
    leg.seat && `${strings.insights.seat} ${leg.seat}`,
  ]).join(' ')
}

/** The parts of a pass row without its booking reference: issuer, description, time, legs. */
export function passParts(pass: PassInsight, year?: number): string[] {
  return present([
    pass.organization,
    pass.description,
    pass.relevant && formatInsightTime(pass.relevant, year),
    ...(pass.boarding?.legs ?? []).map((leg) => legText(leg, year)),
  ])
}

/** The parts of an order or invoice row without its number: who, total, due date, status. */
export function purchaseParts(item: OrderInsight | InvoiceInsight, year?: number): string[] {
  const t = strings.insights
  const total = item.total && formatMoney(item.total)
  if (item.type === 'order') return present([item.merchant, total, item.status])
  return present([item.payee, total, item.due && `${t.due} ${formatInsightDate(item.due, year)}`])
}
