import type { Account, Watch, WatchMode, WatchState } from '@holtmail/api-client'

import { parseExcluded } from './schedule.ts'

export function watchMode(account: Account): WatchMode {
  const mode = account.watch?.mode
  return mode === 'off' || mode === 'folders' ? mode : 'inbox'
}

export function watchRequest(mode: WatchMode, folders: string): Watch {
  return { mode, folders: mode === 'folders' ? parseExcluded(folders) : [] }
}

export function gmailLike(account: Account): boolean {
  const host = account.connection?.host.toLowerCase() ?? ''
  return (
    account.oauth_provider === 'google' ||
    host.endsWith('gmail.com') ||
    host.endsWith('googlemail.com')
  )
}

export type WatchTone = 'live' | 'warn' | 'muted'

export interface WatchBadge {
  label: string
  title: string
  tone: WatchTone
}

export function watchBadge(
  state: WatchState | undefined,
  what: 'mail' | 'contacts',
): WatchBadge | undefined {
  const subject = what === 'mail' ? 'New mail' : 'Changed contacts and events'
  switch (state?.state) {
    case 'live':
      return { label: 'Live', title: `${subject} arrive within seconds`, tone: 'live' }
    case 'starting':
      return {
        label: 'Connecting',
        title: 'Opening a connection to watch for changes',
        tone: 'muted',
      }
    case 'reconnecting':
      return {
        label: 'Reconnecting',
        title: state.reason ?? 'The connection broke; Holtmail tries again',
        tone: 'warn',
      }
    case 'polling':
      return {
        label: 'Scheduled',
        title: 'The server cannot report changes; Holtmail checks on schedule',
        tone: 'muted',
      }
    default:
      return undefined
  }
}
