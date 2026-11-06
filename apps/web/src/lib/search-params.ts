import { type CalendarView, isCalendarView, isDayKey } from './calendar.ts'

export interface MailSearch {
  m?: string
}

export interface SearchRouteSearch extends MailSearch {
  q: string
}

function text(value: unknown): string | undefined {
  if (typeof value === 'string') return value
  if (typeof value === 'number' && Number.isFinite(value)) return String(value)
  return undefined
}

export function parseMailSearch(search: Record<string, unknown>): MailSearch {
  const m = text(search.m)?.trim()
  return m ? { m } : {}
}

export function parseSearchRouteSearch(search: Record<string, unknown>): SearchRouteSearch {
  return { q: text(search.q) ?? '', ...parseMailSearch(search) }
}

export interface LabelRouteSearch extends MailSearch {
  name: string
}

export function parseLabelRouteSearch(search: Record<string, unknown>): LabelRouteSearch {
  return { name: text(search.name) ?? '', ...parseMailSearch(search) }
}

export interface PeopleSearch {
  p?: string
  q?: string
  source?: string
  diff?: true
}

export function parsePeopleSearch(search: Record<string, unknown>): PeopleSearch {
  const p = text(search.p)?.trim()
  const q = text(search.q)
  const source = text(search.source)?.trim()
  const diff = search.diff === true || search.diff === 'true' || search.diff === 1
  return {
    ...(p ? { p } : {}),
    ...(q?.trim() ? { q } : {}),
    ...(source ? { source } : {}),
    ...(diff ? { diff: true as const } : {}),
  }
}

export interface CalendarSearch {
  view?: CalendarView
  d?: string
  off?: string
  removed?: true
}

export function parseCalendarSearch(search: Record<string, unknown>): CalendarSearch {
  const view = text(search.view)
  const d = text(search.d)?.trim()
  const off = text(search.off)
    ?.split(',')
    .map((id) => id.trim())
    .filter(Boolean)
    .join(',')
  const removed = search.removed === true || search.removed === 'true' || search.removed === 1
  return {
    ...(isCalendarView(view) ? { view } : {}),
    ...(d && isDayKey(d) ? { d } : {}),
    ...(off ? { off } : {}),
    ...(removed ? { removed: true as const } : {}),
  }
}

export type AddMail = 'connect' | 'import'

export interface ArchivesSettingsSearch {
  archive?: string
  add?: AddMail
}

export function parseArchivesSettingsSearch(
  search: Record<string, unknown>,
): ArchivesSettingsSearch {
  const archive = text(search.archive)?.trim()
  const add = search.add === 'connect' || search.add === 'import' ? search.add : undefined
  return { ...(archive ? { archive } : {}), ...(archive && add ? { add } : {}) }
}
