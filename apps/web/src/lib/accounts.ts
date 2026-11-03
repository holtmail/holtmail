import type { Account } from '@holtmail/api-client'

import { formatCount } from './format.ts'

export type AccountTone = 'running' | 'error' | 'paused' | 'done' | 'waiting'

export interface AccountState {
  tone: AccountTone
  label: string
}

const units: [Intl.RelativeTimeFormatUnit, number][] = [
  ['day', 86_400_000],
  ['hour', 3_600_000],
  ['minute', 60_000],
]

export function formatAgo(time: number, now: number, locale?: string): string {
  const elapsed = Math.max(0, now - time)
  if (elapsed < 60_000) return 'just now'
  const format = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })
  for (const [unit, size] of units) {
    if (elapsed >= size) return format.format(-Math.floor(elapsed / size), unit)
  }
  return 'just now'
}

const newCount = (count: number) => (count === 1 ? '1 new message' : `${formatCount(count)} new`)

export function accountState(account: Account, now: number): AccountState {
  const { status } = account
  const imported = account.kind === 'import'
  if (status.running) {
    const verb = imported ? 'Importing' : 'Syncing'
    return {
      tone: 'running',
      label: status.new_messages > 0 ? `${verb}… ${newCount(status.new_messages)}` : `${verb}…`,
    }
  }
  if (status.error) return { tone: 'error', label: status.error }
  if (!imported && !account.enabled) return { tone: 'paused', label: 'Paused' }
  if (status.finished_at != null) {
    if (imported) {
      return {
        tone: 'done',
        label:
          status.new_messages === 1
            ? 'Imported 1 message'
            : `Imported ${formatCount(status.new_messages)} messages`,
      }
    }
    const ago = `Synced ${formatAgo(status.finished_at, now)}`
    return {
      tone: 'done',
      label: status.new_messages > 0 ? `${ago} · ${newCount(status.new_messages)}` : ago,
    }
  }
  return {
    tone: imported ? 'done' : 'waiting',
    label: imported ? 'Imported' : 'Waits for the next sync',
  }
}

export const anyRunning = (accounts: readonly Account[] | undefined): boolean =>
  accounts?.some((account) => account.status.running) ?? false

export const importExtensions = ['.mbox', '.mbx', '.eml', '.zip'] as const

export function fileKind(name: string): 'mbox' | 'eml' | 'zip' | 'unknown' {
  const lower = name.toLowerCase()
  if (lower.endsWith('.eml')) return 'eml'
  if (lower.endsWith('.zip')) return 'zip'
  if (lower.endsWith('.mbox') || lower.endsWith('.mbx') || !lower.includes('.')) return 'mbox'
  return 'unknown'
}
