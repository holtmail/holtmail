import type { EventTime, ItemChange } from '@holtmail/api-client'

export interface When {
  local: string
  original?: string
}

export function viewerZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone
}

function isZone(zone: string): boolean {
  try {
    return new Intl.DateTimeFormat('en', { timeZone: zone }).resolvedOptions().timeZone !== ''
  } catch {
    return false
  }
}

const dayMs = 86_400_000

function dateOf(value: string): number | undefined {
  const midnight = Date.parse(`${value.slice(0, 10)}T00:00:00Z`)
  return Number.isNaN(midnight) ? undefined : midnight
}

function range(
  start: number,
  end: number | undefined,
  options: Intl.DateTimeFormatOptions,
  locale?: string,
): string {
  const format = new Intl.DateTimeFormat(locale, options)
  if (end === undefined || end <= start) return format.format(new Date(start))
  return format.formatRange(new Date(start), new Date(end))
}

export function formatWhen(
  start: EventTime | null | undefined,
  end: EventTime | null | undefined,
  allDay: boolean,
  zone: string,
  locale?: string,
): When | undefined {
  if (!start) return undefined
  if (allDay) {
    const from = dateOf(start.value)
    if (from === undefined) return { local: start.value }
    const until = end ? dateOf(end.value) : undefined
    const last = until !== undefined && until - dayMs > from ? until - dayMs : undefined
    return { local: range(from, last, { dateStyle: 'full', timeZone: 'UTC' }, locale) }
  }
  if (start.utc === null || start.utc === undefined) return { local: start.value }
  const endUtc = end?.utc ?? undefined
  if (!start.time_zone) {
    const wall = Date.parse(`${start.value.slice(0, 19)}Z`)
    const wallEnd = end ? Date.parse(`${end.value.slice(0, 19)}Z`) : Number.NaN
    if (Number.isNaN(wall)) return { local: start.value }
    return {
      local: range(
        wall,
        Number.isNaN(wallEnd) ? undefined : wallEnd,
        { dateStyle: 'full', timeStyle: 'short', timeZone: 'UTC' },
        locale,
      ),
    }
  }
  const options: Intl.DateTimeFormatOptions = { dateStyle: 'full', timeStyle: 'short' }
  const local = range(start.utc, endUtc, { ...options, timeZone: zone }, locale)
  if (!isZone(start.time_zone)) {
    return { local, original: `${start.value} ${start.time_zone}` }
  }
  const there = range(start.utc, endUtc, { ...options, timeZone: start.time_zone }, locale)
  if (start.time_zone === zone || there === local) return { local }
  return { local, original: `${there} (${start.time_zone})` }
}

export interface EventParticipant {
  email?: string
  name?: string
  status?: string
}

export interface EventView {
  summary?: string
  description?: string
  location?: string
  start?: EventTime
  end?: EventTime
  allDay: boolean
  recurring: boolean
  status?: string
  organizer?: EventParticipant
  attendees: EventParticipant[]
}

function field(value: unknown, key: string): unknown {
  if (typeof value !== 'object' || value === null) return undefined
  return new Map(Object.entries(value)).get(key)
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value !== '' ? value : undefined
}

function time(value: unknown): EventTime | undefined {
  const written = text(field(value, 'value'))
  if (!written) return undefined
  const utc = field(value, 'utc')
  return {
    value: written,
    time_zone: text(field(value, 'time_zone')) ?? null,
    utc: typeof utc === 'number' ? utc : null,
  }
}

function participant(value: unknown): EventParticipant | undefined {
  if (typeof value !== 'object' || value === null) return undefined
  return {
    email: text(field(value, 'email')),
    name: text(field(value, 'name')),
    status: text(field(value, 'partstat'))?.toLowerCase(),
  }
}

export function readEvent(data: unknown): EventView | undefined {
  if (field(data, 'type') !== 'calendar') return undefined
  const components = field(data, 'components')
  if (!Array.isArray(components)) return undefined
  const main: unknown = components[0]
  if (typeof main !== 'object' || main === null) return undefined
  const attendees = field(main, 'attendees')
  return {
    summary: text(field(main, 'summary')),
    description: text(field(main, 'description')),
    location: text(field(main, 'location')),
    start: time(field(main, 'start')),
    end: time(field(main, 'end')),
    allDay: field(main, 'all_day') === true,
    recurring: field(main, 'recurring') === true,
    status: text(field(main, 'status'))?.toLowerCase(),
    organizer: participant(field(main, 'organizer')),
    attendees: Array.isArray(attendees)
      ? attendees.flatMap((a: unknown) => participant(a) ?? [])
      : [],
  }
}

export function participantLabel(p: EventParticipant): string {
  if (p.name && p.email) return `${p.name} <${p.email}>`
  return p.name ?? p.email ?? ''
}

export type ChangeKind = 'changed' | 'added' | 'removed'

export function changeKind(change: ItemChange): ChangeKind {
  if (change.before == null) return 'added'
  if (change.after == null) return 'removed'
  return 'changed'
}

export function fieldLabel(name: string): string {
  const instance = /^instance (.+) (\S+)$/.exec(name)
  const base = instance?.[2] ?? name
  const words = base.replaceAll('_', ' ')
  const label = words.charAt(0).toUpperCase() + words.slice(1)
  return instance ? `${label} (instance ${instance[1]})` : label
}
