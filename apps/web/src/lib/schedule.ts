export const intervalChoices = [5, 15, 30, 60, 180, 360, 720, 1440, 0] as const

export function formatInterval(minutes: number): string {
  if (minutes === 0) return 'Only when you press Sync now'
  if (minutes < 60) return `Every ${minutes} minutes`
  if (minutes === 60) return 'Every hour'
  if (minutes === 1440) return 'Once a day'
  if (minutes % 1440 === 0) return `Every ${minutes / 1440} days`
  if (minutes % 60 === 0) return `Every ${minutes / 60} hours`
  return `Every ${minutes} minutes`
}

export function formatNextSync(at: number, now: number, locale?: string): string {
  const left = at - now
  if (left < 60_000) return 'Next sync in under a minute'
  const format = new Intl.RelativeTimeFormat(locale, { numeric: 'always' })
  if (left < 3_600_000) return `Next sync ${format.format(Math.round(left / 60_000), 'minute')}`
  if (left < 86_400_000) return `Next sync ${format.format(Math.round(left / 3_600_000), 'hour')}`
  return `Next sync ${format.format(Math.round(left / 86_400_000), 'day')}`
}

export function parseExcluded(text: string): string[] {
  const seen = new Set<string>()
  return text
    .split(/[\n,]/)
    .map((path) => path.trim())
    .filter((path) => path !== '' && !seen.has(path) && Boolean(seen.add(path)))
}
