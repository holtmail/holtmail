import { strings } from '../strings.ts'
import { daysBetween } from './format.ts'

export interface DayGroup {
  key: string
  label: string
}

export function dayGroup(time: number, now: number, locale?: string): DayGroup {
  const date = new Date(time)
  const days = daysBetween(time, now)
  const day = `${date.getFullYear()}-${date.getMonth() + 1}-${date.getDate()}`
  if (days <= 0) return { key: `d:${day}`, label: strings.list.today }
  if (days === 1) return { key: `d:${day}`, label: strings.list.yesterday }
  if (days < 7) {
    return {
      key: `d:${day}`,
      label: new Intl.DateTimeFormat(locale, { weekday: 'long' }).format(date),
    }
  }
  if (date.getFullYear() === new Date(now).getFullYear()) {
    return {
      key: `d:${day}`,
      label: new Intl.DateTimeFormat(locale, { day: 'numeric', month: 'long' }).format(date),
    }
  }
  return {
    key: `m:${date.getFullYear()}-${date.getMonth() + 1}`,
    label: new Intl.DateTimeFormat(locale, { month: 'long', year: 'numeric' }).format(date),
  }
}

export type ListRow<T> =
  | { kind: 'header'; key: string; label: string }
  | { kind: 'item'; key: string; item: T; index: number }

export function groupByDay<T extends { id: string; date: number }>(
  items: readonly T[],
  now: number,
  locale?: string,
): ListRow<T>[] {
  const rows: ListRow<T>[] = []
  let current: string | undefined
  items.forEach((item, index) => {
    const group = dayGroup(item.date, now, locale)
    if (group.key !== current) {
      current = group.key
      rows.push({ kind: 'header', key: `h:${group.key}:${rows.length}`, label: group.label })
    }
    rows.push({ kind: 'item', key: item.id, item, index })
  })
  return rows
}
