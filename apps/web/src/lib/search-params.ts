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
