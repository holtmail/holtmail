import type { CalendarOccurrence } from '@holtmail/api-client'

export type CalendarView = 'agenda' | 'week' | 'month'

export const calendarViews: readonly CalendarView[] = ['agenda', 'week', 'month']

export type DayKey = string

const dayMs = 86_400_000
const dayPattern = /^(\d{4})-(\d{2})-(\d{2})$/

export function isCalendarView(value: string | undefined): value is CalendarView {
  return value === 'agenda' || value === 'week' || value === 'month'
}

export function isDayKey(value: string): boolean {
  const match = dayPattern.exec(value)
  if (!match) return false
  const ms = Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]))
  return dayOf(ms) === value
}

function utcOf(key: DayKey): number {
  const match = dayPattern.exec(key)
  if (!match) return Number.NaN
  return Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]))
}

function dayOf(utcMidnight: number): DayKey {
  return new Date(utcMidnight).toISOString().slice(0, 10)
}

export function addDays(key: DayKey, days: number): DayKey {
  return dayOf(utcOf(key) + days * dayMs)
}

export function addMonths(key: DayKey, months: number): DayKey {
  const date = new Date(utcOf(key))
  const year = date.getUTCFullYear()
  const month = date.getUTCMonth() + months
  const last = new Date(Date.UTC(year, month + 1, 0)).getUTCDate()
  return dayOf(Date.UTC(year, month, Math.min(date.getUTCDate(), last)))
}

/** 0 for Sunday to 6 for Saturday. */
function weekdayOf(key: DayKey): number {
  return new Date(utcOf(key)).getUTCDay()
}

export function startOfWeek(key: DayKey, firstDay = 1): DayKey {
  return addDays(key, -((weekdayOf(key) - firstDay + 7) % 7))
}

function startOfMonth(key: DayKey): DayKey {
  return `${key.slice(0, 7)}-01`
}

function daysBetween(from: DayKey, to: DayKey): number {
  return Math.round((utcOf(to) - utcOf(from)) / dayMs)
}

interface Wall {
  day: DayKey
  minutes: number
}

const formatters = new Map<string, Intl.DateTimeFormat>()

function formatter(zone: string): Intl.DateTimeFormat {
  let format = formatters.get(zone)
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
    formatters.set(zone, format)
  }
  return format
}

/** The wall-clock day and minutes after midnight of a UTC instant in `zone`. */
export function wallClock(ms: number, zone: string): Wall {
  const parts = new Map(
    formatter(zone)
      .formatToParts(new Date(ms))
      .map((p) => [p.type, p.value]),
  )
  const year = parts.get('year') ?? '1970'
  const month = parts.get('month') ?? '01'
  const day = parts.get('day') ?? '01'
  const hour = Number(parts.get('hour') ?? 0) % 24
  const minute = Number(parts.get('minute') ?? 0)
  return { day: `${year.padStart(4, '0')}-${month}-${day}`, minutes: hour * 60 + minute }
}

export function dayKeyAt(ms: number, zone: string): DayKey {
  return wallClock(ms, zone).day
}

function offsetAt(ms: number, zone: string): number {
  const wall = wallClock(ms, zone)
  return utcOf(wall.day) + wall.minutes * 60_000 - Math.floor(ms / 60_000) * 60_000
}

/** The first instant of `key` in `zone`, as UTC milliseconds. */
export function zonedMidnight(key: DayKey, zone: string): number {
  const wall = utcOf(key)
  let guess = wall - offsetAt(wall, zone)
  guess = wall - offsetAt(guess, zone)
  for (let step = 0; step < 8 && dayKeyAt(guess, zone) !== key; step++) guess += 900_000
  return guess
}

export function todayKey(zone: string, now = Date.now()): DayKey {
  return dayKeyAt(now, zone)
}

export interface Span {
  from: number
  to: number
  days: DayKey[]
}

/** The days a view shows around `anchor`, and their span in UTC milliseconds: the agenda shows
 * the anchor's month, the week view its week, the month view whole weeks around its month. */
export function viewSpan(view: CalendarView, anchor: DayKey, zone: string, firstDay = 1): Span {
  let first: DayKey
  let count: number
  if (view === 'week') {
    first = startOfWeek(anchor, firstDay)
    count = 7
  } else {
    const month = startOfMonth(anchor)
    const next = addMonths(month, 1)
    if (view === 'agenda') {
      first = month
      count = daysBetween(month, next)
    } else {
      first = startOfWeek(month, firstDay)
      const end = startOfWeek(addDays(next, 6), firstDay)
      count = daysBetween(first, end)
    }
  }
  const days = Array.from({ length: count }, (_, i) => addDays(first, i))
  return {
    from: zonedMidnight(days[0] ?? first, zone),
    to: zonedMidnight(addDays(days.at(-1) ?? first, 1), zone),
    days,
  }
}

