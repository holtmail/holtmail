import type { Recurrence, RuleDay } from '@holtmail/api-client'

import { strings } from '../strings.ts'

const t = strings.calendar.recurrence

const weekdayIndex: Record<string, number> = { MO: 0, TU: 1, WE: 2, TH: 3, FR: 4, SA: 5, SU: 6 }

function weekdayName(code: string, locale?: string): string {
  const index = weekdayIndex[code] ?? 0
  return new Intl.DateTimeFormat(locale, { weekday: 'long', timeZone: 'UTC' }).format(
    new Date(Date.UTC(2024, 0, 1 + index)),
  )
}

function monthName(month: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { month: 'long', timeZone: 'UTC' }).format(
    new Date(Date.UTC(2024, month - 1, 1)),
  )
}

function list(
  items: string[],
  locale?: string,
  type: 'conjunction' | 'disjunction' = 'conjunction',
) {
  return new Intl.ListFormat(locale, { type }).format(items)
}

function ordinal(n: number): string {
  return t.ordinals[String(n)] ?? (n > 0 ? t.nth(n) : t.nthFromEnd(-n))
}

function sortedDays(days: readonly RuleDay[]): RuleDay[] {
  return days.toSorted((a, b) => (weekdayIndex[a.weekday] ?? 0) - (weekdayIndex[b.weekday] ?? 0))
}

function isWorkweek(days: readonly RuleDay[]): boolean {
  const codes = new Set(days.filter((d) => d.ordinal == null).map((d) => d.weekday))
  return codes.size === 5 && ['MO', 'TU', 'WE', 'TH', 'FR'].every((c) => codes.has(c))
}

function dayPhrase(r: Recurrence, locale?: string): string | undefined {
  if (r.by_day.length === 0) return undefined
  if (r.by_set_pos.length > 0) {
    const position = list(r.by_set_pos.map(ordinal), locale)
    const what = isWorkweek(r.by_day)
      ? t.weekday
      : list(
          sortedDays(r.by_day).map((d) => weekdayName(d.weekday, locale)),
          locale,
          'disjunction',
        )
    return t.the(`${position} ${what}`)
  }
  const plain = r.by_day.filter((d) => d.ordinal == null)
  const numbered = r.by_day.filter((d) => d.ordinal != null)
  const parts = [
    ...numbered.map((d) => t.the(`${ordinal(d.ordinal ?? 1)} ${weekdayName(d.weekday, locale)}`)),
    ...sortedDays(plain).map((d) => weekdayName(d.weekday, locale)),
  ]
  return list(parts, locale)
}

function monthDayPhrase(days: readonly number[], locale?: string): string | undefined {
  if (days.length === 0) return undefined
  if (days.length === 1 && days[0] === -1) return t.lastDay
  const named = days
    .toSorted((a, b) => (a < 0 ? 100 - a : a) - (b < 0 ? 100 - b : b))
    .map((d) => (d > 0 ? String(d) : d === -1 ? t.lastDay : t.fromEnd(-d)))
  return days.length === 1 && (days[0] ?? 0) > 0
    ? t.day(named[0] ?? '')
    : t.days(list(named, locale))
}

function untilDate(until: string, locale?: string): string {
  const match = /^(\d{4})(\d{2})(\d{2})/.exec(until)
  if (!match) return until
  return new Intl.DateTimeFormat(locale, { dateStyle: 'long', timeZone: 'UTC' }).format(
    new Date(Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]))),
  )
}

/** How an event repeats, in words: `Every 2 weeks on Tuesday and Thursday, 5 times`. */
export function describeRecurrence(r: Recurrence, locale?: string): string {
  if (!r.frequency) return r.rule ? t.unknown : t.datesOnly
  const unit = t.units[r.frequency] ?? r.frequency.toLowerCase()
  const parts = [t.every(unit, r.interval)]
  const months = r.by_month.toSorted((a, b) => a - b).map((m) => monthName(m, locale))
  const days = dayPhrase(r, locale)
  const monthDays = monthDayPhrase(r.by_month_day, locale)
  if (r.frequency === 'YEARLY' && months.length > 0) {
    if (monthDays && r.by_month_day.length === 1 && (r.by_month_day[0] ?? 0) > 0 && !days) {
      parts.push(t.on(`${r.by_month_day[0]} ${list(months, locale)}`))
    } else if (days || monthDays) {
      parts.push(
        t.on(
          t.of(
            list(
              [days, monthDays].flatMap((p) => p ?? []),
              locale,
            ),
            list(months, locale),
          ),
        ),
      )
    } else {
      parts.push(t.in(list(months, locale)))
    }
  } else {
    if (days) parts.push(t.on(days))
    if (monthDays) parts.push(t.on(monthDays))
    if (months.length > 0) parts.push(t.in(list(months, locale)))
  }
  if (r.by_hour.length > 0) {
    const minutes = r.by_minute.length > 0 ? r.by_minute : [0]
    const times = r.by_hour.flatMap((h) =>
      minutes.map((m) => `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`),
    )
    parts.push(t.at(list(times, locale)))
  }
  let text = parts.join(' ')
  if (r.count != null) text += `, ${t.times(r.count)}`
  else if (r.until) text += `, ${t.until(untilDate(r.until, locale))}`
  return text
}
