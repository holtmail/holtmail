const units = ['B', 'KB', 'MB', 'GB', 'TB'] as const

export function formatSize(bytes: number, locale?: string): string {
  let value = Math.max(0, bytes)
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const digits = unit === 0 || value >= 10 ? 0 : 1
  const number = new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(value)
  return `${number} ${units[unit]}`
}

export function formatCount(count: number, locale?: string): string {
  return new Intl.NumberFormat(locale).format(count)
}

function startOfDay(time: number): number {
  const date = new Date(time)
  date.setHours(0, 0, 0, 0)
  return date.getTime()
}

export function daysBetween(from: number, to: number): number {
  const a = new Date(startOfDay(from))
  const b = new Date(startOfDay(to))
  const utcA = Date.UTC(a.getFullYear(), a.getMonth(), a.getDate())
  const utcB = Date.UTC(b.getFullYear(), b.getMonth(), b.getDate())
  return Math.round((utcB - utcA) / 86_400_000)
}

export function formatListDate(time: number, now: number, locale?: string): string {
  const days = daysBetween(time, now)
  const date = new Date(time)
  if (days === 0) return new Intl.DateTimeFormat(locale, { timeStyle: 'short' }).format(date)
  if (days > 0 && days < 7) {
    return new Intl.DateTimeFormat(locale, { weekday: 'short' }).format(date)
  }
  if (date.getFullYear() === new Date(now).getFullYear()) {
    return new Intl.DateTimeFormat(locale, { day: 'numeric', month: 'short' }).format(date)
  }
  return new Intl.DateTimeFormat(locale, { dateStyle: 'short' }).format(date)
}

export function formatFullDate(time: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: 'full', timeStyle: 'short' }).format(
    new Date(time),
  )
}
