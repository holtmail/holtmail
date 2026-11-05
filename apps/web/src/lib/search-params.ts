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