/** The anchor one period before or after: a week in the week view, a month otherwise. */
export function shiftAnchor(view: CalendarView, anchor: DayKey, step: number): DayKey {
  return view === 'week' ? addDays(anchor, 7 * step) : addMonths(anchor, step)
}

type Timing = Pick<
  CalendarOccurrence,
  'starts_at' | 'ends_at' | 'all_day' | 'floating' | 'start_local' | 'end_local'
>

function localWall(value: string): Wall | undefined {
  const day = value.slice(0, 10)
  if (!isDayKey(day)) return undefined
  const hour = Number(value.slice(11, 13) || 0)
  const minute = Number(value.slice(14, 16) || 0)
  return { day, minutes: hour * 60 + minute }
}

function bounds(o: Timing, zone: string): { start: Wall; end: Wall } | undefined {
  if (o.all_day || o.floating) {
    const start = localWall(o.start_local)
    const end = localWall(o.end_local) ?? start
    if (!start || !end) return undefined
    return { start, end }
  }
  return { start: wallClock(o.starts_at, zone), end: wallClock(o.ends_at, zone) }
}

const maxDays = 62

/** The days an instance covers in `zone`. Dates and floating times keep their own days; an
 * instance ending at midnight does not reach into the next day. */
export function occurrenceDays(o: Timing, zone: string): DayKey[] {
  const span = bounds(o, zone)
  if (!span) return []
  let last = span.end.day
  if (span.end.minutes === 0 && daysBetween(span.start.day, last) > 0) last = addDays(last, -1)
  const count = Math.min(Math.max(daysBetween(span.start.day, last), 0) + 1, maxDays)
  return Array.from({ length: count }, (_, i) => addDays(span.start.day, i))
}

/** Where a timed instance sits on `day`, in minutes after midnight, cut to the day. */
export function dayMinutes(o: Timing, day: DayKey, zone: string): { start: number; end: number } {
  const span = bounds(o, zone)
  if (!span) return { start: 0, end: 0 }
  const start = span.start.day === day ? span.start.minutes : 0
  const end =
    span.end.day === day ? span.end.minutes : daysBetween(day, span.end.day) > 0 ? 1440 : 0
  return { start, end: Math.max(end, start) }
}

export interface Slot {
  column: number
  columns: number
}

/** Side-by-side columns for overlapping blocks of a day: each block takes the first free column
 * of its cluster of overlapping blocks, and every block of a cluster shares its column count. A
 * block shorter than `minimum` minutes counts as `minimum` long. */
export function layoutColumns(
  blocks: readonly { id: string; start: number; end: number }[],
  minimum = 15,
): Map<string, Slot> {
  const sorted = blocks
    .map((b) => ({ id: b.id, start: b.start, end: Math.max(b.end, b.start + minimum) }))
    .toSorted((a, b) => a.start - b.start || b.end - a.end || a.id.localeCompare(b.id))
  const out = new Map<string, Slot>()
  let cluster: { id: string; column: number }[] = []
  let columnEnds: number[] = []
  let clusterEnd = -Infinity
  const close = () => {
    for (const member of cluster) {
      out.set(member.id, { column: member.column, columns: columnEnds.length })
    }
    cluster = []
    columnEnds = []
  }
  for (const block of sorted) {
    if (block.start >= clusterEnd) close()
    let column = columnEnds.findIndex((end) => end <= block.start)
    if (column === -1) {
      column = columnEnds.length
      columnEnds.push(block.end)
    } else {
      columnEnds[column] = block.end
    }
    cluster.push({ id: block.id, column })
    clusterEnd = Math.max(clusterEnd === -Infinity ? block.end : clusterEnd, block.end)
  }
  close()
  return out
}

/** The instances on each of `days`, all-day ones first, then by start. */
export function byDay<T extends Timing>(
  occurrences: readonly T[],
  days: readonly DayKey[],
  zone: string,
): Map<DayKey, T[]> {
  const out = new Map<DayKey, T[]>(days.map((d) => [d, []]))
  for (const o of occurrences) {
    for (const day of occurrenceDays(o, zone)) out.get(day)?.push(o)
  }
  for (const list of out.values()) {
    list.sort(
      (a, b) =>
        Number(b.all_day) - Number(a.all_day) || a.starts_at - b.starts_at || b.ends_at - a.ends_at,
    )
  }
  return out
}

/** A calendar's color as written, when it is a hex color; `undefined` otherwise. */
export function calendarColor(color: string | null | undefined): string | undefined {
  if (!color) return undefined
  const value = color.trim()
  return /^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(value) ? value.slice(0, 7) : undefined
}

/** The calendars to ask for: every one but the hidden ones, or `undefined` when none is hidden. */
export function shownCalendars(
  all: readonly { id: string }[],
  hidden: readonly string[],
): string[] | undefined {
  if (hidden.length === 0) return undefined
  return all.map((c) => c.id).filter((id) => !hidden.includes(id))
}
